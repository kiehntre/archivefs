//! Bounded, strict parsing of Xenia Canary's `.patch.toml` format.
//!
//! Generic TOML syntax is parsed via the `toml` crate; everything beyond
//! that is ArchiveFS's own schema-specific validator for this one upstream
//! format - this module is never a general-purpose TOML interpreter, and
//! it never executes or interprets the write operations it reads, only
//! records them.
//!
//! The schema mirrors Xenia Canary's own `xe::patcher::PatchDB` reader
//! (`src/xenia/patcher/patch_db.h`/`.cc` upstream): a `title_name`,
//! `title_id`, one or more module `hash` values, an optional `media_id`
//! constraint, and an array of `[[patch]]` tables each holding a name,
//! description, author, upstream `is_enabled` default, and one or more
//! typed write arrays (`be8`/`be16`/`be32`/`be64`/`f32`/`f64`/`string`/
//! `u16string`/`array`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const MAX_PATCH_FILE_BYTES: usize = 256 * 1024;
pub const MAX_PATCHES_PER_FILE: usize = 256;
pub const MAX_WRITES_PER_PATCH: usize = 256;
pub const MAX_HASHES_PER_FILE: usize = 32;
pub const MAX_MEDIA_IDS_PER_FILE: usize = 32;
pub const MAX_BYTE_ARRAY_BYTES: usize = 4096;
pub const MAX_STRING_VALUE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaWriteKind {
    Be8,
    Be16,
    Be32,
    Be64,
    F32,
    F64,
    String,
    U16String,
    ByteArray,
}

impl XeniaWriteKind {
    pub const ALL: [Self; 9] = [
        Self::Be8,
        Self::Be16,
        Self::Be32,
        Self::Be64,
        Self::F32,
        Self::F64,
        Self::String,
        Self::U16String,
        Self::ByteArray,
    ];

    /// The exact TOML array key upstream uses for this write type
    /// (`[[patch.be32]]`, `[[patch.array]]`, ...).
    pub fn toml_key(self) -> &'static str {
        match self {
            Self::Be8 => "be8",
            Self::Be16 => "be16",
            Self::Be32 => "be32",
            Self::Be64 => "be64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::String => "string",
            Self::U16String => "u16string",
            Self::ByteArray => "array",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum XeniaWriteValue {
    /// `be8`/`be16`/`be32`/`be64`: the logical integer value exactly as
    /// declared upstream. Byte order and width are carried by
    /// `XeniaPatchWrite::kind`, not applied here - this is inert data,
    /// never interpreted or executed.
    Integer(u64),
    /// `f32`/`f64`.
    Float(f64),
    /// `string`.
    Text(String),
    /// `u16string`.
    Utf16Text(String),
    /// `array`: decoded from upstream's hex-string encoding.
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XeniaPatchWrite {
    pub kind: XeniaWriteKind,
    pub address: u32,
    pub value: XeniaWriteValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XeniaPatchWarningKind {
    UnsupportedWriteType,
    InvalidAddress,
    InvalidValue,
    OversizedValue,
    DuplicateWrite,
    MissingName,
    MissingAuthor,
    MissingIsEnabled,
    TooManyWrites,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XeniaPatchWarning {
    pub kind: XeniaPatchWarningKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XeniaPatch {
    pub name: String,
    pub description: String,
    pub author: String,
    /// Upstream's own default `is_enabled` value - informational only.
    /// ArchiveFS never applies this automatically; the user's own
    /// selection is always what gets written.
    pub enabled_by_default: bool,
    pub writes: Vec<XeniaPatchWrite>,
    pub warnings: Vec<XeniaPatchWarning>,
}

impl XeniaPatch {
    #[must_use]
    pub fn has_blocking_warning(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// A patch is only ever offered for selection when it parsed cleanly
    /// and declares at least one real write - an empty or warned patch
    /// can never be installed, only reported.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        !self.has_blocking_warning() && !self.writes.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XeniaDocumentWarningKind {
    MissingTitleName,
    MissingTitleId,
    InvalidTitleId,
    MissingHash,
    InvalidHash,
    InvalidMediaId,
    DuplicatePatchName,
    NoPatches,
    ParseError,
    TooManyPatches,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XeniaDocumentWarning {
    pub kind: XeniaDocumentWarningKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XeniaPatchDocument {
    pub title_name: String,
    /// Eight uppercase hex characters, or empty when it could not be
    /// read - see `is_fatally_malformed`.
    pub title_id: String,
    /// Module hashes this file declares (sixteen uppercase hex
    /// characters each). Empty when upstream declares none.
    pub hashes: Vec<String>,
    /// Media IDs this file declares (eight uppercase hex characters
    /// each). Empty means this file does not constrain by Media ID -
    /// callers must never treat that as a wildcard match, only as
    /// "no declared constraint to compare against".
    pub media_ids: Vec<String>,
    pub patches: Vec<XeniaPatch>,
    pub warnings: Vec<XeniaDocumentWarning>,
}

impl XeniaPatchDocument {
    /// A document with no usable Title ID can never produce a trusted
    /// candidate - matching code must refuse it outright rather than
    /// treat it as a partially-verified source.
    #[must_use]
    pub fn is_fatally_malformed(&self) -> bool {
        self.title_id.is_empty()
    }

    #[must_use]
    pub fn selectable_patch_count(&self) -> usize {
        self.patches.iter().filter(|patch| patch.is_selectable()).count()
    }
}

fn document_warning(
    kind: XeniaDocumentWarningKind,
    detail: impl Into<String>,
) -> XeniaDocumentWarning {
    XeniaDocumentWarning {
        kind,
        detail: detail.into(),
    }
}

fn patch_warning(kind: XeniaPatchWarningKind, detail: impl Into<String>) -> XeniaPatchWarning {
    XeniaPatchWarning {
        kind,
        detail: detail.into(),
    }
}

fn malformed(detail: impl Into<String>) -> XeniaPatchDocument {
    XeniaPatchDocument {
        title_name: String::new(),
        title_id: String::new(),
        hashes: Vec::new(),
        media_ids: Vec::new(),
        patches: Vec::new(),
        warnings: vec![document_warning(XeniaDocumentWarningKind::ParseError, detail)],
    }
}

fn is_hex_of_len(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Parses one `.patch.toml` file's raw text as bounded data. Never
/// panics, never executes anything from `text`, and a syntactically or
/// semantically malformed file always comes back as a document whose
/// `is_fatally_malformed()` is `true` - it is never partially trusted.
#[must_use]
pub fn parse_xenia_patch_toml(text: &str) -> XeniaPatchDocument {
    if text.len() > MAX_PATCH_FILE_BYTES {
        return malformed(format!(
            "patch file exceeds the {MAX_PATCH_FILE_BYTES}-byte bound"
        ));
    }
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(error) => return malformed(format!("invalid TOML syntax: {error}")),
    };

    let mut warnings = Vec::new();

    let title_name = table
        .get("title_name")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if title_name.is_empty() {
        warnings.push(document_warning(
            XeniaDocumentWarningKind::MissingTitleName,
            "title_name is missing or empty",
        ));
    }

    let title_id = match table.get("title_id").and_then(toml::Value::as_str) {
        Some(value) if is_hex_of_len(value, 8) => value.to_ascii_uppercase(),
        Some(value) => {
            warnings.push(document_warning(
                XeniaDocumentWarningKind::InvalidTitleId,
                format!("title_id {value:?} is not eight hex characters"),
            ));
            String::new()
        }
        None => {
            warnings.push(document_warning(
                XeniaDocumentWarningKind::MissingTitleId,
                "title_id is missing",
            ));
            String::new()
        }
    };

    let (hashes, hash_warnings) = read_hex_field(
        table.get("hash"),
        16,
        MAX_HASHES_PER_FILE,
        XeniaDocumentWarningKind::InvalidHash,
    );
    warnings.extend(hash_warnings);
    if hashes.is_empty() && table.get("hash").is_none() {
        warnings.push(document_warning(
            XeniaDocumentWarningKind::MissingHash,
            "hash is missing; module-hash matching can never be verified for this file",
        ));
    }

    let (media_ids, media_warnings) = read_hex_field(
        table.get("media_id"),
        8,
        MAX_MEDIA_IDS_PER_FILE,
        XeniaDocumentWarningKind::InvalidMediaId,
    );
    warnings.extend(media_warnings);

    let mut patches = Vec::new();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    if let Some(toml::Value::Array(entries)) = table.get("patch") {
        if entries.len() > MAX_PATCHES_PER_FILE {
            warnings.push(document_warning(
                XeniaDocumentWarningKind::TooManyPatches,
                format!("file declares more than the {MAX_PATCHES_PER_FILE}-patch bound"),
            ));
        }
        for entry in entries.iter().take(MAX_PATCHES_PER_FILE) {
            if let toml::Value::Table(patch_table) = entry {
                let patch = parse_patch_entry(patch_table);
                if !patch.name.is_empty() && !seen_names.insert(patch.name.clone()) {
                    warnings.push(document_warning(
                        XeniaDocumentWarningKind::DuplicatePatchName,
                        format!("duplicate patch name: {}", patch.name),
                    ));
                }
                patches.push(patch);
            }
        }
    }
    if patches.is_empty() {
        warnings.push(document_warning(
            XeniaDocumentWarningKind::NoPatches,
            "file contains no [[patch]] entries",
        ));
    }

    XeniaPatchDocument {
        title_name,
        title_id,
        hashes,
        media_ids,
        patches,
        warnings,
    }
}

/// Reads a field that upstream declares as either one hex string or an
/// array of hex strings (`hash`, `media_id`), matching Xenia's own
/// `PatchDB::ReadHashes` (`is_value()` or `is_array()`).
fn read_hex_field(
    field: Option<&toml::Value>,
    expected_len: usize,
    max_entries: usize,
    invalid_kind: XeniaDocumentWarningKind,
) -> (Vec<String>, Vec<XeniaDocumentWarning>) {
    let mut values = Vec::new();
    let mut warnings = Vec::new();
    let Some(field) = field else {
        return (values, warnings);
    };
    let mut push_one = |value: &toml::Value, warnings: &mut Vec<XeniaDocumentWarning>| {
        let Some(text) = value.as_str() else {
            warnings.push(document_warning(invalid_kind, "expected a hex string"));
            return;
        };
        if text.is_empty() {
            return;
        }
        if !is_hex_of_len(text, expected_len) {
            warnings.push(document_warning(
                invalid_kind,
                format!("{text:?} is not {expected_len} hex characters"),
            ));
            return;
        }
        if values.len() < max_entries {
            values.push(text.to_ascii_uppercase());
        }
    };
    match field {
        toml::Value::Array(entries) => {
            for entry in entries {
                push_one(entry, &mut warnings);
            }
        }
        other => push_one(other, &mut warnings),
    }
    (values, warnings)
}

fn parse_patch_entry(table: &toml::Table) -> XeniaPatch {
    let mut warnings = Vec::new();

    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if name.is_empty() {
        warnings.push(patch_warning(
            XeniaPatchWarningKind::MissingName,
            "name is missing or empty",
        ));
    }
    let description = table
        .get("desc")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let author = table
        .get("author")
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if author.is_empty() {
        warnings.push(patch_warning(
            XeniaPatchWarningKind::MissingAuthor,
            "author is missing or empty",
        ));
    }
    let enabled_by_default = match table.get("is_enabled") {
        Some(toml::Value::Boolean(value)) => *value,
        Some(_) => {
            warnings.push(patch_warning(
                XeniaPatchWarningKind::MissingIsEnabled,
                "is_enabled is not a boolean",
            ));
            false
        }
        None => {
            warnings.push(patch_warning(
                XeniaPatchWarningKind::MissingIsEnabled,
                "is_enabled is missing",
            ));
            false
        }
    };

    let mut writes = Vec::new();
    let mut seen_addresses: BTreeSet<u32> = BTreeSet::new();
    let mut total_write_entries = 0_usize;
    for kind in XeniaWriteKind::ALL {
        let Some(toml::Value::Array(entries)) = table.get(kind.toml_key()) else {
            continue;
        };
        total_write_entries += entries.len();
        for write_entry in entries.iter().take(MAX_WRITES_PER_PATCH) {
            let Some(write_table) = write_entry.as_table() else {
                warnings.push(patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    format!("{} entry is not a table", kind.toml_key()),
                ));
                continue;
            };
            match parse_write(kind, write_table) {
                Ok(write) => {
                    if !seen_addresses.insert(write.address) {
                        warnings.push(patch_warning(
                            XeniaPatchWarningKind::DuplicateWrite,
                            format!("duplicate write at address 0x{:08x}", write.address),
                        ));
                    }
                    writes.push(write);
                }
                Err(warning) => warnings.push(warning),
            }
        }
    }
    if total_write_entries > MAX_WRITES_PER_PATCH {
        warnings.push(patch_warning(
            XeniaPatchWarningKind::TooManyWrites,
            format!("patch declares more than the {MAX_WRITES_PER_PATCH}-write bound"),
        ));
    }

    let known_keys = ["name", "desc", "author", "is_enabled"];
    for (key, value) in table {
        if known_keys.contains(&key.as_str())
            || XeniaWriteKind::ALL.iter().any(|kind| kind.toml_key() == key)
        {
            continue;
        }
        if matches!(value, toml::Value::Array(items) if items.iter().all(toml::Value::is_table)) {
            warnings.push(patch_warning(
                XeniaPatchWarningKind::UnsupportedWriteType,
                format!("unrecognized patch operation type: {key}"),
            ));
        }
    }

    XeniaPatch {
        name,
        description,
        author,
        enabled_by_default,
        writes,
        warnings,
    }
}

fn parse_write(
    kind: XeniaWriteKind,
    table: &toml::Table,
) -> Result<XeniaPatchWrite, XeniaPatchWarning> {
    let address = table
        .get("address")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| {
            patch_warning(
                XeniaPatchWarningKind::InvalidAddress,
                "address is missing or not an integer",
            )
        })?;
    let address = u32::try_from(address).map_err(|_| {
        patch_warning(
            XeniaPatchWarningKind::InvalidAddress,
            format!("address 0x{address:x} does not fit in 32 bits"),
        )
    })?;
    let value = table.get("value").ok_or_else(|| {
        patch_warning(XeniaPatchWarningKind::InvalidValue, "value is missing")
    })?;

    let write_value = match kind {
        XeniaWriteKind::Be8 | XeniaWriteKind::Be16 | XeniaWriteKind::Be32 | XeniaWriteKind::Be64 => {
            let integer = value.as_integer().ok_or_else(|| {
                patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    "expected an integer value",
                )
            })?;
            if integer < 0 {
                return Err(patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    "value must not be negative",
                ));
            }
            let max: i64 = match kind {
                XeniaWriteKind::Be8 => i64::from(u8::MAX),
                XeniaWriteKind::Be16 => i64::from(u16::MAX),
                XeniaWriteKind::Be32 => i64::from(u32::MAX),
                XeniaWriteKind::Be64 => i64::MAX,
                _ => unreachable!(),
            };
            if integer > max {
                return Err(patch_warning(
                    XeniaPatchWarningKind::OversizedValue,
                    format!("value does not fit in {}", kind.toml_key()),
                ));
            }
            XeniaWriteValue::Integer(integer as u64)
        }
        XeniaWriteKind::F32 | XeniaWriteKind::F64 => {
            let float = value
                .as_float()
                .or_else(|| value.as_integer().map(|integer| integer as f64))
                .ok_or_else(|| {
                    patch_warning(
                        XeniaPatchWarningKind::InvalidValue,
                        "expected a floating-point value",
                    )
                })?;
            XeniaWriteValue::Float(float)
        }
        XeniaWriteKind::String | XeniaWriteKind::U16String => {
            let text = value.as_str().ok_or_else(|| {
                patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    "expected a string value",
                )
            })?;
            if text.len() > MAX_STRING_VALUE_BYTES {
                return Err(patch_warning(
                    XeniaPatchWarningKind::OversizedValue,
                    format!("string value exceeds the {MAX_STRING_VALUE_BYTES}-byte bound"),
                ));
            }
            if kind == XeniaWriteKind::String {
                XeniaWriteValue::Text(text.to_string())
            } else {
                XeniaWriteValue::Utf16Text(text.to_string())
            }
        }
        XeniaWriteKind::ByteArray => {
            let text = value.as_str().ok_or_else(|| {
                patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    "expected a hex string value",
                )
            })?;
            let bytes = decode_hex(text).ok_or_else(|| {
                patch_warning(
                    XeniaPatchWarningKind::InvalidValue,
                    "value is not a valid hex-encoded byte array",
                )
            })?;
            if bytes.len() > MAX_BYTE_ARRAY_BYTES {
                return Err(patch_warning(
                    XeniaPatchWarningKind::OversizedValue,
                    format!("byte array exceeds the {MAX_BYTE_ARRAY_BYTES}-byte bound"),
                ));
            }
            XeniaWriteValue::Bytes(bytes)
        }
    };
    Ok(XeniaPatchWrite {
        kind,
        address,
        value: write_value,
    })
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.strip_prefix("0x").unwrap_or(text);
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let digits = text.as_bytes();
    for pair in digits.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_exact_title_and_module_hash_with_a_single_be32_write() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"

[[patch]]
    name = "Performance fix"
    desc = "Disables the FPS limit."
    author = "Sowa_95"
    is_enabled = false

    [[patch.be32]]
        address = 0x821b7140
        value = 0x39600001
"#,
        );
        assert!(!document.is_fatally_malformed());
        assert_eq!(document.title_id, "415607D2");
        assert_eq!(document.hashes, vec!["4768B579A3C5F134"]);
        assert!(document.media_ids.is_empty());
        assert_eq!(document.patches.len(), 1);
        let patch = &document.patches[0];
        assert!(patch.is_selectable());
        assert_eq!(patch.writes.len(), 1);
        assert_eq!(patch.writes[0].address, 0x821b_7140);
        assert_eq!(patch.writes[0].value, XeniaWriteValue::Integer(0x3960_0001));
    }

    #[test]
    fn parses_multiple_patches_and_multiple_module_hashes_in_one_file() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = [
    "4768B579A3C5F134",
    "2B6EE9E95E23E2A5"
]

[[patch]]
    name = "First"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be16]]
        address = 0x1000
        value = 0x0780

[[patch]]
    name = "Second"
    desc = ""
    author = "b"
    is_enabled = true
    [[patch.be8]]
        address = 0x2000
        value = 0x01
"#,
        );
        assert_eq!(document.hashes.len(), 2);
        assert_eq!(document.patches.len(), 2);
        assert_eq!(document.selectable_patch_count(), 2);
        assert!(document.patches[1].enabled_by_default);
        assert!(!document.patches[0].enabled_by_default);
    }

    #[test]
    fn media_id_constraint_is_read_as_a_single_value_or_array() {
        let single = parse_xenia_patch_toml(
            r#"
title_name = "Catherine"
title_id = "415407D7"
hash = "C451BB35FB61698F"
media_id = "580DEC6A"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x1
        value = 0x1
"#,
        );
        assert_eq!(single.media_ids, vec!["580DEC6A"]);

        let multiple = parse_xenia_patch_toml(
            r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"
media_id = ["00000000", "4C27792A"]
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x1
        value = 0x1
"#,
        );
        assert_eq!(multiple.media_ids, vec!["00000000", "4C27792A"]);
    }

    #[test]
    fn multiple_module_hashes_are_all_retained() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Marvel Ultimate Alliance"
title_id = "415607DA"
hash = ["1111111111111111", "2222222222222222", "3333333333333333"]
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x1
        value = 0x1
"#,
        );
        assert_eq!(document.hashes.len(), 3);
    }

    #[test]
    fn incompatible_title_id_is_reported_as_invalid_not_silently_fixed() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Bad"
title_id = "NOTHEX01"
hash = "4768B579A3C5F134"
"#,
        );
        assert!(document.is_fatally_malformed());
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::InvalidTitleId)
        );
    }

    #[test]
    fn incompatible_media_id_shape_is_reported() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Bad"
title_id = "415607D2"
hash = "4768B579A3C5F134"
media_id = "not-hex"
"#,
        );
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::InvalidMediaId)
        );
    }

    #[test]
    fn incompatible_hash_shape_is_reported() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Bad"
title_id = "415607D2"
hash = "tooshort"
"#,
        );
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::InvalidHash)
        );
    }

    #[test]
    fn missing_module_hash_evidence_is_reported_explicitly() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "No Hash"
title_id = "415607D2"
"#,
        );
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::MissingHash)
        );
        assert!(document.hashes.is_empty());
    }

    #[test]
    fn malformed_toml_never_produces_a_partially_trusted_candidate() {
        let document = parse_xenia_patch_toml("this is not [ valid toml");
        assert!(document.is_fatally_malformed());
        assert!(document.patches.is_empty());
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::ParseError)
        );
    }

    #[test]
    fn unsupported_operation_type_is_blocked_not_discarded() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Weird"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be128]]
        address = 0x1
        value = 0x1
"#,
        );
        let patch = &document.patches[0];
        assert!(!patch.is_selectable());
        assert!(
            patch
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaPatchWarningKind::UnsupportedWriteType)
        );
    }

    #[test]
    fn duplicate_patch_definitions_are_detected() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Dup"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "Same"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x1
        value = 0x1
[[patch]]
    name = "Same"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x2
        value = 0x2
"#,
        );
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::DuplicatePatchName)
        );
    }

    #[test]
    fn conflicting_duplicate_writes_within_one_patch_are_warned() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Conflict"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be32]]
        address = 0x1000
        value = 0x1
    [[patch.be32]]
        address = 0x1000
        value = 0x2
"#,
        );
        let patch = &document.patches[0];
        assert!(!patch.is_selectable());
        assert!(
            patch
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaPatchWarningKind::DuplicateWrite)
        );
    }

    #[test]
    fn oversized_byte_array_is_bounded() {
        let hex = "AB".repeat(MAX_BYTE_ARRAY_BYTES + 1);
        let document = parse_xenia_patch_toml(&format!(
            r#"
title_name = "Big"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.array]]
        address = 0x1
        value = "{hex}"
"#
        ));
        let patch = &document.patches[0];
        assert!(!patch.is_selectable());
        assert!(
            patch
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaPatchWarningKind::OversizedValue)
        );
    }

    #[test]
    fn oversized_string_value_is_bounded() {
        let text = "a".repeat(MAX_STRING_VALUE_BYTES + 1);
        let document = parse_xenia_patch_toml(&format!(
            r#"
title_name = "Big"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.string]]
        address = 0x1
        value = "{text}"
"#
        ));
        let patch = &document.patches[0];
        assert!(!patch.is_selectable());
        assert!(
            patch
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaPatchWarningKind::OversizedValue)
        );
    }

    #[test]
    fn byte_array_write_decodes_hex_and_string_write_is_verbatim() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Mixed"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.array]]
        address = 0x1
        value = "DEADBEEF"
    [[patch.string]]
        address = 0x2
        value = "hello"
    [[patch.f32]]
        address = 0x3
        value = 1.5
"#,
        );
        let patch = &document.patches[0];
        assert!(patch.is_selectable());
        assert_eq!(patch.writes.len(), 3);
        let find = |kind: XeniaWriteKind| {
            patch
                .writes
                .iter()
                .find(|write| write.kind == kind)
                .unwrap()
        };
        assert_eq!(
            find(XeniaWriteKind::ByteArray).value,
            XeniaWriteValue::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])
        );
        assert_eq!(
            find(XeniaWriteKind::String).value,
            XeniaWriteValue::Text("hello".into())
        );
        assert_eq!(find(XeniaWriteKind::F32).value, XeniaWriteValue::Float(1.5));
    }

    #[test]
    fn invalid_address_is_rejected() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Bad Address"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = -1
        value = 0x1
"#,
        );
        let patch = &document.patches[0];
        assert!(!patch.is_selectable());
        assert!(
            patch
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaPatchWarningKind::InvalidAddress)
        );
    }

    #[test]
    fn no_patches_at_all_is_reported() {
        let document = parse_xenia_patch_toml(
            r#"
title_name = "Empty"
title_id = "415607D2"
hash = "4768B579A3C5F134"
"#,
        );
        assert!(
            document
                .warnings
                .iter()
                .any(|warning| warning.kind == XeniaDocumentWarningKind::NoPatches)
        );
    }
}
