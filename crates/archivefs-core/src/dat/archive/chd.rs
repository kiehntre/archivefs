//! Dependency-free, read-only parsing of CHD v5 header identity.
//!
//! This module deliberately stops at byte 124. It does not traverse the CHD
//! map or metadata, decompress hunks, open paths, locate parents, or mutate
//! anything. The caller owns the reader and all filesystem policy.
//!
//! Identity has three distinct meanings in a CHD v5 header:
//!
//! - [`ChdV5Header::overall_sha1`] is the candidate identity published by a
//!   MAME-style DAT `<disk sha1="...">` entry.
//! - [`ChdV5Header::raw_sha1`] identifies the CHD's internal logical byte
//!   stream. It is not a DAT disk identity and must not be compared with ROM
//!   hashes.
//! - [`ChdV5Header::parent_sha1`] points to a dependency. It is not this CHD's
//!   identity and this reader never attempts to resolve it.

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

/// Exact byte length of a CHD v5 header.
pub const CHD_V5_HEADER_BYTES: usize = 124;

/// Maximum hunk size defined by the CHD v5 format.
pub const CHD_V5_MAX_HUNK_BYTES: u32 = 512 * 1024;

const CHD_MAGIC: &[u8; 8] = b"MComprHD";
const FIXED_PREFIX_BYTES: usize = 16;

/// Identity and geometry fields exposed directly by a validated CHD v5
/// header. All integer fields are decoded from big-endian storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdV5Header {
    pub compressors: [u32; 4],
    pub logical_bytes: u64,
    pub map_offset: u64,
    pub meta_offset: u64,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    pub raw_sha1: [u8; 20],
    pub overall_sha1: [u8; 20],
    pub parent_sha1: [u8; 20],
}

impl ChdV5Header {
    /// Whether this CHD declares a parent dependency.
    ///
    /// This reports the header fact only. It does not locate, open, or verify
    /// a parent CHD.
    pub fn parent_required(&self) -> bool {
        self.parent_sha1.iter().any(|byte| *byte != 0)
    }
}

/// The exact geometry rule rejected by [`read_chd_v5_header`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdGeometryError {
    HunkBytesZero,
    HunkBytesTooLarge { found: u32, maximum: u32 },
    UnitBytesZero,
    UnitBytesExceedHunk { unit_bytes: u32, hunk_bytes: u32 },
}

/// A named refusal from the bounded CHD v5 header reader.
#[derive(Debug)]
pub enum ChdHeaderError {
    /// EOF occurred before all bytes required for the detected header were
    /// available.
    Truncated {
        expected: usize,
        actual: usize,
    },
    InvalidMagic,
    InvalidLength {
        found: u32,
    },
    UnsupportedVersion {
        found: u32,
    },
    InvalidGeometry(ChdGeometryError),
    /// A seek or read failed for a reason other than EOF.
    Io(io::Error),
}

impl fmt::Display for ChdHeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { expected, actual } => {
                write!(
                    formatter,
                    "truncated CHD header: expected {expected} bytes, got {actual}"
                )
            }
            Self::InvalidMagic => formatter.write_str("invalid CHD magic"),
            Self::InvalidLength { found } => {
                write!(formatter, "invalid CHD v5 header length: {found}")
            }
            Self::UnsupportedVersion { found } => {
                write!(formatter, "unsupported CHD version: {found}")
            }
            Self::InvalidGeometry(reason) => write!(formatter, "invalid CHD geometry: {reason:?}"),
            Self::Io(error) => write!(formatter, "CHD header I/O error: {error}"),
        }
    }
}

impl Error for ChdHeaderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ChdHeaderError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads and validates a CHD v5 header from offset zero.
///
/// At most [`CHD_V5_HEADER_BYTES`] are read. Unsupported versions are refused
/// after the fixed 16-byte prefix, so a genuine shorter v3/v4 header is not
/// mistaken for a truncated v5 header.
///
/// `map_offset` and `meta_offset` are decoded but deliberately not range
/// checked here. A header-only reader cannot validate those targets without
/// expanding into file-size, map, and metadata semantics; guessing would
/// reject valid CHDs. That validation belongs to a later full CHD reader.
pub fn read_chd_v5_header<R: Read + Seek>(reader: &mut R) -> Result<ChdV5Header, ChdHeaderError> {
    reader.seek(SeekFrom::Start(0))?;

    let mut bytes = [0_u8; CHD_V5_HEADER_BYTES];
    read_required(reader, &mut bytes[..FIXED_PREFIX_BYTES], 0)?;

    if &bytes[0..8] != CHD_MAGIC {
        return Err(ChdHeaderError::InvalidMagic);
    }

    let version = u32_at(&bytes, 12);
    if version != 5 {
        return Err(ChdHeaderError::UnsupportedVersion { found: version });
    }

    let length = u32_at(&bytes, 8);
    if length != CHD_V5_HEADER_BYTES as u32 {
        return Err(ChdHeaderError::InvalidLength { found: length });
    }

    read_required(reader, &mut bytes[FIXED_PREFIX_BYTES..], FIXED_PREFIX_BYTES)?;

    let hunk_bytes = u32_at(&bytes, 56);
    let unit_bytes = u32_at(&bytes, 60);
    validate_geometry(hunk_bytes, unit_bytes)?;

    Ok(ChdV5Header {
        compressors: [
            u32_at(&bytes, 16),
            u32_at(&bytes, 20),
            u32_at(&bytes, 24),
            u32_at(&bytes, 28),
        ],
        logical_bytes: u64_at(&bytes, 32),
        map_offset: u64_at(&bytes, 40),
        meta_offset: u64_at(&bytes, 48),
        hunk_bytes,
        unit_bytes,
        raw_sha1: array_at(&bytes, 64),
        overall_sha1: array_at(&bytes, 84),
        parent_sha1: array_at(&bytes, 104),
    })
}

fn read_required<R: Read>(
    reader: &mut R,
    destination: &mut [u8],
    already_read: usize,
) -> Result<(), ChdHeaderError> {
    let mut filled = 0;
    while filled < destination.len() {
        match reader.read(&mut destination[filled..]) {
            Ok(0) => {
                return Err(ChdHeaderError::Truncated {
                    expected: already_read + destination.len(),
                    actual: already_read + filled,
                });
            }
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ChdHeaderError::Io(error)),
        }
    }
    Ok(())
}

fn validate_geometry(hunk_bytes: u32, unit_bytes: u32) -> Result<(), ChdHeaderError> {
    let reason = if hunk_bytes == 0 {
        Some(ChdGeometryError::HunkBytesZero)
    } else if hunk_bytes > CHD_V5_MAX_HUNK_BYTES {
        Some(ChdGeometryError::HunkBytesTooLarge {
            found: hunk_bytes,
            maximum: CHD_V5_MAX_HUNK_BYTES,
        })
    } else if unit_bytes == 0 {
        Some(ChdGeometryError::UnitBytesZero)
    } else if unit_bytes > hunk_bytes {
        Some(ChdGeometryError::UnitBytesExceedHunk {
            unit_bytes,
            hunk_bytes,
        })
    } else {
        None
    };

    match reason {
        Some(reason) => Err(ChdHeaderError::InvalidGeometry(reason)),
        None => Ok(()),
    }
}

fn u32_at(bytes: &[u8; CHD_V5_HEADER_BYTES], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn u64_at(bytes: &[u8; CHD_V5_HEADER_BYTES], offset: usize) -> u64 {
    u64::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn array_at<const LENGTH: usize>(bytes: &[u8; CHD_V5_HEADER_BYTES], offset: usize) -> [u8; LENGTH] {
    let mut result = [0; LENGTH];
    result.copy_from_slice(&bytes[offset..offset + LENGTH]);
    result
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const RAW_SHA1: [u8; 20] = [0x11; 20];
    const OVERALL_SHA1: [u8; 20] = [0x22; 20];
    const PARENT_SHA1: [u8; 20] = [0x33; 20];

    fn synthetic_header() -> [u8; CHD_V5_HEADER_BYTES] {
        let mut bytes = [0_u8; CHD_V5_HEADER_BYTES];
        bytes[0..8].copy_from_slice(CHD_MAGIC);
        put_u32(&mut bytes, 8, CHD_V5_HEADER_BYTES as u32);
        put_u32(&mut bytes, 12, 5);
        for (index, compressor) in [0x0102_0304, 0x1112_1314, 0x2122_2324, 0x3132_3334]
            .into_iter()
            .enumerate()
        {
            put_u32(&mut bytes, 16 + index * 4, compressor);
        }
        put_u64(&mut bytes, 32, 0x0102_0304_0506_0708);
        put_u64(&mut bytes, 40, 0x1112_1314_1516_1718);
        put_u64(&mut bytes, 48, 0x2122_2324_2526_2728);
        put_u32(&mut bytes, 56, 0x0002_0000);
        put_u32(&mut bytes, 60, 0x0000_0800);
        bytes[64..84].copy_from_slice(&RAW_SHA1);
        bytes[84..104].copy_from_slice(&OVERALL_SHA1);
        bytes[104..124].copy_from_slice(&PARENT_SHA1);
        bytes
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    fn parse(bytes: &[u8]) -> Result<ChdV5Header, ChdHeaderError> {
        read_chd_v5_header(&mut Cursor::new(bytes))
    }

    #[test]
    fn valid_v5_header_parses_all_big_endian_fields() {
        let header = parse(&synthetic_header()).unwrap();

        assert_eq!(
            header.compressors,
            [0x0102_0304, 0x1112_1314, 0x2122_2324, 0x3132_3334]
        );
        assert_eq!(header.logical_bytes, 0x0102_0304_0506_0708);
        assert_eq!(header.map_offset, 0x1112_1314_1516_1718);
        assert_eq!(header.meta_offset, 0x2122_2324_2526_2728);
        assert_eq!(header.hunk_bytes, 0x0002_0000);
        assert_eq!(header.unit_bytes, 0x0000_0800);
    }

    #[test]
    fn sha1_fields_use_the_authoritative_offsets_and_semantics() {
        let header = parse(&synthetic_header()).unwrap();

        assert_eq!(header.raw_sha1, RAW_SHA1, "raw stream integrity field");
        assert_eq!(
            header.overall_sha1, OVERALL_SHA1,
            "DAT <disk sha1> identity candidate"
        );
        assert_eq!(
            header.parent_sha1, PARENT_SHA1,
            "dependency pointer, not this CHD's identity"
        );
    }

    #[test]
    fn all_zero_parent_does_not_require_a_parent() {
        let mut bytes = synthetic_header();
        bytes[104..124].fill(0);

        assert!(!parse(&bytes).unwrap().parent_required());
    }

    #[test]
    fn any_nonzero_parent_byte_requires_a_parent() {
        let mut bytes = synthetic_header();
        bytes[104..124].fill(0);
        bytes[123] = 1;

        assert!(parse(&bytes).unwrap().parent_required());
    }

    #[test]
    fn truncated_v5_header_is_named_and_reports_actual_bytes() {
        let bytes = synthetic_header();

        assert!(matches!(
            parse(&bytes[..123]),
            Err(ChdHeaderError::Truncated {
                expected: CHD_V5_HEADER_BYTES,
                actual: 123,
            })
        ));
    }

    #[test]
    fn bad_magic_is_refused() {
        let mut bytes = synthetic_header();
        bytes[0] ^= 0xff;

        assert!(matches!(parse(&bytes), Err(ChdHeaderError::InvalidMagic)));
    }

    #[test]
    fn wrong_v5_header_length_is_refused_before_the_body_read() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 8, 123);

        assert!(matches!(
            parse(&bytes[..FIXED_PREFIX_BYTES]),
            Err(ChdHeaderError::InvalidLength { found: 123 })
        ));
    }

    #[test]
    fn version_four_is_explicitly_unsupported() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 8, 108);
        put_u32(&mut bytes, 12, 4);

        assert!(matches!(
            parse(&bytes[..FIXED_PREFIX_BYTES]),
            Err(ChdHeaderError::UnsupportedVersion { found: 4 })
        ));
    }

    #[test]
    fn version_three_is_explicitly_unsupported() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 8, 120);
        put_u32(&mut bytes, 12, 3);

        assert!(matches!(
            parse(&bytes[..FIXED_PREFIX_BYTES]),
            Err(ChdHeaderError::UnsupportedVersion { found: 3 })
        ));
    }

    #[test]
    fn unknown_version_is_explicitly_unsupported() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 12, 99);

        assert!(matches!(
            parse(&bytes[..FIXED_PREFIX_BYTES]),
            Err(ChdHeaderError::UnsupportedVersion { found: 99 })
        ));
    }

    #[test]
    fn zero_hunk_bytes_is_refused() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 56, 0);

        assert!(matches!(
            parse(&bytes),
            Err(ChdHeaderError::InvalidGeometry(
                ChdGeometryError::HunkBytesZero
            ))
        ));
    }

    #[test]
    fn overlarge_hunk_bytes_is_refused() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 56, CHD_V5_MAX_HUNK_BYTES + 1);

        assert!(matches!(
            parse(&bytes),
            Err(ChdHeaderError::InvalidGeometry(
                ChdGeometryError::HunkBytesTooLarge {
                    found,
                    maximum: CHD_V5_MAX_HUNK_BYTES,
                }
            )) if found == CHD_V5_MAX_HUNK_BYTES + 1
        ));
    }

    #[test]
    fn zero_unit_bytes_is_refused() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 60, 0);

        assert!(matches!(
            parse(&bytes),
            Err(ChdHeaderError::InvalidGeometry(
                ChdGeometryError::UnitBytesZero
            ))
        ));
    }

    #[test]
    fn unit_bytes_larger_than_hunk_is_refused() {
        let mut bytes = synthetic_header();
        put_u32(&mut bytes, 56, 2048);
        put_u32(&mut bytes, 60, 2049);

        assert!(matches!(
            parse(&bytes),
            Err(ChdHeaderError::InvalidGeometry(
                ChdGeometryError::UnitBytesExceedHunk {
                    unit_bytes: 2049,
                    hunk_bytes: 2048,
                }
            ))
        ));
    }

    struct CountingReader<R> {
        inner: R,
        bytes_read: usize,
    }

    impl<R: Read> Read for CountingReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.inner.read(buffer)?;
            self.bytes_read += count;
            Ok(count)
        }
    }

    impl<R: Seek> Seek for CountingReader<R> {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn parser_reads_no_more_than_the_v5_header() {
        let mut input = synthetic_header().to_vec();
        input.extend([0xaa; 1024]);
        let mut reader = CountingReader {
            inner: Cursor::new(input),
            bytes_read: 0,
        };

        read_chd_v5_header(&mut reader).unwrap();

        assert_eq!(reader.bytes_read, CHD_V5_HEADER_BYTES);
        assert_eq!(reader.inner.position(), CHD_V5_HEADER_BYTES as u64);
    }

    #[test]
    fn parsing_does_not_mutate_input_bytes() {
        let original = synthetic_header().to_vec();
        let mut reader = Cursor::new(original.clone());

        read_chd_v5_header(&mut reader).unwrap();

        assert_eq!(reader.into_inner(), original);
    }
}
