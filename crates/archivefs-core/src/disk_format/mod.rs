//! The one read-only disk-image format evidence layer.
//!
//! Platform detection and disc identity both need to answer "what kind of image
//! is this, really?" without extracting, mounting or decoding it. Before this
//! module, each answered it privately: the platform registry compared magic
//! bytes at fixed offsets, and `game_identity` had its own per-format readers.
//! Fixed-offset comparison cannot express a *structural* format - one that is
//! recognised by a set of interdependent header fields rather than a constant -
//! and duplicating structural rules per caller is how two callers come to
//! disagree about the same file.
//!
//! So structural formats live here, once, and both callers consume the same
//! [`DiskFormatEvidence`].
//!
//! # What this module does not do
//!
//! It reads a bounded prefix of a file and validates header structure. It never
//! creates, writes, extracts, mounts or decodes an image, never spawns a
//! process, and never touches a network. It never reconstructs a disk: the
//! deepest it goes is walking a fixed-size table of record headers to confirm
//! the table is internally consistent.
//!
//! # Reading is delegated, not reinvented
//!
//! Every byte comes through [`crate::safe_read::open_bounded_read`], which is
//! the single place this build decides what may be opened. A symlink is followed
//! only under that module's trusted-root policy; this module adds no policy of
//! its own and cannot bypass it.
//!
//! # Honesty about what a structure proves
//!
//! A format's structure being valid is not the same as a platform being
//! certain. An Atari ST `.st` image is a raw FAT12 floppy dump whose boot sector
//! is laid out identically to a PC DOS floppy's, so a valid one proves "this is
//! a plausible ST-geometry FAT12 floppy", not "this is an Atari ST disk". A
//! `.stx` image, by contrast, is the Pasti preservation format, which exists
//! only for Atari ST media, so a valid one does prove the platform.
//!
//! [`DiskFormatEvidence::conclusive`] is how that difference is carried to the
//! caller instead of being flattened away.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use crate::platform::DetectionConfidence;
use crate::safe_read::{SafeFile, TrustedRoots, open_bounded_read};

pub mod atari_st;
pub mod atari_stx;

#[cfg(test)]
mod tests;

// --- Limits ---------------------------------------------------------------
//
// Every limit is explicit, checked before the read it governs, and asserted by
// `limits_are_internally_consistent`.

/// The most this module will ever read from one file, across every step of one
/// inspection. Chosen so a full inspection is a handful of small reads: a boot
/// sector, or one header plus a bounded table of record headers.
pub const MAX_DISK_FORMAT_BYTES_READ: u64 = 64 * 1024;

/// The largest single read.
pub const MAX_DISK_FORMAT_READ_CHUNK: usize = 1024;

/// The furthest offset any read starts at. A structural header lives at the
/// front of a file; nothing here seeks into the body.
pub const MAX_DISK_FORMAT_OFFSET: u64 = 32 * 1024;

/// The largest file this module will treat as a raw floppy image. An Atari ST
/// floppy tops out at 1.44 MB; the slack covers unusual track counts without
/// admitting whole hard-disk images.
pub const MAX_RAW_FLOPPY_BYTES: u64 = 4 * 1024 * 1024;

/// The largest file this module will treat as a Pasti image. Flux-level
/// preservation dumps are larger than the disk they describe.
pub const MAX_PASTI_BYTES: u64 = 32 * 1024 * 1024;

/// Sector size this module accepts for a raw floppy. Atari ST TOS floppies are
/// always 512-byte sectors; accepting others would weaken the check for no real
/// coverage.
pub const FLOPPY_SECTOR_BYTES: u32 = 512;

/// The most track records a Pasti table may declare. Two sides of 84 tracks is
/// already beyond any real Atari ST disk.
pub const MAX_PASTI_TRACK_RECORDS: usize = 168;

/// Which structural format was recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskFormat {
    /// A raw sector dump of an Atari ST floppy: no container header at all,
    /// recognised from its FAT12 boot-sector geometry and its exact length.
    AtariStRawFloppy,
    /// The Pasti (`.stx`) preservation container.
    AtariStPasti,
}

impl DiskFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::AtariStRawFloppy => "Atari ST raw floppy image",
            Self::AtariStPasti => "Atari ST Pasti (STX) image",
        }
    }

    /// The canonical platform identifier this format belongs to.
    pub fn platform(self) -> &'static str {
        match self {
            Self::AtariStRawFloppy | Self::AtariStPasti => "AtariST",
        }
    }

    /// Whether the format's structure, on its own, proves the platform.
    ///
    /// `false` for a raw floppy dump: its boot sector is a FAT12 BPB, the same
    /// structure a PC DOS floppy of the same geometry carries, so the structure
    /// narrows the answer without settling it. `true` for Pasti, which exists
    /// only for Atari ST media.
    pub fn proves_platform(self) -> bool {
        match self {
            Self::AtariStRawFloppy => false,
            Self::AtariStPasti => true,
        }
    }
}

/// Why no format was claimed. Each variant is a distinct, explainable outcome
/// rather than one opaque failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskFormatRefusal {
    /// The file could not be opened under the bounded-read policy. Carries that
    /// module's own reason - a target outside the trusted roots, a broken or
    /// looping symlink, a directory, a device.
    NotReadable { code: &'static str, detail: String },
    /// This module has no adapter for the extension.
    NoAdapter { extension: String },
    /// The file has no extension to dispatch on.
    NoExtension,
    /// Smaller than the structure it would have to contain.
    TooSmall { length: u64, minimum: u64 },
    /// Larger than this module will treat as this kind of image.
    TooLarge { length: u64, maximum: u64 },
    /// A raw floppy image must be a whole number of sectors.
    NotSectorAligned { length: u64, sector_bytes: u32 },
    /// A required read failed or returned short - a truncated file.
    Truncated { offset: u64, wanted: usize },
    /// The structure was read but is not valid. The detail names the field.
    Malformed { detail: String },
    /// The declared geometry and the file length disagree.
    GeometryMismatch {
        declared_bytes: u64,
        actual_bytes: u64,
    },
    /// Cancelled by the caller between bounded steps.
    Cancelled,
}

impl DiskFormatRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NotReadable { detail, .. } => detail.clone(),
            Self::NoAdapter { extension } => {
                format!("no disk-format adapter handles `.{extension}`")
            }
            Self::NoExtension => "the file has no extension to identify it by".to_string(),
            Self::TooSmall { length, minimum } => format!(
                "the file is {length} bytes, below the {minimum} bytes this structure needs"
            ),
            Self::TooLarge { length, maximum } => format!(
                "the file is {length} bytes, above the {maximum}-byte limit for this format"
            ),
            Self::NotSectorAligned {
                length,
                sector_bytes,
            } => format!("{length} bytes is not a whole number of {sector_bytes}-byte sectors"),
            Self::Truncated { offset, wanted } => {
                format!("the file ends before {wanted} bytes could be read at offset {offset}")
            }
            Self::Malformed { detail } => detail.clone(),
            Self::GeometryMismatch {
                declared_bytes,
                actual_bytes,
            } => format!(
                "the boot sector declares {declared_bytes} bytes of sectors but the file is \
                 {actual_bytes} bytes"
            ),
            Self::Cancelled => "the inspection was cancelled".to_string(),
        }
    }

    /// A stable code, for counting refusals in an audit.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotReadable { code, .. } => code,
            Self::NoAdapter { .. } => "no_adapter",
            Self::NoExtension => "no_extension",
            Self::TooSmall { .. } => "too_small",
            Self::TooLarge { .. } => "too_large",
            Self::NotSectorAligned { .. } => "not_sector_aligned",
            Self::Truncated { .. } => "truncated",
            Self::Malformed { .. } => "malformed",
            Self::GeometryMismatch { .. } => "geometry_mismatch",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A raw floppy image's declared geometry, as read from its boot sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FloppyGeometry {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_directory_entries: u16,
    pub total_sectors: u32,
    pub sectors_per_fat: u16,
    pub sectors_per_track: u16,
    pub sides: u16,
    /// Derived, not stored: `total_sectors / (sectors_per_track * sides)`.
    pub tracks: u16,
}

impl FloppyGeometry {
    /// A person-readable summary, e.g. "80 tracks x 2 sides x 9 sectors".
    pub fn summary(&self) -> String {
        format!(
            "{} tracks x {} side(s) x {} sectors of {} bytes ({} sectors total)",
            self.tracks,
            self.sides,
            self.sectors_per_track,
            self.bytes_per_sector,
            self.total_sectors
        )
    }
}

/// What one Pasti container declared about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PastiLayout {
    pub version: u16,
    pub tool: u16,
    pub revision: u8,
    /// Track records the header declares.
    pub declared_track_records: u8,
    /// Track records actually walked and found internally consistent.
    pub validated_track_records: usize,
    /// Total sectors the validated track records declare between them.
    pub declared_sectors: u32,
}

/// Optional format-specific metadata. Only ever the shape the recognised format
/// actually has - never a lowest common denominator that invents fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DiskFormatMetadata {
    Floppy(FloppyGeometry),
    Pasti(PastiLayout),
}

/// The shared result. One shape, whatever the format, so a caller does not need
/// to know which adapter ran.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskFormatEvidence {
    /// The format recognised, or `None` when none was.
    pub format: Option<DiskFormat>,
    /// The canonical platform the format implies. `None` when no format matched.
    pub platform: Option<&'static str>,
    /// How sure the *platform* is, on the one shared scale. See the module
    /// documentation for why a valid structure is not always Confirmed.
    pub confidence: DetectionConfidence,
    /// Whether the structure alone settles the platform. A caller that ranks
    /// evidence uses this rather than assuming every structural match is
    /// conclusive.
    pub conclusive: bool,
    /// Observed facts, in a person's words. Never a guess or a suggestion.
    pub evidence: Vec<String>,
    /// How many bytes were actually read. Bounded by
    /// [`MAX_DISK_FORMAT_BYTES_READ`], and asserted by the tests.
    pub bytes_inspected: u64,
    /// Why nothing was claimed, when nothing was.
    pub refusal: Option<DiskFormatRefusal>,
    pub metadata: Option<DiskFormatMetadata>,
    /// Whether the bytes came through a symlink the trusted-root policy allowed.
    pub read_via_symlink: bool,
}

impl DiskFormatEvidence {
    /// Nothing claimed, for the stated reason.
    pub fn refused(refusal: DiskFormatRefusal) -> Self {
        Self {
            format: None,
            platform: None,
            confidence: DetectionConfidence::Unknown,
            conclusive: false,
            evidence: Vec::new(),
            bytes_inspected: 0,
            refusal: Some(refusal),
            metadata: None,
            read_via_symlink: false,
        }
    }

    pub fn is_recognised(&self) -> bool {
        self.format.is_some()
    }

    /// A one-line summary for a report.
    pub fn summary(&self) -> String {
        match (&self.format, &self.refusal) {
            (Some(format), _) => format!(
                "{} ({}, {} bytes inspected)",
                format.label(),
                self.confidence.label().to_lowercase(),
                self.bytes_inspected
            ),
            (None, Some(refusal)) => format!("not recognised: {}", refusal.detail()),
            (None, None) => "not recognised".to_string(),
        }
    }
}

/// What the caller already believes about the platform, so the shared result can
/// be honest about agreement and conflict without the adapters knowing anything
/// about folders or filters.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskFormatContext<'a> {
    /// A canonical platform the surrounding evidence already points at - in
    /// practice, an exact folder alias.
    pub folder_platform: Option<&'a str>,
}

/// Inspects `path` and reports what structural format it is, if any.
///
/// Dispatch is by extension, because that is what says *which structure to
/// look for*; it is never what decides the answer. A `.st` file whose boot
/// sector does not validate is refused, and the caller is left with extension
/// evidence only.
///
/// `cancel` is checked between bounded steps. Every read goes through
/// [`crate::safe_read`], so `trusted` governs symlinks exactly as it does
/// everywhere else.
pub fn inspect_disk_format(
    path: &Path,
    trusted: &TrustedRoots,
    context: DiskFormatContext<'_>,
    cancel: Option<&AtomicBool>,
) -> DiskFormatEvidence {
    let Some(extension) = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return DiskFormatEvidence::refused(DiskFormatRefusal::NoExtension);
    };
    // Refuse before opening anything when no adapter could use the bytes.
    let adapter = match extension.as_str() {
        "st" => Adapter::AtariStRawFloppy,
        "stx" => Adapter::AtariStPasti,
        _ => return DiskFormatEvidence::refused(DiskFormatRefusal::NoAdapter { extension }),
    };
    if cancelled(cancel) {
        return DiskFormatEvidence::refused(DiskFormatRefusal::Cancelled);
    }

    let mut file = match open_bounded_read(path, trusted) {
        Ok(file) => file,
        Err(refusal) => {
            return DiskFormatEvidence::refused(DiskFormatRefusal::NotReadable {
                code: refusal.code(),
                detail: refusal.detail(),
            });
        }
    };
    let read_via_symlink = file.resolved_via_symlink();

    let mut reader = BoundedReader::new(&mut file);
    let mut evidence = match adapter {
        Adapter::AtariStRawFloppy => atari_st::inspect(&mut reader, context, cancel),
        Adapter::AtariStPasti => atari_stx::inspect(&mut reader, context, cancel),
    };
    evidence.bytes_inspected = reader.bytes_read;
    evidence.read_via_symlink = read_via_symlink;
    if read_via_symlink && evidence.is_recognised() {
        evidence
            .evidence
            .push("Structure read from a validated symlink target".to_string());
    }
    evidence
}

enum Adapter {
    AtariStRawFloppy,
    AtariStPasti,
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// A read budget over one already-validated handle.
///
/// Adapters read only through this, so no adapter can exceed the module's total
/// byte budget, read past the offset limit, or ask for a chunk larger than the
/// documented maximum - regardless of what a header claims.
pub struct BoundedReader<'a> {
    file: &'a mut SafeFile,
    bytes_read: u64,
}

impl<'a> BoundedReader<'a> {
    fn new(file: &'a mut SafeFile) -> Self {
        Self {
            file,
            bytes_read: 0,
        }
    }

    /// The file's length, as observed when it was validated.
    pub fn len(&self) -> u64 {
        self.file.len()
    }

    pub fn is_empty(&self) -> bool {
        self.file.is_empty()
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Reads exactly `length` bytes at `offset`, or refuses.
    ///
    /// Refuses - without reading - when the chunk is too large, the offset is
    /// beyond the module's limit, or the total budget would be exceeded. All
    /// arithmetic is checked, so a hostile header cannot wrap a bound into
    /// looking satisfied.
    pub fn read_exact_at(
        &mut self,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, DiskFormatRefusal> {
        if length == 0 || length > MAX_DISK_FORMAT_READ_CHUNK {
            return Err(DiskFormatRefusal::Malformed {
                detail: format!(
                    "a {length}-byte read is outside the {MAX_DISK_FORMAT_READ_CHUNK}-byte chunk limit"
                ),
            });
        }
        if offset > MAX_DISK_FORMAT_OFFSET {
            return Err(DiskFormatRefusal::Malformed {
                detail: format!(
                    "offset {offset} is past the {MAX_DISK_FORMAT_OFFSET}-byte inspection limit"
                ),
            });
        }
        let would_read = self.bytes_read.checked_add(length as u64).ok_or_else(|| {
            DiskFormatRefusal::Malformed {
                detail: "the read budget overflowed".to_string(),
            }
        })?;
        if would_read > MAX_DISK_FORMAT_BYTES_READ {
            return Err(DiskFormatRefusal::Malformed {
                detail: format!(
                    "reading {length} more bytes would exceed the \
                     {MAX_DISK_FORMAT_BYTES_READ}-byte inspection budget"
                ),
            });
        }
        let bytes = self
            .file
            .read_exact_at(offset, length, MAX_DISK_FORMAT_READ_CHUNK)
            .ok_or(DiskFormatRefusal::Truncated {
                offset,
                wanted: length,
            })?;
        self.bytes_read = would_read;
        Ok(bytes)
    }
}

/// Reads a little-endian `u16` from `bytes` at `offset`, or `None` if the slice
/// is too short. Bounds-checked rather than indexed.
pub(crate) fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = bytes.get(offset..end)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Reads a little-endian `u32` from `bytes` at `offset`, or `None`.
pub(crate) fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// The confidence a recognised format deserves, given what else is known.
///
/// This is where the milestone's confidence rules live, in one place:
///
/// - a format that proves its platform is Confirmed on its own;
/// - a format that only narrows it - a raw FAT12 floppy dump - is Probable, and
///   reaches Confirmed only when a folder alias names the same platform;
/// - a folder naming a *different* platform is a real conflict, and the shared
///   result reports Ambiguous rather than picking a side. The caller's own
///   precedence still decides the overall verdict; this field describes what
///   the *structure* can honestly claim in that situation.
pub(crate) fn confidence_for(
    format: DiskFormat,
    context: DiskFormatContext<'_>,
) -> (DetectionConfidence, bool) {
    let platform = format.platform();
    match context.folder_platform {
        Some(folder) if folder == platform => (DetectionConfidence::Confirmed, true),
        Some(_) => (DetectionConfidence::Ambiguous, false),
        None if format.proves_platform() => (DetectionConfidence::Confirmed, true),
        None => (DetectionConfidence::Probable, false),
    }
}
