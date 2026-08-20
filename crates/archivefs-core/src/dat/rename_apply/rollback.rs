//! Rollback of applied rename transactions.
//!
//! Rollback reverses only entries the filesystem confirmed as Applied, in the
//! reverse order they were applied. Every step re-verifies that the destination
//! is still the recorded object and that the original source path is free,
//! renames back with the no-clobber primitive, and confirms the source is
//! restored before marking the entry RolledBack. A rollback that cannot meet
//! those checks stops and reports - it never claims a full rollback for a
//! partial one, and a repeated rollback request is a safe no-op for entries
//! already rolled back. Cancellation stops the loop with any not-yet-reversed
//! Applied entries left untouched (and retryable); it is an incomplete
//! rollback, never reported as a full one.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::identity::{capture_identity, identity_matches};
use super::journal::write_journal;
use super::model::{EntryState, RenameTransaction, RollbackResult, TransactionState};
use super::noclobber::{NoClobberError, rename_noreplace};
use crate::safe_read::TrustedRoots;

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
///
/// Cancellation stops the loop without marking the untouched Applied entries as
/// failed, so they stay eligible for a later rollback. Because the rollback is
/// then incomplete, the result is never `FullyRolledBack` and the transaction
/// is not persisted as `RolledBack`; the leftover entries are reported with a
/// "rollback cancelled" reason.
pub fn rollback_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
) -> Result<RollbackOutcome, String> {
    rollback_transaction_inner(transaction, journal_dir, cancel, None)
}

/// Identical to [`rollback_transaction`], but additionally re-verifies, for
/// every reverse-rename mutation, that the entire ancestor chain of both the
/// current (destination) and restored (source) paths, not just the leaf, is
/// still plain directories confined to `trusted`, immediately before the
/// rename syscall. This closes a gap ordinary `rollback_transaction` does
/// not cover: an intermediate ancestor directory silently replaced with a
/// symlink (e.g. `source_parent` itself) is transparent to normal path
/// resolution, so an unconfined rollback would follow it like any other
/// directory. Used by [`crate::platform_evidence_fusion::plan_transaction`],
/// which already threads a [`TrustedRoots`] through the forward apply path
/// and is the entry point a future real-canary apply will use.
pub fn rollback_transaction_confined(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
    trusted: &TrustedRoots,
) -> Result<RollbackOutcome, String> {
    rollback_transaction_inner(transaction, journal_dir, cancel, Some(trusted))
}

fn rollback_transaction_inner(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
    trusted: Option<&TrustedRoots>,
) -> Result<RollbackOutcome, String> {
    if transaction.state == TransactionState::RolledBack {
        return Ok(RollbackOutcome {
            result: RollbackResult::FullyRolledBack,
            transaction: transaction.clone(),
        });
    }

    // Reconcile any in-flight entries first: a crash can have left an entry
    // `Applying` (rename may or may not have run) or `RollingBack` (reverse
    // rename may or may not have run). The filesystem decides which. An entry
    // that cannot be cleanly classified stays unresolved and blocks the
    // rollback from claiming completion.
    let issues = super::reconcile::reconcile_recovery(transaction, journal_dir)
        .map_err(|error| error.to_string())?;
    let unresolved = issues
        .iter()
        .filter(|issue| {
            !matches!(
                issue.kind,
                super::reconcile::RecoveryIssueKind::RenameDidNotHappen
                    | super::reconcile::RecoveryIssueKind::RenameConfirmed
            )
        })
        .count();
    if unresolved > 0 {
        let detail = issues
            .iter()
            .filter(|issue| {
                matches!(
                    issue.kind,
                    super::reconcile::RecoveryIssueKind::BothSourceAndDestination
                        | super::reconcile::RecoveryIssueKind::BothAbsent
                        | super::reconcile::RecoveryIssueKind::DestinationIdentityChanged
                        | super::reconcile::RecoveryIssueKind::SourceIdentityChanged
                )
            })
            .map(|issue| format!("{}: {}", issue.kind.label(), issue.detail))
            .collect::<Vec<_>>()
            .join("; ");
        transaction.state = TransactionState::RollbackFailed;
        write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;
        return Ok(RollbackOutcome {
            result: RollbackResult::RollbackFailed {
                failed: vec![(
                    transaction
                        .entries
                        .first()
                        .map(|e| e.source_path.clone())
                        .unwrap_or_default(),
                    format!("manual review required: {detail}"),
                )],
            },
            transaction: transaction.clone(),
        });
    }

    transaction.state = TransactionState::RollingBack;
    write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;

    let mut rolled_back: Vec<std::path::PathBuf> = Vec::new();
    let mut failed: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut stopped_by_cancellation = false;

    // Reverse order of successful operations only.
    for index in (0..transaction.entries.len()).rev() {
        if cancelled(cancel) {
            stopped_by_cancellation = true;
            break;
        }
        if !transaction.entries[index].is_eligible_for_rollback() {
            continue;
        }

        // Durable `RollingBack` checkpoint BEFORE the reverse rename syscall.
        transaction.entries[index].state = EntryState::RollingBack;
        write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;

        match rollback_mutation(&transaction.entries[index], trusted) {
            Ok(()) => {
                transaction.entries[index].state = EntryState::RolledBack;
                transaction.entries[index].rolled_back_at_unix =
                    Some(crate::dat::sources::now_unix());
                transaction.entries[index].failure_reason = None;
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

    // A rollback is complete only when no entry is left Applied or in-flight
    // (`RollingBack`; unresolved recovery states already blocked the pass
    // above). Cancellation leaves the untouched entries Applied rather than
    // marking them failed, so a later retry can finish them - and it must never
    // be read as a full rollback.
    let remaining_incomplete: Vec<std::path::PathBuf> = transaction
        .entries
        .iter()
        .filter(|entry| matches!(entry.state, EntryState::Applied | EntryState::RollingBack))
        .map(|entry| entry.source_path.clone())
        .collect();

    let fully_rolled_back = failed.is_empty() && remaining_incomplete.is_empty();

    transaction.state = if fully_rolled_back {
        TransactionState::RolledBack
    } else {
        TransactionState::RollbackFailed
    };
    write_journal(journal_dir, transaction).map_err(|error| error.to_string())?;

    let result = if fully_rolled_back {
        RollbackResult::FullyRolledBack
    } else if stopped_by_cancellation {
        let cancelled_remaining: Vec<(std::path::PathBuf, String)> = remaining_incomplete
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    "rollback cancelled while entries were still applied".to_string(),
                )
            })
            .collect();
        if rolled_back.is_empty() {
            RollbackResult::RollbackFailed {
                failed: cancelled_remaining,
            }
        } else {
            RollbackResult::PartiallyRolledBack {
                rolled_back,
                failed: cancelled_remaining,
            }
        }
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

/// The reverse-rename mutation window for one applied entry. Requires the
/// destination to still be the recorded object and the original source path
/// to be free. Called only after the entry's `RollingBack` state has been
/// durably persisted.
fn rollback_mutation(
    entry: &super::model::TransactionEntry,
    trusted: Option<&TrustedRoots>,
) -> Result<(), String> {
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

    // Ancestor-directory containment, immediately before the mutation. The
    // leaf-only check above (`symlink_metadata` on the source path itself)
    // cannot see an *intermediate* ancestor - the source or destination
    // parent, grandparent, or deeper - that has been replaced with a
    // symlink: ordinary path resolution follows it transparently, exactly
    // as `cd`/`open` would. `ancestor_chain_is_confined` walks every
    // ancestor component up to the trusted root and requires each one to
    // still be a plain directory (never a symlink), regardless of where a
    // substituted symlink's target points - inside the trusted root or
    // outside it makes no difference, since the caller's expected canonical
    // location was replaced either way. Checked right here, immediately
    // before the rename syscall below, so there is no earlier-check/later-
    // mutation gap for a race to exploit.
    if let Some(trusted) = trusted
        && !trusted.is_empty()
    {
        let destination_confined = entry
            .destination_path
            .parent()
            .is_some_and(|parent| ancestor_chain_is_confined(parent, trusted));
        let source_confined = entry
            .source_path
            .parent()
            .is_some_and(|parent| ancestor_chain_is_confined(parent, trusted));
        if !destination_confined || !source_confined {
            return Err(
                "rollback refused: an ancestor directory has been replaced (e.g. with a \
                 symlink) and is no longer a plain directory confined to the trusted root - \
                 manual recovery required"
                    .to_string(),
            );
        }
    }

    match rename_noreplace(&entry.destination_path, &entry.source_path) {
        Ok(()) => {
            // Confirm the source was restored with the recorded identity.
            match capture_identity(&entry.source_path) {
                Ok(current) if identity_matches(&entry.identity, &current) => Ok(()),
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

/// Whether every ancestor of `path`, from `path` itself up to and including
/// whichever trusted root contains it, is a plain directory - never a
/// symlink. Uses `symlink_metadata` (never follows the final component of
/// each ancestor), so a substituted symlink is caught by its own file type
/// rather than by comparing where it happens to resolve to; this is why an
/// inside-root and an outside-root substitution are refused identically.
/// `path` must be literally contained in one of `trusted`'s roots (a
/// `starts_with` prefix match on the recorded, non-canonical path) or this
/// returns `false` immediately - a path outside every trusted root was
/// never safe to rename into/out of in the first place.
fn ancestor_chain_is_confined(path: &Path, trusted: &TrustedRoots) -> bool {
    let Some(root) = trusted
        .roots()
        .iter()
        .find(|root| path.starts_with(root.as_path()))
    else {
        return false;
    };
    let mut current = path;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            _ => return false,
        }
        if current == root.as_path() {
            return true;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}
