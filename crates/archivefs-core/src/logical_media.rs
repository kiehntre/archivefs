//! The smallest possible abstraction over "some bytes, addressable by
//! offset" - deliberately not a filesystem framework.
//!
//! [`crate::iso9660`] (and any future logical-media reader: UDF, XDVDFS,
//! GameCube/Wii FST) needs to read bytes at arbitrary offsets without caring
//! whether those bytes came from a plain `.iso`/`.bin` file already fully in
//! memory, or - once a CHD hunk decompressor exists (see
//! [`crate::chd_identity`]'s module documentation on that blocker) - from a
//! CHD's decompressed logical data track. Coupling the ISO9660 parser
//! directly to `&[u8]` would make that future integration a rewrite;
//! coupling it to a full virtual-filesystem trait would be over-engineering
//! for what is, today, exactly one concrete implementation. This module is
//! the middle ground: one trait, one method, one in-memory implementation.

/// Why a [`LogicalMedia::read_at`] call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalMediaError {
    /// The requested `[offset, offset + requested_len)` range does not fit
    /// inside a medium of length `media_len`.
    OutOfBounds {
        offset: u64,
        requested_len: usize,
        media_len: u64,
    },
}

impl std::fmt::Display for LogicalMediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds {
                offset,
                requested_len,
                media_len,
            } => write!(
                formatter,
                "read of {requested_len} bytes at offset {offset} is out of bounds for {media_len}-byte media"
            ),
        }
    }
}

impl std::error::Error for LogicalMediaError {}

/// Read-only, bounds-checked access to logical media by absolute byte
/// offset. Implementations must never panic on an out-of-range request -
/// they return [`LogicalMediaError::OutOfBounds`] instead - and must never
/// write anything; there is no write method on this trait to begin with.
pub trait LogicalMedia {
    /// The total addressable length, in bytes.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fills `buf` with the bytes at `[offset, offset + buf.len())`. Fails
    /// closed - `Err`, never a short read or a panic - if that range is not
    /// entirely within `[0, self.len())`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), LogicalMediaError>;
}

/// A [`LogicalMedia`] backed by a plain in-memory byte slice - the case for
/// a `.iso`/`.bin` file already fully read into memory, and for every
/// synthetic fixture in this crate's own tests.
#[derive(Debug, Clone, Copy)]
pub struct SliceMedia<'a>(pub &'a [u8]);

impl LogicalMedia for SliceMedia<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), LogicalMediaError> {
        let out_of_bounds = || LogicalMediaError::OutOfBounds {
            offset,
            requested_len: buf.len(),
            media_len: self.len(),
        };
        let start = usize::try_from(offset).map_err(|_| out_of_bounds())?;
        let end = start.checked_add(buf.len()).ok_or_else(out_of_bounds)?;
        let source = self.0.get(start..end).ok_or_else(out_of_bounds)?;
        buf.copy_from_slice(source);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_exact_requested_range() {
        let data = b"0123456789";
        let media = SliceMedia(data);
        let mut buf = [0u8; 4];
        media.read_at(3, &mut buf).unwrap();
        assert_eq!(&buf, b"3456");
    }

    #[test]
    fn reports_media_length() {
        let media = SliceMedia(b"hello");
        assert_eq!(media.len(), 5);
        assert!(!media.is_empty());
        assert!(SliceMedia(&[]).is_empty());
    }

    #[test]
    fn read_past_the_end_is_out_of_bounds() {
        let media = SliceMedia(b"short");
        let mut buf = [0u8; 4];
        assert_eq!(
            media.read_at(3, &mut buf),
            Err(LogicalMediaError::OutOfBounds {
                offset: 3,
                requested_len: 4,
                media_len: 5
            })
        );
    }

    #[test]
    fn offset_at_exact_end_with_zero_length_read_succeeds() {
        let media = SliceMedia(b"short");
        let mut buf: [u8; 0] = [];
        assert!(media.read_at(5, &mut buf).is_ok());
    }

    #[test]
    fn offset_beyond_end_is_out_of_bounds_even_for_a_zero_length_read() {
        let media = SliceMedia(b"short");
        let mut buf: [u8; 0] = [];
        assert!(media.read_at(6, &mut buf).is_err());
    }

    #[test]
    fn huge_offset_does_not_panic() {
        let media = SliceMedia(b"short");
        let mut buf = [0u8; 4];
        assert!(media.read_at(u64::MAX, &mut buf).is_err());
    }
}
