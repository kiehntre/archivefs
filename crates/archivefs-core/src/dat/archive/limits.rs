//! Central archive-verification limits (POST-ALPHA-1.1, experimental).
//!
//! Every numeric bound the member reader enforces lives here so limits are not
//! magic numbers scattered through the reader. Where an existing constant in
//! the codebase already expresses the same bound, it is reused rather than
//! duplicated; genuinely new limits are defined and documented here.

use crate::game_identity::MAX_ARCHIVE_MEMBERS;
use crate::identity_source::hashing::{HASH_CHUNK_BYTES, MAX_AUTOMATIC_HASH_BYTES};

/// Per-member streamed hash chunk size. Reused from the loose-file hasher so
/// memory stays flat regardless of member size.
pub const ARCHIVE_HASH_CHUNK_BYTES: usize = HASH_CHUNK_BYTES;

/// Largest member (declared logical bytes) that will be hashed without being
/// asked twice. Mirrors the loose-file ceiling so an archive member costs no
/// more than a loose file would. Member sizes above this are refused before
/// any decode.
pub const MAX_MEMBER_LOGICAL_BYTES: u64 = MAX_AUTOMATIC_HASH_BYTES;

/// Largest number of members one archive may contribute. Reused from the
/// game-identity bound; the long-known disagreement with
/// `inspector::INSPECTOR_ENTRY_LIMIT` (100_000) is a deliberate reconcile
/// item for a later slice, not for this experimental scaffold.
pub const MAX_MEMBERS_PER_ARCHIVE: usize = MAX_ARCHIVE_MEMBERS;

/// Largest total logical bytes decoded across all members of one archive.
///
/// NEW. Guards the sum of members hashed in a single archive, independent of
/// any single member's size.
pub const MAX_ARCHIVE_LOGICAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Largest bytes of one solid (multi-member) decode block.
///
/// NEW, and stricter than the ZIP case on purpose: in a solid 7z archive,
/// reaching member K costs decoding every member before it in the same block
/// (`sevenz-rust` documents this — you cannot skip ahead in a solid block).
/// The cap converts an unbounded sequential decode into a named refusal at a
/// predictable bound.
pub const MAX_SOLID_DECODE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Largest 7z dictionary (LZMA/LZMA2) that will be allocated.
///
/// NEW. 7z headers can declare multi-GiB dictionaries; the crate itself only
/// rejects dictionaries above 4 GiB and otherwise allocates on demand, so the
/// bound must be ours, checked from coder properties **before** decode.
pub const MAX_7Z_DICTIONARY_BYTES: u64 = 1024 * 1024 * 1024;

/// Ceiling for decompression amplification (`unpack / pack`) per folder.
///
/// NEW. A classic archive-bomb heuristic, evaluated against declared sizes
/// before any byte is decoded. The exact value is a product decision (the
/// research flags it UNCERTAIN); this is the placeholder ceiling.
pub const MAX_7Z_COMPRESSION_RATIO: u64 = 1000;

/// Largest 7z next-header size the pre-decoder probe will read.
///
/// NEW. `SevenZReader::new` copies `next_header_size` bytes into a buffer it
/// allocates from untrusted metadata; the probe validates this value against
/// this ceiling **before** any such allocation. Legitimate 7z headers are
/// small (bytes-to-KiB); 16 MiB is a generous safe ceiling.
pub const MAX_7Z_HEADER_BYTES: usize = 16 * 1024 * 1024;

/// Largest single coder-properties blob the pre-decoder probe will accept.
///
/// NEW. Coder properties (LZMA/LZMA2 dictionary declarations live inside
/// them) are attacker-controlled header bytes; the probe caps each blob so a
/// hostile archive cannot demand a huge parse/allocation.
pub const MAX_7Z_CODER_PROPERTIES_BYTES: usize = 1024 * 1024;

/// The tunable set an archive reader enforces. Defaults are the shared
/// constants above; tests shrink individual fields to force refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_members: usize,
    pub max_member_logical_bytes: u64,
    pub max_archive_logical_bytes: u64,
    pub max_solid_decode_bytes: u64,
    pub max_dictionary_bytes: u64,
    pub max_compression_ratio: u64,
    /// Ceiling on the 7z next-header size the pre-decoder probe will read and
    /// parse before `sevenz-rust` is ever constructed.
    pub max_header_bytes: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_members: MAX_MEMBERS_PER_ARCHIVE,
            max_member_logical_bytes: MAX_MEMBER_LOGICAL_BYTES,
            max_archive_logical_bytes: MAX_ARCHIVE_LOGICAL_BYTES,
            max_solid_decode_bytes: MAX_SOLID_DECODE_BYTES,
            max_dictionary_bytes: MAX_7Z_DICTIONARY_BYTES,
            max_compression_ratio: MAX_7Z_COMPRESSION_RATIO,
            max_header_bytes: MAX_7Z_HEADER_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_reuse_the_documented_constants() {
        let limits = ArchiveLimits::default();
        assert_eq!(limits.max_members, MAX_MEMBERS_PER_ARCHIVE);
        assert_eq!(limits.max_member_logical_bytes, MAX_MEMBER_LOGICAL_BYTES);
        assert_eq!(limits.max_archive_logical_bytes, MAX_ARCHIVE_LOGICAL_BYTES);
        assert_eq!(limits.max_solid_decode_bytes, MAX_SOLID_DECODE_BYTES);
        assert_eq!(limits.max_dictionary_bytes, MAX_7Z_DICTIONARY_BYTES);
        assert_eq!(limits.max_compression_ratio, MAX_7Z_COMPRESSION_RATIO);
        assert_eq!(limits.max_header_bytes, MAX_7Z_HEADER_BYTES);
    }
}
