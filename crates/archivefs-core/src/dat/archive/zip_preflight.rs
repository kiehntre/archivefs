//! Bounded ZIP/ZIP64 metadata probe run before `zip::ZipArchive::new`.
//!
//! The `zip` crate reserves its central-directory vector from the entry count,
//! so untrusted counts and offsets must be checked before handing it a file.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use super::limits::ArchiveLimits;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const EOCD_MIN_SIZE: u64 = 22;
const EOCD_SEARCH_BYTES: u64 = EOCD_MIN_SIZE + u16::MAX as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipPreflightError {
    Cancelled,
    Refused(&'static str),
    Corrupt(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipPreflightEntry {
    pub index: usize,
    pub flags: u16,
    pub method: u16,
    pub compressed_size: u64,
    pub logical_size: u64,
    pub crc32: u32,
    pub local_header_offset: u64,
    pub data_start: u64,
    pub data_end: u64,
    pub name_raw: Vec<u8>,
    pub is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipPreflightInfo {
    pub entry_count: usize,
    pub central_directory_offset: u64,
    pub central_directory_size: u64,
    pub entries: Vec<ZipPreflightEntry>,
}

pub fn preflight_zip<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    limits: &ArchiveLimits,
    cancel: &AtomicBool,
) -> Result<ZipPreflightInfo, ZipPreflightError> {
    check_cancel(cancel)?;
    if file_len < EOCD_MIN_SIZE {
        return Err(corrupt("file is too short for a ZIP end record"));
    }

    let tail_start = file_len.saturating_sub(EOCD_SEARCH_BYTES);
    let tail_len = usize::try_from(file_len - tail_start)
        .map_err(|_| ZipPreflightError::Refused("ZIP tail size"))?;
    let mut tail = vec![0_u8; tail_len];
    read_exact_at(reader, tail_start, &mut tail)?;
    check_cancel(cancel)?;

    let eocd_in_tail = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&offset| {
            le_u32(&tail[offset..offset + 4]) == EOCD_SIGNATURE
                && offset.checked_add(22).and_then(|end| {
                    end.checked_add(le_u16(&tail[offset + 20..offset + 22]) as usize)
                }) == Some(tail.len())
        })
        .ok_or_else(|| corrupt("ZIP end record is missing or truncated"))?;
    let eocd_offset = tail_start
        .checked_add(eocd_in_tail as u64)
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    let eocd = &tail[eocd_in_tail..eocd_in_tail + 22];

    let disk = le_u16(&eocd[4..6]);
    let central_disk = le_u16(&eocd[6..8]);
    if disk != 0 || central_disk != 0 {
        return Err(ZipPreflightError::Refused("multi-disk ZIP"));
    }

    let entries_on_disk = le_u16(&eocd[8..10]);
    let entries_total = le_u16(&eocd[10..12]);
    let central_size_32 = le_u32(&eocd[12..16]);
    let central_offset_32 = le_u32(&eocd[16..20]);
    let needs_zip64 = entries_on_disk == u16::MAX
        || entries_total == u16::MAX
        || central_size_32 == u32::MAX
        || central_offset_32 == u32::MAX;

    let (entry_count_u64, central_size, central_offset, expected_central_end) = if needs_zip64 {
        parse_zip64(reader, eocd_offset)?
    } else {
        if entries_on_disk != entries_total {
            return Err(corrupt("inconsistent ZIP entry counts"));
        }
        (
            u64::from(entries_total),
            u64::from(central_size_32),
            u64::from(central_offset_32),
            eocd_offset,
        )
    };

    let entry_count =
        usize::try_from(entry_count_u64).map_err(|_| ZipPreflightError::Refused("member count"))?;
    if entry_count > limits.max_members {
        return Err(ZipPreflightError::Refused("member count"));
    }
    if central_size > limits.max_zip_central_directory_bytes as u64 {
        return Err(ZipPreflightError::Refused("central directory size"));
    }
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    if central_end > file_len || central_end != expected_central_end {
        return Err(corrupt("central directory range is outside the ZIP layout"));
    }

    let mut entries = Vec::with_capacity(entry_count);
    let mut cursor = central_offset;
    for index in 0..entry_count {
        check_cancel(cancel)?;
        let mut fixed = [0_u8; 46];
        read_exact_at(reader, cursor, &mut fixed)?;
        if le_u32(&fixed[0..4]) != CENTRAL_SIGNATURE {
            return Err(corrupt("invalid central-directory entry signature"));
        }
        let flags = le_u16(&fixed[8..10]);
        let method = le_u16(&fixed[10..12]);
        let compressed_32 = le_u32(&fixed[20..24]);
        let logical_32 = le_u32(&fixed[24..28]);
        let crc32 = le_u32(&fixed[16..20]);
        let name_len = usize::from(le_u16(&fixed[28..30]));
        let extra_len = usize::from(le_u16(&fixed[30..32]));
        let comment_len = usize::from(le_u16(&fixed[32..34]));
        let disk_start_16 = le_u16(&fixed[34..36]);
        let local_offset_32 = le_u32(&fixed[42..46]);

        cursor = cursor
            .checked_add(46)
            .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
        let variable_len = name_len
            .checked_add(extra_len)
            .and_then(|v| v.checked_add(comment_len))
            .ok_or(ZipPreflightError::Refused("ZIP metadata arithmetic"))?;
        let variable_end = cursor
            .checked_add(variable_len as u64)
            .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
        if variable_end > central_end {
            return Err(corrupt(
                "central-directory entry exceeds its declared range",
            ));
        }
        let mut name_raw = vec![0_u8; name_len];
        read_exact_at(reader, cursor, &mut name_raw)?;
        let mut extra = vec![0_u8; extra_len];
        read_exact_at(reader, cursor + name_len as u64, &mut extra)?;
        cursor = variable_end;

        let (logical_size, compressed_size, local_header_offset, disk_start) =
            resolve_zip64_fields(
                logical_32,
                compressed_32,
                local_offset_32,
                disk_start_16,
                &extra,
            )?;
        if disk_start != 0 {
            return Err(ZipPreflightError::Refused("multi-disk ZIP"));
        }
        let central_entry = CentralEntryMetadata {
            flags,
            method,
            compressed_size,
            logical_size,
            crc32,
            name: &name_raw,
        };
        let (data_start, data_end) =
            validate_local_entry(reader, local_header_offset, central_offset, &central_entry)?;
        entries.push(ZipPreflightEntry {
            index,
            flags,
            method,
            compressed_size,
            logical_size,
            crc32,
            local_header_offset,
            data_start,
            data_end,
            is_directory: name_raw.ends_with(b"/"),
            name_raw,
        });
    }
    if cursor != central_end {
        return Err(corrupt("central-directory size does not match its entries"));
    }

    let mut ranges: Vec<_> = entries
        .iter()
        .map(|entry| (entry.local_header_offset, entry.data_end))
        .collect();
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(corrupt("ZIP local entries or packed data overlap"));
        }
    }

    Ok(ZipPreflightInfo {
        entry_count,
        central_directory_offset: central_offset,
        central_directory_size: central_size,
        entries,
    })
}

fn parse_zip64<R: Read + Seek>(
    reader: &mut R,
    eocd_offset: u64,
) -> Result<(u64, u64, u64, u64), ZipPreflightError> {
    let locator_offset = eocd_offset
        .checked_sub(20)
        .ok_or_else(|| corrupt("ZIP64 locator is missing"))?;
    let mut locator = [0_u8; 20];
    read_exact_at(reader, locator_offset, &mut locator)?;
    if le_u32(&locator[0..4]) != ZIP64_LOCATOR_SIGNATURE
        || le_u32(&locator[4..8]) != 0
        || le_u32(&locator[16..20]) != 1
    {
        return Err(corrupt("invalid ZIP64 locator"));
    }
    let record_offset = le_u64(&locator[8..16]);
    let mut record = [0_u8; 56];
    read_exact_at(reader, record_offset, &mut record)?;
    if le_u32(&record[0..4]) != ZIP64_EOCD_SIGNATURE || le_u64(&record[4..12]) < 44 {
        return Err(corrupt("invalid ZIP64 end record"));
    }
    if le_u32(&record[16..20]) != 0 || le_u32(&record[20..24]) != 0 {
        return Err(ZipPreflightError::Refused("multi-disk ZIP"));
    }
    let entries_on_disk = le_u64(&record[24..32]);
    let entries_total = le_u64(&record[32..40]);
    if entries_on_disk != entries_total {
        return Err(corrupt("inconsistent ZIP64 entry counts"));
    }
    let record_size = le_u64(&record[4..12]);
    let record_end = record_offset
        .checked_add(12)
        .and_then(|v| v.checked_add(record_size))
        .ok_or(ZipPreflightError::Refused("ZIP64 offset arithmetic"))?;
    if record_end != locator_offset {
        return Err(corrupt("ZIP64 end-record range is invalid"));
    }
    Ok((
        entries_total,
        le_u64(&record[40..48]),
        le_u64(&record[48..56]),
        record_offset,
    ))
}

fn resolve_zip64_fields(
    logical_32: u32,
    compressed_32: u32,
    local_offset_32: u32,
    disk_start_16: u16,
    extra: &[u8],
) -> Result<(u64, u64, u64, u32), ZipPreflightError> {
    let needs = logical_32 == u32::MAX
        || compressed_32 == u32::MAX
        || local_offset_32 == u32::MAX
        || disk_start_16 == u16::MAX;
    let mut zip64 = None;
    let mut cursor = 0_usize;
    while cursor < extra.len() {
        if extra.len() - cursor < 4 {
            return Err(corrupt("truncated ZIP extra field"));
        }
        let id = le_u16(&extra[cursor..cursor + 2]);
        let size = usize::from(le_u16(&extra[cursor + 2..cursor + 4]));
        cursor = cursor
            .checked_add(4)
            .ok_or(ZipPreflightError::Refused("ZIP metadata arithmetic"))?;
        let end = cursor
            .checked_add(size)
            .ok_or(ZipPreflightError::Refused("ZIP metadata arithmetic"))?;
        let data = extra
            .get(cursor..end)
            .ok_or_else(|| corrupt("truncated ZIP extra field"))?;
        if id == 0x0001 {
            zip64 = Some(data);
        }
        cursor = end;
    }
    if needs && zip64.is_none() {
        return Err(corrupt("ZIP64 entry metadata is missing"));
    }
    let mut data = zip64.unwrap_or_default();
    let logical = take_zip64(&mut data, logical_32 == u32::MAX)?.unwrap_or(u64::from(logical_32));
    let compressed =
        take_zip64(&mut data, compressed_32 == u32::MAX)?.unwrap_or(u64::from(compressed_32));
    let offset =
        take_zip64(&mut data, local_offset_32 == u32::MAX)?.unwrap_or(u64::from(local_offset_32));
    let disk = if disk_start_16 == u16::MAX {
        let bytes = data
            .get(..4)
            .ok_or_else(|| corrupt("truncated ZIP64 disk field"))?;
        le_u32(bytes)
    } else {
        u32::from(disk_start_16)
    };
    Ok((logical, compressed, offset, disk))
}

fn take_zip64(data: &mut &[u8], required: bool) -> Result<Option<u64>, ZipPreflightError> {
    if !required {
        return Ok(None);
    }
    let bytes = data
        .get(..8)
        .ok_or_else(|| corrupt("truncated ZIP64 entry field"))?;
    *data = &data[8..];
    Ok(Some(le_u64(bytes)))
}

struct CentralEntryMetadata<'a> {
    flags: u16,
    method: u16,
    compressed_size: u64,
    logical_size: u64,
    crc32: u32,
    name: &'a [u8],
}

fn validate_local_entry<R: Read + Seek>(
    reader: &mut R,
    local_offset: u64,
    central_offset: u64,
    central: &CentralEntryMetadata<'_>,
) -> Result<(u64, u64), ZipPreflightError> {
    let mut local = [0_u8; 30];
    read_exact_at(reader, local_offset, &mut local)?;
    if le_u32(&local[0..4]) != LOCAL_SIGNATURE {
        return Err(corrupt("invalid ZIP local-entry signature"));
    }
    if le_u16(&local[6..8]) != central.flags || le_u16(&local[8..10]) != central.method {
        return Err(corrupt("local and central ZIP metadata disagree"));
    }
    let local_name_len = usize::from(le_u16(&local[26..28]));
    if local_name_len != central.name.len() {
        return Err(corrupt("local and central ZIP names disagree"));
    }
    let mut local_name = vec![0_u8; local_name_len];
    let local_name_offset = local_offset
        .checked_add(30)
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    read_exact_at(reader, local_name_offset, &mut local_name)?;
    if local_name != central.name {
        return Err(corrupt("local and central ZIP names disagree"));
    }
    let local_extra_len = usize::from(le_u16(&local[28..30]));
    let local_extra_offset = local_name_offset
        .checked_add(local_name_len as u64)
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    let mut local_extra = vec![0_u8; local_extra_len];
    read_exact_at(reader, local_extra_offset, &mut local_extra)?;
    if central.flags & (1 << 3) == 0 {
        let (local_logical, local_compressed, _, _) = resolve_zip64_fields(
            le_u32(&local[22..26]),
            le_u32(&local[18..22]),
            0,
            0,
            &local_extra,
        )?;
        if le_u32(&local[14..18]) != central.crc32
            || local_logical != central.logical_size
            || local_compressed != central.compressed_size
        {
            return Err(corrupt("local and central ZIP sizes or CRC disagree"));
        }
    }
    let variable = u64::try_from(local_name_len)
        .map_err(|_| ZipPreflightError::Refused("ZIP offset arithmetic"))?
        .checked_add(local_extra_len as u64)
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    let data_start = local_offset
        .checked_add(30)
        .and_then(|v| v.checked_add(variable))
        .ok_or(ZipPreflightError::Refused("ZIP offset arithmetic"))?;
    let data_end = data_start
        .checked_add(central.compressed_size)
        .ok_or(ZipPreflightError::Refused("ZIP packed-size arithmetic"))?;
    if local_offset >= central_offset || data_start > central_offset || data_end > central_offset {
        return Err(corrupt("packed ZIP data is outside the data region"));
    }
    Ok((data_start, data_end))
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), ZipPreflightError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ZipPreflightError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_exact_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    buffer: &mut [u8],
) -> Result<(), ZipPreflightError> {
    reader
        .seek(SeekFrom::Start(offset))
        .and_then(|_| reader.read_exact(buffer))
        .map_err(|error| corrupt(&format!("truncated ZIP metadata: {error}")))
}

fn corrupt(detail: &str) -> ZipPreflightError {
    ZipPreflightError::Corrupt(detail.to_string())
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}
