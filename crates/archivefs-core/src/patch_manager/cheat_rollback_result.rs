//! Stable result and journal schema for RetroArch cheat rollback runs.

use serde::{Deserialize, Serialize};

use super::cheat_install_result::{CheatInstallOutcome, CheatInstallPath};

pub const CHEAT_ROLLBACK_RUN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatRollbackOutcome {
    WouldRemoveInstalledFile,
    RemovedInstalledFile,
    WouldRestoreBackup,
    RestoredBackup,
    AlreadyRestored,
    NoChangeRequired,
    FailedDestinationChanged,
    FailedBackupMissing,
    FailedBackupChanged,
    FailedUnsafeDestination,
    FailedUnsafeBackupPath,
    FailedInvalidJournal,
    FailedIo,
    FailedVerification,
}

impl CheatRollbackOutcome {
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Self::FailedDestinationChanged
                | Self::FailedBackupMissing
                | Self::FailedBackupChanged
                | Self::FailedUnsafeDestination
                | Self::FailedUnsafeBackupPath
                | Self::FailedInvalidJournal
                | Self::FailedIo
                | Self::FailedVerification
        )
    }

    fn is_write(self) -> bool {
        matches!(self, Self::RemovedInstalledFile | Self::RestoredBackup)
    }

    fn is_preview(self) -> bool {
        matches!(
            self,
            Self::WouldRemoveInstalledFile | Self::WouldRestoreBackup
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatRollbackEntryResult {
    pub original_outcome: CheatInstallOutcome,
    pub destination_path: Option<CheatInstallPath>,
    pub expected_installed_hash: Option<String>,
    pub expected_previous_hash: Option<String>,
    pub observed_current_hash: Option<String>,
    pub backup_path: Option<CheatInstallPath>,
    pub outcome: CheatRollbackOutcome,
    pub wrote: bool,
    pub error_code: Option<String>,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatRollbackSummary {
    pub requested: usize,
    pub removed: usize,
    pub restored: usize,
    pub already_restored: usize,
    pub no_change_required: usize,
    pub would_change: usize,
    pub failed: usize,
    pub writes_succeeded: usize,
}

impl CheatRollbackSummary {
    pub fn from_entries(entries: &[CheatRollbackEntryResult]) -> Self {
        let mut result = Self {
            requested: entries.len(),
            ..Self::default()
        };
        for entry in entries {
            match entry.outcome {
                CheatRollbackOutcome::RemovedInstalledFile => result.removed += 1,
                CheatRollbackOutcome::RestoredBackup => result.restored += 1,
                CheatRollbackOutcome::AlreadyRestored => result.already_restored += 1,
                CheatRollbackOutcome::NoChangeRequired => result.no_change_required += 1,
                outcome if outcome.is_preview() => result.would_change += 1,
                outcome if outcome.is_failure() => result.failed += 1,
                _ => {}
            }
            if entry.outcome.is_write() && entry.wrote {
                result.writes_succeeded += 1;
            }
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatRollbackRunStatus {
    Success,
    PartialFailure,
    Failed,
    DryRun,
}

impl CheatRollbackRunStatus {
    pub fn derive(summary: &CheatRollbackSummary, dry_run: bool) -> Self {
        if dry_run {
            return Self::DryRun;
        }
        if summary.failed == 0 {
            return Self::Success;
        }
        if summary.writes_succeeded > 0
            || summary.already_restored > 0
            || summary.no_change_required > 0
        {
            Self::PartialFailure
        } else {
            Self::Failed
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatRollbackRun {
    pub schema_version: u32,
    pub run_id: String,
    pub original_install_run_id: String,
    pub original_journal_path: CheatInstallPath,
    pub started_at_unix_seconds: u64,
    pub completed_at_unix_seconds: Option<u64>,
    pub dry_run: bool,
    pub confirmed: bool,
    pub destination_root: CheatInstallPath,
    pub entries: Vec<CheatRollbackEntryResult>,
    pub summary: CheatRollbackSummary,
    pub status: CheatRollbackRunStatus,
    pub rollback_journal_path: Option<CheatInstallPath>,
    pub journal_write_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheatRollbackRunSchemaError {
    UnsupportedVersion(u32),
    Malformed(String),
}

impl std::fmt::Display for CheatRollbackRunSchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported cheat rollback run schema version {version} (expected {CHEAT_ROLLBACK_RUN_SCHEMA_VERSION})"
            ),
            Self::Malformed(message) => {
                write!(formatter, "malformed cheat rollback run: {message}")
            }
        }
    }
}

impl std::error::Error for CheatRollbackRunSchemaError {}

pub fn parse_cheat_rollback_run(
    json: &str,
) -> Result<CheatRollbackRun, CheatRollbackRunSchemaError> {
    let run: CheatRollbackRun = serde_json::from_str(json)
        .map_err(|error| CheatRollbackRunSchemaError::Malformed(error.to_string()))?;
    if run.schema_version != CHEAT_ROLLBACK_RUN_SCHEMA_VERSION {
        return Err(CheatRollbackRunSchemaError::UnsupportedVersion(
            run.schema_version,
        ));
    }
    let summary = CheatRollbackSummary::from_entries(&run.entries);
    if run.summary != summary {
        return Err(CheatRollbackRunSchemaError::Malformed(
            "summary does not match entries".to_string(),
        ));
    }
    if run.status != CheatRollbackRunStatus::derive(&summary, run.dry_run) {
        return Err(CheatRollbackRunSchemaError::Malformed(
            "status does not match entries and dry_run".to_string(),
        ));
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_preview_and_failures() {
        let entries = vec![
            CheatRollbackEntryResult {
                original_outcome: CheatInstallOutcome::InstalledNew,
                destination_path: None,
                expected_installed_hash: None,
                expected_previous_hash: None,
                observed_current_hash: None,
                backup_path: None,
                outcome: CheatRollbackOutcome::WouldRemoveInstalledFile,
                wrote: false,
                error_code: None,
                message: String::new(),
                retryable: true,
            },
            CheatRollbackEntryResult {
                original_outcome: CheatInstallOutcome::ReplacedWithBackup,
                destination_path: None,
                expected_installed_hash: None,
                expected_previous_hash: None,
                observed_current_hash: None,
                backup_path: None,
                outcome: CheatRollbackOutcome::FailedDestinationChanged,
                wrote: false,
                error_code: Some("destination_changed".into()),
                message: String::new(),
                retryable: true,
            },
        ];
        let summary = CheatRollbackSummary::from_entries(&entries);
        assert_eq!(summary.requested, 2);
        assert_eq!(summary.would_change, 1);
        assert_eq!(summary.failed, 1);
    }
}
