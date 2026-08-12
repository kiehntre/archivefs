//! The typed, gated transaction model for applying approved rename proposals.
//!
//! A [`RenameTransaction`] is the reversible unit of work for the explicit,
//! user-approved application of rename *plan* proposals. It is written to a
//! durable journal before any mutation, its state is persisted after every
//! transition, and it records everything needed to roll a partially applied
//! batch back.
//!
//! # No transaction may be marked Applied before the filesystem confirms it
//!
//! [`TransactionEntry::Applied`] is only set by the executor *after* the
//! no-clobber rename succeeded **and** the destination was re-checked to match
//! the recorded source identity. The state vocabulary below is exactly the one
//! the executor and rollback use; nothing else may mutate it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The overall state of a transaction.
///
/// This is the state stored in the journal. A transaction that reached
/// `Applied` (every requested entry applied) or `RolledBack` is settled;
/// anything else may be an interrupted transaction that recovery should
/// surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    /// Journal written, nothing renamed yet.
    #[default]
    Planned,
    /// At least one entry is being renamed.
    Applying,
    /// Every requested entry has been renamed and confirmed by the filesystem.
    Applied,
    /// At least one entry failed; the batch stopped. Already-applied entries
    /// are eligible for rollback.
    ApplyFailed,
    /// Rollback is in progress.
    RollingBack,
    /// Every applied entry has been reversed and confirmed.
    RolledBack,
    /// Rollback could not be completed safely.
    RollbackFailed,
}

impl TransactionState {
    /// Whether a journal in this state represents an *interrupted* transaction
    /// that crash recovery should surface (something was or may have been
    /// mutated, or the batch was not settled).
    pub fn needs_recovery(self) -> bool {
        matches!(
            self,
            Self::Planned
                | Self::Applying
                | Self::ApplyFailed
                | Self::RollingBack
                | Self::RollbackFailed
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Planned => "Planned",
            Self::Applying => "Applying",
            Self::Applied => "Applied",
            Self::ApplyFailed => "Apply failed",
            Self::RollingBack => "Rolling back",
            Self::RolledBack => "Rolled back",
            Self::RollbackFailed => "Rollback failed",
        }
    }
}

/// The per-entry state of a transaction step.
///
/// `Skipped` records an entry the batch deliberately did not rename (a hard
/// preflight conflict in the "apply only the safe subset" mode, or a later
/// hard failure that stopped the batch without touching that entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    #[default]
    Planned,
    PreflightPassed,
    Applying,
    Applied,
    ApplyFailed,
    RollingBack,
    RolledBack,
    RollbackFailed,
    Skipped,
}

impl EntryState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Planned => "Planned",
            Self::PreflightPassed => "Preflight passed",
            Self::Applying => "Applying",
            Self::Applied => "Applied",
            Self::ApplyFailed => "Apply failed",
            Self::RollingBack => "Rolling back",
            Self::RolledBack => "Rolled back",
            Self::RollbackFailed => "Rollback failed",
            Self::Skipped => "Skipped",
        }
    }
}

/// What kind of filesystem object a rename would, if approved, act on.
///
/// Only `RegularFile` is ever applied; anything else fails preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    RegularFile,
    Symlink,
    BrokenSymlink,
    Other,
}

impl ObjectKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::RegularFile => "regular file",
            Self::Symlink => "symlink",
            Self::BrokenSymlink => "broken symlink",
            Self::Other => "other",
        }
    }
}

/// A snapshot of a source file's identity at the moment the transaction was
/// built. Preflight and post-rename verification compare against this so that
/// a file replaced, resized, or swapped for a symlink after review is never
/// renamed by mistake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectIdentity {
    pub size_bytes: u64,
    /// Modification time in whole seconds since the Unix epoch.
    pub modified_unix: i64,
    pub kind: ObjectKind,
    /// Inode number, on platforms that have one.
    #[cfg(unix)]
    #[serde(default)]
    pub ino: u64,
    /// Device number, on platforms that have one.
    #[cfg(unix)]
    #[serde(default)]
    pub dev: u64,
}

/// One step of a rename transaction: one approved proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionEntry {
    /// The file's path at review time.
    pub source_path: PathBuf,
    /// The path the rename would (or did) produce.
    pub destination_path: PathBuf,
    pub original_basename: String,
    pub proposed_basename: String,
    /// The source identity captured when the transaction was built.
    pub identity: ObjectIdentity,
    /// The last preflight result for this entry, when one ran.
    #[serde(default)]
    pub preflight_passed: bool,
    #[serde(default)]
    pub preflight_failures: Vec<String>,
    #[serde(default)]
    pub state: EntryState,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub applied_at_unix: Option<u64>,
    #[serde(default)]
    pub rolled_back_at_unix: Option<u64>,
    /// Keys a future build wrote that this one does not understand, kept
    /// verbatim so reading a journal never discards them.
    #[serde(flatten)]
    #[serde(default)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl TransactionEntry {
    /// Whether a rollback pass may reverse this entry (it was applied and has
    /// not already been rolled back).
    pub fn is_eligible_for_rollback(&self) -> bool {
        self.state == EntryState::Applied
    }
}

/// A gated, journal-backed rename transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameTransaction {
    /// Stable identifier, recorded in History & Logs.
    pub transaction_id: String,
    /// The rename plan generation this batch was built from. Apply and
    /// rollback re-check it against the current plan so a stale batch is
    /// never acted on.
    pub plan_generation: u64,
    /// Classifier rules used to generate the reviewed plan. Older journals
    /// decode this as `None` and may still be inspected or rolled back, but
    /// must never be applied as though they used the current rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_version: Option<String>,
    pub created_at_unix: u64,
    /// The folder the plan audited, for provenance only.
    pub source_scan_root: String,
    #[serde(default)]
    pub state: TransactionState,
    pub entries: Vec<TransactionEntry>,
    /// Directories this transaction created so the destination would exist
    /// (for example canonical platform folders under a master ROM root).
    ///
    /// Recorded durably before they are created so crash recovery and rollback
    /// know exactly which directories belong to EmuWiz. Rollback removes
    /// only these, in reverse order, and only while they are still empty;
    /// a pre-existing user directory is never recorded here and never removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_directories: Vec<PathBuf>,
    /// Keys a future build wrote that this one does not understand, kept
    /// verbatim so reading a journal never discards them.
    #[serde(flatten)]
    #[serde(default)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl RenameTransaction {
    pub fn applied_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == EntryState::Applied)
            .count()
    }

    pub fn skipped_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == EntryState::Skipped)
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == EntryState::ApplyFailed)
            .count()
    }

    pub fn rolled_back_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == EntryState::RolledBack)
            .count()
    }

    /// Whether any entry has actually been renamed (and not yet rolled back).
    pub fn has_applied_entries(&self) -> bool {
        self.applied_count() > 0
    }

    /// Whether every entry is settled (applied, skipped, or failed - nothing
    /// still in flight).
    pub fn is_settled(&self) -> bool {
        !matches!(
            self.state,
            TransactionState::Planned | TransactionState::Applying | TransactionState::RollingBack
        )
    }
}

/// A human-readable summary of an apply or rollback run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionSummary {
    pub transaction_id: String,
    pub requested: usize,
    pub applied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub rollback: RollbackStatus,
    /// Seconds since the Unix epoch when the run started/ended, when known.
    pub started_at_unix: Option<u64>,
    pub ended_at_unix: Option<u64>,
}

impl TransactionSummary {
    pub fn from_transaction(transaction: &RenameTransaction) -> Self {
        Self {
            transaction_id: transaction.transaction_id.clone(),
            requested: transaction.entries.len(),
            applied: transaction.applied_count(),
            skipped: transaction.skipped_count(),
            failed: transaction.failed_count(),
            rollback: match transaction.state {
                TransactionState::RolledBack => RollbackStatus::FullyRolledBack,
                TransactionState::RollbackFailed => RollbackStatus::RollbackFailed,
                TransactionState::ApplyFailed | TransactionState::Applied => {
                    if transaction.rolled_back_count() > 0 {
                        RollbackStatus::PartiallyRolledBack
                    } else {
                        RollbackStatus::NotRequested
                    }
                }
                _ => RollbackStatus::NotRequested,
            },
            started_at_unix: None,
            ended_at_unix: None,
        }
    }
}

/// How a transaction ended with respect to rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RollbackStatus {
    /// Nothing was applied, or the user never asked to roll back.
    #[default]
    NotRequested,
    FullyRolledBack,
    PartiallyRolledBack,
    RollbackFailed,
}

impl RollbackStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotRequested => "Not requested",
            Self::FullyRolledBack => "Fully rolled back",
            Self::PartiallyRolledBack => "Partially rolled back",
            Self::RollbackFailed => "Rollback failed",
        }
    }
}

/// The outcome of one rollback pass. It must distinguish "everything was
/// reversed", "some was reversed and some was not", and "nothing could be
/// reversed", so a claim of full rollback is never made for a partial one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackResult {
    FullyRolledBack,
    PartiallyRolledBack {
        rolled_back: Vec<PathBuf>,
        failed: Vec<(PathBuf, String)>,
    },
    RollbackFailed {
        failed: Vec<(PathBuf, String)>,
    },
}

impl RollbackResult {
    pub fn rolled_back_paths(&self) -> Vec<&PathBuf> {
        match self {
            Self::FullyRolledBack => Vec::new(),
            Self::PartiallyRolledBack { rolled_back, .. } => rolled_back.iter().collect(),
            Self::RollbackFailed { .. } => Vec::new(),
        }
    }

    pub fn failed(&self) -> Vec<&(PathBuf, String)> {
        match self {
            Self::FullyRolledBack => Vec::new(),
            Self::PartiallyRolledBack { failed, .. } | Self::RollbackFailed { failed } => {
                failed.iter().collect()
            }
        }
    }
}
