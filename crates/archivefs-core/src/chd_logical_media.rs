//! Pure, read-only CHD logical-data reading: the connector between
//! [`crate::chd_identity`]'s CHD container/media observation and
//! [`crate::iso9660`]'s filesystem observation.
//!
//! ```text
//! .chd -> CHD logical reader -> selected data track -> sector payload
//!      -> LogicalMedia -> ISO9660 -> root files / interesting paths
//! ```
//!
//! Hunk decompression itself is not hand-implemented here. CHD v5's hunk
//! map is, when compressed, a separately huffman-coded structure (verified
//! against MAME's `chd.cpp`/`chd.h`), and the CD-specific codecs (`cdlz`,
//! `cdzl`, `cdfl`) each wrap a different underlying compressor. Reproducing
//! all of that correctly and safely is a large undertaking with real risk
//! of subtly wrong output - exactly the kind of thing this crate's own
//! policy says to reuse a mature dependency for rather than rebuild. This
//! module instead adapts the [`chd`](https://docs.rs/chd) crate
//! (`chd-rs`, BSD-3-Clause, by SnowflakePowered): a read-only,
//! dependency-free-of-C, pure-Rust reimplementation of CHD v1-5 that
//! already covers every codec our real samples use (backed by `flate2`
//! for zlib, `lzma-rs` for LZMA, `claxon` for FLAC - no C/C++ toolchain
//! anywhere in that chain) and already reports a missing parent as its own
//! `Error::RequiresParent`, matching this crate's existing
//! "`NeedsParent` is not `Malformed`" principle.
//!
//! What this module *does* hand-write, because it is CHD/CD-domain
//! knowledge `chd-rs` deliberately leaves to its caller, is: which track's
//! bytes to read (built on [`crate::chd_identity::select_candidate_data_track`]),
//! and how to turn one raw 2352-byte CD sector into the 2048-byte logical
//! block a filesystem reader expects. Both are verified against primary
//! sources - see [`extract_user_data`]'s documentation for the sector
//! layout, and the module-level safety notes below for the track-mapping
//! assumptions this first implementation deliberately limits itself to.
//!
//! # What this chunk deliberately does not support (fails closed, not guessed)
//!
//! - Only **track 1** of the CHD is supported as the data track. Computing
//!   a later track's starting offset correctly requires verifying how
//!   `chdman` accounts for every preceding track's pregap/postgap in the
//!   physical layout, which has not been done here - see
//!   [`ChdLogicalMediaError::UnsupportedTrackPosition`].
//! - Only a **zero pregap** on that track is supported, for the same
//!   reason - see [`ChdLogicalMediaError::UnsupportedPregap`].
//! - Only `MODE1_RAW` and `MODE2_RAW` track types are interpreted; for
//!   `MODE2_RAW`, only Form 1 sectors are supported - a Form 2 sector
//!   encountered mid-read is a [`LogicalMediaError::DecodeFailed`], not a
//!   silently wrong 2048 bytes. See [`extract_user_data`].
//! - No metadata/codec/frame chain is ever written back; this module has
//!   no write path at all, matching `chd-rs` itself (read-only by design).
//!
//! # A verified, honestly-documented risk
//!
//! An uncompressed CHD hunk whose bytes are entirely absent from the
//! physical file (a genuinely truncated/corrupt `.chd`) decodes as an
//! all-zero hunk rather than an error. This is not a bug introduced by
//! this adapter - it is `std::io::Read`'s own short-read-at-EOF semantics,
//! which `chd-rs`'s uncompressed-hunk path does not itself reject - and it
//! is verified, not assumed: see the test
//! `absent_trailing_hunk_bytes_read_as_zero_not_an_error`. A missing *map*
//! entry (the map itself is read eagerly, in full, at open time) does fail
//! closed - see `malformed_truncated_map_fails_closed`.

use std::cell::RefCell;
use std::io::Cursor;

use chd::Chd;
use chd::header::Header;

use crate::chd_identity::{ChdMetadataOutcome, observe_chd_identity, select_candidate_data_track};
use crate::dat::archive::chd::ChdHeaderError;
use crate::logical_media::{LogicalMedia, LogicalMediaError};

/// The size of one raw CD sector, verified against MAME's
/// `cdrom.h` (`MAX_SECTOR_DATA = 2352`).
pub const RAW_SECTOR_BYTES: usize = 2352;

/// The size of the logical block this adapter exposes to a filesystem
/// reader - the conventional ISO9660/CD-ROM user-data block size.
pub const LOGICAL_BLOCK_BYTES: usize = 2048;

/// `MODE1` user-data offset within a raw sector: sync(12) + header(4).
/// Verified against `simias/cdimage`'s `sector.rs` (an independently
/// authored, working CD sector reader) and cross-checked against ECMA-130's
/// documented Mode 1 field sizes (12+4+2048+4+8+172+104 = 2352).
pub const MODE1_USER_DATA_OFFSET: usize = 16;

/// `MODE2 Form 1` user-data offset within a raw sector: sync(12) +
/// header(4) + subheader(8). Verified against the same source as
/// [`MODE1_USER_DATA_OFFSET`] (`self.data[24..2072]` for Form 1).
pub const MODE2_FORM1_USER_DATA_OFFSET: usize = 24;

/// The byte offset, within a raw sector, of the XA subheader's `submode`
/// byte (first of its two duplicated copies). Verified against the same
/// source.
pub const MODE2_SUBMODE_OFFSET: usize = 18;

/// The bit of the XA submode byte that is set for a Form 2 sector and
/// clear for a Form 1 sector. Verified against the same source
/// (`self.data[18] & (1 << 5) != 0` => Form 2).
pub const MODE2_SUBMODE_FORM2_BIT: u8 = 1 << 5;

/// Which recorded CD/GD-ROM track type this module knows how to extract
/// filesystem-relevant user data from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Mode1Raw,
    Mode2Raw,
}

/// Extracts the 2048-byte logical block from one raw `sector` (must be
/// exactly [`RAW_SECTOR_BYTES`] long), according to `kind`.
///
/// For `Mode2Raw`, a Form 2 sector (submode bit `0x20` set) is refused
/// rather than misread - Form 2's own user-data field is 2324 bytes, not
/// 2048, and treating it as a plain 2048-byte block would silently corrupt
/// every byte read from that sector onward. See the module documentation.
fn extract_user_data(sector: &[u8], kind: TrackKind) -> Result<&[u8], String> {
    debug_assert_eq!(sector.len(), RAW_SECTOR_BYTES);
    match kind {
        TrackKind::Mode1Raw => {
            Ok(&sector[MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES])
        }
        TrackKind::Mode2Raw => {
            let submode = sector[MODE2_SUBMODE_OFFSET];
            if submode & MODE2_SUBMODE_FORM2_BIT != 0 {
                Err(
                    "encountered a CD-XA Mode 2 Form 2 sector; only Form 1 sectors are \
                     supported for filesystem reads"
                        .to_string(),
                )
            } else {
                Ok(&sector[MODE2_FORM1_USER_DATA_OFFSET
                    ..MODE2_FORM1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES])
            }
        }
    }
}

/// Why [`open_chd_track_logical_media`] could not produce a
/// [`ChdTrackLogicalMedia`].
#[derive(Debug)]
pub enum ChdLogicalMediaError {
    /// The CHD v5 header itself did not parse - see
    /// [`crate::chd_identity::observe_chd_identity`].
    Header(ChdHeaderError),
    /// This CHD requires a parent CHD. Not a corruption signal - see the
    /// crate's existing `parent_required` principle in
    /// [`crate::chd_identity`]. Carries the parent's combined SHA-1 so a
    /// caller could locate it, though this module never searches for one
    /// itself.
    NeedsParent { parent_sha1: [u8; 20] },
    /// No non-audio CD/GD-ROM track was found in this CHD's metadata.
    NoDataTrack,
    /// The selected data track's `track_type` is not one this module
    /// interprets (only `MODE1_RAW`/`MODE2_RAW` are supported today).
    UnsupportedTrackType { track_type: String },
    /// The selected data track is not track 1 - see the module
    /// documentation's scope limits.
    UnsupportedTrackPosition { track: u32 },
    /// The selected data track's pregap is not zero (or is unknown) - see
    /// the module documentation's scope limits.
    UnsupportedPregap { pregap: Option<u32> },
    /// `chd-rs` itself refused to open the file or its map for a reason
    /// this module has no more specific variant for. `detail` is
    /// `chd-rs`'s own error, rendered as text - this module deliberately
    /// does not re-export `chd-rs` types in its own public API.
    Codec { detail: String },
}

impl std::fmt::Display for ChdLogicalMediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(error) => write!(formatter, "{error}"),
            Self::NeedsParent { parent_sha1 } => {
                write!(
                    formatter,
                    "CHD requires a parent with combined SHA-1 {}",
                    hex(parent_sha1)
                )
            }
            Self::NoDataTrack => formatter.write_str("no non-audio CD/GD-ROM track was found"),
            Self::UnsupportedTrackType { track_type } => {
                write!(formatter, "unsupported track type: {track_type}")
            }
            Self::UnsupportedTrackPosition { track } => {
                write!(
                    formatter,
                    "unsupported data track position: track {track} (only track 1 is supported)"
                )
            }
            Self::UnsupportedPregap { pregap } => {
                write!(
                    formatter,
                    "unsupported data track pregap: {pregap:?} (only a zero pregap is supported)"
                )
            }
            Self::Codec { detail } => write!(formatter, "CHD decode error: {detail}"),
        }
    }
}

impl std::error::Error for ChdLogicalMediaError {}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A [`LogicalMedia`] that presents one CHD's selected CD/GD-ROM data track
/// as a clean, contiguous stream of 2048-byte logical blocks - decoding
/// only the hunks a given read actually touches, never the whole image.
///
/// Interior mutability ([`RefCell`]) is used to satisfy [`LogicalMedia::read_at`]'s
/// `&self` signature while `chd-rs`'s own hunk decoding needs `&mut`
/// access (decompression is inherently stateful). No write ever happens
/// through this type or through `chd-rs`; the mutability is entirely about
/// decoder/cache bookkeeping.
pub struct ChdTrackLogicalMedia<'a> {
    chd: RefCell<Chd<Cursor<&'a [u8]>>>,
    hunk_bytes: u64,
    unit_bytes: u64,
    frame_count: u64,
    track_kind: TrackKind,
    /// A single decoded-hunk cache: `(hunk_index, decoded bytes)`. Bounded
    /// by construction (never more than one hunk, at most
    /// `CHD_V5_MAX_HUNK_BYTES` = 512 KiB) - see the module documentation's
    /// "cache only if simple and bounded" requirement.
    cache: RefCell<Option<(u32, Vec<u8>)>>,
}

/// Opens `data` as a CHD, selects its data track (via
/// [`select_candidate_data_track`]), and returns a [`LogicalMedia`] over
/// that track's decoded, sector-extracted bytes.
///
/// Pure and read-only: `data` is borrowed for the lifetime of the returned
/// adapter and never mutated. Nothing is decoded at open time beyond the
/// header, metadata chain, and hunk map that `chd-rs` itself reads to
/// validate the file - no hunk's compressed data is touched until a
/// [`LogicalMedia::read_at`] call actually needs it.
pub fn open_chd_track_logical_media(
    data: &[u8],
) -> Result<ChdTrackLogicalMedia<'_>, ChdLogicalMediaError> {
    let identity = observe_chd_identity(data).map_err(ChdLogicalMediaError::Header)?;
    if identity.parent_required {
        return Err(ChdLogicalMediaError::NeedsParent {
            parent_sha1: identity.parent_sha1,
        });
    }

    let ChdMetadataOutcome::Observed(metadata) = &identity.metadata else {
        return Err(ChdLogicalMediaError::NoDataTrack);
    };
    let candidate =
        select_candidate_data_track(metadata).ok_or(ChdLogicalMediaError::NoDataTrack)?;
    if candidate.track != 1 {
        return Err(ChdLogicalMediaError::UnsupportedTrackPosition {
            track: candidate.track,
        });
    }
    if candidate.pregap != Some(0) {
        return Err(ChdLogicalMediaError::UnsupportedPregap {
            pregap: candidate.pregap,
        });
    }
    let track_kind = match candidate.track_type.as_str() {
        "MODE1_RAW" => TrackKind::Mode1Raw,
        "MODE2_RAW" => TrackKind::Mode2Raw,
        other => {
            return Err(ChdLogicalMediaError::UnsupportedTrackType {
                track_type: other.to_string(),
            });
        }
    };

    let chd = Chd::open(Cursor::new(data), None).map_err(|error| match error {
        chd::Error::RequiresParent => ChdLogicalMediaError::NeedsParent {
            parent_sha1: identity.parent_sha1,
        },
        other => ChdLogicalMediaError::Codec {
            detail: format!("{other:?}"),
        },
    })?;
    let Header::V5Header(header_v5) = chd.header() else {
        return Err(ChdLogicalMediaError::Codec {
            detail: "expected a CHD v5 header".to_string(),
        });
    };
    let hunk_bytes = u64::from(header_v5.hunk_bytes);
    let unit_bytes = u64::from(header_v5.unit_bytes);

    Ok(ChdTrackLogicalMedia {
        chd: RefCell::new(chd),
        hunk_bytes,
        unit_bytes,
        frame_count: u64::from(candidate.frames),
        track_kind,
        cache: RefCell::new(None),
    })
}

impl ChdTrackLogicalMedia<'_> {
    /// Decodes hunk `hunk_index` (via the 1-hunk cache when possible) and
    /// returns its raw decompressed bytes.
    fn decode_hunk(&self, hunk_index: u32) -> Result<Vec<u8>, String> {
        if let Some((cached_index, cached_bytes)) = self.cache.borrow().as_ref()
            && *cached_index == hunk_index
        {
            return Ok(cached_bytes.clone());
        }

        let mut chd = self.chd.borrow_mut();
        let mut output = chd.get_hunksized_buffer();
        let mut compressed_buffer = Vec::new();
        let mut hunk = chd.hunk(hunk_index).map_err(|error| format!("{error:?}"))?;
        hunk.read_hunk_in(&mut compressed_buffer, &mut output)
            .map_err(|error| format!("{error:?}"))?;
        drop(chd);

        *self.cache.borrow_mut() = Some((hunk_index, output.clone()));
        Ok(output)
    }

    /// Reads the raw [`RAW_SECTOR_BYTES`]-byte sector at `sector_index`
    /// within the selected track (track 1, zero pregap - so this index is
    /// also the frame's absolute position in the CHD's logical stream).
    fn read_raw_sector(&self, sector_index: u64) -> Result<Vec<u8>, String> {
        if sector_index >= self.frame_count {
            return Err(format!(
                "sector index {sector_index} exceeds the selected track's frame count {}",
                self.frame_count
            ));
        }
        let absolute_byte_offset = sector_index * self.unit_bytes;
        let hunk_index = u32::try_from(absolute_byte_offset / self.hunk_bytes)
            .map_err(|_| "hunk index exceeds u32 range".to_string())?;
        let offset_within_hunk = (absolute_byte_offset % self.hunk_bytes) as usize;

        let hunk = self.decode_hunk(hunk_index)?;
        let end = offset_within_hunk + RAW_SECTOR_BYTES;
        if end > hunk.len() {
            return Err(format!(
                "decoded hunk ({} bytes) too short for expected sector at {offset_within_hunk}",
                hunk.len()
            ));
        }
        Ok(hunk[offset_within_hunk..end].to_vec())
    }
}

impl LogicalMedia for ChdTrackLogicalMedia<'_> {
    fn len(&self) -> u64 {
        self.frame_count * LOGICAL_BLOCK_BYTES as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), LogicalMediaError> {
        let out_of_bounds = || LogicalMediaError::OutOfBounds {
            offset,
            requested_len: buf.len(),
            media_len: self.len(),
        };
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(out_of_bounds)?;
        if end > self.len() {
            return Err(out_of_bounds());
        }

        let mut filled = 0usize;
        while filled < buf.len() {
            let absolute = offset + filled as u64;
            let sector_index = absolute / LOGICAL_BLOCK_BYTES as u64;
            let within_sector = (absolute % LOGICAL_BLOCK_BYTES as u64) as usize;

            let sector = self
                .read_raw_sector(sector_index)
                .map_err(|detail| LogicalMediaError::DecodeFailed { detail })?;
            let user_data = extract_user_data(&sector, self.track_kind)
                .map_err(|detail| LogicalMediaError::DecodeFailed { detail })?;

            let take = (LOGICAL_BLOCK_BYTES - within_sector).min(buf.len() - filled);
            buf[filled..filled + take]
                .copy_from_slice(&user_data[within_sector..within_sector + take]);
            filled += take;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::chd::CHD_MAGIC;
    use crate::iso9660::{ISO9660_STANDARD_IDENTIFIER, observe_iso9660};

    const CDROM_TRACK2_TAG: u32 = u32::from_be_bytes(*b"CHT2");

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    /// Builds a fully valid, UNCOMPRESSED synthetic CHD v5 file
    /// (`compression = [0,0,0,0]`, so the map is the simple flat 4-byte-per-hunk
    /// form - see the module documentation's dependency-decision notes).
    /// `unit_bytes` is fixed at [`RAW_SECTOR_BYTES`] (no trailing subcode padding) -
    /// a deliberate test simplification; real `chdman` output uses 2448-byte
    /// units (2352 sector + 96 subcode), which this adapter also handles
    /// correctly because it always reads exactly `RAW_SECTOR_BYTES` from the
    /// *start* of each unit, ignoring anything after it.
    ///
    /// Only `physical_frame_count` of the logical `frames` sectors are
    /// actually written into the file; the rest are left entirely absent, so
    /// a test can prove a read never required bytes beyond what it actually
    /// touched.
    fn build_uncompressed_chd(
        track_type: &str,
        frames: u32,
        frames_per_hunk: u32,
        physical_frame_count: u32,
        parent_sha1: [u8; 20],
        mut sector_bytes: impl FnMut(u32) -> [u8; RAW_SECTOR_BYTES],
    ) -> Vec<u8> {
        let unit_bytes = RAW_SECTOR_BYTES as u32;
        let hunk_bytes = unit_bytes * frames_per_hunk;
        let logical_bytes = frames as u64 * unit_bytes as u64;

        let mut data = vec![0u8; 124];
        data[0..8].copy_from_slice(CHD_MAGIC);
        put_u32(&mut data, 8, 124);
        put_u32(&mut data, 12, 5);
        put_u64(&mut data, 32, logical_bytes);
        put_u32(&mut data, 56, hunk_bytes);
        put_u32(&mut data, 60, unit_bytes);
        data[104..124].copy_from_slice(&parent_sha1);

        // Metadata chain: a single CDROM_TRACK2 entry - the verified v2 text
        // format, same as crate::chd_identity's own tests use.
        let meta_offset = data.len() as u64;
        let payload = format!(
            "TRACK:1 TYPE:{track_type} SUBTYPE:NONE FRAMES:{frames} PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0"
        )
        .into_bytes();
        data.extend_from_slice(&CDROM_TRACK2_TAG.to_be_bytes());
        data.push(0); // flags
        let length = payload.len() as u32;
        data.extend_from_slice(&length.to_be_bytes()[1..]); // 24-bit BE length
        data.extend_from_slice(&0u64.to_be_bytes()); // next = 0: end of chain
        data.extend_from_slice(&payload);

        // Map: hunk_count 4-byte big-endian hunk indices (uncompressed form).
        // Each value is multiplied by hunk_bytes by chd-rs to get a real file
        // offset - EXCEPT that a value of exactly 0 is chd-rs's own special
        // case for "this hunk is all zeros" (see `Chd::read_hunk_v5`'s
        // `MapEntry::V5Uncompressed` handling of `block_offset() == 0`), not
        // "read from file offset 0". So the raw hunk data below must start
        // at a file offset that is a *nonzero* multiple of hunk_bytes.
        let hunk_count = logical_bytes.div_ceil(hunk_bytes as u64) as u32;
        let map_offset = data.len() as u64;
        let map_end = map_offset + hunk_count as u64 * 4;
        let hunk_data_start = map_end.div_ceil(hunk_bytes as u64).max(1) * hunk_bytes as u64;
        let base_index = hunk_data_start / hunk_bytes as u64;
        for index in 0..hunk_count {
            let value = (base_index + index as u64) as u32;
            data.extend_from_slice(&value.to_be_bytes());
        }
        debug_assert_eq!(data.len() as u64, map_end);

        // Raw, uncompressed hunk data - physical_frame_count sectors only,
        // starting exactly at hunk_data_start.
        data.resize(hunk_data_start as usize, 0);
        for frame in 0..physical_frame_count {
            data.extend_from_slice(&sector_bytes(frame));
        }
        let physical_hunk_count = physical_frame_count.div_ceil(frames_per_hunk.max(1));
        let physical_hunk_bytes_needed = physical_hunk_count as u64 * hunk_bytes as u64;
        let physically_written = data.len() as u64 - hunk_data_start;
        if physically_written < physical_hunk_bytes_needed {
            data.resize((hunk_data_start + physical_hunk_bytes_needed) as usize, 0);
        }

        put_u64(&mut data, 40, map_offset);
        put_u64(&mut data, 48, meta_offset);
        data
    }

    /// A raw sector with a distinguishable, checkable pattern in its
    /// MODE1_RAW user-data region (offset 16, 2048 bytes): every byte equals
    /// `sector_index` wrapping-added to its position within the block.
    fn mode1_pattern_sector(sector_index: u32) -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0xAAu8; RAW_SECTOR_BYTES]; // sync/header region: arbitrary, never read
        for (position, byte) in sector
            [MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES]
            .iter_mut()
            .enumerate()
        {
            *byte = (sector_index as u8).wrapping_add(position as u8);
        }
        sector
    }

    fn expected_mode1_pattern(sector_index: u32) -> Vec<u8> {
        (0..LOGICAL_BLOCK_BYTES)
            .map(|position| (sector_index as u8).wrapping_add(position as u8))
            .collect()
    }

    fn mode2_form_sector(
        user_data: &[u8; LOGICAL_BLOCK_BYTES],
        form2: bool,
    ) -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0u8; RAW_SECTOR_BYTES];
        sector[MODE2_SUBMODE_OFFSET] = if form2 { MODE2_SUBMODE_FORM2_BIT } else { 0 };
        sector[MODE2_FORM1_USER_DATA_OFFSET..MODE2_FORM1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES]
            .copy_from_slice(user_data);
        sector
    }

    // ------------------------------------------------------------------
    // 1-2: opening
    // ------------------------------------------------------------------

    #[test]
    fn opens_a_valid_uncompressed_v5_chd() {
        let data = build_uncompressed_chd("MODE1_RAW", 2, 2, 2, [0; 20], mode1_pattern_sector);
        assert!(open_chd_track_logical_media(&data).is_ok());
    }

    #[test]
    fn invalid_chd_is_rejected() {
        let data = vec![0u8; 256];
        assert!(matches!(
            open_chd_track_logical_media(&data),
            Err(ChdLogicalMediaError::Header(_))
        ));
    }

    // ------------------------------------------------------------------
    // 3-7: read_at behaviour
    // ------------------------------------------------------------------

    #[test]
    fn arbitrary_offset_read_works() {
        let data = build_uncompressed_chd("MODE1_RAW", 3, 3, 3, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; 16];
        media.read_at(2048 + 100, &mut buf).unwrap();
        assert_eq!(buf, expected_mode1_pattern(1)[100..116]);
    }

    #[test]
    fn read_across_a_hunk_boundary_works() {
        // frames_per_hunk = 1: every sector is its own hunk, so any read
        // spanning two sectors necessarily spans two hunks.
        let data = build_uncompressed_chd("MODE1_RAW", 3, 1, 3, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; 32];
        media.read_at(2048 - 16, &mut buf).unwrap();
        let mut expected = expected_mode1_pattern(0)[2032..2048].to_vec();
        expected.extend_from_slice(&expected_mode1_pattern(1)[0..16]);
        assert_eq!(buf.to_vec(), expected);
    }

    #[test]
    fn out_of_range_read_is_rejected() {
        let data = build_uncompressed_chd("MODE1_RAW", 2, 2, 2, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; 4];
        assert!(matches!(
            media.read_at(media.len(), &mut buf),
            Err(LogicalMediaError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn repeated_read_is_deterministic() {
        let data = build_uncompressed_chd("MODE1_RAW", 2, 2, 2, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut first = [0u8; 64];
        let mut second = [0u8; 64];
        media.read_at(10, &mut first).unwrap();
        media.read_at(10, &mut second).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn source_bytes_are_never_modified() {
        let data = build_uncompressed_chd("MODE1_RAW", 2, 2, 2, [0; 20], mode1_pattern_sector);
        let before = data.clone();
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; 64];
        let _ = media.read_at(0, &mut buf);
        assert_eq!(data, before);
    }

    // ------------------------------------------------------------------
    // 8-10: codec paths
    // ------------------------------------------------------------------

    #[test]
    fn uncompressed_hunk_works() {
        // Every test in this module uses compression = [0,0,0,0] - this is
        // the "uncompressed hunk" path exercised throughout. Compressed
        // codec paths (cdlz/cdzl/cdfl) are verified against the real PS1/
        // Dreamcast CHD samples instead - see the crate-level report; hand-
        // crafting a valid compressed CD-codec stream for a unit test would
        // mean re-implementing chdman's own encoder just to test against it.
        let data = build_uncompressed_chd("MODE1_RAW", 1, 1, 1, [0; 20], mode1_pattern_sector);
        assert!(open_chd_track_logical_media(&data).is_ok());
    }

    #[test]
    fn unsupported_codec_fails_explicitly() {
        let mut data = build_uncompressed_chd("MODE1_RAW", 1, 1, 1, [0; 20], mode1_pattern_sector);
        // An unrecognised compression[0] FOURCC - chd-rs's own header
        // validation refuses this before any map/hunk is ever touched.
        put_u32(&mut data, 16, u32::from_be_bytes(*b"zzzz"));
        assert!(matches!(
            open_chd_track_logical_media(&data),
            Err(ChdLogicalMediaError::Codec { .. })
        ));
    }

    // ------------------------------------------------------------------
    // 11-13, 16: track selection and boundaries
    // ------------------------------------------------------------------

    #[test]
    fn parent_required_returns_needs_parent() {
        let mut parent = [0u8; 20];
        parent[0] = 0xaa;
        // No metadata chain needed at all: our own header/parent check runs
        // before chd-rs is ever invoked.
        let mut data = vec![0u8; 124];
        data[0..8].copy_from_slice(CHD_MAGIC);
        put_u32(&mut data, 8, 124);
        put_u32(&mut data, 12, 5);
        put_u32(&mut data, 56, RAW_SECTOR_BYTES as u32);
        put_u32(&mut data, 60, RAW_SECTOR_BYTES as u32);
        data[104..124].copy_from_slice(&parent);

        assert!(matches!(
            open_chd_track_logical_media(&data),
            Err(ChdLogicalMediaError::NeedsParent { parent_sha1 }) if parent_sha1 == parent
        ));
    }

    #[test]
    fn audio_track_is_not_exposed_as_filesystem_data() {
        let data = build_uncompressed_chd("AUDIO", 2, 2, 2, [0; 20], mode1_pattern_sector);
        assert!(matches!(
            open_chd_track_logical_media(&data),
            Err(ChdLogicalMediaError::NoDataTrack)
        ));
    }

    #[test]
    fn selected_track_boundary_matches_declared_frame_count() {
        let data = build_uncompressed_chd("MODE1_RAW", 3, 3, 3, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        assert_eq!(media.len(), 3 * LOGICAL_BLOCK_BYTES as u64);
    }

    #[test]
    fn no_read_bleeds_into_trailing_data_past_the_declared_track() {
        // Physically write 3 sectors' worth of data, but declare the track
        // as only 2 frames long - the same layout a real file would have
        // with a second track's data immediately following on disc.
        let data = build_uncompressed_chd("MODE1_RAW", 2, 2, 3, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        assert_eq!(media.len(), 2 * LOGICAL_BLOCK_BYTES as u64);
        let mut buf = [0u8; 1];
        assert!(matches!(
            media.read_at(2 * LOGICAL_BLOCK_BYTES as u64, &mut buf),
            Err(LogicalMediaError::OutOfBounds { .. })
        ));
    }

    // ------------------------------------------------------------------
    // 14-15: sector payload mapping
    // ------------------------------------------------------------------

    #[test]
    fn mode1_raw_sector_payload_is_mapped_correctly() {
        let data = build_uncompressed_chd("MODE1_RAW", 1, 1, 1, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media.read_at(0, &mut buf).unwrap();
        assert_eq!(buf.to_vec(), expected_mode1_pattern(0));
    }

    #[test]
    fn mode2_raw_form1_sector_payload_is_mapped_correctly() {
        let mut user_data = [0u8; LOGICAL_BLOCK_BYTES];
        user_data
            .iter_mut()
            .enumerate()
            .for_each(|(index, byte)| *byte = index as u8);
        let sector = mode2_form_sector(&user_data, false);
        let data = build_uncompressed_chd("MODE2_RAW", 1, 1, 1, [0; 20], move |_| sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, user_data);
    }

    #[test]
    fn mode2_raw_form2_sector_is_refused_not_misread() {
        let user_data = [0u8; LOGICAL_BLOCK_BYTES];
        let sector = mode2_form_sector(&user_data, true);
        let data = build_uncompressed_chd("MODE2_RAW", 1, 1, 1, [0; 20], move |_| sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        assert!(matches!(
            media.read_at(0, &mut buf),
            Err(LogicalMediaError::DecodeFailed { .. })
        ));
    }

    // ------------------------------------------------------------------
    // 17-18: ISO9660 connection
    // ------------------------------------------------------------------

    /// A minimal, valid ISO9660 image (PVD + Set Terminator at LBA 16/17,
    /// root directory at LBA 18 containing one file, `SYSTEM.CNF;1`),
    /// built independently of `crate::iso9660`'s own test fixtures.
    fn minimal_iso9660_image() -> Vec<u8> {
        const BLOCK: usize = 2048;
        let mut image = vec![0u8; 19 * BLOCK];

        // Root directory extent at LBA 18: "." , "..", then SYSTEM.CNF;1.
        let root_start = 18 * BLOCK;
        let mut cursor = root_start;
        cursor = write_iso_record(&mut image, cursor, &[0x00], false, 18, BLOCK as u32);
        cursor = write_iso_record(&mut image, cursor, &[0x01], true, 18, BLOCK as u32);
        write_iso_record(&mut image, cursor, b"SYSTEM.CNF;1", false, 0, 64);

        // Primary Volume Descriptor at LBA 16.
        let pvd = 16 * BLOCK;
        image[pvd] = 1;
        image[pvd + 1..pvd + 6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        image[pvd + 6] = 1;
        image[pvd + 40..pvd + 72].fill(b' ');
        image[pvd + 40..pvd + 46].copy_from_slice(b"CHDISO");
        image[pvd + 128..pvd + 130].copy_from_slice(&(BLOCK as u16).to_le_bytes());
        image[pvd + 130..pvd + 132].copy_from_slice(&(BLOCK as u16).to_be_bytes());
        let root_record = &mut image[pvd + 156..pvd + 190];
        root_record[0] = 34;
        root_record[2..6].copy_from_slice(&18u32.to_le_bytes());
        root_record[6..10].copy_from_slice(&18u32.to_be_bytes());
        root_record[10..14].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        root_record[14..18].copy_from_slice(&(BLOCK as u32).to_be_bytes());
        root_record[25] = 0x02;
        root_record[32] = 1;
        root_record[33] = 0x00;

        // Set Terminator at LBA 17.
        let terminator = 17 * BLOCK;
        image[terminator] = 255;
        image[terminator + 1..terminator + 6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        image[terminator + 6] = 1;

        image
    }

    fn write_iso_record(
        image: &mut [u8],
        cursor: usize,
        name: &[u8],
        is_dir: bool,
        extent_lba: u32,
        size: u32,
    ) -> usize {
        let mut length = 33 + name.len();
        if name.len().is_multiple_of(2) {
            length += 1;
        }
        image[cursor] = length as u8;
        image[cursor + 2..cursor + 6].copy_from_slice(&extent_lba.to_le_bytes());
        image[cursor + 6..cursor + 10].copy_from_slice(&extent_lba.to_be_bytes());
        image[cursor + 10..cursor + 14].copy_from_slice(&size.to_le_bytes());
        image[cursor + 14..cursor + 18].copy_from_slice(&size.to_be_bytes());
        image[cursor + 25] = if is_dir { 0x02 } else { 0x00 };
        image[cursor + 32] = name.len() as u8;
        image[cursor + 33..cursor + 33 + name.len()].copy_from_slice(name);
        cursor + length
    }

    fn mode1_sectors_for(image: &[u8]) -> Vec<[u8; RAW_SECTOR_BYTES]> {
        image
            .chunks(LOGICAL_BLOCK_BYTES)
            .map(|block| {
                let mut sector = [0u8; RAW_SECTOR_BYTES];
                sector[MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + block.len()]
                    .copy_from_slice(block);
                sector
            })
            .collect()
    }

    #[test]
    fn iso9660_can_be_observed_through_a_chd_backed_logical_media() {
        let image = minimal_iso9660_image();
        let sectors = mode1_sectors_for(&image);
        let frame_count = sectors.len() as u32;
        let data = build_uncompressed_chd(
            "MODE1_RAW",
            frame_count,
            frame_count,
            frame_count,
            [0; 20],
            move |index| sectors[index as usize],
        );
        let media = open_chd_track_logical_media(&data).unwrap();
        let observation = observe_iso9660(&media).expect("CHD-backed ISO9660 should be observable");
        assert_eq!(observation.volume_identifier, "CHDISO");
    }

    #[test]
    fn system_cnf_lookup_works_through_a_chd_backed_logical_media() {
        let image = minimal_iso9660_image();
        let sectors = mode1_sectors_for(&image);
        let frame_count = sectors.len() as u32;
        let data = build_uncompressed_chd(
            "MODE1_RAW",
            frame_count,
            frame_count,
            frame_count,
            [0; 20],
            move |index| sectors[index as usize],
        );
        let media = open_chd_track_logical_media(&data).unwrap();
        let observation = observe_iso9660(&media).unwrap();
        let found = observation
            .root_entries
            .iter()
            .find(|entry| entry.comparison_name == "SYSTEM.CNF");
        assert!(
            found.is_some(),
            "SYSTEM.CNF should be visible through the CHD-backed reader"
        );
    }

    // ------------------------------------------------------------------
    // 19-20: safety
    // ------------------------------------------------------------------

    #[test]
    fn malformed_truncated_map_fails_closed() {
        // Declares 4 frames spanning 2 hunks (so the map needs 8 bytes), but
        // the file is cut off after only 4 of those map bytes - a genuine
        // structural defect (distinct from the earlier
        // `no_whole_image_extraction_is_required` test, where the map is
        // complete and only trailing *hunk data* is absent, which
        // `chd-rs`/`Read::read` treats as a harmless short read, not an
        // error - it never claims those trailing hunks are needed at all).
        // Our own header/metadata parsing, which never touches the map,
        // still succeeds; only `chd-rs`'s own map read fails, and it must
        // fail closed rather than panic or fabricate a hunk map.
        let mut data = build_uncompressed_chd("MODE1_RAW", 4, 2, 2, [0; 20], mode1_pattern_sector);
        assert!(crate::chd_identity::observe_chd_identity(&data).is_ok());

        let map_offset = u64::from_be_bytes(data[40..48].try_into().unwrap()) as usize;
        data.truncate(map_offset + 4);

        assert!(matches!(
            open_chd_track_logical_media(&data),
            Err(ChdLogicalMediaError::Codec { .. })
        ));
    }

    #[test]
    fn absent_trailing_hunk_bytes_read_as_zero_not_an_error() {
        // Documents a real, verified `chd-rs`/`Read` behaviour rather than
        // hiding it: seeking past the physical end of a `Cursor<&[u8]>` and
        // reading returns `Ok(0)` (a short read), not an `io::Error` - so a
        // hunk whose bytes are entirely absent from the file decodes as an
        // all-zero hunk, silently. This is a genuine risk for a truly
        // corrupt/truncated real-world CHD - see the crate-level report's
        // risks section - and is different from `malformed_truncated_map_fails_closed`,
        // where the *map itself* (read eagerly at open time) is incomplete
        // and does fail closed.
        let data = build_uncompressed_chd("MODE1_RAW", 4, 2, 2, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media
            .read_at(3 * LOGICAL_BLOCK_BYTES as u64, &mut buf)
            .unwrap();
        assert_eq!(
            buf, [0u8; LOGICAL_BLOCK_BYTES],
            "absent hunk bytes decode as zero, not an error"
        );
    }

    #[test]
    fn no_whole_image_extraction_is_required() {
        // 100 declared frames (50 hunks), but only the first hunk (2 frames)
        // is physically present in the file. A read confined to that first
        // hunk must succeed - proving this reader never needed the other 49
        // hunks' bytes, which do not even exist in this file.
        let data = build_uncompressed_chd("MODE1_RAW", 100, 2, 2, [0; 20], mode1_pattern_sector);
        let media = open_chd_track_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media.read_at(0, &mut buf).unwrap();
        assert_eq!(buf.to_vec(), expected_mode1_pattern(0));
    }

    // ------------------------------------------------------------------
    // 21: platform safety
    // ------------------------------------------------------------------

    #[test]
    fn no_platform_inference_is_emitted() {
        // Structural: this module has no ContentDetector implementation and
        // produces no ContentEvidence - it returns only raw LogicalMedia
        // bytes or a ChdLogicalMediaError, neither of which has any field a
        // platform name could occupy.
        let messages = [
            ChdLogicalMediaError::NoDataTrack.to_string(),
            ChdLogicalMediaError::UnsupportedTrackPosition { track: 2 }.to_string(),
            ChdLogicalMediaError::UnsupportedPregap { pregap: Some(150) }.to_string(),
        ];
        for message in messages {
            let lower = message.to_lowercase();
            for platform in ["playstation", "dreamcast", "xbox", "gamecube", "saturn"] {
                assert!(!lower.contains(platform));
            }
        }
    }
}
