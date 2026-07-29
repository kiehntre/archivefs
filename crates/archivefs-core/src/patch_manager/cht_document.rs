//! Full-fidelity, panic-free parsing and deterministic rendering of
//! RetroArch/libretro `.cht` cheat files.
//!
//! ## Why this exists alongside the two existing `.cht` readers
//!
//! - `retroarch_inventory::parse_cheat_summary` answers "how many cheats
//!   does this installed file declare, and did it parse cleanly?" and
//!   keeps aggregate counts only.
//! - `cheat_catalogue::parse_cht_cheats` keeps one description and
//!   enabled-by-default flag per entry, and deliberately never retains a
//!   cheat *code* body.
//!
//! Neither can answer "install exactly these three cheats", because
//! generating an installable file requires the code bodies. This module is
//! the first reader that retains them, and it is therefore also the first
//! one that can *write* a `.cht` file. It is used only on the explicit
//! install path (a user has selected one candidate and is choosing which
//! of its cheats to install); catalogue indexing still uses the
//! metadata-only parser, so the broad indexing pass keeps its existing
//! memory profile.
//!
//! ## Guarantees
//!
//! - **Never panics on catalogue input.** Every field is bounded, every
//!   index is checked, and no slicing is done on a non-char boundary.
//! - **Never mutates the source.** Parsing is a pure function of bytes.
//! - **Deterministic rendering.** [`render_cht_file`] is a pure function of
//!   its input slice: same entries in, byte-identical file out.
//! - **Documented, deterministic repairs only.** The only content the
//!   renderer alters is a double quote inside a description or code value
//!   (RetroArch's own `config_file` reader has no escape syntax, so an
//!   unescaped quote would truncate the value it is written into). That
//!   single substitution - `"` becomes `'` - is applied at render time,
//!   reported as [`ChtEntryWarningKind::QuoteNormalized`] at parse time,
//!   and never written back to the catalogue.

use std::fmt;

use serde::Serialize;

/// Mirrors `cheat_catalogue::MAX_CHEATS_PER_GAME` and
/// `retroarch_inventory::MAX_CHEAT_ENTRIES_PER_FILE`.
pub const MAX_CHT_ENTRIES: usize = 16_384;
/// One `cheatN_*` value, after unquoting. Longer values are truncated and
/// the entry is marked unselectable rather than silently shortened.
pub const MAX_CHT_FIELD_BYTES: usize = 4 * 1024;
/// Preserved non-`cheatN_*` keys (`cheat_delay`, custom tooling keys, ...).
pub const MAX_CHT_GLOBAL_FIELDS: usize = 64;
/// Preserved leading `#` comment lines.
pub const MAX_CHT_PRESERVED_COMMENTS: usize = 32;
/// Preserved `cheatN_<field>` keys other than `desc`/`code`/`enable`.
pub const MAX_CHT_EXTRA_FIELDS_PER_ENTRY: usize = 32;
/// Bounded document-level warning list.
pub const MAX_CHT_DOCUMENT_WARNINGS: usize = 256;

/// A whole-file parse failure. An individual bad *line* never produces one
/// of these - it produces a warning and leaves the rest of the file usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChtParseErrorKind {
    /// The bytes are not valid UTF-8. RetroArch cheat files are ASCII in
    /// practice; anything else is reported rather than lossily decoded.
    UnsupportedEncoding,
    /// A UTF-16/UTF-32 byte-order mark was found. Reported separately from
    /// generic invalid UTF-8 because it is a recognisable, actionable case.
    UnsupportedUtf16Encoding,
    /// The file declares no `cheats = N` key and contains no `cheatN_*`
    /// entry at all - it is not a cheat file.
    NotACheatFile,
    /// More than [`MAX_CHT_ENTRIES`] distinct entry indexes.
    TooManyEntries,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtParseError {
    pub kind: ChtParseErrorKind,
    pub detail: String,
}

impl fmt::Display for ChtParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ChtParseError {}

/// Why one entry is imperfect. Presence of a warning does not by itself
/// make an entry unselectable - see [`ChtEntry::is_selectable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChtEntryWarningKind {
    /// No `cheatN_code` key at all. Blocking: nothing could be installed.
    MissingCode,
    /// `cheatN_code` present but empty after unquoting. Blocking.
    EmptyCode,
    /// No `cheatN_desc`. Not blocking - a synthetic, index-derived
    /// description is rendered instead.
    MissingDescription,
    /// A `cheatN_*` key appeared twice. The first value wins; the later one
    /// is dropped. Not blocking.
    DuplicateField,
    /// `cheatN_enable` had a value other than `true`/`false`. Treated as
    /// `false`. Not blocking.
    UnparsableEnableValue,
    /// A value exceeded [`MAX_CHT_FIELD_BYTES`]. Blocking, because the
    /// retained value is not the source value.
    OversizedField,
    /// A double quote inside a value will be written as `'` by
    /// [`render_cht_file`]. Not blocking; see the module docs.
    QuoteNormalized,
    /// The value contained a control character (newline, NUL, ...) that
    /// cannot appear in a RetroArch config value. Blocking.
    ControlCharacter,
}

impl ChtEntryWarningKind {
    /// Whether this warning alone makes an entry unsafe to install.
    #[must_use]
    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::MissingCode | Self::EmptyCode | Self::OversizedField | Self::ControlCharacter
        )
    }

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingCode => "cht_entry_missing_code",
            Self::EmptyCode => "cht_entry_empty_code",
            Self::MissingDescription => "cht_entry_missing_description",
            Self::DuplicateField => "cht_entry_duplicate_field",
            Self::UnparsableEnableValue => "cht_entry_unparsable_enable_value",
            Self::OversizedField => "cht_entry_oversized_field",
            Self::QuoteNormalized => "cht_entry_quote_normalized",
            Self::ControlCharacter => "cht_entry_control_character",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtEntryWarning {
    pub kind: ChtEntryWarningKind,
    pub detail: String,
}

/// A document-level problem that is not attributable to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChtDocumentWarningKind {
    /// A non-empty, non-comment line with no `=`.
    MalformedLine,
    /// `cheats = <not a number>`.
    MalformedDeclaredCount,
    /// A `cheatN_` key whose `N` is not a plain decimal index.
    MalformedEntryIndex,
    /// An entry index at or beyond [`MAX_CHT_ENTRIES`].
    EntryIndexOutOfRange,
    /// `cheats = N` disagrees with the number of distinct parsed indexes.
    DeclaredCountMismatch,
    /// The parsed indexes are not `0..n` - e.g. `cheat0_*` and `cheat5_*`
    /// with nothing between. Never repaired in place; renumbering happens
    /// only in the rendered output.
    NonContiguousIndexes,
    /// A bound ([`MAX_CHT_GLOBAL_FIELDS`], [`MAX_CHT_PRESERVED_COMMENTS`],
    /// [`MAX_CHT_EXTRA_FIELDS_PER_ENTRY`], [`MAX_CHT_DOCUMENT_WARNINGS`])
    /// was reached and later content of that kind was dropped.
    LimitReached,
}

impl ChtDocumentWarningKind {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedLine => "cht_malformed_line",
            Self::MalformedDeclaredCount => "cht_malformed_declared_count",
            Self::MalformedEntryIndex => "cht_malformed_entry_index",
            Self::EntryIndexOutOfRange => "cht_entry_index_out_of_range",
            Self::DeclaredCountMismatch => "cht_declared_count_mismatch",
            Self::NonContiguousIndexes => "cht_non_contiguous_indexes",
            Self::LimitReached => "cht_limit_reached",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtDocumentWarning {
    pub kind: ChtDocumentWarningKind,
    /// 1-based source line, when the warning is attributable to one.
    pub line: Option<u32>,
    pub detail: String,
}

/// One parsed cheat, retaining everything needed to write it back out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtEntry {
    /// The `cheatN_` index exactly as the source declared it. Rendering
    /// renumbers; this field never does.
    pub index: u32,
    pub description: Option<String>,
    pub code: Option<String>,
    pub enabled_by_default: bool,
    /// `cheatN_<field>` pairs other than `desc`/`code`/`enable`, in
    /// first-seen order, with `<field>` (not the full key) as the name.
    pub extra_fields: Vec<(String, String)>,
    pub warnings: Vec<ChtEntryWarning>,
}

impl ChtEntry {
    /// Whether this entry can be offered for selection and installed. An
    /// entry with only non-blocking warnings stays selectable; the warnings
    /// are still shown.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.code.as_deref().is_some_and(|code| !code.is_empty())
            && !self
                .warnings
                .iter()
                .any(|warning| warning.kind.is_blocking())
    }

    /// The description shown in the picker and written to the installed
    /// file. Falls back to a stable, index-derived label so an entry with
    /// no `cheatN_desc` is never rendered with an empty name.
    #[must_use]
    pub fn effective_description(&self) -> String {
        match self.description.as_deref() {
            Some(text) if !text.trim().is_empty() => text.to_string(),
            _ => format!("Cheat {}", self.index),
        }
    }

    pub fn blocking_warnings(&self) -> impl Iterator<Item = &ChtEntryWarning> {
        self.warnings
            .iter()
            .filter(|warning| warning.kind.is_blocking())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtDocument {
    /// The `cheats = N` value the source declared, if any.
    pub declared_count: Option<u32>,
    /// Entries in ascending declared-index order - the catalogue's own
    /// order, which the picker and the renderer both preserve.
    pub entries: Vec<ChtEntry>,
    /// Leading `#` comment lines, verbatim minus the leading `#`.
    pub preserved_comments: Vec<String>,
    /// Non-`cheatN_*`, non-`cheats` keys, in first-seen order.
    pub global_fields: Vec<(String, String)>,
    pub warnings: Vec<ChtDocumentWarning>,
}

impl ChtDocument {
    #[must_use]
    pub fn selectable_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.is_selectable())
            .count()
    }

    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty() || self.entries.iter().any(|entry| !entry.warnings.is_empty())
    }

    #[must_use]
    pub fn entry(&self, index: u32) -> Option<&ChtEntry> {
        self.entries.iter().find(|entry| entry.index == index)
    }
}

/// Parses raw catalogue bytes. Returns `Err` only for a whole-file problem
/// (see [`ChtParseErrorKind`]); a file with individually broken lines still
/// parses, with warnings.
pub fn parse_cht_bytes(bytes: &[u8]) -> Result<ChtDocument, ChtParseError> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(ChtParseError {
            kind: ChtParseErrorKind::UnsupportedUtf16Encoding,
            detail:
                "file begins with a UTF-16 byte-order mark; only UTF-8 cheat files are supported"
                    .to_string(),
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|error| ChtParseError {
        kind: ChtParseErrorKind::UnsupportedEncoding,
        detail: format!(
            "file is not valid UTF-8 (first invalid byte at offset {})",
            error.valid_up_to()
        ),
    })?;
    parse_cht_text(text.strip_prefix('\u{feff}').unwrap_or(text))
}

/// Parses already-decoded text. Prefer [`parse_cht_bytes`] for catalogue
/// input so encoding problems are reported rather than assumed away.
pub fn parse_cht_text(text: &str) -> Result<ChtDocument, ChtParseError> {
    use std::collections::BTreeMap;

    struct Draft {
        description: Option<String>,
        code: Option<String>,
        enable: Option<bool>,
        extra_fields: Vec<(String, String)>,
        warnings: Vec<ChtEntryWarning>,
    }

    let mut declared_count: Option<u32> = None;
    let mut drafts: BTreeMap<u32, Draft> = BTreeMap::new();
    let mut preserved_comments: Vec<String> = Vec::new();
    let mut global_fields: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<ChtDocumentWarning> = Vec::new();
    let mut seen_any_body_line = false;
    let mut limit_reported = false;

    let push_warning = |warnings: &mut Vec<ChtDocumentWarning>,
                        kind: ChtDocumentWarningKind,
                        line: Option<u32>,
                        detail: String| {
        if warnings.len() < MAX_CHT_DOCUMENT_WARNINGS {
            warnings.push(ChtDocumentWarning { kind, line, detail });
        }
    };

    for (offset, raw_line) in text.lines().enumerate() {
        let line_number = u32::try_from(offset + 1).unwrap_or(u32::MAX);
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if !seen_any_body_line && preserved_comments.len() < MAX_CHT_PRESERVED_COMMENTS {
                preserved_comments.push(comment.trim().to_string());
            }
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            seen_any_body_line = true;
            push_warning(
                &mut warnings,
                ChtDocumentWarningKind::MalformedLine,
                Some(line_number),
                format!("line {line_number} has no '=' separator and was ignored"),
            );
            continue;
        };
        seen_any_body_line = true;
        let key = raw_key.trim();
        let (value, value_warnings) = decode_value(raw_value.trim());

        if key.eq_ignore_ascii_case("cheats") {
            match value.parse::<u32>() {
                Ok(count) => declared_count = Some(count),
                Err(_) => push_warning(
                    &mut warnings,
                    ChtDocumentWarningKind::MalformedDeclaredCount,
                    Some(line_number),
                    format!("line {line_number}: 'cheats' value {value:?} is not a number"),
                ),
            }
            continue;
        }

        // `cheat_delay` and friends are RetroArch's own global cheat keys:
        // they share the `cheat` prefix but continue with `_`, never with a
        // digit, so they are preserved as global fields rather than
        // mistaken for a malformed `cheatN_` entry key.
        let entry_key = key
            .strip_prefix("cheat")
            .filter(|remainder| !remainder.starts_with('_'));
        let Some(remainder) = entry_key else {
            if global_fields.len() < MAX_CHT_GLOBAL_FIELDS {
                if !global_fields.iter().any(|(name, _)| name == key) {
                    global_fields.push((key.to_string(), value));
                }
            } else if !limit_reported {
                limit_reported = true;
                push_warning(
                    &mut warnings,
                    ChtDocumentWarningKind::LimitReached,
                    Some(line_number),
                    format!("more than {MAX_CHT_GLOBAL_FIELDS} non-cheat keys; later keys dropped"),
                );
            }
            continue;
        };

        let digit_count = remainder.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 || !remainder[digit_count..].starts_with('_') {
            push_warning(
                &mut warnings,
                ChtDocumentWarningKind::MalformedEntryIndex,
                Some(line_number),
                format!("line {line_number}: key {key:?} is not a valid cheatN_<field> key"),
            );
            continue;
        }
        let Ok(entry_index) = remainder[..digit_count].parse::<u32>() else {
            push_warning(
                &mut warnings,
                ChtDocumentWarningKind::MalformedEntryIndex,
                Some(line_number),
                format!("line {line_number}: entry index in {key:?} is out of numeric range"),
            );
            continue;
        };
        if entry_index as usize >= MAX_CHT_ENTRIES {
            push_warning(
                &mut warnings,
                ChtDocumentWarningKind::EntryIndexOutOfRange,
                Some(line_number),
                format!(
                    "line {line_number}: entry index {entry_index} is past the supported limit"
                ),
            );
            continue;
        }
        let field = &remainder[digit_count + 1..];
        if !drafts.contains_key(&entry_index) && drafts.len() >= MAX_CHT_ENTRIES {
            return Err(ChtParseError {
                kind: ChtParseErrorKind::TooManyEntries,
                detail: format!("file declares more than {MAX_CHT_ENTRIES} cheat entries"),
            });
        }
        let draft = drafts.entry(entry_index).or_insert_with(|| Draft {
            description: None,
            code: None,
            enable: None,
            extra_fields: Vec::new(),
            warnings: Vec::new(),
        });
        draft.warnings.extend(value_warnings);

        match field {
            "desc" => set_once(
                &mut draft.description,
                value,
                "desc",
                entry_index,
                &mut draft.warnings,
            ),
            "code" => set_once(
                &mut draft.code,
                value,
                "code",
                entry_index,
                &mut draft.warnings,
            ),
            "enable" => {
                if draft.enable.is_some() {
                    draft.warnings.push(ChtEntryWarning {
                        kind: ChtEntryWarningKind::DuplicateField,
                        detail: format!("cheat{entry_index}_enable appeared more than once"),
                    });
                } else if value.eq_ignore_ascii_case("true") {
                    draft.enable = Some(true);
                } else if value.eq_ignore_ascii_case("false") {
                    draft.enable = Some(false);
                } else {
                    draft.enable = Some(false);
                    draft.warnings.push(ChtEntryWarning {
                        kind: ChtEntryWarningKind::UnparsableEnableValue,
                        detail: format!(
                            "cheat{entry_index}_enable value {value:?} is not true/false; treated as false"
                        ),
                    });
                }
            }
            "" => {
                push_warning(
                    &mut warnings,
                    ChtDocumentWarningKind::MalformedEntryIndex,
                    Some(line_number),
                    format!("line {line_number}: key {key:?} has an empty field name"),
                );
            }
            other => {
                if draft.extra_fields.len() < MAX_CHT_EXTRA_FIELDS_PER_ENTRY
                    && !draft.extra_fields.iter().any(|(name, _)| name == other)
                {
                    draft.extra_fields.push((other.to_string(), value));
                }
            }
        }
    }

    if declared_count.is_none() && drafts.is_empty() {
        return Err(ChtParseError {
            kind: ChtParseErrorKind::NotACheatFile,
            detail: "no 'cheats' key and no cheatN_* entry was found".to_string(),
        });
    }

    let mut entries: Vec<ChtEntry> = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts {
        let mut entry_warnings = draft.warnings;
        match draft.code.as_deref() {
            None => entry_warnings.push(ChtEntryWarning {
                kind: ChtEntryWarningKind::MissingCode,
                detail: format!("cheat{index} has no cheat{index}_code key"),
            }),
            Some("") => entry_warnings.push(ChtEntryWarning {
                kind: ChtEntryWarningKind::EmptyCode,
                detail: format!("cheat{index}_code is empty"),
            }),
            Some(_) => {}
        }
        if draft
            .description
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            entry_warnings.push(ChtEntryWarning {
                kind: ChtEntryWarningKind::MissingDescription,
                detail: format!("cheat{index} has no usable cheat{index}_desc key"),
            });
        }
        entries.push(ChtEntry {
            index,
            description: draft.description,
            code: draft.code,
            enabled_by_default: draft.enable.unwrap_or(false),
            extra_fields: draft.extra_fields,
            warnings: entry_warnings,
        });
    }

    if let Some(count) = declared_count
        && count as usize != entries.len()
    {
        push_warning(
            &mut warnings,
            ChtDocumentWarningKind::DeclaredCountMismatch,
            None,
            format!(
                "file declares cheats = {count} but {} distinct entries were parsed",
                entries.len()
            ),
        );
    }
    if entries
        .iter()
        .enumerate()
        .any(|(position, entry)| u32::try_from(position).unwrap_or(u32::MAX) != entry.index)
    {
        push_warning(
            &mut warnings,
            ChtDocumentWarningKind::NonContiguousIndexes,
            None,
            "declared entry indexes are not contiguous from zero; the installed file is renumbered"
                .to_string(),
        );
    }

    Ok(ChtDocument {
        declared_count,
        entries,
        preserved_comments,
        global_fields,
        warnings,
    })
}

fn set_once(
    slot: &mut Option<String>,
    value: String,
    field: &str,
    index: u32,
    warnings: &mut Vec<ChtEntryWarning>,
) {
    if slot.is_some() {
        warnings.push(ChtEntryWarning {
            kind: ChtEntryWarningKind::DuplicateField,
            detail: format!(
                "cheat{index}_{field} appeared more than once; the first value is kept"
            ),
        });
        return;
    }
    *slot = Some(value);
}

/// Unquotes and bounds one raw value, reporting anything that makes it
/// unsafe to write back out.
///
/// RetroArch's own `config_file` reader has no escape syntax inside a
/// quoted value, so this deliberately does *not* invent one: a `\"`
/// sequence is decoded (it is what some third-party generators emit) but a
/// bare interior quote is only flagged, never used to end the value early.
fn decode_value(raw: &str) -> (String, Vec<ChtEntryWarning>) {
    let mut warnings = Vec::new();
    let unquoted = match raw.strip_prefix('"') {
        Some(rest) => rest.strip_suffix('"').unwrap_or(rest),
        None => raw,
    };

    let mut decoded = String::with_capacity(unquoted.len());
    let mut characters = unquoted.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next() {
                Some('"') => decoded.push('"'),
                Some('\\') => decoded.push('\\'),
                Some('n') => decoded.push('\n'),
                Some(other) => {
                    decoded.push('\\');
                    decoded.push(other);
                }
                None => decoded.push('\\'),
            }
            continue;
        }
        decoded.push(character);
    }

    if decoded
        .chars()
        .any(|character| character.is_control() && character != '\t' || character == '\u{0}')
    {
        warnings.push(ChtEntryWarning {
            kind: ChtEntryWarningKind::ControlCharacter,
            detail:
                "value contains a control character that cannot appear in a RetroArch config value"
                    .to_string(),
        });
    }
    if decoded.contains('"') {
        warnings.push(ChtEntryWarning {
            kind: ChtEntryWarningKind::QuoteNormalized,
            detail: "value contains a double quote; it is written as an apostrophe because \
                     RetroArch config values have no escape syntax"
                .to_string(),
        });
    }
    if decoded.len() > MAX_CHT_FIELD_BYTES {
        let mut boundary = MAX_CHT_FIELD_BYTES;
        while boundary > 0 && !decoded.is_char_boundary(boundary) {
            boundary -= 1;
        }
        decoded.truncate(boundary);
        warnings.push(ChtEntryWarning {
            kind: ChtEntryWarningKind::OversizedField,
            detail: format!("value exceeded {MAX_CHT_FIELD_BYTES} bytes and was truncated"),
        });
    }

    (decoded, warnings)
}

// ---------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------

/// One cheat as it will appear in the installed file. Built from a
/// [`ChtEntry`] the user selected, never from a whole document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChtInstallEntry {
    pub description: String,
    pub code: String,
    /// Whether RetroArch should have this cheat *active* on load, as
    /// opposed to merely present in the file.
    pub enabled: bool,
    /// Preserved `cheatN_<field>` pairs carried through from the source.
    pub extra_fields: Vec<(String, String)>,
}

impl ChtInstallEntry {
    /// Builds an installable entry from a parsed one. Returns `None` for an
    /// entry [`ChtEntry::is_selectable`] rejects, so an unsafe entry can
    /// never reach the renderer even if a caller mis-tracks its own
    /// selection state.
    #[must_use]
    pub fn from_entry(entry: &ChtEntry, enabled: bool) -> Option<Self> {
        if !entry.is_selectable() {
            return None;
        }
        Some(Self {
            description: entry.effective_description(),
            code: entry.code.clone()?,
            enabled,
            extra_fields: entry.extra_fields.clone(),
        })
    }
}

/// Renders a complete, RetroArch-loadable `.cht` file.
///
/// Deterministic: the same slice always produces the same bytes. Output
/// indexes are contiguous from zero regardless of the source indexes, the
/// `cheats = N` header always agrees with the number of entries written,
/// and the file always ends with exactly one newline.
///
/// `header_comments` are written as `#` lines before the header. They are
/// the caller's provenance note plus (bounded) source comments; nothing
/// time-varying belongs there, or determinism is lost.
#[must_use]
pub fn render_cht_file(entries: &[ChtInstallEntry], header_comments: &[String]) -> String {
    let mut output = String::new();
    for comment in header_comments.iter().take(MAX_CHT_PRESERVED_COMMENTS) {
        let sanitized: String = comment
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        output.push_str("# ");
        output.push_str(sanitized.trim());
        output.push('\n');
    }
    if !header_comments.is_empty() {
        output.push('\n');
    }

    output.push_str(&format!("cheats = {}\n", entries.len()));
    for (position, entry) in entries.iter().enumerate() {
        output.push('\n');
        output.push_str(&format!(
            "cheat{position}_desc = \"{}\"\n",
            escape_config_value(&entry.description)
        ));
        output.push_str(&format!(
            "cheat{position}_code = \"{}\"\n",
            escape_config_value(&entry.code)
        ));
        output.push_str(&format!(
            "cheat{position}_enable = {}\n",
            if entry.enabled { "true" } else { "false" }
        ));
        for (field, value) in &entry.extra_fields {
            let field: String = field
                .chars()
                .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect();
            if field.is_empty() {
                continue;
            }
            output.push_str(&format!(
                "cheat{position}_{field} = \"{}\"\n",
                escape_config_value(value)
            ));
        }
    }
    output
}

/// Makes one value safe to place inside a double-quoted RetroArch config
/// value. See the module docs for why a quote becomes an apostrophe rather
/// than a backslash escape.
fn escape_config_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| if character == '"' { '\'' } else { character })
        .collect()
}

#[cfg(test)]
mod tests;
