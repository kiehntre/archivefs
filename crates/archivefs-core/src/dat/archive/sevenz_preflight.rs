//! Pre-decoder 7z header probe (POST-ALPHA-1.1, experimental).
//!
//! A minimal, bounded parser over just enough of the 7z header to enforce
//! hard resource limits **before** `sevenz-rust` is ever constructed. It is
//! deliberately NOT a complete 7z decoder: it extracts the information needed
//! for resource accounting (next-header size, encoded-header presence,
//! pack-stream/file/folder/coder counts, LZMA/LZMA2 dictionary declarations
//! and the **aggregate** decoder memory of each folder's coder chain, packed/
//! unpacked sizes, per-member sub-stream sizes, and the FilesInfo layout) and
//! refuses hostile declarations. It is not a full parser, but it validates
//! enough structure (FilesInfo properties, names, bind-pair and packed-stream
//! indices, pack-stream consumption, CRCs, truncation, trailing bytes) that
//! malformed input cannot reach panic-prone code inside `sevenz-rust`.
//!
//! # Bounds before allocation
//!
//! Every count/size read from the header is validated against a configured
//! ceiling before any `Vec` of that size is allocated, all arithmetic on
//! untrusted lengths is checked, and `u64` lengths are converted to `usize`
//! with checked conversion. Reads past the bounded header buffer are a named
//! malformed-archive refusal, never a panic.
//!
//! # Encoded headers
//!
//! A 7z archive may store its real header in a packed/encrypted "encoded
//! header" block whose decompressed size is not known until it is decoded.
//! Because that expansion cannot be bounded without decoding, the probe
//! refuses every archive with an encoded header. This is a fail-closed
//! experimental restriction; see the module doc of `sevenz.rs`.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::identity_source::hashing::Crc32;

use super::limits::{ARCHIVE_HASH_CHUNK_BYTES, ArchiveLimits, MAX_7Z_CODER_PROPERTIES_BYTES};

pub(crate) const SEVEN_Z_SIGNATURE: &[u8] = &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
pub(crate) const SIGNATURE_HEADER_SIZE: u64 = 32;
const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS_INFO: u8 = 0x03;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_EMPTY_STREAM: u8 = 0x0E;
const K_EMPTY_FILE: u8 = 0x0F;
const K_ANTI: u8 = 0x10;
const K_NAME: u8 = 0x11;
const K_START_POS: u8 = 0x18;
const K_ENCODED_HEADER: u8 = 0x17;

const ID_LZMA: &[u8] = &[0x03, 0x01, 0x01];
const ID_LZMA2: &[u8] = &[0x21];

/// Why a hostile (or malformed) header was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightRefusal {
    BadSignature,
    UnsupportedVersion {
        major: u8,
        minor: u8,
    },
    /// The real header is stored packed/encrypted and cannot be bounded.
    EncodedHeader,
    /// A read went past the end of the file or the bounded header buffer.
    Truncated,
    NextHeaderTooLarge {
        size: u64,
        limit: usize,
    },
    TooManyFiles {
        count: u64,
        limit: usize,
    },
    TooManyFolders {
        count: u64,
        limit: usize,
    },
    TooManyPackStreams {
        count: u64,
        limit: usize,
    },
    /// A single folder's coder chain exceeds the per-folder ceiling.
    CoderChainTooLong {
        count: usize,
        limit: usize,
    },
    PropertyBlobTooLarge {
        size: u64,
        limit: usize,
    },
    DictionaryTooLarge {
        dictionary: u64,
        limit: u64,
    },
    /// The checked sum of a folder's LZMA/LZMA2 dictionaries (simultaneously
    /// constructed decoder memory) exceeds the aggregate budget.
    AggregateDecoderMemoryExceeded {
        bytes: u64,
        limit: u64,
    },
    MemberSizeExceeded {
        size: u64,
        limit: u64,
    },
    SolidDecodeBudgetExceeded {
        bytes: u64,
        limit: u64,
    },
    CompressionRatioExceeded {
        unpack: u64,
        pack: u64,
        ratio: u64,
    },
    LogicalBytesExceeded {
        bytes: u64,
        limit: u64,
    },
    MemberCountExceeded {
        count: usize,
        limit: usize,
    },
    ArithmeticOverflow,
    Malformed {
        detail: &'static str,
    },
    Cancelled,
    Io(String),
}

/// The resource-relevant facts extracted from a plain (non-encoded) header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SevenZPreflightInfo {
    /// Stream-bearing members implied by the sub-stream counts.
    pub member_count: usize,
    /// Sum of every folder's declared unpacked size (all stream members,
    /// including nested ones), checked against the logical-bytes ceiling.
    pub total_logical_bytes: u64,
    /// Largest single folder's unpacked size (the solid-decode worst case).
    pub max_folder_unpack: u64,
}

/// A minimal coder record: method id bytes + properties (LZMA/LZMA2
/// dictionary declarations live in `properties`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CoderInfo {
    id: Vec<u8>,
    props: Vec<u8>,
}

/// A minimal folder record: everything the probe needs for accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderInfo {
    coders: Vec<CoderInfo>,
    total_output_streams: usize,
    bind_pairs: Vec<(u64, u64)>,
    packed_streams: Vec<u64>,
    unpack_sizes: Vec<u64>,
    num_unpack_sub_streams: usize,
    has_crc: bool,
}

impl FolderInfo {
    /// The folder's declared unpacked size, mirroring sevenz-rust's
    /// `Folder::get_unpack_size`: the last output stream with no bind pair.
    fn unpack_size(&self) -> u64 {
        for index in (0..self.total_output_streams).rev() {
            if !self.bind_pairs.iter().any(|&(_, out)| out == index as u64) {
                return self.unpack_sizes.get(index).copied().unwrap_or(0);
            }
        }
        0
    }
}

/// Parses and enforces limits over the 7z next header found in `reader`.
///
/// `reader` must be the already-validated read-only handle; `reader_len` is
/// its observed length. The probe reads only the 32-byte file header plus the
/// (bounded) next header, validates both CRCs, checks cancellation during the
/// header read and every count-driven parse loop, and returns the extracted
/// resource facts.
pub fn preflight_sevenz<R: Read + Seek>(
    reader: &mut R,
    reader_len: u64,
    limits: &ArchiveLimits,
    cancel: &AtomicBool,
) -> Result<SevenZPreflightInfo, PreflightRefusal> {
    if cancel.load(Ordering::Relaxed) {
        return Err(PreflightRefusal::Cancelled);
    }
    let file_header =
        read_at(reader, 0, 32).map_err(|detail| PreflightRefusal::Io(detail.to_string()))?;
    if file_header.len() != 32 || &file_header[..6] != SEVEN_Z_SIGNATURE {
        return Err(PreflightRefusal::BadSignature);
    }
    if file_header[6] != 0 {
        return Err(PreflightRefusal::UnsupportedVersion {
            major: file_header[6],
            minor: file_header[7],
        });
    }
    let start_header_crc = u32::from_le_bytes(file_header[8..12].try_into().unwrap());
    let next_header_offset = u64::from_le_bytes(file_header[12..20].try_into().unwrap());
    let next_header_size = u64::from_le_bytes(file_header[20..28].try_into().unwrap());
    let stored_next_crc = u32::from_le_bytes(file_header[28..32].try_into().unwrap());

    // The start-header CRC is a real CRC even when its stored value is 0; it
    // is always validated (a 0 stored value must still equal the computed one).
    let mut start_crc = Crc32::new();
    start_crc.update(&file_header[12..32]);
    if start_crc.finish() != start_header_crc {
        return Err(PreflightRefusal::Malformed {
            detail: "start header checksum mismatch",
        });
    }

    if next_header_size > limits.max_header_bytes as u64 {
        return Err(PreflightRefusal::NextHeaderTooLarge {
            size: next_header_size,
            limit: limits.max_header_bytes,
        });
    }
    let next_header_pos = SIGNATURE_HEADER_SIZE
        .checked_add(next_header_offset)
        .ok_or(PreflightRefusal::ArithmeticOverflow)?;
    let header_end = next_header_pos
        .checked_add(next_header_size)
        .ok_or(PreflightRefusal::ArithmeticOverflow)?;
    if header_end > reader_len {
        return Err(PreflightRefusal::Truncated);
    }

    // Read the next header in bounded chunks. The size was validated against
    // the ceiling before this allocation; cancellation surfaces distinctly.
    let mut header = vec![0_u8; next_header_size as usize];
    match read_at_chunked(reader, next_header_pos, &mut header, cancel) {
        Ok(()) => {}
        Err(ReadFailure::Cancelled) => return Err(PreflightRefusal::Cancelled),
        Err(ReadFailure::Io(detail)) => return Err(PreflightRefusal::Io(detail)),
    }

    // The next-header CRC is validated before its structure is trusted.
    let mut next_crc = Crc32::new();
    next_crc.update(&header);
    if next_crc.finish() != stored_next_crc {
        return Err(PreflightRefusal::Malformed {
            detail: "next header checksum mismatch",
        });
    }

    let mut cursor = HeaderCursor::new(&header);
    let info = parse_header(&mut cursor, limits, cancel)?;
    if !cursor.is_exhausted() {
        return Err(PreflightRefusal::Malformed {
            detail: "trailing header bytes",
        });
    }
    Ok(info)
}

fn parse_header(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    cancel: &AtomicBool,
) -> Result<SevenZPreflightInfo, PreflightRefusal> {
    let mut nid = cursor.read_u8()?;
    if nid == K_ENCODED_HEADER {
        return Err(PreflightRefusal::EncodedHeader);
    }
    if nid != K_HEADER {
        return Err(PreflightRefusal::Malformed {
            detail: "expected K_HEADER",
        });
    }
    nid = cursor.read_u8()?;
    if nid == K_ARCHIVE_PROPERTIES {
        parse_archive_properties(cursor, cancel)?;
        nid = cursor.read_u8()?;
    }
    if nid == K_ADDITIONAL_STREAMS_INFO {
        return Err(PreflightRefusal::Malformed {
            detail: "additional streams are unsupported",
        });
    }

    let mut pack_sizes: Vec<u64> = Vec::new();
    let mut folders: Vec<FolderInfo> = Vec::new();
    let mut total_stream_files: usize = 0;
    if nid == K_MAIN_STREAMS_INFO {
        nid = cursor.read_u8()?;
        if nid == K_PACK_INFO {
            parse_pack_info(cursor, limits, &mut pack_sizes, cancel)?;
            nid = cursor.read_u8()?;
        }
        if nid == K_UNPACK_INFO {
            parse_unpack_info(cursor, limits, &mut folders, cancel)?;
            nid = cursor.read_u8()?;
        }
        if nid == K_SUB_STREAMS_INFO {
            parse_sub_streams_info(cursor, limits, &mut folders, cancel)?;
            nid = cursor.read_u8()?;
        }
        if nid != K_END {
            return Err(PreflightRefusal::Malformed {
                detail: "bad streams-info terminator",
            });
        }
        nid = cursor.read_u8()?;
        total_stream_files = folders
            .iter()
            .map(|folder| folder.num_unpack_sub_streams)
            .sum();
    }
    if nid == K_FILES_INFO {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        let num_files = cursor.read_varint()?;
        if num_files > limits.max_members as u64 {
            return Err(PreflightRefusal::TooManyFiles {
                count: num_files,
                limit: limits.max_members,
            });
        }
        parse_files_info(cursor, num_files as usize, total_stream_files, cancel)?;
        nid = cursor.read_u8()?;
    } else if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "missing files info",
        });
    }
    if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "missing header terminator",
        });
    }

    enforce_and_summarize(&pack_sizes, &folders, limits)
}

fn parse_archive_properties(
    cursor: &mut HeaderCursor<'_>,
    cancel: &AtomicBool,
) -> Result<(), PreflightRefusal> {
    let mut nid = cursor.read_u8()?;
    while nid != K_END {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        let size = cursor.read_varint()?;
        cursor.skip(size)?;
        nid = cursor.read_u8()?;
    }
    Ok(())
}

fn parse_pack_info(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    pack_sizes: &mut Vec<u64>,
    cancel: &AtomicBool,
) -> Result<(), PreflightRefusal> {
    let _pack_pos = cursor.read_varint()?;
    let num_pack_streams = cursor.read_varint()?;
    if num_pack_streams > limits.max_members as u64 {
        return Err(PreflightRefusal::TooManyPackStreams {
            count: num_pack_streams,
            limit: limits.max_members,
        });
    }
    let mut nid = cursor.read_u8()?;
    if nid == K_SIZE {
        pack_sizes.reserve(num_pack_streams as usize);
        for _ in 0..num_pack_streams {
            if cancel.load(Ordering::Relaxed) {
                return Err(PreflightRefusal::Cancelled);
            }
            pack_sizes.push(cursor.read_varint()?);
        }
        nid = cursor.read_u8()?;
    }
    if nid == K_CRC {
        let bits = read_all_or_bits(cursor, num_pack_streams as usize, cancel)?;
        for set in bits {
            if set {
                cursor.skip(4)?;
            }
        }
        nid = cursor.read_u8()?;
    }
    if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "bad pack-info terminator",
        });
    }
    Ok(())
}

fn parse_unpack_info(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    folders: &mut Vec<FolderInfo>,
    cancel: &AtomicBool,
) -> Result<(), PreflightRefusal> {
    let nid = cursor.read_u8()?;
    if nid != K_FOLDER {
        return Err(PreflightRefusal::Malformed {
            detail: "expected kFolder",
        });
    }
    let num_folders = cursor.read_varint()?;
    if num_folders > limits.max_members as u64 {
        return Err(PreflightRefusal::TooManyFolders {
            count: num_folders,
            limit: limits.max_members,
        });
    }
    let external = cursor.read_u8()?;
    if external != 0 {
        return Err(PreflightRefusal::Malformed {
            detail: "external folders are unsupported",
        });
    }
    folders.reserve(num_folders as usize);
    for _ in 0..num_folders {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        folders.push(parse_folder(cursor, limits, cancel)?);
    }
    let nid = cursor.read_u8()?;
    if nid != K_CODERS_UNPACK_SIZE {
        return Err(PreflightRefusal::Malformed {
            detail: "expected kCodersUnpackSize",
        });
    }
    for folder in folders.iter_mut() {
        folder.unpack_sizes.reserve(folder.total_output_streams);
        for _ in 0..folder.total_output_streams {
            if cancel.load(Ordering::Relaxed) {
                return Err(PreflightRefusal::Cancelled);
            }
            folder.unpack_sizes.push(cursor.read_varint()?);
        }
    }
    let mut nid = cursor.read_u8()?;
    if nid == K_CRC {
        let bits = read_all_or_bits(cursor, folders.len(), cancel)?;
        for (index, set) in bits.iter().enumerate() {
            if *set {
                folders[index].has_crc = true;
                cursor.skip(4)?;
            }
        }
        nid = cursor.read_u8()?;
    }
    if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "bad unpack-info terminator",
        });
    }
    Ok(())
}

fn parse_folder(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    cancel: &AtomicBool,
) -> Result<FolderInfo, PreflightRefusal> {
    let num_coders = cursor.read_varint()?;
    if num_coders > limits.max_coders_per_folder as u64 {
        return Err(PreflightRefusal::CoderChainTooLong {
            count: num_coders as usize,
            limit: limits.max_coders_per_folder,
        });
    }
    let mut coders = Vec::with_capacity(num_coders as usize);
    let mut total_in_streams: u64 = 0;
    let mut total_out_streams: u64 = 0;
    for _ in 0..num_coders {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        let bits = cursor.read_u8()?;
        if bits & 0x80 != 0 {
            return Err(PreflightRefusal::Malformed {
                detail: "alternative coder methods are unsupported",
            });
        }
        let id_size = (bits & 0x0f) as u64;
        let id = cursor.read_exact(id_size)?;
        let (num_in, num_out) = if bits & 0x10 == 0 {
            (1_u64, 1_u64)
        } else {
            (cursor.read_varint()?, cursor.read_varint()?)
        };
        total_in_streams = total_in_streams
            .checked_add(num_in)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        total_out_streams = total_out_streams
            .checked_add(num_out)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        if total_in_streams > limits.max_members as u64
            || total_out_streams > limits.max_members as u64
        {
            return Err(PreflightRefusal::Malformed {
                detail: "folder stream counts exceed the structural ceiling",
            });
        }
        let props = if bits & 0x20 != 0 {
            let property_size = cursor.read_varint()?;
            if property_size > MAX_7Z_CODER_PROPERTIES_BYTES as u64 {
                return Err(PreflightRefusal::PropertyBlobTooLarge {
                    size: property_size,
                    limit: MAX_7Z_CODER_PROPERTIES_BYTES,
                });
            }
            cursor.read_exact(property_size)?
        } else {
            Vec::new()
        };
        coders.push(CoderInfo { id, props });
    }

    let num_bind_pairs = total_out_streams
        .checked_sub(1)
        .ok_or(PreflightRefusal::Malformed {
            detail: "folder has no output streams",
        })?;
    let mut bind_pairs = Vec::with_capacity(num_bind_pairs as usize);
    for _ in 0..num_bind_pairs {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        let in_index = cursor.read_varint()?;
        let out_index = cursor.read_varint()?;
        if in_index >= total_in_streams || out_index >= total_out_streams {
            return Err(PreflightRefusal::Malformed {
                detail: "bind pair index out of range",
            });
        }
        bind_pairs.push((in_index, out_index));
    }
    if total_in_streams < num_bind_pairs {
        return Err(PreflightRefusal::Malformed {
            detail: "more bind pairs than input streams",
        });
    }
    let num_packed_streams = total_in_streams - num_bind_pairs;
    let mut packed_streams = Vec::with_capacity(num_packed_streams as usize);
    if num_packed_streams == 1 {
        let index = (0..total_in_streams)
            .find(|&index| !bind_pairs.iter().any(|&(in_index, _)| in_index == index));
        let Some(index) = index else {
            return Err(PreflightRefusal::Malformed {
                detail: "no packed stream index",
            });
        };
        packed_streams.push(index);
    } else {
        for _ in 0..num_packed_streams {
            if cancel.load(Ordering::Relaxed) {
                return Err(PreflightRefusal::Cancelled);
            }
            let index = cursor.read_varint()?;
            if index >= total_in_streams {
                return Err(PreflightRefusal::Malformed {
                    detail: "packed stream index out of range",
                });
            }
            packed_streams.push(index);
        }
    }

    Ok(FolderInfo {
        coders,
        total_output_streams: usize::try_from(total_out_streams)
            .map_err(|_| PreflightRefusal::ArithmeticOverflow)?,
        bind_pairs,
        packed_streams,
        unpack_sizes: Vec::new(),
        num_unpack_sub_streams: 1,
        has_crc: false,
    })
}

fn parse_sub_streams_info(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    folders: &mut [FolderInfo],
    cancel: &AtomicBool,
) -> Result<(), PreflightRefusal> {
    let mut nid = cursor.read_u8()?;
    if nid == K_NUM_UNPACK_STREAM {
        let mut total_sub_streams: usize = 0;
        for folder in folders.iter_mut() {
            if cancel.load(Ordering::Relaxed) {
                return Err(PreflightRefusal::Cancelled);
            }
            let count = cursor.read_varint()?;
            if count > limits.max_members as u64 {
                return Err(PreflightRefusal::TooManyFiles {
                    count,
                    limit: limits.max_members,
                });
            }
            folder.num_unpack_sub_streams = count as usize;
            total_sub_streams = total_sub_streams
                .checked_add(folder.num_unpack_sub_streams)
                .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        }
        if total_sub_streams > limits.max_members {
            return Err(PreflightRefusal::MemberCountExceeded {
                count: total_sub_streams,
                limit: limits.max_members,
            });
        }
        nid = cursor.read_u8()?;
    }
    let saw_size = nid == K_SIZE;
    if saw_size {
        // Per-member sizes: the per-file logical sizes, used to enforce the
        // per-member ceiling before any decoder is constructed.
        for folder in folders.iter() {
            if folder.num_unpack_sub_streams == 0 {
                continue;
            }
            let mut sum: u64 = 0;
            for _ in 0..folder.num_unpack_sub_streams.saturating_sub(1) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(PreflightRefusal::Cancelled);
                }
                let size = cursor.read_varint()?;
                if size > limits.max_member_logical_bytes {
                    return Err(PreflightRefusal::MemberSizeExceeded {
                        size,
                        limit: limits.max_member_logical_bytes,
                    });
                }
                sum = sum
                    .checked_add(size)
                    .ok_or(PreflightRefusal::ArithmeticOverflow)?;
            }
            let folder_unpack = folder.unpack_size();
            if sum > folder_unpack {
                return Err(PreflightRefusal::Malformed {
                    detail: "sub-stream sizes exceed folder unpack size",
                });
            }
            let last = folder_unpack - sum;
            if last > limits.max_member_logical_bytes {
                return Err(PreflightRefusal::MemberSizeExceeded {
                    size: last,
                    limit: limits.max_member_logical_bytes,
                });
            }
        }
        nid = cursor.read_u8()?;
    } else {
        // Without K_SIZE, `sevenz-rust` derives one size per folder and would
        // index past the sizes for a multi-sub-stream (solid) folder — a panic
        // vector. Fail closed.
        if folders
            .iter()
            .any(|folder| folder.num_unpack_sub_streams > 1)
        {
            return Err(PreflightRefusal::Malformed {
                detail: "solid folder without sub-stream sizes",
            });
        }
    }
    if nid == K_CRC {
        let mut num_digests = 0;
        for folder in folders.iter() {
            if folder.num_unpack_sub_streams != 1 || !folder.has_crc {
                num_digests += folder.num_unpack_sub_streams;
            }
        }
        let has_missing = read_all_or_bits(cursor, num_digests, cancel)?;
        for set in has_missing {
            if set {
                cursor.skip(4)?;
            }
        }
        nid = cursor.read_u8()?;
    }
    if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "bad sub-streams terminator",
        });
    }
    Ok(())
}

/// Validates the FilesInfo section far enough that `sevenz-rust` cannot hit
/// its panic-prone paths (zero-sized K_NAME, count/size index mismatches).
///
/// Consumes the property records up to and including the FilesInfo `K_END`.
fn parse_files_info(
    cursor: &mut HeaderCursor<'_>,
    num_files: usize,
    total_stream_files: usize,
    cancel: &AtomicBool,
) -> Result<(), PreflightRefusal> {
    let mut empty_stream: Option<Vec<bool>> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        let property = cursor.read_u8()?;
        if property == K_END {
            break;
        }
        let size = cursor.read_varint()?;
        match property {
            K_EMPTY_STREAM => {
                empty_stream = Some(read_all_or_bits(cursor, num_files, cancel)?);
            }
            K_EMPTY_FILE | K_ANTI => {
                read_all_or_bits(cursor, num_files, cancel)?;
            }
            K_NAME => {
                if size == 0 {
                    return Err(PreflightRefusal::Malformed {
                        detail: "zero-sized K_NAME property",
                    });
                }
                let external = cursor.read_u8()?;
                if external != 0 {
                    return Err(PreflightRefusal::Malformed {
                        detail: "external K_NAME is unsupported",
                    });
                }
                let names_bytes = size - 1;
                if names_bytes % 2 != 0 {
                    return Err(PreflightRefusal::Malformed {
                        detail: "K_NAME names are not UTF-16 aligned",
                    });
                }
                parse_file_names(cursor, names_bytes, num_files)?;
            }
            K_START_POS => {
                return Err(PreflightRefusal::Malformed {
                    detail: "kStartPos is unsupported",
                });
            }
            _ => {
                // Times, attributes, comments, and unknown properties are
                // skipped by their declared size; the size is bounded by the
                // cursor (a declared size past the end is Truncated).
                cursor.skip(size)?;
            }
        }
    }

    let stream_files = match &empty_stream {
        Some(bits) => num_files - bits.iter().filter(|&&bit| bit).count(),
        None => num_files,
    };
    if stream_files != total_stream_files {
        return Err(PreflightRefusal::Malformed {
            detail: "file count inconsistent with stream sizes",
        });
    }
    Ok(())
}

/// Validates that `names_bytes` encodes exactly `num_files` null-terminated
/// UTF-16 names with no dangling or trailing bytes.
fn parse_file_names(
    cursor: &mut HeaderCursor<'_>,
    names_bytes: u64,
    num_files: usize,
) -> Result<(), PreflightRefusal> {
    let bytes = cursor.read_exact(names_bytes)?;
    if num_files == 0 {
        // An empty archive may carry a zero-length name block only.
        return if bytes.is_empty() {
            Ok(())
        } else {
            Err(PreflightRefusal::Malformed {
                detail: "K_NAME names for zero files",
            })
        };
    }
    if bytes.len() < 2 || !bytes.ends_with(&[0, 0]) {
        return Err(PreflightRefusal::Malformed {
            detail: "K_NAME names not null-terminated",
        });
    }
    let mut names_seen = 0usize;
    for chunk in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            names_seen += 1;
        }
    }
    if names_seen != num_files {
        return Err(PreflightRefusal::Malformed {
            detail: "K_NAME names count mismatch",
        });
    }
    Ok(())
}

fn enforce_and_summarize(
    pack_sizes: &[u64],
    folders: &[FolderInfo],
    limits: &ArchiveLimits,
) -> Result<SevenZPreflightInfo, PreflightRefusal> {
    let mut member_count: usize = 0;
    let mut total_logical_bytes: u64 = 0;
    let mut max_folder_unpack: u64 = 0;
    let mut first_pack_stream: usize = 0;

    for folder in folders {
        member_count = member_count
            .checked_add(folder.num_unpack_sub_streams)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        if member_count > limits.max_members {
            return Err(PreflightRefusal::MemberCountExceeded {
                count: member_count,
                limit: limits.max_members,
            });
        }

        let unpack = folder.unpack_size();
        total_logical_bytes = total_logical_bytes
            .checked_add(unpack)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        if total_logical_bytes > limits.max_archive_logical_bytes {
            return Err(PreflightRefusal::LogicalBytesExceeded {
                bytes: total_logical_bytes,
                limit: limits.max_archive_logical_bytes,
            });
        }
        max_folder_unpack = max_folder_unpack.max(unpack);

        // Solid (multi-member) block budget.
        if folder.num_unpack_sub_streams > 1 && unpack > limits.max_solid_decode_bytes {
            return Err(PreflightRefusal::SolidDecodeBudgetExceeded {
                bytes: unpack,
                limit: limits.max_solid_decode_bytes,
            });
        }

        // Compression ratio, without lossy integer division, with checked
        // packed-size summation and explicit zero-compressed-size handling.
        let pack_end = first_pack_stream
            .checked_add(folder.packed_streams.len())
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        if pack_end > pack_sizes.len() {
            return Err(PreflightRefusal::Malformed {
                detail: "folder pack streams out of range",
            });
        }
        let mut pack: u64 = 0;
        for &size in &pack_sizes[first_pack_stream..pack_end] {
            pack = pack
                .checked_add(size)
                .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        }
        if unpack > 0 && (pack == 0 || ratio_exceeded(unpack, pack, limits.max_compression_ratio)) {
            return Err(PreflightRefusal::CompressionRatioExceeded {
                unpack,
                pack,
                ratio: limits.max_compression_ratio,
            });
        }

        // Dictionary declarations and the aggregate decoder memory of the
        // coder chain: sevenz-rust builds a nested decoder stack, so every
        // LZMA/LZMA2 dictionary in a folder is allocated simultaneously.
        let mut aggregate_decoder_memory: u64 = 0;
        for coder in &folder.coders {
            let dictionary = if coder.id.as_slice() == ID_LZMA {
                lzma_dictionary_size(&coder.props)
                    .map_err(|detail| PreflightRefusal::Malformed { detail })?
            } else if coder.id.as_slice() == ID_LZMA2 {
                lzma2_dictionary_size(&coder.props)
                    .map_err(|detail| PreflightRefusal::Malformed { detail })?
            } else {
                continue;
            };
            if u64::from(dictionary) > limits.max_dictionary_bytes {
                return Err(PreflightRefusal::DictionaryTooLarge {
                    dictionary: u64::from(dictionary),
                    limit: limits.max_dictionary_bytes,
                });
            }
            aggregate_decoder_memory = aggregate_decoder_memory
                .checked_add(u64::from(dictionary))
                .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        }
        if aggregate_decoder_memory > limits.max_aggregate_decoder_memory_bytes {
            return Err(PreflightRefusal::AggregateDecoderMemoryExceeded {
                bytes: aggregate_decoder_memory,
                limit: limits.max_aggregate_decoder_memory_bytes,
            });
        }

        first_pack_stream = pack_end;
    }

    // Every declared pack stream must be consumed by a folder; leftover or
    // unreferenced streams indicate an inconsistent (malformed) archive.
    if folders.is_empty() && !pack_sizes.is_empty() {
        return Err(PreflightRefusal::Malformed {
            detail: "pack streams without folders",
        });
    }
    if first_pack_stream != pack_sizes.len() {
        return Err(PreflightRefusal::Malformed {
            detail: "pack streams not fully consumed",
        });
    }

    Ok(SevenZPreflightInfo {
        member_count,
        total_logical_bytes,
        max_folder_unpack,
    })
}

/// `unpack > pack * ratio`, computed without lossy integer division.
fn ratio_exceeded(unpack: u64, pack: u64, ratio: u64) -> bool {
    match pack.checked_mul(ratio) {
        Some(limit) => unpack > limit,
        None => true,
    }
}

/// LZMA dictionary size from coder properties (`props[1..5]`, LE u32).
fn lzma_dictionary_size(properties: &[u8]) -> Result<u32, &'static str> {
    let bytes = properties.get(1..5).ok_or("LZMA properties too short")?;
    let mut buffer = [0_u8; 4];
    buffer.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(buffer))
}

/// LZMA2 dictionary size from coder properties (dict bits → bytes).
fn lzma2_dictionary_size(properties: &[u8]) -> Result<u32, &'static str> {
    let bits = 0xff & u32::from(*properties.first().ok_or("LZMA2 properties too short")?);
    if (bits & (!0x3f)) != 0 {
        return Err("Unsupported LZMA2 property bits");
    }
    if bits > 40 {
        return Err("LZMA2 dictionary larger than 4 GiB");
    }
    if bits == 40 {
        return Ok(0xFFFF_FFFF);
    }
    Ok((2 | (bits & 0x1)) << (bits / 2 + 11))
}

/// A bounded cursor over the already-read next-header buffer.
struct HeaderCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> HeaderCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn is_exhausted(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn read_u8(&mut self) -> Result<u8, PreflightRefusal> {
        let byte = self
            .bytes
            .get(self.position)
            .copied()
            .ok_or(PreflightRefusal::Truncated)?;
        self.position += 1;
        Ok(byte)
    }

    /// Reads the 7z variable-length integer (mirrors sevenz-rust's encoding).
    fn read_varint(&mut self) -> Result<u64, PreflightRefusal> {
        let first = self.read_u8()? as u64;
        let mut mask = 0x80_u64;
        let mut value: u64 = 0;
        for index in 0..8 {
            if (first & mask) == 0 {
                return Ok(value | ((first & (mask - 1)) << (8 * index)));
            }
            let byte = self.read_u8()? as u64;
            value |= byte << (8 * index);
            mask >>= 1;
        }
        Ok(value)
    }

    /// Reads exactly `length` bytes (checked `u64`→`usize` conversion).
    fn read_exact(&mut self, length: u64) -> Result<Vec<u8>, PreflightRefusal> {
        let length = usize::try_from(length).map_err(|_| PreflightRefusal::ArithmeticOverflow)?;
        let end = self
            .position
            .checked_add(length)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(PreflightRefusal::Truncated)?;
        self.position = end;
        Ok(slice.to_vec())
    }

    /// Advances the cursor by `length` bytes without allocating.
    fn skip(&mut self, length: u64) -> Result<(), PreflightRefusal> {
        let length = usize::try_from(length).map_err(|_| PreflightRefusal::ArithmeticOverflow)?;
        let end = self
            .position
            .checked_add(length)
            .ok_or(PreflightRefusal::ArithmeticOverflow)?;
        if end > self.bytes.len() {
            return Err(PreflightRefusal::Truncated);
        }
        self.position = end;
        Ok(())
    }
}

/// `read_all_or_bits`: a leading "all" flag byte, else one bit per 8-bit
/// mask, MSB-first.
fn read_all_or_bits(
    cursor: &mut HeaderCursor<'_>,
    size: usize,
    cancel: &AtomicBool,
) -> Result<Vec<bool>, PreflightRefusal> {
    let all = cursor.read_u8()?;
    if all != 0 {
        return Ok(vec![true; size]);
    }
    let mut out = Vec::with_capacity(size);
    let mut mask = 0_u32;
    let mut cache = 0_u32;
    for _ in 0..size {
        if cancel.load(Ordering::Relaxed) {
            return Err(PreflightRefusal::Cancelled);
        }
        if mask == 0 {
            mask = 0x80;
            cache = cursor.read_u8()? as u32;
        }
        out.push((cache & mask) != 0);
        mask >>= 1;
    }
    Ok(out)
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, length: usize) -> std::io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0_u8; length];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// A bounded, cancellable chunked read failure.
enum ReadFailure {
    Cancelled,
    Io(String),
}

/// Reads `length` bytes into `buffer` in bounded chunks, checking
/// cancellation per chunk and surfacing it distinctly from I/O errors.
fn read_at_chunked<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    buffer: &mut [u8],
    cancel: &AtomicBool,
) -> Result<(), ReadFailure> {
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|error| ReadFailure::Io(error.to_string()))?;
    let mut position = 0;
    while position < buffer.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadFailure::Cancelled);
        }
        let chunk = ARCHIVE_HASH_CHUNK_BYTES.min(buffer.len() - position);
        reader
            .read_exact(&mut buffer[position..position + chunk])
            .map_err(|error| ReadFailure::Io(error.to_string()))?;
        position += chunk;
    }
    Ok(())
}
