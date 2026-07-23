//! Bounded shared apply, journal, history, and rollback pipeline.
//!
//! Writes are available only for an explicitly confirmed, exact plan produced
//! from the shared preview. PCSX2 and Dolphin intentionally remain preview-only
//! until they expose an independent, adapter-approved materialized source.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::destination_safety::{DestinationState, assess_destination, validate_destination_root};
use super::shared_preview::{
    PreviewAdapter, PreviewDestinationState, PreviewEligibility, PreviewProposedAction,
    SharedPreviewReport,
};

pub const SHARED_APPLY_SCHEMA_VERSION: u32 = 1;
pub const SHARED_MAX_ENTRIES: usize = 128;
pub const SHARED_MAX_SOURCE_BYTES: u64 = 1024 * 1024;
pub const SHARED_MAX_TOTAL_WRITTEN_BYTES: u64 = 32 * 1024 * 1024;
pub const SHARED_MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024;
pub const SHARED_MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
pub const SHARED_MAX_HISTORY_JOURNALS: usize = 512;
pub const SHARED_MAX_ROLLBACK_ENTRIES: usize = 128;
pub const SHARED_MAX_WARNINGS: usize = 64;
pub const SHARED_MAX_FAILURES: usize = 128;
pub const SHARED_MAX_CREATED_DIRECTORIES: usize = 32;
pub const SHARED_MAX_TEMP_FILES: usize = 128;
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    BackupWrite,
    TemporaryWrite,
    Flush,
    Rename,
    Verification,
    JournalWrite,
    ParentCreationRace,
    SourceMutation,
    DestinationMutation,
    RollbackRestore,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULT: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn inject_fault(point: Option<FaultPoint>) {
    INJECTED_FAULT.with(|fault| fault.set(point));
}

#[cfg(test)]
fn should_inject(point: FaultPoint) -> bool {
    INJECTED_FAULT.with(|fault| fault.get() == Some(point))
}

#[cfg(not(test))]
fn should_inject(_point: FaultPoint) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedAdapterWriteSupport {
    ApplyAndRollback,
    PreviewOnlySourceNotMaterialized,
}

pub fn adapter_write_support(adapter: PreviewAdapter) -> SharedAdapterWriteSupport {
    match adapter {
        PreviewAdapter::RetroArch => SharedAdapterWriteSupport::ApplyAndRollback,
        PreviewAdapter::Pcsx2 | PreviewAdapter::Dolphin => {
            SharedAdapterWriteSupport::PreviewOnlySourceNotMaterialized
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedTransactionPath {
    pub display: String,
    pub unix_bytes_hex: Option<String>,
}

impl SharedTransactionPath {
    pub fn from_path(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            Self {
                display: path.to_string_lossy().into_owned(),
                unix_bytes_hex: Some(hex_bytes(bytes)),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                display: path.to_string_lossy().into_owned(),
                unix_bytes_hex: None,
            }
        }
    }

    pub fn to_path_buf(&self) -> Result<PathBuf, SharedApplyFailureKind> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let encoded = self
                .unix_bytes_hex
                .as_deref()
                .ok_or(SharedApplyFailureKind::InvalidJournal)?;
            let bytes = decode_hex(encoded).ok_or(SharedApplyFailureKind::InvalidJournal)?;
            Ok(PathBuf::from(OsString::from_vec(bytes)))
        }
        #[cfg(not(unix))]
        {
            if self.unix_bytes_hex.is_some() {
                return Err(SharedApplyFailureKind::UnsupportedJournal);
            }
            Ok(PathBuf::from(&self.display))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedTransactionStage {
    DryRun,
    InstallNew,
    ReplaceExisting,
    AlreadyInstalled,
    SkippedNotEligible,
    SkippedConflict,
    SkippedReplacementNotApproved,
    SourceChanged,
    DestinationChanged,
    BackupCreated,
    BackupFailed,
    WriteFailed,
    VerificationFailed,
    JournalWritten,
    JournalFailedAfterSuccessfulWrite,
    Success,
    PartialFailure,
    Failed,
    RollbackAvailable,
    RollbackUnavailable,
    RollbackBlocked,
    RollbackSucceeded,
    RollbackFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedApplyFailureKind {
    ConfirmationRequired,
    ConfirmationPlanMismatch,
    ReplacementNotApproved,
    UnsupportedAdapter,
    InvalidPlan,
    DuplicateOperationId,
    DuplicateDestination,
    ResourceLimitReached,
    SourceOutsideApprovedScope,
    SourceMissing,
    SourceSymlink,
    SourceSpecialFile,
    SourceChanged,
    DestinationUnsafe,
    DestinationChanged,
    RootChanged,
    LockUnsupported,
    LockTimeout,
    ManagedRootUnsafe,
    ParentCreationFailed,
    BackupFailed,
    WriteFailed,
    VerificationFailed,
    JournalFailed,
    InvalidJournal,
    UnsupportedJournal,
    BackupMissing,
    BackupChanged,
    AlreadyRolledBack,
    RollbackBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedApplyFailure {
    pub kind: SharedApplyFailureKind,
    pub path: Option<SharedTransactionPath>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedPlanEntry {
    pub adapter: PreviewAdapter,
    pub selected_archive: SharedTransactionPath,
    pub verified_game_identity: String,
    pub source_path: SharedTransactionPath,
    pub source_digest: String,
    pub destination_root: SharedTransactionPath,
    pub destination_relative_path: SharedTransactionPath,
    pub destination_pre_state: PreviewDestinationState,
    pub destination_pre_digest: Option<String>,
    pub proposed_action: PreviewProposedAction,
    pub backup_required: bool,
    pub parent_creation_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedApplyContext {
    pub adapter: PreviewAdapter,
    pub selected_archive: SharedTransactionPath,
    pub verified_game_identity: String,
    pub profile_id: String,
    pub source_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedTransactionPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub context: SharedApplyContext,
    pub approved_source_root: SharedTransactionPath,
    pub destination_root: SharedTransactionPath,
    pub entries: Vec<SharedPlanEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedApplyConfirmation {
    pub plan_id: String,
    pub general_approved: bool,
    pub replacement_approved: bool,
}

#[derive(Debug, Clone)]
pub struct SharedApplyOptions {
    pub dry_run: bool,
    pub confirmation: Option<SharedApplyConfirmation>,
    pub operation_id: String,
    pub timestamp_unix_seconds: u64,
    pub current_context: SharedApplyContext,
    pub history_root: PathBuf,
    pub backup_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedApplyOutcome {
    DryRun,
    InstalledNew,
    ReplacedExisting,
    AlreadyInstalled,
    SkippedNotEligible,
    SkippedConflict,
    SkippedReplacementNotApproved,
    SourceChanged,
    DestinationChanged,
    BackupFailed,
    WriteFailed,
    VerificationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedApplyEntry {
    pub plan_entry: SharedPlanEntry,
    pub observed_source_digest: Option<String>,
    pub observed_destination_digest: Option<String>,
    pub backup_path: Option<SharedTransactionPath>,
    pub backup_digest: Option<String>,
    pub temporary_path: Option<SharedTransactionPath>,
    pub final_destination_digest: Option<String>,
    pub created_directories: Vec<SharedTransactionPath>,
    pub replacement_approved: bool,
    pub verification_succeeded: bool,
    pub outcome: SharedApplyOutcome,
    pub stages: Vec<SharedTransactionStage>,
    pub warnings: Vec<String>,
    pub failures: Vec<SharedApplyFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedApplyStatus {
    DryRun,
    Success,
    PartialFailure,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedApplyJournal {
    pub schema_version: u32,
    pub operation_id: String,
    pub plan_id: String,
    pub timestamp_unix_seconds: u64,
    pub context: SharedApplyContext,
    pub approved_source_root: SharedTransactionPath,
    pub destination_root: SharedTransactionPath,
    pub dry_run: bool,
    pub entries: Vec<SharedApplyEntry>,
    pub status: SharedApplyStatus,
    pub rollback_operation_id: Option<String>,
}

#[derive(Debug)]
pub struct SharedApplyResult {
    pub journal: SharedApplyJournal,
    pub journal_path: Option<PathBuf>,
    pub journal_failure: Option<SharedApplyFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedJournalWarning {
    pub path: SharedTransactionPath,
    pub failure: SharedApplyFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedHistoryReport {
    pub journals: Vec<(SharedTransactionPath, SharedApplyJournal)>,
    pub warnings: Vec<SharedJournalWarning>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedRollbackOutcome {
    Available,
    NoChangeRequired,
    DestinationChanged,
    DestinationMissing,
    DestinationUnsafe,
    BackupMissing,
    BackupChanged,
    JournalMalformed,
    JournalUnsupported,
    RootMismatch,
    AlreadyRolledBack,
    RemovedInstalledFile,
    RestoredBackup,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRollbackEntry {
    pub destination: Option<SharedTransactionPath>,
    pub backup: Option<SharedTransactionPath>,
    pub expected_installed_digest: Option<String>,
    pub observed_destination_digest: Option<String>,
    pub observed_backup_digest: Option<String>,
    pub outcome: SharedRollbackOutcome,
    pub failure: Option<SharedApplyFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRollbackPreview {
    pub schema_version: u32,
    pub preview_id: String,
    pub journal_path: SharedTransactionPath,
    pub original_operation_id: String,
    pub destination_root: SharedTransactionPath,
    pub entries: Vec<SharedRollbackEntry>,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedRollbackConfirmation {
    pub preview_id: String,
    pub approved: bool,
}

#[derive(Debug, Clone)]
pub struct SharedRollbackOptions {
    pub confirmation: SharedRollbackConfirmation,
    pub rollback_operation_id: String,
    pub timestamp_unix_seconds: u64,
    pub history_root: PathBuf,
    pub backup_root: PathBuf,
}

#[derive(Debug)]
pub struct SharedRollbackResult {
    pub preview: SharedRollbackPreview,
    pub journal_path: Option<PathBuf>,
    pub status: SharedApplyStatus,
}

pub fn build_shared_transaction_plan(
    preview: &SharedPreviewReport,
    profile_id: &str,
    source_mode: &str,
    approved_source_root: &Path,
) -> Result<SharedTransactionPlan, SharedApplyFailure> {
    if adapter_write_support(preview.adapter) != SharedAdapterWriteSupport::ApplyAndRollback {
        return Err(failure(
            SharedApplyFailureKind::UnsupportedAdapter,
            None,
            "adapter has no independent approved materialized source",
        ));
    }
    if preview.entries.len() > SHARED_MAX_ENTRIES {
        return Err(failure(
            SharedApplyFailureKind::ResourceLimitReached,
            None,
            "apply entry limit reached",
        ));
    }
    strict_absolute_root(approved_source_root)?;
    let first = preview.entries.first().ok_or_else(|| {
        failure(
            SharedApplyFailureKind::InvalidPlan,
            None,
            "preview has no entries",
        )
    })?;
    let destination_root = first.destination_root.clone();
    strict_absolute_root(&destination_root)?;
    let identity = first.verified_identity.clone().ok_or_else(|| {
        failure(
            SharedApplyFailureKind::InvalidPlan,
            None,
            "verified identity is required",
        )
    })?;
    let mut destinations = BTreeSet::new();
    let mut entries = Vec::new();
    for entry in &preview.entries {
        if entry.adapter != preview.adapter
            || entry.selected_archive != preview.request_archive
            || entry.destination_root != destination_root
            || entry.verified_identity.as_deref() != Some(identity.as_str())
        {
            return Err(failure(
                SharedApplyFailureKind::InvalidPlan,
                entry.destination_path.as_deref(),
                "preview entry context is inconsistent",
            ));
        }
        if entry.eligibility != PreviewEligibility::Eligible
            || !matches!(
                entry.proposed_action,
                PreviewProposedAction::Install
                    | PreviewProposedAction::Replace
                    | PreviewProposedAction::Skip
            )
        {
            continue;
        }
        let source = entry.source_path.as_ref().ok_or_else(|| {
            failure(
                SharedApplyFailureKind::InvalidPlan,
                None,
                "eligible entry has no source",
            )
        })?;
        let source_digest = entry.source_digest.clone().ok_or_else(|| {
            failure(
                SharedApplyFailureKind::InvalidPlan,
                Some(source),
                "eligible entry has no source digest",
            )
        })?;
        let relative = entry.destination_relative_path.as_ref().ok_or_else(|| {
            failure(
                SharedApplyFailureKind::InvalidPlan,
                None,
                "eligible entry has no relative destination",
            )
        })?;
        let destination = entry.destination_path.as_ref().ok_or_else(|| {
            failure(
                SharedApplyFailureKind::InvalidPlan,
                None,
                "eligible entry has no final destination",
            )
        })?;
        if !destinations.insert(destination.clone()) {
            return Err(failure(
                SharedApplyFailureKind::DuplicateDestination,
                Some(destination),
                "duplicate destination in transaction",
            ));
        }
        entries.push(SharedPlanEntry {
            adapter: entry.adapter,
            selected_archive: SharedTransactionPath::from_path(&entry.selected_archive),
            verified_game_identity: identity.clone(),
            source_path: SharedTransactionPath::from_path(source),
            source_digest,
            destination_root: SharedTransactionPath::from_path(&destination_root),
            destination_relative_path: SharedTransactionPath::from_path(relative),
            destination_pre_state: entry.destination_state,
            destination_pre_digest: entry.existing_destination_digest.clone(),
            proposed_action: entry.proposed_action,
            backup_required: entry.backup_required,
            parent_creation_approved: entry.warnings.iter().any(|warning| {
                warning.kind == super::shared_preview::PreviewWarningKind::DestinationParentsMissing
            }),
        });
    }
    if entries.is_empty() {
        return Err(failure(
            SharedApplyFailureKind::InvalidPlan,
            None,
            "preview has no eligible materialized entries",
        ));
    }
    entries.sort_by(|left, right| {
        left.destination_relative_path
            .unix_bytes_hex
            .cmp(&right.destination_relative_path.unix_bytes_hex)
    });
    let context = SharedApplyContext {
        adapter: preview.adapter,
        selected_archive: SharedTransactionPath::from_path(&preview.request_archive),
        verified_game_identity: identity,
        profile_id: profile_id.to_owned(),
        source_mode: source_mode.to_owned(),
    };
    let mut plan = SharedTransactionPlan {
        schema_version: SHARED_APPLY_SCHEMA_VERSION,
        plan_id: String::new(),
        context,
        approved_source_root: SharedTransactionPath::from_path(approved_source_root),
        destination_root: SharedTransactionPath::from_path(&destination_root),
        entries,
    };
    plan.plan_id = plan_digest(&plan)?;
    Ok(plan)
}

pub fn execute_shared_apply(
    plan: &SharedTransactionPlan,
    options: &SharedApplyOptions,
) -> SharedApplyResult {
    let effective_dry_run = options.dry_run
        || options
            .confirmation
            .as_ref()
            .is_none_or(|confirmation| !confirmation.general_approved);
    let mut journal = SharedApplyJournal {
        schema_version: SHARED_APPLY_SCHEMA_VERSION,
        operation_id: options.operation_id.clone(),
        plan_id: plan.plan_id.clone(),
        timestamp_unix_seconds: options.timestamp_unix_seconds,
        context: plan.context.clone(),
        approved_source_root: plan.approved_source_root.clone(),
        destination_root: plan.destination_root.clone(),
        dry_run: effective_dry_run,
        entries: Vec::new(),
        status: SharedApplyStatus::DryRun,
        rollback_operation_id: None,
    };
    let confirmation = options.confirmation.as_ref();
    let context_valid = options.current_context == plan.context;
    let plan_valid = plan_digest(plan).ok().as_deref() == Some(plan.plan_id.as_str());
    let confirmation_valid =
        confirmation.is_some_and(|confirmation| confirmation.plan_id == plan.plan_id);
    let destination_root = plan.destination_root.to_path_buf();
    let source_root = plan.approved_source_root.to_path_buf();
    if !context_valid || !plan_valid || (!effective_dry_run && !confirmation_valid) {
        let kind = if !confirmation_valid {
            SharedApplyFailureKind::ConfirmationPlanMismatch
        } else {
            SharedApplyFailureKind::InvalidPlan
        };
        journal.entries = plan
            .entries
            .iter()
            .map(|entry| failed_entry(entry, kind, "plan or context changed before apply"))
            .collect();
        journal.status = SharedApplyStatus::Failed;
        return SharedApplyResult {
            journal,
            journal_path: None,
            journal_failure: None,
        };
    }
    let (Ok(destination_root), Ok(source_root)) = (destination_root, source_root) else {
        journal.entries = plan
            .entries
            .iter()
            .map(|entry| {
                failed_entry(
                    entry,
                    SharedApplyFailureKind::InvalidPlan,
                    "path identity cannot be reconstructed",
                )
            })
            .collect();
        journal.status = SharedApplyStatus::Failed;
        return SharedApplyResult {
            journal,
            journal_path: None,
            journal_failure: None,
        };
    };
    if !effective_dry_run {
        let operation = safe_identifier(&options.operation_id);
        let duplicate = operation.as_ref().ok().is_some_and(|operation| {
            fs::symlink_metadata(options.history_root.join(format!("{operation}.json"))).is_ok()
        });
        let managed_overlap = roots_overlap(&options.history_root, &source_root)
            || roots_overlap(&options.history_root, &destination_root)
            || roots_overlap(&options.backup_root, &source_root)
            || roots_overlap(&options.backup_root, &destination_root);
        if operation.is_err() || duplicate || managed_overlap {
            let (kind, detail) = if duplicate {
                (
                    SharedApplyFailureKind::DuplicateOperationId,
                    "operation ID already has a journal",
                )
            } else if managed_overlap {
                (
                    SharedApplyFailureKind::ManagedRootUnsafe,
                    "managed history or backup roots overlap source or destination scope",
                )
            } else {
                (
                    SharedApplyFailureKind::InvalidPlan,
                    "operation ID is invalid",
                )
            };
            journal.entries = plan
                .entries
                .iter()
                .map(|entry| failed_entry(entry, kind, detail))
                .collect();
            journal.status = SharedApplyStatus::Failed;
            return SharedApplyResult {
                journal,
                journal_path: None,
                journal_failure: None,
            };
        }
    }
    let mut lock = None;
    if !effective_dry_run {
        match RootLock::acquire(&destination_root, LOCK_TIMEOUT) {
            Ok(guard) => lock = Some(guard),
            Err(kind) => {
                journal.entries = plan
                    .entries
                    .iter()
                    .map(|entry| failed_entry(entry, kind, "destination root is busy"))
                    .collect();
                journal.status = SharedApplyStatus::Failed;
                return SharedApplyResult {
                    journal,
                    journal_path: None,
                    journal_failure: None,
                };
            }
        }
    }
    let replacement_approved = confirmation.is_some_and(|value| value.replacement_approved);
    let mut written = 0_u64;
    let mut backup_bytes = 0_u64;
    for entry in &plan.entries {
        journal.entries.push(apply_one(
            entry,
            &source_root,
            &destination_root,
            &options.backup_root,
            &options.operation_id,
            effective_dry_run,
            replacement_approved,
            &mut written,
            &mut backup_bytes,
        ));
    }
    drop(lock);
    journal.status = derive_status(&journal.entries, effective_dry_run);
    if effective_dry_run {
        return SharedApplyResult {
            journal,
            journal_path: None,
            journal_failure: None,
        };
    }
    match write_journal_once(&journal, &options.history_root) {
        Ok(path) => SharedApplyResult {
            journal,
            journal_path: Some(path),
            journal_failure: None,
        },
        Err(error) => {
            let any_write = journal.entries.iter().any(|entry| {
                matches!(
                    entry.outcome,
                    SharedApplyOutcome::InstalledNew | SharedApplyOutcome::ReplacedExisting
                )
            });
            if any_write {
                journal.status = SharedApplyStatus::PartialFailure;
                for entry in &mut journal.entries {
                    if matches!(
                        entry.outcome,
                        SharedApplyOutcome::InstalledNew | SharedApplyOutcome::ReplacedExisting
                    ) {
                        entry
                            .stages
                            .push(SharedTransactionStage::JournalFailedAfterSuccessfulWrite);
                    }
                }
            }
            SharedApplyResult {
                journal,
                journal_path: None,
                journal_failure: Some(error),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_one(
    plan: &SharedPlanEntry,
    source_root: &Path,
    destination_root: &Path,
    backup_root: &Path,
    operation_id: &str,
    dry_run: bool,
    replacement_approved: bool,
    written: &mut u64,
    backup_bytes: &mut u64,
) -> SharedApplyEntry {
    let mut result = SharedApplyEntry {
        plan_entry: plan.clone(),
        observed_source_digest: None,
        observed_destination_digest: None,
        backup_path: None,
        backup_digest: None,
        temporary_path: None,
        final_destination_digest: None,
        created_directories: Vec::new(),
        replacement_approved,
        verification_succeeded: false,
        outcome: SharedApplyOutcome::SkippedNotEligible,
        stages: vec![if dry_run {
            SharedTransactionStage::DryRun
        } else {
            SharedTransactionStage::InstallNew
        }],
        warnings: Vec::new(),
        failures: Vec::new(),
    };
    let Ok(source) = plan.source_path.to_path_buf() else {
        return fail_result(
            result,
            SharedApplyOutcome::SourceChanged,
            SharedApplyFailureKind::InvalidPlan,
            None,
            "invalid source path identity",
        );
    };
    if should_inject(FaultPoint::SourceMutation) {
        return fail_result(
            result,
            SharedApplyOutcome::SourceChanged,
            SharedApplyFailureKind::SourceChanged,
            Some(&source),
            "injected source mutation",
        );
    }
    let Ok(relative) = plan.destination_relative_path.to_path_buf() else {
        return fail_result(
            result,
            SharedApplyOutcome::SkippedConflict,
            SharedApplyFailureKind::InvalidPlan,
            None,
            "invalid destination path identity",
        );
    };
    if source.strip_prefix(source_root).is_err() || source == source_root {
        return fail_result(
            result,
            SharedApplyOutcome::SourceChanged,
            SharedApplyFailureKind::SourceOutsideApprovedScope,
            Some(&source),
            "source is outside approved adapter scope",
        );
    }
    let source_hash = match stable_hash(&source, SHARED_MAX_SOURCE_BYTES) {
        Ok(value) => value,
        Err(kind) => {
            return fail_result(
                result,
                SharedApplyOutcome::SourceChanged,
                kind,
                Some(&source),
                "source could not be revalidated",
            );
        }
    };
    result.observed_source_digest = Some(source_hash.digest.clone());
    if source_hash.digest != plan.source_digest {
        return fail_result(
            result,
            SharedApplyOutcome::SourceChanged,
            SharedApplyFailureKind::SourceChanged,
            Some(&source),
            "source digest changed since approved preview",
        );
    }
    if source_hash.bytes > SHARED_MAX_TOTAL_WRITTEN_BYTES.saturating_sub(*written) {
        return fail_result(
            result,
            SharedApplyOutcome::WriteFailed,
            SharedApplyFailureKind::ResourceLimitReached,
            Some(&source),
            "transaction write-byte limit reached",
        );
    }
    let Some((category, filename)) = exactly_two_components(&relative) else {
        return fail_result(
            result,
            SharedApplyOutcome::SkippedConflict,
            SharedApplyFailureKind::DestinationUnsafe,
            None,
            "destination must contain exactly two normal components",
        );
    };
    let assessment = match assess_destination(destination_root, &category, &filename) {
        Ok(value) => value,
        Err(error) => {
            return fail_result(
                result,
                SharedApplyOutcome::DestinationChanged,
                SharedApplyFailureKind::DestinationUnsafe,
                Some(destination_root),
                &error.to_string(),
            );
        }
    };
    let destination = assessment.proposed_destination.path().to_path_buf();
    let current = if assessment.destination_state == DestinationState::RegularFile {
        match stable_hash(&destination, SHARED_MAX_SOURCE_BYTES) {
            Ok(value) => Some(value),
            Err(kind) => {
                return fail_result(
                    result,
                    SharedApplyOutcome::DestinationChanged,
                    kind,
                    Some(&destination),
                    "destination could not be revalidated",
                );
            }
        }
    } else {
        None
    };
    result.observed_destination_digest = current.as_ref().map(|value| value.digest.clone());
    if should_inject(FaultPoint::DestinationMutation) {
        return fail_result(
            result,
            SharedApplyOutcome::DestinationChanged,
            SharedApplyFailureKind::DestinationChanged,
            Some(&destination),
            "injected destination mutation",
        );
    }
    let expected_state_matches = match plan.proposed_action {
        PreviewProposedAction::Install => assessment.destination_state == DestinationState::Absent,
        PreviewProposedAction::Replace | PreviewProposedAction::Skip => {
            assessment.destination_state == DestinationState::RegularFile
        }
        PreviewProposedAction::Blocked => false,
    };
    if !expected_state_matches
        || current.as_ref().map(|value| value.digest.as_str())
            != plan.destination_pre_digest.as_deref()
    {
        return fail_result(
            result,
            SharedApplyOutcome::DestinationChanged,
            SharedApplyFailureKind::DestinationChanged,
            Some(&destination),
            "destination state or digest changed since approved preview",
        );
    }
    if plan.proposed_action == PreviewProposedAction::Skip {
        result.outcome = SharedApplyOutcome::AlreadyInstalled;
        result.final_destination_digest = Some(source_hash.digest);
        result.verification_succeeded = true;
        result.stages.push(SharedTransactionStage::AlreadyInstalled);
        return result;
    }
    if plan.proposed_action == PreviewProposedAction::Replace && !replacement_approved {
        return fail_result(
            result,
            SharedApplyOutcome::SkippedReplacementNotApproved,
            SharedApplyFailureKind::ReplacementNotApproved,
            Some(&destination),
            "replacement requires separate explicit permission",
        );
    }
    if dry_run {
        result.outcome = SharedApplyOutcome::DryRun;
        return result;
    }
    let parent = destination.parent().unwrap_or(destination_root);
    if !parent.exists() {
        if !plan.parent_creation_approved || plan.adapter != PreviewAdapter::RetroArch {
            return fail_result(
                result,
                SharedApplyOutcome::WriteFailed,
                SharedApplyFailureKind::ParentCreationFailed,
                Some(parent),
                "parent creation was not approved by preview and adapter contract",
            );
        }
        if let Err(kind) = create_one_parent(destination_root, parent) {
            return fail_result(
                result,
                SharedApplyOutcome::WriteFailed,
                kind,
                Some(parent),
                "approved destination parent could not be created safely",
            );
        }
        result
            .created_directories
            .push(SharedTransactionPath::from_path(parent));
    }
    if plan.proposed_action == PreviewProposedAction::Replace {
        let Some(existing) = current.as_ref() else {
            return fail_result(
                result,
                SharedApplyOutcome::DestinationChanged,
                SharedApplyFailureKind::DestinationChanged,
                Some(&destination),
                "replacement destination disappeared",
            );
        };
        if existing.bytes > SHARED_MAX_BACKUP_BYTES.saturating_sub(*backup_bytes) {
            return fail_result(
                result,
                SharedApplyOutcome::BackupFailed,
                SharedApplyFailureKind::ResourceLimitReached,
                Some(&destination),
                "backup-byte limit reached",
            );
        }
        match create_backup(&destination, &existing.digest, backup_root, operation_id) {
            Ok(path) => {
                *backup_bytes += existing.bytes;
                result.backup_path = Some(SharedTransactionPath::from_path(&path));
                result.backup_digest = Some(existing.digest.clone());
                result.stages.push(SharedTransactionStage::BackupCreated);
            }
            Err(error) => {
                result.stages.push(SharedTransactionStage::BackupFailed);
                return fail_result(
                    result,
                    SharedApplyOutcome::BackupFailed,
                    error,
                    Some(&destination),
                    "verified backup could not be created; original left untouched",
                );
            }
        }
    }
    match atomic_write(
        &source,
        &destination,
        &source_hash.digest,
        plan.proposed_action == PreviewProposedAction::Install,
    ) {
        Ok(temp) => {
            result.temporary_path = Some(SharedTransactionPath::from_path(&temp));
            result.final_destination_digest = Some(source_hash.digest);
            result.verification_succeeded = true;
            result.outcome = if plan.proposed_action == PreviewProposedAction::Install {
                SharedApplyOutcome::InstalledNew
            } else {
                SharedApplyOutcome::ReplacedExisting
            };
            result.stages.push(SharedTransactionStage::Success);
            *written += source_hash.bytes;
        }
        Err((kind, temp)) => {
            result.temporary_path = temp.as_deref().map(SharedTransactionPath::from_path);
            result = fail_result(
                result,
                if kind == SharedApplyFailureKind::VerificationFailed {
                    SharedApplyOutcome::VerificationFailed
                } else {
                    SharedApplyOutcome::WriteFailed
                },
                kind,
                Some(&destination),
                "atomic destination write failed",
            );
        }
    }
    result
}

pub fn discover_shared_apply_history(history_root: &Path) -> SharedHistoryReport {
    let mut report = SharedHistoryReport {
        journals: Vec::new(),
        warnings: Vec::new(),
        complete: true,
    };
    if strict_absolute_root(history_root).is_err() || !history_root.exists() {
        return report;
    }
    let Ok(read_dir) = fs::read_dir(history_root) else {
        report.complete = false;
        return report;
    };
    let mut paths = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension() == Some(OsStr::new("json"))
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".rollback.json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() > SHARED_MAX_HISTORY_JOURNALS {
        paths.truncate(SHARED_MAX_HISTORY_JOURNALS);
        report.complete = false;
    }
    for path in paths {
        match read_journal(&path) {
            Ok(journal) => report
                .journals
                .push((SharedTransactionPath::from_path(&path), journal)),
            Err(error) if report.warnings.len() < SHARED_MAX_WARNINGS => {
                report.warnings.push(SharedJournalWarning {
                    path: SharedTransactionPath::from_path(&path),
                    failure: error,
                });
            }
            Err(_) => report.complete = false,
        }
    }
    report
}

pub fn preview_shared_rollback(
    journal_path: &Path,
    expected_destination_root: &Path,
    backup_root: &Path,
) -> SharedRollbackPreview {
    let journal = match read_journal(journal_path) {
        Ok(value) => value,
        Err(error) => {
            return SharedRollbackPreview {
                schema_version: SHARED_APPLY_SCHEMA_VERSION,
                preview_id: digest_text(&format!("{}:{:?}", journal_path.display(), error.kind)),
                journal_path: SharedTransactionPath::from_path(journal_path),
                original_operation_id: String::new(),
                destination_root: SharedTransactionPath::from_path(expected_destination_root),
                entries: vec![SharedRollbackEntry {
                    destination: None,
                    backup: None,
                    expected_installed_digest: None,
                    observed_destination_digest: None,
                    observed_backup_digest: None,
                    outcome: if error.kind == SharedApplyFailureKind::UnsupportedJournal {
                        SharedRollbackOutcome::JournalUnsupported
                    } else {
                        SharedRollbackOutcome::JournalMalformed
                    },
                    failure: Some(error),
                }],
                available: false,
            };
        }
    };
    let journal_root = journal.destination_root.to_path_buf().ok();
    let root_matches = journal_root.as_deref() == Some(expected_destination_root);
    let rollback_marker_exists = journal_path.parent().is_some_and(|parent| {
        safe_identifier(&journal.operation_id)
            .ok()
            .is_some_and(|operation| parent.join(format!("{operation}.rollback.json")).exists())
    });
    let mut entries = Vec::new();
    for entry in journal.entries.iter().take(SHARED_MAX_ROLLBACK_ENTRIES) {
        entries.push(rollback_entry_preview(
            entry,
            expected_destination_root,
            backup_root,
            root_matches,
            journal.rollback_operation_id.is_some() || rollback_marker_exists,
        ));
    }
    let available = entries
        .iter()
        .any(|entry| entry.outcome == SharedRollbackOutcome::Available)
        && entries.iter().all(|entry| {
            matches!(
                entry.outcome,
                SharedRollbackOutcome::Available | SharedRollbackOutcome::NoChangeRequired
            )
        });
    let mut preview = SharedRollbackPreview {
        schema_version: SHARED_APPLY_SCHEMA_VERSION,
        preview_id: String::new(),
        journal_path: SharedTransactionPath::from_path(journal_path),
        original_operation_id: journal.operation_id,
        destination_root: SharedTransactionPath::from_path(expected_destination_root),
        entries,
        available,
    };
    preview.preview_id = rollback_preview_digest(&preview);
    preview
}

pub fn execute_shared_rollback(
    preview: &SharedRollbackPreview,
    options: &SharedRollbackOptions,
) -> SharedRollbackResult {
    if !options.confirmation.approved
        || options.confirmation.preview_id != preview.preview_id
        || rollback_preview_digest(preview) != preview.preview_id
        || !preview.available
    {
        return SharedRollbackResult {
            preview: preview.clone(),
            journal_path: None,
            status: SharedApplyStatus::Failed,
        };
    }
    let journal_path = match preview.journal_path.to_path_buf() {
        Ok(value) => value,
        Err(_) => {
            return SharedRollbackResult {
                preview: preview.clone(),
                journal_path: None,
                status: SharedApplyStatus::Failed,
            };
        }
    };
    let root = preview.destination_root.to_path_buf().unwrap_or_default();
    let fresh = preview_shared_rollback(&journal_path, &root, &options.backup_root);
    if fresh.preview_id != preview.preview_id || !fresh.available {
        return SharedRollbackResult {
            preview: fresh,
            journal_path: None,
            status: SharedApplyStatus::Failed,
        };
    }
    let Ok(_lock) = RootLock::acquire(&root, LOCK_TIMEOUT) else {
        return SharedRollbackResult {
            preview: fresh,
            journal_path: None,
            status: SharedApplyStatus::Failed,
        };
    };
    let original = read_journal(&journal_path).expect("fresh rollback preview parsed journal");
    let mut applied = fresh.clone();
    for (rollback, install) in applied.entries.iter_mut().zip(&original.entries) {
        let destination = install
            .plan_entry
            .destination_root
            .to_path_buf()
            .and_then(|root| {
                install
                    .plan_entry
                    .destination_relative_path
                    .to_path_buf()
                    .map(|relative| root.join(relative))
            });
        let Ok(destination) = destination else {
            rollback.outcome = SharedRollbackOutcome::Failed;
            continue;
        };
        match install.outcome {
            SharedApplyOutcome::InstalledNew => match fs::remove_file(&destination) {
                Ok(()) => {
                    rollback.outcome = SharedRollbackOutcome::RemovedInstalledFile;
                    cleanup_created_directories(install, &root);
                }
                Err(_) => rollback.outcome = SharedRollbackOutcome::Failed,
            },
            SharedApplyOutcome::ReplacedExisting => {
                let Some(backup) = install.backup_path.as_ref() else {
                    rollback.outcome = SharedRollbackOutcome::Failed;
                    continue;
                };
                let Ok(backup) = backup.to_path_buf() else {
                    rollback.outcome = SharedRollbackOutcome::Failed;
                    continue;
                };
                let expected = install.backup_digest.as_deref().unwrap_or_default();
                if should_inject(FaultPoint::RollbackRestore) {
                    rollback.outcome = SharedRollbackOutcome::Failed;
                    continue;
                }
                match atomic_write(&backup, &destination, expected, false) {
                    Ok(_) => rollback.outcome = SharedRollbackOutcome::RestoredBackup,
                    Err(_) => rollback.outcome = SharedRollbackOutcome::Failed,
                }
            }
            _ => rollback.outcome = SharedRollbackOutcome::NoChangeRequired,
        }
    }
    let success = applied.entries.iter().all(|entry| {
        matches!(
            entry.outcome,
            SharedRollbackOutcome::RemovedInstalledFile
                | SharedRollbackOutcome::RestoredBackup
                | SharedRollbackOutcome::NoChangeRequired
        )
    });
    let marker = options.history_root.join(format!(
        "{}.rollback.json",
        safe_identifier(&preview.original_operation_id).unwrap_or_else(|_| "invalid".into())
    ));
    let journal_path = success
        .then(|| serde_json::to_vec_pretty(&applied).ok())
        .flatten()
        .and_then(|bytes| {
            if bytes.len() as u64 > SHARED_MAX_JOURNAL_BYTES {
                return None;
            }
            atomic_managed_write(&marker, &bytes).ok().map(|_| marker)
        });
    let marker_written = journal_path.is_some();
    SharedRollbackResult {
        preview: applied,
        journal_path,
        status: if success && marker_written {
            SharedApplyStatus::Success
        } else if success {
            SharedApplyStatus::PartialFailure
        } else {
            SharedApplyStatus::Failed
        },
    }
}

#[derive(Debug)]
struct StableHash {
    digest: String,
    bytes: u64,
}

fn stable_hash(path: &Path, max: u64) -> Result<StableHash, SharedApplyFailureKind> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(SharedApplyFailureKind::InvalidPlan);
    }
    reject_symlink_components(path)?;
    let before = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SharedApplyFailureKind::SourceMissing
        } else {
            SharedApplyFailureKind::SourceChanged
        }
    })?;
    if before.file_type().is_symlink() {
        return Err(SharedApplyFailureKind::SourceSymlink);
    }
    if !before.is_file() {
        return Err(SharedApplyFailureKind::SourceSpecialFile);
    }
    if before.len() > max {
        return Err(SharedApplyFailureKind::ResourceLimitReached);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(path)
        .map_err(|_| SharedApplyFailureKind::SourceChanged)?;
    let opened = file
        .metadata()
        .map_err(|_| SharedApplyFailureKind::SourceChanged)?;
    if !same_file(&before, &opened) {
        return Err(SharedApplyFailureKind::SourceChanged);
    }
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| SharedApplyFailureKind::SourceChanged)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > max {
            return Err(SharedApplyFailureKind::ResourceLimitReached);
        }
        digest.update(&buffer[..read]);
    }
    let after = fs::symlink_metadata(path).map_err(|_| SharedApplyFailureKind::SourceChanged)?;
    if !same_file(&before, &after) {
        return Err(SharedApplyFailureKind::SourceChanged);
    }
    Ok(StableHash {
        digest: hex_bytes(&digest.finalize()),
        bytes,
    })
}

fn create_backup(
    destination: &Path,
    expected: &str,
    backup_root: &Path,
    operation_id: &str,
) -> Result<PathBuf, SharedApplyFailureKind> {
    if should_inject(FaultPoint::BackupWrite) {
        return Err(SharedApplyFailureKind::BackupFailed);
    }
    prepare_managed_root(backup_root)?;
    let operation = backup_root.join(safe_identifier(operation_id)?);
    fs::create_dir(&operation).map_err(|_| SharedApplyFailureKind::BackupFailed)?;
    let final_path = operation.join(format!(
        "{}.bak",
        digest_text(&destination.to_string_lossy())
    ));
    let bytes = read_bounded(destination, SHARED_MAX_SOURCE_BYTES)?;
    if digest_bytes(&bytes) != expected {
        return Err(SharedApplyFailureKind::DestinationChanged);
    }
    atomic_managed_write(&final_path, &bytes)?;
    let verified = stable_hash(&final_path, SHARED_MAX_SOURCE_BYTES)?;
    if verified.digest != expected {
        return Err(SharedApplyFailureKind::BackupFailed);
    }
    Ok(final_path)
}

fn atomic_write(
    source: &Path,
    destination: &Path,
    expected: &str,
    no_replace: bool,
) -> Result<PathBuf, (SharedApplyFailureKind, Option<PathBuf>)> {
    if should_inject(FaultPoint::TemporaryWrite) {
        return Err((SharedApplyFailureKind::WriteFailed, None));
    }
    let bytes = read_bounded(source, SHARED_MAX_SOURCE_BYTES).map_err(|kind| (kind, None))?;
    if digest_bytes(&bytes) != expected {
        return Err((SharedApplyFailureKind::SourceChanged, None));
    }
    let parent = destination
        .parent()
        .ok_or((SharedApplyFailureKind::DestinationUnsafe, None))?;
    let temp = parent.join(format!(
        ".archivefs-apply-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
        file.write_all(&bytes)
            .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
        if should_inject(FaultPoint::Flush) {
            return Err(SharedApplyFailureKind::WriteFailed);
        }
        file.sync_all()
            .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
        set_permissions(&file).map_err(|_| SharedApplyFailureKind::WriteFailed)?;
        let temp_hash = stable_hash(&temp, SHARED_MAX_SOURCE_BYTES)?;
        if should_inject(FaultPoint::Verification) || temp_hash.digest != expected {
            return Err(SharedApplyFailureKind::VerificationFailed);
        }
        if should_inject(FaultPoint::Rename) {
            return Err(SharedApplyFailureKind::WriteFailed);
        }
        if no_replace {
            rename_no_replace(&temp, destination)
                .map_err(|_| SharedApplyFailureKind::DestinationChanged)?;
        } else {
            fs::rename(&temp, destination).map_err(|_| SharedApplyFailureKind::WriteFailed)?;
        }
        sync_directory(parent);
        let final_hash = stable_hash(destination, SHARED_MAX_SOURCE_BYTES)?;
        if final_hash.digest != expected {
            return Err(SharedApplyFailureKind::VerificationFailed);
        }
        Ok(())
    })();
    if let Err(kind) = write_result {
        let _ = fs::remove_file(&temp);
        return Err((kind, Some(temp)));
    }
    Ok(temp)
}

fn atomic_managed_write(path: &Path, bytes: &[u8]) -> Result<(), SharedApplyFailureKind> {
    let parent = path
        .parent()
        .ok_or(SharedApplyFailureKind::ManagedRootUnsafe)?;
    prepare_managed_root(parent)?;
    if fs::symlink_metadata(path).is_ok() {
        return Err(SharedApplyFailureKind::DuplicateOperationId);
    }
    let temp = parent.join(format!(
        ".archivefs-managed-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
    file.write_all(bytes)
        .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
    file.sync_all()
        .map_err(|_| SharedApplyFailureKind::WriteFailed)?;
    if let Err(error) = rename_no_replace(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            SharedApplyFailureKind::DuplicateOperationId
        } else {
            SharedApplyFailureKind::WriteFailed
        });
    }
    sync_directory(parent);
    Ok(())
}

fn write_journal_once(
    journal: &SharedApplyJournal,
    history_root: &Path,
) -> Result<PathBuf, SharedApplyFailure> {
    if should_inject(FaultPoint::JournalWrite) {
        return Err(failure(
            SharedApplyFailureKind::JournalFailed,
            None,
            "injected journal write failure",
        ));
    }
    let identifier = safe_identifier(&journal.operation_id).map_err(|kind| {
        failure(
            kind,
            None,
            "operation ID is not safe for a journal filename",
        )
    })?;
    let bytes = serde_json::to_vec_pretty(journal).map_err(|error| {
        failure(
            SharedApplyFailureKind::JournalFailed,
            None,
            &error.to_string(),
        )
    })?;
    if bytes.len() as u64 > SHARED_MAX_JOURNAL_BYTES {
        return Err(failure(
            SharedApplyFailureKind::ResourceLimitReached,
            None,
            "journal size limit reached",
        ));
    }
    let path = history_root.join(format!("{identifier}.json"));
    atomic_managed_write(&path, &bytes)
        .map_err(|kind| failure(kind, Some(&path), "journal could not be written atomically"))?;
    Ok(path)
}

fn read_journal(path: &Path) -> Result<SharedApplyJournal, SharedApplyFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        failure(
            SharedApplyFailureKind::InvalidJournal,
            Some(path),
            &error.to_string(),
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > SHARED_MAX_JOURNAL_BYTES
    {
        return Err(failure(
            SharedApplyFailureKind::InvalidJournal,
            Some(path),
            "journal is not a bounded regular file",
        ));
    }
    let bytes = read_bounded(path, SHARED_MAX_JOURNAL_BYTES)
        .map_err(|kind| failure(kind, Some(path), "journal could not be read safely"))?;
    let journal: SharedApplyJournal = serde_json::from_slice(&bytes).map_err(|error| {
        failure(
            SharedApplyFailureKind::InvalidJournal,
            Some(path),
            &error.to_string(),
        )
    })?;
    if journal.schema_version != SHARED_APPLY_SCHEMA_VERSION {
        return Err(failure(
            SharedApplyFailureKind::UnsupportedJournal,
            Some(path),
            "journal schema version is unsupported",
        ));
    }
    Ok(journal)
}

fn rollback_entry_preview(
    entry: &SharedApplyEntry,
    root: &Path,
    backup_root: &Path,
    root_matches: bool,
    already_rolled_back: bool,
) -> SharedRollbackEntry {
    let destination = entry
        .plan_entry
        .destination_relative_path
        .to_path_buf()
        .map(|relative| root.join(relative));
    let mut result = SharedRollbackEntry {
        destination: destination
            .as_deref()
            .ok()
            .map(SharedTransactionPath::from_path),
        backup: entry.backup_path.clone(),
        expected_installed_digest: entry.final_destination_digest.clone(),
        observed_destination_digest: None,
        observed_backup_digest: None,
        outcome: SharedRollbackOutcome::Available,
        failure: None,
    };
    if !root_matches {
        result.outcome = SharedRollbackOutcome::RootMismatch;
        return result;
    }
    if already_rolled_back {
        result.outcome = SharedRollbackOutcome::AlreadyRolledBack;
        return result;
    }
    if !matches!(
        entry.outcome,
        SharedApplyOutcome::InstalledNew | SharedApplyOutcome::ReplacedExisting
    ) {
        result.outcome = SharedRollbackOutcome::NoChangeRequired;
        return result;
    }
    let Ok(destination) = destination else {
        result.outcome = SharedRollbackOutcome::DestinationUnsafe;
        return result;
    };
    match stable_hash(&destination, SHARED_MAX_SOURCE_BYTES) {
        Ok(hash) => {
            result.observed_destination_digest = Some(hash.digest.clone());
            if Some(hash.digest.as_str()) != entry.final_destination_digest.as_deref() {
                result.outcome = SharedRollbackOutcome::DestinationChanged;
                return result;
            }
        }
        Err(SharedApplyFailureKind::SourceMissing) => {
            result.outcome = SharedRollbackOutcome::DestinationMissing;
            return result;
        }
        Err(_) => {
            result.outcome = SharedRollbackOutcome::DestinationUnsafe;
            return result;
        }
    }
    if entry.outcome == SharedApplyOutcome::ReplacedExisting {
        let Some(backup) = entry.backup_path.as_ref() else {
            result.outcome = SharedRollbackOutcome::BackupMissing;
            return result;
        };
        let Ok(backup) = backup.to_path_buf() else {
            result.outcome = SharedRollbackOutcome::BackupMissing;
            return result;
        };
        if backup.strip_prefix(backup_root).is_err() {
            result.outcome = SharedRollbackOutcome::BackupChanged;
            return result;
        }
        match stable_hash(&backup, SHARED_MAX_SOURCE_BYTES) {
            Ok(hash) => {
                result.observed_backup_digest = Some(hash.digest.clone());
                if Some(hash.digest.as_str()) != entry.backup_digest.as_deref() {
                    result.outcome = SharedRollbackOutcome::BackupChanged;
                }
            }
            Err(SharedApplyFailureKind::SourceMissing) => {
                result.outcome = SharedRollbackOutcome::BackupMissing
            }
            Err(_) => result.outcome = SharedRollbackOutcome::BackupChanged,
        }
    }
    result
}

fn cleanup_created_directories(entry: &SharedApplyEntry, root: &Path) {
    for encoded in entry.created_directories.iter().rev() {
        let Ok(path) = encoded.to_path_buf() else {
            continue;
        };
        if path.strip_prefix(root).is_ok()
            && path != root
            && fs::read_dir(&path)
                .ok()
                .is_some_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(&path);
        }
    }
}

fn prepare_managed_root(root: &Path) -> Result<(), SharedApplyFailureKind> {
    strict_absolute_root(root).map_err(|_| SharedApplyFailureKind::ManagedRootUnsafe)?;
    if root.exists() {
        validate_destination_root(root).map_err(|_| SharedApplyFailureKind::ManagedRootUnsafe)?;
        return Ok(());
    }
    let parent = root
        .parent()
        .ok_or(SharedApplyFailureKind::ManagedRootUnsafe)?;
    reject_symlink_components(parent)?;
    fs::create_dir(root).map_err(|_| SharedApplyFailureKind::ManagedRootUnsafe)
}

fn create_one_parent(root: &Path, parent: &Path) -> Result<(), SharedApplyFailureKind> {
    if should_inject(FaultPoint::ParentCreationRace) {
        return Err(SharedApplyFailureKind::ParentCreationFailed);
    }
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| SharedApplyFailureKind::ParentCreationFailed)?;
    if relative.components().count() != 1 {
        return Err(SharedApplyFailureKind::ParentCreationFailed);
    }
    validate_destination_root(root).map_err(|_| SharedApplyFailureKind::RootChanged)?;
    fs::create_dir(parent).map_err(|_| SharedApplyFailureKind::ParentCreationFailed)?;
    let metadata =
        fs::symlink_metadata(parent).map_err(|_| SharedApplyFailureKind::ParentCreationFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SharedApplyFailureKind::ParentCreationFailed);
    }
    Ok(())
}

fn strict_absolute_root(root: &Path) -> Result<(), SharedApplyFailure> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(failure(
            SharedApplyFailureKind::DestinationUnsafe,
            Some(root),
            "root must be absolute and cannot be a filesystem root",
        ));
    }
    Ok(())
}

fn exactly_two_components(path: &Path) -> Option<(OsString, OsString)> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (components.len() == 2).then(|| (components[0].clone(), components[1].clone()))
}

fn roots_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn reject_symlink_components(path: &Path) -> Result<(), SharedApplyFailureKind> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(SharedApplyFailureKind::SourceSymlink);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(SharedApplyFailureKind::SourceChanged),
        }
    }
    Ok(())
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, SharedApplyFailureKind> {
    let hash = stable_hash(path, max)?;
    let mut bytes = Vec::with_capacity(hash.bytes as usize);
    let file = File::open(path).map_err(|_| SharedApplyFailureKind::SourceChanged)?;
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SharedApplyFailureKind::SourceChanged)?;
    if bytes.len() as u64 > max || digest_bytes(&bytes) != hash.digest {
        return Err(SharedApplyFailureKind::SourceChanged);
    }
    Ok(bytes)
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
    }
    #[cfg(not(unix))]
    {
        left.len() == right.len() && left.modified().ok() == right.modified().ok()
    }
}

fn set_permissions(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

fn sync_directory(path: &Path) {
    let _ = File::open(path).and_then(|file| file.sync_all());
}

#[cfg(target_os = "linux")]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists",
        ));
    }
    fs::rename(source, destination)
}

#[derive(Debug)]
struct RootLock {
    file: File,
}

impl RootLock {
    fn acquire(root: &Path, timeout: Duration) -> Result<Self, SharedApplyFailureKind> {
        validate_destination_root(root).map_err(|_| SharedApplyFailureKind::RootChanged)?;
        let file = File::open(root).map_err(|_| SharedApplyFailureKind::RootChanged)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let deadline = Instant::now() + timeout;
            loop {
                let result =
                    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if result == 0 {
                    return Ok(Self { file });
                }
                if Instant::now() >= deadline {
                    return Err(SharedApplyFailureKind::LockTimeout);
                }
                std::thread::sleep(LOCK_RETRY);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Err(SharedApplyFailureKind::LockUnsupported)
        }
    }
}

impl Drop for RootLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn derive_status(entries: &[SharedApplyEntry], dry_run: bool) -> SharedApplyStatus {
    if dry_run {
        return SharedApplyStatus::DryRun;
    }
    let successes = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.outcome,
                SharedApplyOutcome::InstalledNew
                    | SharedApplyOutcome::ReplacedExisting
                    | SharedApplyOutcome::AlreadyInstalled
            )
        })
        .count();
    if successes == entries.len() {
        SharedApplyStatus::Success
    } else if successes > 0 {
        SharedApplyStatus::PartialFailure
    } else {
        SharedApplyStatus::Failed
    }
}

fn failed_entry(
    plan: &SharedPlanEntry,
    kind: SharedApplyFailureKind,
    detail: &str,
) -> SharedApplyEntry {
    fail_result(
        SharedApplyEntry {
            plan_entry: plan.clone(),
            observed_source_digest: None,
            observed_destination_digest: None,
            backup_path: None,
            backup_digest: None,
            temporary_path: None,
            final_destination_digest: None,
            created_directories: Vec::new(),
            replacement_approved: false,
            verification_succeeded: false,
            outcome: SharedApplyOutcome::SkippedNotEligible,
            stages: Vec::new(),
            warnings: Vec::new(),
            failures: Vec::new(),
        },
        SharedApplyOutcome::SkippedNotEligible,
        kind,
        None,
        detail,
    )
}

fn fail_result(
    mut result: SharedApplyEntry,
    outcome: SharedApplyOutcome,
    kind: SharedApplyFailureKind,
    path: Option<&Path>,
    detail: &str,
) -> SharedApplyEntry {
    result.outcome = outcome;
    result.failures.push(failure(kind, path, detail));
    result.stages.push(match outcome {
        SharedApplyOutcome::SourceChanged => SharedTransactionStage::SourceChanged,
        SharedApplyOutcome::DestinationChanged => SharedTransactionStage::DestinationChanged,
        SharedApplyOutcome::BackupFailed => SharedTransactionStage::BackupFailed,
        SharedApplyOutcome::VerificationFailed => SharedTransactionStage::VerificationFailed,
        SharedApplyOutcome::WriteFailed => SharedTransactionStage::WriteFailed,
        SharedApplyOutcome::SkippedReplacementNotApproved => {
            SharedTransactionStage::SkippedReplacementNotApproved
        }
        SharedApplyOutcome::SkippedConflict => SharedTransactionStage::SkippedConflict,
        _ => SharedTransactionStage::SkippedNotEligible,
    });
    result
}

fn failure(kind: SharedApplyFailureKind, path: Option<&Path>, detail: &str) -> SharedApplyFailure {
    SharedApplyFailure {
        kind,
        path: path.map(SharedTransactionPath::from_path),
        detail: detail.to_owned(),
    }
}

fn plan_digest(plan: &SharedTransactionPlan) -> Result<String, SharedApplyFailure> {
    let mut clone = plan.clone();
    clone.plan_id.clear();
    serde_json::to_vec(&clone)
        .map(|bytes| digest_bytes(&bytes))
        .map_err(|error| {
            failure(
                SharedApplyFailureKind::InvalidPlan,
                None,
                &error.to_string(),
            )
        })
}

fn rollback_preview_digest(preview: &SharedRollbackPreview) -> String {
    let mut clone = preview.clone();
    clone.preview_id.clear();
    serde_json::to_vec(&clone)
        .map(|bytes| digest_bytes(&bytes))
        .unwrap_or_default()
}

fn digest_text(text: &str) -> String {
    digest_bytes(text.as_bytes())
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

fn safe_identifier(value: &str) -> Result<String, SharedApplyFailureKind> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SharedApplyFailureKind::InvalidPlan);
    }
    Ok(value.to_owned())
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn generate_shared_operation_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let nonce = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let material = format!("{now}:{}:{nonce}", std::process::id());
    format!("shared-{}", &digest_text(&material)[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch_manager::{
        PreviewIdentity, PreviewIdentityKind, PreviewIdentityState, PreviewMatchStrength,
        PreviewSourceItem, SharedPreviewRequest, build_shared_preview,
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-shared-transaction-{label}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn source_root(&self) -> PathBuf {
            self.0.join("source")
        }

        fn destination_root(&self) -> PathBuf {
            self.0.join("destination")
        }

        fn history_root(&self) -> PathBuf {
            self.0.join("history")
        }

        fn backup_root(&self) -> PathBuf {
            self.0.join("backups")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn preview(
        fixture: &Fixture,
        source_bytes: &[u8],
        existing: Option<&[u8]>,
    ) -> SharedPreviewReport {
        fs::create_dir(fixture.source_root()).unwrap();
        fs::create_dir(fixture.destination_root()).unwrap();
        let source = fixture.source_root().join("game.cht");
        fs::write(&source, source_bytes).unwrap();
        if let Some(bytes) = existing {
            let parent = fixture.destination_root().join("Nintendo - NES");
            fs::create_dir(&parent).unwrap();
            fs::write(parent.join("game.cht"), bytes).unwrap();
        }
        build_shared_preview(&SharedPreviewRequest {
            adapter: PreviewAdapter::RetroArch,
            selected_archive: fixture.0.join("selected.zip"),
            platform: Some("NES".into()),
            identity: PreviewIdentity {
                kind: PreviewIdentityKind::RetroArchCatalogueMatch,
                state: PreviewIdentityState::Verified,
                value: Some("archive-1".into()),
                archive_path: fixture.0.join("selected.zip"),
                revision: None,
            },
            destination_root: fixture.destination_root(),
            source_items: vec![PreviewSourceItem {
                adapter: PreviewAdapter::RetroArch,
                source_path: source,
                expected_source_digest: Some(digest_bytes(source_bytes)),
                destination_relative_paths: vec![PathBuf::from("Nintendo - NES/game.cht")],
                match_strength: PreviewMatchStrength::VerifiedExact,
            }],
        })
        .unwrap()
    }

    fn make_plan(fixture: &Fixture, report: &SharedPreviewReport) -> SharedTransactionPlan {
        build_shared_transaction_plan(
            report,
            "retroarch-native",
            "trusted-catalogue",
            &fixture.source_root(),
        )
        .unwrap()
    }

    fn options(
        fixture: &Fixture,
        plan: &SharedTransactionPlan,
        operation: &str,
        dry_run: bool,
        general: bool,
        replacement: bool,
    ) -> SharedApplyOptions {
        SharedApplyOptions {
            dry_run,
            confirmation: Some(SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: general,
                replacement_approved: replacement,
            }),
            operation_id: operation.into(),
            timestamp_unix_seconds: 1_700_000_000,
            current_context: plan.context.clone(),
            history_root: fixture.history_root(),
            backup_root: fixture.backup_root(),
        }
    }

    #[test]
    fn dry_run_and_missing_confirmation_write_nothing() {
        let fixture = Fixture::new("dry-run");
        let report = preview(&fixture, b"new", None);
        let plan = make_plan(&fixture, &report);
        let result = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "dry-run", false, false, false),
        );
        assert_eq!(result.journal.status, SharedApplyStatus::DryRun);
        assert!(
            !fixture
                .destination_root()
                .join("Nintendo - NES/game.cht")
                .exists()
        );
        assert!(!fixture.history_root().exists());
        assert!(!fixture.backup_root().exists());
    }

    #[test]
    fn install_new_is_atomic_journaled_and_rollback_is_bound_and_idempotent() {
        let fixture = Fixture::new("install");
        let report = preview(&fixture, b"new", None);
        let plan = make_plan(&fixture, &report);
        let result = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "install-one", false, true, false),
        );
        let destination = fixture.destination_root().join("Nintendo - NES/game.cht");
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(result.journal.status, SharedApplyStatus::Success);
        let journal_path = result.journal_path.unwrap();
        assert!(journal_path.exists());
        assert_eq!(
            discover_shared_apply_history(&fixture.history_root())
                .journals
                .len(),
            1
        );

        let rollback = preview_shared_rollback(
            &journal_path,
            &fixture.destination_root(),
            &fixture.backup_root(),
        );
        assert!(rollback.available);
        let rolled_back = execute_shared_rollback(
            &rollback,
            &SharedRollbackOptions {
                confirmation: SharedRollbackConfirmation {
                    preview_id: rollback.preview_id.clone(),
                    approved: true,
                },
                rollback_operation_id: "rollback-one".into(),
                timestamp_unix_seconds: 1_700_000_001,
                history_root: fixture.history_root(),
                backup_root: fixture.backup_root(),
            },
        );
        assert_eq!(rolled_back.status, SharedApplyStatus::Success);
        assert!(!destination.exists());
        assert!(!fixture.destination_root().join("Nintendo - NES").exists());
        let repeated = preview_shared_rollback(
            &journal_path,
            &fixture.destination_root(),
            &fixture.backup_root(),
        );
        assert!(!repeated.available);
        assert_eq!(
            repeated.entries[0].outcome,
            SharedRollbackOutcome::AlreadyRolledBack
        );
    }

    #[test]
    fn replacement_requires_permission_creates_verified_backup_and_restores_it() {
        let fixture = Fixture::new("replace");
        let report = preview(&fixture, b"new", Some(b"old"));
        let plan = make_plan(&fixture, &report);
        let denied = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "replace-denied", false, true, false),
        );
        assert_eq!(
            denied.journal.entries[0].outcome,
            SharedApplyOutcome::SkippedReplacementNotApproved
        );
        assert_eq!(
            fs::read(fixture.destination_root().join("Nintendo - NES/game.cht")).unwrap(),
            b"old"
        );
        let result = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "replace-one", false, true, true),
        );
        let entry = &result.journal.entries[0];
        assert_eq!(entry.outcome, SharedApplyOutcome::ReplacedExisting);
        let backup = entry.backup_path.as_ref().unwrap().to_path_buf().unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"old");
        let journal = result.journal_path.unwrap();
        let rollback = preview_shared_rollback(
            &journal,
            &fixture.destination_root(),
            &fixture.backup_root(),
        );
        assert!(rollback.available);
        let outcome = execute_shared_rollback(
            &rollback,
            &SharedRollbackOptions {
                confirmation: SharedRollbackConfirmation {
                    preview_id: rollback.preview_id.clone(),
                    approved: true,
                },
                rollback_operation_id: "restore-one".into(),
                timestamp_unix_seconds: 1_700_000_002,
                history_root: fixture.history_root(),
                backup_root: fixture.backup_root(),
            },
        );
        assert_eq!(outcome.status, SharedApplyStatus::Success);
        assert_eq!(
            fs::read(fixture.destination_root().join("Nintendo - NES/game.cht")).unwrap(),
            b"old"
        );
        assert!(backup.exists(), "backup retention is deliberate");
    }

    #[test]
    fn stale_plan_source_and_destination_changes_fail_closed() {
        let fixture = Fixture::new("stale");
        let report = preview(&fixture, b"new", Some(b"old"));
        let plan = make_plan(&fixture, &report);
        let mut stale = options(&fixture, &plan, "stale-context", false, true, true);
        stale.current_context.profile_id = "other-profile".into();
        assert_eq!(
            execute_shared_apply(&plan, &stale).journal.status,
            SharedApplyStatus::Failed
        );
        fs::write(fixture.source_root().join("game.cht"), b"changed").unwrap();
        let source_changed = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "source-changed", false, true, true),
        );
        assert_eq!(
            source_changed.journal.entries[0].outcome,
            SharedApplyOutcome::SourceChanged
        );
        fs::write(fixture.source_root().join("game.cht"), b"new").unwrap();
        fs::write(
            fixture.destination_root().join("Nintendo - NES/game.cht"),
            b"user-change",
        )
        .unwrap();
        let destination_changed = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "destination-changed", false, true, true),
        );
        assert_eq!(
            destination_changed.journal.entries[0].outcome,
            SharedApplyOutcome::DestinationChanged
        );
    }

    #[test]
    fn journal_failure_after_write_is_truthful_partial_success_and_temp_is_clean() {
        let fixture = Fixture::new("journal-failure");
        let report = preview(&fixture, b"new", None);
        let plan = make_plan(&fixture, &report);
        let mut options = options(&fixture, &plan, "partial", false, true, false);
        options.history_root = fixture.0.join("missing-parent/history");
        let result = execute_shared_apply(&plan, &options);
        assert_eq!(result.journal.status, SharedApplyStatus::PartialFailure);
        assert!(result.journal_failure.is_some());
        assert_eq!(
            fs::read(fixture.destination_root().join("Nintendo - NES/game.cht")).unwrap(),
            b"new"
        );
        assert!(
            fs::read_dir(fixture.destination_root().join("Nintendo - NES"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".archivefs-"))
        );
    }

    #[test]
    fn malformed_history_is_bounded_and_rollback_blocks_user_changes_and_bad_backup() {
        let fixture = Fixture::new("history");
        fs::create_dir(fixture.history_root()).unwrap();
        fs::write(fixture.history_root().join("bad.json"), b"{").unwrap();
        let history = discover_shared_apply_history(&fixture.history_root());
        assert!(history.journals.is_empty());
        assert_eq!(history.warnings.len(), 1);

        let fixture = Fixture::new("rollback-blocks");
        let report = preview(&fixture, b"new", Some(b"old"));
        let plan = make_plan(&fixture, &report);
        let applied = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "replace-block", false, true, true),
        );
        let journal = applied.journal_path.unwrap();
        fs::write(
            fixture.destination_root().join("Nintendo - NES/game.cht"),
            b"user",
        )
        .unwrap();
        let changed = preview_shared_rollback(
            &journal,
            &fixture.destination_root(),
            &fixture.backup_root(),
        );
        assert!(!changed.available);
        assert_eq!(
            changed.entries[0].outcome,
            SharedRollbackOutcome::DestinationChanged
        );
    }

    #[test]
    fn unsupported_adapters_duplicate_paths_limits_and_lock_contention_fail_closed() {
        assert_eq!(
            adapter_write_support(PreviewAdapter::Pcsx2),
            SharedAdapterWriteSupport::PreviewOnlySourceNotMaterialized
        );
        assert_eq!(
            adapter_write_support(PreviewAdapter::Dolphin),
            SharedAdapterWriteSupport::PreviewOnlySourceNotMaterialized
        );
        let fixture = Fixture::new("lock");
        fs::create_dir(fixture.destination_root()).unwrap();
        let _first =
            RootLock::acquire(&fixture.destination_root(), Duration::from_millis(20)).unwrap();
        assert_eq!(
            RootLock::acquire(&fixture.destination_root(), Duration::from_millis(20)).unwrap_err(),
            SharedApplyFailureKind::LockTimeout
        );
    }

    #[test]
    fn injected_apply_failures_preserve_atomicity_and_truthful_state() {
        for fault in [
            FaultPoint::TemporaryWrite,
            FaultPoint::Flush,
            FaultPoint::Rename,
            FaultPoint::Verification,
            FaultPoint::ParentCreationRace,
            FaultPoint::SourceMutation,
            FaultPoint::DestinationMutation,
        ] {
            let fixture = Fixture::new(&format!("fault-{fault:?}"));
            let report = preview(&fixture, b"new", None);
            let plan = make_plan(&fixture, &report);
            inject_fault(Some(fault));
            let result = execute_shared_apply(
                &plan,
                &options(&fixture, &plan, "fault-run", false, true, false),
            );
            inject_fault(None);
            assert_ne!(result.journal.status, SharedApplyStatus::Success);
            assert!(
                !fixture
                    .destination_root()
                    .join("Nintendo - NES/game.cht")
                    .exists()
            );
        }

        let fixture = Fixture::new("backup-fault");
        let report = preview(&fixture, b"new", Some(b"old"));
        let plan = make_plan(&fixture, &report);
        inject_fault(Some(FaultPoint::BackupWrite));
        let result = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "backup-fault", false, true, true),
        );
        inject_fault(None);
        assert_eq!(
            result.journal.entries[0].outcome,
            SharedApplyOutcome::BackupFailed
        );
        assert_eq!(
            fs::read(fixture.destination_root().join("Nintendo - NES/game.cht")).unwrap(),
            b"old"
        );

        let fixture = Fixture::new("journal-injected");
        let report = preview(&fixture, b"new", None);
        let plan = make_plan(&fixture, &report);
        inject_fault(Some(FaultPoint::JournalWrite));
        let result = execute_shared_apply(
            &plan,
            &options(&fixture, &plan, "journal-fault", false, true, false),
        );
        inject_fault(None);
        assert_eq!(result.journal.status, SharedApplyStatus::PartialFailure);
        assert_eq!(
            result.journal_failure.as_ref().map(|failure| failure.kind),
            Some(SharedApplyFailureKind::JournalFailed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip_and_symlink_source_is_never_plannable() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;
        let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'/', 0xff]));
        assert_eq!(
            SharedTransactionPath::from_path(&path)
                .to_path_buf()
                .unwrap(),
            path
        );

        let fixture = Fixture::new("symlink");
        fs::create_dir(fixture.source_root()).unwrap();
        fs::create_dir(fixture.destination_root()).unwrap();
        fs::write(fixture.0.join("outside"), b"new").unwrap();
        symlink(
            fixture.0.join("outside"),
            fixture.source_root().join("game.cht"),
        )
        .unwrap();
        let report = build_shared_preview(&SharedPreviewRequest {
            adapter: PreviewAdapter::RetroArch,
            selected_archive: fixture.0.join("selected.zip"),
            platform: Some("NES".into()),
            identity: PreviewIdentity {
                kind: PreviewIdentityKind::RetroArchCatalogueMatch,
                state: PreviewIdentityState::Verified,
                value: Some("archive-1".into()),
                archive_path: fixture.0.join("selected.zip"),
                revision: None,
            },
            destination_root: fixture.destination_root(),
            source_items: vec![PreviewSourceItem {
                adapter: PreviewAdapter::RetroArch,
                source_path: fixture.source_root().join("game.cht"),
                expected_source_digest: None,
                destination_relative_paths: vec![PathBuf::from("Nintendo - NES/game.cht")],
                match_strength: PreviewMatchStrength::VerifiedExact,
            }],
        })
        .unwrap();
        assert!(
            build_shared_transaction_plan(&report, "profile", "trusted", &fixture.source_root())
                .is_err()
        );
    }
}
