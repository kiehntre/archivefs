//! The DAT source registry on disk: `~/.config/archivefs/dat_sources.toml`.
//!
//! # This file keeps what it does not understand
//!
//! `cheat_sources.toml` is `#[serde(deny_unknown_fields)]`, which means every
//! already-released binary refuses to read it the moment a new key appears -
//! including a `format_version` key that could have said so politely. That
//! constraint is now permanent for that file.
//!
//! This one is new, so it does not have to repeat the mistake. Unknown keys are
//! captured with `#[serde(flatten)]` at both the document and the entry level
//! and re-emitted verbatim on save. A future build can therefore add a field,
//! and this build will carry it through a load/edit/save cycle untouched
//! instead of deleting the user's line.
//!
//! There is deliberately still no `format_version`. A version number is only
//! useful to a reader that would otherwise misinterpret the file, and a reader
//! that preserves what it does not know has nothing to misinterpret. Adding one
//! now would be a key with no consumer.
//!
//! # Durability
//!
//! Saving goes through [`crate::atomic_write_text`]: temporary file in the same
//! directory, `sync_all`, permissions carried across, atomic rename, parent
//! directory synced, temporary file removed on any failure. A failed save
//! therefore leaves the previous file exactly as it was.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{DatHealthState, DatSourceEntry, DatSourceHealth, DatSourceKind, MIN_DAT_PRIORITY};
use crate::ArchiveFsError;
use crate::dat::policy::config::DatPolicyConfig;

/// The whole preferences document.
///
/// Alongside the registered sources it carries one optional `[policy]` table:
/// the DAT matching preferences. The policy is deliberately part of this file
/// rather than a second document, so there is a single answer to "what is this
/// user's DAT matching policy" and a single durable-write path for both.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DatSourcesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<DatSourceConfigEntry>>,

    /// The DAT matching policy, present only when the user has set at least
    /// one preference. Absent means the safe defaults apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<DatPolicyConfig>,

    /// Top-level keys a newer build wrote. Never interpreted, never dropped.
    #[serde(flatten)]
    pub unknown_fields: toml::Table,
}

/// One `[[sources]]` entry.
///
/// Health is stored as flat, prefixed scalars rather than a nested
/// `[sources.health]` table. TOML requires every scalar in a table to precede
/// any sub-table, and a flattened catch-all can contain either; keeping the
/// entry free of sub-tables of our own removes that ordering hazard entirely
/// and leaves the file readable at a glance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatSourceConfigEntry {
    pub id: String,
    pub display_name: String,
    pub path: String,
    pub kind: DatSourceKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_unix_seconds: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_state: Option<DatHealthState>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_last_validated_unix_seconds: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_detail: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_entry_count: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_rom_count: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_file_count: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_formats: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_observed_size_bytes: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_observed_modified_unix_seconds: Option<i64>,

    /// Keys inside this entry that a newer build wrote.
    #[serde(flatten)]
    pub unknown_fields: toml::Table,
}

impl DatSourceConfigEntry {
    /// Turns a persisted entry into a live one.
    ///
    /// `id` is passed in already validated, so this cannot construct an entry
    /// whose identity the registry would refuse.
    ///
    /// A priority outside the accepted range is clamped rather than rejected,
    /// matching how the cheat registry treats a hand-edited file: refusing to
    /// start over one bad number would be worse than correcting it, and the
    /// correction is visible because the file is rewritten with the clamped
    /// value on the next save.
    pub(super) fn into_entry(self, id: String) -> DatSourceEntry {
        DatSourceEntry {
            id,
            display_name: self.display_name,
            path: PathBuf::from(self.path),
            kind: self.kind,
            enabled: self.enabled.unwrap_or(true),
            priority: self
                .priority
                .unwrap_or(super::DEFAULT_DAT_PRIORITY)
                .clamp(MIN_DAT_PRIORITY, super::MAX_DAT_PRIORITY),
            platform: self.platform.filter(|value| !value.trim().is_empty()),
            origin: self.origin,
            added_unix_seconds: self.added_unix_seconds,
            health: DatSourceHealth {
                state: self.health_state,
                last_validated_unix_seconds: self.health_last_validated_unix_seconds,
                detail: self.health_detail,
                entry_count: self.health_entry_count,
                rom_count: self.health_rom_count,
                file_count: self.health_file_count,
                formats: self.health_formats,
                observed_size_bytes: self.health_observed_size_bytes,
                observed_modified_unix_seconds: self.health_observed_modified_unix_seconds,
            },
            unknown_fields: self.unknown_fields,
        }
    }

    pub(super) fn from_entry(entry: &DatSourceEntry) -> Self {
        Self {
            id: entry.id.clone(),
            display_name: entry.display_name.clone(),
            path: entry.path.to_string_lossy().into_owned(),
            kind: entry.kind,
            enabled: Some(entry.enabled),
            priority: Some(entry.priority),
            platform: entry.platform.clone(),
            origin: entry.origin.clone(),
            added_unix_seconds: entry.added_unix_seconds,
            health_state: entry.health.state,
            health_last_validated_unix_seconds: entry.health.last_validated_unix_seconds,
            health_detail: entry.health.detail.clone(),
            health_entry_count: entry.health.entry_count,
            health_rom_count: entry.health.rom_count,
            health_file_count: entry.health.file_count,
            health_formats: entry.health.formats.clone(),
            health_observed_size_bytes: entry.health.observed_size_bytes,
            health_observed_modified_unix_seconds: entry.health.observed_modified_unix_seconds,
            unknown_fields: entry.unknown_fields.clone(),
        }
    }
}

/// Where the registry lives for the current user.
pub fn default_dat_sources_config_path() -> Result<PathBuf, ArchiveFsError> {
    dat_sources_config_path_in(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")))
}

/// The registry path under `home`, or an error when there is no home.
///
/// Split out so the no-home case is testable without removing `HOME` from the
/// process: these tests run in parallel with others that read it.
pub(super) fn dat_sources_config_path_in(
    home: Option<OsString>,
) -> Result<PathBuf, ArchiveFsError> {
    let home = home.ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("dat_sources.toml"))
}

pub fn load_dat_sources_config_default() -> Result<DatSourcesConfig, ArchiveFsError> {
    load_dat_sources_config_from(default_dat_sources_config_path()?)
}

/// Reads the registry. An absent or empty file means "no DAT sources yet".
pub fn load_dat_sources_config_from(
    path: impl AsRef<Path>,
) -> Result<DatSourcesConfig, ArchiveFsError> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DatSourcesConfig::default());
        }
        Err(source) => return Err(ArchiveFsError::io(path.to_path_buf(), source)),
    };
    if text.trim().is_empty() {
        return Ok(DatSourcesConfig::default());
    }
    toml::from_str(&text).map_err(|error| {
        ArchiveFsError::Config(format!("failed to parse {}: {error}", path.display()))
    })
}

pub fn save_dat_sources_config_default(config: &DatSourcesConfig) -> Result<(), ArchiveFsError> {
    save_dat_sources_config_to(default_dat_sources_config_path()?, config)
}

/// Writes the registry durably.
pub fn save_dat_sources_config_to(
    path: impl AsRef<Path>,
    config: &DatSourcesConfig,
) -> Result<(), ArchiveFsError> {
    let path = path.as_ref();
    let header = "# ArchiveFS DAT source registry\n\
                  # Managed by the DAT Sources page. Keys this build does not\n\
                  # recognise are preserved rather than removed.\n\n";
    let body = toml::to_string_pretty(config).map_err(|error| {
        ArchiveFsError::Config(format!("failed to serialize DAT source registry: {error}"))
    })?;
    crate::atomic_write_text(path, &format!("{header}{body}"))
}
