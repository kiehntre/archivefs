//! Pre-decoder 7z header probe (POST-ALPHA-1.1, experimental).
//!
//! A minimal, bounded parser over just enough of the 7z header to enforce
//! hard resource limits **before** `sevenz-rust` is ever constructed. It is
//! deliberately NOT a complete 7z decoder: it extracts only the information
//! needed for resource accounting (next-header size, encoded-header presence,
//! pack-stream/file/folder/coder counts, LZMA/LZMA2 dictionary declarations,
//! packed/unpacked sizes, per-folder sub-stream counts) and refuses hostile
//! declarations.
//!
//! Why this exists: `SevenZReader::new` copies `next_header_size` bytes into a
//! buffer it allocates from untrusted metadata, builds file/folder/coder
//! vectors sized by attacker-controlled counts, and constructs LZMA/LZMA2
//! decoders whose dictionaries approach 4 GiB at decode time. Every one of
//! those allocations must be bounded by EmuWiz's configured limits *before*
//! the upstream crate sees the archive.
//!
//! # Bounds before allocation
//!
//! Every count/size read from the header is validated against a configured
//! ceiling before any `Vec` of that size is allocated, and all arithmetic on
//! untrusted lengths is checked. Reads past the bounded header buffer are a
//! named malformed-archive refusal, never a panic.
//!
//! # Encoded headers
//!
//! A 7z archive may store its real header in a packed/encrypted "encoded
//! header" block whose decompressed size is not known until it is decoded.
//! Because that expansion cannot be bounded without decoding, the probe
//! refuses every archive with an encoded header. This is a fail-closed
//! experimental restriction (most archives store a plain header); see the
//! module doc of `sevenz.rs`.

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
    TooManyCoders {
        count: u64,
        limit: usize,
    },
    TooManyPackStreams {
        count: u64,
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
/// (bounded) next header, checks cancellation during the header read and
/// parse, and returns the extracted resource facts.
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
    let mut start_crc = Crc32::new();
    start_crc.update(&file_header[12..32]);
    let start_crc = start_crc.finish();
    if start_header_crc != 0 && start_crc != start_header_crc {
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

    // Read the next header in bounded chunks, checking cancellation. The size
    // was validated against the ceiling before this allocation.
    let mut header = vec![0_u8; next_header_size as usize];
    read_at_chunked(reader, next_header_pos, &mut header, cancel)
        .map_err(|detail| PreflightRefusal::Io(detail.to_string()))?;

    let mut cursor = HeaderCursor::new(&header);
    parse_header(&mut cursor, limits, cancel)
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
        parse_archive_properties(cursor)?;
        nid = cursor.read_u8()?;
    }
    if nid == K_ADDITIONAL_STREAMS_INFO {
        return Err(PreflightRefusal::Malformed {
            detail: "additional streams are unsupported",
        });
    }

    let mut pack_sizes: Vec<u64> = Vec::new();
    let mut folders: Vec<FolderInfo> = Vec::new();
    if nid == K_MAIN_STREAMS_INFO {
        nid = cursor.read_u8()?;
        if nid == K_PACK_INFO {
            parse_pack_info(cursor, limits, &mut pack_sizes)?;
            nid = cursor.read_u8()?;
        }
        if nid == K_UNPACK_INFO {
            parse_unpack_info(cursor, limits, &mut folders, cancel)?;
            nid = cursor.read_u8()?;
        }
        if nid == K_SUB_STREAMS_INFO {
            parse_sub_streams_info(cursor, limits, &mut folders)?;
            nid = cursor.read_u8()?;
        }
        if nid != K_END {
            return Err(PreflightRefusal::Malformed {
                detail: "bad streams-info terminator",
            });
        }
        nid = cursor.read_u8()?;
    }
    if nid == K_FILES_INFO {
        // Only the declared file count matters for the allocation ceiling:
        // `sevenz-rust` sizes its files vector by this value.
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
    } else if nid != K_END {
        return Err(PreflightRefusal::Malformed {
            detail: "missing files info",
        });
    }

    enforce_and_summarize(&pack_sizes, &folders, limits)
}

fn parse_archive_properties(cursor: &mut HeaderCursor<'_>) -> Result<(), PreflightRefusal> {
    let mut nid = cursor.read_u8()?;
    while nid != K_END {
        let size = cursor.read_varint()?;
        cursor.skip(size as usize)?;
        nid = cursor.read_u8()?;
    }
    Ok(())
}

fn parse_pack_info(
    cursor: &mut HeaderCursor<'_>,
    limits: &ArchiveLimits,
    pack_sizes: &mut Vec<u64>,
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
            pack_sizes.push(cursor.read_varint()?);
        }
        nid = cursor.read_u8()?;
    }
    if nid == K_CRC {
        let bits = read_all_or_bits(cursor, num_pack_streams as usize)?;
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
        folders.push(parse_folder(cursor, limits)?);
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
            folder.unpack_sizes.push(cursor.read_varint()?);
        }
    }
    let mut nid = cursor.read_u8()?;
    if nid == K_CRC {
        let bits = read_all_or_bits(cursor, folders.len())?;
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
) -> Result<FolderInfo, PreflightRefusal> {
    let num_coders = cursor.read_varint()?;
    if num_coders > limits.max_members as u64 {
        return Err(PreflightRefusal::TooManyCoders {
            count: num_coders,
            limit: limits.max_members,
        });
    }
    let mut coders = Vec::with_capacity(num_coders as usize);
    let mut total_in_streams: u64 = 0;
    let mut total_out_streams: u64 = 0;
    for _ in 0..num_coders {
        let bits = cursor.read_u8()?;
        if bits & 0x80 != 0 {
            return Err(PreflightRefusal::Malformed {
                detail: "alternative coder methods are unsupported",
            });
        }
        let id_size = (bits & 0x0f) as usize;
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
            return Err(PreflightRefusal::TooManyCoders {
                count: total_out_streams,
                limit: limits.max_members,
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
            cursor.read_exact(property_size as usize)?
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
        let in_index = cursor.read_varint()?;
        let out_index = cursor.read_varint()?;
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
            packed_streams.push(cursor.read_varint()?);
        }
    }

    Ok(FolderInfo {
        coders,
        total_output_streams: total_out_streams as usize,
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
) -> Result<(), PreflightRefusal> {
    let mut nid = cursor.read_u8()?;
    if nid == K_NUM_UNPACK_STREAM {
        let mut total_sub_streams: usize = 0;
        for folder in folders.iter_mut() {
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
    if nid == K_SIZE {
        // Per-file sizes are not needed for the limits: the total logical
        // bytes equal the sum of folder unpack sizes. Skip the declared
        // sub-stream sizes (one fewer per folder; the last is derived).
        for folder in folders.iter() {
            for _ in 0..folder.num_unpack_sub_streams.saturating_sub(1) {
                cursor.read_varint()?;
            }
        }
        nid = cursor.read_u8()?;
    }
    if nid == K_CRC {
        let mut num_digests = 0;
        for folder in folders.iter() {
            if folder.num_unpack_sub_streams != 1 || !folder.has_crc {
                num_digests += folder.num_unpack_sub_streams;
            }
        }
        let has_missing = read_all_or_bits(cursor, num_digests)?;
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

        // Dictionary declarations, before any decoder construction.
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
        }

        first_pack_stream = pack_end;
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

    fn read_exact(&mut self, length: usize) -> Result<Vec<u8>, PreflightRefusal> {
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

    fn skip(&mut self, length: usize) -> Result<(), PreflightRefusal> {
        self.read_exact(length).map(|_| ())
    }
}

/// `read_all_or_bits`: a leading "all" flag byte, else one bit per 8-bit
/// mask, MSB-first.
fn read_all_or_bits(
    cursor: &mut HeaderCursor<'_>,
    size: usize,
) -> Result<Vec<bool>, PreflightRefusal> {
    let all = cursor.read_u8()?;
    if all != 0 {
        return Ok(vec![true; size]);
    }
    let mut out = Vec::with_capacity(size);
    let mut mask = 0_u32;
    let mut cache = 0_u32;
    for _ in 0..size {
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

/// Reads `length` bytes into `buffer` in bounded chunks, checking
/// cancellation per chunk.
fn read_at_chunked<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    buffer: &mut [u8],
    cancel: &AtomicBool,
) -> std::io::Result<()> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut position = 0;
    while position < buffer.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let chunk = ARCHIVE_HASH_CHUNK_BYTES.min(buffer.len() - position);
        reader.read_exact(&mut buffer[position..position + chunk])?;
        position += chunk;
    }
    Ok(())
}
