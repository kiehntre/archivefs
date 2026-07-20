//! Read-only discovery and inspection of RetroArch cheat install journals.
//!
//! Journal data is attacker-controlled. Paths are reconstructed only after
//! lossless root binding and component validation, and every filesystem probe
//! uses `symlink_metadata` or the shared destination-safety layer. This module
//! never creates a directory, changes a file, or follows a final symlink.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::cheat_install_result::{
    CheatInstallEntryResult, CheatInstallOutcome, CheatInstallPath, CheatInstallRun,
    CheatInstallSummary, parse_cheat_install_run,
};
use super::cheat_rollback_result::{
    CheatRollbackOutcome, CheatRollbackRun, CheatRollbackRunStatus, parse_cheat_rollback_run,
};
use super::destination_safety::{
    DestinationSafetyFailureReason, DestinationState, assess_destination, validate_destination_root,
};

pub const CHEAT_HISTORY_RESULT_SCHEMA_VERSION: u32 = 1;

/// A display-safe path that retains exact non-UTF-8 bytes on Unix.
/// Security decisions always use the original `Path`; this is an output model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatInspectionPath {
    pub display: String,
    pub lossy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_bytes: Option<Vec<u8>>,
}

impl CheatInspectionPath {
    pub fn from_path(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            match path.to_str() {
                Some(display) => Self {
                    display: display.to_string(),
                    lossy: false,
                    raw_bytes: None,
                },
                None => Self {
                    display: path.to_string_lossy().into_owned(),
                    lossy: true,
                    raw_bytes: Some(path.as_os_str().as_bytes().to_vec()),
                },
            }
        }
        #[cfg(not(unix))]
        {
            match path.to_str() {
                Some(display) => Self {
                    display: display.to_string(),
                    lossy: false,
                    raw_bytes: None,
                },
                None => Self {
                    display: path.to_string_lossy().into_owned(),
                    lossy: true,
                    raw_bytes: None,
                },
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheatHistoryOptions {
    pub journal_root: PathBuf,
    pub backup_root: PathBuf,
    pub rollback_journal_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatHistoryReport {
    pub schema_version: u32,
    pub journal_root: CheatInspectionPath,
    pub entries: Vec<CheatJournalInspection>,
    pub warnings: Vec<CheatHistoryWarning>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatHistoryWarning {
    pub code: String,
    pub path: CheatInspectionPath,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatJournalInspection {
    pub journal_path: CheatInspectionPath,
    pub run_id: String,
    pub started_at_unix_seconds: u64,
    pub completed_at_unix_seconds: Option<u64>,
    pub dry_run: bool,
    pub catalogue_source: String,
    pub destination_root: Option<CheatInstallPath>,
    pub install_status: super::cheat_install_result::CheatInstallRunStatus,
    pub install_summary: CheatInstallSummary,
    pub entries: Vec<CheatHistoryEntry>,
    pub rollback: CheatRollbackJournalMatch,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatHistoryEntry {
    pub source_path: CheatInstallPath,
    pub platform: Option<String>,
    pub display_title: Option<String>,
    pub destination_path: Option<CheatInstallPath>,
    pub original_outcome: CheatInstallOutcome,
    pub applied: bool,
    pub previous_hash: Option<String>,
    pub installed_hash: Option<String>,
    pub backup_path: Option<CheatInstallPath>,
    pub backup_expected_hash: Option<String>,
    pub destination: CheatDestinationAssessment,
    pub destination_observed_hash: Option<String>,
    pub backup: CheatBackupAssessment,
    pub backup_observed_hash: Option<String>,
    pub rollback_availability: CheatRollbackAvailability,
    pub rollback_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatDestinationAssessment {
    UnchangedSinceInstall,
    Missing,
    Changed,
    Inaccessible,
    UnsafePath,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatBackupAssessment {
    NotApplicable,
    PresentAndValid,
    Missing,
    Changed,
    Inaccessible,
    UnsafePath,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatRollbackAvailability {
    Available,
    Unnecessary,
    AlreadyCompleted,
    BlockedDestinationChanged,
    BlockedMissingBackup,
    BlockedBackupChanged,
    BlockedUnsafePath,
    BlockedInvalidJournal,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatRollbackJournalMatch {
    pub exists: bool,
    pub completed_successfully: Option<bool>,
    pub ambiguous: bool,
    pub journal_path: Option<CheatInspectionPath>,
    pub run_id: Option<String>,
    pub status: Option<CheatRollbackRunStatus>,
}

impl CheatRollbackJournalMatch {
    fn absent() -> Self {
        Self {
            exists: false,
            completed_successfully: None,
            ambiguous: false,
            journal_path: None,
            run_id: None,
            status: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatJournalInspectionError {
    pub code: String,
    pub path: CheatInspectionPath,
    pub message: String,
}

impl std::fmt::Display for CheatJournalInspectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CheatJournalInspectionError {}

pub fn discover_cheat_history(options: &CheatHistoryOptions) -> CheatHistoryReport {
    let mut report = CheatHistoryReport {
        schema_version: CHEAT_HISTORY_RESULT_SCHEMA_VERSION,
        journal_root: CheatInspectionPath::from_path(&options.journal_root),
        entries: Vec::new(),
        warnings: Vec::new(),
    };

    match validate_destination_root(&options.journal_root) {
        Ok(root) if !root.path().exists() => return report,
        Ok(_) => {}
        Err(error) => {
            report.warnings.push(CheatHistoryWarning {
                code: "unsafe_journal_root".into(),
                path: CheatInspectionPath::from_path(&options.journal_root),
                message: error.to_string(),
            });
            return report;
        }
    }

    let directory = match fs::read_dir(&options.journal_root) {
        Ok(directory) => directory,
        Err(error) => {
            report.warnings.push(CheatHistoryWarning {
                code: "journal_root_inaccessible".into(),
                path: CheatInspectionPath::from_path(&options.journal_root),
                message: error.to_string(),
            });
            return report;
        }
    };

    let mut paths = Vec::new();
    for item in directory {
        match item {
            Ok(item) if item.path().extension() == Some(OsStr::new("json")) => {
                paths.push(item.path())
            }
            Ok(_) => {}
            Err(error) => report.warnings.push(CheatHistoryWarning {
                code: "journal_entry_inaccessible".into(),
                path: CheatInspectionPath::from_path(&options.journal_root),
                message: error.to_string(),
            }),
        }
    }
    paths.sort();

    for path in paths {
        match inspect_cheat_install_journal(&path, options) {
            Ok(inspection) => report.entries.push(inspection),
            Err(error) => report.warnings.push(CheatHistoryWarning {
                code: error.code,
                path: error.path,
                message: error.message,
            }),
        }
    }

    report.entries.sort_by(|left, right| {
        right
            .completed_at_unix_seconds
            .or(Some(right.started_at_unix_seconds))
            .cmp(
                &left
                    .completed_at_unix_seconds
                    .or(Some(left.started_at_unix_seconds)),
            )
            .then_with(|| left.journal_path.display.cmp(&right.journal_path.display))
    });
    report
}

pub fn inspect_cheat_install_journal(
    journal_path: &Path,
    options: &CheatHistoryOptions,
) -> Result<CheatJournalInspection, CheatJournalInspectionError> {
    validate_selected_journal(journal_path, &options.journal_root)?;
    let text = fs::read_to_string(journal_path)
        .map_err(|error| inspection_error("journal_inaccessible", journal_path, error))?;
    let run = parse_cheat_install_run(&text).map_err(|error| CheatJournalInspectionError {
        code: match error {
            super::cheat_install_result::CheatInstallRunSchemaError::UnsupportedVersion(_) => {
                "unsupported_journal_version".into()
            }
            super::cheat_install_result::CheatInstallRunSchemaError::Malformed(_) => {
                "malformed_journal".into()
            }
        },
        path: CheatInspectionPath::from_path(journal_path),
        message: error.to_string(),
    })?;

    validate_install_journal_paths(&run, journal_path, options)?;
    let (rollback, mut warnings) = discover_rollback_match(&run, journal_path, options);
    let completed = rollback.completed_successfully == Some(true);
    let ambiguous = rollback.ambiguous;
    let entries = run
        .entries
        .iter()
        .map(|entry| inspect_entry(entry, &run, options, completed, ambiguous))
        .collect();

    let expected_summary = CheatInstallSummary::from_entries(&run.entries, run.dry_run);
    if run.summary != expected_summary {
        warnings.push("install summary does not match entries".into());
    }
    if run.status
        != super::cheat_install_result::CheatInstallRunStatus::derive(
            &expected_summary,
            run.dry_run,
        )
    {
        warnings.push("install status does not match entries and dry_run".into());
    }

    Ok(CheatJournalInspection {
        journal_path: CheatInspectionPath::from_path(journal_path),
        run_id: run.run_id,
        started_at_unix_seconds: run.started_at_unix_seconds,
        completed_at_unix_seconds: run.completed_at_unix_seconds,
        dry_run: run.dry_run,
        catalogue_source: run.catalogue_source,
        destination_root: run.destination_root,
        install_status: run.status,
        install_summary: run.summary,
        entries,
        rollback,
        warnings,
    })
}

fn validate_selected_journal(path: &Path, root: &Path) -> Result<(), CheatJournalInspectionError> {
    validate_destination_root(root).map_err(|error| CheatJournalInspectionError {
        code: "unsafe_journal_root".into(),
        path: CheatInspectionPath::from_path(root),
        message: error.to_string(),
    })?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CheatJournalInspectionError {
            code: "journal_outside_root".into(),
            path: CheatInspectionPath::from_path(path),
            message: "journal path is outside the selected journal root".into(),
        })?;
    if relative.components().count() != 1
        || !matches!(relative.components().next(), Some(Component::Normal(_)))
    {
        return Err(CheatJournalInspectionError {
            code: "unsafe_journal_path".into(),
            path: CheatInspectionPath::from_path(path),
            message: "journal must be a direct child of the selected journal root".into(),
        });
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| inspection_error("journal_inaccessible", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CheatJournalInspectionError {
            code: "unsafe_journal_path".into(),
            path: CheatInspectionPath::from_path(path),
            message: "journal is not a plain regular file".into(),
        });
    }
    Ok(())
}

fn validate_install_journal_paths(
    run: &CheatInstallRun,
    journal_path: &Path,
    options: &CheatHistoryOptions,
) -> Result<(), CheatJournalInspectionError> {
    for entry in &run.entries {
        if let Some(destination) = &entry.destination_path {
            reconstruct_destination(run.destination_root.as_ref(), destination).map_err(
                |message| CheatJournalInspectionError {
                    code: "invalid_destination_path".into(),
                    path: CheatInspectionPath::from_path(journal_path),
                    message,
                },
            )?;
        }
        if let Some(backup) = &entry.backup_path {
            reconstruct_direct_child(&options.backup_root, backup).map_err(|message| {
                CheatJournalInspectionError {
                    code: "invalid_backup_path".into(),
                    path: CheatInspectionPath::from_path(journal_path),
                    message,
                }
            })?;
        }
    }
    Ok(())
}

fn inspect_entry(
    entry: &CheatInstallEntryResult,
    run: &CheatInstallRun,
    options: &CheatHistoryOptions,
    rollback_completed: bool,
    rollback_ambiguous: bool,
) -> CheatHistoryEntry {
    let (destination, destination_observed_hash) = assess_entry_destination(entry, run);
    let (backup, backup_observed_hash) = assess_entry_backup(entry, options);
    let mut reasons = Vec::new();
    let availability = rollback_availability(
        entry,
        destination,
        destination_observed_hash.as_deref(),
        backup,
        rollback_completed,
        rollback_ambiguous,
        &mut reasons,
    );
    let platform = entry.destination_path.as_ref().and_then(|path| {
        run.destination_root.as_ref().and_then(|root| {
            Path::new(&path.display)
                .strip_prefix(Path::new(&root.display))
                .ok()
                .and_then(|relative| relative.components().next())
                .and_then(|component| match component {
                    Component::Normal(value) => value.to_str().map(str::to_owned),
                    _ => None,
                })
        })
    });
    let display_title = Path::new(&entry.source_path.display)
        .file_stem()
        .and_then(OsStr::to_str)
        .map(str::to_owned);
    CheatHistoryEntry {
        source_path: entry.source_path.clone(),
        platform,
        display_title,
        destination_path: entry.destination_path.clone(),
        original_outcome: entry.outcome,
        applied: entry.applied,
        previous_hash: entry.previous_destination_hash.clone(),
        installed_hash: entry
            .resulting_destination_hash
            .clone()
            .or_else(|| entry.expected_source_hash.clone()),
        backup_path: entry.backup_path.clone(),
        backup_expected_hash: entry.previous_destination_hash.clone(),
        destination,
        destination_observed_hash,
        backup,
        backup_observed_hash,
        rollback_availability: availability,
        rollback_reasons: reasons,
    }
}

fn assess_entry_destination(
    entry: &CheatInstallEntryResult,
    run: &CheatInstallRun,
) -> (CheatDestinationAssessment, Option<String>) {
    let Ok((root, platform, file_name, path)) = reconstruct_destination(
        run.destination_root.as_ref(),
        match entry.destination_path.as_ref() {
            Some(path) => path,
            None => return (CheatDestinationAssessment::Unknown, None),
        },
    ) else {
        return (CheatDestinationAssessment::UnsafePath, None);
    };
    let assessment = match assess_destination(&root, &platform, &file_name) {
        Ok(value) => value,
        Err(error) => {
            return if error.reason == DestinationSafetyFailureReason::InspectionFailed {
                (CheatDestinationAssessment::Inaccessible, None)
            } else {
                (CheatDestinationAssessment::UnsafePath, None)
            };
        }
    };
    if assessment.destination_state == DestinationState::Absent {
        return (CheatDestinationAssessment::Missing, None);
    }
    let expected = entry
        .resulting_destination_hash
        .as_deref()
        .or(entry.expected_source_hash.as_deref());
    match hash_plain_file(&path) {
        Ok(hash) if Some(hash.as_str()) == expected => (
            CheatDestinationAssessment::UnchangedSinceInstall,
            Some(hash),
        ),
        Ok(hash) => (CheatDestinationAssessment::Changed, Some(hash)),
        Err(_) => (CheatDestinationAssessment::Inaccessible, None),
    }
}

fn assess_entry_backup(
    entry: &CheatInstallEntryResult,
    options: &CheatHistoryOptions,
) -> (CheatBackupAssessment, Option<String>) {
    let Some(encoded) = entry.backup_path.as_ref() else {
        return if entry.outcome == CheatInstallOutcome::ReplacedWithBackup && entry.applied {
            (CheatBackupAssessment::Missing, None)
        } else {
            (CheatBackupAssessment::NotApplicable, None)
        };
    };
    let path = match reconstruct_direct_child(&options.backup_root, encoded) {
        Ok(path) => path,
        Err(_) => return (CheatBackupAssessment::UnsafePath, None),
    };
    if validate_destination_root(&options.backup_root).is_err() {
        return (CheatBackupAssessment::UnsafePath, None);
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (CheatBackupAssessment::Missing, None);
        }
        Err(_) => return (CheatBackupAssessment::Inaccessible, None),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return (CheatBackupAssessment::UnsafePath, None);
    }
    match hash_plain_file(&path) {
        Ok(hash) if Some(hash.as_str()) == entry.previous_destination_hash.as_deref() => {
            (CheatBackupAssessment::PresentAndValid, Some(hash))
        }
        Ok(hash) => (CheatBackupAssessment::Changed, Some(hash)),
        Err(_) => (CheatBackupAssessment::Inaccessible, None),
    }
}

fn rollback_availability(
    entry: &CheatInstallEntryResult,
    destination: CheatDestinationAssessment,
    destination_observed_hash: Option<&str>,
    backup: CheatBackupAssessment,
    completed: bool,
    ambiguous: bool,
    reasons: &mut Vec<String>,
) -> CheatRollbackAvailability {
    if ambiguous {
        reasons.push("multiple rollback journals have equally strong binding".into());
        return CheatRollbackAvailability::Unknown;
    }
    if completed {
        return CheatRollbackAvailability::AlreadyCompleted;
    }
    if !entry.applied
        || !matches!(
            entry.outcome,
            CheatInstallOutcome::InstalledNew | CheatInstallOutcome::ReplacedWithBackup
        )
    {
        return CheatRollbackAvailability::Unnecessary;
    }
    if entry.resulting_destination_hash.is_none() {
        reasons.push("applied install entry has no resulting destination hash".into());
        return CheatRollbackAvailability::BlockedInvalidJournal;
    }
    if destination == CheatDestinationAssessment::UnsafePath {
        reasons.push("destination path is unsafe".into());
        return CheatRollbackAvailability::BlockedUnsafePath;
    }
    if destination == CheatDestinationAssessment::Inaccessible {
        reasons.push("destination is inaccessible".into());
        return CheatRollbackAvailability::Unknown;
    }
    if entry.outcome == CheatInstallOutcome::InstalledNew {
        return match destination {
            CheatDestinationAssessment::UnchangedSinceInstall => {
                CheatRollbackAvailability::Available
            }
            CheatDestinationAssessment::Missing => CheatRollbackAvailability::Unnecessary,
            CheatDestinationAssessment::Changed => {
                reasons.push("destination changed since installation".into());
                CheatRollbackAvailability::BlockedDestinationChanged
            }
            _ => CheatRollbackAvailability::Unknown,
        };
    }
    if destination_observed_hash == entry.previous_destination_hash.as_deref() {
        return CheatRollbackAvailability::Unnecessary;
    }
    if destination == CheatDestinationAssessment::Changed {
        reasons.push("destination changed since installation".into());
        return CheatRollbackAvailability::BlockedDestinationChanged;
    }
    if destination == CheatDestinationAssessment::Missing {
        reasons.push("replacement destination is missing".into());
        return CheatRollbackAvailability::BlockedDestinationChanged;
    }
    match backup {
        CheatBackupAssessment::PresentAndValid => CheatRollbackAvailability::Available,
        CheatBackupAssessment::Missing => {
            reasons.push("required backup is missing".into());
            CheatRollbackAvailability::BlockedMissingBackup
        }
        CheatBackupAssessment::Changed => {
            reasons.push("required backup hash changed".into());
            CheatRollbackAvailability::BlockedBackupChanged
        }
        CheatBackupAssessment::UnsafePath => {
            reasons.push("backup path is unsafe".into());
            CheatRollbackAvailability::BlockedUnsafePath
        }
        CheatBackupAssessment::Inaccessible | CheatBackupAssessment::Unknown => {
            reasons.push("backup cannot be assessed".into());
            CheatRollbackAvailability::Unknown
        }
        CheatBackupAssessment::NotApplicable => {
            reasons.push("replacement journal has no backup metadata".into());
            CheatRollbackAvailability::BlockedInvalidJournal
        }
    }
}

fn discover_rollback_match(
    install: &CheatInstallRun,
    install_path: &Path,
    options: &CheatHistoryOptions,
) -> (CheatRollbackJournalMatch, Vec<String>) {
    let mut warnings = Vec::new();
    let root = match validate_destination_root(&options.rollback_journal_root) {
        Ok(root) if !root.path().exists() => {
            return (CheatRollbackJournalMatch::absent(), warnings);
        }
        Ok(_) => &options.rollback_journal_root,
        Err(error) => {
            warnings.push(format!("rollback journal root is unsafe: {error}"));
            return (CheatRollbackJournalMatch::absent(), warnings);
        }
    };
    let directory = match fs::read_dir(root) {
        Ok(directory) => directory,
        Err(error) => {
            warnings.push(format!("rollback journal root is inaccessible: {error}"));
            return (CheatRollbackJournalMatch::absent(), warnings);
        }
    };
    let mut matches = Vec::new();
    for item in directory.flatten() {
        let path = item.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(rollback) = parse_cheat_rollback_run(&text) else {
            continue;
        };
        if rollback.original_install_run_id != install.run_id {
            continue;
        }
        if rollback_binding_is_valid(&rollback, install, install_path) {
            matches.push((path, rollback));
        } else {
            warnings.push(format!(
                "rollback run {} names this install run but fails path/hash binding validation",
                rollback.run_id
            ));
        }
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    if matches.len() > 1 {
        return (
            CheatRollbackJournalMatch {
                exists: true,
                completed_successfully: None,
                ambiguous: true,
                journal_path: None,
                run_id: None,
                status: None,
            },
            warnings,
        );
    }
    let Some((path, rollback)) = matches.pop() else {
        return (CheatRollbackJournalMatch::absent(), warnings);
    };
    let completed = rollback_completed_successfully(&rollback, install);
    (
        CheatRollbackJournalMatch {
            exists: true,
            completed_successfully: Some(completed),
            ambiguous: false,
            journal_path: Some(CheatInspectionPath::from_path(&path)),
            run_id: Some(rollback.run_id),
            status: Some(rollback.status),
        },
        warnings,
    )
}

fn rollback_binding_is_valid(
    rollback: &CheatRollbackRun,
    install: &CheatInstallRun,
    install_path: &Path,
) -> bool {
    let encoded_install_path = CheatInstallPath::from_path(install_path);
    !encoded_install_path.lossy
        && rollback.original_journal_path == encoded_install_path
        && install.destination_root.as_ref() == Some(&rollback.destination_root)
        && rollback.entries.len() == install.entries.len()
        && rollback
            .entries
            .iter()
            .zip(&install.entries)
            .all(|(rolled, original)| {
                rolled.original_outcome == original.outcome
                    && rolled.destination_path == original.destination_path
                    && rolled.expected_installed_hash
                        == original
                            .resulting_destination_hash
                            .clone()
                            .or_else(|| original.expected_source_hash.clone())
                    && rolled.expected_previous_hash == original.previous_destination_hash
                    && rolled.backup_path == original.backup_path
            })
}

fn rollback_completed_successfully(rollback: &CheatRollbackRun, install: &CheatInstallRun) -> bool {
    !rollback.dry_run
        && rollback.confirmed
        && rollback.status == CheatRollbackRunStatus::Success
        && rollback
            .entries
            .iter()
            .zip(&install.entries)
            .all(
                |(rolled, original)| match (original.outcome, rolled.outcome) {
                    (
                        CheatInstallOutcome::InstalledNew,
                        CheatRollbackOutcome::RemovedInstalledFile,
                    ) if original.applied => {
                        rolled.wrote
                            && rolled.observed_current_hash == rolled.expected_installed_hash
                    }
                    (CheatInstallOutcome::InstalledNew, CheatRollbackOutcome::AlreadyRestored)
                        if original.applied =>
                    {
                        !rolled.wrote && rolled.observed_current_hash.is_none()
                    }
                    (
                        CheatInstallOutcome::ReplacedWithBackup,
                        CheatRollbackOutcome::RestoredBackup,
                    ) if original.applied => {
                        rolled.wrote
                            && rolled.observed_current_hash == rolled.expected_installed_hash
                    }
                    (
                        CheatInstallOutcome::ReplacedWithBackup,
                        CheatRollbackOutcome::AlreadyRestored,
                    ) if original.applied => {
                        !rolled.wrote
                            && rolled.observed_current_hash == rolled.expected_previous_hash
                    }
                    _ => rolled.outcome == CheatRollbackOutcome::NoChangeRequired && !rolled.wrote,
                },
            )
}

fn reconstruct_destination(
    encoded_root: Option<&CheatInstallPath>,
    encoded_path: &CheatInstallPath,
) -> Result<(PathBuf, std::ffi::OsString, std::ffi::OsString, PathBuf), String> {
    let root = encoded_root.ok_or_else(|| "journal has no destination root".to_string())?;
    if root.lossy || encoded_path.lossy {
        return Err("journal destination path cannot be reconstructed losslessly".into());
    }
    let root_path = PathBuf::from(&root.display);
    if !root_path.is_absolute()
        || root_path
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err("journal destination root is not a safe absolute path".into());
    }
    let path = PathBuf::from(&encoded_path.display);
    let relative = path
        .strip_prefix(&root_path)
        .map_err(|_| "journal destination is outside its recorded root".to_string())?;
    let parts = relative.components().collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err(
            "journal destination must contain exactly platform and filename components".into(),
        );
    }
    let (Component::Normal(platform), Component::Normal(file_name)) = (parts[0], parts[1]) else {
        return Err("journal destination contains an unsafe component".into());
    };
    let rebuilt = root_path.join(platform).join(file_name);
    if rebuilt != path || CheatInstallPath::from_path(&rebuilt) != *encoded_path {
        return Err("journal destination disagrees with safely reconstructed path".into());
    }
    Ok((
        root_path,
        platform.to_os_string(),
        file_name.to_os_string(),
        rebuilt,
    ))
}

fn reconstruct_direct_child(root: &Path, encoded: &CheatInstallPath) -> Result<PathBuf, String> {
    if encoded.lossy || CheatInstallPath::from_path(root).lossy {
        return Err("path cannot be reconstructed losslessly".into());
    }
    let path = PathBuf::from(&encoded.display);
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "path is outside the expected ArchiveFS root".to_string())?;
    if relative.components().count() != 1
        || !matches!(relative.components().next(), Some(Component::Normal(_)))
        || root.join(relative) != path
    {
        return Err(
            "path is not a direct, traversal-free child of the expected ArchiveFS root".into(),
        );
    }
    Ok(path)
}

fn hash_plain_file(path: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a plain file",
        ));
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn inspection_error(
    code: &str,
    path: &Path,
    error: impl std::fmt::Display,
) -> CheatJournalInspectionError {
    CheatJournalInspectionError {
        code: code.into(),
        path: CheatInspectionPath::from_path(path),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
