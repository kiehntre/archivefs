//! Filesystem reconciliation of in-flight transaction entries after a crash.
//!
//! A durable journal checkpoint marks an entry `Applying` (or, during rollback,
//! `RollingBack`) **before** the corresponding rename syscall. If the process
//! dies after the checkpoint but before the terminal state is persisted, the
//! journal cannot say whether the syscall ran. Recovery reconciles the entry
//! against the filesystem - read-only, never resuming the rename - and
//! classifies it:
//!
//! - only at the source (identity matches, destination absent): the rename (or
//!   reverse rename) did not happen;
//! - only at the destination (identity matches, source absent): the rename did
//!   happen and is filesystem-confirmed;
//! - both present, or both absent, or an identity mismatch: unsafe or unknown -
//!   the entry is left unresolved for manual review, never guessed.
//!
//! The reconciled state is persisted to the journal before it is exposed, so
//! `applied_count()` reflects reality and rollback can act on entries the
//! filesystem proved were renamed.

use std::path::Path;

use super::identity::{capture_identity, identity_matches};
use super::journal::write_journal;
use super::model::{EntryState, RenameTransaction, TransactionEntry, TransactionState};

/// Why an in-flight entry could not be cleanly classified, or what it was
/// reconciled to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryIssueKind {
    /// The rename (or reverse rename) did not happen; reconciled as not
    /// applied / rolled back.
    RenameDidNotHappen,
    /// The rename did happen; reconciled as filesystem-confirmed Applied (or
    /// RolledBack for an in-flight rollback).
    RenameConfirmed,
    /// Source and destination both exist. Unsafe; left unresolved.
    BothSourceAndDestination,
    /// Neither source nor destination exists. Unknown; left unresolved.
    BothAbsent,
    /// The destination exists but is not the recorded object. Unsafe external
    /// change; left unresolved.
    DestinationIdentityChanged,
    /// The source exists but is not the recorded object. Unsafe external
    /// change; left unresolved.
    SourceIdentityChanged,
}

impl RecoveryIssueKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::RenameDidNotHappen => "Rename did not happen",
            Self::RenameConfirmed => "Rename confirmed by the filesystem",
            Self::BothSourceAndDestination => "Source and destination both exist",
            Self::BothAbsent => "Neither source nor destination exists",
            Self::DestinationIdentityChanged => "Destination identity changed",
            Self::SourceIdentityChanged => "Source identity changed",
        }
    }
}

/// One reconciliation finding for one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryIssue {
    pub entry_index: usize,
    pub kind: RecoveryIssueKind,
    pub detail: String,
}

/// Reconciles every in-flight (`Applying` / `RollingBack`) entry of
/// `transaction` against the filesystem, persists the reconciled journal, and
/// returns the findings.
///
/// Read-only with respect to files (only `symlink_metadata`/`read`-style
/// identity capture); never resumes a rename. Entries that cannot be cleanly
/// classified are left unresolved so manual review is required.
pub fn reconcile_recovery(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
) -> Result<Vec<RecoveryIssue>, String> {
    let mut issues = Vec::new();
    let mut changed = false;

    for index in 0..transaction.entries.len() {
        let state = transaction.entries[index].state;
        if !matches!(state, EntryState::Applying | EntryState::RollingBack) {
            continue;
        }
        let issue = classify_entry(&transaction.entries[index], index);
        match issue.kind {
            RecoveryIssueKind::RenameDidNotHappen => {
                // Applying: the rename did not happen. RollingBack: the reverse
                // rename did happen (the file is back at source).
                if state == EntryState::Applying {
                    transaction.entries[index].state = EntryState::Skipped;
                } else {
                    transaction.entries[index].state = EntryState::RolledBack;
                    transaction.entries[index].rolled_back_at_unix =
                        Some(crate::dat::sources::now_unix());
                }
                changed = true;
            }
            RecoveryIssueKind::RenameConfirmed => {
                // Applying: the rename happened. RollingBack: it did not (the
                // file is still applied) - back to Applied so rollback can act.
                transaction.entries[index].state = EntryState::Applied;
                if state == EntryState::Applying {
                    transaction.entries[index].applied_at_unix =
                        Some(crate::dat::sources::now_unix());
                }
                changed = true;
            }
            _ => {
                // Unsafe or unknown: leave unresolved for manual review.
            }
        }
        issues.push(issue);
    }

    changed |= reconcile_transaction_level_state(transaction);

    if changed {
        write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;
    }
    Ok(issues)
}

/// Repairs a transaction-level `state` left stuck at `Applying` by a final
/// journal write that never landed, even though every entry's own (already
/// durable, and by this point already entry-reconciled) state proves the
/// batch actually finished.
///
/// [`super::executor::apply_transaction`] durably checkpoints `Applying`
/// before its per-entry loop, then - unconditionally, as the very next
/// statement once the loop ends with no failure - bumps `transaction.state`
/// to `Applied` and writes that. Every *failure* branch inside that loop
/// instead sets `transaction.state = ApplyFailed` and writes it in the same
/// synchronous step as the entry's own failed state, so a durably observed
/// entry failure is always paired with a durably observed `ApplyFailed` at
/// the transaction level - the two can never disagree. That leaves exactly
/// one way for `transaction.state` to still read `Applying` once every
/// entry has settled: the final unconditional write above ran and (for
/// whatever reason - a transient I/O failure) did not persist. This
/// reconstructs the outcome that final write was always going to record,
/// using the same "every entry settled cleanly" rule the executor itself
/// already relies on - never a new state machine, never a guess.
///
/// Fails closed: an entry left in any state other than `Applied`/`Skipped`
/// (including one this same call's entry-reconciliation pass could not
/// safely resolve, so it is still `Applying`/`RollingBack`) means the batch
/// cannot be proven complete, so `transaction.state` is left exactly as it
/// was for manual review - never guessed at, never promoted.
fn reconcile_transaction_level_state(transaction: &mut RenameTransaction) -> bool {
    if transaction.state != TransactionState::Applying {
        return false;
    }
    let every_entry_settled_clean = transaction
        .entries
        .iter()
        .all(|entry| matches!(entry.state, EntryState::Applied | EntryState::Skipped));
    if !every_entry_settled_clean {
        return false;
    }
    transaction.state = TransactionState::Applied;
    true
}

/// Classifies one in-flight entry against the live filesystem.
fn classify_entry(entry: &TransactionEntry, index: usize) -> RecoveryIssue {
    let source_present = std::fs::symlink_metadata(&entry.source_path).is_ok();
    let destination_present = std::fs::symlink_metadata(&entry.destination_path).is_ok();
    let source_matches = source_present
        .then(|| capture_identity(&entry.source_path).ok())
        .flatten()
        .is_some_and(|identity| identity_matches(&entry.identity, &identity));
    let destination_matches = destination_present
        .then(|| capture_identity(&entry.destination_path).ok())
        .flatten()
        .is_some_and(|identity| identity_matches(&entry.identity, &identity));

    if destination_present && !destination_matches {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::DestinationIdentityChanged,
            detail: "the destination exists but is not the recorded object; manual review is \
                     required"
                .to_string(),
        }
    } else if source_present && !source_matches {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::SourceIdentityChanged,
            detail: "the source exists but is not the recorded object; manual review is required"
                .to_string(),
        }
    } else if source_matches && !destination_present {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::RenameDidNotHappen,
            detail: "the source is intact and the destination is absent; no rename happened"
                .to_string(),
        }
    } else if !source_present && destination_matches {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::RenameConfirmed,
            detail: "the source is gone and the destination matches the recorded identity; the \
                     rename happened"
                .to_string(),
        }
    } else if source_present && destination_present {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::BothSourceAndDestination,
            detail: "source and destination both exist; refusing to guess which is intended"
                .to_string(),
        }
    } else {
        RecoveryIssue {
            entry_index: index,
            kind: RecoveryIssueKind::BothAbsent,
            detail: "neither the source nor the destination exists; manual review is required"
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: &Path, destination: &Path, state: EntryState) -> TransactionEntry {
        TransactionEntry {
            source_path: source.to_path_buf(),
            destination_path: destination.to_path_buf(),
            original_basename: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            proposed_basename: destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            identity: capture_identity(source).unwrap(),
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }
    }

    fn transaction_with(entry: TransactionEntry) -> RenameTransaction {
        transaction_with_entries(vec![entry])
    }

    fn transaction_with_entries(entries: Vec<TransactionEntry>) -> RenameTransaction {
        RenameTransaction {
            transaction_id: "reconcile-test".to_string(),
            plan_generation: 1,
            classifier_version: Some(crate::dat::classification::CLASSIFIER_VERSION.to_string()),
            created_at_unix: 1,
            source_scan_root: "/tmp/roms".to_string(),
            state: super::super::model::TransactionState::Applying,
            entries,
            created_directories: Vec::new(),
            unknown: Default::default(),
        }
    }

    #[test]
    fn a_source_only_applying_entry_is_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::Applying));
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Skipped);
        assert_eq!(issues[0].kind, RecoveryIssueKind::RenameDidNotHappen);
    }

    #[test]
    fn a_destination_only_applying_entry_is_confirmed_applied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::Applying));
        // Simulate the rename having happened, with no journal update.
        std::fs::rename(&source, &destination).unwrap();
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Applied);
        assert_eq!(issues[0].kind, RecoveryIssueKind::RenameConfirmed);
    }

    #[test]
    fn a_rolling_back_entry_with_source_restored_is_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::RollingBack));
        // Reverse rename already happened; journal still says RollingBack.
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::RolledBack);
        assert_eq!(issues[0].kind, RecoveryIssueKind::RenameDidNotHappen);
    }

    #[test]
    fn a_rolling_back_entry_still_at_destination_is_back_to_applied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::RollingBack));
        std::fs::rename(&source, &destination).unwrap();
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Applied);
        assert_eq!(issues[0].kind, RecoveryIssueKind::RenameConfirmed);
    }

    #[test]
    fn both_present_is_an_unresolved_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        // A hard link shares the inode, so both paths exist with matching
        // identity - an indeterminate state reconciliation must not resolve.
        std::fs::hard_link(&source, &destination).unwrap();
        let mut tx = transaction_with(entry(&source, &destination, EntryState::Applying));
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Applying, "left unresolved");
        assert_eq!(issues[0].kind, RecoveryIssueKind::BothSourceAndDestination);
    }

    #[test]
    fn both_absent_is_an_unresolved_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("gone.bin");
        let destination = dir.path().join("gone2.bin");
        // Build identity from a temporary stand-in so the entry is well-formed;
        // neither the source nor the destination path ever exists.
        let stand_in = dir.path().join("standin.bin");
        std::fs::write(&stand_in, b"data").unwrap();
        let mut entry = entry(&stand_in, &destination, EntryState::Applying);
        entry.identity = capture_identity(&stand_in).unwrap();
        entry.source_path = source;
        let mut tx = transaction_with(entry);
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Applying, "left unresolved");
        assert_eq!(issues[0].kind, RecoveryIssueKind::BothAbsent);
    }

    #[test]
    fn destination_identity_change_is_unresolved_and_manual() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::Applying));
        std::fs::rename(&source, &destination).unwrap();
        // Replace the destination with a different object.
        std::fs::remove_file(&destination).unwrap();
        std::fs::write(&destination, b"replaced").unwrap();
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let issues = reconcile_recovery(&mut tx, &journal).unwrap();
        assert_eq!(tx.entries[0].state, EntryState::Applying, "left unresolved");
        assert_eq!(
            issues[0].kind,
            RecoveryIssueKind::DestinationIdentityChanged
        );
    }

    // -------------------------------------------------------------------
    // Transaction-level reconciliation: a `transaction.state` stuck at
    // `Applying` after every entry has already durably settled (the final
    // journal write that would have bumped it to `Applied` never landed).
    // -------------------------------------------------------------------

    /// A journal whose transaction-level state is `Applying` but whose sole
    /// entry is already durably `Applied` must be reconciled to
    /// `Applied` - and that correction must itself be persisted to disk,
    /// not just held in memory.
    #[test]
    fn a_transaction_stuck_applying_with_an_applied_entry_is_reconciled_to_applied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        let destination = dir.path().join("b.bin");
        // A placeholder so the `entry()` builder's own identity capture
        // succeeds; overwritten below with the destination's real identity,
        // exactly as `both_absent_is_an_unresolved_unknown` already does for
        // the same reason.
        std::fs::write(&source, b"placeholder").unwrap();
        std::fs::write(&destination, b"data").unwrap();
        let mut applied_entry = entry(&source, &destination, EntryState::Applied);
        // The entry's own recorded identity must match the (already renamed)
        // destination for it to be a genuinely durable `Applied` entry, not
        // just a label - `capture_identity` was taken from `source` before
        // it existed, so retake it from the real, already-renamed object.
        applied_entry.identity = capture_identity(&destination).unwrap();
        let mut tx = transaction_with(applied_entry);
        assert_eq!(tx.state, TransactionState::Applying);

        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        write_journal(&journal, &tx).unwrap();

        let issues = reconcile_recovery(&mut tx, &journal).unwrap();

        assert_eq!(tx.state, TransactionState::Applied);
        // The entry itself needed no reconciliation (it was never
        // `Applying`/`RollingBack`), so there is nothing to report about it.
        assert!(issues.is_empty());

        // The correction is durable, not just an in-memory patch.
        let path = super::super::journal::journal_path(&journal, &tx.transaction_id).unwrap();
        let reread = super::super::journal::read_journal(&path).unwrap();
        assert_eq!(reread.state, TransactionState::Applied);
    }

    /// The same shape, but with a `Skipped` entry alongside the `Applied`
    /// one (a `SkipUnsafeSubset` batch) - `Skipped` is just as settled as
    /// `Applied`, and must not block the transaction-level correction.
    #[test]
    fn a_transaction_stuck_applying_with_applied_and_skipped_entries_is_reconciled_to_applied() {
        let dir = tempfile::tempdir().unwrap();
        let applied_source = dir.path().join("a.bin");
        let applied_destination = dir.path().join("a-renamed.bin");
        std::fs::write(&applied_source, b"placeholder").unwrap();
        std::fs::write(&applied_destination, b"data").unwrap();
        let mut applied_entry = entry(&applied_source, &applied_destination, EntryState::Applied);
        applied_entry.identity = capture_identity(&applied_destination).unwrap();

        let skipped_source = dir.path().join("c.bin");
        std::fs::write(&skipped_source, b"never touched").unwrap();
        let skipped_destination = dir.path().join("c-renamed.bin");
        let mut skipped_entry = entry(&skipped_source, &skipped_destination, EntryState::Skipped);
        skipped_entry.identity = capture_identity(&skipped_source).unwrap();

        let mut tx = transaction_with_entries(vec![applied_entry, skipped_entry]);
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();

        reconcile_recovery(&mut tx, &journal).unwrap();

        assert_eq!(tx.state, TransactionState::Applied);
    }

    /// Fail closed: a transaction whose overall state is `Applying` but
    /// which still has a genuinely unresolvable in-flight entry (this
    /// reconciliation pass could not prove what happened to it) must never
    /// be promoted to `Applied` - the batch cannot be proven complete.
    #[test]
    fn a_transaction_with_a_genuinely_unresolved_entry_is_not_promoted_to_applied() {
        let dir = tempfile::tempdir().unwrap();

        let applied_source = dir.path().join("a.bin");
        let applied_destination = dir.path().join("a-renamed.bin");
        std::fs::write(&applied_source, b"placeholder").unwrap();
        std::fs::write(&applied_destination, b"data").unwrap();
        let mut applied_entry = entry(&applied_source, &applied_destination, EntryState::Applied);
        applied_entry.identity = capture_identity(&applied_destination).unwrap();

        // A hard-linked source+destination pair is the same unresolvable
        // "both present, matching identity" shape the existing
        // `both_present_is_an_unresolved_conflict` test already proves stays
        // `Applying` at the entry level.
        let unresolved_source = dir.path().join("d.bin");
        std::fs::write(&unresolved_source, b"ambiguous").unwrap();
        let unresolved_destination = dir.path().join("d-renamed.bin");
        std::fs::hard_link(&unresolved_source, &unresolved_destination).unwrap();
        let unresolved_entry = entry(
            &unresolved_source,
            &unresolved_destination,
            EntryState::Applying,
        );

        let mut tx = transaction_with_entries(vec![applied_entry, unresolved_entry]);
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();

        let issues = reconcile_recovery(&mut tx, &journal).unwrap();

        assert_eq!(
            tx.entries[1].state,
            EntryState::Applying,
            "the ambiguous entry must stay unresolved"
        );
        assert_eq!(issues[0].kind, RecoveryIssueKind::BothSourceAndDestination);
        assert_eq!(
            tx.state,
            TransactionState::Applying,
            "the batch cannot be proven complete while one entry is still unresolved, so the \
             transaction-level state must never be promoted"
        );
    }

    /// A reconciled `Applied` transaction is exactly as rollbackable as one
    /// that reached `Applied` through the normal apply path - the
    /// reconciliation must never leave it in a state the rest of the system
    /// treats differently.
    #[test]
    fn a_reconciled_transaction_remains_rollbackable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        let destination = dir.path().join("b.bin");
        std::fs::write(&source, b"placeholder").unwrap();
        std::fs::write(&destination, b"data").unwrap();
        let mut applied_entry = entry(&source, &destination, EntryState::Applied);
        applied_entry.identity = capture_identity(&destination).unwrap();
        let mut tx = transaction_with(applied_entry);

        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        reconcile_recovery(&mut tx, &journal).unwrap();

        assert_eq!(tx.state, TransactionState::Applied);
        assert!(
            tx.is_rollbackable(),
            "a reconciled Applied transaction with an applied entry must still be rollbackable"
        );
    }

    /// Existing `RollingBack` recovery semantics are unchanged: a
    /// transaction whose overall state is `RollingBack` (never `Applying`)
    /// must never have its transaction-level state touched by the new
    /// reconciliation step, even after its entries settle.
    #[test]
    fn a_rolling_back_transaction_level_state_is_never_touched_by_the_new_step() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let mut tx = transaction_with(entry(&source, &destination, EntryState::RollingBack));
        tx.state = super::super::model::TransactionState::RollingBack;
        // Reverse rename already happened; journal still says RollingBack.
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();

        reconcile_recovery(&mut tx, &journal).unwrap();

        assert_eq!(tx.entries[0].state, EntryState::RolledBack);
        assert_eq!(
            tx.state,
            TransactionState::RollingBack,
            "only the entry settles here; the transaction-level state is a separate concern the \
             rollback executor itself owns, and the new Applying-only reconciliation step must \
             never touch a RollingBack transaction"
        );
    }
}
