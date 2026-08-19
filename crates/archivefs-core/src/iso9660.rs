//! Pure, read-only ISO9660 logical-filesystem observation.
//!
//! This is the first layer in the pipeline this chunk exists to start:
//!
//! ```text
//! container -> media -> logical media reader -> filesystem/root tree
//!     -> boot/layout observations -> platform evidence (later, elsewhere)
//! ```
//!
//! [`crate::chd_identity`] already answers "what container/media is this?"
//! (CHD, CD-ROM, GD-ROM, hard disk, ...). This module answers a different,
//! independent question: "given some logical byte stream, does it contain a
//! valid ISO9660 filesystem, and what does its root layout look like?" A
//! filesystem is evidence, never a platform - `ISO9660 != PlayStation`,
//! exactly as `GD-ROM != Dreamcast` in [`crate::chd_identity`]. Nothing here
//! imports `crate::platform` or `crate::dat::identity`, and no fact this
//! module emits is a platform name.
//!
//! # Not coupled to CHD
//!
//! This parser consumes [`crate::logical_media::LogicalMedia`], not `&[u8]`
//! directly and not any CHD type. A plain `.iso`/`.bin` file already read
//! into memory is a [`crate::logical_media::SliceMedia`]; a CHD's logical
//! data track would be a different `LogicalMedia` implementation, once one
//! exists (it does not yet - see [`crate::chd_identity`]'s documentation on
//! why CHD hunk decompression is out of scope for this chunk).
//!
//! # Format verified, not assumed
//!
//! The Primary Volume Descriptor and Directory Record byte layouts below
//! were verified against the Linux kernel's own ISO9660 UAPI header
//! (`https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/linux/iso_fs.h`,
//! structs `iso_primary_descriptor` and `iso_directory_record`), which
//! encodes the ECMA-119 field layout exactly (its `ISODCL(a,b)` field-size
//! macro is 1-indexed inclusive byte ranges; offsets below are converted to
//! 0-indexed). The `.`/`..` special single-byte identifiers (`0x00`/`0x01`)
//! and the "both-endian" (`7xx`) field convention were cross-checked against
//! independent ISO9660/ECMA-119 summaries.
//!
//! ```text
//! Volume Descriptor (2048 bytes), offset within the descriptor:
//! [ 0]      type            (1 byte)   1 = Primary, 255 = Set Terminator
//! [ 1.. 6]  id               (5 bytes)  "CD001"
//! [ 6]      version          (1 byte)
//!
//! Primary Volume Descriptor, additional fields:
//! [ 40.. 72]  volume_id            (32 bytes, d-characters, space padded)
//! [ 80.. 88]  volume_space_size    (8 bytes, both-endian u32: LE @80, BE @84)
//! [128..132]  logical_block_size   (4 bytes, both-endian u16: LE @128, BE @130)
//! [156..190]  root_directory_record (34-byte embedded Directory Record)
//!
//! Directory Record, offset within the record:
//! [ 0]       length                (1 byte; 0 = end-of-block padding marker)
//! [ 1]       ext_attr_length       (1 byte)
//! [ 2..10]   extent                (8 bytes, both-endian u32: LE @2, BE @6)
//! [10..18]   size                  (8 bytes, both-endian u32: LE @10, BE @14)
//! [18..25]   recording date/time   (7 bytes)
//! [25]       flags                 (1 byte; bit 0x02 = is a directory)
//! [26]       file_unit_size        (1 byte)
//! [27]       interleave            (1 byte)
//! [28..32]   volume_sequence_number (4 bytes, both-endian u16)
//! [32]       name_len              (1 byte)
//! [33..33+name_len]  name          (name_len bytes; 1-byte 0x00/0x01 = "."/"..")
//! ```
//!
//! Records never cross a logical-block boundary; a `length == 0` byte marks
//! unused padding to the end of the current block, not an error.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector, ContentDiagnostic};
use crate::content_evidence::{
    ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind, value,
};
use crate::logical_media::{LogicalMedia, LogicalMediaError};

/// The exact 5-byte standard identifier every ISO9660 volume descriptor
/// begins its `id` field with.
pub const ISO9660_STANDARD_IDENTIFIER: &[u8; 5] = b"CD001";

/// The logical block index of the first Volume Descriptor. Fixed by the
/// format (16 blocks reserved as the "System Area").
pub const PRIMARY_VOLUME_DESCRIPTOR_LBA: u64 = 16;

/// The only logical block size this first implementation supports. Every
/// CD/DVD-derived ISO9660 image this crate has reason to read uses 2048;
/// supporting other sizes is deferred rather than guessed at (see
/// [`Iso9660Error::UnsupportedLogicalBlockSize`]).
pub const SUPPORTED_LOGICAL_BLOCK_SIZE: u32 = 2048;

/// How many Volume Descriptors [`find_primary_volume_descriptor`] will read
/// while searching for the Primary Volume Descriptor before giving up. Real
/// discs carry a handful (Boot Record, Primary, sometimes Supplementary,
/// Terminator); this is generous headroom against a corrupt or hostile
/// image with no Terminator at all.
pub const MAX_VOLUME_DESCRIPTORS: usize = 32;

/// How many directory records one directory's extent will be parsed into
/// before this module refuses to continue. Bounds the work a single
/// maliciously large `size` field can force.
pub const MAX_ENTRIES_PER_DIRECTORY: usize = 8192;

/// How many path components [`find_path`] will descend before refusing to
/// continue. ISO9660 itself limits real hierarchies to 8 levels; this is
/// generous headroom, not an endorsement of deeper trees.
pub const MAX_PATH_DEPTH: usize = 32;

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Iso9660Error {
    Media(LogicalMediaError),
    /// The first Volume Descriptor's `id` field was not `"CD001"`.
    InvalidStandardIdentifier,
    /// No Volume Descriptor with `type == 1` (Primary) was found before
    /// either a Set Terminator (`type == 255`) or [`MAX_VOLUME_DESCRIPTORS`]
    /// was reached.
    NoPrimaryVolumeDescriptor,
    /// The Primary Volume Descriptor's `logical_block_size` both-endian
    /// halves disagreed, or agreed on a value other than
    /// [`SUPPORTED_LOGICAL_BLOCK_SIZE`].
    UnsupportedLogicalBlockSize {
        found: Option<u32>,
    },
    /// A both-endian field's little-endian and big-endian halves disagreed,
    /// which is a genuine structural inconsistency, not merely an
    /// unfamiliar value.
    InconsistentBothEndianField {
        field: &'static str,
    },
    /// A directory's `extent`/`size` fields describe a byte range outside
    /// the media.
    DirectoryExtentOutOfBounds {
        extent_lba: u32,
        size: u32,
    },
    /// A directory record's own `length` byte put it partially or wholly
    /// outside the logical block it started in, or declared a `name_len`
    /// that would not fit within that `length`.
    DirectoryRecordInvalid {
        block_offset: u64,
    },
    /// A directory's extent required parsing more than
    /// [`MAX_ENTRIES_PER_DIRECTORY`] records.
    TooManyDirectoryEntries,
    /// [`find_path`] descended more than [`MAX_PATH_DEPTH`] components.
    PathTooDeep,
}

impl From<LogicalMediaError> for Iso9660Error {
    fn from(error: LogicalMediaError) -> Self {
        Self::Media(error)
    }
}

impl std::fmt::Display for Iso9660Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Media(error) => write!(formatter, "{error}"),
            Self::InvalidStandardIdentifier => formatter
                .write_str("volume descriptor is missing the \"CD001\" standard identifier"),
            Self::NoPrimaryVolumeDescriptor => {
                formatter.write_str("no Primary Volume Descriptor found")
            }
            Self::UnsupportedLogicalBlockSize { found } => {
                write!(
                    formatter,
                    "unsupported ISO9660 logical block size: {found:?}"
                )
            }
            Self::InconsistentBothEndianField { field } => {
                write!(
                    formatter,
                    "both-endian field \"{field}\" has mismatched halves"
                )
            }
            Self::DirectoryExtentOutOfBounds { extent_lba, size } => write!(
                formatter,
                "directory extent (lba={extent_lba}, size={size}) is out of bounds for the media"
            ),
            Self::DirectoryRecordInvalid { block_offset } => {
                write!(
                    formatter,
                    "invalid directory record at block offset {block_offset}"
                )
            }
            Self::TooManyDirectoryEntries => {
                write!(
                    formatter,
                    "directory extent exceeds {MAX_ENTRIES_PER_DIRECTORY} entries"
                )
            }
            Self::PathTooDeep => write!(formatter, "path exceeds {MAX_PATH_DEPTH} components"),
        }
    }
}

impl std::error::Error for Iso9660Error {}

// ---------------------------------------------------------------------
// Both-endian field helpers
// ---------------------------------------------------------------------

/// Reads an ECMA-119 "733"-style both-endian 32-bit field (LE half at
/// `bytes[0..4]`, BE half at `bytes[4..8]`), requiring the two halves to
/// agree. A disagreement is a genuine structural inconsistency, not merely
/// an unfamiliar value - see [`Iso9660Error::InconsistentBothEndianField`].
fn read_both_endian_u32(bytes: &[u8]) -> Option<u32> {
    let little = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let big = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
    (little == big).then_some(little)
}

fn read_both_endian_u16(bytes: &[u8]) -> Option<u16> {
    let little = u16::from_le_bytes(bytes[0..2].try_into().ok()?);
    let big = u16::from_be_bytes(bytes[2..4].try_into().ok()?);
    (little == big).then_some(little)
}

// ---------------------------------------------------------------------
// Directory records / entries
// ---------------------------------------------------------------------

/// One entry read from a directory's extent - a file or a subdirectory.
/// `.` and `..` self/parent records are never returned as entries (see
/// [`read_directory_entries`]'s documentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iso9660Entry {
    /// The exact identifier recorded on the disc, including any `;n`
    /// version suffix (e.g. `"SYSTEM.CNF;1"`). Never rewritten.
    pub original_name: String,
    /// `original_name` with the `;n` version suffix stripped (if the
    /// suffix after the last `;` is entirely ASCII digits) and uppercased.
    /// ISO9660 identifiers are already uppercase d-characters in a
    /// conformant image; the uppercasing here is defensive, not a
    /// correction of anything actually observed. See the module
    /// documentation's normalization rule.
    pub comparison_name: String,
    pub is_directory: bool,
    pub extent_lba: u32,
    pub size: u32,
}

/// Strips a trailing `;<digits>` ISO9660 version suffix (if present) and
/// uppercases the result. This is the one, explicit, documented
/// normalization rule this module applies - see [`Iso9660Entry::comparison_name`].
fn comparison_name(original: &str) -> String {
    let stripped = match original.rsplit_once(';') {
        Some((base, suffix))
            if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            base
        }
        _ => original,
    };
    stripped.to_ascii_uppercase()
}

/// One parsed record, plus how many bytes of the block it consumed - `None`
/// return from [`parse_directory_record`] means "this was a `.`/`..` self
/// or parent record", which the caller filters out rather than exposing.
enum ParsedRecord {
    SelfOrParent {
        consumed: usize,
    },
    Entry {
        entry: Iso9660Entry,
        consumed: usize,
    },
}

/// Parses one directory record starting at `block[cursor..]`. `block` is
/// the remainder of the current logical block (never bytes from a
/// different block - directory records never cross block boundaries).
/// Returns `Ok(None)` for a `length == 0` byte, meaning "no more records in
/// this block, skip to the next one" - not an error.
fn parse_directory_record(
    block: &[u8],
    block_offset: u64,
) -> Result<Option<ParsedRecord>, Iso9660Error> {
    let Some(&length) = block.first() else {
        return Ok(None);
    };
    if length == 0 {
        return Ok(None);
    }
    let length = length as usize;
    if length < 34 || length > block.len() {
        return Err(Iso9660Error::DirectoryRecordInvalid { block_offset });
    }
    let record = &block[..length];

    let extent_lba =
        read_both_endian_u32(&record[2..10]).ok_or(Iso9660Error::InconsistentBothEndianField {
            field: "directory_record.extent",
        })?;
    let size =
        read_both_endian_u32(&record[10..18]).ok_or(Iso9660Error::InconsistentBothEndianField {
            field: "directory_record.size",
        })?;
    let flags = record[25];
    let name_len = record[32] as usize;
    if 33 + name_len > length {
        return Err(Iso9660Error::DirectoryRecordInvalid { block_offset });
    }
    let name_bytes = &record[33..33 + name_len];

    if name_len == 1 && (name_bytes[0] == 0x00 || name_bytes[0] == 0x01) {
        return Ok(Some(ParsedRecord::SelfOrParent { consumed: length }));
    }

    let original_name = String::from_utf8_lossy(name_bytes).into_owned();
    let entry = Iso9660Entry {
        comparison_name: comparison_name(&original_name),
        original_name,
        is_directory: flags & 0x02 != 0,
        extent_lba,
        size,
    };
    Ok(Some(ParsedRecord::Entry {
        entry,
        consumed: length,
    }))
}

/// Reads every entry directly inside the directory whose extent is
/// `(extent_lba, size)`, in on-disc order.
///
/// `.` and `..` records are deliberately excluded from the result: they
/// identify the directory itself and its parent, never a child a caller
/// would look up by name, and including them would make "does this
/// directory contain N entries" ambiguous. This is also what makes
/// recursive descent in [`find_path`] safe from the classic
/// self-referential-directory loop - `.`/`..` are the only records that
/// point backward, and they are never followed.
pub fn read_directory_entries<M: LogicalMedia>(
    media: &M,
    extent_lba: u32,
    size: u32,
    logical_block_size: u32,
) -> Result<Vec<Iso9660Entry>, Iso9660Error> {
    let block_len = logical_block_size as u64;
    let extent_start = (extent_lba as u64)
        .checked_mul(block_len)
        .ok_or(Iso9660Error::DirectoryExtentOutOfBounds { extent_lba, size })?;
    let extent_end = extent_start
        .checked_add(size as u64)
        .ok_or(Iso9660Error::DirectoryExtentOutOfBounds { extent_lba, size })?;
    if extent_end > media.len() {
        return Err(Iso9660Error::DirectoryExtentOutOfBounds { extent_lba, size });
    }

    let mut entries = Vec::new();
    let mut block_start = extent_start;
    while block_start < extent_end {
        let this_block_len = block_len.min(extent_end - block_start) as usize;
        let mut block = vec![0u8; this_block_len];
        media.read_at(block_start, &mut block)?;

        let mut cursor = 0usize;
        while cursor < block.len() {
            match parse_directory_record(&block[cursor..], block_start + cursor as u64)? {
                None => break, // rest of this block is padding
                Some(ParsedRecord::SelfOrParent { consumed }) => cursor += consumed,
                Some(ParsedRecord::Entry { entry, consumed }) => {
                    if entries.len() >= MAX_ENTRIES_PER_DIRECTORY {
                        return Err(Iso9660Error::TooManyDirectoryEntries);
                    }
                    entries.push(entry);
                    cursor += consumed;
                }
            }
        }
        block_start += block_len;
    }

    Ok(entries)
}

// ---------------------------------------------------------------------
// Volume descriptor / top-level observation
// ---------------------------------------------------------------------

/// Which logical filesystem [`DiscFilesystemObservation`] describes. A
/// single variant today - UDF, XDVDFS, and GameCube/Wii FST are deliberately
/// out of scope for this chunk (see the crate-level report for the intended
/// route for each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscFilesystemKind {
    Iso9660,
}

/// What this module directly observed about a logical filesystem's root
/// layout. Content/structure evidence only - never a platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscFilesystemObservation {
    pub filesystem_kind: DiscFilesystemKind,
    /// The Primary Volume Descriptor's `volume_id`, space-trimmed.
    pub volume_identifier: String,
    pub logical_block_size: u32,
    pub root_entries: Vec<Iso9660Entry>,
}

/// A cheap, pure pre-check: does `media` begin (at the fixed Primary Volume
/// Descriptor location) with the ISO9660 standard identifier `"CD001"`?
/// Says nothing about whether the rest of the volume/directory structure is
/// valid - mirrors [`crate::chd_identity::looks_like_chd`]'s role for CHD.
pub fn looks_like_iso9660<M: LogicalMedia>(media: &M) -> bool {
    let mut header = [0u8; 6];
    let offset = PRIMARY_VOLUME_DESCRIPTOR_LBA * SUPPORTED_LOGICAL_BLOCK_SIZE as u64;
    if media.read_at(offset, &mut header).is_err() {
        return false;
    }
    &header[1..6] == ISO9660_STANDARD_IDENTIFIER
}

struct PrimaryVolumeDescriptor {
    volume_identifier: String,
    logical_block_size: u32,
    root_extent_lba: u32,
    root_size: u32,
}

/// Scans Volume Descriptors starting at [`PRIMARY_VOLUME_DESCRIPTOR_LBA`]
/// for the Primary Volume Descriptor (`type == 1`), stopping at a Set
/// Terminator (`type == 255`) or [`MAX_VOLUME_DESCRIPTORS`], whichever
/// comes first.
fn find_primary_volume_descriptor<M: LogicalMedia>(
    media: &M,
) -> Result<PrimaryVolumeDescriptor, Iso9660Error> {
    let mut descriptor = [0u8; SUPPORTED_LOGICAL_BLOCK_SIZE as usize];

    for index in 0..MAX_VOLUME_DESCRIPTORS {
        let offset =
            (PRIMARY_VOLUME_DESCRIPTOR_LBA + index as u64) * SUPPORTED_LOGICAL_BLOCK_SIZE as u64;
        media.read_at(offset, &mut descriptor)?;

        if &descriptor[1..6] != ISO9660_STANDARD_IDENTIFIER {
            return Err(Iso9660Error::InvalidStandardIdentifier);
        }

        let descriptor_type = descriptor[0];
        if descriptor_type == 1 {
            let logical_block_size = read_both_endian_u16(&descriptor[128..132])
                .map(u32::from)
                .filter(|found| *found == SUPPORTED_LOGICAL_BLOCK_SIZE);
            let logical_block_size = logical_block_size
                .ok_or(Iso9660Error::UnsupportedLogicalBlockSize { found: None })?;

            let volume_identifier = String::from_utf8_lossy(&descriptor[40..72])
                .trim_end()
                .to_string();

            let root_record = &descriptor[156..190];
            let root_extent_lba = read_both_endian_u32(&root_record[2..10]).ok_or(
                Iso9660Error::InconsistentBothEndianField {
                    field: "root_directory_record.extent",
                },
            )?;
            let root_size = read_both_endian_u32(&root_record[10..18]).ok_or(
                Iso9660Error::InconsistentBothEndianField {
                    field: "root_directory_record.size",
                },
            )?;

            return Ok(PrimaryVolumeDescriptor {
                volume_identifier,
                logical_block_size,
                root_extent_lba,
                root_size,
            });
        }
        if descriptor_type == 255 {
            break;
        }
    }

    Err(Iso9660Error::NoPrimaryVolumeDescriptor)
}

/// Observes `media` as an ISO9660 filesystem: locates the Primary Volume
/// Descriptor, then reads the root directory's immediate entries.
///
/// Pure and read-only: every byte comes from `media.read_at`, bounds-checked
/// at every step; nothing is ever written, and no more than a handful of
/// 2048-byte blocks plus the root directory's own extent are ever read.
pub fn observe_iso9660<M: LogicalMedia>(
    media: &M,
) -> Result<DiscFilesystemObservation, Iso9660Error> {
    let pvd = find_primary_volume_descriptor(media)?;
    let root_entries = read_directory_entries(
        media,
        pvd.root_extent_lba,
        pvd.root_size,
        pvd.logical_block_size,
    )?;
    Ok(DiscFilesystemObservation {
        filesystem_kind: DiscFilesystemKind::Iso9660,
        volume_identifier: pvd.volume_identifier,
        logical_block_size: pvd.logical_block_size,
        root_entries,
    })
}

/// Looks up `path` (`/`-separated components, e.g. `"PSP_GAME/PARAM.SFO"`)
/// starting from `observation`'s root, descending into subdirectories as
/// needed via `media`. Each component is matched against
/// [`Iso9660Entry::comparison_name`] - see the module documentation's
/// normalization rule.
///
/// `Ok(None)` means "does not exist" (including the case of naming a file
/// as though it were a directory partway through the path); this is not an
/// error. `Err` is reserved for a genuine structural problem encountered
/// while reading a subdirectory's extent. Bounded to [`MAX_PATH_DEPTH`]
/// components.
pub fn find_path<M: LogicalMedia>(
    media: &M,
    observation: &DiscFilesystemObservation,
    path: &str,
) -> Result<Option<Iso9660Entry>, Iso9660Error> {
    let components: Vec<&str> = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    if components.len() > MAX_PATH_DEPTH {
        return Err(Iso9660Error::PathTooDeep);
    }

    let mut current_entries = observation.root_entries.clone();
    let mut found: Option<Iso9660Entry> = None;

    for (index, component) in components.iter().enumerate() {
        let wanted = comparison_name(component);
        let Some(entry) = current_entries
            .iter()
            .find(|entry| entry.comparison_name == wanted)
            .cloned()
        else {
            return Ok(None);
        };

        let is_last = index + 1 == components.len();
        if !is_last {
            if !entry.is_directory {
                return Ok(None);
            }
            current_entries = read_directory_entries(
                media,
                entry.extent_lba,
                entry.size,
                observation.logical_block_size,
            )?;
        }
        found = Some(entry);
    }

    Ok(found)
}

// ---------------------------------------------------------------------
// Well-known boot-relevant paths - existence facts only, never a platform
// ---------------------------------------------------------------------

/// Paths whose mere *existence* is worth recording as a content-layout
/// fact. This table says nothing about what platform a match implies -
/// see the module documentation and [`Iso9660Detector`]. Deliberately not
/// exhaustive; entries are additive research context from the EmuWiz
/// platform-research effort, not a claim that these are the only
/// interesting paths.
pub const INTERESTING_ROOT_PATHS: &[&str] = &[
    "SYSTEM.CNF",
    "PSP_GAME",
    "PSP_GAME/PARAM.SFO",
    "PSP_GAME/SYSDIR/EBOOT.BIN",
    "PS3_GAME",
    "PS3_GAME/PARAM.SFO",
    "PS3_GAME/USRDIR/EBOOT.BIN",
    "1ST_READ.BIN",
    "default.xbe",
    "default.xex",
];

// ---------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------

fn malformed_category(error: &Iso9660Error) -> &'static str {
    match error {
        Iso9660Error::Media(_) => "media_out_of_bounds",
        Iso9660Error::InvalidStandardIdentifier => "invalid_standard_identifier",
        Iso9660Error::NoPrimaryVolumeDescriptor => "no_primary_volume_descriptor",
        Iso9660Error::UnsupportedLogicalBlockSize { .. } => "unsupported_logical_block_size",
        Iso9660Error::InconsistentBothEndianField { .. } => "inconsistent_both_endian_field",
        Iso9660Error::DirectoryExtentOutOfBounds { .. } => "directory_extent_out_of_bounds",
        Iso9660Error::DirectoryRecordInvalid { .. } => "directory_record_invalid",
        Iso9660Error::TooManyDirectoryEntries => "too_many_directory_entries",
        Iso9660Error::PathTooDeep => "path_too_deep",
    }
}

/// A [`ContentDetector`] for ISO9660. Operates on `data: &[u8]` via
/// [`crate::logical_media::SliceMedia`] - the trait requires whole-buffer
/// input, so this detector only ever sees plain in-memory images, never a
/// CHD directly (a CHD's data would first need to be decompressed into a
/// logical byte stream by a caller - see the module documentation).
///
/// - [`ContentDetectionOutcome::NotRecognized`]: no `"CD001"` at the fixed
///   Primary Volume Descriptor location.
/// - [`ContentDetectionOutcome::Recognized`]: a valid Primary Volume
///   Descriptor and root directory were read. `Filesystem = ISO9660`
///   (Strong), plus one `BootStructure` fact (Weak) per
///   [`INTERESTING_ROOT_PATHS`] entry that exists on the disc - existence
///   only, never a platform (see [`Iso9660Error`]'s and the module's own
///   "hard rule" documentation, and the crate-level report for why
///   `SYSTEM.CNF`/`EBOOT.BIN`/etc. alone prove nothing).
/// - [`ContentDetectionOutcome::Malformed`]: `"CD001"` was present but the
///   volume or root directory structure failed to parse.
pub struct Iso9660Detector;

impl ContentDetector for Iso9660Detector {
    fn id(&self) -> &'static str {
        "iso9660_filesystem"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        use crate::logical_media::SliceMedia;
        let media = SliceMedia(data);

        if !looks_like_iso9660(&media) {
            return ContentDetectionOutcome::NotRecognized;
        }

        match observe_iso9660(&media) {
            Ok(observation) => {
                let mut evidence = vec![ContentEvidence::new(
                    ContentEvidenceKind::Filesystem,
                    value::ISO9660,
                    ContentEvidenceConfidence::Strong,
                    "a valid ISO9660 Primary Volume Descriptor and root directory were parsed",
                )];

                for path in INTERESTING_ROOT_PATHS {
                    if matches!(find_path(&media, &observation, path), Ok(Some(_))) {
                        evidence.push(ContentEvidence::new(
                            ContentEvidenceKind::BootStructure,
                            *path,
                            ContentEvidenceConfidence::Weak,
                            "path exists on the ISO9660 filesystem - layout fact only, not platform proof",
                        ));
                    }
                }

                ContentDetectionOutcome::Recognized { evidence }
            }
            Err(error) => ContentDetectionOutcome::Malformed {
                evidence: Vec::new(),
                diagnostic: ContentDiagnostic {
                    detector_id: "iso9660_filesystem",
                    category: malformed_category(&error),
                    message: error.to_string(),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logical_media::SliceMedia;

    /// Builds a minimal, valid ISO9660 image entirely in memory: a Primary
    /// Volume Descriptor and Set Terminator at LBA 16/17, and a root
    /// directory extent at LBA 18 containing `.`, `..`, and whatever
    /// `entries` describe. Each entry is `(name, is_directory, extent_lba,
    /// size)`; file entries reference nothing (extent/size are just facts
    /// recorded, not backed by real data at that LBA unless the caller adds
    /// it separately).
    struct FixtureBuilder {
        image: Vec<u8>,
        volume_id: &'static str,
    }

    const BLOCK: usize = SUPPORTED_LOGICAL_BLOCK_SIZE as usize;

    impl FixtureBuilder {
        fn new(volume_id: &'static str) -> Self {
            Self {
                image: vec![0u8; 16 * BLOCK],
                volume_id,
            }
        }

        fn ensure_len(&mut self, len: usize) {
            if self.image.len() < len {
                self.image.resize(len, 0);
            }
        }

        /// Writes `.`, `..`, then `entries` into the directory extent
        /// starting at `extent_lba`, respecting block boundaries exactly as
        /// real ISO9660 requires (a record that would not fit in the
        /// remainder of the current 2048-byte block starts the next block
        /// instead, leaving a `length == 0` padding byte behind). Returns
        /// the extent's total size in bytes - always a multiple of `BLOCK`.
        fn write_directory_extent(
            &mut self,
            extent_lba: u32,
            parent_lba: u32,
            entries: &[(&str, bool, u32, u32)],
        ) -> u32 {
            let start = extent_lba as usize * BLOCK;
            self.ensure_len(start + BLOCK);

            let mut cursor = start;
            let mut block_end = start + BLOCK;
            (cursor, block_end) = write_record_at(
                &mut self.image,
                cursor,
                block_end,
                &[0x00],
                false,
                extent_lba,
                BLOCK as u32,
            );
            (cursor, block_end) = write_record_at(
                &mut self.image,
                cursor,
                block_end,
                &[0x01],
                true,
                parent_lba,
                BLOCK as u32,
            );
            for (name, is_dir, child_lba, size) in entries {
                (cursor, block_end) = write_record_at(
                    &mut self.image,
                    cursor,
                    block_end,
                    name.as_bytes(),
                    *is_dir,
                    *child_lba,
                    *size,
                );
            }
            self.ensure_len(block_end);
            let _ = cursor;
            (block_end - start) as u32
        }

        fn write_pvd(&mut self, root_lba: u32, root_size: u32) {
            self.ensure_len(19 * BLOCK);
            let pvd_start = 16 * BLOCK;
            self.image[pvd_start] = 1;
            self.image[pvd_start + 1..pvd_start + 6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
            self.image[pvd_start + 6] = 1;

            let id_field = &mut self.image[pvd_start + 40..pvd_start + 72];
            id_field.fill(b' ');
            id_field[..self.volume_id.len()].copy_from_slice(self.volume_id.as_bytes());

            write_both_endian_u16(
                &mut self.image[pvd_start + 128..pvd_start + 132],
                SUPPORTED_LOGICAL_BLOCK_SIZE as u16,
            );

            let root_record = &mut self.image[pvd_start + 156..pvd_start + 190];
            root_record[0] = 34;
            write_both_endian_u32(&mut root_record[2..10], root_lba);
            write_both_endian_u32(&mut root_record[10..18], root_size);
            root_record[25] = 0x02;
            root_record[32] = 1;
            root_record[33] = 0x00;

            let terminator_start = 17 * BLOCK;
            self.image[terminator_start] = 255;
            self.image[terminator_start + 1..terminator_start + 6]
                .copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
            self.image[terminator_start + 6] = 1;
        }

        fn build(self) -> Vec<u8> {
            self.image
        }
    }

    fn write_both_endian_u32(dest: &mut [u8], value: u32) {
        dest[0..4].copy_from_slice(&value.to_le_bytes());
        dest[4..8].copy_from_slice(&value.to_be_bytes());
    }

    fn write_both_endian_u16(dest: &mut [u8], value: u16) {
        dest[0..2].copy_from_slice(&value.to_le_bytes());
        dest[2..4].copy_from_slice(&value.to_be_bytes());
    }

    /// Writes one directory record at `cursor`, first rolling over to the
    /// start of the next block if it would not fit before `block_end`.
    /// Returns the new `(cursor, block_end)`.
    fn write_record_at(
        image: &mut Vec<u8>,
        cursor: usize,
        block_end: usize,
        name: &[u8],
        is_dir: bool,
        extent_lba: u32,
        size: u32,
    ) -> (usize, usize) {
        let mut header_and_name_len = 33 + name.len();
        if name.len().is_multiple_of(2) {
            header_and_name_len += 1; // padding to keep the record even
        }
        let length = header_and_name_len;

        let (cursor, block_end) = if cursor + length > block_end {
            (block_end, block_end + BLOCK)
        } else {
            (cursor, block_end)
        };

        if image.len() < block_end {
            image.resize(block_end, 0);
        }
        image[cursor] = length as u8;
        write_both_endian_u32(&mut image[cursor + 2..cursor + 10], extent_lba);
        write_both_endian_u32(&mut image[cursor + 10..cursor + 18], size);
        image[cursor + 25] = if is_dir { 0x02 } else { 0x00 };
        image[cursor + 32] = name.len() as u8;
        image[cursor + 33..cursor + 33 + name.len()].copy_from_slice(name);
        (cursor + length, block_end)
    }

    /// A simple, single-directory fixture: root contains one file
    /// (`SYSTEM.CNF;1`) and one subdirectory (`PSP_GAME`, extent LBA 19,
    /// itself containing `PARAM.SFO`).
    fn sample_image() -> Vec<u8> {
        let mut builder = FixtureBuilder::new("SAMPLE");
        let psp_game_size = builder.write_directory_extent(19, 18, &[("PARAM.SFO", false, 0, 4)]);
        let root_size = builder.write_directory_extent(
            18,
            18,
            &[
                ("SYSTEM.CNF;1", false, 0, 100),
                ("PSP_GAME", true, 19, psp_game_size),
            ],
        );
        builder.write_pvd(18, root_size);
        builder.build()
    }

    // ------------------------------------------------------------------

    #[test]
    fn valid_iso9660_is_recognized() {
        let data = sample_image();
        assert!(looks_like_iso9660(&SliceMedia(&data)));
        assert!(Iso9660Detector.detect(&data).is_recognized());
    }

    #[test]
    fn non_iso_bytes_are_not_recognized() {
        let data = vec![0u8; 20 * BLOCK];
        assert!(!looks_like_iso9660(&SliceMedia(&data)));
        assert_eq!(
            Iso9660Detector.detect(&data),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn bad_cd001_is_rejected_safely() {
        let mut data = sample_image();
        data[16 * BLOCK + 1] ^= 0xff; // corrupt one byte of "CD001"
        assert!(!looks_like_iso9660(&SliceMedia(&data)));
        assert_eq!(
            Iso9660Detector.detect(&data),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn volume_identifier_is_read() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        assert_eq!(observation.volume_identifier, "SAMPLE");
    }

    #[test]
    fn logical_block_size_is_read() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        assert_eq!(observation.logical_block_size, SUPPORTED_LOGICAL_BLOCK_SIZE);
    }

    #[test]
    fn root_directory_is_enumerated() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        assert_eq!(observation.root_entries.len(), 2);
    }

    #[test]
    fn root_file_is_detected() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        let entry = observation
            .root_entries
            .iter()
            .find(|entry| entry.comparison_name == "SYSTEM.CNF")
            .unwrap();
        assert!(!entry.is_directory);
    }

    #[test]
    fn root_directory_entry_is_detected() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        let entry = observation
            .root_entries
            .iter()
            .find(|entry| entry.comparison_name == "PSP_GAME")
            .unwrap();
        assert!(entry.is_directory);
    }

    #[test]
    fn nested_path_lookup_works() {
        let data = sample_image();
        let media = SliceMedia(&data);
        let observation = observe_iso9660(&media).unwrap();
        let found = find_path(&media, &observation, "PSP_GAME/PARAM.SFO").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().size, 4);
    }

    #[test]
    fn version_suffix_comparison_works() {
        let data = sample_image();
        let media = SliceMedia(&data);
        let observation = observe_iso9660(&media).unwrap();
        // Recorded on-disc as "SYSTEM.CNF;1"; looked up without the suffix.
        let found = find_path(&media, &observation, "SYSTEM.CNF").unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn original_filename_is_preserved() {
        let data = sample_image();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        let entry = observation
            .root_entries
            .iter()
            .find(|entry| entry.comparison_name == "SYSTEM.CNF")
            .unwrap();
        assert_eq!(entry.original_name, "SYSTEM.CNF;1");
    }

    #[test]
    fn malformed_directory_record_fails_closed() {
        let mut builder = FixtureBuilder::new("BAD");
        let size = builder.write_directory_extent(18, 18, &[]);
        builder.write_pvd(18, size);
        let mut data = builder.build();
        // Corrupt the "." record's length byte to something absurd.
        data[18 * BLOCK] = 5; // below the 34-byte minimum
        let outcome = Iso9660Detector.detect(&data);
        assert!(outcome.is_malformed());
    }

    #[test]
    fn out_of_bounds_extent_fails_closed() {
        let mut builder = FixtureBuilder::new("BAD");
        builder.write_pvd(9999, BLOCK as u32); // extent far past the image
        let data = builder.build();
        assert!(matches!(
            observe_iso9660(&SliceMedia(&data)),
            Err(Iso9660Error::DirectoryExtentOutOfBounds { .. })
        ));
    }

    #[test]
    fn excessive_directory_entry_count_is_bounded() {
        let mut builder = FixtureBuilder::new("BIG");
        let many: Vec<(&str, bool, u32, u32)> = (0..(MAX_ENTRIES_PER_DIRECTORY + 1))
            .map(|_| ("A", false, 0, 0))
            .collect();
        let size = builder.write_directory_extent(18, 18, &many);
        builder.write_pvd(18, size);
        let data = builder.build();
        assert!(matches!(
            observe_iso9660(&SliceMedia(&data)),
            Err(Iso9660Error::TooManyDirectoryEntries)
        ));
    }

    #[test]
    fn empty_directory_is_valid() {
        let mut builder = FixtureBuilder::new("EMPTY");
        let size = builder.write_directory_extent(18, 18, &[]);
        builder.write_pvd(18, size);
        let data = builder.build();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        assert!(observation.root_entries.is_empty());
    }

    #[test]
    fn duplicate_names_are_preserved_deterministically() {
        let mut builder = FixtureBuilder::new("DUPE");
        let size =
            builder.write_directory_extent(18, 18, &[("SAME", false, 0, 1), ("SAME", false, 0, 2)]);
        builder.write_pvd(18, size);
        let data = builder.build();
        let observation = observe_iso9660(&SliceMedia(&data)).unwrap();
        let matches: Vec<_> = observation
            .root_entries
            .iter()
            .filter(|entry| entry.comparison_name == "SAME")
            .collect();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].size, 1);
        assert_eq!(matches[1].size, 2);
    }

    #[test]
    fn random_bytes_never_become_strong_platform_evidence() {
        let data: Vec<u8> = (0..(20 * BLOCK)).map(|index| (index % 251) as u8).collect();
        let outcome = Iso9660Detector.detect(&data);
        // Either NotRecognized, or - if this random data happened to
        // satisfy CD001 by chance (it won't for this deterministic
        // pattern) - every fact would still only be Filesystem/BootStructure,
        // never a platform-shaped kind or value.
        for fact in outcome.evidence() {
            assert!(matches!(
                fact.kind,
                ContentEvidenceKind::Filesystem | ContentEvidenceKind::BootStructure
            ));
        }
    }

    #[test]
    fn system_cnf_is_layout_evidence_only() {
        let data = sample_image();
        let outcome = Iso9660Detector.detect(&data);
        let fact = outcome
            .evidence()
            .iter()
            .find(|fact| fact.value == "SYSTEM.CNF")
            .unwrap();
        assert_eq!(fact.kind, ContentEvidenceKind::BootStructure);
        assert_eq!(fact.confidence, ContentEvidenceConfidence::Weak);
    }

    #[test]
    fn psp_game_directory_is_layout_evidence_only() {
        let mut builder = FixtureBuilder::new("PSP");
        let psp_game_size = builder.write_directory_extent(19, 18, &[]);
        let root_size =
            builder.write_directory_extent(18, 18, &[("PSP_GAME", true, 19, psp_game_size)]);
        builder.write_pvd(18, root_size);
        let data = builder.build();
        let outcome = Iso9660Detector.detect(&data);
        let fact = outcome
            .evidence()
            .iter()
            .find(|fact| fact.value == "PSP_GAME")
            .unwrap();
        assert_eq!(fact.kind, ContentEvidenceKind::BootStructure);
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let data = sample_image();
        let media = SliceMedia(&data);
        assert_eq!(
            observe_iso9660(&media).unwrap(),
            observe_iso9660(&media).unwrap()
        );
    }

    #[test]
    fn source_bytes_are_never_modified() {
        let data = sample_image();
        let before = data.clone();
        let _ = observe_iso9660(&SliceMedia(&data));
        assert_eq!(data, before);
    }

    #[test]
    fn iso9660_evidence_never_resolves_a_platform() {
        let data = sample_image();
        let outcome = Iso9660Detector.detect(&data);
        for fact in outcome.evidence() {
            assert!(matches!(
                fact.kind,
                ContentEvidenceKind::Filesystem | ContentEvidenceKind::BootStructure
            ));
        }
    }

    #[test]
    fn parser_is_not_coupled_specifically_to_chd() {
        // observe_iso9660/find_path/read_directory_entries are all generic
        // over `M: LogicalMedia`, and this test drives them with a plain
        // SliceMedia - no crate::chd_identity or crate::dat::archive::chd
        // type appears anywhere in this module's signatures.
        let data = sample_image();
        let media = SliceMedia(&data);
        let observation = observe_iso9660(&media).unwrap();
        assert_eq!(observation.filesystem_kind, DiscFilesystemKind::Iso9660);
    }
}
