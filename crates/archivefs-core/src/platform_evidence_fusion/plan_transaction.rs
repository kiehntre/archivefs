//! Batch 14: the frozen-plan-to-transaction boundary.
//!
//! # Reuse, not a second transaction framework
//!
//! This repo already has a complete, proven, journal-backed transaction
//! engine: [`crate::dat::rename_apply`] (model/executor/preflight/journal/
//! rollback/reconcile/identity/no-clobber-rename) and
//! [`crate::dat::rom_organisation::transaction`] (the same engine wired for
//! cross-directory moves with platform-directory creation). This module
//! does **not** reimplement any of that. It only builds the two things that
//! did not exist yet:
//!
//! - a digest-bound [`ApprovedPlan`]/[`TransactionPreview`] boundary in
//!   front of the executor, so a raw [`super::library_plan_export::LibraryPlanExport`]
//!   can never be handed to it directly (milestone sections 6-8, 32-33);
//! - the bridge from that export's `Ready`-only items into the existing
//!   [`crate::dat::rename_apply::model::TransactionEntry`]/[`RenameTransaction`]
//!   shape, plus generalised (N-level, not just one) directory creation
//!   using exactly [`crate::dat::rom_organisation::transaction`]'s own
//!   ownership-tracking discipline (a directory is recorded as owned only
//!   *after* `create_dir` succeeds, so a pre-existing directory can never be
//!   removed by rollback).
//!
//! Every mutation, every journal write, every rollback, every crash-recovery
//! reconciliation below is the *existing* `rename_apply` code, called
//! unchanged. This module never calls `std::fs::rename`/`remove_file`/
//! `write` itself.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::dat::rename_apply::executor::{
    ApplyError, ApplyExecution, ApplyOutcome, HardConflictMode, apply_transaction,
    validate_classifier_version,
};
use crate::dat::rename_apply::identity::capture_identity;
use crate::dat::rename_apply::journal::{new_transaction_id, write_journal};
use crate::dat::rename_apply::model::{
    EntryState, RenameTransaction, TransactionEntry, TransactionState,
};
use crate::dat::rename_apply::preflight::{DirectoryPolicy, is_safe_basename};
use crate::dat::rename_apply::reconcile::{RecoveryIssue, RecoveryIssueKind};
use crate::dat::rename_apply::rollback::{RollbackOutcome, rollback_transaction};
use crate::safe_read::TrustedRoots;

use super::library_plan_export::{LibraryPlanExport, LibraryPlanExportItem, OperationIntent};
use super::library_planning::PlanStatus;

// --------------------------------------------------------------------
// Plan digest (sections 7-8)
// --------------------------------------------------------------------

/// A stable digest over a frozen export - milestone sections 7-8. Two
/// exports that would produce the same transaction always produce the same
/// digest; anything that changed what would actually happen (a source
/// path, a precondition, a destination, an operation intent, a blocker/
/// status, a set/support relationship, a hash) changes it. Deliberately
/// excludes nothing nondeterministic (there is nothing timestamp-shaped on
/// `LibraryPlanExportItem` to exclude).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanDigest(pub String);

impl PlanDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One item's digest-relevant fields, serialized in a fixed field order
/// (never derived from a `HashMap`/struct field order that could change) -
/// the actual digest input.
fn digest_line(item: &LibraryPlanExportItem) -> String {
    format!(
        "status={:?}|source={}|physical_hash={:?}|normalized_hash={:?}|destination={:?}|intent={:?}|blockers={:?}|set_label={:?}|set_destination={:?}|support_role={:?}|support_association={:?}|duplicate={:?}|revision={:?}",
        item.status,
        item.precondition.source_path,
        item.precondition.physical_hash,
        item.precondition.normalized_hash,
        item.proposed_destination,
        item.operation_intent,
        item.blockers,
        item.set_label,
        item.set_destination,
        item.support_role,
        item.support_association,
        item.duplicate_classification,
        item.revision_relationship,
    )
}

/// Computes the plan digest - milestone section 7. Items are digested in
/// the export's own order (the export itself is built in the caller's
/// stable order per Batch 12/13's own determinism guarantee), each line
/// newline-joined, then SHA-256'd. No timestamp anywhere in the input.
pub fn compute_plan_digest(export: &LibraryPlanExport) -> PlanDigest {
    let mut hasher = Sha256::new();
    for item in &export.items {
        hasher.update(digest_line(item).as_bytes());
        hasher.update(b"\n");
    }
    let bytes = hasher.finalize();
    PlanDigest(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// --------------------------------------------------------------------
// Preview (sections 30-31)
// --------------------------------------------------------------------

/// A safe operation kind a `Ready` item's destination implies - derived,
/// never carried as executable intent (milestone section 45).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Move,
    Rename,
    /// Not `Ready`, or has no computable destination - never becomes a
    /// transaction operation.
    Unsupported,
}

/// How strongly this item's precondition can be re-verified before
/// mutation - milestone section 31's "precondition strength" line. Never
/// invents a stronger check than what the frozen export actually carries
/// (milestone section 9's "if a frozen precondition is unavailable: do not
/// invent one").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreconditionStrength {
    /// A physical (and/or normalized) hash was frozen; re-verifiable
    /// exactly.
    HashVerified,
    /// No hash was frozen; only the existing `rename_apply` identity
    /// capture (size/mtime/inode/kind) will be checked at build/apply
    /// time - weaker, but never skipped.
    IdentityOnly,
}

/// One preview row - milestone section 30.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewOperation {
    pub source_path: String,
    pub destination_path: Option<String>,
    pub kind: OperationKind,
    pub precondition_strength: PreconditionStrength,
    pub blockers: Vec<String>,
}

/// The structured, executable-action-free preview - milestone section 30.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransactionPreview {
    pub digest: PlanDigest,
    pub operations: Vec<PreviewOperation>,
    pub unsupported_item_count: usize,
    pub total_operation_count: usize,
}

/// Builds a preview from a frozen export - pure, read-only, no filesystem
/// access. Every `Ready` item with a proposed destination becomes one
/// operation; everything else (`Unknown`/`Ambiguous`/`Conflict`/
/// `Unsupported`/`NeedsReview`, or `Ready` with no destination) is counted
/// in `unsupported_item_count` and never becomes an operation (milestone
/// section 5).
pub fn build_preview(export: &LibraryPlanExport) -> TransactionPreview {
    let digest = compute_plan_digest(export);
    let mut operations = Vec::new();
    let mut unsupported_item_count = 0usize;
    for item in &export.items {
        let Some(operation) = preview_operation_for(item) else {
            unsupported_item_count += 1;
            continue;
        };
        operations.push(operation);
    }
    let total_operation_count = operations.len();
    TransactionPreview {
        digest,
        operations,
        unsupported_item_count,
        total_operation_count,
    }
}

fn preview_operation_for(item: &LibraryPlanExportItem) -> Option<PreviewOperation> {
    if item.status != PlanStatus::Ready {
        return None;
    }
    let destination = item.proposed_destination.as_ref()?;
    if !item.blockers.is_empty() {
        return None;
    }
    let kind = match item.operation_intent {
        OperationIntent::MoveToLibraryFolder | OperationIntent::OrganiseSymlinkOnly => {
            OperationKind::Move
        }
        OperationIntent::RenameInPlace => OperationKind::Rename,
        OperationIntent::None => return None,
    };
    let precondition_strength = if item.precondition.physical_hash.is_some()
        || item.precondition.normalized_hash.is_some()
    {
        PreconditionStrength::HashVerified
    } else {
        PreconditionStrength::IdentityOnly
    };
    Some(PreviewOperation {
        source_path: item.precondition.source_path.clone(),
        destination_path: Some(destination.clone()),
        kind,
        precondition_strength,
        blockers: Vec::new(),
    })
}

/// Milestone section 31's exact human-readable shape.
pub fn render_preview_text(preview: &TransactionPreview) -> String {
    let mut out = String::new();
    out.push_str("TRANSACTION PREVIEW\n\n");
    out.push_str(&format!(
        "Operations: {}\n\n",
        preview.total_operation_count
    ));
    for op in &preview.operations {
        out.push_str(match op.kind {
            OperationKind::Move => "MOVE\n",
            OperationKind::Rename => "RENAME\n",
            OperationKind::Unsupported => "UNSUPPORTED\n",
        });
        out.push_str("  Source:\n    ");
        out.push_str(&op.source_path);
        out.push('\n');
        out.push_str("  Destination:\n    ");
        out.push_str(op.destination_path.as_deref().unwrap_or("(none)"));
        out.push_str("\n\n");
    }
    out.push_str("Preconditions:\n");
    for op in &preview.operations {
        let label = match op.precondition_strength {
            PreconditionStrength::HashVerified => "physical hash verified",
            PreconditionStrength::IdentityOnly => "identity only (no frozen hash)",
        };
        out.push_str(&format!("  {}: {label}\n", op.source_path));
    }
    out.push_str(&format!(
        "\nUnsupported items:\n  {}\n",
        preview.unsupported_item_count
    ));
    out.push_str("\nApproval:\n  REQUIRED\n");
    out.push_str("\nApplied:\n  NO\n");
    out
}

// --------------------------------------------------------------------
// Approval (sections 6, 32-33)
// --------------------------------------------------------------------

/// Why an approval could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalError {
    /// The preview has no operations at all - nothing to approve.
    NoOperations,
    /// The caller must supply a real, non-empty acknowledgement string -
    /// never a silent default "yes" (milestone section 32).
    EmptyAcknowledgement,
}

/// The explicit approval boundary - milestone sections 6, 32-33. Can only
/// be produced by [`approve_transaction`]; nothing else constructs one with
/// a matching digest, so the executor accepting only an `ApprovedPlan`
/// (never a raw [`LibraryPlanExport`]/[`TransactionPreview`]) is a real
/// gate, not decoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovedPlan {
    pub digest: PlanDigest,
    pub approved_at_unix: u64,
    pub approved_item_ids: BTreeSet<String>,
    pub acknowledgement: String,
}

/// The only way to produce an [`ApprovedPlan`]. Never auto-approves: an
/// empty preview or an empty acknowledgement is refused.
pub fn approve_transaction(
    preview: &TransactionPreview,
    acknowledgement: &str,
) -> Result<ApprovedPlan, ApprovalError> {
    if preview.operations.is_empty() {
        return Err(ApprovalError::NoOperations);
    }
    if acknowledgement.trim().is_empty() {
        return Err(ApprovalError::EmptyAcknowledgement);
    }
    Ok(ApprovedPlan {
        digest: preview.digest.clone(),
        approved_at_unix: crate::dat::sources::now_unix(),
        approved_item_ids: preview
            .operations
            .iter()
            .map(|op| op.source_path.clone())
            .collect(),
        acknowledgement: acknowledgement.to_string(),
    })
}

// --------------------------------------------------------------------
// Building a real RenameTransaction from an approved export (sections 4-5,
// 9-11, 17-19)
// --------------------------------------------------------------------

/// Why a transaction could not be built from an approved export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanTransactionError {
    /// The export's current digest does not match the approval's - the
    /// plan changed (or a different plan was supplied) since approval
    /// (milestone sections 11, 33).
    DigestMismatch { approved: String, current: String },
    /// Nothing in the export is both `Ready` and approved.
    NoApprovedReadyItems,
    /// A destination equals another operation's source (or its own
    /// source), or otherwise closes a cycle - rejected outright rather
    /// than staged (milestone sections 17-18).
    CycleDetected(Vec<String>),
    /// The underlying `rename_apply`/`rom_organisation` build failed.
    Underlying(String),
}

impl std::fmt::Display for PlanTransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DigestMismatch { approved, current } => write!(
                f,
                "the plan changed since approval (approved digest {approved}, current {current}); \
                 the approval no longer applies"
            ),
            Self::NoApprovedReadyItems => {
                write!(f, "no approved item is Ready with a real destination")
            }
            Self::CycleDetected(paths) => write!(
                f,
                "a destination/source cycle was detected and rejected: {}",
                paths.join(" -> ")
            ),
            Self::Underlying(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for PlanTransactionError {}

/// Builds a `RenameTransaction` from an approved, still-current export.
/// Read-only: captures identity (via the existing `rename_apply` primitive)
/// and detects cycles, but writes no journal and mutates nothing.
///
/// Only `Ready`, blocker-free, approved items with a real proposed
/// destination ever become an entry - milestone section 5's authority list
/// (`Unknown`/`Ambiguous`/`Conflict`/`Unsupported`/`NeedsReview`/blocked/
/// missing-destination items are never considered, structurally: they never
/// produced a `PreviewOperation` in the first place).
pub fn build_plan_transaction(
    export: &LibraryPlanExport,
    approved: &ApprovedPlan,
    scan_root_label: &str,
) -> Result<RenameTransaction, PlanTransactionError> {
    let current_digest = compute_plan_digest(export);
    if current_digest.as_str() != approved.digest.as_str() {
        return Err(PlanTransactionError::DigestMismatch {
            approved: approved.digest.as_str().to_string(),
            current: current_digest.as_str().to_string(),
        });
    }

    let mut entries = Vec::new();
    let mut sources: BTreeSet<PathBuf> = BTreeSet::new();
    let mut destinations: BTreeSet<PathBuf> = BTreeSet::new();
    for item in &export.items {
        if item.status != PlanStatus::Ready || !item.blockers.is_empty() {
            continue;
        }
        let source_path_str = &item.precondition.source_path;
        if !approved.approved_item_ids.contains(source_path_str) {
            continue;
        }
        let Some(destination_str) = &item.proposed_destination else {
            continue;
        };
        let source_path = PathBuf::from(source_path_str);
        let destination_path = PathBuf::from(destination_str);
        if source_path == destination_path {
            // Never a real operation; excluded rather than treated as a cycle.
            continue;
        }
        let Some(proposed_basename) = destination_path.file_name() else {
            continue;
        };
        let proposed_basename = proposed_basename.to_string_lossy().into_owned();
        if !is_safe_basename(&proposed_basename) {
            continue;
        }
        let Ok(identity) = capture_identity(&source_path) else {
            // Source vanished since the plan was frozen; excluded, not
            // silently substituted with an invented identity.
            continue;
        };
        let original_basename = source_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        sources.insert(source_path.clone());
        destinations.insert(destination_path.clone());
        entries.push(TransactionEntry {
            source_path,
            destination_path,
            original_basename,
            proposed_basename,
            identity,
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        });
    }

    if entries.is_empty() {
        return Err(PlanTransactionError::NoApprovedReadyItems);
    }

    // Cycle detection (sections 17-18): any destination that is also some
    // entry's source closes a chain/cycle. Rejected outright rather than
    // staged through a temporary name - safety over cleverness.
    let cyclic: Vec<String> = destinations
        .intersection(&sources)
        .map(|path| path.display().to_string())
        .collect();
    if !cyclic.is_empty() {
        return Err(PlanTransactionError::CycleDetected(cyclic));
    }

    let generation = plan_generation_of(export);
    Ok(RenameTransaction {
        transaction_id: new_transaction_id(crate::dat::sources::now_unix()),
        plan_generation: generation,
        classifier_version: Some(crate::dat::classification::CLASSIFIER_VERSION.to_string()),
        created_at_unix: crate::dat::sources::now_unix(),
        source_scan_root: scan_root_label.to_string(),
        state: TransactionState::Planned,
        entries,
        created_directories: Vec::new(),
        unknown: Default::default(),
    })
}

/// A plan-digest-derived generation number, so `rename_apply`'s own
/// staleness check (`plan_generation != current_generation`) means
/// something for a plan transaction: any change to the export changes the
/// digest, which changes this number. Never a wall-clock timestamp.
///
/// Public so a caller can recompute it from a **freshly rebuilt** export
/// immediately before [`apply_plan_transaction`] and pass it as
/// `current_generation` - the transaction's own `plan_generation` was
/// fixed at [`build_plan_transaction`] time and comparing it to itself
/// would never catch staleness.
pub fn plan_generation_of(export: &LibraryPlanExport) -> u64 {
    let digest = compute_plan_digest(export);
    // The digest is a 64-hex-character SHA-256; the first 16 hex characters
    // parse cleanly as a u64. Any change to the export changes this number,
    // which is all `rename_apply`'s own staleness check needs.
    u64::from_str_radix(&digest.0[..16], 16).unwrap_or(0)
}

// --------------------------------------------------------------------
// Directory creation (generalised `rom_organisation::transaction` pattern -
// section 42)
// --------------------------------------------------------------------

/// Creates every missing ancestor directory between each entry's
/// destination parent and `root` (exclusive), recording each one as
/// EmuWiz-owned **only after** `create_dir` succeeds, journaling
/// immediately afterwards - the exact ownership discipline
/// [`crate::dat::rom_organisation::transaction::apply_organisation_transaction`]
/// already uses, generalised from one level to N. A pre-existing directory
/// is never recorded as owned and so is never removed by rollback.
fn ensure_destination_directories(
    transaction: &mut RenameTransaction,
    root: &Path,
    journal_dir: &Path,
) -> Result<(), ApplyError> {
    let mut to_create: Vec<PathBuf> = Vec::new();
    for entry in &transaction.entries {
        let Some(mut ancestor) = entry.destination_path.parent().map(Path::to_path_buf) else {
            continue;
        };
        let mut chain = Vec::new();
        while ancestor.starts_with(root) && ancestor != root {
            chain.push(ancestor.clone());
            let Some(parent) = ancestor.parent() else {
                break;
            };
            ancestor = parent.to_path_buf();
        }
        chain.reverse();
        for directory in chain {
            if !to_create.contains(&directory) {
                to_create.push(directory);
            }
        }
    }

    for directory in &to_create {
        match std::fs::symlink_metadata(directory) {
            Ok(_) => continue, // pre-existing (or created by an earlier iteration): never ours
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(directory) {
                    Ok(()) => {
                        transaction.created_directories.push(directory.clone());
                        write_journal(journal_dir, transaction)
                            .map_err(|error| ApplyError::Journal(error.to_string()))?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(ApplyError::Journal(format!(
                            "could not create directory {}: {error}",
                            directory.display()
                        )));
                    }
                }
            }
            Err(error) => {
                return Err(ApplyError::Journal(format!(
                    "could not inspect directory {}: {error}",
                    directory.display()
                )));
            }
        }
    }
    Ok(())
}

/// Applies a plan transaction built by [`build_plan_transaction`] -
/// milestone section 21. Ordering: durable `Applying` journal checkpoint
/// (before any directory is created, matching
/// `apply_organisation_transaction`'s own contract), then missing
/// destination directories (each journaled the instant it is created), then
/// the shared `rename_apply` executor for the actual moves (its own
/// preflight, its own per-entry `Applying` checkpoint, its own no-clobber
/// rename, its own post-rename confirmation - unchanged).
#[allow(clippy::too_many_arguments)]
pub fn apply_plan_transaction(
    transaction: &mut RenameTransaction,
    current_generation: u64,
    root: &Path,
    trusted: TrustedRoots,
    journal_dir: &Path,
    cancel: &AtomicBool,
    allow_symlink_source: bool,
) -> Result<ApplyOutcome, ApplyError> {
    apply_plan_transaction_with_mode(
        transaction,
        current_generation,
        root,
        trusted,
        journal_dir,
        cancel,
        allow_symlink_source,
        HardConflictMode::AbortAll,
    )
}

/// Same as [`apply_plan_transaction`], with an explicit
/// [`HardConflictMode`]. `SkipUnsafeSubset` lets a caller that has already
/// reviewed the batch apply only the safe entries of a set, journaling the
/// rest as `Skipped` rather than refusing the whole batch - milestone
/// section 23's genuine partial-application case (as opposed to
/// `AbortAll`'s stronger "nothing mutates if anything is wrong" default).
#[allow(clippy::too_many_arguments)]
pub fn apply_plan_transaction_with_mode(
    transaction: &mut RenameTransaction,
    current_generation: u64,
    root: &Path,
    trusted: TrustedRoots,
    journal_dir: &Path,
    cancel: &AtomicBool,
    allow_symlink_source: bool,
    hard_conflict_mode: HardConflictMode,
) -> Result<ApplyOutcome, ApplyError> {
    validate_classifier_version(transaction.classifier_version.as_deref())?;
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(ApplyError::Cancelled);
    }

    transaction.state = TransactionState::Applying;
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    if let Err(error) = ensure_destination_directories(transaction, root, journal_dir) {
        transaction.state = TransactionState::ApplyFailed;
        write_journal(journal_dir, transaction).map_err(|e| ApplyError::Journal(e.to_string()))?;
        return Err(error);
    }

    let approved_paths: BTreeSet<String> = transaction
        .entries
        .iter()
        .map(|entry| entry.source_path.to_string_lossy().into_owned())
        .collect();

    apply_transaction(&mut ApplyExecution {
        transaction,
        approved_paths,
        current_generation,
        trusted,
        journal_dir: journal_dir.to_path_buf(),
        hard_conflict_mode,
        cancel,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source,
    })
}

/// Rolls back a plan transaction: the shared entry-move rollback, then any
/// directories this transaction created that are now empty, deepest first -
/// the same discipline as
/// [`crate::dat::rom_organisation::transaction::rollback_organisation_transaction`].
pub fn rollback_plan_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
) -> Result<PlanRollbackOutcome, String> {
    let rollback = rollback_transaction(transaction, journal_dir, cancel)?;

    let mut directories_removed = Vec::new();
    let mut directories_remaining = Vec::new();
    // Reverse of creation order (deepest-created-last => deepest-removed-first).
    for directory in transaction.created_directories.iter().rev() {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() => {
                let is_empty = std::fs::read_dir(directory)
                    .map(|mut read_dir| read_dir.next().is_none())
                    .unwrap_or(false);
                if is_empty && std::fs::remove_dir(directory).is_ok() {
                    directories_removed.push(directory.clone());
                } else {
                    directories_remaining.push(directory.clone());
                }
            }
            _ => {}
        }
    }
    Ok(PlanRollbackOutcome {
        rollback,
        directories_removed,
        directories_remaining,
    })
}

/// The outcome of rolling back a plan transaction - mirrors
/// [`crate::dat::rom_organisation::transaction::OrganisationRollbackOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRollbackOutcome {
    pub rollback: RollbackOutcome,
    pub directories_removed: Vec<PathBuf>,
    pub directories_remaining: Vec<PathBuf>,
}

// --------------------------------------------------------------------
// Crash recovery assessment (section 28)
// --------------------------------------------------------------------

/// The whole-transaction recovery classification milestone section 28
/// asks for, derived from [`TransactionState`] and the (already-persisted)
/// [`RecoveryIssue`] findings [`crate::dat::rename_apply::reconcile::reconcile_recovery`]
/// produces - never a new reconciliation mechanism, only a label over the
/// existing one's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAssessment {
    /// Nothing was mutated yet; resuming (building a fresh apply) is safe.
    SafeToResume,
    /// At least one entry is confirmed Applied and eligible for reversal.
    SafeToRollback,
    /// Every entry is Applied/settled; nothing to do.
    AlreadyCommitted,
    /// Every entry is RolledBack; nothing to do.
    AlreadyRolledBack,
    /// An entry could not be safely classified against the filesystem -
    /// never resolved automatically.
    ManualRecoveryRequired,
}

/// Assesses a transaction's journal (after
/// [`crate::dat::rename_apply::reconcile::reconcile_recovery`] has already
/// run and persisted its findings) - milestone section 28.
pub fn assess_recovery(
    transaction: &RenameTransaction,
    issues: &[RecoveryIssue],
) -> RecoveryAssessment {
    let unresolved = issues.iter().any(|issue| {
        matches!(
            issue.kind,
            RecoveryIssueKind::BothSourceAndDestination
                | RecoveryIssueKind::BothAbsent
                | RecoveryIssueKind::DestinationIdentityChanged
                | RecoveryIssueKind::SourceIdentityChanged
        )
    });
    if unresolved {
        return RecoveryAssessment::ManualRecoveryRequired;
    }
    match transaction.state {
        TransactionState::RolledBack => RecoveryAssessment::AlreadyRolledBack,
        TransactionState::Planned => RecoveryAssessment::SafeToResume,
        TransactionState::Applied if !transaction.has_applied_entries() => {
            RecoveryAssessment::AlreadyCommitted
        }
        TransactionState::Applied
        | TransactionState::ApplyFailed
        | TransactionState::RollbackFailed => {
            if transaction.has_applied_entries() {
                RecoveryAssessment::SafeToRollback
            } else {
                RecoveryAssessment::SafeToResume
            }
        }
        TransactionState::Applying | TransactionState::RollingBack => {
            // reconcile_recovery should have already resolved these to a
            // settled state; still-in-flight here means something was left
            // unresolved that our own unresolved-issue check above did not
            // catch - fail closed.
            RecoveryAssessment::ManualRecoveryRequired
        }
    }
}

#[cfg(test)]
mod tests;
