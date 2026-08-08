//! Building, applying and rolling back organisation transactions.
//!
//! An organisation transaction is a [`RenameTransaction`] whose entries move
//! files (or symlink objects) from their current location to a canonical
//! destination, exactly like a DAT rename - the engine, journal, no-clobber
//! primitive, identity capture, reconcile and rollback are the shared
//! `rename_apply` ones. What is organisation-specific here is thin:
//!
//! - platform **directory creation** under the master ROM root, recorded
//!   durably before it happens and never removing a pre-existing user
//!   directory on rollback;
//! - the **same-filesystem** directory policy (a move into another directory
//!   on the same device) and, for symlink-only mode, permission to move a
//!   symlink *object* without ever dereferencing its target;
//! - honest reporting of partial apply and of rollback directory cleanup.
//!
//! No copy+delete fallback exists: if the destination is on a different
//! filesystem the mutation is refused before anything happens.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::safe_read::TrustedRoots;

use crate::dat::rename_apply::executor::{
    ApplyError, ApplyExecution, ApplyOutcome, apply_transaction,
};
use crate::dat::rename_apply::identity::capture_identity;
use crate::dat::rename_apply::journal::write_journal;
use crate::dat::rename_apply::model::{
    EntryState, RenameTransaction, TransactionEntry, TransactionState,
};
use crate::dat::rename_apply::preflight::DirectoryPolicy;
use crate::dat::rename_apply::rollback::{RollbackOutcome, rollback_transaction};

use crate::dat::rom_organisation::model::{OrganisationMode, OrganisationPlan, OrganisationStatus};

/// Builds the transaction for the approved Suggested entries of a plan.
///
/// Read-only and side-effect free: it captures each source's identity and
/// plans which canonical platform directories may need to be created, but
/// writes no journal and mutates nothing.
pub fn build_organisation_transaction(
    plan: &OrganisationPlan,
    approved_sources: &BTreeSet<String>,
    generation: u64,
) -> Result<RenameTransaction, String> {
    if generation != plan.generation {
        return Err(format!(
            "the organisation plan is stale (generation {}; current {generation}); regenerate it",
            plan.generation
        ));
    }
    let mut entries = Vec::new();
    let mut created_directories = Vec::new();
    for entry in plan
        .entries
        .iter()
        .filter(|entry| entry.status == OrganisationStatus::Suggested)
    {
        if !approved_sources.contains(&entry.source_path.to_string_lossy().into_owned()) {
            continue;
        }
        if entry.destination_path.as_os_str().is_empty() {
            continue;
        }
        let Ok(identity) = capture_identity(&entry.source_path) else {
            // The source vanished between planning and building; the entry
            // cannot carry a recorded identity, so it is excluded.
            continue;
        };
        let original_basename = entry
            .source_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(proposed_basename) = entry.destination_path.file_name() else {
            continue;
        };
        entries.push(TransactionEntry {
            source_path: entry.source_path.clone(),
            destination_path: entry.destination_path.clone(),
            original_basename,
            proposed_basename: proposed_basename.to_string_lossy().into_owned(),
            identity,
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        });
        if entry.mode != OrganisationMode::RenameInPlace
            && let Some(parent) = entry.destination_path.parent()
            && is_platform_directory_candidate(parent, &plan.master_root)
            && !created_directories.contains(&parent.to_path_buf())
        {
            created_directories.push(parent.to_path_buf());
        }
    }
    if entries.is_empty() {
        return Err("no approved Suggested entries in the plan".to_string());
    }
    Ok(RenameTransaction {
        transaction_id: crate::dat::rename_apply::journal::new_transaction_id(
            crate::dat::sources::now_unix(),
        ),
        plan_generation: plan.generation,
        created_at_unix: crate::dat::sources::now_unix(),
        source_scan_root: plan.master_root.to_string_lossy().into_owned(),
        state: TransactionState::Planned,
        entries,
        created_directories,
        unknown: Default::default(),
    })
}

/// Whether `parent` is a plausible canonical platform directory this
/// transaction may create: exactly one safe component directly under the
/// configured master ROM root.
fn is_platform_directory_candidate(parent: &Path, master_root: &Path) -> bool {
    use crate::dat::rename_apply::preflight::is_safe_basename;
    let Some(name) = parent.file_name().map(|n| n.to_string_lossy()) else {
        return false;
    };
    if !is_safe_basename(&name) {
        return false;
    }
    parent.parent().is_some_and(|root| root == master_root)
}

/// Applies an organisation transaction.
///
/// Ordering is the same as rename-apply: durable journal of intent first,
/// then the platform directories this transaction owns, then the shared
/// executor's per-entry Applying checkpoint + no-clobber move. A cancellation
/// before any file mutation leaves zero file mutations.
pub fn apply_organisation_transaction(
    transaction: &mut RenameTransaction,
    approved_sources: &BTreeSet<String>,
    generation: u64,
    trusted: TrustedRoots,
    journal_dir: &Path,
    cancel: &AtomicBool,
    mode: OrganisationMode,
) -> Result<ApplyOutcome, ApplyError> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(ApplyError::Cancelled);
    }

    // Record durable intent before creating any directory or moving anything.
    transaction.state = TransactionState::Applying;
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    // Create only the platform directories that do not already exist. A
    // directory that exists is a pre-existing user directory and is never
    // recorded as ours (so rollback never removes it).
    let planned: Vec<std::path::PathBuf> = transaction.created_directories.clone();
    let mut actually_created: Vec<std::path::PathBuf> = Vec::new();
    for directory in &planned {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match std::fs::symlink_metadata(directory) {
            Ok(_) => {
                // Appeared (or pre-existed); never ours.
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(directory) {
                    Ok(()) => actually_created.push(directory.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Appeared concurrently; pre-existing, not ours.
                    }
                    Err(error) => {
                        transaction.created_directories = actually_created;
                        transaction.state = TransactionState::ApplyFailed;
                        write_journal(journal_dir, transaction).map_err(|journal_error| {
                            ApplyError::Journal(journal_error.to_string())
                        })?;
                        return Err(ApplyError::Journal(format!(
                            "could not create platform directory {}: {error}",
                            directory.display()
                        )));
                    }
                }
            }
            Err(_) => {
                transaction.created_directories = actually_created;
                transaction.state = TransactionState::ApplyFailed;
                write_journal(journal_dir, transaction)
                    .map_err(|journal_error| ApplyError::Journal(journal_error.to_string()))?;
                return Err(ApplyError::Journal(format!(
                    "could not inspect platform directory {}",
                    directory.display()
                )));
            }
        }
    }
    if actually_created.len() != planned.len() {
        transaction.created_directories = actually_created;
        write_journal(journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
    }

    // Batch preflight immediately before the shared executor runs, so a
    // pre-existing conflict (identity changed, destination appeared, stale
    // generation) is rejected cleanly: the freshly created platform
    // directories are removed again and the failure is journaled, leaving no
    // orphaned mutation. The shared executor re-preflights a moment later for
    // the race window; any directories it would then leave are recorded in
    // the journal and recoverable.
    let destinations =
        crate::dat::rename_apply::preflight::batch_destinations(&transaction.entries);
    let preflight_options = crate::dat::rename_apply::preflight::PreflightOptions {
        plan_generation: transaction.plan_generation,
        current_generation: generation,
        approved_paths: approved_sources,
        trusted: &trusted,
        batch_destinations: &destinations,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source: mode == OrganisationMode::OrganiseSymlinkOnly,
    };
    let mut hard_conflicts: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
    for entry in &mut transaction.entries {
        if let Err(failures) =
            crate::dat::rename_apply::preflight::run_preflight(entry, &preflight_options)
        {
            let reasons: Vec<String> = failures.iter().map(|f| f.reason()).collect();
            entry.preflight_failures = reasons.clone();
            entry.preflight_passed = false;
            hard_conflicts.push((entry.source_path.clone(), reasons));
        }
    }
    if !hard_conflicts.is_empty() {
        // Clean up only the directories this call created (all empty) and
        // journal the failure so recovery does not resurface them.
        let created = transaction.created_directories.clone();
        for directory in created.iter().rev() {
            let _ = std::fs::remove_dir(directory);
        }
        transaction.created_directories = Vec::new();
        transaction.state = TransactionState::ApplyFailed;
        write_journal(journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
        return Err(ApplyError::HardConflicts(hard_conflicts));
    }

    // The shared executor runs the same preflight (trusted roots, safe
    // basenames, destination-not-exists, case-fold, batch collisions, stale
    // generation, identity re-check), the same Applying checkpoint and the
    // same no-clobber move.
    apply_transaction(&mut ApplyExecution {
        transaction,
        approved_paths: approved_sources.clone(),
        current_generation: generation,
        trusted,
        journal_dir: journal_dir.to_path_buf(),
        hard_conflict_mode: crate::dat::rename_apply::executor::HardConflictMode::AbortAll,
        cancel,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source: mode == OrganisationMode::OrganiseSymlinkOnly,
    })
}

/// The outcome of rolling back an organisation transaction: the shared
/// rollback of the moved entries, plus which platform directories this
/// transaction created were removed and which could not be removed (they
/// still exist, possibly because they are no longer empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganisationRollbackOutcome {
    pub rollback: RollbackOutcome,
    /// Directories this transaction created that were removed (still empty).
    pub directories_removed: Vec<std::path::PathBuf>,
    /// Directories this transaction created that remain (not empty, or not
    /// removable). Never a pre-existing user directory.
    pub directories_remaining: Vec<std::path::PathBuf>,
}

/// Rolls back an organisation transaction: the entry moves via the shared
/// rollback engine, then any platform directories this transaction created
/// that are now empty. A pre-existing user directory is never removed, and a
/// directory is only removed when it is empty and sits exactly one level
/// beneath the configured master ROM root.
pub fn rollback_organisation_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
    master_root: &Path,
) -> Result<OrganisationRollbackOutcome, String> {
    let rollback = rollback_transaction(transaction, journal_dir, cancel)?;

    let mut directories_removed = Vec::new();
    let mut directories_remaining = Vec::new();
    for directory in transaction.created_directories.iter().rev() {
        if !is_owned_platform_directory(directory, master_root) {
            // Defensive: never remove a directory that is not exactly one safe
            // component beneath the master root.
            directories_remaining.push(directory.clone());
            continue;
        }
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() => {
                let is_empty = std::fs::read_dir(directory)
                    .map(|mut read_dir| read_dir.next().is_none())
                    .unwrap_or(false);
                if is_empty {
                    if std::fs::remove_dir(directory).is_ok() {
                        directories_removed.push(directory.clone());
                    } else {
                        directories_remaining.push(directory.clone());
                    }
                } else {
                    directories_remaining.push(directory.clone());
                }
            }
            // Missing (never created, or already gone): nothing to clean.
            _ => {}
        }
    }
    Ok(OrganisationRollbackOutcome {
        rollback,
        directories_removed,
        directories_remaining,
    })
}

/// A directory ArchiveFS may remove on rollback: exactly one safe component
/// directly beneath the master ROM root.
fn is_owned_platform_directory(directory: &Path, master_root: &Path) -> bool {
    use crate::dat::rename_apply::preflight::is_safe_basename;
    let Some(name) = directory.file_name().map(|n| n.to_string_lossy()) else {
        return false;
    };
    is_safe_basename(&name) && directory.parent().is_some_and(|root| root == master_root)
}
