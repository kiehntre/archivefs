//! A persistent registry of local DAT sources.
//!
//! Until now a DAT was a path typed on the command line: parsed once, reported
//! on, and forgotten (`dat/parsers/mod.rs` documents that deliberate CLI
//! exception). This module is the other half - a place to *keep* a DAT source,
//! so it can be named, enabled, assigned a platform, validated, and audited
//! from the GUI without retyping a path every time.
//!
//! # Separate from cheat sources, on purpose
//!
//! DAT sources live in their own file (`~/.config/archivefs/dat_sources.toml`)
//! and their own priority space. The two answer different questions - "which
//! catalogue is authoritative for this game's *identity*?" versus "which
//! catalogue provides its *cheats*?" - so a single ordering would be
//! meaningless, and a single file would have to be read by every build that
//! only understands one of them.
//!
//! Sharing `cheat_sources.toml` was considered and rejected: that file is
//! `deny_unknown_fields`, so adding any key to it makes it unreadable by every
//! already-released binary. A new file has no such constraint, and this one is
//! built from the start to keep what it does not understand (see
//! [`config`]).
//!
//! # What is policy and what is not
//!
//! Everything a user chose - the ID, the path, enabled state, platform,
//! priority - is policy and is persisted. Health is *runtime state*: it records
//! what a validation run observed. It is persisted too, because re-parsing
//! every registered DAT on page open would be worse, but it is always presented
//! with the time it was taken and, for file sources, with a marker when the file
//! has changed since (see [`DatSourceHealth::is_stale_for`]).
//!
//! # Nothing here writes to a DAT or a ROM
//!
//! This module reads DAT files and writes exactly one thing: its own
//! preferences file, through the shared durable-write path. Removing a source
//! removes a registry entry and nothing else.

pub mod audit_run;
pub mod config;
pub mod validation;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use config::{
    DatSourceConfigEntry, DatSourcesConfig, default_dat_sources_config_path,
    load_dat_sources_config_default, load_dat_sources_config_from, save_dat_sources_config_default,
    save_dat_sources_config_to,
};
pub use crate::dat::policy::{DatPolicyConfig, default_dat_policy};
pub use validation::{
    DatDiagnostic, DatFileOutcome, DatFileReport, DatPathRefusal, DatValidationReport,
    DuplicateDatIdentity, MAX_FOLDER_DAT_FILES, SkippedFolderEntry, discover_dat_files,
    validate_dat_path, validate_dat_source,
};

/// What a registered path is: one DAT file, or a folder holding several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatSourceKind {
    File,
    Folder,
}

impl DatSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::File => "DAT file",
            Self::Folder => "DAT folder",
        }
    }
}

/// A new DAT source's priority.
///
/// DAT priority is *platform-local*: it is only ever compared against other DAT
/// sources that are relevant to the same platform, and never against a cheat
/// source. So the absolute number carries no cross-space meaning - it only has
/// to leave room to move a source earlier or later, which `100` does. A user
/// with one DAT source per platform never needs to touch it, which is why
/// Stage 1 persists the field but does not offer an editor for it.
pub const DEFAULT_DAT_PRIORITY: u32 = 100;

/// The accepted priority range, matching the cheat space's range so a
/// hand-edited value behaves the same way in either file.
pub const MIN_DAT_PRIORITY: u32 = 1;
pub const MAX_DAT_PRIORITY: u32 = 999;

/// The longest a source ID may be, in bytes.
pub const MAX_SOURCE_ID_BYTES: usize = 128;

/// The longest a display name may be, in bytes.
///
/// A name is derived from a filename by default, and a filename can be as long
/// as the filesystem allows; this stops one turning into an unbounded string in
/// the preferences file.
pub const MAX_DISPLAY_NAME_BYTES: usize = 256;

/// What a validation run last observed about a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatHealthState {
    /// Never validated by this build. Distinct from "checked and fine", and
    /// must never be rendered as either healthy or failed.
    NotChecked,
    /// Everything registered parsed cleanly.
    Valid,
    /// Everything parsed, but the parser had something to say about it.
    ValidWithWarnings,
    /// At least one registered DAT could not be parsed.
    Invalid,
    /// The path itself is gone, or is no longer the kind of thing registered.
    Unreadable,
}

impl DatHealthState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChecked => "Not checked",
            Self::Valid => "Valid",
            Self::ValidWithWarnings => "Valid, with warnings",
            Self::Invalid => "Invalid",
            Self::Unreadable => "Path unreadable",
        }
    }

    /// Whether this state was produced by actually looking at the source.
    pub fn is_checked(self) -> bool {
        !matches!(self, Self::NotChecked)
    }
}

/// The result of the last validation run, as persisted.
///
/// `observed_size_bytes` / `observed_modified_unix_seconds` describe the DAT
/// file at the moment it was validated. They exist so a stored verdict can be
/// shown as *stale* rather than as current when the file has changed
/// underneath - a health record that silently keeps claiming "Valid" for a file
/// that has since been replaced is worse than no record at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatSourceHealth {
    pub state: Option<DatHealthState>,
    pub last_validated_unix_seconds: Option<u64>,
    pub detail: Option<String>,
    pub entry_count: Option<u64>,
    pub rom_count: Option<u64>,
    /// How many DAT files a folder source contributed. `None` for a file
    /// source, where the answer is always one.
    pub file_count: Option<u64>,
    /// Formats seen, as their stable serialised names, deduplicated and sorted.
    pub formats: Option<Vec<String>>,
    pub observed_size_bytes: Option<u64>,
    pub observed_modified_unix_seconds: Option<i64>,
}

impl DatSourceHealth {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The recorded state, or `NotChecked` when nothing was recorded.
    pub fn state(&self) -> DatHealthState {
        self.state.unwrap_or(DatHealthState::NotChecked)
    }

    /// Whether the file this verdict describes has changed since.
    ///
    /// Only answerable for a file source, and only when a size/mtime pair was
    /// recorded. A folder source returns `false`: its contents can change in
    /// ways one fingerprint cannot describe, so the honest presentation there
    /// is the timestamp alone rather than a staleness claim that would be wrong
    /// as often as it was right.
    pub fn is_stale_for(&self, path: &Path, kind: DatSourceKind) -> bool {
        if kind != DatSourceKind::File {
            return false;
        }
        let Some(recorded_size) = self.observed_size_bytes else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(path) else {
            // Gone or unreadable. Reported as unreadable by the next
            // validation; not called stale here, because "stale" implies the
            // file is still there.
            return false;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_secs() as i64);
        metadata.len() != recorded_size || modified != self.observed_modified_unix_seconds
    }
}

/// One registered DAT source.
///
/// `platform` holds whatever the user assigned, canonicalised where this build
/// can canonicalise it and kept verbatim where it cannot. An ID a newer build
/// understands must survive a load/edit/save cycle here, not be quietly dropped
/// as an unknown platform.
#[derive(Debug, Clone, PartialEq)]
pub struct DatSourceEntry {
    pub id: String,
    pub display_name: String,
    pub path: PathBuf,
    pub kind: DatSourceKind,
    pub enabled: bool,
    pub priority: u32,
    pub platform: Option<String>,
    /// How this source came to be registered, for provenance. Free text set by
    /// whoever added it (`"added via GUI"`), never interpreted.
    pub origin: Option<String>,
    pub added_unix_seconds: Option<u64>,
    pub health: DatSourceHealth,
    /// Keys a newer build wrote that this one does not define, kept verbatim so
    /// saving from this build does not delete them.
    pub unknown_fields: toml::Table,
}

impl DatSourceEntry {
    /// A source with the safe defaults for a freshly registered path.
    ///
    /// Enabled is `true`, which deliberately differs from the "new sources
    /// start disabled" rule that applies to *remote* sources. A local DAT file
    /// is a file the user just picked from their own disk in a file dialog;
    /// registering it and then requiring a second click to turn it on
    /// communicates a caution that does not apply to a path they chose
    /// themselves, and nothing is fetched, executed, or written as a result.
    pub fn new(id: String, display_name: String, path: PathBuf, kind: DatSourceKind) -> Self {
        Self {
            id,
            display_name,
            path,
            kind,
            enabled: true,
            priority: DEFAULT_DAT_PRIORITY,
            platform: None,
            origin: None,
            added_unix_seconds: Some(now_unix()),
            health: DatSourceHealth::default(),
            unknown_fields: toml::Table::new(),
        }
    }

    /// The assigned platform's display name, or `None` when unassigned.
    ///
    /// An ID this build cannot resolve renders as itself rather than
    /// disappearing, so an unresolved assignment is visible instead of looking
    /// like no assignment at all.
    pub fn platform_display(&self) -> Option<String> {
        self.platform.as_ref().map(|id| {
            crate::canonical_platform_for_alias(id)
                .map(|canonical| crate::platform::display_name_for(canonical).to_string())
                .unwrap_or_else(|| id.clone())
        })
    }

    /// Whether the assigned platform is one this build knows.
    pub fn platform_is_resolved(&self) -> bool {
        self.platform
            .as_ref()
            .is_none_or(|id| crate::canonical_platform_for_alias(id).is_some())
    }
}

/// Why a source could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatRegistryError {
    /// The ID is empty, too long, or contains something not allowed in one.
    InvalidId {
        id: String,
        reason: String,
    },
    DuplicateId {
        id: String,
    },
    /// Another entry already registers this exact path. Registering it twice
    /// would double every result it contributes without saying so.
    DuplicatePath {
        id: String,
        path: PathBuf,
    },
    /// The display name is empty or too long.
    InvalidDisplayName {
        reason: String,
    },
    /// The path failed the registered-path policy.
    Path {
        refusal: DatPathRefusal,
    },
}

impl std::fmt::Display for DatRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId { id, reason } => write!(f, "source ID '{id}' is not usable: {reason}"),
            Self::DuplicateId { id } => write!(f, "a source with ID '{id}' is already registered"),
            Self::DuplicatePath { id, path } => write!(
                f,
                "'{id}' already registers {} — adding it twice would count everything in it twice",
                path.display()
            ),
            Self::InvalidDisplayName { reason } => write!(f, "the name is not usable: {reason}"),
            Self::Path { refusal } => write!(f, "{}", refusal.detail()),
        }
    }
}

impl std::error::Error for DatRegistryError {}

/// A registered entry this build cannot fully act on, kept exactly as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedDatSetting {
    pub source_id: String,
    pub kind: UnresolvedDatSettingKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedDatSettingKind {
    /// A `platform` this build cannot canonicalise. The assignment is kept and
    /// re-emitted, but it will not match anything here.
    UnresolvedPlatform,
    /// Keys inside a `[[sources]]` entry that this build does not define.
    UnknownFields,
}

impl UnresolvedDatSetting {
    pub fn describe(&self) -> String {
        match self.kind {
            UnresolvedDatSettingKind::UnresolvedPlatform => format!(
                "'{}' is assigned to platform '{}', which this build does not recognise. \
                 The assignment is kept as written.",
                self.source_id, self.detail
            ),
            UnresolvedDatSettingKind::UnknownFields => format!(
                "'{}' carries settings this build does not understand ({}). \
                 They are kept as written.",
                self.source_id, self.detail
            ),
        }
    }
}

/// The registered DAT sources, the DAT matching policy, and everything in the
/// file this build kept but did not interpret.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatSourceRegistry {
    entries: Vec<DatSourceEntry>,
    /// The DAT matching policy, carried through load/edit/save like any other
    /// user preference. `Default` means the safe defaults apply.
    policy: DatPolicyConfig,
    /// Top-level keys a newer build wrote, re-emitted verbatim on save.
    unknown_top_level: toml::Table,
}

impl DatSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a registry from a loaded configuration.
    ///
    /// Entries whose ID is unusable or duplicated are the only ones rejected,
    /// and they are reported rather than dropped silently: an ID is what every
    /// lookup names a source by, so two entries claiming one would leave the
    /// first present but unreachable.
    pub fn from_config(config: &DatSourcesConfig) -> (Self, Vec<String>) {
        let mut registry = Self {
            entries: Vec::new(),
            policy: config.policy.clone().unwrap_or_default(),
            unknown_top_level: config.unknown_fields.clone(),
        };
        let mut problems = Vec::new();
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();

        for entry in config.sources.iter().flatten() {
            let id = entry.id.trim().to_string();
            if let Err(reason) = validate_source_id(&id) {
                problems.push(format!(
                    "a [[sources]] entry with ID '{}' was ignored: {reason}",
                    entry.id
                ));
                continue;
            }
            if seen.insert(id.clone(), ()).is_some() {
                problems.push(format!(
                    "a second [[sources]] entry claiming ID '{id}' was ignored"
                ));
                continue;
            }
            registry.entries.push(entry.clone().into_entry(id));
        }

        (registry, problems)
    }

    /// Serialises the registry back to the preferences shape.
    ///
    /// Every entry is written in full - a DAT source has no compiled-in default
    /// to be compared against, so "only write what differs" has nothing to mean
    /// here. Unknown keys, at both levels, are re-emitted exactly as read.
    pub fn to_config(&self) -> DatSourcesConfig {
        let sources: Vec<DatSourceConfigEntry> = self
            .sorted_all()
            .into_iter()
            .map(DatSourceConfigEntry::from_entry)
            .collect();
        DatSourcesConfig {
            sources: if sources.is_empty() {
                None
            } else {
                Some(sources)
            },
            policy: if self.policy == DatPolicyConfig::default() {
                None
            } else {
                Some(self.policy.clone())
            },
            unknown_fields: self.unknown_top_level.clone(),
        }
    }

    /// The DAT matching policy.
    pub fn policy(&self) -> &DatPolicyConfig {
        &self.policy
    }

    /// The DAT matching policy, for editing a draft registry.
    pub fn policy_mut(&mut self) -> &mut DatPolicyConfig {
        &mut self.policy
    }

    pub fn entries(&self) -> &[DatSourceEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every source, enabled or not, in a stable order.
    ///
    /// Ordered by priority then by ID, matching the cheat registry's
    /// comparator, so enabling or disabling a source never moves it: it is
    /// already in the position it will occupy.
    pub fn sorted_all(&self) -> Vec<&DatSourceEntry> {
        let mut all: Vec<&DatSourceEntry> = self.entries.iter().collect();
        all.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        all
    }

    /// Every enabled source, in consultation order.
    ///
    /// Used by the policy resolver for the global scope (no platform), where
    /// "participates" simply means enabled. Ordered the same way as
    /// [`Self::sorted_all`]: priority then ID.
    pub fn sorted_enabled(&self) -> Vec<&DatSourceEntry> {
        let mut enabled: Vec<&DatSourceEntry> =
            self.entries.iter().filter(|entry| entry.enabled).collect();
        enabled.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        enabled
    }

    /// The enabled sources relevant to `platform_id`, in consultation order.
    ///
    /// A source with no platform assignment is relevant to every platform -
    /// that is what "unassigned" means for a DAT whose own header names its
    /// platform - so it is not the same as "relevant to nothing".
    pub fn sorted_enabled_for_platform(&self, platform_id: &str) -> Vec<&DatSourceEntry> {
        let normalized = crate::canonical_platform_for_alias(platform_id).unwrap_or(platform_id);
        let mut relevant: Vec<&DatSourceEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .filter(|entry| match &entry.platform {
                None => true,
                Some(assigned) => {
                    crate::canonical_platform_for_alias(assigned).unwrap_or(assigned) == normalized
                }
            })
            .collect();
        relevant.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        relevant
    }

    pub fn get(&self, id: &str) -> Option<&DatSourceEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut DatSourceEntry> {
        self.entries.iter_mut().find(|entry| entry.id == id)
    }

    /// Registers a source, refusing a duplicate ID or a duplicate path.
    ///
    /// The path policy is applied here rather than at save time so a bad path
    /// is reported while the user is still looking at the dialog that produced
    /// it.
    pub fn add(&mut self, entry: DatSourceEntry) -> Result<(), DatRegistryError> {
        validate_source_id(&entry.id).map_err(|reason| DatRegistryError::InvalidId {
            id: entry.id.clone(),
            reason,
        })?;
        validate_display_name(&entry.display_name)
            .map_err(|reason| DatRegistryError::InvalidDisplayName { reason })?;
        if self.get(&entry.id).is_some() {
            return Err(DatRegistryError::DuplicateId {
                id: entry.id.clone(),
            });
        }
        if let Some(existing) = self
            .entries
            .iter()
            .find(|existing| existing.path == entry.path)
        {
            return Err(DatRegistryError::DuplicatePath {
                id: existing.id.clone(),
                path: entry.path.clone(),
            });
        }
        validate_dat_path(&entry.path, entry.kind)
            .map_err(|refusal| DatRegistryError::Path { refusal })?;
        self.entries.push(entry);
        Ok(())
    }

    /// Removes a registry entry, and only a registry entry.
    ///
    /// Nothing on disk is touched: not the DAT file, not the folder, not
    /// anything beside them. Returns the removed entry so a caller can say what
    /// it was.
    pub fn remove(&mut self, id: &str) -> Option<DatSourceEntry> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index))
    }

    /// An ID derived from `path` that is not already taken.
    ///
    /// Derived rather than asked for: a Stage 1 user adding a DAT they just
    /// downloaded has no opinion about its stable identifier, and making them
    /// invent one is a dialog they would have to be taught to fill in. The
    /// derivation is deterministic, and the suffix only appears when it has to.
    pub fn suggest_id(&self, path: &Path) -> String {
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base = slugify_id(&stem);
        let base = if base.is_empty() {
            "dat-source".to_string()
        } else {
            base
        };
        if self.get(&base).is_none() {
            return base;
        }
        // Bounded: the loop cannot run past the number of entries plus one,
        // because at most that many suffixes can be taken.
        for suffix in 2..=(self.entries.len() + 2) {
            let candidate = format!("{base}-{suffix}");
            if self.get(&candidate).is_none() {
                return candidate;
            }
        }
        base
    }

    /// Everything in the file this build kept but cannot act on.
    ///
    /// Deterministic order: entries in registry order, platform before fields.
    pub fn unresolved_settings(&self) -> Vec<UnresolvedDatSetting> {
        let mut out = Vec::new();
        for entry in self.sorted_all() {
            if let Some(platform) = &entry.platform
                && crate::canonical_platform_for_alias(platform).is_none()
            {
                out.push(UnresolvedDatSetting {
                    source_id: entry.id.clone(),
                    kind: UnresolvedDatSettingKind::UnresolvedPlatform,
                    detail: platform.clone(),
                });
            }
            if !entry.unknown_fields.is_empty() {
                let mut keys: Vec<&str> = entry.unknown_fields.keys().map(String::as_str).collect();
                keys.sort_unstable();
                out.push(UnresolvedDatSetting {
                    source_id: entry.id.clone(),
                    kind: UnresolvedDatSettingKind::UnknownFields,
                    detail: keys.join(", "),
                });
            }
        }
        out
    }

    /// Top-level keys retained from a file written by a newer build.
    pub fn unknown_top_level(&self) -> &toml::Table {
        &self.unknown_top_level
    }
}

/// Whether `id` is usable as a stable source identity.
///
/// The rules are the model's: ASCII letters, digits, `-`, `_`, `.`; 1–128
/// bytes; not `.` or `..`; no leading or trailing `.`; no path separators. The
/// point of forbidding separators and dot-runs is that an ID is a label, never
/// a path component - nothing may ever join it onto a directory.
pub fn validate_source_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("it is empty".to_string());
    }
    if id.len() > MAX_SOURCE_ID_BYTES {
        return Err(format!(
            "it is {} bytes; the limit is {MAX_SOURCE_ID_BYTES}",
            id.len()
        ));
    }
    if id == "." || id == ".." {
        return Err("'.' and '..' are not names".to_string());
    }
    if id.starts_with('.') || id.ends_with('.') {
        return Err("it must not start or end with '.'".to_string());
    }
    if let Some(bad) = id
        .chars()
        .find(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
    {
        return Err(format!(
            "'{bad}' is not allowed; use ASCII letters, digits, '-', '_' or '.'"
        ));
    }
    Ok(())
}

fn validate_display_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("it is empty".to_string());
    }
    if name.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(format!(
            "it is {} bytes; the limit is {MAX_DISPLAY_NAME_BYTES}",
            name.len()
        ));
    }
    Ok(())
}

/// Turns arbitrary text into something [`validate_source_id`] accepts.
fn slugify_id(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.' | ' ') && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_SOURCE_ID_BYTES {
            break;
        }
    }
    out.trim_matches(['-', '.']).to_string()
}

/// A display name derived from a path, for a source the user did not name.
pub fn suggest_display_name(path: &Path) -> String {
    let raw = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let mut name = raw.trim().to_string();
    if name.is_empty() {
        name = "DAT source".to_string();
    }
    // Truncated on a character boundary, so a long filename cannot produce an
    // invalid name or a panic.
    if name.len() > MAX_DISPLAY_NAME_BYTES {
        let mut end = MAX_DISPLAY_NAME_BYTES;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    name
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
