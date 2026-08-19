//! Pure, read-only XDVDFS volume-descriptor magic check - shared by
//! [`crate::xbox_boot_evidence`] (original Xbox) and
//! [`crate::xbox360_boot_evidence`] (Xbox 360), since both consoles use the
//! same disc filesystem.
//!
//! # Deliberately a minimal structural observer, not a filesystem browser
//!
//! A pure-Rust `xdvdfs` crate (`https://crates.io/crates/xdvdfs`, by
//! antangelo) is already an `archivefs-core` dependency, so this crate does
//! not need to hand-write XDVDFS directory/FST parsing from scratch. This
//! module deliberately does not integrate it yet, though: wiring real
//! volume traversal into this arc's [`crate::logical_media::LogicalMedia`]
//! pipeline is a separate, larger integration this chunk's time budget does
//! not cover. What ships here is the cheap, verified magic check alone -
//! "never fake filesystem traversal" - with the crate dependency already in
//! place for that follow-up.
//!
//! # Format verified, not assumed
//!
//! The 20-byte magic string and its location (logical sector 32, i.e. byte
//! offset `32 * 2048 = 0x10000`) are verified against two independent
//! sources that agree exactly: the Xbox-Linux XDVDFS documentation
//! (`https://multimedia.cx/xdvdfs.html`) and the `xdvdfs` crate's own
//! source (`layout.rs`: `pub const VOLUME_HEADER_MAGIC: [u8; 0x14] =
//! *b"MICROSOFT*XBOX*MEDIA";`), which this module's constant is defined to
//! match exactly.

pub const XDVDFS_VOLUME_DESCRIPTOR_SECTOR: u64 = 32;
pub const XDVDFS_SECTOR_BYTES: u64 = 2048;
pub const XDVDFS_VOLUME_HEADER_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

/// The byte offset of the volume descriptor within a logical XDVDFS data
/// stream.
pub const XDVDFS_VOLUME_DESCRIPTOR_OFFSET: u64 =
    XDVDFS_VOLUME_DESCRIPTOR_SECTOR * XDVDFS_SECTOR_BYTES;

/// Whether `sector` (the bytes read from [`XDVDFS_VOLUME_DESCRIPTOR_OFFSET`])
/// begins with the XDVDFS magic.
pub fn looks_like_xdvdfs(sector: &[u8]) -> bool {
    sector.len() >= XDVDFS_VOLUME_HEADER_MAGIC.len()
        && &sector[..XDVDFS_VOLUME_HEADER_MAGIC.len()] == XDVDFS_VOLUME_HEADER_MAGIC.as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_matches_the_xdvdfs_crates_own_constant() {
        assert_eq!(
            XDVDFS_VOLUME_HEADER_MAGIC.as_slice(),
            xdvdfs::layout::VOLUME_HEADER_MAGIC.as_slice()
        );
    }

    #[test]
    fn valid_magic_is_recognized() {
        assert!(looks_like_xdvdfs(XDVDFS_VOLUME_HEADER_MAGIC.as_slice()));
    }

    #[test]
    fn non_matching_bytes_are_not_recognized() {
        assert!(!looks_like_xdvdfs(b"not an xdvdfs volume"));
    }

    #[test]
    fn truncated_magic_fails_closed() {
        assert!(!looks_like_xdvdfs(b"MICROSOFT*XBOX"));
    }

    #[test]
    fn offset_matches_sector_32() {
        assert_eq!(XDVDFS_VOLUME_DESCRIPTOR_OFFSET, 0x10000);
    }
}
