//! Safe, gated application of explicitly approved rename plans, with durable
//! transaction journaling and rollback.
//!
//! This is the apply side of the read-only rename planning (PR #14). It may
//! rename a file **only** when every gate holds:
//!
//! - the proposal comes from the current validated rename plan generation;
//! - the proposal is `Suggested`, actionable, collision-free, and explicitly
//!   approved by the user;
//! - at apply time the source is still the very same regular file that was
//!   reviewed (same size and, where supported, same inode/device; never a
//!   symlink);
//! - the destination does not exist, shares the source's directory, and lies
//!   inside the configured trusted roots;
//! - every preflight check passes immediately before the rename.
//!
//! # The hard safety boundary
//!
//! The executor is the **only** place renames happen; the GUI never calls
//! `std::fs::rename`. Mutations use a no-clobber primitive
//! (`renameat2(RENAME_NOREPLACE)` on Linux) so an existing destination is
//! never overwritten, there is no copy+delete fallback, and a destination that
//! appears between preflight and rename is refused atomically. A batch is
//! journaled durably before the first mutation and updated after every
//! transition, so a crash leaves a recoverable record. Nothing here runs
//! unattended, retries a failed mutation, or resumes automatically.
//!
//! # Unsupported cases
//!
//! Symlink sources, broken symlinks, directories, archive members, cross-
//! directory or cross-filesystem moves, overwrites, and any rename outside the
//! trusted roots are never performed. On platforms without a verified
//! no-clobber primitive (everything but Linux in this build) the executor
//! refuses to mutate rather than risk a TOCTOU-prone exists+rename sequence.

pub mod executor;
pub mod identity;
pub mod journal;
pub mod model;
pub mod noclobber;
pub mod preflight;
pub mod rollback;

pub use executor::{
    ApplyError, ApplyExecution, ApplyOutcome, HardConflictMode, apply_transaction,
    build_transaction, build_transaction_entries, is_approved,
};
pub use identity::{capture_identity, classify_at, identity_matches};
pub use journal::{
    RENAME_TRANSACTIONS_DIRECTORY, default_rename_transaction_dir, find_recovery_transactions,
    journal_exists, journal_path, list_journals, new_transaction_id, read_journal, remove_journal,
    rename_transaction_dir_in, write_journal,
};
pub use model::{
    EntryState, ObjectIdentity, ObjectKind, RenameTransaction, RollbackResult, RollbackStatus,
    TransactionEntry, TransactionState, TransactionSummary,
};
pub use noclobber::{NoClobberError, rename_noreplace};
pub use preflight::{PreflightFailure, PreflightOptions, batch_destinations, run_preflight};
pub use rollback::{RollbackOutcome, rollback_transaction};

#[cfg(test)]
mod tests;
