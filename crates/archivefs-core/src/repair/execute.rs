//! Repair transaction execution, reusing the existing `rename_apply` engine.
//!
//! The Repair Center deliberately does **not** invent a parallel transaction
//! system. A repair batch becomes a
//! [`crate::dat::rename_apply::RenameTransaction`], the exact journaled,
//! no-clobber, identity-revalidated model the hardened rename layer already
//! ships, and is applied and rolled back through that engine. The only
//! additions here are:
//!
//! 1. the mapping from a validated [`RepairPlan`] to that transaction
//!    ([`build_repair_transaction`]),
//! 2. the executor driver that enforces "no action may start if the plan is
//!    invalid" ([`apply_repair_transaction`]),
//! 3. a post-apply re-verification pass ([`reverify_transaction`]) and
//! 4. a recovery classification over the existing journal directory
//!    ([`classify_persisted_transactions`]).
//!
//! # The safety boundary is inherited, not re-implemented
//!
//! Every mutation goes through `renameat2(RENAME_NOREPLACE)`, every source is
//! re-verified against its recorded `ObjectIdentity` immediately before the
//! rename and confirmed at the destination after it, the batch is journaled
//! durably before the first mutation, and a failure rolls back completed steps
//! in reverse order with partial-rollback honesty - all from `rename_apply`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use crate::dat::classification::CLASSIFIER_VERSION;
use crate::dat::rename_apply::executor::{ApplyExecution, HardConflictMode, apply_transaction};
use crate::dat::rename_apply::identity::{capture_identity, identity_matches};
use crate::dat::rename_apply::journal::{
    find_recovery_transactions, find_rollbackable_transactions, list_journals, new_transaction_id,
};
use crate::dat::rename_apply::model::{
    EntryState, RenameTransaction, RollbackResult, TransactionEntry, TransactionState,
    TransactionSummary,
};
use crate::dat::rename_apply::preflight::DirectoryPolicy;
use crate::dat::rename_apply::rollback::rollback_transaction;
use crate::dat::sources::now_unix;
use crate::safe_read::TrustedRoots;

use super::plan::RepairPlan;

/// Why a repair batch could not be built or executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairExecutionError {
    /// The plan is not fully executable (conflicts, deferred actions, or a
    /// proposal the planner did not classify `Safe`). Nothing runs.
    NotExecutable { detail: String },
    /// A source's identity changed between plan build and transaction build.
    /// Never silently re-audited under the old proposal.
    StaleSource { source: PathBuf },
    /// The transaction could not be constructed (e.g. a bad id).
    Build { detail: String },
    /// The underlying rename engine refused the batch.
    Apply(String),
    /// The transaction could not be built into a journal-safe id.
    InvalidTransactionId(String),
}

impl std::fmt::Display for RepairExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotExecutable { detail } => {
                write!(f, "the repair plan is not executable: {detail}")
            }
            Self::StaleSource { source } => write!(
                f,
                "the source '{}' changed since it was proposed; re-audit before executing",
                source.display()
            ),
            Self::Build { detail } => write!(f, "could not build the repair transaction: {detail}"),
            Self::Apply(detail) => write!(f, "repair apply failed: {detail}"),
            Self::InvalidTransactionId(id) => {
                write!(f, "transaction id '{id}' cannot name a journal file")
            }
        }
    }
}

impl std::error::Error for RepairExecutionError {}

/// What the Repair Center needs to execute a batch: the trusted roots and the
/// journal directory (reusing the rename transaction journal).
#[derive(Debug, Clone)]
pub struct RepairExecutionOptions {
    pub trusted: TrustedRoots,
    pub journal_dir: PathBuf,
}

/// The outcome of one repair batch, including the reused transaction model and
/// a post-apply re-verification pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairTransactionResult {
    /// The journaled transaction exactly as the rename engine left it.
    pub transaction: RenameTransaction,
    pub summary: TransactionSummary,
    /// Re-verification of every applied destination against the recorded
    /// source identity, immediately after the batch.
    pub reverify: Vec<RepairReverifyEntry>,
}

/// The post-apply re-verification result for one applied entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReverifyEntry {
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    pub outcome: RepairReverifyOutcome,
    pub detail: String,
}

/// How the post-apply re-verification of a destination resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairReverifyOutcome {
    /// The destination exists and still matches the recorded source identity.
    Verified,
    /// The destination is missing.
    Missing,
    /// The destination exists but is no longer the recorded object.
    Changed,
}

impl RepairReverifyOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Missing => "missing",
            Self::Changed => "changed",
        }
    }
}

/// Builds the journal-backed transaction for a fully executable plan, capturing
/// each source's identity **now** (review time). Read-only: no journal is
/// written and nothing is renamed.
///
/// A proposal whose recorded identity no longer matches the live source fails
/// the build: the batch is stale and must be re-planned, never executed under
/// old evidence.
pub fn build_repair_transaction(
    plan: &RepairPlan,
) -> Result<RenameTransaction, RepairExecutionError> {
    if !plan.all_executable() {
        return Err(RepairExecutionError::NotExecutable {
            detail: "the plan has conflicts or contains non-executable proposals".to_string(),
        });
    }

    let mut entries = Vec::with_capacity(plan.proposals.len());
    for proposal in plan.executable_proposals() {
        let Some(destination) = proposal.destination() else {
            return Err(RepairExecutionError::Build {
                detail: format!(
                    "proposal '{}' is executable but has no destination",
                    proposal.id
                ),
            });
        };
        let Some(source_basename) = proposal
            .source_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return Err(RepairExecutionError::Build {
                detail: format!("proposal '{}' source has no basename", proposal.id),
            });
        };
        let Some(destination_basename) = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            return Err(RepairExecutionError::Build {
                detail: format!("proposal '{}' destination has no basename", proposal.id),
            });
        };

        let current = capture_identity(&proposal.source_path).map_err(|_| {
            RepairExecutionError::StaleSource {
                source: proposal.source_path.clone(),
            }
        })?;
        let identity = match proposal.expected_source_identity.as_ref() {
            Some(expected) if !identity_matches(expected, &current) => {
                return Err(RepairExecutionError::StaleSource {
                    source: proposal.source_path.clone(),
                });
            }
            Some(expected) => expected.clone(),
            None => current,
        };

        entries.push(TransactionEntry {
            source_path: proposal.source_path.clone(),
            destination_path: destination.clone(),
            original_basename: source_basename,
            proposed_basename: destination_basename,
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
        return Err(RepairExecutionError::NotExecutable {
            detail: "the plan contains no executable proposals".to_string(),
        });
    }

    let transaction_id = new_transaction_id(now_unix());
    if crate::dat::rename_apply::journal::journal_file_name(&transaction_id).is_none() {
        return Err(RepairExecutionError::InvalidTransactionId(transaction_id));
    }

    Ok(RenameTransaction {
        transaction_id,
        // Repair plans re-validate against the same evidence rules as rename
        // plans; a journal built under older rules is refused.
        classifier_version: Some(CLASSIFIER_VERSION.to_string()),
        plan_generation: plan.generation,
        created_at_unix: now_unix(),
        source_scan_root: plan.source_scan_id.clone().unwrap_or_default(),
        state: TransactionState::Planned,
        entries,
        created_directories: Vec::new(),
        unknown: Default::default(),
    })
}

/// Everything `apply_repair_transaction` needs, mirroring the rename engine's
/// execution struct but carrying an owned transaction.
#[derive(Debug)]
pub struct RepairApplyExecution<'a> {
    pub transaction: &'a mut RenameTransaction,
    pub options: &'a RepairExecutionOptions,
    pub cancel: &'a AtomicBool,
}

/// Applies a prebuilt repair transaction through the reused rename engine.
///
/// The engine: whole-batch preflight (abort on any hard conflict, never a
/// silent subset), durable journal before the first mutation, per-entry
/// `Applying` checkpoint, `renameat2(RENAME_NOREPLACE)`, filesystem-confirmed
/// `Applied`, and reverse-order rollback on failure.
pub fn apply_repair_transaction(
    execution: &mut RepairApplyExecution<'_>,
) -> Result<RepairTransactionResult, RepairExecutionError> {
    let approved_paths: BTreeSet<String> = execution
        .transaction
        .entries
        .iter()
        .map(|entry| entry.source_path.to_string_lossy().into_owned())
        .collect();
    let current_generation = execution.transaction.plan_generation;

    let mut apply = ApplyExecution {
        transaction: execution.transaction,
        approved_paths,
        current_generation,
        trusted: execution.options.trusted.clone(),
        journal_dir: execution.options.journal_dir.clone(),
        hard_conflict_mode: HardConflictMode::AbortAll,
        cancel: execution.cancel,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source: false,
    };

    let outcome = apply_transaction(&mut apply)
        .map_err(|error| RepairExecutionError::Apply(error.to_string()))?;

    let reverify = reverify_transaction(&outcome.transaction);
    Ok(RepairTransactionResult {
        transaction: outcome.transaction,
        summary: outcome.summary,
        reverify,
    })
}

/// Convenience: builds and applies a repair plan in one step. The transaction
/// is built (identities captured) and applied (identities re-checked) with no
/// window for the caller to drift from the plan it approved.
pub fn execute_repair_plan(
    plan: &RepairPlan,
    options: &RepairExecutionOptions,
    cancel: &AtomicBool,
) -> Result<RepairTransactionResult, RepairExecutionError> {
    let mut transaction = build_repair_transaction(plan)?;
    apply_repair_transaction(&mut RepairApplyExecution {
        transaction: &mut transaction,
        options,
        cancel,
    })
}

/// Re-verifies every applied destination against the recorded source identity.
/// This is the REVERIFY / AUDIT leg of the Repair Center.
pub fn reverify_transaction(transaction: &RenameTransaction) -> Vec<RepairReverifyEntry> {
    transaction
        .entries
        .iter()
        .filter(|entry| entry.state == EntryState::Applied)
        .map(|entry| {
            let (outcome, detail) = match capture_identity(&entry.destination_path) {
                Err(_) => (
                    RepairReverifyOutcome::Missing,
                    "the destination does not exist after apply".to_string(),
                ),
                Ok(current) if identity_matches(&entry.identity, &current) => (
                    RepairReverifyOutcome::Verified,
                    "the destination matches the recorded source identity".to_string(),
                ),
                Ok(_) => (
                    RepairReverifyOutcome::Changed,
                    "the destination no longer matches the recorded source identity".to_string(),
                ),
            };
            RepairReverifyEntry {
                source_path: entry.source_path.clone(),
                destination_path: entry.destination_path.clone(),
                outcome,
                detail,
            }
        })
        .collect()
}

/// Rolls back an applied repair transaction through the reused rollback
/// engine: reverse order, destination re-verified, no-clobber reverse rename,
/// and an explicit partial/failed rollback distinction.
pub fn rollback_repair_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<RollbackResult, String> {
    rollback_transaction(transaction, journal_dir, cancel).map(|outcome| outcome.result)
}

/// How a persisted (journaled) repair/rename transaction is classified for
/// recovery. Reuses the existing journal discovery; no second journal format.
#[derive(Debug, Clone, Default)]
pub struct RepairRecoveryReport {
    /// Settled transactions (fully applied and confirmed, or fully rolled back).
    /// Never replayed.
    pub complete: Vec<RenameTransaction>,
    /// Interrupted or failed transactions that recovery must surface.
    pub recoverable: Vec<RenameTransaction>,
    /// Transactions a user can still roll back (settled applied batches with
    /// applied entries, plus everything recoverable).
    pub rollbackable: Vec<RenameTransaction>,
    /// Journals that could not be parsed. Surfaced, never deleted.
    pub corrupt: Vec<String>,
}

/// Classifies every persisted transaction in `journal_dir`.
pub fn classify_persisted_transactions(journal_dir: &std::path::Path) -> RepairRecoveryReport {
    let (all, corrupt) = list_journals(journal_dir);
    let (recoverable, _) = find_recovery_transactions(journal_dir);
    let (rollbackable, _) = find_rollbackable_transactions(journal_dir);

    let complete: Vec<RenameTransaction> = all
        .iter()
        .filter(|transaction| {
            !transaction.state.needs_recovery()
                && transaction.state != TransactionState::Applying
                && transaction.state != TransactionState::RollingBack
        })
        .cloned()
        .collect();

    RepairRecoveryReport {
        complete,
        recoverable,
        rollbackable,
        corrupt,
    }
}
