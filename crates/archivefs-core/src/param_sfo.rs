//! Pure, read-only PARAM.SFO ("PSF"/System File Object) parser, shared by
//! [`crate::psp_boot_evidence`] and [`crate::ps3_boot_evidence`] - the
//! same file format, keyed differently per platform (`DISC_ID` on PSP,
//! `TITLE_ID` on PS3), so the parser itself carries no platform
//! assumption.
//!
//! # Format verified, not assumed
//!
//! Cross-checked against two independent community references that agree
//! exactly: the PS3 Developer wiki's PARAM.SFO page (summarized via
//! search, the page itself blocks automated fetches) and
//! `Jasily/py.dataformat.sfo`'s `sfo.py`
//! (`https://github.com/Jasily/py.dataformat.sfo/blob/master/sfo.py`,
//! whose own header comment cites the PS3 Developer wiki as its source).
//!
//! ```text
//! Header (20 bytes):
//! [0..4]   magic            "\0PSF" (bytes 0x00,'P','S','F')
//! [4..8]   version
//! [8..12]  key_table_start   u32 LE, absolute file offset
//! [12..16] data_table_start  u32 LE, absolute file offset
//! [16..20] tables_entries    u32 LE, entry count
//!
//! Index table entry (16 bytes each, immediately after the header):
//! [0..2]  key_offset      u16 LE, relative to key_table_start
//! [2..4]  data_fmt        u16 LE (0x0004 = UTF-8 special/non-terminated,
//!                                 0x0204 = UTF-8 null-terminated,
//!                                 0x0404 = int32)
//! [4..8]  data_len        u32 LE, actual value length in bytes
//! [8..12] data_max_len    u32 LE, reserved/padded allocation length
//! [12..16] data_offset    u32 LE, relative to data_table_start
//! ```
//!
//! Keys are NUL-terminated ASCII strings in the key table; values live at
//! `data_table_start + data_offset`, `data_len` bytes long.
//!
//! # Collision safety
//!
//! PARAM.SFO is a general Sony-ecosystem container format, used across
//! PSP, PS3, and other PlayStation platforms with different conventional
//! keys. Its presence alone proves nothing about which platform - see
//! [`crate::psp_boot_evidence`]/[`crate::ps3_boot_evidence`]'s own
//! collision-safety notes.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const SFO_MAGIC: &[u8; 4] = &[0x00, b'P', b'S', b'F'];
const SFO_HEADER_BYTES: usize = 20;
const SFO_INDEX_ENTRY_BYTES: usize = 16;

/// Generous bound on total file size this parser will attempt - real
/// PARAM.SFO files are a few KiB; this leaves headroom without admitting
/// an unbounded read.
pub const MAX_SFO_BYTES: usize = 1024 * 1024;
/// Bound on the number of index-table entries, independent of the file
/// size bound, so a maliciously small `tables_entries` combined with a
/// huge implied table can never be attempted.
pub const MAX_SFO_ENTRIES: u32 = 4096;
/// Bound on one value's length, applied regardless of `data_max_len`.
pub const MAX_SFO_VALUE_BYTES: u32 = 64 * 1024;

const FORMAT_UTF8_SPECIAL: u16 = 0x0004;
const FORMAT_UTF8: u16 = 0x0204;
const FORMAT_INT32: u16 = 0x0404;

/// One key's value, exactly as its declared format says - never
/// reinterpreted across formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfoValue {
    /// `FORMAT_UTF8`/`FORMAT_UTF8_SPECIAL`, decoded lossily and with any
    /// trailing NUL trimmed.
    Text(String),
    Int32(i32),
    /// A `data_fmt` this parser does not interpret - the raw bytes are
    /// preserved rather than dropped.
    Unknown {
        data_fmt: u16,
        raw: Vec<u8>,
    },
}

/// One key/value pair, in on-disk index-table order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfoEntry {
    pub key: String,
    pub value: SfoValue,
}

/// A parsed PARAM.SFO file: every entry, in file order. Duplicate keys
/// (malformed, but not rejected outright) are all preserved - see
/// [`SfoObservation::get`] for the "first match wins" lookup convention.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SfoObservation {
    pub entries: Vec<SfoEntry>,
}

impl SfoObservation {
    /// The first entry's value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&SfoValue> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| &entry.value)
    }

    /// [`Self::get`], returning the text form only (`None` for a missing
    /// key, an `Int32` value, or an `Unknown` value).
    pub fn get_text(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(SfoValue::Text(text)) => Some(text.as_str()),
            _ => None,
        }
    }
}

/// Parses `bytes` as a PARAM.SFO file.
///
/// Bounded and fails closed at every step: `bytes` longer than
/// [`MAX_SFO_BYTES`], a bad magic, an entry count exceeding
/// [`MAX_SFO_ENTRIES`], or any key/data offset or length that would read
/// outside `bytes` all return `None` - never a partial/guessed result. An
/// unrecognised `data_fmt` is preserved as [`SfoValue::Unknown`] rather
/// than being treated as an error, since the format explicitly allows
/// implementation-specific types.
pub fn parse_param_sfo(bytes: &[u8]) -> Option<SfoObservation> {
    if bytes.len() > MAX_SFO_BYTES || bytes.len() < SFO_HEADER_BYTES {
        return None;
    }
    if &bytes[0..4] != SFO_MAGIC.as_slice() {
        return None;
    }
    let key_table_start = read_u32le(bytes, 8)? as usize;
    let data_table_start = read_u32le(bytes, 12)? as usize;
    let tables_entries = read_u32le(bytes, 16)?;
    if tables_entries > MAX_SFO_ENTRIES {
        return None;
    }

    let index_table_end =
        SFO_HEADER_BYTES.checked_add(tables_entries as usize * SFO_INDEX_ENTRY_BYTES)?;
    let index_table = bytes.get(SFO_HEADER_BYTES..index_table_end)?;

    let mut entries = Vec::with_capacity(tables_entries as usize);
    for raw_entry in index_table.chunks_exact(SFO_INDEX_ENTRY_BYTES) {
        let key_offset = u16::from_le_bytes(raw_entry[0..2].try_into().unwrap()) as usize;
        let data_fmt = u16::from_le_bytes(raw_entry[2..4].try_into().unwrap());
        let data_len = u32::from_le_bytes(raw_entry[4..8].try_into().unwrap());
        // raw_entry[8..12] is data_max_len (the reserved/padded allocation
        // length) - not used for reading the value, since data_len is the
        // actual content length. The real data_offset is at [12..16].
        let data_offset = u32::from_le_bytes(raw_entry[12..16].try_into().unwrap()) as usize;

        if data_len > MAX_SFO_VALUE_BYTES {
            return None;
        }

        let key_start = key_table_start.checked_add(key_offset)?;
        let key_bytes = bytes.get(key_start..)?;
        let key_end = key_bytes.iter().position(|byte| *byte == 0)?;
        let key = String::from_utf8_lossy(&key_bytes[..key_end]).into_owned();

        let value_start = data_table_start.checked_add(data_offset)?;
        let value_end = value_start.checked_add(data_len as usize)?;
        let raw_value = bytes.get(value_start..value_end)?;

        let value = match data_fmt {
            FORMAT_UTF8 | FORMAT_UTF8_SPECIAL => {
                let text = String::from_utf8_lossy(raw_value)
                    .trim_end_matches('\0')
                    .to_string();
                SfoValue::Text(text)
            }
            FORMAT_INT32 => {
                if raw_value.len() != 4 {
                    return None;
                }
                SfoValue::Int32(i32::from_le_bytes(raw_value.try_into().unwrap()))
            }
            other => SfoValue::Unknown {
                data_fmt: other,
                raw: raw_value.to_vec(),
            },
        };

        entries.push(SfoEntry { key, value });
    }

    Some(SfoObservation { entries })
}

fn read_u32le(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes(slice.try_into().unwrap()))
}

/// Turns a text-valued SFO key into a [`ContentEvidenceKind::ProductCode`]
/// fact at `Corroborated` confidence, if present. Shared helper for
/// [`crate::psp_boot_evidence`] (`DISC_ID`) and
/// [`crate::ps3_boot_evidence`] (`TITLE_ID`).
pub fn product_code_evidence(sfo: &SfoObservation, key: &str) -> Option<ContentEvidence> {
    sfo.get_text(key).map(|value| {
        ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            value.to_string(),
            ContentEvidenceConfidence::Corroborated,
            format!("candidate product/title code read from PARAM.SFO key {key}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SfoBuilder {
        entries: Vec<(String, u16, Vec<u8>)>,
    }

    impl SfoBuilder {
        fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }

        fn text(mut self, key: &str, value: &str) -> Self {
            let mut raw = value.as_bytes().to_vec();
            raw.push(0); // null terminator, matching FORMAT_UTF8
            self.entries.push((key.to_string(), FORMAT_UTF8, raw));
            self
        }

        fn int32(mut self, key: &str, value: i32) -> Self {
            self.entries
                .push((key.to_string(), FORMAT_INT32, value.to_le_bytes().to_vec()));
            self
        }

        fn unknown(mut self, key: &str, data_fmt: u16, raw: Vec<u8>) -> Self {
            self.entries.push((key.to_string(), data_fmt, raw));
            self
        }

        fn build(self) -> Vec<u8> {
            let index_table_len = self.entries.len() * SFO_INDEX_ENTRY_BYTES;
            let key_table_start = SFO_HEADER_BYTES + index_table_len;

            let mut key_table = Vec::new();
            let mut key_offsets = Vec::new();
            for (key, _, _) in &self.entries {
                key_offsets.push(key_table.len() as u16);
                key_table.extend_from_slice(key.as_bytes());
                key_table.push(0);
            }
            // Pad key table to a multiple of 4, matching real SFO files.
            while key_table.len() % 4 != 0 {
                key_table.push(0);
            }

            let data_table_start = key_table_start + key_table.len();
            let mut data_table = Vec::new();
            let mut data_offsets = Vec::new();
            for (_, _, raw) in &self.entries {
                data_offsets.push(data_table.len() as u32);
                data_table.extend_from_slice(raw);
            }

            let mut out = vec![0u8; SFO_HEADER_BYTES];
            out[0..4].copy_from_slice(SFO_MAGIC.as_slice());
            out[4..8].copy_from_slice(&0x0101_u32.to_le_bytes()); // version 1.01
            out[8..12].copy_from_slice(&(key_table_start as u32).to_le_bytes());
            out[12..16].copy_from_slice(&(data_table_start as u32).to_le_bytes());
            out[16..20].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());

            for (index, (_, data_fmt, raw)) in self.entries.iter().enumerate() {
                let mut entry = [0u8; SFO_INDEX_ENTRY_BYTES];
                entry[0..2].copy_from_slice(&key_offsets[index].to_le_bytes());
                entry[2..4].copy_from_slice(&data_fmt.to_le_bytes());
                entry[4..8].copy_from_slice(&(raw.len() as u32).to_le_bytes());
                entry[8..12].copy_from_slice(&(raw.len() as u32).to_le_bytes());
                entry[12..16].copy_from_slice(&data_offsets[index].to_le_bytes());
                out.extend_from_slice(&entry);
            }
            out.extend_from_slice(&key_table);
            out.extend_from_slice(&data_table);
            out
        }
    }

    // ------------------------------------------------------------------
    // Fixtures
    // ------------------------------------------------------------------

    #[test]
    fn valid_psp_fixture_parses() {
        let data = SfoBuilder::new()
            .text("DISC_ID", "ULUS10000")
            .text("TITLE", "Test PSP Game")
            .int32("DISC_VERSION", 1)
            .build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert_eq!(sfo.get_text("DISC_ID"), Some("ULUS10000"));
        assert_eq!(sfo.get_text("TITLE"), Some("Test PSP Game"));
        assert_eq!(sfo.get("DISC_VERSION"), Some(&SfoValue::Int32(1)));
    }

    #[test]
    fn valid_ps3_fixture_parses() {
        let data = SfoBuilder::new()
            .text("TITLE_ID", "BLUS30000")
            .text("TITLE", "Test PS3 Game")
            .text("APP_VER", "01.00")
            .int32("CATEGORY_INT", 0)
            .build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert_eq!(sfo.get_text("TITLE_ID"), Some("BLUS30000"));
        assert_eq!(sfo.get_text("APP_VER"), Some("01.00"));
    }

    // ------------------------------------------------------------------
    // Bounds / malformed input
    // ------------------------------------------------------------------

    #[test]
    fn truncated_header_fails_closed() {
        assert_eq!(parse_param_sfo(&[0u8; 10]), None);
    }

    #[test]
    fn bad_magic_fails_closed() {
        let mut data = SfoBuilder::new().text("A", "b").build();
        data[0] = 0xFF;
        assert_eq!(parse_param_sfo(&data), None);
    }

    #[test]
    fn invalid_key_offset_fails_closed() {
        let mut data = SfoBuilder::new().text("A", "b").build();
        // Point key_table_start far past the end of the file.
        data[8..12].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        assert_eq!(parse_param_sfo(&data), None);
    }

    #[test]
    fn invalid_data_offset_fails_closed() {
        let mut data = SfoBuilder::new().text("A", "b").build();
        data[12..16].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
        assert_eq!(parse_param_sfo(&data), None);
    }

    #[test]
    fn overlapping_out_of_bounds_value_length_fails_closed() {
        let mut data = SfoBuilder::new().text("A", "b").build();
        // Inflate the one entry's data_len far past what's actually there.
        let entry_start = SFO_HEADER_BYTES;
        data[entry_start + 4..entry_start + 8].copy_from_slice(&0xFFFFu32.to_le_bytes());
        assert_eq!(parse_param_sfo(&data), None);
    }

    #[test]
    fn absurd_entry_count_fails_closed() {
        let mut data = SfoBuilder::new().text("A", "b").build();
        data[16..20].copy_from_slice(&(MAX_SFO_ENTRIES + 1).to_le_bytes());
        assert_eq!(parse_param_sfo(&data), None);
    }

    #[test]
    fn oversized_file_fails_closed() {
        let huge = vec![0u8; MAX_SFO_BYTES + 1];
        assert_eq!(parse_param_sfo(&huge), None);
    }

    // ------------------------------------------------------------------
    // Unknown keys/types
    // ------------------------------------------------------------------

    #[test]
    fn unknown_key_is_preserved() {
        let data = SfoBuilder::new().text("SOME_FUTURE_KEY", "value").build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert_eq!(sfo.get_text("SOME_FUTURE_KEY"), Some("value"));
    }

    #[test]
    fn unknown_data_type_is_preserved_not_rejected() {
        let data = SfoBuilder::new()
            .unknown("WEIRD", 0x9999, vec![1, 2, 3, 4])
            .build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert_eq!(
            sfo.get("WEIRD"),
            Some(&SfoValue::Unknown {
                data_fmt: 0x9999,
                raw: vec![1, 2, 3, 4]
            })
        );
    }

    #[test]
    fn missing_key_returns_none() {
        let data = SfoBuilder::new().text("A", "b").build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert_eq!(sfo.get_text("MISSING"), None);
    }

    // ------------------------------------------------------------------
    // Evidence / determinism / no writes
    // ------------------------------------------------------------------

    #[test]
    fn product_code_evidence_is_corroborated() {
        let data = SfoBuilder::new().text("DISC_ID", "ULUS10000").build();
        let sfo = parse_param_sfo(&data).unwrap();
        let evidence = product_code_evidence(&sfo, "DISC_ID").unwrap();
        assert_eq!(evidence.kind, ContentEvidenceKind::ProductCode);
        assert_eq!(evidence.confidence, ContentEvidenceConfidence::Corroborated);
        assert_eq!(evidence.value, "ULUS10000");
    }

    #[test]
    fn missing_product_code_key_yields_no_evidence() {
        let data = SfoBuilder::new().text("TITLE", "no id here").build();
        let sfo = parse_param_sfo(&data).unwrap();
        assert!(product_code_evidence(&sfo, "DISC_ID").is_none());
    }

    #[test]
    fn parsing_never_mutates_input() {
        let data = SfoBuilder::new().text("A", "b").build();
        let before = data.clone();
        let _ = parse_param_sfo(&data);
        assert_eq!(data, before);
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let data = SfoBuilder::new().text("DISC_ID", "ULUS10000").build();
        assert_eq!(parse_param_sfo(&data), parse_param_sfo(&data));
    }
}
