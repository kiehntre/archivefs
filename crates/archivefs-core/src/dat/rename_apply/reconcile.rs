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
use super::model::{EntryState, RenameTransaction, TransactionEntry};

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

    if changed {
        write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;
    }
    Ok(issues)
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
        RenameTransaction {
            transaction_id: "reconcile-test".to_string(),
            plan_generation: 1,
            created_at_unix: 1,
            source_scan_root: "/tmp/roms".to_string(),
            state: super::super::model::TransactionState::Applying,
            entries: vec![entry],
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
}
