//! The gated transaction executor: the only place renames happen.
//!
//! The executor turns an explicitly approved subset of a rename plan into a
//! journal-backed transaction, preflights the whole batch, journals it durably
//! **before** mutating anything, then applies entries one at a time - each one
//! freshly preflighted, renamed with a no-clobber primitive, and confirmed by
//! the filesystem before it is marked Applied. Nothing here runs unattended;
//! it never retries a failed mutation, and a failure stops the batch.
//!
//! # Build and apply are separate layers
//!
//! [`build_transaction`] captures each approved source's identity **at review
//! time** (when the user previews the batch). [`apply_transaction`] then
//! preflights everything again **immediately before** each rename, so a hostile
//! change between review and apply - a different inode, a symlink substitution,
//! a resized file, an appearing destination - is caught. The GUI never calls
//! `std::fs::rename`; this module owns every mutation.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dat::rename_plan::{RenamePlan, RenameProposal};
use crate::safe_read::TrustedRoots;

use super::identity::capture_identity;
use super::journal::{new_transaction_id, write_journal};
use super::model::{
    EntryState, RenameTransaction, TransactionEntry, TransactionState, TransactionSummary,
};
use super::noclobber::{NoClobberError, rename_noreplace};
use super::preflight::{PreflightOptions, batch_destinations, run_preflight};

/// How the executor treats a batch that contains a hard preflight conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardConflictMode {
    /// Any hard conflict prevents the batch from starting at all; nothing is
    /// mutated and no journal is written.
    AbortAll,
    /// Only the entries that pass preflight are applied; every other entry is
    /// journaled as Skipped. The user must have explicitly chosen this.
    SkipUnsafeSubset,
}

/// Why a batch could not be built or started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// No proposals were both approved and actionable.
    NothingApproved,
    /// The plan is stale (its generation no longer matches the current one).
    StalePlan { plan: u64, current: u64 },
    /// One or more entries failed preflight and the batch is in AbortAll mode.
    HardConflicts(Vec<(PathBuf, Vec<String>)>),
    /// The transaction id could not name a journal file.
    InvalidTransactionId(String),
    /// Writing the journal failed.
    Journal(String),
    /// The batch was cancelled before any mutation.
    Cancelled,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingApproved => {
                write!(f, "no approved, actionable proposals were selected")
            }
            Self::StalePlan { plan, current } => write!(
                f,
                "the plan is stale (generation {plan}, current {current}); run a fresh audit"
            ),
            Self::HardConflicts(conflicts) => {
                write!(f, "preflight found {} hard conflict(s)", conflicts.len())
            }
            Self::InvalidTransactionId(id) => write!(f, "transaction id '{id}' is not usable"),
            Self::Journal(detail) => write!(f, "could not write the transaction journal: {detail}"),
            Self::Cancelled => write!(f, "the apply was cancelled"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// The result of an apply pass.
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    pub transaction: RenameTransaction,
    pub summary: TransactionSummary,
}

/// Builds the transaction for the approved, actionable proposals of a plan,
/// capturing each source's identity **at review time**.
///
/// Read-only (identity capture never follows a link) and side-effect free: it
/// writes no journal and renames nothing. The returned transaction is what a
/// caller shows in its read-only review before execution.
pub fn build_transaction(
    plan: &RenamePlan,
    approved_paths: &BTreeSet<String>,
    current_generation: u64,
) -> Result<RenameTransaction, ApplyError> {
    if plan.generation != current_generation {
        return Err(ApplyError::StalePlan {
            plan: plan.generation,
            current: current_generation,
        });
    }
    let entries = build_transaction_entries(plan, approved_paths);
    if entries.is_empty() {
        return Err(ApplyError::NothingApproved);
    }
    Ok(RenameTransaction {
        transaction_id: new_transaction_id(crate::dat::sources::now_unix()),
        plan_generation: plan.generation,
        created_at_unix: crate::dat::sources::now_unix(),
        source_scan_root: plan.scan_root.clone(),
        state: TransactionState::Planned,
        entries,
        created_directories: Vec::new(),
        unknown: Default::default(),
    })
}

/// Builds the transaction entries for the approved, actionable proposals of a
/// plan, capturing each source's current identity. Read-only.
pub fn build_transaction_entries(
    plan: &RenamePlan,
    approved_paths: &BTreeSet<String>,
) -> Vec<TransactionEntry> {
    let mut entries = Vec::new();
    for proposal in &plan.proposals {
        if !is_applicable_proposal(proposal) {
            continue;
        }
        if !approved_paths.contains(&proposal.source_path.to_string_lossy().into_owned()) {
            continue;
        }
        let Some(proposed) = &proposal.proposed_basename else {
            continue;
        };
        let Some(parent) = proposal.source_path.parent() else {
            continue;
        };
        let Ok(identity) = capture_identity(&proposal.source_path) else {
            // The source is gone between plan and build; the entry cannot be
            // built with a recorded identity, so it is not included.
            continue;
        };
        entries.push(TransactionEntry {
            source_path: proposal.source_path.clone(),
            destination_path: parent.join(proposed),
            original_basename: proposal.current_basename.clone(),
            proposed_basename: proposed.clone(),
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
    entries
}

/// Whether a proposal may ever be applied: it must be a Suggested, actionable
/// proposal with no collision, a proposed name, and a regular-file source.
fn is_applicable_proposal(proposal: &RenameProposal) -> bool {
    use crate::dat::rename_plan::{ProposalState, SourceObjectKind};
    proposal.state == ProposalState::Suggested
        && proposal.actionable
        && proposal.collision.is_none()
        && proposal.proposed_basename.is_some()
        && proposal.object_kind == SourceObjectKind::RegularFile
}

/// The approval helper the GUI uses: does a recorded decision count as
/// approved for apply?
pub fn is_approved(decision: &crate::dat::rename_plan::ReviewDecision) -> bool {
    *decision == crate::dat::rename_plan::ReviewDecision::AcceptedForReview
}

/// Everything the executor needs to apply a transaction that was built at
/// review time.
#[derive(Debug)]
pub struct ApplyExecution<'a> {
    /// The transaction to apply (built by [`build_transaction`]).
    pub transaction: &'a mut RenameTransaction,
    /// The approved source paths the transaction was built from, re-checked
    /// by preflight.
    pub approved_paths: BTreeSet<String>,
    /// The current plan generation; must equal the transaction's.
    pub current_generation: u64,
    pub trusted: TrustedRoots,
    pub journal_dir: PathBuf,
    pub hard_conflict_mode: HardConflictMode,
    pub cancel: &'a AtomicBool,
    /// Whether the destination may live outside the source's directory.
    /// `SameDirectory` (the default) preserves rename-apply's behaviour.
    pub directory_policy: super::preflight::DirectoryPolicy,
    /// Whether a symlink object is an acceptable source (the ROM organiser's
    /// symlink-only mode). The link object itself is renamed; its target is
    /// never dereferenced and its identity is still re-verified.
    pub allow_symlink_source: bool,
}

/// Applies a prebuilt transaction. This is the only place a rename happens.
pub fn apply_transaction(execution: &mut ApplyExecution<'_>) -> Result<ApplyOutcome, ApplyError> {
    if cancelled(execution.cancel) {
        return Err(ApplyError::Cancelled);
    }
    let transaction = &mut *execution.transaction;
    if transaction.plan_generation != execution.current_generation {
        return Err(ApplyError::StalePlan {
            plan: transaction.plan_generation,
            current: execution.current_generation,
        });
    }

    let destinations = batch_destinations(&transaction.entries);
    let preflight_options = PreflightOptions {
        plan_generation: transaction.plan_generation,
        current_generation: execution.current_generation,
        approved_paths: &execution.approved_paths,
        trusted: &execution.trusted,
        batch_destinations: &destinations,
        directory_policy: execution.directory_policy,
        allow_symlink_source: execution.allow_symlink_source,
    };

    // Preflight the whole batch first.
    let mut hard_conflicts: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for entry in &mut transaction.entries {
        if let Err(failures) = run_preflight(entry, &preflight_options) {
            entry.preflight_failures = failures.iter().map(|f| f.reason()).collect();
            entry.preflight_passed = false;
            hard_conflicts.push((entry.source_path.clone(), entry.preflight_failures.clone()));
        } else {
            entry.preflight_passed = true;
        }
    }

    match execution.hard_conflict_mode {
        HardConflictMode::AbortAll => {
            if !hard_conflicts.is_empty() {
                return Err(ApplyError::HardConflicts(hard_conflicts));
            }
        }
        HardConflictMode::SkipUnsafeSubset => {
            for entry in &mut transaction.entries {
                if !entry.preflight_passed {
                    entry.state = EntryState::Skipped;
                }
            }
        }
    }

    // Journal the exact batch before the first mutation.
    write_journal(&execution.journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    if transaction
        .entries
        .iter()
        .all(|e| e.state == EntryState::Skipped)
    {
        transaction.state = TransactionState::ApplyFailed;
        write_journal(&execution.journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
        let summary = TransactionSummary::from_transaction(transaction);
        return Ok(ApplyOutcome {
            transaction: transaction.clone(),
            summary,
        });
    }

    transaction.state = TransactionState::Applying;
    write_journal(&execution.journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    // Apply one at a time. For each entry the journal is durably checkpointed
    // to `Applying` BEFORE the rename syscall, so a crash at any point leaves
    // a recoverable record (the file is either at the source with the entry
    // Planned/Applying, or at the destination with the entry Applying, and
    // recovery reconciles either case).
    for index in 0..transaction.entries.len() {
        if cancelled(execution.cancel) {
            transaction.state = TransactionState::ApplyFailed;
            write_journal(&execution.journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            let summary = TransactionSummary::from_transaction(transaction);
            return Ok(ApplyOutcome {
                transaction: transaction.clone(),
                summary,
            });
        }
        if transaction.entries[index].state == EntryState::Skipped {
            continue;
        }

        // Preflight (before entering the mutation window).
        if let Err(failures) = run_preflight(&transaction.entries[index], &preflight_options) {
            transaction.entries[index].preflight_passed = false;
            transaction.entries[index].preflight_failures =
                failures.iter().map(|f| f.reason()).collect();
            transaction.entries[index].state = EntryState::ApplyFailed;
            transaction.entries[index].failure_reason = Some(
                failures
                    .iter()
                    .map(|f| f.reason())
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            transaction.state = TransactionState::ApplyFailed;
            write_journal(&execution.journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            break;
        }
        transaction.entries[index].preflight_passed = true;

        // Durable `Applying` checkpoint BEFORE the rename syscall. If this
        // write fails, no mutation may happen.
        transaction.entries[index].state = EntryState::Applying;
        write_journal(&execution.journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;

        // Mutation window: the rename syscall plus the filesystem
        // confirmation. Nothing else runs while the file is in flight.
        match apply_mutation(&transaction.entries[index]) {
            Ok(()) => {
                transaction.entries[index].state = EntryState::Applied;
                transaction.entries[index].applied_at_unix = Some(crate::dat::sources::now_unix());
            }
            Err((state, reason)) => {
                transaction.entries[index].state = state;
                transaction.entries[index].failure_reason = Some(reason);
                transaction.state = TransactionState::ApplyFailed;
                write_journal(&execution.journal_dir, transaction)
                    .map_err(|error| ApplyError::Journal(error.to_string()))?;
                break;
            }
        }
        write_journal(&execution.journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
    }

    if transaction.state == TransactionState::Applying {
        transaction.state = TransactionState::Applied;
    }
    write_journal(&execution.journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    let summary = TransactionSummary::from_transaction(transaction);
    Ok(ApplyOutcome {
        transaction: transaction.clone(),
        summary,
    })
}

/// The mutation window: the no-clobber rename and the filesystem confirmation.
///
/// Called only after the entry's `Applying` state has been durably persisted.
/// On success returns `Ok`; on any failure returns the entry state to record
/// (`ApplyFailed`) and the exact reason.
fn apply_mutation(entry: &TransactionEntry) -> Result<(), (EntryState, String)> {
    match rename_noreplace(&entry.source_path, &entry.destination_path) {
        Ok(()) => {
            // The filesystem must confirm the rename before Applied.
            match confirm_rename(entry) {
                Ok(()) => Ok(()),
                Err(reason) => Err((EntryState::ApplyFailed, reason)),
            }
        }
        Err(NoClobberError::DestinationExists) => Err((
            EntryState::ApplyFailed,
            "the destination appeared during apply and was never overwritten".to_string(),
        )),
        Err(error) => Err((EntryState::ApplyFailed, error.to_string())),
    }
}

/// Confirms, from the filesystem, that a rename actually happened: the source
/// is gone and the destination exists with the recorded identity.
fn confirm_rename(entry: &TransactionEntry) -> Result<(), String> {
    if std::fs::symlink_metadata(&entry.source_path).is_ok() {
        return Err("the source still exists after the rename".to_string());
    }
    match capture_identity(&entry.destination_path) {
        Err(_) => Err("the destination does not exist after the rename".to_string()),
        Ok(current) => {
            if super::identity::identity_matches(&entry.identity, &current) {
                Ok(())
            } else {
                Err("the destination identity does not match the recorded source".to_string())
            }
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}
