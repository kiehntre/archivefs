//! Pure, read-only CHD identity observation.
//!
//! This is the fourth normalization/identity prototype in this series, after
//! [`crate::n64_byte_order`], [`crate::header_normalization`], and
//! [`crate::smd_normalization`] - and structurally the most different one.
//! Those three each produce a *canonical byte view*; this module produces no
//! bytes at all. A CHD's compressed hunks are opaque without decompressing
//! them, and this chunk deliberately does not decompress anything. Instead
//! it exposes the identity facts a CHD v5 header *already records
//! authoritatively*, reusing the existing, already-reviewed
//! [`crate::dat::archive::chd`] header parser rather than adding a new
//! dependency or duplicating its byte-offset logic.
//!
//! # A CHD has more than one identity - verified, not assumed
//!
//! Before writing anything here, the exact semantics of the three SHA-1
//! fields a CHD v5 header carries were verified against MAME's own
//! authoritative source
//! (`https://github.com/mamedev/mame/blob/master/src/lib/util/chd.h`):
//!
//! ```text
//! [ 64] uint8_t  rawsha1[20];    // raw data SHA1
//! [ 84] uint8_t  sha1[20];       // combined raw+meta SHA1
//! [104] uint8_t  parentsha1[20]; // combined raw+meta SHA1 of parent
//! ```
//!
//! and: *"If parentsha1 != 0, we have a parent (no need for flags)"* - which
//! is exactly what [`crate::dat::archive::chd::ChdV5Header::parent_required`]
//! already implements.
//!
//! This chunk adds a fifth identity dimension - conservative *media* facts,
//! read from the CHD's own metadata chain - on top of the four this module
//! already separated:
//!
//! | Identity | What it measures | Where it lives |
//! |---|---|---|
//! | Physical CHD SHA-256 | the compressed `.chd` file's own bytes | computed by a caller (e.g. [`examples/chd_probe.rs`](../../examples/chd_probe.rs)) over the whole file, via the crate's existing hashing helper |
//! | CHD raw SHA-1 | the logical/raw data stream *inside* the CHD | [`ChdIdentityObservation::raw_sha1`] |
//! | CHD combined SHA-1 | raw data + metadata together - what a MAME-style DAT `<disk sha1="...">` entry actually names | [`ChdIdentityObservation::combined_sha1`] |
//! | CHD parent SHA-1 | the combined SHA-1 a *different* CHD must have to serve as this one's parent | [`ChdIdentityObservation::parent_sha1`] |
//! | Media facts | what the CHD's own metadata chain directly states about the media it holds (hard-disk geometry, CD/GD-ROM tracks, laserdisc/AV tags) | [`ChdIdentityObservation::metadata`] |
//!
//! None of these five are interchangeable, and this module never conflates
//! any pair of them. Media facts in particular are never a hash and never a
//! platform - see [`ChdMediaClass`] and [`ChdIdentityDetector`].
//!
//! # CHD metadata format - verified, not assumed
//!
//! Verified against MAME's own `chd.h`/`chd.cpp`
//! (`https://github.com/mamedev/mame/blob/master/src/lib/util/chd.h`,
//! `https://github.com/mamedev/mame/blob/master/src/lib/util/chd.cpp`,
//! function `chd_file::metadata_find`).
//!
//! Each metadata entry begins with a fixed 16-byte header
//! ([`CHD_METADATA_HEADER_BYTES`], `METADATA_HEADER_SIZE` upstream):
//!
//! ```text
//! [ 0] uint32_t metatag;      // big-endian FOURCC, e.g. 'G','D','D','D'
//! [ 4] uint8_t  flags;        // bit 0 = CHD_MDFLAGS_CHECKSUM
//! [ 5] uint24_t length;       // big-endian, payload length in bytes
//! [ 8] uint64_t next;         // big-endian absolute file offset of the
//!                             // next entry, or 0 to end the chain
//! [16] ..length  payload;     // the entry's own data, immediately after
//! ```
//!
//! `chd_file::metadata_find`'s own loop is exactly `while (offset != 0) {
//! read header at offset; ...; offset = next; }` - unbounded on the upstream
//! side because MAME trusts files it wrote itself. This module does not
//! extend that trust to arbitrary input: [`read_chd_metadata_chain`] bounds
//! every offset against the buffer it was actually given, refuses to revisit
//! an offset it has already read (a loop), and refuses to walk more than
//! [`CHD_METADATA_MAX_ENTRIES`] entries - real CHDs (even a 99-track CD
//! image) stay far below that cap.
//!
//! The known tag FOURCCs and their in-file text formats
//! (`chd_file::read_metadata`/`write_metadata` call sites in `chd.cpp`) are
//! reproduced in [`meta_tag`] and [`interpret_metadata_payload`].
//!
//! # What this chunk deliberately does not do
//!
//! - It does not decompress hunks. The metadata chain is stored
//!   uncompressed, directly in the file at `meta_offset`, so no compressor
//!   is ever invoked to reach it.
//! - It does not resolve `IDNT`/`KEY `/`CIS `/`DVD `/`AVAV`/`AVLD` payloads
//!   into structured facts - their tag identity, length, and flags are
//!   still recorded (see [`ChdMetadataEntry`]), but their content is left as
//!   [`ChdMetadataFact::Unparsed`] rather than guessed at. Only the hard-disk
//!   geometry, CD-ROM track, and GD-ROM track text formats are interpreted,
//!   because those are the formats independently verified above.
//! - It does not support CHD v3/v4: the underlying reader refuses them
//!   outright, so every successfully-parsed [`ChdIdentityObservation`] is a
//!   v5 header with every field structurally present.
//! - It never claims a canonical platform. See [`ChdIdentityDetector`] and
//!   [`ChdMediaClass`]'s own documentation: `MediaClass = GD-ROM` is not
//!   proof of Dreamcast (Naomi/Naomi 2/Triforce/other GD-ROM-based hardware
//!   also exist), `MediaClass = CD-ROM` is not proof of any one platform
//!   either, and nothing here imports `crate::platform` or
//!   `crate::dat::identity`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Cursor;

use crate::content_detector::{ContentDetectionOutcome, ContentDetector, ContentDiagnostic};
use crate::content_evidence::{
    ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind, value,
};
use crate::dat::archive::chd::{CHD_MAGIC, ChdHeaderError, read_chd_v5_header};

/// Every CHD v5 header identity fact this module observes, plus whether a
/// parent CHD is required and what its own metadata chain directly states.
///
/// `raw_sha1`, `combined_sha1`, and `parent_sha1` are never given the same
/// field name or type alias as each other, and none of them is the physical
/// `.chd` file's own hash - see the module documentation's identity table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdIdentityObservation {
    /// Always `5` today - the only version [`read_chd_v5_header`] parses.
    pub version: u32,
    pub logical_bytes: u64,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    /// SHA-1 of the raw/logical data stream only - never metadata, never the
    /// physical file.
    pub raw_sha1: [u8; 20],
    /// SHA-1 of raw data *and* metadata combined. This, not `raw_sha1`, is
    /// what a MAME-style DAT `<disk sha1="...">` entry identifies.
    pub combined_sha1: [u8; 20],
    /// The combined SHA-1 a parent CHD must have for this CHD to attach to
    /// it. All-zero when this CHD is standalone.
    pub parent_sha1: [u8; 20],
    /// `true` exactly when `parent_sha1` is non-zero. A `true` value is not
    /// a corruption signal - see the module documentation.
    pub parent_required: bool,
    /// What the CHD's own metadata chain directly states. Parsed
    /// independently of `parent_required`: a child CHD needing its parent
    /// still has its own metadata chain read and reported here.
    pub metadata: ChdMetadataOutcome,
}

impl ChdIdentityObservation {
    pub fn raw_sha1_hex(&self) -> String {
        hex(&self.raw_sha1)
    }

    pub fn combined_sha1_hex(&self) -> String {
        hex(&self.combined_sha1)
    }

    pub fn parent_sha1_hex(&self) -> String {
        hex(&self.parent_sha1)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `data`'s first bytes are the fixed CHD magic
/// (`"MComprHD"`) - a cheap, pure pre-check that decides nothing about
/// whether the header is otherwise valid. Reuses the exact magic constant
/// [`read_chd_v5_header`] itself checks (`crate::dat::archive::chd::CHD_MAGIC`),
/// so there is only ever one literal copy of it in the crate.
pub fn looks_like_chd(data: &[u8]) -> bool {
    data.len() >= CHD_MAGIC.len() && &data[..CHD_MAGIC.len()] == CHD_MAGIC.as_slice()
}

/// Parses `data` as a CHD v5 header, then walks its metadata chain, and
/// returns every identity fact recorded.
///
/// Pure and read-only throughout: `data` is an immutable byte slice, never
/// mutated. The header parse is unchanged from before (at most the fixed
/// 124-byte v5 header, via [`read_chd_v5_header`]); the metadata walk that
/// follows only ever reads bytes already inside `data`, bounds-checking
/// every offset against `data.len()` - see [`read_chd_metadata_chain`]. A
/// CHD that legitimately requires a parent is **not** a failure case here -
/// `parent_required` is simply `true` in an otherwise-`Ok` observation, and
/// its own metadata chain is still parsed regardless.
pub fn observe_chd_identity(data: &[u8]) -> Result<ChdIdentityObservation, ChdHeaderError> {
    let mut cursor = Cursor::new(data);
    let header = read_chd_v5_header(&mut cursor)?;
    let metadata = read_chd_metadata_chain(data, header.meta_offset);
    Ok(ChdIdentityObservation {
        version: 5,
        logical_bytes: header.logical_bytes,
        hunk_bytes: header.hunk_bytes,
        unit_bytes: header.unit_bytes,
        raw_sha1: header.raw_sha1,
        combined_sha1: header.overall_sha1,
        parent_sha1: header.parent_sha1,
        parent_required: header.parent_required(),
        metadata,
    })
}

// ---------------------------------------------------------------------
// Metadata chain
// ---------------------------------------------------------------------

/// Exact byte length of one metadata entry's header, before its payload.
/// `METADATA_HEADER_SIZE` in MAME's `chd.cpp`.
pub const CHD_METADATA_HEADER_BYTES: usize = 16;

/// A conservative upper bound on how many metadata entries this module will
/// walk before refusing to continue. Chosen to sit far above any real CHD -
/// even a 99-track CD/GD-ROM image, plus session/ident/key tags, stays in
/// the low hundreds of entries - while still bounding the work an untrusted
/// or corrupt file can force this module to do.
pub const CHD_METADATA_MAX_ENTRIES: usize = 4096;

/// Bit 0 of a metadata entry's `flags` byte: `CHD_MDFLAGS_CHECKSUM` upstream,
/// meaning the payload is checksummed. This module does not verify that
/// checksum (doing so would require knowing which algorithm/field MAME uses
/// for it, which has not been verified here); the bit is exposed as-is on
/// [`ChdMetadataEntry::flags`] for a caller who wants it.
pub const CHD_METADATA_FLAG_CHECKSUM: u8 = 0x01;

/// The well-known CHD metadata tag FOURCCs, verified against MAME's
/// `chd.h`. Values are the big-endian 32-bit encoding of the four ASCII
/// characters, exactly as `CHD_MAKE_TAG` produces upstream.
pub mod meta_tag {
    pub const HARD_DISK: u32 = u32::from_be_bytes(*b"GDDD");
    pub const HARD_DISK_IDENT: u32 = u32::from_be_bytes(*b"IDNT");
    pub const HARD_DISK_KEY: u32 = u32::from_be_bytes(*b"KEY ");
    pub const PCMCIA_CIS: u32 = u32::from_be_bytes(*b"CIS ");
    pub const CDROM_OLD: u32 = u32::from_be_bytes(*b"CHCD");
    pub const CDROM_TRACK: u32 = u32::from_be_bytes(*b"CHTR");
    pub const CDROM_TRACK2: u32 = u32::from_be_bytes(*b"CHT2");
    pub const CDROM_SESSION: u32 = u32::from_be_bytes(*b"CHSE");
    pub const GDROM_OLD: u32 = u32::from_be_bytes(*b"CHGT");
    pub const GDROM_TRACK: u32 = u32::from_be_bytes(*b"CHGD");
    pub const DVD: u32 = u32::from_be_bytes(*b"DVD ");
    pub const AV: u32 = u32::from_be_bytes(*b"AVAV");
    pub const AV_LASERDISC: u32 = u32::from_be_bytes(*b"AVLD");
}

/// Which well-known tag a metadata entry's raw FOURCC corresponds to, if
/// any. `Unknown` is not an error - an unrecognised tag is a perfectly valid
/// CHD metadata entry this module simply has no interpretation for yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdMetadataTagKind {
    HardDisk,
    HardDiskIdent,
    HardDiskKey,
    PcmciaCis,
    CdromOld,
    CdromTrack,
    CdromTrack2,
    CdromSession,
    GdromOld,
    GdromTrack,
    Dvd,
    Av,
    AvLaserDisc,
    /// A structurally valid tag this module has no name for. Carries the
    /// raw FOURCC so it is never silently dropped.
    Unknown(u32),
}

impl ChdMetadataTagKind {
    fn from_tag(tag: u32) -> Self {
        match tag {
            meta_tag::HARD_DISK => Self::HardDisk,
            meta_tag::HARD_DISK_IDENT => Self::HardDiskIdent,
            meta_tag::HARD_DISK_KEY => Self::HardDiskKey,
            meta_tag::PCMCIA_CIS => Self::PcmciaCis,
            meta_tag::CDROM_OLD => Self::CdromOld,
            meta_tag::CDROM_TRACK => Self::CdromTrack,
            meta_tag::CDROM_TRACK2 => Self::CdromTrack2,
            meta_tag::CDROM_SESSION => Self::CdromSession,
            meta_tag::GDROM_OLD => Self::GdromOld,
            meta_tag::GDROM_TRACK => Self::GdromTrack,
            meta_tag::DVD => Self::Dvd,
            meta_tag::AV => Self::Av,
            meta_tag::AV_LASERDISC => Self::AvLaserDisc,
            other => Self::Unknown(other),
        }
    }

    /// The conservative [`ChdMediaClass`] this tag's mere presence directly
    /// supports, if any. Never guesses: a tag this module cannot name
    /// (`Unknown`) or that says nothing about media class (`HardDiskIdent`,
    /// `HardDiskKey`, `PcmciaCis`, `Dvd`) returns `None`.
    fn media_class(self) -> Option<ChdMediaClass> {
        match self {
            Self::HardDisk => Some(ChdMediaClass::HardDisk),
            Self::CdromOld | Self::CdromTrack | Self::CdromTrack2 | Self::CdromSession => {
                Some(ChdMediaClass::CdRom)
            }
            Self::GdromOld | Self::GdromTrack => Some(ChdMediaClass::GdRom),
            Self::Av | Self::AvLaserDisc => Some(ChdMediaClass::LaserDisc),
            Self::HardDiskIdent
            | Self::HardDiskKey
            | Self::PcmciaCis
            | Self::Dvd
            | Self::Unknown(_) => None,
        }
    }
}

/// A conservative media class, derived only from which metadata tags a CHD
/// actually carries - never from hunk size, file extension, or anything
/// else.
///
/// There is deliberately no `Unknown` variant here: a CHD whose metadata
/// carries none of these tags simply contributes nothing to
/// [`ChdMetadataObservation::media_classes`], which returns an empty list
/// rather than inventing a placeholder value. An empty list and an explicit
/// "Unknown" tag would mean the same thing; this type only represents the
/// case where something *was* found.
///
/// **`GdRom` is not proof of Dreamcast.** Naomi, Naomi 2, Triforce, and
/// other arcade hardware also use GD-ROM CHDs. See [`ChdIdentityDetector`]
/// for how this is kept separate from platform evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChdMediaClass {
    HardDisk,
    CdRom,
    GdRom,
    LaserDisc,
}

/// Directly-stated hard-disk geometry, from a `GDDD` (`HARD_DISK_METADATA_TAG`)
/// entry's text payload `"CYLS:%d,HEADS:%d,SECS:%d,BPS:%d"`. No filesystem or
/// platform is inferred from these numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardDiskGeometry {
    pub cylinders: u32,
    pub heads: u32,
    pub sectors: u32,
    pub bytes_per_sector: u32,
}

/// Directly-stated CD-ROM track facts, from a `CHTR`/`CHT2`
/// (`CDROM_TRACK_METADATA_TAG`/`CDROM_TRACK_METADATA2_TAG`) entry's text
/// payload. `track_type` and `subtype` are kept as the exact strings CHD
/// stores (e.g. `"MODE1"`, `"AUDIO"`, `"NONE"`) rather than mapped onto an
/// enum this module would have to guess the full membership of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdromTrackFact {
    pub track: u32,
    pub track_type: String,
    pub subtype: String,
    pub frames: u32,
    /// Present only in the v2 (`CHT2`) format.
    pub pregap: Option<u32>,
    pub pregap_type: Option<String>,
    pub pregap_subtype: Option<String>,
    pub postgap: Option<u32>,
}

/// Directly-stated GD-ROM track facts, from a `CHGD` (`GDROM_TRACK_METADATA_TAG`)
/// entry's text payload. Kept as its own type, distinct from
/// [`CdromTrackFact`], because the GD-ROM text format carries an extra
/// `PAD` field the CD-ROM formats do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GdromTrackFact {
    pub track: u32,
    pub track_type: String,
    pub subtype: String,
    pub frames: u32,
    pub pad: Option<u32>,
    pub pregap: Option<u32>,
    pub pregap_type: Option<String>,
    pub pregap_subtype: Option<String>,
    pub postgap: Option<u32>,
}

/// What was made of one metadata entry's payload.
///
/// `Unparsed` is not an error and is not the same as an unrecognised tag -
/// it also covers a *recognised* tag (say, `GDDD`) whose text did not match
/// the expected format closely enough to extract every required field. In
/// both cases the entry's [`ChdMetadataEntry::tag`]/`kind`/`length`/`flags`
/// are still recorded; only the interpreted fact is withheld.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChdMetadataFact {
    HardDiskGeometry(HardDiskGeometry),
    CdromTrack(CdromTrackFact),
    GdromTrack(GdromTrackFact),
    Unparsed,
}

impl ChdMetadataFact {
    pub fn is_interpreted(&self) -> bool {
        !matches!(self, Self::Unparsed)
    }
}

/// One metadata entry, exactly as read from the chain: raw identity first,
/// interpretation (if any) second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdMetadataEntry {
    /// The raw big-endian FOURCC, always present even for a tag `kind`
    /// cannot name.
    pub tag: u32,
    pub kind: ChdMetadataTagKind,
    pub flags: u8,
    /// The payload length in bytes, as declared by the entry's own header
    /// (already bounds-checked against the buffer during parsing).
    pub length: u32,
    pub fact: ChdMetadataFact,
}

/// Every metadata entry read from one CHD's chain, in chain order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChdMetadataObservation {
    pub entries: Vec<ChdMetadataEntry>,
}

impl ChdMetadataObservation {
    /// The distinct [`ChdMediaClass`]es this metadata chain directly
    /// supports, sorted and deduplicated. Empty when no entry's tag maps to
    /// a media class - this is the "no media claim" case, not a guess.
    /// More than one entry is possible (for example a CHD carrying both
    /// `CHTR` and `CHSE` tags still yields a single `CdRom`, but a
    /// malformed/unusual file mixing CD-ROM and GD-ROM track tags would
    /// yield both, visible rather than silently collapsed).
    pub fn media_classes(&self) -> Vec<ChdMediaClass> {
        let mut classes: Vec<ChdMediaClass> = self
            .entries
            .iter()
            .filter_map(|entry| entry.kind.media_class())
            .collect();
        classes.sort();
        classes.dedup();
        classes
    }

    /// Whether any entry supporting `class` was interpreted structurally
    /// (not merely present by tag). Used to choose evidence confidence.
    fn has_interpreted_fact_for(&self, class: ChdMediaClass) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.kind.media_class() == Some(class) && entry.fact.is_interpreted())
    }
}

/// The exact reason [`read_chd_metadata_chain`] refused to keep walking a
/// metadata chain. Every variant is a genuine structural problem, never a
/// merely-unfamiliar tag (see [`ChdMetadataTagKind::Unknown`], which is not
/// an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdMetadataError {
    /// The entry's 16-byte header does not fit inside the buffer at the
    /// offset the previous entry (or the CHD header's own `meta_offset`)
    /// pointed to.
    HeaderOutOfBounds { offset: u64 },
    /// The entry's declared payload length runs past the end of the buffer.
    PayloadOutOfBounds { offset: u64, length: u32 },
    /// The chain's `next` pointer led back to an offset already visited in
    /// this walk.
    LoopDetected { offset: u64 },
    /// The chain did not terminate (`next == 0`) within
    /// [`CHD_METADATA_MAX_ENTRIES`] entries.
    ChainTooLong { max_entries: usize },
}

impl fmt::Display for ChdMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderOutOfBounds { offset } => {
                write!(
                    formatter,
                    "metadata entry header at offset {offset} is out of bounds"
                )
            }
            Self::PayloadOutOfBounds { offset, length } => {
                write!(
                    formatter,
                    "metadata entry at offset {offset} declares a {length}-byte payload that runs past the end of the file"
                )
            }
            Self::LoopDetected { offset } => {
                write!(formatter, "metadata chain loops back to offset {offset}")
            }
            Self::ChainTooLong { max_entries } => {
                write!(
                    formatter,
                    "metadata chain exceeds {max_entries} entries without terminating"
                )
            }
        }
    }
}

impl std::error::Error for ChdMetadataError {}

/// The result of walking a CHD's metadata chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChdMetadataOutcome {
    /// The header's `meta_offset` was zero: this CHD declares no metadata
    /// chain at all. Not an error.
    Empty,
    /// The chain was walked to completion (a `next == 0` terminator) within
    /// [`CHD_METADATA_MAX_ENTRIES`], and every entry's header and payload
    /// fit inside the buffer.
    Observed(ChdMetadataObservation),
    /// The chain violated a structural safety rule and was abandoned. This
    /// does not retract the CHD *header* facts already parsed - the header
    /// and the metadata chain are validated independently, exactly as the
    /// module's own identity separation principle requires.
    Malformed(ChdMetadataError),
}

/// Walks the metadata chain starting at `meta_offset` inside `data`,
/// bounds-checking, loop-detecting, and length-capping as it goes.
///
/// Pure and read-only: `data` is borrowed immutably throughout. No hunk is
/// ever decompressed - the metadata chain lives outside the compressed hunk
/// data entirely, at plain, directly-addressed offsets within `data`.
pub fn read_chd_metadata_chain(data: &[u8], meta_offset: u64) -> ChdMetadataOutcome {
    if meta_offset == 0 {
        return ChdMetadataOutcome::Empty;
    }

    let mut visited: Vec<u64> = Vec::new();
    let mut entries: Vec<ChdMetadataEntry> = Vec::new();
    let mut offset = meta_offset;

    while offset != 0 {
        if entries.len() >= CHD_METADATA_MAX_ENTRIES {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::ChainTooLong {
                max_entries: CHD_METADATA_MAX_ENTRIES,
            });
        }
        if visited.contains(&offset) {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::LoopDetected { offset });
        }
        visited.push(offset);

        let Ok(header_start) = usize::try_from(offset) else {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::HeaderOutOfBounds { offset });
        };
        let Some(header_end) = header_start.checked_add(CHD_METADATA_HEADER_BYTES) else {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::HeaderOutOfBounds { offset });
        };
        if header_end > data.len() {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::HeaderOutOfBounds { offset });
        }

        let header_bytes = &data[header_start..header_end];
        let tag = u32::from_be_bytes([
            header_bytes[0],
            header_bytes[1],
            header_bytes[2],
            header_bytes[3],
        ]);
        let flags = header_bytes[4];
        let length = u32::from_be_bytes([0, header_bytes[5], header_bytes[6], header_bytes[7]]);
        let next = u64::from_be_bytes([
            header_bytes[8],
            header_bytes[9],
            header_bytes[10],
            header_bytes[11],
            header_bytes[12],
            header_bytes[13],
            header_bytes[14],
            header_bytes[15],
        ]);

        let Some(payload_end) = header_end.checked_add(length as usize) else {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::PayloadOutOfBounds {
                offset,
                length,
            });
        };
        if payload_end > data.len() {
            return ChdMetadataOutcome::Malformed(ChdMetadataError::PayloadOutOfBounds {
                offset,
                length,
            });
        }
        let payload = &data[header_end..payload_end];

        let kind = ChdMetadataTagKind::from_tag(tag);
        let fact = interpret_metadata_payload(kind, payload);

        entries.push(ChdMetadataEntry {
            tag,
            kind,
            flags,
            length,
            fact,
        });

        offset = next;
    }

    ChdMetadataOutcome::Observed(ChdMetadataObservation { entries })
}

/// Interprets one metadata entry's payload according to its tag, verified
/// against MAME's own `chd.cpp` text formats (see the module documentation).
/// Never panics on malformed text - a payload that is not valid UTF-8, or
/// that is missing an expected field, or that has a non-numeric value where
/// a number is expected, simply yields [`ChdMetadataFact::Unparsed`].
fn interpret_metadata_payload(kind: ChdMetadataTagKind, payload: &[u8]) -> ChdMetadataFact {
    let Ok(text) = std::str::from_utf8(payload) else {
        return ChdMetadataFact::Unparsed;
    };
    let text = text.trim_end_matches('\0').trim();

    let parsed = match kind {
        ChdMetadataTagKind::HardDisk => {
            parse_hard_disk_geometry(text).map(ChdMetadataFact::HardDiskGeometry)
        }
        ChdMetadataTagKind::CdromTrack | ChdMetadataTagKind::CdromTrack2 => {
            parse_cdrom_track(text).map(ChdMetadataFact::CdromTrack)
        }
        ChdMetadataTagKind::GdromTrack => parse_gdrom_track(text).map(ChdMetadataFact::GdromTrack),
        _ => None,
    };
    parsed.unwrap_or(ChdMetadataFact::Unparsed)
}

/// Splits CHD's `"KEY:value"` text metadata (comma- or space-separated) into
/// a lookup table. Shared by every text format this module interprets, per
/// the verified formats:
/// `"CYLS:%d,HEADS:%d,SECS:%d,BPS:%d"`,
/// `"TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d [PREGAP:%d PGTYPE:%s PGSUB:%s POSTGAP:%d]"`,
/// `"TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d PAD:%d PREGAP:%d PGTYPE:%s PGSUB:%s POSTGAP:%d"`.
fn parse_key_value_tokens(text: &str) -> BTreeMap<&str, &str> {
    let mut tokens = BTreeMap::new();
    for token in text.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some((key, value)) = token.split_once(':') {
            tokens.insert(key, value);
        }
    }
    tokens
}

fn parse_hard_disk_geometry(text: &str) -> Option<HardDiskGeometry> {
    let tokens = parse_key_value_tokens(text);
    Some(HardDiskGeometry {
        cylinders: tokens.get("CYLS")?.parse().ok()?,
        heads: tokens.get("HEADS")?.parse().ok()?,
        sectors: tokens.get("SECS")?.parse().ok()?,
        bytes_per_sector: tokens.get("BPS")?.parse().ok()?,
    })
}

fn parse_cdrom_track(text: &str) -> Option<CdromTrackFact> {
    let tokens = parse_key_value_tokens(text);
    Some(CdromTrackFact {
        track: tokens.get("TRACK")?.parse().ok()?,
        track_type: (*tokens.get("TYPE")?).to_string(),
        subtype: (*tokens.get("SUBTYPE")?).to_string(),
        frames: tokens.get("FRAMES")?.parse().ok()?,
        pregap: tokens.get("PREGAP").and_then(|value| value.parse().ok()),
        pregap_type: tokens.get("PGTYPE").map(|value| value.to_string()),
        pregap_subtype: tokens.get("PGSUB").map(|value| value.to_string()),
        postgap: tokens.get("POSTGAP").and_then(|value| value.parse().ok()),
    })
}

fn parse_gdrom_track(text: &str) -> Option<GdromTrackFact> {
    let tokens = parse_key_value_tokens(text);
    Some(GdromTrackFact {
        track: tokens.get("TRACK")?.parse().ok()?,
        track_type: (*tokens.get("TYPE")?).to_string(),
        subtype: (*tokens.get("SUBTYPE")?).to_string(),
        frames: tokens.get("FRAMES")?.parse().ok()?,
        pad: tokens.get("PAD").and_then(|value| value.parse().ok()),
        pregap: tokens.get("PREGAP").and_then(|value| value.parse().ok()),
        pregap_type: tokens.get("PGTYPE").map(|value| value.to_string()),
        pregap_subtype: tokens.get("PGSUB").map(|value| value.to_string()),
        postgap: tokens.get("POSTGAP").and_then(|value| value.parse().ok()),
    })
}

fn media_class_value(class: ChdMediaClass) -> &'static str {
    match class {
        ChdMediaClass::HardDisk => value::HARD_DISK,
        ChdMediaClass::CdRom => value::CD_ROM,
        ChdMediaClass::GdRom => value::GD_ROM,
        ChdMediaClass::LaserDisc => value::LASERDISC,
    }
}

// ---------------------------------------------------------------------
// Track selection - metadata only, never bytes
// ---------------------------------------------------------------------
//
// A logical-filesystem reader (see `crate::iso9660`) needs to know *which*
// track of a multi-track CD/GD-ROM CHD to read a filesystem from, and must
// never mistake an audio track for one. This function is metadata-only
// track *classification* - it answers "which track would a logical
// filesystem reader target", never "here are its bytes". Producing those
// bytes is [`crate::chd_logical_media`]'s job, built on top of this
// function's output plus the `frames`/`pregap` facts it also exposes for
// track-boundary math.

/// A conservative choice of which CD/GD-ROM track is likely to carry a
/// logical filesystem, based only on already-parsed track metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateDataTrack {
    pub track: u32,
    pub track_type: String,
    pub media_class: ChdMediaClass,
    /// The track's declared frame (sector) count - how many CD/GD-ROM
    /// frames of this track were physically stored in the CHD.
    pub frames: u32,
    /// The track's declared pregap frame count, if the metadata format
    /// carried one. `None` only for the older v1 `CHTR` text format, which
    /// has no `PREGAP` field at all - not the same as a `PREGAP:0` fact.
    pub pregap: Option<u32>,
}

/// Picks the lowest-numbered CD-ROM or GD-ROM track whose recorded
/// `track_type` is not `"AUDIO"` (the literal token CHD's own text format
/// uses for an audio track - verified alongside the rest of the track text
/// format in this module's documentation). Returns `None` when there is no
/// such track: an all-audio disc, or a metadata chain with no CD/GD-ROM
/// track facts at all.
pub fn select_candidate_data_track(
    metadata: &ChdMetadataObservation,
) -> Option<CandidateDataTrack> {
    metadata
        .entries
        .iter()
        .filter_map(|entry| match &entry.fact {
            ChdMetadataFact::CdromTrack(track) if track.track_type != "AUDIO" => {
                Some(CandidateDataTrack {
                    track: track.track,
                    track_type: track.track_type.clone(),
                    media_class: ChdMediaClass::CdRom,
                    frames: track.frames,
                    pregap: track.pregap,
                })
            }
            ChdMetadataFact::GdromTrack(track) if track.track_type != "AUDIO" => {
                Some(CandidateDataTrack {
                    track: track.track,
                    track_type: track.track_type.clone(),
                    media_class: ChdMediaClass::GdRom,
                    frames: track.frames,
                    pregap: track.pregap,
                })
            }
            _ => None,
        })
        .min_by_key(|candidate| candidate.track)
}

/// The Dreamcast GD-ROM low-density/high-density boundary: the frame index
/// (within the CHD's own track-ordered logical stream) where the
/// high-density game-data area begins. Verified as a well-established,
/// independently-documented Dreamcast GD-ROM convention (matches the
/// `opticaldiscs` crate's own `GDROM_HD_START_LBA` constant).
///
/// Track 1 of a real GD-ROM CHD is consistently the small, CD-compatible
/// "low-density" area (a handful of warning/text files - `ABSTRACT.TXT`,
/// `BIBLIOGR.TXT`, `COPYRIGH.TXT`, and similar - never the actual game).
/// [`select_candidate_data_track`] has no way to know this and simply picks
/// the lowest-numbered non-audio track, which for a GD-ROM is always that
/// low-density track. [`needs_specialist_optical_backend`] is how a caller
/// finds out its selection is architecturally incomplete for this CHD.
pub const GDROM_HIGH_DENSITY_START_FRAME: u32 = 45000;

/// Whether this CHD is a GD-ROM whose real game data lives in a
/// high-density track beyond the low-density track
/// [`select_candidate_data_track`] would pick - i.e. whether
/// [`crate::chd_logical_media::ChdTrackLogicalMedia`] (track 1 only) can
/// only ever reach the low-density warning area for this disc, never the
/// actual game.
///
/// Pure metadata arithmetic - cumulative frame offsets, summed from the
/// metadata chain's own track order (verified true for chdman-produced
/// files: the chain lists tracks in track order) - no bytes are read. This
/// is deliberately always available, independent of any specialist-backend
/// feature, so a caller can make this routing decision even in a build
/// that has no specialist backend compiled in at all - see
/// [`crate::chd_optical_specialist`]'s module documentation for what such a
/// caller does with a `true` result.
pub fn needs_specialist_optical_backend(metadata: &ChdMetadataObservation) -> bool {
    let mut cumulative_frames: u64 = 0;
    for entry in &metadata.entries {
        let ChdMetadataFact::GdromTrack(track) = &entry.fact else {
            continue;
        };
        if track.track_type != "AUDIO" && cumulative_frames >= GDROM_HIGH_DENSITY_START_FRAME as u64
        {
            return true;
        }
        cumulative_frames += track.frames as u64;
    }
    false
}

// ---------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------

/// A [`ContentDetector`] for CHD identity and conservative media facts.
///
/// - [`ContentDetectionOutcome::NotRecognized`]: `data` does not begin with
///   the CHD magic at all - no evidence this is a CHD.
/// - [`ContentDetectionOutcome::Recognized`]: a valid, fully-readable CHD v5
///   header, whether standalone or a child requiring a parent, whose
///   metadata chain (if any) was either empty or walked safely to
///   completion. `MediaClass` evidence is added only for classes the
///   metadata chain directly supports - see [`ChdMediaClass`].
/// - [`ContentDetectionOutcome::Malformed`]: either the header failed to
///   parse, or the header parsed but its metadata chain violated a
///   structural safety rule (out-of-bounds offset/length, a loop, or an
///   excessively long chain). `Container`/`ContentSignature` evidence is
///   still included when the header itself was valid - a broken metadata
///   chain does not retract facts the header already proved.
///
/// Every fact emitted is about the *container/media*, never a platform:
/// `Container = "CHD"`, a `ContentSignature` naming the header version, and
/// `MediaClass` facts drawn only from [`content_evidence::value`] container
/// vocabulary are the only evidence kinds this detector ever produces.
/// Nothing here infers Dreamcast, MAME, Sega CD, Neo Geo CD, Naomi, or any
/// other platform from a CHD alone, and [`crate::platform::PLATFORMS`]
/// remains untouched by this module entirely.
pub struct ChdIdentityDetector;

impl ContentDetector for ChdIdentityDetector {
    fn id(&self) -> &'static str {
        "chd_identity"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_chd(data) {
            return ContentDetectionOutcome::NotRecognized;
        }

        match observe_chd_identity(data) {
            Ok(observation) => {
                let mut evidence = vec![
                    ContentEvidence::new(
                        ContentEvidenceKind::Container,
                        value::CHD,
                        ContentEvidenceConfidence::Strong,
                        "a valid CHD v5 header was parsed",
                    ),
                    ContentEvidence::new(
                        ContentEvidenceKind::ContentSignature,
                        format!("chd-v{}", observation.version),
                        ContentEvidenceConfidence::Strong,
                        "CHD header version field",
                    ),
                ];
                if observation.parent_required {
                    evidence.push(ContentEvidence::new(
                        ContentEvidenceKind::ContentSignature,
                        "chd-parent-required",
                        ContentEvidenceConfidence::Strong,
                        format!(
                            "this CHD's header declares a non-zero parent SHA-1 ({}); it is a \
                             child/differential CHD - this is a structural fact, not a \
                             corruption signal",
                            observation.parent_sha1_hex()
                        ),
                    ));
                }

                match &observation.metadata {
                    ChdMetadataOutcome::Empty => {}
                    ChdMetadataOutcome::Observed(metadata) => {
                        for class in metadata.media_classes() {
                            let confidence = if metadata.has_interpreted_fact_for(class) {
                                ContentEvidenceConfidence::Strong
                            } else {
                                ContentEvidenceConfidence::Corroborated
                            };
                            evidence.push(ContentEvidence::new(
                                ContentEvidenceKind::MediaClass,
                                media_class_value(class),
                                confidence,
                                "directly stated by the CHD's own metadata chain",
                            ));
                        }
                    }
                    ChdMetadataOutcome::Malformed(metadata_error) => {
                        return ContentDetectionOutcome::Malformed {
                            evidence,
                            diagnostic: ContentDiagnostic {
                                detector_id: "chd_identity",
                                category: metadata_malformed_category(metadata_error),
                                message: metadata_error.to_string(),
                            },
                        };
                    }
                }

                ContentDetectionOutcome::Recognized { evidence }
            }
            Err(error) => ContentDetectionOutcome::Malformed {
                evidence: Vec::new(),
                diagnostic: ContentDiagnostic {
                    detector_id: "chd_identity",
                    category: malformed_category(&error),
                    message: error.to_string(),
                },
            },
        }
    }
}

fn malformed_category(error: &ChdHeaderError) -> &'static str {
    match error {
        ChdHeaderError::Truncated { .. } => "truncated",
        ChdHeaderError::InvalidMagic => "invalid_magic",
        ChdHeaderError::InvalidLength { .. } => "invalid_length",
        ChdHeaderError::UnsupportedVersion { .. } => "unsupported_version",
        ChdHeaderError::InvalidGeometry(_) => "invalid_geometry",
        ChdHeaderError::Io(_) => "io_error",
    }
}

fn metadata_malformed_category(error: &ChdMetadataError) -> &'static str {
    match error {
        ChdMetadataError::HeaderOutOfBounds { .. } => "metadata_header_out_of_bounds",
        ChdMetadataError::PayloadOutOfBounds { .. } => "metadata_payload_out_of_bounds",
        ChdMetadataError::LoopDetected { .. } => "metadata_loop",
        ChdMetadataError::ChainTooLong { .. } => "metadata_chain_too_long",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::hash::hash_member_stream;
    use std::sync::atomic::AtomicBool;

    const RAW_SHA1: [u8; 20] = [0x11; 20];
    const COMBINED_SHA1: [u8; 20] = [0x22; 20];

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    /// A synthetic, valid CHD v5 header - the same 124-byte layout
    /// `crate::dat::archive::chd`'s own tests build, constructed
    /// independently here so this module's tests do not depend on that
    /// module's private test helpers.
    fn synthetic_chd_header(parent_sha1: [u8; 20]) -> Vec<u8> {
        let mut bytes = vec![0u8; 124];
        bytes[0..8].copy_from_slice(CHD_MAGIC);
        put_u32(&mut bytes, 8, 124);
        put_u32(&mut bytes, 12, 5);
        put_u64(&mut bytes, 32, 0x1234_5678_0000_0000); // logical_bytes
        put_u64(&mut bytes, 40, 0); // map_offset
        put_u64(&mut bytes, 48, 0); // meta_offset
        put_u32(&mut bytes, 56, 0x0002_0000); // hunk_bytes
        put_u32(&mut bytes, 60, 0x0000_0800); // unit_bytes
        bytes[64..84].copy_from_slice(&RAW_SHA1);
        bytes[84..104].copy_from_slice(&COMBINED_SHA1);
        bytes[104..124].copy_from_slice(&parent_sha1);
        bytes
    }

    /// Appends a well-formed metadata chain (each entry's `next` pointing to
    /// the following one, the last pointing to 0) after a synthetic header,
    /// and points the header's `meta_offset` at the first entry.
    fn chd_with_metadata_entries(parent_sha1: [u8; 20], entries: &[(u32, &[u8])]) -> Vec<u8> {
        let mut data = synthetic_chd_header(parent_sha1);
        let meta_start = data.len() as u64;

        let mut offsets = Vec::with_capacity(entries.len());
        let mut cursor = meta_start;
        for (_, payload) in entries {
            offsets.push(cursor);
            cursor += CHD_METADATA_HEADER_BYTES as u64 + payload.len() as u64;
        }

        for (index, (tag, payload)) in entries.iter().enumerate() {
            let next = offsets.get(index + 1).copied().unwrap_or(0);
            data.extend_from_slice(&tag.to_be_bytes());
            data.push(0); // flags
            let length = payload.len() as u32;
            data.extend_from_slice(&length.to_be_bytes()[1..]); // 24-bit BE
            data.extend_from_slice(&next.to_be_bytes());
            data.extend_from_slice(payload);
        }

        if !entries.is_empty() {
            put_u64(&mut data, 48, meta_start);
        }
        data
    }

    fn sha256_hex(data: &[u8]) -> String {
        hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
            .expect("hashing an in-memory buffer never fails")
            .hashes
            .sha256
    }

    // ------------------------------------------------------------------
    // Recognition (unchanged from the identity-only chunk)
    // ------------------------------------------------------------------

    #[test]
    fn non_chd_data_is_not_recognized() {
        let data = b"this is definitely not a CHD file at all, just text";
        assert!(!looks_like_chd(data));
        assert_eq!(
            ChdIdentityDetector.detect(data),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn empty_input_is_not_recognized() {
        assert!(!looks_like_chd(&[]));
        assert_eq!(
            ChdIdentityDetector.detect(&[]),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn valid_chd_is_recognized() {
        let data = synthetic_chd_header([0; 20]);
        assert!(looks_like_chd(&data));
        assert!(ChdIdentityDetector.detect(&data).is_recognized());
    }

    #[test]
    fn chd_version_is_recorded() {
        let data = synthetic_chd_header([0; 20]);
        assert_eq!(observe_chd_identity(&data).unwrap().version, 5);
    }

    #[test]
    fn logical_size_is_recorded() {
        let data = synthetic_chd_header([0; 20]);
        assert_eq!(
            observe_chd_identity(&data).unwrap().logical_bytes,
            0x1234_5678_0000_0000
        );
    }

    #[test]
    fn raw_sha1_is_exposed_distinctly() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.raw_sha1, RAW_SHA1);
        assert_ne!(observation.raw_sha1, observation.combined_sha1);
    }

    #[test]
    fn combined_sha1_is_exposed_distinctly() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.combined_sha1, COMBINED_SHA1);
        assert_ne!(observation.combined_sha1, observation.raw_sha1);
    }

    #[test]
    fn zero_parent_hash_is_standalone() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert!(!observation.parent_required);
    }

    #[test]
    fn nonzero_parent_hash_requires_a_parent() {
        let mut parent = [0u8; 20];
        parent[19] = 1;
        let data = synthetic_chd_header(parent);
        assert!(observe_chd_identity(&data).unwrap().parent_required);
    }

    #[test]
    fn malformed_recognizable_chd_fails_closed() {
        let mut data = synthetic_chd_header([0; 20]);
        put_u32(&mut data, 56, 0); // hunk_bytes = 0: invalid geometry
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(outcome.is_malformed());
    }

    #[test]
    fn original_bytes_are_never_modified() {
        let data = synthetic_chd_header([0; 20]);
        let before = data.clone();
        let _ = observe_chd_identity(&data);
        assert_eq!(data, before);
    }

    #[test]
    fn chd_evidence_never_resolves_a_platform() {
        let data = synthetic_chd_header([0; 20]);
        let outcome = ChdIdentityDetector.detect(&data);
        for fact in outcome.evidence() {
            assert!(matches!(
                fact.kind,
                ContentEvidenceKind::Container
                    | ContentEvidenceKind::ContentSignature
                    | ContentEvidenceKind::MediaClass
            ));
        }
    }

    // ------------------------------------------------------------------
    // Metadata chain: required minimum cases (task section 12, 1-18)
    // ------------------------------------------------------------------

    #[test]
    fn case_1_empty_metadata_chain() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.metadata, ChdMetadataOutcome::Empty);
    }

    #[test]
    fn case_2_one_known_metadata_entry() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:615,HEADS:4,SECS:17,BPS:512")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => assert_eq!(metadata.entries.len(), 1),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_3_multiple_entries() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[
                (meta_tag::CDROM_TRACK2, b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:16 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0"),
                (meta_tag::CDROM_TRACK2, b"TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:32 PREGAP:150 PGTYPE:SILENCE PGSUB:NONE POSTGAP:0"),
            ],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => assert_eq!(metadata.entries.len(), 2),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_4_unknown_tag_preserved_safely() {
        let unknown_tag = u32::from_be_bytes(*b"ZZZZ");
        let data = chd_with_metadata_entries([0; 20], &[(unknown_tag, b"anything at all")]);
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => {
                assert_eq!(metadata.entries.len(), 1);
                assert_eq!(metadata.entries[0].tag, unknown_tag);
                assert_eq!(
                    metadata.entries[0].kind,
                    ChdMetadataTagKind::Unknown(unknown_tag)
                );
                assert_eq!(metadata.entries[0].fact, ChdMetadataFact::Unparsed);
            }
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_5_invalid_metadata_offset_rejected() {
        let mut data = synthetic_chd_header([0; 20]);
        put_u64(&mut data, 48, 10_000); // far beyond the buffer
        let outcome = read_chd_metadata_chain(&data, 10_000);
        assert!(matches!(
            outcome,
            ChdMetadataOutcome::Malformed(ChdMetadataError::HeaderOutOfBounds { offset: 10_000 })
        ));

        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.metadata, outcome);
        assert!(ChdIdentityDetector.detect(&data).is_malformed());
    }

    #[test]
    fn case_6_metadata_length_out_of_bounds_rejected() {
        let mut data = synthetic_chd_header([0; 20]);
        let meta_start = data.len() as u64;
        // Header declares a 100-byte payload but supplies none.
        data.extend_from_slice(&meta_tag::HARD_DISK.to_be_bytes());
        data.push(0);
        data.extend_from_slice(&100u32.to_be_bytes()[1..]);
        data.extend_from_slice(&0u64.to_be_bytes());
        put_u64(&mut data, 48, meta_start);

        let outcome = read_chd_metadata_chain(&data, meta_start);
        assert!(matches!(
            outcome,
            ChdMetadataOutcome::Malformed(ChdMetadataError::PayloadOutOfBounds { length: 100, .. })
        ));
    }

    #[test]
    fn case_7_metadata_loop_detected() {
        let mut data = synthetic_chd_header([0; 20]);
        let meta_start = data.len() as u64;
        // A zero-length entry whose "next" points at itself.
        data.extend_from_slice(&meta_tag::HARD_DISK.to_be_bytes());
        data.push(0);
        data.extend_from_slice(&0u32.to_be_bytes()[1..]);
        data.extend_from_slice(&meta_start.to_be_bytes());
        put_u64(&mut data, 48, meta_start);

        let outcome = read_chd_metadata_chain(&data, meta_start);
        assert!(
            matches!(outcome, ChdMetadataOutcome::Malformed(ChdMetadataError::LoopDetected { offset }) if offset == meta_start)
        );
    }

    #[test]
    fn case_8_excessive_chain_length_capped() {
        let mut data = synthetic_chd_header([0; 20]);
        let meta_start = data.len() as u64;
        let unknown_tag = u32::from_be_bytes(*b"ZZZZ");

        // One more zero-length entry than the cap allows, each pointing to
        // the next distinct offset (no loop) so only the length cap fires.
        let entry_count = CHD_METADATA_MAX_ENTRIES + 1;
        for index in 0..entry_count {
            let offset = meta_start + (index as u64) * CHD_METADATA_HEADER_BYTES as u64;
            let next = if index + 1 == entry_count {
                0
            } else {
                offset + CHD_METADATA_HEADER_BYTES as u64
            };
            data.extend_from_slice(&unknown_tag.to_be_bytes());
            data.push(0);
            data.extend_from_slice(&0u32.to_be_bytes()[1..]);
            data.extend_from_slice(&next.to_be_bytes());
        }
        put_u64(&mut data, 48, meta_start);

        let outcome = read_chd_metadata_chain(&data, meta_start);
        assert!(matches!(
            outcome,
            ChdMetadataOutcome::Malformed(ChdMetadataError::ChainTooLong {
                max_entries: CHD_METADATA_MAX_ENTRIES
            })
        ));
    }

    #[test]
    fn case_9_hard_disk_metadata_parsed_correctly() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:615,HEADS:4,SECS:17,BPS:512")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => {
                assert_eq!(
                    metadata.entries[0].fact,
                    ChdMetadataFact::HardDiskGeometry(HardDiskGeometry {
                        cylinders: 615,
                        heads: 4,
                        sectors: 17,
                        bytes_per_sector: 512,
                    })
                );
            }
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_10_cd_track_metadata_parsed_correctly() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::CDROM_TRACK2,
                b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2048 PREGAP:150 PGTYPE:SILENCE PGSUB:NONE POSTGAP:0",
            )],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => match &metadata.entries[0].fact {
                ChdMetadataFact::CdromTrack(track) => {
                    assert_eq!(track.track, 1);
                    assert_eq!(track.track_type, "MODE1_RAW");
                    assert_eq!(track.subtype, "NONE");
                    assert_eq!(track.frames, 2048);
                    assert_eq!(track.pregap, Some(150));
                    assert_eq!(track.pregap_type.as_deref(), Some("SILENCE"));
                    assert_eq!(track.postgap, Some(0));
                }
                other => panic!("expected CdromTrack, got {other:?}"),
            },
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_11_gd_track_metadata_parsed_correctly() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::GDROM_TRACK,
                b"TRACK:3 TYPE:AUDIO SUBTYPE:NONE FRAMES:4500 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
            )],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => match &metadata.entries[0].fact {
                ChdMetadataFact::GdromTrack(track) => {
                    assert_eq!(track.track, 3);
                    assert_eq!(track.track_type, "AUDIO");
                    assert_eq!(track.frames, 4500);
                    assert_eq!(track.pad, Some(0));
                }
                other => panic!("expected GdromTrack, got {other:?}"),
            },
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_12_media_class_is_conservative() {
        // No metadata at all: no media class claimed.
        let plain = synthetic_chd_header([0; 20]);
        assert!(observe_chd_identity(&plain).unwrap().metadata == ChdMetadataOutcome::Empty);

        // A tag that says nothing about media class (HARD_DISK_IDENT) still
        // yields an empty media_classes() list, not a guess.
        let ident_only =
            chd_with_metadata_entries([0; 20], &[(meta_tag::HARD_DISK_IDENT, b"some ident text")]);
        let observation = observe_chd_identity(&ident_only).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => assert!(metadata.media_classes().is_empty()),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_13_gd_rom_does_not_resolve_platform() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::GDROM_TRACK,
                b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:2048 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
            )],
        );
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(outcome.evidence().iter().any(
            |fact| fact.kind == ContentEvidenceKind::MediaClass && fact.value == value::GD_ROM
        ));
        // No fact anywhere carries a platform-shaped kind or value - the
        // outcome type itself has no field a platform could occupy, and
        // this module never imports crate::platform or crate::dat::identity.
        for fact in outcome.evidence() {
            assert!(matches!(
                fact.kind,
                ContentEvidenceKind::Container
                    | ContentEvidenceKind::ContentSignature
                    | ContentEvidenceKind::MediaClass
            ));
        }
    }

    #[test]
    fn case_14_unknown_metadata_does_not_become_malformed_if_structurally_valid() {
        // A recognised tag (HARD_DISK) whose text does not match the
        // expected format at all: still Observed, just Unparsed.
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"not the expected format")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => {
                assert_eq!(metadata.entries[0].fact, ChdMetadataFact::Unparsed);
            }
            other => panic!("expected Observed (not Malformed), got {other:?}"),
        }
        assert!(ChdIdentityDetector.detect(&data).is_recognized());
    }

    #[test]
    fn case_15_parent_required_chd_still_parses_metadata() {
        let mut parent = [0u8; 20];
        parent[0] = 0xaa;
        let data = chd_with_metadata_entries(
            parent,
            &[(meta_tag::HARD_DISK, b"CYLS:1,HEADS:1,SECS:1,BPS:512")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        assert!(observation.parent_required);
        match observation.metadata {
            ChdMetadataOutcome::Observed(metadata) => assert_eq!(metadata.entries.len(), 1),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn case_16_repeated_observation_is_deterministic() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:1,HEADS:1,SECS:1,BPS:512")],
        );
        assert_eq!(
            observe_chd_identity(&data).unwrap(),
            observe_chd_identity(&data).unwrap()
        );
    }

    #[test]
    fn case_17_original_bytes_unchanged_after_metadata_parse() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:1,HEADS:1,SECS:1,BPS:512")],
        );
        let before = data.clone();
        let _ = observe_chd_identity(&data);
        assert_eq!(data, before);
    }

    #[test]
    fn case_18_physical_raw_combined_hashes_remain_distinct_concepts() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:1,HEADS:1,SECS:1,BPS:512")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let physical = sha256_hex(&data);
        assert_eq!(physical.len(), 64);
        assert_eq!(observation.raw_sha1_hex().len(), 40);
        assert_eq!(observation.combined_sha1_hex().len(), 40);
        assert_ne!(physical, observation.raw_sha1_hex());
        assert_ne!(physical, observation.combined_sha1_hex());
        assert_ne!(observation.raw_sha1_hex(), observation.combined_sha1_hex());
    }

    // ------------------------------------------------------------------
    // General
    // ------------------------------------------------------------------

    #[test]
    fn container_evidence_is_chd() {
        let data = synthetic_chd_header([0; 20]);
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(
            outcome
                .evidence()
                .iter()
                .any(|fact| fact.kind == ContentEvidenceKind::Container
                    && fact.value == value::CHD
                    && fact.confidence == ContentEvidenceConfidence::Strong)
        );
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(ChdIdentityDetector.id(), "chd_identity");
    }

    #[test]
    fn interpreted_media_class_evidence_is_strong_not_merely_corroborated() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::HARD_DISK, b"CYLS:1,HEADS:1,SECS:1,BPS:512")],
        );
        let outcome = ChdIdentityDetector.detect(&data);
        let fact = outcome
            .evidence()
            .iter()
            .find(|fact| fact.kind == ContentEvidenceKind::MediaClass)
            .expect("hard disk media class evidence expected");
        assert_eq!(fact.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn candidate_data_track_skips_leading_audio_track() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[
                (meta_tag::CDROM_TRACK2, b"TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:100 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0"),
                (meta_tag::CDROM_TRACK2, b"TRACK:2 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:200 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0"),
            ],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        let candidate = select_candidate_data_track(&metadata).expect("a non-audio track exists");
        assert_eq!(candidate.track, 2);
        assert_eq!(candidate.track_type, "MODE1_RAW");
        assert_eq!(candidate.media_class, ChdMediaClass::CdRom);
    }

    #[test]
    fn candidate_data_track_is_none_for_an_all_audio_disc() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(meta_tag::CDROM_TRACK2, b"TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:100 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0")],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        assert!(select_candidate_data_track(&metadata).is_none());
    }

    #[test]
    fn candidate_data_track_works_for_gd_rom() {
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::GDROM_TRACK,
                b"TRACK:3 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2048 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
            )],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        let candidate =
            select_candidate_data_track(&metadata).expect("a non-audio GD-ROM track exists");
        assert_eq!(candidate.media_class, ChdMediaClass::GdRom);
    }

    #[test]
    fn single_track_gd_rom_does_not_need_a_specialist_backend() {
        // A GD-ROM CHD carrying only a low-density track: the simple
        // track-1 selection already reaches everything there is.
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::GDROM_TRACK,
                b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2048 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
            )],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        assert!(!needs_specialist_optical_backend(&metadata));
    }

    #[test]
    fn real_world_shaped_gd_rom_needs_a_specialist_backend() {
        // Mirrors the real Jet Set Radio / Mr. Driller track layout this
        // chunk validated against: track 1 (low-density, small), track 2
        // (audio), track 3 (high-density game data, starting well past
        // frame 45000 once track 1 + track 2's frames are summed).
        let data = chd_with_metadata_entries(
            [0; 20],
            &[
                (
                    meta_tag::GDROM_TRACK,
                    b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:6835 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
                ),
                (
                    meta_tag::GDROM_TRACK,
                    b"TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:38165 PAD:0 PREGAP:150 PGTYPE:SILENCE PGSUB:NONE POSTGAP:0",
                ),
                (
                    meta_tag::GDROM_TRACK,
                    b"TRACK:3 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:504150 PAD:0 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
                ),
            ],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        // select_candidate_data_track still (correctly, per its own
        // documented scope) picks track 1 - the low-density area.
        assert_eq!(select_candidate_data_track(&metadata).unwrap().track, 1);
        // But this CHD does have real high-density data beyond it.
        assert!(needs_specialist_optical_backend(&metadata));
    }

    #[test]
    fn a_plain_cd_rom_never_needs_a_specialist_backend() {
        // CdromTrack facts are never considered - GDROM_HIGH_DENSITY_START_FRAME
        // is a Dreamcast-specific convention with no meaning for CD-ROM.
        let data = chd_with_metadata_entries(
            [0; 20],
            &[(
                meta_tag::CDROM_TRACK2,
                b"TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:999999 PREGAP:0 PGTYPE:NONE PGSUB:NONE POSTGAP:0",
            )],
        );
        let observation = observe_chd_identity(&data).unwrap();
        let ChdMetadataOutcome::Observed(metadata) = observation.metadata else {
            panic!("expected Observed metadata");
        };
        assert!(!needs_specialist_optical_backend(&metadata));
    }

    #[test]
    fn unparsed_tag_still_yields_corroborated_media_class_evidence() {
        // The tag identifies CD-ROM unambiguously even though this
        // particular payload didn't parse - Corroborated, not Strong.
        let data = chd_with_metadata_entries([0; 20], &[(meta_tag::CDROM_TRACK, b"garbage")]);
        let outcome = ChdIdentityDetector.detect(&data);
        let fact = outcome
            .evidence()
            .iter()
            .find(|fact| fact.kind == ContentEvidenceKind::MediaClass)
            .expect("cd-rom media class evidence expected");
        assert_eq!(fact.confidence, ContentEvidenceConfidence::Corroborated);
    }
}
