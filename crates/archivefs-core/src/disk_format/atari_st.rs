//! Atari ST raw floppy image (`.st`).
//!
//! # What the format actually is
//!
//! Nothing. A `.st` file is a raw, headerless dump of every sector of an Atari
//! ST floppy, in order, with no container and no signature. So there is no magic
//! number to compare, and a fixed-offset byte check cannot recognise one. What
//! *can* be checked is the first sector, which on a TOS-formatted disk is a
//! FAT12 BIOS Parameter Block, and whether the geometry it declares accounts for
//! exactly as many bytes as the file contains.
//!
//! The BPB fields this reads, at their documented offsets:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0x0B   | 2    | bytes per sector |
//! | 0x0D   | 1    | sectors per cluster |
//! | 0x0E   | 2    | reserved (boot) sectors |
//! | 0x10   | 1    | number of FATs |
//! | 0x11   | 2    | root directory entries |
//! | 0x13   | 2    | total sectors |
//! | 0x16   | 2    | sectors per FAT |
//! | 0x18   | 2    | sectors per track |
//! | 0x1A   | 2    | number of sides |
//!
//! # What a valid one proves, and what it does not
//!
//! It proves the file is a whole number of 512-byte sectors, that its first
//! sector is a coherent FAT12 BPB, that the geometry is one an Atari ST drive
//! could actually produce, and that the geometry accounts for the file's length
//! exactly.
//!
//! It does **not** prove the platform. This is the same boot-sector structure a
//! PC DOS 720 KB floppy carries, and a 720 KB DOS image would satisfy every
//! check here. That is why [`DiskFormat::proves_platform`] is `false` for this
//! format and why a match alone is reported as Probable. Claiming otherwise
//! would be inventing certainty that the bytes do not contain.
//!
//! Reads exactly one 512-byte sector. Nothing else is touched: no FAT, no root
//! directory, no file data.

use std::sync::atomic::AtomicBool;

use super::{
    BoundedReader, DiskFormat, DiskFormatContext, DiskFormatEvidence, DiskFormatMetadata,
    DiskFormatRefusal, FLOPPY_SECTOR_BYTES, FloppyGeometry, MAX_RAW_FLOPPY_BYTES, confidence_for,
    le_u16,
};

/// The boot sector, and the only thing read.
const BOOT_SECTOR_BYTES: usize = 512;

/// Sectors per track an Atari ST drive can produce. Nine is the TOS default;
/// ten and eleven are the standard "extended format" densities, and the ST's own
/// formatter would not go outside this range.
const SECTORS_PER_TRACK: std::ops::RangeInclusive<u16> = 8..=12;

/// Tracks per side. Eighty is standard; a few more were commonly squeezed on,
/// and eighty-six is past anything a real drive wrote.
const TRACKS_PER_SIDE: std::ops::RangeInclusive<u16> = 74..=86;

/// Root directory entries. Always a multiple of sixteen because a directory
/// entry is 32 bytes and a sector holds sixteen of them.
const ROOT_DIRECTORY_ENTRIES: std::ops::RangeInclusive<u16> = 16..=1024;

const SECTORS_PER_FAT: std::ops::RangeInclusive<u16> = 1..=32;
const RESERVED_SECTORS: std::ops::RangeInclusive<u16> = 1..=8;

/// Validates one `.st` image.
pub(super) fn inspect(
    reader: &mut BoundedReader<'_>,
    context: DiskFormatContext<'_>,
    cancel: Option<&AtomicBool>,
) -> DiskFormatEvidence {
    match validate(reader, cancel) {
        Ok(geometry) => {
            let format = DiskFormat::AtariStRawFloppy;
            let (confidence, conclusive) = confidence_for(format, context);
            let mut evidence = vec![
                format!("Geometry: {}", geometry.summary()),
                format!(
                    "The declared geometry accounts for exactly the file's {} bytes",
                    reader.len()
                ),
                format!(
                    "FAT12 boot sector: {} FAT(s) of {} sector(s), {} root entries, \
                     {} sector(s) per cluster",
                    geometry.fat_count,
                    geometry.sectors_per_fat,
                    geometry.root_directory_entries,
                    geometry.sectors_per_cluster
                ),
            ];
            if !format.proves_platform() {
                // Said on every match, because it is the honest limit of what
                // this structure can show.
                evidence.push(
                    "A raw floppy dump carries no Atari ST marker: this boot sector is the same \
                     FAT12 structure a PC DOS floppy of the same geometry has, so the structure \
                     narrows the platform rather than proving it"
                        .to_string(),
                );
            }
            if let Some(folder) = context.folder_platform {
                evidence.push(if folder == format.platform() {
                    "The containing folder names the same platform, which is what raises this \
                     to confirmed"
                        .to_string()
                } else {
                    format!(
                        "The containing folder names {folder} instead, so the structure and the \
                         folder disagree"
                    )
                });
            }
            DiskFormatEvidence {
                format: Some(format),
                platform: Some(format.platform()),
                confidence,
                conclusive,
                evidence,
                bytes_inspected: reader.bytes_read(),
                refusal: None,
                metadata: Some(DiskFormatMetadata::Floppy(geometry)),
                read_via_symlink: false,
            }
        }
        Err(refusal) => {
            let mut refused = DiskFormatEvidence::refused(refusal);
            refused.bytes_inspected = reader.bytes_read();
            refused
        }
    }
}

fn validate(
    reader: &mut BoundedReader<'_>,
    cancel: Option<&AtomicBool>,
) -> Result<FloppyGeometry, DiskFormatRefusal> {
    let length = reader.len();
    let sector_bytes = u64::from(FLOPPY_SECTOR_BYTES);

    // Cheap structural facts first, so a file that cannot possibly be a floppy
    // image is refused without any read at all.
    if length < sector_bytes {
        return Err(DiskFormatRefusal::TooSmall {
            length,
            minimum: sector_bytes,
        });
    }
    if length > MAX_RAW_FLOPPY_BYTES {
        return Err(DiskFormatRefusal::TooLarge {
            length,
            maximum: MAX_RAW_FLOPPY_BYTES,
        });
    }
    if !length.is_multiple_of(sector_bytes) {
        return Err(DiskFormatRefusal::NotSectorAligned {
            length,
            sector_bytes: FLOPPY_SECTOR_BYTES,
        });
    }
    if super::cancelled(cancel) {
        return Err(DiskFormatRefusal::Cancelled);
    }

    let boot = reader.read_exact_at(0, BOOT_SECTOR_BYTES)?;
    let malformed = |detail: String| DiskFormatRefusal::Malformed { detail };
    let field = |offset: usize, name: &str| -> Result<u16, DiskFormatRefusal> {
        le_u16(&boot, offset).ok_or_else(|| malformed(format!("the boot sector has no {name}")))
    };

    let bytes_per_sector = field(0x0B, "bytes-per-sector field")?;
    if u32::from(bytes_per_sector) != FLOPPY_SECTOR_BYTES {
        return Err(malformed(format!(
            "the boot sector declares {bytes_per_sector}-byte sectors; an Atari ST TOS floppy \
             always uses {FLOPPY_SECTOR_BYTES}"
        )));
    }

    let sectors_per_cluster = *boot
        .get(0x0D)
        .ok_or_else(|| malformed("the boot sector has no sectors-per-cluster field".to_string()))?;
    if sectors_per_cluster == 0 || sectors_per_cluster > 8 || !sectors_per_cluster.is_power_of_two()
    {
        return Err(malformed(format!(
            "{sectors_per_cluster} sectors per cluster is not a power of two in 1..=8"
        )));
    }

    let reserved_sectors = field(0x0E, "reserved-sectors field")?;
    if !RESERVED_SECTORS.contains(&reserved_sectors) {
        return Err(malformed(format!(
            "{reserved_sectors} reserved sectors is outside {RESERVED_SECTORS:?}"
        )));
    }

    let fat_count = *boot
        .get(0x10)
        .ok_or_else(|| malformed("the boot sector has no FAT-count field".to_string()))?;
    if fat_count == 0 || fat_count > 2 {
        return Err(malformed(format!(
            "{fat_count} file allocation tables is not 1 or 2"
        )));
    }

    let root_directory_entries = field(0x11, "root-directory-entries field")?;
    if !ROOT_DIRECTORY_ENTRIES.contains(&root_directory_entries) || root_directory_entries % 16 != 0
    {
        return Err(malformed(format!(
            "{root_directory_entries} root directory entries is not a multiple of 16 in \
             {ROOT_DIRECTORY_ENTRIES:?}"
        )));
    }

    let total_sectors = u32::from(field(0x13, "total-sectors field")?);
    if total_sectors == 0 {
        return Err(malformed(
            "the boot sector declares zero total sectors".to_string(),
        ));
    }

    let sectors_per_fat = field(0x16, "sectors-per-FAT field")?;
    if !SECTORS_PER_FAT.contains(&sectors_per_fat) {
        return Err(malformed(format!(
            "{sectors_per_fat} sectors per FAT is outside {SECTORS_PER_FAT:?}"
        )));
    }

    let sectors_per_track = field(0x18, "sectors-per-track field")?;
    if !SECTORS_PER_TRACK.contains(&sectors_per_track) {
        return Err(malformed(format!(
            "{sectors_per_track} sectors per track is outside {SECTORS_PER_TRACK:?}, which no \
             Atari ST drive produces"
        )));
    }

    let sides = field(0x1A, "sides field")?;
    if sides == 0 || sides > 2 {
        return Err(malformed(format!("{sides} sides is not 1 or 2")));
    }

    // The geometry must be self-consistent: a whole number of tracks, in a range
    // a real drive wrote. Checked arithmetic throughout - these are attacker
    // controlled numbers.
    let sectors_per_cylinder = u32::from(sectors_per_track)
        .checked_mul(u32::from(sides))
        .ok_or_else(|| malformed("the geometry overflows".to_string()))?;
    if sectors_per_cylinder == 0 || total_sectors % sectors_per_cylinder != 0 {
        return Err(malformed(format!(
            "{total_sectors} total sectors is not a whole number of tracks at \
             {sectors_per_track} sectors x {sides} side(s)"
        )));
    }
    let tracks_u32 = total_sectors / sectors_per_cylinder;
    let tracks = u16::try_from(tracks_u32).map_err(|_| {
        malformed(format!(
            "{tracks_u32} tracks is not a plausible track count"
        ))
    })?;
    if !TRACKS_PER_SIDE.contains(&tracks) {
        return Err(malformed(format!(
            "{tracks} tracks per side is outside {TRACKS_PER_SIDE:?}"
        )));
    }

    // The declared geometry must account for the file exactly. A shorter file is
    // truncated; a longer one is not the disk its own boot sector describes.
    let declared_bytes = u64::from(total_sectors)
        .checked_mul(sector_bytes)
        .ok_or_else(|| malformed("the declared size overflows".to_string()))?;
    if declared_bytes != length {
        return Err(DiskFormatRefusal::GeometryMismatch {
            declared_bytes,
            actual_bytes: length,
        });
    }

    // The metadata area must fit inside the disk it claims to describe.
    let root_sectors = u32::from(root_directory_entries)
        .checked_mul(32)
        .and_then(|bytes| bytes.checked_div(FLOPPY_SECTOR_BYTES))
        .ok_or_else(|| malformed("the root directory size overflows".to_string()))?;
    let metadata_sectors = u32::from(reserved_sectors)
        .checked_add(
            u32::from(fat_count)
                .checked_mul(u32::from(sectors_per_fat))
                .ok_or_else(|| malformed("the FAT size overflows".to_string()))?,
        )
        .and_then(|used| used.checked_add(root_sectors))
        .ok_or_else(|| malformed("the metadata size overflows".to_string()))?;
    if metadata_sectors >= total_sectors {
        return Err(malformed(format!(
            "the boot sector, FATs and root directory need {metadata_sectors} sectors, which \
             leaves no room in {total_sectors}"
        )));
    }

    Ok(FloppyGeometry {
        bytes_per_sector,
        sectors_per_cluster,
        reserved_sectors,
        fat_count,
        root_directory_entries,
        total_sectors,
        sectors_per_fat,
        sectors_per_track,
        sides,
        tracks,
    })
}
