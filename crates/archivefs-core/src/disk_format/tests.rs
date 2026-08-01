//! Tests for the shared disk-format layer.
//!
//! Images are generated rather than committed: an `.st` is a raw sector dump and
//! a `.stx` is a header plus a track table, so both are built exactly, byte for
//! byte, from the structures the adapters document. That also means a hostile
//! case can be constructed precisely - a fuzzy mask that does not fit its record,
//! a record length that overflows a `u64` - rather than approximated.

use super::*;
use crate::platform::{
    DetectionConfidence, DetectionRequest, DetectionSource, detect_platform_report,
};
use std::path::PathBuf;

/// A throwaway tree with a trusted library root and an untrusted one beside it.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-disk-format-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for directory in ["library", "library/atari-st", "downloads", "elsewhere"] {
            std::fs::create_dir_all(root.join(directory)).expect("fixture");
        }
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        std::fs::write(&path, bytes).expect("fixture");
        path
    }

    #[cfg(unix)]
    fn link(&self, from: &str, to: &Path) -> PathBuf {
        let path = self.path(from);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        std::os::unix::fs::symlink(to, &path).expect("fixture");
        path
    }

    fn trusted(&self) -> TrustedRoots {
        TrustedRoots::from_paths([self.path("library"), self.path("downloads")])
    }

    fn inspect(&self, path: &Path) -> DiskFormatEvidence {
        inspect_disk_format(path, &self.trusted(), DiskFormatContext::default(), None)
    }

    fn inspect_in_folder(&self, path: &Path, folder_platform: &str) -> DiskFormatEvidence {
        inspect_disk_format(
            path,
            &self.trusted(),
            DiskFormatContext {
                folder_platform: Some(folder_platform),
            },
            None,
        )
    }

    /// Path, type, size, mtime and mode for everything in the tree.
    fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        let mut entries = std::collections::BTreeMap::new();
        let mut stack = vec![self.root.clone()];
        while let Some(current) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in read_dir.filter_map(Result::ok) {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    stack.push(path.clone());
                }
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                };
                #[cfg(not(unix))]
                let mode = 0_u32;
                entries.insert(
                    path.to_string_lossy().into_owned(),
                    format!(
                        "{:?}|{}|{mode:o}|{:?}",
                        metadata.file_type(),
                        metadata.len(),
                        metadata.modified().ok()
                    ),
                );
            }
        }
        entries
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// --- Image builders -------------------------------------------------------

/// One Atari ST floppy image, built exactly from a geometry.
///
/// Writes a real FAT12 BPB at the documented offsets and pads to the full sector
/// count, so the resulting bytes are what a TOS format would have produced as far
/// as the adapter looks.
fn st_image(sides: u16, sectors_per_track: u16, tracks: u16) -> Vec<u8> {
    let total_sectors = u32::from(sectors_per_track) * u32::from(sides) * u32::from(tracks);
    let mut image = vec![0_u8; (total_sectors * 512) as usize];
    // 0x00: a branch instruction, as a real boot sector has.
    image[0x00] = 0x60;
    image[0x01] = 0x1c;
    image[0x02..0x08].copy_from_slice(b"Loader");
    image[0x0B..0x0D].copy_from_slice(&512_u16.to_le_bytes()); // bytes per sector
    image[0x0D] = 2; // sectors per cluster
    image[0x0E..0x10].copy_from_slice(&1_u16.to_le_bytes()); // reserved sectors
    image[0x10] = 2; // FATs
    // 112 root entries is the TOS default for both single- and double-sided.
    image[0x11..0x13].copy_from_slice(&112_u16.to_le_bytes());
    image[0x13..0x15].copy_from_slice(&u16::try_from(total_sectors).expect("fits").to_le_bytes());
    image[0x15] = 0xf9; // media descriptor
    image[0x16..0x18].copy_from_slice(&5_u16.to_le_bytes()); // sectors per FAT
    image[0x18..0x1A].copy_from_slice(&sectors_per_track.to_le_bytes());
    image[0x1A..0x1C].copy_from_slice(&sides.to_le_bytes());
    image
}

/// The standard double-sided 720 KB ST disk.
fn st_720k() -> Vec<u8> {
    st_image(2, 9, 80)
}

/// One Pasti container with `tracks` minimal track records.
fn stx_image(version: u16, track_count: u8, record_length: u32) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(b"RSY\0");
    image.extend_from_slice(&version.to_le_bytes());
    image.extend_from_slice(&1_u16.to_le_bytes()); // tool
    image.extend_from_slice(&0_u16.to_le_bytes()); // reserved
    image.push(track_count);
    image.push(0); // revision
    image.extend_from_slice(&0_u32.to_le_bytes()); // reserved
    assert_eq!(image.len(), 16, "the file header is 16 bytes");
    for index in 0..track_count {
        let start = image.len();
        image.extend_from_slice(&record_length.to_le_bytes()); // record length
        image.extend_from_slice(&0_u32.to_le_bytes()); // fuzzy length
        image.extend_from_slice(&9_u16.to_le_bytes()); // sector count
        image.extend_from_slice(&0_u16.to_le_bytes()); // flags
        image.extend_from_slice(&6250_u16.to_le_bytes()); // MFM track length
        image.push(index); // track number, side 0
        image.push(0); // record type
        assert_eq!(image.len() - start, 16, "a track header is 16 bytes");
        image.resize(start + record_length as usize, 0);
    }
    image
}

fn valid_stx() -> Vec<u8> {
    stx_image(3, 10, 64)
}

// --- Limits ---------------------------------------------------------------

/// Test 1
#[test]
fn limits_are_internally_consistent() {
    // Compile-time, because these are all constants: a change that breaks the
    // relationship should fail the build, not one test run.
    const _: () = assert!(MAX_DISK_FORMAT_READ_CHUNK as u64 <= MAX_DISK_FORMAT_BYTES_READ);
    const _: () = assert!(MAX_DISK_FORMAT_OFFSET <= MAX_PASTI_BYTES);
    const _: () = assert!(MAX_RAW_FLOPPY_BYTES < MAX_PASTI_BYTES);
    // A boot sector must be readable in one chunk.
    const _: () = assert!(512 <= MAX_DISK_FORMAT_READ_CHUNK);
    // The whole Pasti track table must fit in the budget: one 16-byte header per
    // record, plus the file header.
    let table = 16 * MAX_PASTI_TRACK_RECORDS as u64 + 16;
    assert!(
        table <= MAX_DISK_FORMAT_BYTES_READ,
        "the read budget must admit a full track table"
    );
}

// --- Atari ST: valid ------------------------------------------------------

/// Test 2
#[test]
fn a_standard_720k_st_image_is_recognised() {
    let fixture = Fixture::new("st-720k");
    let image = fixture.write("library/game.st", &st_720k());
    let evidence = fixture.inspect(&image);

    assert_eq!(evidence.format, Some(DiskFormat::AtariStRawFloppy));
    assert_eq!(evidence.platform, Some("AtariST"));
    assert_eq!(
        evidence.confidence,
        DetectionConfidence::Probable,
        "a FAT12 boot sector is shared with PC DOS floppies, so alone it is not proof"
    );
    assert!(!evidence.conclusive);
    let Some(DiskFormatMetadata::Floppy(geometry)) = evidence.metadata else {
        panic!("expected floppy geometry: {:?}", evidence.metadata);
    };
    assert_eq!(geometry.sides, 2);
    assert_eq!(geometry.sectors_per_track, 9);
    assert_eq!(geometry.tracks, 80);
    assert_eq!(geometry.total_sectors, 1440);
    assert_eq!(
        evidence.bytes_inspected, 512,
        "one boot sector, nothing more"
    );
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.contains("same FAT12 structure a PC DOS floppy")),
        "the limit of what this proves must be stated: {:?}",
        evidence.evidence
    );
}

/// Test 3
#[test]
fn a_second_valid_geometry_is_recognised() {
    let fixture = Fixture::new("st-880k");
    // Single-sided 400 KB and the 11-sector 880 KB extended format.
    for (sides, spt, tracks, expected_sectors) in
        [(1_u16, 10_u16, 80_u16, 800_u32), (2, 11, 80, 1760)]
    {
        let image = fixture.write(
            &format!("library/g{sides}{spt}.st"),
            &st_image(sides, spt, tracks),
        );
        let evidence = fixture.inspect(&image);
        assert_eq!(
            evidence.format,
            Some(DiskFormat::AtariStRawFloppy),
            "{sides} sides x {spt} sectors should validate: {:?}",
            evidence.refusal
        );
        let Some(DiskFormatMetadata::Floppy(geometry)) = evidence.metadata else {
            panic!("expected geometry");
        };
        assert_eq!(geometry.total_sectors, expected_sectors);
    }
}

/// Test 4
#[test]
fn an_st_image_in_an_atari_st_folder_is_confirmed() {
    let fixture = Fixture::new("st-folder");
    let image = fixture.write("library/atari-st/game.st", &st_720k());
    let evidence = fixture.inspect_in_folder(&image, "AtariST");
    assert_eq!(evidence.confidence, DetectionConfidence::Confirmed);
    assert!(evidence.conclusive);
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.contains("raises this to confirmed"))
    );
}

/// Test 5
#[test]
fn an_st_image_under_a_conflicting_folder_is_ambiguous() {
    let fixture = Fixture::new("st-conflict");
    let image = fixture.write("library/megadrive/game.st", &st_720k());
    let evidence = fixture.inspect_in_folder(&image, "MegaDrive");
    assert_eq!(
        evidence.confidence,
        DetectionConfidence::Ambiguous,
        "structure and folder disagree, so the structure claims nothing on its own"
    );
    assert!(!evidence.conclusive);
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.contains("names MegaDrive instead"))
    );
}

// --- Atari ST: refused ----------------------------------------------------

/// Test 6
#[test]
fn a_truncated_st_image_is_refused() {
    let fixture = Fixture::new("st-truncated");
    let mut image = st_720k();
    image.truncate(512 * 700); // fewer sectors than the boot sector declares
    let path = fixture.write("library/game.st", &image);
    let evidence = fixture.inspect(&path);
    assert_eq!(evidence.format, None);
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("geometry_mismatch"),
        "{:?}",
        evidence.refusal
    );
}

/// Test 7
#[test]
fn an_st_image_shorter_than_a_sector_is_refused() {
    let fixture = Fixture::new("st-tiny");
    let path = fixture.write("library/game.st", &[0_u8; 64]);
    assert_eq!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .map(DiskFormatRefusal::code),
        Some("too_small")
    );
}

/// Test 8
#[test]
fn an_st_image_that_is_not_sector_aligned_is_refused() {
    let fixture = Fixture::new("st-unaligned");
    let mut image = st_720k();
    image.push(0); // one byte past a whole sector
    let path = fixture.write("library/game.st", &image);
    assert_eq!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .map(DiskFormatRefusal::code),
        Some("not_sector_aligned")
    );
}

/// Test 9
#[test]
fn impossible_st_geometry_is_refused() {
    let fixture = Fixture::new("st-impossible");
    // Each case keeps the file length valid but corrupts one BPB field, so the
    // refusal can only come from the geometry check it targets.
    let cases: &[(&str, usize, &[u8])] = &[
        ("40 sectors per track", 0x18, &40_u16.to_le_bytes()),
        ("5 sides", 0x1A, &5_u16.to_le_bytes()),
        ("1024-byte sectors", 0x0B, &1024_u16.to_le_bytes()),
        ("zero sides", 0x1A, &0_u16.to_le_bytes()),
        ("zero sectors per track", 0x18, &0_u16.to_le_bytes()),
        ("7 FATs", 0x10, &[7]),
        ("3 sectors per cluster", 0x0D, &[3]),
        ("100 root entries", 0x11, &100_u16.to_le_bytes()),
        ("zero total sectors", 0x13, &0_u16.to_le_bytes()),
    ];
    for (label, offset, bytes) in cases {
        let mut image = st_720k();
        image[*offset..*offset + bytes.len()].copy_from_slice(bytes);
        let path = fixture.write("library/broken.st", &image);
        let evidence = fixture.inspect(&path);
        assert_eq!(
            evidence.format, None,
            "{label} must not be accepted as an Atari ST image"
        );
        assert!(
            evidence.refusal.is_some(),
            "{label} must be refused with a reason"
        );
    }
}

/// Test 10
#[test]
fn a_geometry_that_does_not_match_the_file_length_is_refused() {
    let fixture = Fixture::new("st-mismatch");
    // A perfectly valid 80-track, 2-side, 9-sector boot sector - 1440 sectors,
    // 737,280 bytes - in a file padded out to 1600 sectors. The geometry passes
    // every plausibility check, so the only thing wrong is that it does not
    // account for the file's length.
    let mut image = st_720k();
    image.resize(1600 * 512, 0);
    let path = fixture.write("library/game.st", &image);
    let evidence = fixture.inspect(&path);
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("geometry_mismatch"),
        "{:?}",
        evidence.refusal
    );
    let detail = evidence.refusal.as_ref().expect("refused").detail();
    assert!(
        detail.contains("737280") && detail.contains("819200"),
        "the refusal should name both sizes: {detail}"
    );
}

/// Test 11
#[test]
fn random_data_with_an_st_extension_is_refused() {
    let fixture = Fixture::new("st-random");
    // Sector-aligned, plausible length, but no coherent boot sector. This is the
    // case that must not be waved through on size alone.
    let mut image = vec![0_u8; 737_280];
    for (index, byte) in image.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(31).wrapping_add(7);
    }
    let path = fixture.write("library/noise.st", &image);
    let evidence = fixture.inspect(&path);
    assert_eq!(
        evidence.format, None,
        "a sector-aligned file of the right size is not enough: {:?}",
        evidence.evidence
    );
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("malformed")
    );
}

/// Test 12
#[test]
fn an_all_zero_st_image_is_refused() {
    let fixture = Fixture::new("st-zero");
    let path = fixture.write("library/blank.st", &vec![0_u8; 737_280]);
    assert_eq!(fixture.inspect(&path).format, None);
}

/// Test 13
#[test]
fn an_oversized_st_file_is_refused_before_it_is_read() {
    let fixture = Fixture::new("st-huge");
    // Sparse: only the length matters, and the adapter must refuse on length
    // alone without reading a byte.
    let path = fixture.path("library/huge.st");
    let file = std::fs::File::create(&path).expect("fixture");
    file.set_len(MAX_RAW_FLOPPY_BYTES + 512).expect("fixture");
    drop(file);
    let evidence = fixture.inspect(&path);
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("too_large")
    );
    assert_eq!(evidence.bytes_inspected, 0, "refused before any read");
}

// --- Pasti (.stx) ---------------------------------------------------------

/// Test 14
#[test]
fn a_minimal_valid_pasti_container_is_recognised_and_conclusive() {
    let fixture = Fixture::new("stx-valid");
    let path = fixture.write("library/game.stx", &valid_stx());
    let evidence = fixture.inspect(&path);

    assert_eq!(evidence.format, Some(DiskFormat::AtariStPasti));
    assert_eq!(evidence.platform, Some("AtariST"));
    assert_eq!(
        evidence.confidence,
        DetectionConfidence::Confirmed,
        "Pasti exists only for Atari ST media, so a valid container settles it"
    );
    assert!(evidence.conclusive);
    let Some(DiskFormatMetadata::Pasti(layout)) = evidence.metadata else {
        panic!("expected Pasti layout");
    };
    assert_eq!(layout.version, 3);
    assert_eq!(layout.declared_track_records, 10);
    assert_eq!(layout.validated_track_records, 10);
    assert_eq!(layout.declared_sectors, 90);
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.contains("never reconstructed")),
        "the report must state that the disk was not decoded"
    );
}

/// Test 15
#[test]
fn an_unsupported_pasti_version_is_refused_rather_than_guessed_at() {
    let fixture = Fixture::new("stx-version");
    for version in [0_u16, 1, 2, 4, 99, u16::MAX] {
        let path = fixture.write("library/game.stx", &stx_image(version, 4, 64));
        let evidence = fixture.inspect(&path);
        assert_eq!(
            evidence.format, None,
            "version {version} must not be claimed"
        );
        assert!(
            evidence
                .refusal
                .as_ref()
                .expect("refused")
                .detail()
                .contains("not one this build understands")
        );
    }
}

/// Test 16
#[test]
fn a_file_without_the_pasti_signature_is_refused() {
    let fixture = Fixture::new("stx-signature");
    let mut image = valid_stx();
    image[0..4].copy_from_slice(b"RSX\0");
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("Pasti `RSY\\0` signature")
    );
}

/// Test 17
#[test]
fn a_truncated_pasti_header_is_refused() {
    let fixture = Fixture::new("stx-truncated");
    for length in [0_usize, 4, 15, 16, 20, 31] {
        let mut image = valid_stx();
        image.truncate(length);
        let path = fixture.write("library/game.stx", &image);
        let evidence = fixture.inspect(&path);
        assert_eq!(
            evidence.format, None,
            "a {length}-byte file cannot be valid"
        );
        assert!(evidence.refusal.is_some());
    }
}

/// Test 18
#[test]
fn a_pasti_record_shorter_than_its_own_header_is_refused() {
    let fixture = Fixture::new("stx-short-record");
    let mut image = valid_stx();
    // Track 0 claims an 8-byte record; its own header is 16.
    image[16..20].copy_from_slice(&8_u32.to_le_bytes());
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("shorter than its own")
    );
}

/// Test 19
#[test]
fn a_pasti_record_reaching_past_the_file_is_refused() {
    let fixture = Fixture::new("stx-past-end");
    let mut image = valid_stx();
    image[16..20].copy_from_slice(&100_000_u32.to_le_bytes());
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("past the file's")
    );
}

/// Test 20
#[test]
fn a_pasti_fuzzy_mask_that_does_not_fit_its_record_is_refused() {
    let fixture = Fixture::new("stx-fuzzy");
    let mut image = valid_stx();
    // A 64-byte record claiming a 4096-byte fuzzy mask.
    image[20..24].copy_from_slice(&4096_u32.to_le_bytes());
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("does not fit in its")
    );
}

/// Test 21
#[test]
fn an_excessive_pasti_track_count_is_refused_without_allocating() {
    let fixture = Fixture::new("stx-track-count");
    // A tiny file claiming the maximum possible track count. The count must never
    // size an allocation, so this has to be cheap and refused.
    let mut image = valid_stx();
    image[0x0A] = u8::MAX;
    let path = fixture.write("library/game.stx", &image);
    let evidence = fixture.inspect(&path);
    assert_eq!(evidence.format, None);
    assert!(
        evidence
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("record limit")
    );
    assert!(
        evidence.bytes_inspected <= MAX_DISK_FORMAT_BYTES_READ,
        "the budget must hold whatever the header claims"
    );
}

/// Test 22
#[test]
fn a_pasti_header_declaring_no_tracks_is_refused() {
    let fixture = Fixture::new("stx-zero-tracks");
    let mut image = valid_stx();
    image[0x0A] = 0;
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("no track records")
    );
}

/// Test 23
#[test]
fn pasti_record_lengths_that_try_to_overflow_are_refused() {
    let fixture = Fixture::new("stx-overflow");
    // `u32::MAX` record length: adding it to the offset must be checked, not
    // wrapped into something that looks in-range.
    for hostile in [u32::MAX, u32::MAX - 15, 0xffff_0000] {
        let mut image = valid_stx();
        image[16..20].copy_from_slice(&hostile.to_le_bytes());
        let path = fixture.write("library/game.stx", &image);
        let evidence = fixture.inspect(&path);
        assert_eq!(
            evidence.format, None,
            "record length {hostile} must be refused"
        );
    }
    // And a fuzzy length that would overflow when added to the header size.
    let mut image = valid_stx();
    image[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    let path = fixture.write("library/game.stx", &image);
    assert_eq!(fixture.inspect(&path).format, None);
}

/// Test 24
#[test]
fn an_invalid_pasti_revision_is_refused() {
    let fixture = Fixture::new("stx-revision");
    let mut image = valid_stx();
    image[0x0B] = 7;
    let path = fixture.write("library/game.stx", &image);
    assert!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .expect("refused")
            .detail()
            .contains("revision 7")
    );
}

/// Test 25
#[test]
fn random_data_with_an_stx_extension_is_refused() {
    let fixture = Fixture::new("stx-random");
    let mut image = vec![0_u8; 8192];
    for (index, byte) in image.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(97).wrapping_add(13);
    }
    let path = fixture.write("library/noise.stx", &image);
    assert_eq!(fixture.inspect(&path).format, None);
}

/// Test 26
#[test]
fn a_pasti_track_table_longer_than_the_budget_stops_at_the_bound() {
    let fixture = Fixture::new("stx-long-table");
    // Records long enough that walking them all would run past the inspection
    // window. The walk must stop at the bound and report what it proved, not
    // claim the rest was checked and not exceed the budget.
    let image = stx_image(3, 160, 512);
    let path = fixture.write("library/big.stx", &image);
    let evidence = fixture.inspect(&path);
    assert_eq!(evidence.format, Some(DiskFormat::AtariStPasti));
    let Some(DiskFormatMetadata::Pasti(layout)) = evidence.metadata else {
        panic!("expected layout");
    };
    assert_eq!(layout.declared_track_records, 160);
    assert!(
        layout.validated_track_records < 160,
        "the walk should have stopped at the inspection bound"
    );
    assert!(layout.validated_track_records > 0);
    assert!(evidence.bytes_inspected <= MAX_DISK_FORMAT_BYTES_READ);
}

// --- Dispatch, symlinks, cancellation, purity -----------------------------

/// Test 27
#[test]
fn an_unhandled_extension_is_refused_without_opening_the_file() {
    let fixture = Fixture::new("dispatch");
    for name in [
        "game.bin",
        "game.iso",
        "game.dsk",
        "RESOURCE.GEN",
        "game.msa",
    ] {
        let path = fixture.write(&format!("library/{name}"), &st_720k());
        let evidence = fixture.inspect(&path);
        assert_eq!(evidence.format, None, "{name} has no adapter");
        assert_eq!(
            evidence.refusal.as_ref().map(DiskFormatRefusal::code),
            Some("no_adapter")
        );
        assert_eq!(evidence.bytes_inspected, 0);
    }
    // And a file with no extension at all.
    let path = fixture.write("library/plain", &st_720k());
    assert_eq!(
        fixture
            .inspect(&path)
            .refusal
            .as_ref()
            .map(DiskFormatRefusal::code),
        Some("no_extension")
    );
}

/// Test 28
#[cfg(unix)]
#[test]
fn a_symlink_inside_the_trusted_roots_is_inspected() {
    let fixture = Fixture::new("symlink-ok");
    let target = fixture.write("downloads/real.st", &st_720k());
    let link = fixture.link("library/game.st", &target);
    let evidence = fixture.inspect(&link);
    assert_eq!(evidence.format, Some(DiskFormat::AtariStRawFloppy));
    assert!(evidence.read_via_symlink);
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.contains("validated symlink target"))
    );
}

/// Test 29
#[cfg(unix)]
#[test]
fn a_symlink_escaping_the_trusted_roots_is_refused_by_the_shared_policy() {
    let fixture = Fixture::new("symlink-escape");
    let target = fixture.write("elsewhere/secret.st", &st_720k());
    let link = fixture.link("library/game.st", &target);
    let evidence = fixture.inspect(&link);
    assert_eq!(evidence.format, None);
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("target_outside_trusted_roots"),
        "the refusal must come from safe_read, not from a policy invented here"
    );
}

/// Test 30
#[cfg(unix)]
#[test]
fn a_broken_or_looping_symlink_is_refused() {
    let fixture = Fixture::new("symlink-broken");
    let broken = fixture.link("library/broken.st", &fixture.path("downloads/absent.st"));
    let first = fixture.path("library/loop-a.st");
    let second = fixture.path("library/loop-b.st");
    std::os::unix::fs::symlink(&second, &first).expect("fixture");
    std::os::unix::fs::symlink(&first, &second).expect("fixture");
    for path in [broken, first] {
        let evidence = fixture.inspect(&path);
        assert_eq!(evidence.format, None, "{} must be refused", path.display());
        assert_eq!(
            evidence.refusal.as_ref().map(DiskFormatRefusal::code),
            Some("unresolvable_target")
        );
    }
}

/// Test 31
#[cfg(unix)]
#[test]
fn a_symlink_is_refused_when_no_trusted_root_is_configured() {
    let fixture = Fixture::new("symlink-fail-closed");
    let target = fixture.write("downloads/real.stx", &valid_stx());
    let link = fixture.link("library/game.stx", &target);
    let evidence = inspect_disk_format(
        &link,
        &TrustedRoots::none(),
        DiskFormatContext::default(),
        None,
    );
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("no_trusted_roots")
    );
}

/// Test 32
#[test]
fn cancellation_is_honoured_between_bounded_steps() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new("cancel");
    let path = fixture.write("library/game.stx", &stx_image(3, 100, 64));
    let cancel = AtomicBool::new(true);
    let evidence = inspect_disk_format(
        &path,
        &fixture.trusted(),
        DiskFormatContext::default(),
        Some(&cancel),
    );
    assert_eq!(
        evidence.refusal.as_ref().map(DiskFormatRefusal::code),
        Some("cancelled")
    );
    // Not cancelled, the same file validates - so the refusal above really was
    // the flag and not a malformed fixture.
    cancel.store(false, Ordering::Relaxed);
    let evidence = inspect_disk_format(
        &path,
        &fixture.trusted(),
        DiskFormatContext::default(),
        Some(&cancel),
    );
    assert_eq!(evidence.format, Some(DiskFormat::AtariStPasti));
}

/// Test 33
#[test]
fn every_inspection_stays_inside_the_documented_read_budget() {
    let fixture = Fixture::new("budget");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("library/a.st", st_720k()),
        ("library/b.st", st_image(1, 10, 80)),
        ("library/c.st", vec![0_u8; 737_280]),
        ("library/d.stx", valid_stx()),
        ("library/e.stx", stx_image(3, 160, 512)),
        ("library/f.stx", stx_image(3, 168, 64)),
    ];
    for (name, image) in cases {
        let path = fixture.write(name, &image);
        let evidence = fixture.inspect(&path);
        assert!(
            evidence.bytes_inspected <= MAX_DISK_FORMAT_BYTES_READ,
            "{name} read {} bytes, over the {MAX_DISK_FORMAT_BYTES_READ} budget",
            evidence.bytes_inspected
        );
    }
}

/// Test 34
#[test]
fn inspection_is_deterministic_and_changes_nothing_on_disk() {
    let fixture = Fixture::new("read-only");
    fixture.write("library/valid.st", &st_720k());
    fixture.write("library/valid.stx", &valid_stx());
    fixture.write("library/noise.st", &vec![7_u8; 737_280]);
    fixture.write("library/short.stx", &valid_stx()[..8]);
    let before = fixture.snapshot();

    let mut first: Vec<String> = Vec::new();
    for name in [
        "valid.st",
        "valid.stx",
        "noise.st",
        "short.stx",
        "absent.st",
    ] {
        let evidence = fixture.inspect(&fixture.path(&format!("library/{name}")));
        first.push(evidence.summary());
    }
    for _ in 0..5 {
        let mut again: Vec<String> = Vec::new();
        for name in [
            "valid.st",
            "valid.stx",
            "noise.st",
            "short.stx",
            "absent.st",
        ] {
            again.push(
                fixture
                    .inspect(&fixture.path(&format!("library/{name}")))
                    .summary(),
            );
        }
        assert_eq!(first, again, "the same inputs must give the same answer");
    }

    assert_eq!(
        fixture.snapshot(),
        before,
        "inspection must not create, remove, modify or re-timestamp anything"
    );
}

/// Test 35
#[test]
fn no_module_here_can_write_extract_mount_or_reach_a_network() {
    for (name, source) in [
        ("mod.rs", include_str!("mod.rs")),
        ("atari_st.rs", include_str!("atari_st.rs")),
        ("atari_stx.rs", include_str!("atari_stx.rs")),
    ] {
        // Comments are stripped first: this module's documentation legitimately
        // uses these words while explaining that it does none of them.
        let code: String = source
            .split("#[cfg(test)]")
            .next()
            .expect("production half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "fs::rename",
            "fs::set_permissions",
            "File::create",
            "OpenOptions",
            "ureq",
            "reqwest",
            "TcpStream",
            "Command",
            "std::process",
            "ZipArchive",
            "mount",
        ] {
            assert!(
                !code.contains(forbidden),
                "`{forbidden}` must never appear in {name}"
            );
        }
    }
}

// --- Through the platform detector ---------------------------------------

/// Test 36
#[test]
fn the_detector_confirms_an_st_image_in_an_atari_st_folder() {
    let fixture = Fixture::new("detect-st-folder");
    let image = fixture.write("library/atari-st/Game.st", &st_720k());
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    assert_eq!(report.platform, Some("AtariST"));
    assert_eq!(
        report.confidence,
        DetectionConfidence::Confirmed,
        "structure plus folder is the case the milestone says is Confirmed: {:?}",
        report.evidence
    );
}

/// Test 37
#[test]
fn the_detector_keeps_an_unlabelled_st_image_at_probable() {
    let fixture = Fixture::new("detect-st-alone");
    let image = fixture.write("library/unsorted/Game.st", &st_720k());
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    assert_eq!(report.platform, Some("AtariST"));
    assert_eq!(
        report.confidence,
        DetectionConfidence::Probable,
        "a FAT12 boot sector is not proof of the platform on its own"
    );
    assert!(
        report.evidence.iter().any(|item| {
            item.source == DetectionSource::Signature && item.detail.contains("raw floppy")
        }),
        "the structural match should still be reported: {:?}",
        report.evidence
    );
}

/// Test 38
#[test]
fn the_detector_confirms_a_pasti_image_with_no_folder_evidence_at_all() {
    let fixture = Fixture::new("detect-stx");
    let image = fixture.write("library/unsorted/Game.stx", &valid_stx());
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    assert_eq!(report.platform, Some("AtariST"));
    assert_eq!(
        report.confidence,
        DetectionConfidence::Confirmed,
        "Pasti is Atari ST specific, so the container alone settles it"
    );
}

/// Test 39
#[test]
fn a_malformed_st_image_falls_back_to_extension_evidence_only() {
    let fixture = Fixture::new("detect-st-malformed");
    let image = fixture.write("library/unsorted/Game.st", &vec![0_u8; 737_280]);
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    // `.st` is still a strong extension, so Atari ST remains the best guess -
    // but no structural claim is made for it.
    assert_eq!(report.platform, Some("AtariST"));
    assert_eq!(report.confidence, DetectionConfidence::Probable);
    assert!(
        !report
            .evidence
            .iter()
            .any(|item| item.source == DetectionSource::Signature),
        "a malformed image must claim no signature: {:?}",
        report.evidence
    );
}

/// Test 40
#[test]
fn a_valid_st_image_under_a_contradicting_folder_keeps_the_folders_platform() {
    let fixture = Fixture::new("detect-st-conflict");
    let image = fixture.write("library/megadrive/Game.st", &st_720k());
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    // The established precedence rule: the folder outranks a contradicting
    // structural match, and the result is Probable with the conflict visible.
    assert_eq!(report.platform, Some("MegaDrive"));
    assert_eq!(report.confidence, DetectionConfidence::Probable);
    assert!(
        report
            .candidates
            .iter()
            .any(|candidate| candidate.platform == "AtariST"),
        "the contradicting structure must stay visible as a candidate"
    );
}

/// Test 41
#[test]
fn a_manual_assignment_still_overrides_a_valid_structure() {
    let fixture = Fixture::new("detect-st-manual");
    let image = fixture.write("library/atari-st/Game.stx", &valid_stx());
    let report = detect_platform_report(
        &DetectionRequest::new(&image, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted())
            .with_manual_platform(Some("Amiga")),
    );
    assert_eq!(report.platform, Some("Amiga"));
    assert!(report.manually_assigned);
}

// --- Regression: nothing else moved ---------------------------------------

/// Test 42: the ScummVM case this project fixed earlier stays fixed - adding a
/// structural adapter must not disturb it.
#[test]
fn scummvm_resource_gen_is_still_scummvm() {
    let fixture = Fixture::new("regression-scummvm");
    fixture.write("library/scummvm/laurabow2/RESOURCE.MAP", b"map");
    let target = fixture.write("library/scummvm/laurabow2/RESOURCE.GEN", b"not a rom");
    let report = detect_platform_report(
        &DetectionRequest::new(&target, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    assert_eq!(report.platform, Some("ScummVM"));
    assert_ne!(report.platform, Some("MegaDrive"));
    assert_ne!(report.platform, Some("AtariST"));
}

/// Test 43: shared extensions stay ambiguous. A `.dsk` is one of the extensions
/// Atari ST shares, so this is exactly where an over-eager adapter would leak.
#[test]
fn generic_shared_extensions_stay_ambiguous() {
    let fixture = Fixture::new("regression-shared");
    for name in ["game.dsk", "game.bin", "game.iso"] {
        // Give each one bytes that would satisfy the ST boot-sector check, so the
        // only thing stopping an Atari ST claim is that no adapter handles the
        // extension.
        let path = fixture.write(&format!("library/unsorted/{name}"), &st_720k());
        let report = detect_platform_report(
            &DetectionRequest::new(&path, &fixture.path("library"))
                .inspecting_content()
                .with_trusted_roots(fixture.trusted()),
        );
        assert_eq!(report.platform, None, "{name} must stay unresolved");
        assert_eq!(
            report.confidence,
            DetectionConfidence::Ambiguous,
            "{name} should list its candidates rather than pick one"
        );
        assert!(
            !report
                .evidence
                .iter()
                .any(|item| item.source == DetectionSource::Signature),
            "{name} must produce no structural claim"
        );
    }
}

/// Test 44: a Mega Drive ROM is still identified by its own header, and an
/// Atari ST adapter has not shadowed it.
#[test]
fn a_mega_drive_rom_is_still_confirmed_from_its_header() {
    let fixture = Fixture::new("regression-megadrive");
    let mut rom = vec![0_u8; 0x200];
    rom[0x100..0x104].copy_from_slice(b"SEGA");
    let path = fixture.write("library/unsorted/mystery.bin", &rom);
    let report = detect_platform_report(
        &DetectionRequest::new(&path, &fixture.path("library"))
            .inspecting_content()
            .with_trusted_roots(fixture.trusted()),
    );
    assert_eq!(report.platform, Some("MegaDrive"));
    assert_eq!(report.confidence, DetectionConfidence::Confirmed);
}

/// Test 45: the schema is untouched. This milestone adds no migration, so the
/// version and the migration list must be exactly what they were.
#[test]
fn the_database_schema_and_migrations_are_unchanged() {
    let versions = crate::database::migration_versions_for_tests();
    assert_eq!(
        versions,
        vec![1_i64, 2, 3, 4, 5, 6],
        "migrations must remain exactly 0001 through 0006"
    );
}
