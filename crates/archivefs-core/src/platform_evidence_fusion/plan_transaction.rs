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
use crate::dat::rename_apply::preflight::{
    DirectoryPolicy, PreflightOptions, batch_destinations, is_safe_basename, run_preflight,
};
use crate::dat::rename_apply::reconcile::{RecoveryIssue, RecoveryIssueKind};
use crate::dat::rename_apply::rollback::{RollbackOutcome, rollback_transaction_confined};
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

/// The `TransactionEntry.unknown` key a built entry's `set_label` (when it
/// has one) is carried under - milestone section 5. Private: nothing outside
/// this module should read or write it directly.
const SET_LABEL_KEY: &str = "set_label";

/// Reads the `set_label` a [`build_plan_transaction`] entry was tagged with,
/// if any - the exact frozen-export value, never reconstructed from a path.
fn set_label_of(entry: &TransactionEntry) -> Option<String> {
    entry
        .unknown
        .get(SET_LABEL_KEY)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

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
        let mut unknown = std::collections::BTreeMap::new();
        if let Some(set_label) = &item.set_label {
            // Batch 16 (sections 5-9): carried through the existing
            // forward-compat `unknown` flatten field rather than widening
            // `TransactionEntry` itself, so set-aware `SkipUnsafeSubset`
            // handling stays entirely inside this bridge module - see
            // [`set_label_of`] and the grouping pass in
            // [`apply_plan_transaction_with_mode`]. Never reconstructed from
            // filenames/folders: this is exactly the frozen export's own
            // `set_label`, nothing else.
            unknown.insert(
                SET_LABEL_KEY.to_string(),
                serde_json::Value::String(set_label.clone()),
            );
        }
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
            unknown,
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
    // Batch 15 finding: once a transaction has entered or completed
    // rollback, its entries' source paths are restored to their original,
    // recorded identity - which the shared executor's own preflight would
    // otherwise treat as fresh and safe to reapply, silently resurrecting
    // an already-reversed transaction. Refused here, before the shared
    // executor is ever invoked. `Applied`/`ApplyFailed`/`Planned` are not
    // guarded here: a second apply on those is already safely refused by
    // the shared executor's own preflight (the source is gone or already
    // failed), which existing tests already rely on.
    if matches!(
        transaction.state,
        TransactionState::RolledBack
            | TransactionState::RollingBack
            | TransactionState::RollbackFailed
    ) {
        return Err(ApplyError::AlreadySettled {
            transaction_id: transaction.transaction_id.clone(),
            state: transaction.state,
        });
    }

    transaction.state = TransactionState::Applying;
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    if let Err(error) = ensure_destination_directories(transaction, root, journal_dir) {
        transaction.state = TransactionState::ApplyFailed;
        write_journal(journal_dir, transaction).map_err(|e| ApplyError::Journal(e.to_string()))?;
        return Err(error);
    }

    let mut approved_paths: BTreeSet<String> = transaction
        .entries
        .iter()
        .map(|entry| entry.source_path.to_string_lossy().into_owned())
        .collect();

    // Batch 16 (sections 3-9): make `SkipUnsafeSubset` set-aware without
    // touching the shared executor at all. The shared executor only ever
    // decides "skip" per entry, from whether that entry's own preflight
    // passed - it has no concept of a set. So this pre-pass runs the exact
    // same preflight check the executor is about to run again, but only to
    // *decide*, for every entry that belongs to a set (`set_label_of`
    // is `Some`), whether that whole set is safe. If any member of a set is
    // unsafe, every member's source path is removed from `approved_paths`
    // before the executor ever sees it - which makes the executor's own
    // preflight reject every member of that set with `NotApproved`, so
    // `SkipUnsafeSubset` marks the *entire* set `Skipped`, never a partial
    // move (never "Disc1 moved, Disc2 skipped, playlist moved"). Entries
    // with no `set_label` are unaffected: they keep the pre-existing
    // per-entry skip behaviour, which was already correct for them.
    if hard_conflict_mode == HardConflictMode::SkipUnsafeSubset {
        let dry_run_destinations = batch_destinations(&transaction.entries);
        let dry_run_options = PreflightOptions {
            plan_generation: transaction.plan_generation,
            current_generation,
            approved_paths: &approved_paths,
            trusted: &trusted,
            batch_destinations: &dry_run_destinations,
            directory_policy: DirectoryPolicy::SameFilesystem,
            allow_symlink_source,
        };
        let mut unsafe_set_labels: BTreeSet<String> = BTreeSet::new();
        for entry in &transaction.entries {
            if run_preflight(entry, &dry_run_options).is_err()
                && let Some(set_label) = set_label_of(entry)
            {
                unsafe_set_labels.insert(set_label);
            }
        }
        if !unsafe_set_labels.is_empty() {
            for entry in &transaction.entries {
                if set_label_of(entry).is_some_and(|label| unsafe_set_labels.contains(&label)) {
                    approved_paths.remove(&entry.source_path.to_string_lossy().into_owned());
                }
            }
        }
    }

    let result = apply_transaction(&mut ApplyExecution {
        transaction,
        approved_paths,
        current_generation,
        trusted,
        journal_dir: journal_dir.to_path_buf(),
        hard_conflict_mode,
        cancel,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source,
    });

    // Batch 16 finding: `apply_transaction`'s `AbortAll` hard-conflict path
    // returns `Err(HardConflicts(_))` before it ever demotes
    // `transaction.state` away from `Applying` - correct in that nothing was
    // mutated (the whole point of `AbortAll`), but it leaves the *journal*
    // durably claiming a batch is still in flight when it never started.
    // `assess_recovery` already treats a lingering `Applying` state as
    // `ManualRecoveryRequired` (fail-closed, never mistaken for safe), so
    // this was never a safety hole - only a stale-journal ergonomics gap. If
    // any directories were created (by `ensure_destination_directories`,
    // above) before the shared executor refused the batch, they are
    // harmless (empty, owned, and `rollback_plan_transaction` still removes
    // them) but would otherwise sit next to a journal that never says the
    // batch actually failed. Demoting to `ApplyFailed` here - the same
    // terminal state a batch that fails after starting to mutate reaches -
    // fixes that without touching the shared executor at all.
    if result.is_err() && transaction.state == TransactionState::Applying {
        transaction.state = TransactionState::ApplyFailed;
        let _ = write_journal(journal_dir, transaction);
    }

    result
}

/// Rolls back a plan transaction: the shared entry-move rollback (with
/// ancestor-directory containment re-verified immediately before every
/// reverse rename, via [`rollback_transaction_confined`] - see that
/// function's doc comment for why the leaf-only checks in ordinary
/// `rollback_transaction` are not enough), then any directories this
/// transaction created that are now empty, deepest first - the same
/// discipline as
/// [`crate::dat::rom_organisation::transaction::rollback_organisation_transaction`].
///
/// `trusted` should be the same [`TrustedRoots`] the transaction was applied
/// with; an empty set disables the ancestor check (matching
/// `rollback_transaction`'s unconfined behavior) rather than refusing
/// everything.
pub fn rollback_plan_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
    trusted: &TrustedRoots,
) -> Result<PlanRollbackOutcome, String> {
    let rollback = rollback_transaction_confined(transaction, journal_dir, cancel, trusted)?;

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

// --------------------------------------------------------------------
// First-real-canary eligibility and preview (Batch 16, sections 10-15,
// 23-24)
// --------------------------------------------------------------------

/// The hardcoded, non-configurable production root a canary candidate must
/// never touch - milestone section 13. This is a defensive belt layered on
/// top of whatever disposable canary root a caller supplies, never a
/// replacement for it: a caller must still supply its own trusted,
/// disposable canary root to [`assess_canary_eligibility`].
const PRODUCTION_ROMS_ROOT: &str = "/mnt/games/roms";

fn is_under_production_roms_root(path: &Path) -> bool {
    path.starts_with(PRODUCTION_ROMS_ROOT)
}

/// The only hard-conflict policy a real-apply run is ever allowed to
/// request - milestone section 10. There is deliberately no variant that
/// can express [`HardConflictMode::SkipUnsafeSubset`]: a future real-apply
/// entry point cannot request it even by mistake, because this type simply
/// cannot carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealApplyPolicy {
    /// The first, smallest, most conservative real-apply mode.
    Canary,
}

impl RealApplyPolicy {
    /// The only [`HardConflictMode`] this policy may ever produce.
    pub fn hard_conflict_mode(self) -> HardConflictMode {
        match self {
            RealApplyPolicy::Canary => HardConflictMode::AbortAll,
        }
    }
}

/// The conservative first-canary file-size ceiling - milestone section 12.
/// Modelled here, in the future-canary validator, deliberately never in the
/// shared executor (which has, and should keep, no size policy of its own).
pub const CANARY_MAX_SIZE_BYTES: u64 = 64 * 1024 * 1024;

/// Every reason one candidate item was refused for a first real canary -
/// milestone section 11's exhaustive list. Never a vague "not okay".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CanaryIneligibleReason {
    DigestStale,
    NotReady,
    HasBlockers,
    NotApproved,
    NoDestination,
    BelongsToSet,
    HasSupportAssociation,
    SourceMissing,
    SourceNotRegularFile,
    SourceIsSymlink,
    DestinationAlreadyExists,
    DestinationParentMissing,
    NotSameFilesystem,
    NoHashPrecondition,
    SourceOutsideCanaryRoot,
    DestinationOutsideCanaryRoot,
    SourceUnderProductionRoot,
    DestinationUnderProductionRoot,
    SourceTooLarge { bytes: u64, limit: u64 },
    CycleOrDuplicateTarget,
}

/// The precondition-strength report milestone section 23 asks for - one
/// explicit boolean/value per named check, never a vague "looks okay".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanaryPreconditionReport {
    pub physical_hash_present: bool,
    pub normalized_hash_present: bool,
    pub is_regular_file: bool,
    pub is_symlink: bool,
    pub same_filesystem: bool,
    pub destination_clear: bool,
    pub size_bytes: Option<u64>,
    pub strong_enough_for_canary: bool,
}

/// Assesses whether one export item is eligible to be the very first real
/// canary - milestone sections 11-15. Read-only: inspects the filesystem
/// (identity, existence, device ids) but never mutates it, and never builds
/// a transaction. `canary_root` is the caller's own disposable, trusted
/// staging directory; `/mnt/games/roms` is refused unconditionally
/// regardless of what `canary_root` is.
pub fn assess_canary_eligibility(
    export: &LibraryPlanExport,
    item: &LibraryPlanExportItem,
    approved: &ApprovedPlan,
    canary_root: &Path,
) -> Result<CanaryPreconditionReport, Vec<CanaryIneligibleReason>> {
    let mut reasons = Vec::new();

    if compute_plan_digest(export).as_str() != approved.digest.as_str() {
        reasons.push(CanaryIneligibleReason::DigestStale);
    }
    if item.status != PlanStatus::Ready {
        reasons.push(CanaryIneligibleReason::NotReady);
    }
    if !item.blockers.is_empty() {
        reasons.push(CanaryIneligibleReason::HasBlockers);
    }
    if !approved
        .approved_item_ids
        .contains(&item.precondition.source_path)
    {
        reasons.push(CanaryIneligibleReason::NotApproved);
    }
    let Some(destination_str) = item.proposed_destination.as_ref() else {
        reasons.push(CanaryIneligibleReason::NoDestination);
        return Err(reasons);
    };
    if item.set_label.is_some() {
        reasons.push(CanaryIneligibleReason::BelongsToSet);
    }
    if item.support_role.is_some() || item.support_association.is_some() {
        reasons.push(CanaryIneligibleReason::HasSupportAssociation);
    }

    let source_path = Path::new(&item.precondition.source_path);
    let destination_path = Path::new(destination_str);

    if is_under_production_roms_root(source_path) {
        reasons.push(CanaryIneligibleReason::SourceUnderProductionRoot);
    }
    if is_under_production_roms_root(destination_path) {
        reasons.push(CanaryIneligibleReason::DestinationUnderProductionRoot);
    }
    if !source_path.starts_with(canary_root) {
        reasons.push(CanaryIneligibleReason::SourceOutsideCanaryRoot);
    }
    if !destination_path.starts_with(canary_root) {
        reasons.push(CanaryIneligibleReason::DestinationOutsideCanaryRoot);
    }

    // Cycle / duplicate-target, checked against every other Ready+approved
    // item in the same export - never this item in isolation.
    let mut other_sources: BTreeSet<&str> = BTreeSet::new();
    let mut duplicate_target_count = 0usize;
    for other in &export.items {
        if other.status != PlanStatus::Ready
            || !other.blockers.is_empty()
            || !approved
                .approved_item_ids
                .contains(&other.precondition.source_path)
        {
            continue;
        }
        other_sources.insert(other.precondition.source_path.as_str());
        if other.proposed_destination.as_deref() == Some(destination_str.as_str()) {
            duplicate_target_count += 1;
        }
    }
    let is_cycle = other_sources.contains(destination_str.as_str());
    if duplicate_target_count > 1 || is_cycle {
        reasons.push(CanaryIneligibleReason::CycleOrDuplicateTarget);
    }

    let identity = capture_identity(source_path);
    let (is_regular_file, is_symlink, size_bytes) = match &identity {
        Ok(identity) => (
            identity.kind == crate::dat::rename_apply::model::ObjectKind::RegularFile,
            matches!(
                identity.kind,
                crate::dat::rename_apply::model::ObjectKind::Symlink
                    | crate::dat::rename_apply::model::ObjectKind::BrokenSymlink
            ),
            Some(identity.size_bytes),
        ),
        Err(_) => {
            reasons.push(CanaryIneligibleReason::SourceMissing);
            (false, false, None)
        }
    };
    if identity.is_ok() && !is_regular_file {
        reasons.push(CanaryIneligibleReason::SourceNotRegularFile);
    }
    if is_symlink {
        reasons.push(CanaryIneligibleReason::SourceIsSymlink);
    }
    if let Some(bytes) = size_bytes
        && bytes > CANARY_MAX_SIZE_BYTES
    {
        reasons.push(CanaryIneligibleReason::SourceTooLarge {
            bytes,
            limit: CANARY_MAX_SIZE_BYTES,
        });
    }

    let destination_clear = std::fs::symlink_metadata(destination_path).is_err();
    if !destination_clear {
        reasons.push(CanaryIneligibleReason::DestinationAlreadyExists);
    }
    let destination_parent_exists = destination_path.parent().is_some_and(|p| p.exists());
    if !destination_parent_exists {
        reasons.push(CanaryIneligibleReason::DestinationParentMissing);
    }

    let same_filesystem = source_path
        .parent()
        .zip(destination_path.parent())
        .map(|(s, d)| canary_same_filesystem(s, d))
        .unwrap_or(false);
    if !same_filesystem {
        reasons.push(CanaryIneligibleReason::NotSameFilesystem);
    }

    let physical_hash_present = item.precondition.physical_hash.is_some();
    let normalized_hash_present = item.precondition.normalized_hash.is_some();
    if !physical_hash_present && !normalized_hash_present {
        reasons.push(CanaryIneligibleReason::NoHashPrecondition);
    }

    if !reasons.is_empty() {
        return Err(reasons);
    }

    Ok(CanaryPreconditionReport {
        physical_hash_present,
        normalized_hash_present,
        is_regular_file,
        is_symlink,
        same_filesystem,
        destination_clear,
        size_bytes,
        strong_enough_for_canary: true,
    })
}

#[cfg(unix)]
fn canary_same_filesystem(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(l), Ok(r)) => l.dev() == r.dev(),
        _ => false,
    }
}
#[cfg(not(unix))]
fn canary_same_filesystem(_left: &Path, _right: &Path) -> bool {
    false
}

/// Milestone section 24's exact human-readable shape - still preview-only:
/// nothing here ever mutates, and it is built entirely from
/// [`assess_canary_eligibility`]'s own read-only output.
pub fn render_canary_preview(
    item: &LibraryPlanExportItem,
    eligibility: &Result<CanaryPreconditionReport, Vec<CanaryIneligibleReason>>,
) -> String {
    let mut out = String::new();
    out.push_str("REAL APPLY CANARY PREVIEW\n\n");
    out.push_str("Mode:\n  AbortAll\n\n");
    out.push_str(&format!("Source:\n  {}\n\n", item.precondition.source_path));
    out.push_str(&format!(
        "Destination:\n  {}\n\n",
        item.proposed_destination.as_deref().unwrap_or("(none)")
    ));
    match eligibility {
        Ok(report) => {
            out.push_str(&format!(
                "Size:\n  {}\n\n",
                report
                    .size_bytes
                    .map(|bytes| format!("{bytes} bytes"))
                    .unwrap_or_else(|| "unknown".to_string())
            ));
            out.push_str(&format!(
                "Physical hash:\n  {}\n\n",
                if report.physical_hash_present {
                    "present"
                } else {
                    "absent"
                }
            ));
            out.push_str(&format!(
                "Same filesystem:\n  {}\n\n",
                if report.same_filesystem { "YES" } else { "NO" }
            ));
            out.push_str(&format!(
                "Symlink:\n  {}\n\n",
                if report.is_symlink { "YES" } else { "NO" }
            ));
            out.push_str("Preconditions:\n  PASS\n\n");
            out.push_str("Blast radius:\n  1 file\n\n");
        }
        Err(reasons) => {
            out.push_str("Preconditions:\n  FAIL\n");
            for reason in reasons {
                out.push_str(&format!("    - {reason:?}\n"));
            }
            out.push('\n');
        }
    }
    out.push_str("Approval:\n  REQUIRED\n");
    out.push_str("\nApplied:\n  NO\n");
    out
}

// --------------------------------------------------------------------
// Developer-probe hard temp-safety guard (Batch 15, milestone section 39)
// --------------------------------------------------------------------

/// Whether every operation in `preview` has both its source and its
/// destination underneath `root` - the hard guard a mutation-capable
/// developer probe must check before ever invoking the executor. Pure and
/// read-only: it inspects only the strings already captured in the preview,
/// never touches the filesystem, and never trusts a caller-supplied
/// destination that was not already confined to a root the probe created
/// itself.
pub fn preview_is_confined_to_root(preview: &TransactionPreview, root: &Path) -> bool {
    preview.operations.iter().all(|op| {
        let source_ok = Path::new(&op.source_path).starts_with(root);
        let destination_ok = op
            .destination_path
            .as_deref()
            .map(|destination| Path::new(destination).starts_with(root))
            .unwrap_or(false);
        source_ok && destination_ok
    })
}

// --------------------------------------------------------------------
// Human-readable manual recovery output (Batch 15, milestone section 36)
// --------------------------------------------------------------------

/// Renders a human-readable manual recovery report for a transaction whose
/// [`RecoveryAssessment`] is anything other than a clean settled state -
/// milestone section 36. Never proposes or performs a destructive fix; it
/// only describes what is known, what is uncertain, and a safe next step.
///
/// Deliberately reports `plan_generation` (the only plan-identifying value
/// [`RenameTransaction`] itself persists) rather than a full [`PlanDigest`]:
/// this module never stores the 64-character digest on the transaction
/// itself, only the derived generation number folded into
/// `plan_generation` by [`plan_generation_of`]. A human recovering by hand
/// can still match that number against a freshly recomputed
/// `plan_generation_of` on the export they believe was in force.
pub fn render_recovery_report(
    transaction: &RenameTransaction,
    issues: &[RecoveryIssue],
    assessment: RecoveryAssessment,
) -> String {
    let mut out = String::new();
    out.push_str("MANUAL RECOVERY REPORT\n\n");
    out.push_str(&format!(
        "Transaction id:\n  {}\n",
        transaction.transaction_id
    ));
    out.push_str(&format!(
        "Plan generation (derived from the plan digest):\n  {}\n",
        transaction.plan_generation
    ));
    out.push_str(&format!(
        "Current transaction state:\n  {:?}\n",
        transaction.state
    ));
    out.push_str(&format!("Assessment:\n  {assessment:?}\n\n"));

    let last_successful = transaction
        .entries
        .iter()
        .filter(|entry| entry.state == EntryState::Applied)
        .max_by_key(|entry| entry.applied_at_unix.unwrap_or(0));
    out.push_str("Last successful operation:\n");
    match last_successful {
        Some(entry) => {
            out.push_str(&format!("  Source:      {}\n", entry.source_path.display()));
            out.push_str(&format!(
                "  Destination: {}\n",
                entry.destination_path.display()
            ));
        }
        None => out.push_str("  (none - nothing in this transaction is confirmed applied)\n"),
    }
    out.push('\n');

    out.push_str("Uncertain operations:\n");
    let uncertain: Vec<&TransactionEntry> = transaction
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.state,
                EntryState::Applying | EntryState::RollingBack | EntryState::RollbackFailed
            )
        })
        .collect();
    if uncertain.is_empty() && issues.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for entry in &uncertain {
            let observation = observe_path(&entry.source_path, &entry.destination_path);
            out.push_str(&format!(
                "  - Original source:     {}\n",
                entry.source_path.display()
            ));
            out.push_str(&format!(
                "    Intended destination: {}\n",
                entry.destination_path.display()
            ));
            out.push_str(&format!(
                "    Expected identity:    size={} bytes, kind={:?}\n",
                entry.identity.size_bytes, entry.identity.kind
            ));
            out.push_str(&format!("    Current observation:  {observation}\n"));
            // Surfaces the exact refusal reason (e.g. an ancestor-symlink
            // substitution) rather than only the generic "changed" facts
            // above - so a genuinely detectable cause is never reported as
            // a vague "missing file".
            if let Some(reason) = &entry.failure_reason {
                out.push_str(&format!("    Refusal reason:       {reason}\n"));
            }
        }
        for issue in issues {
            out.push_str(&format!(
                "  - Journal finding (entry #{}): {}\n",
                issue.entry_index, issue.detail
            ));
        }
    }
    out.push('\n');

    out.push_str("Suggested non-destructive next step:\n  ");
    out.push_str(match assessment {
        RecoveryAssessment::SafeToResume => {
            "rebuild a fresh transaction from a current plan export and apply it; nothing was mutated by this one."
        }
        RecoveryAssessment::SafeToRollback => {
            "call rollback on this exact transaction id to reverse its applied entries."
        }
        RecoveryAssessment::AlreadyCommitted => {
            "nothing to do; every entry already settled cleanly."
        }
        RecoveryAssessment::AlreadyRolledBack => {
            "nothing to do; every entry was already reversed."
        }
        RecoveryAssessment::ManualRecoveryRequired => {
            "do not run apply or rollback automatically; inspect each uncertain operation above \
             by hand, comparing the current observation against the expected identity, before \
             deciding a safe manual action."
        }
    });
    out.push('\n');
    out
}

fn observe_path(source: &Path, destination: &Path) -> String {
    let source_state = describe_symlink_metadata(source);
    let destination_state = describe_symlink_metadata(destination);
    format!("source={source_state}, destination={destination_state}")
}

fn describe_symlink_metadata(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                "present (symlink)".to_string()
            } else if metadata.is_dir() {
                "present (directory)".to_string()
            } else {
                format!("present ({} bytes)", metadata.len())
            }
        }
        Err(_) => "absent".to_string(),
    }
}

#[cfg(test)]
mod canary_run_tests;
#[cfg(test)]
mod closeout_tests;
#[cfg(test)]
mod hardening_tests;
#[cfg(test)]
mod real_rom_canary_tests;
#[cfg(test)]
mod tests;
