//! Full-fidelity, panic-free parsing and in-place editing of Dolphin
//! GameSettings INI files, for the Gecko cheat-code section specifically.
//!
//! ## Why this exists alongside `dolphin_local`
//!
//! `dolphin_local::inspect_dolphin_profile` already parses every
//! GameSettings INI in a profile - but only far enough to answer "which
//! code *names* exist, and which are enabled?" (`DolphinGameIniFile`'s
//! `gecko_names`/`enabled_gecko_names`). It deliberately never retains a
//! code's hex lines, matching the same metadata-only precedent
//! `cheat_catalogue`/`retroarch_inventory` established for RetroArch.
//! Selecting specific codes to enable requires the actual lines (for the
//! preview and for anyone auditing what was written), so this module is
//! the first Dolphin reader that keeps them.
//!
//! ## Why this can't just generate a new file (unlike the RetroArch path)
//!
//! A RetroArch cheat install writes a whole new, small file to its own
//! destination. A Dolphin GameSettings INI is a *shared* file: the same
//! `<GameID>.ini` also carries `[Core]` overclock settings, `[Video_*]`
//! tweaks, and - critically - the full `[Gecko]`/`[ActionReplay]` code
//! bodies themselves, whether or not any of them are enabled. Installing a
//! selection here means enabling some of the game's *own* existing codes,
//! not introducing a new file, so the write has to be a surgical, in-place
//! edit of one section ([`replace_gecko_enabled_section`]) that leaves
//! every other byte of the file - including sections this module does not
//! understand - untouched.
//!
//! ## Guarantees
//!
//! - **Never panics on catalogue input.** Bounded reads, no unchecked
//!   slicing.
//! - **Never mutates the source.** [`parse_dolphin_ini`] is a pure
//!   function of its input text.
//! - **Deterministic, minimal rewrite.**
//!   [`replace_gecko_enabled_section`] changes only the `[Gecko_Enabled]`
//!   section's own lines; every other section (including `[Gecko]` itself)
//!   is reproduced byte-for-byte from the original document, in its
//!   original order. Section order is otherwise never a Dolphin-meaningful
//!   concern (Dolphin's own INI loader is not order-sensitive), so a newly
//!   added `[Gecko_Enabled]` section is appended at the end rather than
//!   guessing a "correct" position.

use serde::Serialize;

/// Bounds mirrored from `dolphin_local` so this reader never exceeds what
/// the metadata-only inspector already accepts for the same file.
pub const MAX_GECKO_CODES: usize = 4_096;
pub const MAX_GECKO_CODE_LINES: usize = 4_096;
pub const MAX_GECKO_LINE_BYTES: usize = 4 * 1024;

/// One Gecko code's own warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeckoCodeWarningKind {
    /// A body line is not a `XXXXXXXX YYYYYYYY` hex pair. Blocking: the
    /// retained text is not confirmed to be a real code line.
    MalformedLine,
    /// The code has a `$Name` header but no body lines at all. Blocking.
    EmptyCode,
    /// The code name is empty after the `$`. Blocking.
    MissingName,
}

impl GeckoCodeWarningKind {
    #[must_use]
    pub fn is_blocking(self) -> bool {
        true
    }

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedLine => "gecko_code_malformed_line",
            Self::EmptyCode => "gecko_code_empty",
            Self::MissingName => "gecko_code_missing_name",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeckoCodeWarning {
    pub kind: GeckoCodeWarningKind,
    pub detail: String,
}

/// One parsed Gecko code, in the exact order it appeared in `[Gecko]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeckoCode {
    /// Everything after `$` on the header line, up to a `=` or tab -
    /// matches `dolphin_local`'s own name-extraction exactly, so a code
    /// selected here corresponds 1:1 to the same name that module already
    /// lists in `gecko_names`/`enabled_gecko_names`. Conventionally
    /// `"Display Name [Author]"`, but never parsed apart - Dolphin itself
    /// does not split the two.
    pub name: String,
    /// Raw `XXXXXXXX YYYYYYYY` hex-pair lines, verbatim.
    pub lines: Vec<String>,
    /// `*Note` lines immediately following the code, verbatim minus the
    /// leading `*`.
    pub notes: Vec<String>,
    pub enabled_by_default: bool,
    pub warnings: Vec<GeckoCodeWarning>,
}

impl GeckoCode {
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        !self.lines.is_empty()
            && !self
                .warnings
                .iter()
                .any(|warning| warning.kind.is_blocking())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinIniWarningKind {
    /// A `[Section` line with no closing `]`.
    MalformedSectionHeader,
    /// More than [`MAX_GECKO_CODES`] `$` headers under `[Gecko]`.
    TooManyCodes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinIniWarning {
    pub kind: DolphinIniWarningKind,
    pub line: Option<u32>,
    pub detail: String,
}

/// One INI section, preserved exactly as read (its raw body text,
/// including original line endings within the body) so it can be written
/// back byte-for-byte untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IniSection {
    /// Exact original header text between `[` and `]`.
    name: String,
    /// Exact original body lines (not including the header line itself).
    raw_lines: Vec<String>,
}

/// A parsed Dolphin GameSettings INI: every section in original order,
/// plus - when present - the `[Gecko]`/`[ActionReplay]` sections
/// additionally parsed into individual codes. Both sections share the
/// exact same `$Name` + hex-pair-lines + `*Note` body shape (confirmed
/// against Dolphin's own source - see `GameCubeCodeFormat`'s doc comment
/// in `gamehacking_gamecube_provider.rs`), so `GeckoCode` is reused
/// structurally for both; only which section a code came from
/// distinguishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinIniDocument {
    sections: Vec<IniSection>,
    /// Codes from `[Gecko]`, in catalogue order. Empty if the file has no
    /// `[Gecko]` section.
    pub gecko_codes: Vec<GeckoCode>,
    /// Names already enabled in the file's own `[Gecko_Enabled]` section
    /// (verbatim, not cross-checked against `gecko_codes` - a name here
    /// may reference a code from Dolphin's *bundled* database that this
    /// file's own `[Gecko]` section never defines, exactly like the real
    /// `[ActionReplay_Enabled]`-only files this format allows).
    pub gecko_enabled_names: Vec<String>,
    /// Codes from `[ActionReplay]`, in catalogue order. Empty if the file
    /// has no `[ActionReplay]` section.
    pub action_replay_codes: Vec<GeckoCode>,
    /// Names already enabled in the file's own `[ActionReplay_Enabled]`
    /// section, exactly as `gecko_enabled_names` documents for Gecko.
    pub action_replay_enabled_names: Vec<String>,
    pub warnings: Vec<DolphinIniWarning>,
}

impl DolphinIniDocument {
    #[must_use]
    pub fn selectable_gecko_count(&self) -> usize {
        self.gecko_codes
            .iter()
            .filter(|code| code.is_selectable())
            .count()
    }

    #[must_use]
    pub fn has_gecko_section(&self) -> bool {
        self.sections.iter().any(|section| is_gecko(&section.name))
    }

    #[must_use]
    pub fn section_names(&self) -> Vec<String> {
        self.sections
            .iter()
            .filter(|section| !section.name.is_empty())
            .map(|section| section.name.clone())
            .collect()
    }

    /// The exact raw body lines of an arbitrary named section (any
    /// section this module doesn't otherwise specifically parse,
    /// including a caller-defined bookkeeping section), or an empty
    /// `Vec` if it doesn't exist. Read-only counterpart to
    /// [`replace_named_section`].
    #[must_use]
    pub fn named_section_lines(&self, section_name: &str) -> Vec<String> {
        self.sections
            .iter()
            .find(|section| section.name.eq_ignore_ascii_case(section_name))
            .map(|section| section.raw_lines.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeckoMergeError {
    pub code_name: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for GeckoMergeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GeckoMergeError {}

fn is_gecko(name: &str) -> bool {
    name.eq_ignore_ascii_case("gecko")
}

fn is_gecko_enabled(name: &str) -> bool {
    name.eq_ignore_ascii_case("gecko_enabled")
}

fn is_action_replay(name: &str) -> bool {
    name.eq_ignore_ascii_case("actionreplay")
}

fn is_action_replay_enabled(name: &str) -> bool {
    name.eq_ignore_ascii_case("actionreplay_enabled")
}

/// Parses a Dolphin GameSettings INI's full text. Never panics or fails:
/// unparseable content becomes a warning, and the rest of the file - every
/// section this module does not specifically understand - is retained
/// verbatim for later exact reproduction.
#[must_use]
pub fn parse_dolphin_ini(text: &str) -> DolphinIniDocument {
    let mut sections: Vec<IniSection> = Vec::new();
    let mut warnings: Vec<DolphinIniWarning> = Vec::new();
    let mut current: Option<IniSection> = None;

    for (offset, raw_line) in text.lines().enumerate() {
        let line_number = u32::try_from(offset + 1).unwrap_or(u32::MAX);
        let trimmed = raw_line.trim_end_matches('\r');
        if trimmed.trim_start().starts_with('[') {
            if !trimmed.trim_end().ends_with(']') {
                warnings.push(DolphinIniWarning {
                    kind: DolphinIniWarningKind::MalformedSectionHeader,
                    line: Some(line_number),
                    detail: format!("line {line_number}: section header has no closing ']'"),
                });
                if let Some(section) = current.take() {
                    sections.push(section);
                }
                current = None;
                continue;
            }
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let inner = trimmed.trim_start().trim_start_matches('[');
            let name = inner[..inner.len() - 1].to_string();
            current = Some(IniSection {
                name,
                raw_lines: Vec::new(),
            });
            continue;
        }
        match &mut current {
            Some(section) => section.raw_lines.push(raw_line.to_string()),
            None => {
                // Content before any `[Section]` header - Dolphin itself
                // would ignore this too, but it is still preserved as an
                // unnamed leading section so round-tripping stays exact.
                let section = current.get_or_insert(IniSection {
                    name: String::new(),
                    raw_lines: Vec::new(),
                });
                section.raw_lines.push(raw_line.to_string());
            }
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }

    let gecko_enabled_names = sections
        .iter()
        .find(|section| is_gecko_enabled(&section.name))
        .map(|section| extract_names(&section.raw_lines))
        .unwrap_or_default();

    let (gecko_codes, code_warnings) = sections
        .iter()
        .find(|section| is_gecko(&section.name))
        .map(|section| parse_gecko_codes(&section.raw_lines, "Gecko"))
        .unwrap_or_default();
    warnings.extend(code_warnings);

    let action_replay_enabled_names = sections
        .iter()
        .find(|section| is_action_replay_enabled(&section.name))
        .map(|section| extract_names(&section.raw_lines))
        .unwrap_or_default();

    let (action_replay_codes, ar_code_warnings) = sections
        .iter()
        .find(|section| is_action_replay(&section.name))
        .map(|section| parse_gecko_codes(&section.raw_lines, "ActionReplay"))
        .unwrap_or_default();
    warnings.extend(ar_code_warnings);

    DolphinIniDocument {
        sections,
        gecko_codes,
        gecko_enabled_names,
        action_replay_codes,
        action_replay_enabled_names,
        warnings,
    }
}

/// Extracts `$Name` values from an already-isolated section body, using
/// the exact same name-extraction convention `dolphin_local` uses
/// (`line[1..]` up to the first `=` or tab), so names line up exactly with
/// what the metadata-only inspector already reports.
fn extract_names(raw_lines: &[String]) -> Vec<String> {
    raw_lines
        .iter()
        .filter_map(|raw| {
            let line = raw.trim();
            let rest = line.strip_prefix('$')?;
            let name = rest.split(['=', '\t']).next().unwrap_or_default().trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

fn parse_gecko_codes(
    raw_lines: &[String],
    section_label: &str,
) -> (Vec<GeckoCode>, Vec<DolphinIniWarning>) {
    let mut codes: Vec<GeckoCode> = Vec::new();
    let mut warnings: Vec<DolphinIniWarning> = Vec::new();
    let mut current: Option<GeckoCode> = None;

    let finish = |current: Option<GeckoCode>, codes: &mut Vec<GeckoCode>| {
        if let Some(mut code) = current {
            if code.name.is_empty() {
                code.warnings.push(GeckoCodeWarning {
                    kind: GeckoCodeWarningKind::MissingName,
                    detail: "code header has no name after '$'".to_string(),
                });
            } else if code.lines.is_empty() {
                code.warnings.push(GeckoCodeWarning {
                    kind: GeckoCodeWarningKind::EmptyCode,
                    detail: format!("code {:?} has no hex code lines", code.name),
                });
            }
            codes.push(code);
        }
    };

    for raw in raw_lines {
        if codes.len() >= MAX_GECKO_CODES {
            warnings.push(DolphinIniWarning {
                kind: DolphinIniWarningKind::TooManyCodes,
                line: None,
                detail: format!(
                    "more than {MAX_GECKO_CODES} {section_label} codes; later codes ignored"
                ),
            });
            break;
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('$') {
            finish(current.take(), &mut codes);
            let name = rest.split(['=', '\t']).next().unwrap_or_default().trim();
            current = Some(GeckoCode {
                name: name.to_string(),
                lines: Vec::new(),
                notes: Vec::new(),
                enabled_by_default: false,
                warnings: Vec::new(),
            });
            continue;
        }
        let Some(code) = current.as_mut() else {
            continue;
        };
        if let Some(note) = line.strip_prefix('*') {
            code.notes.push(note.trim().to_string());
            continue;
        }
        if code.lines.len() >= MAX_GECKO_CODE_LINES {
            continue;
        }
        if !is_gecko_code_line(line) {
            code.warnings.push(GeckoCodeWarning {
                kind: GeckoCodeWarningKind::MalformedLine,
                detail: format!("{line:?} is not a valid 'XXXXXXXX YYYYYYYY' code line"),
            });
            continue;
        }
        if line.len() > MAX_GECKO_LINE_BYTES {
            code.warnings.push(GeckoCodeWarning {
                kind: GeckoCodeWarningKind::MalformedLine,
                detail: "code line exceeds the maximum supported length".to_string(),
            });
            continue;
        }
        code.lines.push(line.to_string());
    }
    finish(current, &mut codes);
    (codes, warnings)
}

/// A real Gecko code line is two 8-digit hexadecimal groups separated by
/// one space - e.g. `28134C58 00000001`. Checked structurally so a
/// malformed line is reported rather than silently accepted as a code
/// line and later written back out unmodified but unverified.
pub(crate) fn is_gecko_code_line(line: &str) -> bool {
    let Some((first, second)) = line.split_once(' ') else {
        return false;
    };
    is_hex8(first) && is_hex8(second.trim())
}

fn is_hex8(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Renders `[Gecko_Enabled]\n$Name\n...` for exactly the given names, in
/// the order given, one per line - the format Dolphin itself writes.
#[must_use]
fn render_gecko_enabled_body(names: &[String]) -> Vec<String> {
    names.iter().map(|name| format!("${name}")).collect()
}

/// Rewrites `document`'s source text so that `[Gecko_Enabled]` contains
/// exactly `enabled_names` (in the given order) and nothing else changes:
/// every other section - including `[Gecko]` itself - is reproduced
/// byte-for-byte from the original parse, in its original order. If no
/// `[Gecko_Enabled]` section existed, one is appended at the end.
///
/// This is the only place this module writes anything, and it never
/// touches a file directly - it returns text for the caller to stage and
/// apply through the existing transaction machinery.
#[must_use]
pub fn replace_gecko_enabled_section(
    document: &DolphinIniDocument,
    enabled_names: &[String],
) -> String {
    let mut sections = document.sections.clone();
    let new_body = render_gecko_enabled_body(enabled_names);
    match sections
        .iter_mut()
        .find(|section| is_gecko_enabled(&section.name))
    {
        Some(section) => section.raw_lines = new_body,
        None => sections.push(IniSection {
            name: "Gecko_Enabled".to_string(),
            raw_lines: new_body,
        }),
    }
    render_sections(&sections)
}

/// Adds selected external definitions and updates only their enabled state. Existing unrelated
/// settings, Gecko definitions, and enabled names are preserved. A provider name that already
/// exists with a different body is an explicit conflict; it is never overwritten or duplicated.
pub fn merge_external_gecko_codes(
    document: &DolphinIniDocument,
    provider_codes: &[GeckoCode],
    selected_names: &[String],
) -> Result<String, GeckoMergeError> {
    let provider_names: std::collections::BTreeSet<&str> = provider_codes
        .iter()
        .map(|code| code.name.as_str())
        .collect();
    let selected: std::collections::BTreeSet<&str> =
        selected_names.iter().map(String::as_str).collect();
    if selected.is_empty() {
        return Err(GeckoMergeError {
            code_name: None,
            detail: "at least one external Gecko code must be selected".to_string(),
        });
    }
    if let Some(unknown) = selected
        .iter()
        .find(|name| !provider_names.contains(**name))
    {
        return Err(GeckoMergeError {
            code_name: Some((*unknown).to_string()),
            detail: format!("selected Gecko code {unknown:?} is not in the provider result"),
        });
    }

    let mut additions = Vec::new();
    for code in provider_codes
        .iter()
        .filter(|code| selected.contains(code.name.as_str()))
    {
        if !code.is_selectable() {
            return Err(GeckoMergeError {
                code_name: Some(code.name.clone()),
                detail: format!(
                    "Gecko code {:?} is malformed and cannot be merged",
                    code.name
                ),
            });
        }
        match document
            .gecko_codes
            .iter()
            .find(|existing| existing.name == code.name)
        {
            Some(existing) if existing.lines == code.lines => {}
            Some(_) => {
                return Err(GeckoMergeError {
                    code_name: Some(code.name.clone()),
                    detail: format!(
                        "an existing Gecko code named {:?} has a different body; ArchiveFS will not overwrite it",
                        code.name
                    ),
                });
            }
            None => additions.push(code.clone()),
        }
    }

    let mut sections = document.sections.clone();
    if !additions.is_empty() {
        let gecko = match sections.iter_mut().find(|section| is_gecko(&section.name)) {
            Some(section) => section,
            None => {
                sections.push(IniSection {
                    name: "Gecko".to_string(),
                    raw_lines: Vec::new(),
                });
                sections.last_mut().expect("just inserted Gecko section")
            }
        };
        if !gecko.raw_lines.is_empty()
            && gecko.raw_lines.last().is_some_and(|line| !line.is_empty())
        {
            gecko.raw_lines.push(String::new());
        }
        for code in additions {
            gecko.raw_lines.push(format!("${}", code.name));
            gecko.raw_lines.extend(code.lines);
            gecko
                .raw_lines
                .extend(code.notes.into_iter().map(|note| format!("*{note}")));
        }
    }

    let mut enabled: Vec<String> = document
        .gecko_enabled_names
        .iter()
        .filter(|name| !provider_names.contains(name.as_str()))
        .cloned()
        .collect();
    for name in selected_names {
        if !enabled.contains(name) {
            enabled.push(name.clone());
        }
    }
    let new_body = render_gecko_enabled_body(&enabled);
    match sections
        .iter_mut()
        .find(|section| is_gecko_enabled(&section.name))
    {
        Some(section) => section.raw_lines = new_body,
        None => sections.push(IniSection {
            name: "Gecko_Enabled".to_string(),
            raw_lines: new_body,
        }),
    }
    Ok(render_sections(&sections))
}

/// Exactly `merge_external_gecko_codes`, targeting `[ActionReplay]`/
/// `[ActionReplay_Enabled]` instead of `[Gecko]`/`[Gecko_Enabled]`. Kept
/// as a separate function rather than a generalized one so each format's
/// well-tested behavior can never accidentally regress the other.
pub fn merge_external_action_replay_codes(
    document: &DolphinIniDocument,
    provider_codes: &[GeckoCode],
    selected_names: &[String],
) -> Result<String, GeckoMergeError> {
    let provider_names: std::collections::BTreeSet<&str> = provider_codes
        .iter()
        .map(|code| code.name.as_str())
        .collect();
    let selected: std::collections::BTreeSet<&str> =
        selected_names.iter().map(String::as_str).collect();
    if selected.is_empty() {
        return Err(GeckoMergeError {
            code_name: None,
            detail: "at least one external Action Replay code must be selected".to_string(),
        });
    }
    if let Some(unknown) = selected
        .iter()
        .find(|name| !provider_names.contains(**name))
    {
        return Err(GeckoMergeError {
            code_name: Some((*unknown).to_string()),
            detail: format!(
                "selected Action Replay code {unknown:?} is not in the provider result"
            ),
        });
    }

    let mut additions = Vec::new();
    for code in provider_codes
        .iter()
        .filter(|code| selected.contains(code.name.as_str()))
    {
        if !code.is_selectable() {
            return Err(GeckoMergeError {
                code_name: Some(code.name.clone()),
                detail: format!(
                    "Action Replay code {:?} is malformed and cannot be merged",
                    code.name
                ),
            });
        }
        match document
            .action_replay_codes
            .iter()
            .find(|existing| existing.name == code.name)
        {
            Some(existing) if existing.lines == code.lines => {}
            Some(_) => {
                return Err(GeckoMergeError {
                    code_name: Some(code.name.clone()),
                    detail: format!(
                        "an existing Action Replay code named {:?} has a different body; ArchiveFS will not overwrite it",
                        code.name
                    ),
                });
            }
            None => additions.push(code.clone()),
        }
    }

    let mut sections = document.sections.clone();
    if !additions.is_empty() {
        let action_replay = match sections
            .iter_mut()
            .find(|section| is_action_replay(&section.name))
        {
            Some(section) => section,
            None => {
                sections.push(IniSection {
                    name: "ActionReplay".to_string(),
                    raw_lines: Vec::new(),
                });
                sections
                    .last_mut()
                    .expect("just inserted ActionReplay section")
            }
        };
        if !action_replay.raw_lines.is_empty()
            && action_replay
                .raw_lines
                .last()
                .is_some_and(|line| !line.is_empty())
        {
            action_replay.raw_lines.push(String::new());
        }
        for code in additions {
            action_replay.raw_lines.push(format!("${}", code.name));
            action_replay.raw_lines.extend(code.lines);
            action_replay
                .raw_lines
                .extend(code.notes.into_iter().map(|note| format!("*{note}")));
        }
    }

    let mut enabled: Vec<String> = document
        .action_replay_enabled_names
        .iter()
        .filter(|name| !provider_names.contains(name.as_str()))
        .cloned()
        .collect();
    for name in selected_names {
        if !enabled.contains(name) {
            enabled.push(name.clone());
        }
    }
    let new_body = render_gecko_enabled_body(&enabled);
    match sections
        .iter_mut()
        .find(|section| is_action_replay_enabled(&section.name))
    {
        Some(section) => section.raw_lines = new_body,
        None => sections.push(IniSection {
            name: "ActionReplay_Enabled".to_string(),
            raw_lines: new_body,
        }),
    }
    Ok(render_sections(&sections))
}

/// Which of Dolphin's two identically-shaped cheat-code section pairs an
/// operation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DolphinCodeSectionKind {
    Gecko,
    ActionReplay,
}

/// Removes exactly the named code blocks (`$Name` header through its
/// hex/note lines, up to but not including the next `$` header or the
/// section's end) from the given section kind's body, and removes the
/// same names from its `_Enabled` list. Every other section - including
/// any code in that body whose name isn't in `names`, and the sibling
/// Gecko/ActionReplay section entirely - is reproduced byte-for-byte.
/// Removing a name that doesn't exist in this section at all is a safe
/// no-op for that name (the caller is expected to have already confirmed
/// which names it actually manages before calling this).
#[must_use]
pub fn remove_named_codes(
    document: &DolphinIniDocument,
    kind: DolphinCodeSectionKind,
    names: &[String],
) -> String {
    type SectionPredicate = fn(&str) -> bool;
    let (is_body, is_enabled): (SectionPredicate, SectionPredicate) = match kind {
        DolphinCodeSectionKind::Gecko => (is_gecko, is_gecko_enabled),
        DolphinCodeSectionKind::ActionReplay => (is_action_replay, is_action_replay_enabled),
    };
    let removed: std::collections::BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut sections = document.sections.clone();
    if let Some(section) = sections.iter_mut().find(|section| is_body(&section.name)) {
        let mut kept = Vec::with_capacity(section.raw_lines.len());
        let mut skipping = false;
        for raw in &section.raw_lines {
            let trimmed = raw.trim();
            if let Some(rest) = trimmed.strip_prefix('$') {
                let name = rest.split(['=', '\t']).next().unwrap_or_default().trim();
                skipping = removed.contains(name);
                if skipping {
                    continue;
                }
            } else if skipping {
                continue;
            }
            kept.push(raw.clone());
        }
        section.raw_lines = kept;
    }
    if let Some(section) = sections
        .iter_mut()
        .find(|section| is_enabled(&section.name))
    {
        section.raw_lines.retain(|raw| {
            let trimmed = raw.trim();
            let Some(name) = trimmed.strip_prefix('$') else {
                return true;
            };
            !removed.contains(name)
        });
    }
    render_sections(&sections)
}

/// Rewrites an arbitrary named section's raw body to exactly `lines`,
/// preserving every other section byte-for-byte (including `[Gecko]`/
/// `[ActionReplay]` and their own `_Enabled` lists). If the section
/// doesn't already exist, one is appended at the end, matching every
/// other section-insertion rule in this module. Intended for a caller's
/// own bookkeeping section (e.g. tracking which code names it manages)
/// that this module has no opinion about the contents of.
#[must_use]
pub fn replace_named_section(
    document: &DolphinIniDocument,
    section_name: &str,
    lines: Vec<String>,
) -> String {
    let mut sections = document.sections.clone();
    match sections
        .iter_mut()
        .find(|section| section.name.eq_ignore_ascii_case(section_name))
    {
        Some(section) => section.raw_lines = lines,
        None => sections.push(IniSection {
            name: section_name.to_string(),
            raw_lines: lines,
        }),
    }
    render_sections(&sections)
}

fn render_sections(sections: &[IniSection]) -> String {
    let mut output = String::new();
    for section in sections {
        if !section.name.is_empty() {
            output.push('[');
            output.push_str(&section.name);
            output.push_str("]\n");
        }
        for line in &section.raw_lines {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests;
