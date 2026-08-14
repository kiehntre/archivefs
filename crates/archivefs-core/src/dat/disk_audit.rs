//! CHD-aware disk evidence.
//!
//! Matches one physical `.chd` file's header identity against the DAT's
//! declared disk SHA-1s. Deliberately separate from the ROM evidence path in
//! [`crate::dat::audit`]: a CHD's DAT identity is its header's
//! `overall_sha1` field, not a hash of the `.chd` file's own bytes, so this
//! module never builds a [`crate::dat::audit::KnownFileEvidence`], never
//! calls [`crate::dat::audit::audit_one`], never opens the file as a ZIP/7z
//! archive member, and never touches [`crate::dat::index::DatIndex`] (the
//! ROM-hash index). The only lookup this module ever performs is against
//! [`crate::dat::index::DatDiskIndex::by_disk_sha1`].
//!
//! This module reads at most the bounded CHD v5 header
//! ([`crate::dat::archive::chd::read_chd_v5_header`]) - it never opens the
//! CHD map, never decompresses a hunk, and never verifies `raw_sha1` against
//! reconstructed content. Identity only.

use std::path::{Path, PathBuf};

use super::archive::chd::{ChdHeaderError, read_chd_v5_header};
use super::index::{DatDiskIndex, DatDiskRef};
use crate::safe_read::{TrustedRoots, open_bounded_read};

/// The outcome of matching one CHD's header identity against the DAT's
/// declared disk SHA-1s.
///
/// Deliberately mirrors [`crate::dat::audit::AuditVerdict`]'s Exact /
/// ExactMultipleCandidates / NotInDat shape rather than reusing that type: a
/// disk verdict has no `rom_name`, no CRC32/filename-only tiers (a CHD gives
/// only one identity field), and its own malformed-header refusal reason.
#[derive(Debug)]
pub enum DiskAuditVerdict {
    /// The header's `overall_sha1` matched exactly one DAT disk entry.
    Exact {
        game_name: String,
        disk_name: String,
    },
    /// The header's `overall_sha1` matched more than one DAT disk entry.
    /// Never resolved here - every named candidate stays unattributed.
    ExactMultipleCandidates {
        count: usize,
        game_names: Vec<String>,
    },
    /// The header parsed and validated, but its `overall_sha1` matches no
    /// declared disk in this DAT.
    NotInDat,
    /// The file could not be opened, or its CHD v5 header could not be read
    /// or validated. Never treated as a match - see
    /// [`crate::dat::archive::chd::ChdHeaderError`] for the exact reason.
    HeaderMalformed(ChdHeaderError),
}

/// One `.chd` file's disk evidence.
///
/// `matched_refs` follows the same rule the ROM bridge's
/// `DatArchiveMemberAudit::matched_refs` does: non-empty means authoritative
/// positional evidence, populated only for [`DiskAuditVerdict::Exact`] and
/// [`DiskAuditVerdict::ExactMultipleCandidates`] - never reconstructed from
/// `disk_name` or any other display metadata.
#[derive(Debug)]
pub struct DatDiskAudit {
    pub chd_path: PathBuf,
    /// The header's normalised lowercase 40-hex `overall_sha1`, when the
    /// header could be read at all.
    pub overall_sha1: Option<String>,
    /// The header's `parent_required()` fact, surfaced but never resolved
    /// (dependency-chain resolution is Stage 2d's job, not this batch's).
    pub parent_required: bool,
    pub verdict: Option<DiskAuditVerdict>,
    pub matched_refs: Vec<DatDiskRef>,
}

/// Reads one `.chd` file's header and matches its `overall_sha1` against
/// `index`.
///
/// Opens the file through [`open_bounded_read`], the same trusted-root/
/// symlink-safe read path the rest of the audit pipeline uses - never a raw
/// `File::open`. A header that cannot be opened, parsed, or validated
/// produces [`DiskAuditVerdict::HeaderMalformed`] with `matched_refs` empty;
/// never a guess, never `Exact`.
pub fn audit_chd_disk(
    chd_path: &Path,
    trusted: &TrustedRoots,
    index: &DatDiskIndex,
) -> DatDiskAudit {
    let file = match open_bounded_read(chd_path, trusted) {
        Ok(safe_file) => safe_file.into_file(),
        Err(refusal) => {
            return DatDiskAudit {
                chd_path: chd_path.to_path_buf(),
                overall_sha1: None,
                parent_required: false,
                verdict: Some(DiskAuditVerdict::HeaderMalformed(ChdHeaderError::Io(
                    std::io::Error::other(refusal.detail()),
                ))),
                matched_refs: Vec::new(),
            };
        }
    };
    let mut reader = file;

    let header = match read_chd_v5_header(&mut reader) {
        Ok(header) => header,
        Err(error) => {
            return DatDiskAudit {
                chd_path: chd_path.to_path_buf(),
                overall_sha1: None,
                parent_required: false,
                verdict: Some(DiskAuditVerdict::HeaderMalformed(error)),
                matched_refs: Vec::new(),
            };
        }
    };

    let overall_sha1 = hex_lower(&header.overall_sha1);
    let parent_required = header.parent_required();
    let candidates = index.lookup_disk_sha1(&overall_sha1);

    let (verdict, matched_refs) = match candidates.len() {
        0 => (DiskAuditVerdict::NotInDat, Vec::new()),
        1 => (
            DiskAuditVerdict::Exact {
                game_name: candidates[0].game_name.clone(),
                disk_name: candidates[0].disk_name.clone(),
            },
            candidates.to_vec(),
        ),
        count => (
            DiskAuditVerdict::ExactMultipleCandidates {
                count,
                game_names: candidates
                    .iter()
                    .map(|candidate| candidate.game_name.clone())
                    .collect(),
            },
            candidates.to_vec(),
        ),
    };

    DatDiskAudit {
        chd_path: chd_path.to_path_buf(),
        overall_sha1: Some(overall_sha1),
        parent_required,
        verdict: Some(verdict),
        matched_refs,
    }
}

/// Whether `path` looks like a CHD by extension. Dispatch is by extension
/// only, matching how the ZIP/7z sources decide what to open - this never
/// sniffs file contents to pick a format.
pub fn is_chd_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("chd"))
}

fn hex_lower(bytes: &[u8; 20]) -> String {
    let mut out = String::with_capacity(40);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;
    use crate::dat::model::{
        DatDiskEntry, DatEcosystem, DatFormat, DatGameEntry, DatSource, ParsedDat,
    };

    const RAW_SHA1: [u8; 20] = [0x11; 20];
    const PARENT_SHA1_ZERO: [u8; 20] = [0x00; 20];

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    /// Builds a syntactically valid CHD v5 header (124 bytes) with the given
    /// `overall_sha1` and `parent_sha1`, mirroring the fixture in
    /// `archive::chd`'s own tests.
    fn synthetic_chd_header(overall_sha1: [u8; 20], parent_sha1: [u8; 20]) -> Vec<u8> {
        let mut bytes = [0_u8; 124];
        bytes[0..8].copy_from_slice(b"MComprHD");
        put_u32(&mut bytes, 8, 124);
        put_u32(&mut bytes, 12, 5);
        put_u64(&mut bytes, 32, 4096);
        put_u64(&mut bytes, 40, 0);
        put_u64(&mut bytes, 48, 0);
        put_u32(&mut bytes, 56, 0x0002_0000);
        put_u32(&mut bytes, 60, 0x0000_0800);
        bytes[64..84].copy_from_slice(&RAW_SHA1);
        bytes[84..104].copy_from_slice(&overall_sha1);
        bytes[104..124].copy_from_slice(&parent_sha1);
        bytes.to_vec()
    }

    fn write_chd(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    fn disk(name: &str, sha1_byte: u8) -> DatDiskEntry {
        DatDiskEntry {
            name: Some(name.to_string()),
            sha1: Some(hex_lower(&[sha1_byte; 20])),
            merge: None,
            region: None,
            index: None,
            writable: None,
            status: None,
            optional: None,
        }
    }

    fn dat_with_disks(games: Vec<(&str, Vec<DatDiskEntry>)>) -> ParsedDat {
        ParsedDat {
            source: DatSource {
                format: DatFormat::Logiqx,
                ecosystem: DatEcosystem::GenericLogiqx,
                file_path: "test.dat".into(),
                name: None,
                description: None,
                version: None,
                author: None,
                homepage: None,
                clrmamepro_header: None,
                entry_count: games.len(),
                rom_count: 0,
                parse_warnings: Vec::new(),
            },
            games: games
                .into_iter()
                .map(|(name, disks)| DatGameEntry {
                    name: name.to_string(),
                    description: None,
                    roms: Vec::new(),
                    clone_of: None,
                    sample_of: None,
                    disks,
                    board: None,
                    rebuild_to: None,
                    year: None,
                    manufacturer: None,
                    source_file: None,
                    comment: None,
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn overall_sha1_exact_single_match_is_reported_and_matched_refs_has_one_entry() {
        let dir = tempdir().unwrap();
        let bytes = synthetic_chd_header([0xaa; 20], PARENT_SHA1_ZERO);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0xaa)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(matches!(
            audit.verdict,
            Some(DiskAuditVerdict::Exact { ref game_name, ref disk_name })
                if game_name == "Game" && disk_name == "disc.chd"
        ));
        assert_eq!(audit.matched_refs.len(), 1);
        assert_eq!(
            audit.overall_sha1.as_deref(),
            Some(hex_lower(&[0xaa; 20]).as_str())
        );
    }

    #[test]
    fn overall_sha1_matching_multiple_dat_disks_retains_every_candidate() {
        let dir = tempdir().unwrap();
        let bytes = synthetic_chd_header([0xbb; 20], PARENT_SHA1_ZERO);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![
            ("Game A", vec![disk("a.chd", 0xbb)]),
            ("Game B", vec![disk("b.chd", 0xbb)]),
        ]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        match audit.verdict {
            Some(DiskAuditVerdict::ExactMultipleCandidates { count, game_names }) => {
                assert_eq!(count, 2);
                assert_eq!(game_names, vec!["Game A".to_string(), "Game B".to_string()]);
            }
            other => panic!("expected ExactMultipleCandidates, got {other:?}"),
        }
        assert_eq!(audit.matched_refs.len(), 2, "no silent selection");
    }

    #[test]
    fn overall_sha1_matching_no_dat_disk_is_not_in_dat() {
        let dir = tempdir().unwrap();
        let bytes = synthetic_chd_header([0xcc; 20], PARENT_SHA1_ZERO);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0xdd)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(matches!(audit.verdict, Some(DiskAuditVerdict::NotInDat)));
        assert!(audit.matched_refs.is_empty());
    }

    #[test]
    fn truncated_chd_is_header_malformed_never_exact() {
        let dir = tempdir().unwrap();
        let full = synthetic_chd_header([0xee; 20], PARENT_SHA1_ZERO);
        let path = write_chd(dir.path(), "disc.chd", &full[..100]);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0xee)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(matches!(
            audit.verdict,
            Some(DiskAuditVerdict::HeaderMalformed(
                ChdHeaderError::Truncated { .. }
            ))
        ));
        assert!(audit.matched_refs.is_empty());
    }

    #[test]
    fn unsupported_chd_version_is_header_malformed_never_exact() {
        let dir = tempdir().unwrap();
        let mut bytes = synthetic_chd_header([0xff; 20], PARENT_SHA1_ZERO);
        put_u32(&mut bytes, 8, 108);
        put_u32(&mut bytes, 12, 4);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0xff)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(matches!(
            audit.verdict,
            Some(DiskAuditVerdict::HeaderMalformed(
                ChdHeaderError::UnsupportedVersion { found: 4 }
            ))
        ));
    }

    #[test]
    fn zero_parent_sha1_reports_parent_not_required() {
        let dir = tempdir().unwrap();
        let bytes = synthetic_chd_header([0x12; 20], [0; 20]);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0x12)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(!audit.parent_required);
        assert!(matches!(
            audit.verdict,
            Some(DiskAuditVerdict::Exact { .. })
        ));
    }

    #[test]
    fn nonzero_parent_sha1_reports_parent_required_and_still_reaches_exact() {
        let dir = tempdir().unwrap();
        let mut parent = [0_u8; 20];
        parent[19] = 1;
        let bytes = synthetic_chd_header([0x34; 20], parent);
        let path = write_chd(dir.path(), "disc.chd", &bytes);

        let dat = dat_with_disks(vec![("Game", vec![disk("disc.chd", 0x34)])]);
        let index = DatDiskIndex::build(&dat);

        let audit = audit_chd_disk(&path, &TrustedRoots::none(), &index);

        assert!(audit.parent_required);
        assert!(matches!(
            audit.verdict,
            Some(DiskAuditVerdict::Exact { .. })
        ));
    }

    #[test]
    fn is_chd_path_matches_only_the_chd_extension_case_insensitively() {
        assert!(is_chd_path(Path::new("disc.chd")));
        assert!(is_chd_path(Path::new("disc.CHD")));
        assert!(!is_chd_path(Path::new("disc.zip")));
        assert!(!is_chd_path(Path::new("disc")));
    }

    #[test]
    fn hex_lower_matches_manual_formatting() {
        assert_eq!(hex_lower(&[0; 20]), "0".repeat(40));
        let mut bytes = [0_u8; 20];
        bytes[0] = 0xab;
        assert_eq!(hex_lower(&bytes), format!("ab{}", "0".repeat(38)));
    }
}
