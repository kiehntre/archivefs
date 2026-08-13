//! Bounded, cancellable streaming member hashing.
//!
//! Hashes a decompressed archive member's stream into the same four
//! identifiers the DAT index understands (CRC32/MD5/SHA-1/SHA-256) in one
//! chunked pass, mirroring `identity_source::hashing::hash_file_reporting`
//! but reading from an arbitrary `Read` (the archive decoder) instead of a
//! validated `File`. The per-chunk cancellation check and the byte ceiling
//! match the loose-file hasher so a multi-gigabyte member can be stopped
//! within one chunk.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::identity_source::hashing::Crc32;

use super::ArchiveMemberHashes;
use super::limits::ARCHIVE_HASH_CHUNK_BYTES;

/// Why a member could not be hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStreamError {
    /// Cancellation flag was set during a chunk read.
    Cancelled,
    /// The member exceeded `max_bytes`; refusal happened before the limit was
    /// breached further, so at most `max_bytes + chunk` bytes were read.
    TooLarge { limit: u64 },
    /// The decoder failed (corrupt data, checksum mismatch, …).
    Io(String),
}

/// Hashes completed successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedMember {
    pub hashes: ArchiveMemberHashes,
    /// Bytes actually read from the stream.
    pub bytes_read: u64,
}

/// Streams `reader` into CRC32/MD5/SHA-1/SHA-256, bounded to `max_bytes`.
///
/// Reads are checked against `cancel` before every chunk. Returns
/// [`MemberStreamError::TooLarge`] as soon as the byte count would exceed
/// `max_bytes` (at most one chunk over-read, which is then discarded — nothing
/// beyond the limit is ever hashed or trusted).
pub fn hash_member_stream<R: Read>(
    reader: R,
    max_bytes: u64,
    cancel: &AtomicBool,
) -> Result<HashedMember, MemberStreamError> {
    let mut crc = Crc32::new();
    let mut md5 = Md5::new();
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut reader = reader;
    let mut buffer = vec![0_u8; ARCHIVE_HASH_CHUNK_BYTES];
    let mut total: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MemberStreamError::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| MemberStreamError::Io(error.kind().to_string()))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > max_bytes {
            return Err(MemberStreamError::TooLarge { limit: max_bytes });
        }
        let chunk = &buffer[..read];
        crc.update(chunk);
        md5.update(chunk);
        sha1.update(chunk);
        sha256.update(chunk);
    }

    Ok(HashedMember {
        hashes: ArchiveMemberHashes {
            crc32: crc.finish_hex(),
            md5: hex(&md5.finalize()),
            sha1: hex(&sha1.finalize()),
            sha256: hex(&sha256.finalize()),
        },
        bytes_read: total,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stream_hashes_to_empty_values() {
        let cancel = AtomicBool::new(false);
        let result = hash_member_stream(&b""[..], 1024, &cancel).unwrap();
        assert_eq!(result.hashes.crc32, "00000000");
        assert_eq!(result.bytes_read, 0);
    }

    #[test]
    fn small_stream_hashes_all_four_identifiers() {
        let cancel = AtomicBool::new(false);
        let result = hash_member_stream(&b"hello world"[..], 1024, &cancel).unwrap();
        // "hello world" known digests.
        assert_eq!(result.hashes.crc32, "0d4a1185");
        assert_eq!(result.hashes.md5, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert_eq!(
            result.hashes.sha1,
            "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
        );
        assert_eq!(
            result.hashes.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(result.bytes_read, 11);
    }

    #[test]
    fn oversized_stream_is_refused_not_partially_trusted() {
        let cancel = AtomicBool::new(false);
        let result = hash_member_stream(&b"this string is longer than ten bytes"[..], 10, &cancel);
        assert_eq!(result, Err(MemberStreamError::TooLarge { limit: 10 }));
    }

    #[test]
    fn cancellation_is_checked_per_chunk() {
        // Chunk size is 256 KiB; a stream that stays within one chunk still
        // reports cancellation because the flag is read before every read.
        let cancel = AtomicBool::new(true);
        let result = hash_member_stream(&b"abc"[..], 1024, &cancel);
        assert_eq!(result, Err(MemberStreamError::Cancelled));
    }
}
