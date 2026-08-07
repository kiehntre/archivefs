//! Rollback of applied rename transactions.
//!
//! Rollback reverses only entries the filesystem confirmed as Applied, in the
//! reverse order they were applied. Every step re-verifies that the destination
//! is still the recorded object and that the original source path is free,
//! renames back with the no-clobber primitive, and confirms the source is
//! restored before marking the entry RolledBack. A rollback that cannot meet
//! those checks stops and reports - it never claims a full rollback for a
//! partial one, and a repeated rollback request is a safe no-op for entries
//! already rolled back.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::identity::{capture_identity, identity_matches};
use super::journal::write_journal;
use super::model::{EntryState, RenameTransaction, RollbackResult, TransactionState};
use super::noclobber::{NoClobberError, rename_noreplace};

/// The outcome of a rollback pass (fully / partially / failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    pub result: RollbackResult,
    pub transaction: RenameTransaction,
}

/// Rolls back every Applied entry of `transaction`, in reverse order.
///
/// `journal_dir` is where the transaction's journal lives; it is updated
/// durably after every transition. Idempotent: entries already RolledBack (or
/// never Applied) are skipped, so calling this twice never renames anything
/// twice, and a transaction already RolledBack returns a no-op full result.
pub fn rollback_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
) -> Result<RollbackOutcome, String> {
    if transaction.state == TransactionState::RolledBack {
        return Ok(RollbackOutcome {
            result: RollbackResult::FullyRolledBack,
            transaction: transaction.clone(),
        });
    }

    transaction.state = TransactionState::RollingBack;
    write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;

    let mut rolled_back: Vec<std::path::PathBuf> = Vec::new();
    let mut failed: Vec<(std::path::PathBuf, String)> = Vec::new();

    // Reverse order of successful operations only.
    for index in (0..transaction.entries.len()).rev() {
        if cancelled(cancel) {
            break;
        }
        if !transaction.entries[index].is_eligible_for_rollback() {
            continue;
        }
        match rollback_one_entry(&mut transaction.entries[index]) {
            Ok(()) => {
                rolled_back.push(transaction.entries[index].source_path.clone());
            }
            Err(reason) => {
                transaction.entries[index].state = EntryState::RollbackFailed;
                transaction.entries[index].failure_reason = Some(reason.clone());
                failed.push((transaction.entries[index].source_path.clone(), reason));
                break;
            }
        }
        write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;
    }

    transaction.state = if failed.is_empty() {
        TransactionState::RolledBack
    } else {
        TransactionState::RollbackFailed
    };
    write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;

    let result = if failed.is_empty() {
        RollbackResult::FullyRolledBack
    } else if rolled_back.is_empty() {
        RollbackResult::RollbackFailed { failed }
    } else {
        RollbackResult::PartiallyRolledBack {
            rolled_back,
            failed,
        }
    };

    Ok(RollbackOutcome {
        result,
        transaction: transaction.clone(),
    })
}

/// Reverses one applied entry. Requires the destination to still be the
/// recorded object and the original source path to be free.
fn rollback_one_entry(entry: &mut super::model::TransactionEntry) -> Result<(), String> {
    // Destination must still exist and still be the recorded object.
    match capture_identity(&entry.destination_path) {
        Err(_) => {
            return Err(
                "rollback refused: the destination no longer exists (it was changed externally)"
                    .to_string(),
            );
        }
        Ok(current) if !identity_matches(&entry.identity, &current) => {
            return Err(
                "rollback refused: the destination is no longer the object that was renamed"
                    .to_string(),
            );
        }
        Ok(_) => {}
    }

    // Original source path must be free.
    if std::fs::symlink_metadata(&entry.source_path).is_ok() {
        return Err("rollback refused: the original source path is now occupied".to_string());
    }

    entry.state = EntryState::RollingBack;
    match rename_noreplace(&entry.destination_path, &entry.source_path) {
        Ok(()) => {
            // Confirm the source was restored with the recorded identity.
            match capture_identity(&entry.source_path) {
                Ok(current) if identity_matches(&entry.identity, &current) => {
                    entry.state = EntryState::RolledBack;
                    entry.rolled_back_at_unix = Some(crate::dat::sources::now_unix());
                    entry.failure_reason = None;
                    Ok(())
                }
                Ok(_) => Err(
                    "rollback renamed but the restored source identity does not match".to_string(),
                ),
                Err(_) => {
                    Err("rollback renamed but the restored source path is not readable".to_string())
                }
            }
        }
        Err(NoClobberError::DestinationExists) => {
            Err("rollback refused: the original source path is occupied (no-overwrite)".to_string())
        }
        Err(error) => Err(format!("rollback rename failed: {error}")),
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}
