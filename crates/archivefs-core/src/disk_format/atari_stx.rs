//! Atari ST Pasti image (`.stx`).
//!
//! # What the format is
//!
//! Pasti is a flux-aware preservation container written by the Atari ST imaging
//! tool of the same name. Unlike a raw `.st` dump it does have a header, and it
//! exists only for Atari ST media - so unlike `.st`, a valid one really does
//! settle the platform.
//!
//! The file header, 16 bytes at offset 0:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0x00   | 4    | signature, `RSY\0` |
//! | 0x04   | 2    | version |
//! | 0x06   | 2    | tool that wrote it |
//! | 0x08   | 2    | reserved |
//! | 0x0A   | 1    | track record count |
//! | 0x0B   | 1    | revision |
//! | 0x0C   | 4    | reserved |
//!
//! Then one record per track, each beginning with a 16-byte header:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0x00   | 4    | record length, including this header |
//! | 0x04   | 4    | fuzzy-mask length |
//! | 0x08   | 2    | sector count |
//! | 0x0A   | 2    | flags |
//! | 0x0C   | 2    | MFM track length |
//! | 0x0E   | 1    | track number, high bit = side |
//! | 0x0F   | 1    | record type |
//!
//! # What is validated, and what is not
//!
//! Validated: the signature exactly; that the version is one this build claims
//! to understand; that the declared track count is within a real disk's bounds;
//! and that the track table is internally consistent - every record at least as
//! long as its own header, every record entirely inside the file, and the chain
//! of record lengths reaching the end without overlapping or overflowing.
//!
//! Not validated, and deliberately not read: sector descriptors, fuzzy masks,
//! timing data and track data. The disk is never reconstructed. Walking the
//! table is a bounded traversal of record *headers* only.
//!
//! # Hostile input
//!
//! Every offset and length is attacker-controlled, so all arithmetic is checked
//! and every step is bounded before it is taken. The declared record count never
//! sizes an allocation: the walk is capped by
//! [`MAX_PASTI_TRACK_RECORDS`](super::MAX_PASTI_TRACK_RECORDS) and by the shared
//! read budget, whatever the header claims.

use std::sync::atomic::AtomicBool;

use super::{
    BoundedReader, DiskFormat, DiskFormatContext, DiskFormatEvidence, DiskFormatMetadata,
    DiskFormatRefusal, MAX_PASTI_BYTES, MAX_PASTI_TRACK_RECORDS, PastiLayout, confidence_for,
    le_u16, le_u32,
};

/// The exact file signature.
const SIGNATURE: &[u8; 4] = b"RSY\0";

/// The file header size.
const FILE_HEADER_BYTES: usize = 16;

/// One track record's header size, and the smallest a record may claim to be.
const TRACK_HEADER_BYTES: usize = 16;

/// Versions this build claims to understand. Pasti version 3 is what the tool
/// wrote; refusing anything else is honest rather than guessing at a layout that
/// may differ.
const SUPPORTED_VERSIONS: &[u16] = &[3];

pub(super) fn inspect(
    reader: &mut BoundedReader<'_>,
    context: DiskFormatContext<'_>,
    cancel: Option<&AtomicBool>,
) -> DiskFormatEvidence {
    match validate(reader, cancel) {
        Ok(layout) => {
            let format = DiskFormat::AtariStPasti;
            let (confidence, conclusive) = confidence_for(format, context);
            let mut evidence = vec![
                format!(
                    "Pasti signature `RSY\\0` and version {} at the start of the file",
                    layout.version
                ),
                format!(
                    "Track table: {} record(s) declared, {} walked and internally consistent",
                    layout.declared_track_records, layout.validated_track_records
                ),
                format!(
                    "Those records declare {} sector(s) between them",
                    layout.declared_sectors
                ),
                "Pasti is written only for Atari ST media, so a valid container settles the \
                 platform on its own"
                    .to_string(),
                "Sector, timing and track data were not read: only the record headers were \
                 walked, and the disk was never reconstructed"
                    .to_string(),
            ];
            if let Some(folder) = context.folder_platform
                && folder != format.platform()
            {
                evidence.push(format!(
                    "The containing folder names {folder} instead, so the container and the \
                     folder disagree"
                ));
            }
            DiskFormatEvidence {
                format: Some(format),
                platform: Some(format.platform()),
                confidence,
                conclusive,
                evidence,
                bytes_inspected: reader.bytes_read(),
                refusal: None,
                metadata: Some(DiskFormatMetadata::Pasti(layout)),
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
) -> Result<PastiLayout, DiskFormatRefusal> {
    let length = reader.len();
    let minimum = (FILE_HEADER_BYTES + TRACK_HEADER_BYTES) as u64;
    if length < minimum {
        return Err(DiskFormatRefusal::TooSmall { length, minimum });
    }
    if length > MAX_PASTI_BYTES {
        return Err(DiskFormatRefusal::TooLarge {
            length,
            maximum: MAX_PASTI_BYTES,
        });
    }
    if super::cancelled(cancel) {
        return Err(DiskFormatRefusal::Cancelled);
    }

    let header = reader.read_exact_at(0, FILE_HEADER_BYTES)?;
    let malformed = |detail: String| DiskFormatRefusal::Malformed { detail };

    if header.get(0..4) != Some(&SIGNATURE[..]) {
        return Err(malformed(
            "the file does not begin with the Pasti `RSY\\0` signature".to_string(),
        ));
    }
    let version = le_u16(&header, 0x04).ok_or_else(|| malformed("no version field".to_string()))?;
    if !SUPPORTED_VERSIONS.contains(&version) {
        return Err(malformed(format!(
            "Pasti version {version} is not one this build understands ({SUPPORTED_VERSIONS:?}), \
             so no claim is made about its layout"
        )));
    }
    let tool = le_u16(&header, 0x06).ok_or_else(|| malformed("no tool field".to_string()))?;
    let declared_track_records = *header
        .get(0x0A)
        .ok_or_else(|| malformed("no track-count field".to_string()))?;
    let revision = *header
        .get(0x0B)
        .ok_or_else(|| malformed("no revision field".to_string()))?;
    if revision > 2 {
        return Err(malformed(format!(
            "revision {revision} is outside the documented 0..=2"
        )));
    }
    if declared_track_records == 0 {
        return Err(malformed(
            "the header declares no track records at all".to_string(),
        ));
    }
    if usize::from(declared_track_records) > MAX_PASTI_TRACK_RECORDS {
        return Err(malformed(format!(
            "{declared_track_records} track records is beyond the \
             {MAX_PASTI_TRACK_RECORDS}-record limit for a real Atari ST disk"
        )));
    }

    // Walk the track table. `offset` only ever moves forward by a record length
    // that has already been proven to fit inside the file, so the walk cannot
    // loop, cannot overlap and cannot leave the file.
    let mut offset = FILE_HEADER_BYTES as u64;
    let mut declared_sectors: u32 = 0;
    let mut validated = 0_usize;
    for index in 0..usize::from(declared_track_records) {
        if super::cancelled(cancel) {
            return Err(DiskFormatRefusal::Cancelled);
        }
        // The record header itself must be inside the file before it is read.
        let header_end = offset
            .checked_add(TRACK_HEADER_BYTES as u64)
            .ok_or_else(|| malformed("a track offset overflowed".to_string()))?;
        if header_end > length {
            return Err(DiskFormatRefusal::Truncated {
                offset,
                wanted: TRACK_HEADER_BYTES,
            });
        }
        // Past the module's inspection window, stop walking and report what was
        // proven rather than pretending the rest was checked. This is a bound,
        // not a failure: everything walked so far really was consistent.
        if offset > super::MAX_DISK_FORMAT_OFFSET
            || reader
                .bytes_read()
                .saturating_add(TRACK_HEADER_BYTES as u64)
                > super::MAX_DISK_FORMAT_BYTES_READ
        {
            break;
        }

        let track = reader.read_exact_at(offset, TRACK_HEADER_BYTES)?;
        let record_length = le_u32(&track, 0x00)
            .ok_or_else(|| malformed(format!("track {index} has no record length")))?;
        let fuzzy_length = le_u32(&track, 0x04)
            .ok_or_else(|| malformed(format!("track {index} has no fuzzy length")))?;
        let sector_count = le_u16(&track, 0x08)
            .ok_or_else(|| malformed(format!("track {index} has no sector count")))?;

        if (record_length as usize) < TRACK_HEADER_BYTES {
            return Err(malformed(format!(
                "track {index} declares a {record_length}-byte record, shorter than its own \
                 {TRACK_HEADER_BYTES}-byte header"
            )));
        }
        // The fuzzy mask lives inside the record, so it cannot be longer than it.
        let header_and_fuzzy = (TRACK_HEADER_BYTES as u64)
            .checked_add(u64::from(fuzzy_length))
            .ok_or_else(|| malformed(format!("track {index} fuzzy length overflows")))?;
        if header_and_fuzzy > u64::from(record_length) {
            return Err(malformed(format!(
                "track {index} declares a {fuzzy_length}-byte fuzzy mask that does not fit in \
                 its {record_length}-byte record"
            )));
        }
        let next = offset
            .checked_add(u64::from(record_length))
            .ok_or_else(|| malformed(format!("track {index} record length overflows")))?;
        if next > length {
            return Err(malformed(format!(
                "track {index} claims to end at {next}, past the file's {length} bytes"
            )));
        }
        declared_sectors = declared_sectors.saturating_add(u32::from(sector_count));
        offset = next;
        validated = validated
            .checked_add(1)
            .ok_or_else(|| malformed("the record counter overflowed".to_string()))?;
    }

    if validated == 0 {
        return Err(malformed("no track record could be validated".to_string()));
    }

    Ok(PastiLayout {
        version,
        tool,
        revision,
        declared_track_records,
        validated_track_records: validated,
        declared_sectors,
    })
}
