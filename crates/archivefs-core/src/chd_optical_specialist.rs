//! Optional specialist optical-disc backend for CHD layouts the pure-Rust
//! [`crate::chd_logical_media`] reader deliberately does not handle.
//!
//! # Why this exists
//!
//! [`crate::chd_logical_media::ChdTrackLogicalMedia`] is intentionally
//! narrow: track 1 only, zero pregap only, `MODE1_RAW`/`MODE2_RAW`-Form-1
//! only. That is enough to reach a PS1/PS2/Saturn-style single-data-track
//! disc's actual filesystem, and it stays *correct* by refusing everything
//! else rather than guessing. But it is architecturally unable to reach a
//! Dreamcast GD-ROM's real game data: a GD-ROM's track 1 is always the
//! small, CD-compatible "low-density" area (a handful of warning/text
//! files), and the actual game lives in a later, high-density track -
//! see [`crate::chd_identity::needs_specialist_optical_backend`], which
//! detects this from metadata alone, with no feature required.
//!
//! This module is the other side of that decision: when the pure-Rust path
//! cannot safely represent a disc, this backend can, for the layouts it
//! covers. It does **not** replace [`crate::chd_logical_media`] - both
//! remain, and a caller picks between them (see the module documentation's
//! "Fallback policy" below).
//!
//! # What backs this
//!
//! The [`opticaldiscs`](https://docs.rs/opticaldiscs) crate (MIT, by
//! danifunker), specifically its `chd` feature, which wraps
//! [`libchdman-rs`](https://docs.rs/libchdman-rs) - a Rust binding to
//! MAME's own `chd_file`/`cdrom_file` C++ core (BSD-3-Clause). That core
//! already implements every CD sector-cooking mode, every track/pregap
//! edge case, and (via `opticaldiscs`'s own `GdromSectorReader`/
//! `RebaseSectorReader`) the GD-ROM low-density/high-density absolute-LBA
//! rebasing this crate would otherwise have to reverse-engineer. Verified
//! directly against the real PS1/Dreamcast samples this whole arc has used
//! - see the crate-level report for exact findings, including the
//! dependency-weight and build-portability tradeoffs of the native core
//! this pulls in, which are real and worth reading before enabling this
//! feature.
//!
//! # Fallback policy
//!
//! ```text
//! try crate::chd_logical_media::open_chd_track_logical_media(bytes)
//!     Ok(media)?
//!         crate::chd_identity::needs_specialist_optical_backend(metadata)?
//!             false -> use `media` (the simple path already reaches
//!                      everything this disc has)
//!             true  -> the simple path "succeeded" but only reached a
//!                      GD-ROM's low-density area; prefer this module's
//!                      `open_chd_optical_specialist` instead, if the
//!                      `chd-optical-specialist` feature is compiled in
//!     Err(_)?
//!         `chd-optical-specialist` feature compiled in?
//!             yes -> try `open_chd_optical_specialist`
//!             no  -> Unsupported/Unknown - nothing left to try
//! ```
//!
//! Never a guess: a caller without this feature compiled in still gets a
//! fully correct (if sometimes incomplete-for-GD-ROM) answer from the pure-
//! Rust path alone, because `needs_specialist_optical_backend` is plain
//! metadata arithmetic with no feature requirement of its own.
//!
//! # Filesystem integration
//!
//! This module exposes only [`crate::logical_media::LogicalMedia`] - cooked
//! 2048-byte sectors - and nothing else. It does **not** use `opticaldiscs`'s
//! own filesystem browser. [`crate::iso9660`] remains the single ISO9660
//! implementation in this crate; feeding it different byte sources (a plain
//! `.iso`, the pure-Rust CHD reader, or this specialist reader) is exactly
//! what [`crate::logical_media::LogicalMedia`] exists to make interchangeable.
//!
//! # A real API-shape difference, documented rather than hidden
//!
//! Every other reader in this crate operates on `&[u8]` already read into
//! memory. `libchdman-rs` opens its own native file handle by **path**, so
//! this module's entry point takes a [`std::path::Path`], not bytes. This is
//! the one place in the CHD-reading pipeline where that is true, and it is
//! why this module cannot simply implement `LogicalMedia` the way
//! `ChdTrackLogicalMedia` does over a borrowed slice - see
//! [`ChdOpticalSpecialist`]'s own fields.

use std::cell::RefCell;
use std::path::Path;

use opticaldiscs::chd::{ChdInfo, ChdMedia, ChdTrack, chd_media, open_chd};
use opticaldiscs::sector_reader::{
    ChdSectorReader, GDROM_HD_START_LBA, GdromHdTrack, GdromSectorReader, SectorReader,
};

use crate::logical_media::{LogicalMedia, LogicalMediaError};

/// Why [`open_chd_optical_specialist`] could not produce a
/// [`ChdOpticalSpecialist`].
#[derive(Debug)]
pub enum ChdOpticalSpecialistError {
    /// The path could not be read, or the CHD header/track metadata could
    /// not be parsed by `libchdman-rs`. `detail` is its own error, rendered
    /// as text - this module deliberately does not re-export `opticaldiscs`
    /// or `libchdman-rs` types in its own public API.
    Backend { detail: String },
    /// The CHD is not CD/GD-ROM optical media (e.g. a hard-disk or A/V
    /// CHD) - nothing for this module to read.
    NotOptical { media: &'static str },
    /// The CHD carries no non-audio data track at all.
    NoDataTrack,
}

impl std::fmt::Display for ChdOpticalSpecialistError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend { detail } => {
                write!(formatter, "optical specialist backend error: {detail}")
            }
            Self::NotOptical { media } => write!(formatter, "not CD/GD-ROM optical media: {media}"),
            Self::NoDataTrack => formatter.write_str("no non-audio data track was found"),
        }
    }
}

impl std::error::Error for ChdOpticalSpecialistError {}

/// A [`LogicalMedia`] over one CHD's data area, decoded on demand by the
/// native MAME core via `opticaldiscs`/`libchdman-rs`.
///
/// For a GD-ROM whose real game data spans one or more high-density tracks
/// (see [`crate::chd_identity::needs_specialist_optical_backend`]), reads
/// are addressed the way the disc's own ISO9660 volume actually addresses
/// them: **absolute** GD-ROM LBAs, transparently routed to whichever
/// physical track holds them (via `opticaldiscs`'s own `GdromSectorReader`).
/// For an ordinary CD-ROM data track, reads are plain track-relative
/// sectors.
///
/// Interior mutability ([`RefCell`]) satisfies [`LogicalMedia::read_at`]'s
/// `&self` signature; the underlying `SectorReader` needs `&mut self` to
/// decode a sector (decompression is inherently stateful), exactly as
/// [`crate::chd_logical_media::ChdTrackLogicalMedia`] already does for the
/// same reason.
pub struct ChdOpticalSpecialist {
    reader: RefCell<Box<dyn SectorReader>>,
    /// The address space this reader was built for: `Some(GDROM_HD_START_LBA)`
    /// when addressing is GD-ROM-absolute, `None` for plain track-relative
    /// CD-ROM addressing. Exposed only for diagnostics/probes.
    pub base_lba: Option<u64>,
}

/// Opens `path` as a CHD and selects the best data area this backend can
/// read:
///
/// - a GD-ROM with high-density data: **every** high-density track (there
///   can be more than one, separated by audio tracks - see
///   [`ChdInfo::find_gdrom_hd_tracks`]), addressed by absolute GD-ROM LBA;
/// - otherwise: the first data track, addressed track-relative.
///
/// Pure and read-only: only track metadata is read at open time (via
/// `libchdman-rs`'s own header/map parsing); sector data is decoded only
/// as [`LogicalMedia::read_at`] requests it.
pub fn open_chd_optical_specialist(
    path: impl AsRef<Path>,
) -> Result<ChdOpticalSpecialist, ChdOpticalSpecialistError> {
    let path = path.as_ref().to_path_buf();

    let media = chd_media(&path).map_err(|error| ChdOpticalSpecialistError::Backend {
        detail: format!("{error:?}"),
    })?;
    if !media.has_tracks() {
        return Err(ChdOpticalSpecialistError::NotOptical {
            media: media.display_name(),
        });
    }

    let info = open_chd(&path).map_err(|error| ChdOpticalSpecialistError::Backend {
        detail: format!("{error:?}"),
    })?;

    if media == ChdMedia::GdRom
        && let Some(reader) = open_gdrom_high_density(&path, &info)?
    {
        return Ok(ChdOpticalSpecialist {
            reader: RefCell::new(reader),
            base_lba: Some(GDROM_HD_START_LBA),
        });
    }

    let track = info
        .find_first_data_track()
        .ok_or(ChdOpticalSpecialistError::NoDataTrack)?;
    let reader = open_track(&path, track)?;
    Ok(ChdOpticalSpecialist {
        reader: RefCell::new(Box::new(reader)),
        base_lba: None,
    })
}

fn open_track(path: &Path, track: &ChdTrack) -> Result<ChdSectorReader, ChdOpticalSpecialistError> {
    ChdSectorReader::open(path, track).map_err(|error| ChdOpticalSpecialistError::Backend {
        detail: format!("{error:?}"),
    })
}

/// Builds a [`GdromSectorReader`] over every high-density track, or `None`
/// if this CHD (though a GD-ROM) has no high-density track at all.
fn open_gdrom_high_density(
    path: &Path,
    info: &ChdInfo,
) -> Result<Option<Box<dyn SectorReader>>, ChdOpticalSpecialistError> {
    let hd_tracks = info.find_gdrom_hd_tracks();
    if hd_tracks.is_empty() {
        return Ok(None);
    }

    let mut tracks = Vec::with_capacity(hd_tracks.len());
    for track in hd_tracks {
        let reader = open_track(path, track)?;
        tracks.push(GdromHdTrack {
            start_lba: track.frame_offset,
            frame_count: track.frames as u64,
            reader: Box::new(reader),
        });
    }
    Ok(Some(Box::new(GdromSectorReader::from_tracks(tracks))))
}

impl LogicalMedia for ChdOpticalSpecialist {
    fn len(&self) -> u64 {
        // The underlying SectorReader has no notion of a fixed upper bound
        // (a GD-ROM's absolute-LBA address space is open-ended by design -
        // see GdromSectorReader's own documentation), so this reports a
        // generously large but finite bound. crate::iso9660 never reads
        // past what the volume's own PVD/directory records declare, so
        // this bound is never actually reached in practice; it exists only
        // so LogicalMedia::read_at has a length to bounds-check against.
        u64::MAX / 2
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), LogicalMediaError> {
        const SECTOR_SIZE: u64 = 2048;
        let mut filled = 0usize;
        let mut reader = self.reader.borrow_mut();
        while filled < buf.len() {
            let absolute = offset + filled as u64;
            let lba = absolute / SECTOR_SIZE;
            let within_sector = (absolute % SECTOR_SIZE) as usize;
            let sector =
                reader
                    .read_sector(lba)
                    .map_err(|error| LogicalMediaError::DecodeFailed {
                        detail: format!("{error:?}"),
                    })?;
            if sector.len() != SECTOR_SIZE as usize {
                return Err(LogicalMediaError::DecodeFailed {
                    detail: format!(
                        "cooked sector at lba {lba} was {} bytes, expected {SECTOR_SIZE}",
                        sector.len()
                    ),
                });
            }
            let take = (SECTOR_SIZE as usize - within_sector).min(buf.len() - filled);
            buf[filled..filled + take]
                .copy_from_slice(&sector[within_sector..within_sector + take]);
            filled += take;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------
//
// This module's "does it actually decode a real GD-ROM/CD-ROM correctly"
// behaviour is validated against real files via `examples/disc_probe.rs`
// (see the crate-level report), not here: `libchdman-rs` opens a CHD by
// path via its own native parser, so a meaningful happy-path fixture would
// mean either a real multi-hundred-megabyte sample (not something this
// crate's test suite depends on - every other test here runs with no
// external files) or hand-encoding a compressed CHD map well enough to
// satisfy a C++ parser this crate does not control, which is exactly the
// "retest opticaldiscs internally" this crate's own review guidance says
// not to do. What *is* tested here, without any real file, is this
// module's own error handling and platform-safety boundary.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_path_is_rejected() {
        let result = open_chd_optical_specialist("/nonexistent/path/does-not-exist.chd");
        assert!(result.is_err());
    }

    #[test]
    fn non_chd_file_is_rejected() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "chd_optical_specialist_test_{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, b"this is definitely not a CHD file").unwrap();

        let result = open_chd_optical_specialist(&path);
        let _ = std::fs::remove_file(&path);

        assert!(matches!(
            result,
            Err(ChdOpticalSpecialistError::Backend { .. })
        ));
    }

    #[test]
    fn no_platform_inference_is_emitted() {
        // Structural: this module has no ContentDetector implementation and
        // produces no ContentEvidence - it returns only raw LogicalMedia
        // bytes or a ChdOpticalSpecialistError, neither of which has any
        // field a platform name could occupy.
        let messages = [
            ChdOpticalSpecialistError::NoDataTrack.to_string(),
            ChdOpticalSpecialistError::NotOptical { media: "hard-disk" }.to_string(),
        ];
        for message in messages {
            let lower = message.to_lowercase();
            for platform in ["playstation", "dreamcast", "xbox", "gamecube", "saturn"] {
                assert!(!lower.contains(platform));
            }
        }
    }
}
