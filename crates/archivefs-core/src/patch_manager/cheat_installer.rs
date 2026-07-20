//! Write-capable RetroArch cheat installer - the first milestone in this
//! codebase that actually creates a directory, writes a file, creates a
//! backup, or writes a journal for a RetroArch cheat.
//!
//! This module reuses, and never duplicates:
//!
//! - [`super::cheat_catalogue`]'s staging preview
//!   ([`CheatAvailabilityEntry`]/its `staging_plan`) for eligibility and
//!   the originally proposed destination - `retroarch-cheat-catalogue`
//!   itself remains strictly read-only; nothing there changed to add this
//!   module.
//! - [`super::destination_safety`]'s read-only, symlink-aware path
//!   validation ([`assess_destination`]) for every revalidation this
//!   module performs immediately before a write - that module's own doc
//!   comment says a future write-capable caller must revalidate
//!   immediately before writing; this is that caller. This module never
//!   reimplements symlink/traversal detection itself.
//! - [`super::cheat_install_result`]'s [`CheatInstallEntryResult`]/
//!   [`CheatInstallRun`]/[`CheatInstallSummary`]/[`CheatInstallRunStatus`]
//!   data model, and its pure [`plan_cheat_install_entry`] bridge, as the
//!   starting point for every result this module produces - this module
//!   only ever *upgrades* a planned result to a real one, or downgrades it
//!   to a `skipped_*`/`failed_*` outcome on revalidation failure. It never
//!   defines a second result or journal shape.
//! - [`crate::canonical_platform_for_alias`] (the exact alias table
//!   matching already uses) to reconstruct the destination's platform
//!   directory fresh from the catalogue record, rather than trusting the
//!   preview's own path string.
//! - [`HostReadOnlyFilesystem`] for every bounded read this module
//!   performs (source revalidation, destination re-hash, backup
//!   verification).
//!
//! ## Revalidation, not trust
//!
//! Every write in this module is preceded by: a fresh bounded read and
//! hash of the source file (rejecting `skipped_source_changed` on any
//! mismatch or read failure); a fresh [`assess_destination`] call
//! (rejecting `failed_unsafe_path` on any symlink/traversal/wrong-type
//! problem); and a fresh read/hash of any existing destination content
//! (rejecting `skipped_destination_changed` on any mismatch from what the
//! staging preview captured). The staging preview's own proposed path and
//! hashes are never trusted as still-current.
//!
//! ## Accepted residual TOCTOU gap
//!
//! Per [`super::destination_safety`]'s own documented limitation, no
//! amount of revalidation before an operation eliminates every race
//! between that check and the operation itself. This module narrows that
//! window as far as practical: every write lands first in a uniquely
//! named temporary file in the *same* directory as the final destination
//! (never a predictable shared path), is flushed and `fsync`ed, and is
//! hash-verified *before* ever being made visible at its final name via
//! [`rename_no_replace`] (a Linux `renameat2(..., RENAME_NOREPLACE)` call
//! that atomically refuses to clobber an unexpected concurrent creation,
//! falling back to an existence check plus `rename` elsewhere). The
//! original destination content for a replacement is never read into a
//! write path at all - only ever backed up (verified) and then atomically
//! replaced - so there is no scenario in which this module's own code
//! path can leave the original destination partially modified without
//! either a verified backup or a verified new file in its place.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::canonical_platform_for_alias;
use crate::emulator_environment::{
    BoundedReadResult, HostReadOnlyFilesystem, ReadOnlyHostFilesystem,
};

use super::cheat_catalogue::{CheatAvailabilityEntry, MAX_CATALOGUE_FILE_BYTES};
use super::cheat_install_result::{
    CHEAT_INSTALL_RUN_SCHEMA_VERSION, CheatInstallEntryResult, CheatInstallOutcome,
    CheatInstallPath, CheatInstallRun, CheatInstallRunStatus, CheatInstallSummary,
    PreviousDestinationState, plan_cheat_install_entry,
};
use super::destination_safety::{
    DestinationSafetyFailureReason, DestinationState, assess_destination,
};

/// Directory name (beneath the ArchiveFS XDG data directory) that holds one
/// journal file per real apply run.
pub const CHEAT_INSTALL_RUNS_DIRECTORY_NAME: &str = "cheat-install-runs";
/// Directory name (beneath the ArchiveFS XDG data directory) that holds
/// pre-replacement backups.
pub const CHEAT_INSTALL_BACKUPS_DIRECTORY_NAME: &str = "cheat-install-backups";

// ---------------------------------------------------------------------
// Test-only fault injection - compiled only under `cfg(test)`; a release
// build contains no injection state and `should_inject` is a trivial
// always-`false` function the optimizer removes entirely.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    BackupWrite,
    TempDestinationWrite,
    Rename,
    FinalVerification,
    JournalWrite,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULT: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn inject_fault_for_test(point: Option<FaultPoint>) {
    INJECTED_FAULT.with(|cell| cell.set(point));
}

#[cfg(test)]
fn should_inject(point: FaultPoint) -> bool {
    INJECTED_FAULT.with(|cell| cell.get() == Some(point))
}

#[cfg(not(test))]
fn should_inject(_point: FaultPoint) -> bool {
    false
}

// ---------------------------------------------------------------------
// Options and top-level entry point
// ---------------------------------------------------------------------

/// Every input [`execute_cheat_install_run`] needs, gathered in one place
/// so the pure/impure boundary is explicit: everything here is supplied by
/// the caller (the CLI in production; a test fixture in tests) - this
/// module never resolves a default path, reads `$HOME`, or opens the live
/// ArchiveFS database itself.
pub struct CheatInstallOptions {
    pub destination_root: PathBuf,
    pub allow_replace_different: bool,
    pub dry_run: bool,
    /// Whether the caller passed `--yes`. When `false`, the run is always
    /// treated as a dry run regardless of `dry_run` - see
    /// [`execute_cheat_install_run`].
    pub confirmed: bool,
    pub journal_directory: PathBuf,
    pub backup_directory: PathBuf,
    pub run_id: String,
    pub started_at_unix_seconds: u64,
    pub catalogue_source: String,
}

pub struct CheatInstallRunOutcome {
    pub run: CheatInstallRun,
    /// Where the journal was written, for a real (non-dry-run) run whose
    /// journal write succeeded. Always `None` for a dry run.
    pub journal_path: Option<PathBuf>,
    /// Set when a real run's journal write itself failed - the run's own
    /// entries and summary are still fully valid and already reflect
    /// exactly what was (or was not) written to the filesystem; only the
    /// durable record of that run failed to be persisted.
    pub journal_error: Option<String>,
}

/// Executes one full installation run: plans every entry (reusing
/// [`plan_cheat_install_entry`]), revalidates and, if eligible and
/// permitted, applies each independently, then writes one journal file for
/// a real (non-dry-run, confirmed) run.
///
/// `options.confirmed` is the `--yes` gate: whenever it is `false`, this
/// function behaves *exactly* as a dry run (identical planned outcomes,
/// zero filesystem writes, `dry_run: true` on the returned run) regardless
/// of `options.dry_run` - the two are combined, not layered, so a caller
/// can never accidentally write by only checking one of them.
///
/// One entry failing never stops another from being processed: every
/// entry in `entries` is always attempted independently, in the given
/// (already-deterministic) order.
pub fn execute_cheat_install_run(
    entries: &[CheatAvailabilityEntry],
    options: &CheatInstallOptions,
) -> CheatInstallRunOutcome {
    let effective_dry_run = options.dry_run || !options.confirmed;
    let mut claimed_destinations: BTreeSet<PathBuf> = BTreeSet::new();

    let results: Vec<CheatInstallEntryResult> = entries
        .iter()
        .map(|entry| {
            install_one_entry(entry, options, effective_dry_run, &mut claimed_destinations)
        })
        .collect();

    let summary = CheatInstallSummary::from_entries(&results, effective_dry_run);
    let status = CheatInstallRunStatus::derive(&summary, effective_dry_run);
    let completed_at_unix_seconds = current_unix_seconds().max(options.started_at_unix_seconds);

    let run = CheatInstallRun {
        schema_version: CHEAT_INSTALL_RUN_SCHEMA_VERSION,
        run_id: options.run_id.clone(),
        started_at_unix_seconds: options.started_at_unix_seconds,
        completed_at_unix_seconds: Some(completed_at_unix_seconds),
        dry_run: effective_dry_run,
        allow_replace_different: options.allow_replace_different,
        destination_root: Some(CheatInstallPath::from_path(&options.destination_root)),
        catalogue_source: options.catalogue_source.clone(),
        entries: results,
        summary,
        status,
    };

    if effective_dry_run {
        return CheatInstallRunOutcome {
            run,
            journal_path: None,
            journal_error: None,
        };
    }

    match write_cheat_install_journal(&run, &options.journal_directory) {
        Ok(path) => CheatInstallRunOutcome {
            run,
            journal_path: Some(path),
            journal_error: None,
        },
        Err(error) => CheatInstallRunOutcome {
            run,
            journal_path: None,
            journal_error: Some(error),
        },
    }
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------
// Per-entry install
// ---------------------------------------------------------------------

fn install_one_entry(
    entry: &CheatAvailabilityEntry,
    options: &CheatInstallOptions,
    effective_dry_run: bool,
    claimed_destinations: &mut BTreeSet<PathBuf>,
) -> CheatInstallEntryResult {
    let mut result = plan_cheat_install_entry(entry, options.allow_replace_different);

    // `skipped_not_eligible` / `skipped_conflict` / `skipped_replace_not_allowed`
    // are already fully and correctly decided by the pure bridge above -
    // no filesystem work is ever needed (or performed) for them.
    if !matches!(
        result.outcome,
        CheatInstallOutcome::InstalledNew
            | CheatInstallOutcome::AlreadyInstalled
            | CheatInstallOutcome::ReplacedWithBackup
    ) {
        return result;
    }

    let record = &entry.game;
    let filesystem = HostReadOnlyFilesystem;

    // --- 1. Source revalidation: never trust the preview's captured hash. ---
    // `EncodedPath.display` is a lossy, non-reversible rendering whenever
    // `lossy` is true - reconstructing a `Path` from it would silently read
    // the wrong file (or nothing at all). Every source that reaches
    // `cheat_catalogue`'s own pipeline already has a non-lossy path (a
    // non-UTF-8 catalogue filename is skipped before a `CheatGameRecord` is
    // ever created), but this module never assumes that invariant holds
    // without checking.
    if record.source_file_path.lossy {
        return reject(
            result,
            CheatInstallOutcome::SkippedSourceChanged,
            "source_path_lossy_cannot_revalidate",
            "source path is only available in lossy form; cannot safely re-open it",
        );
    }
    let source_path = Path::new(&record.source_file_path.display);
    let source_bytes = match filesystem.read_bounded(source_path, MAX_CATALOGUE_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) => bytes,
        _ => {
            return reject(
                result,
                CheatInstallOutcome::SkippedSourceChanged,
                "source_missing_or_unreadable",
                "source file could not be re-read immediately before install",
            );
        }
    };
    let observed_source_hash = hex_sha256(&source_bytes);
    result.observed_source_hash = Some(observed_source_hash.clone());
    if Some(observed_source_hash.as_str()) != result.expected_source_hash.as_deref() {
        return reject(
            result,
            CheatInstallOutcome::SkippedSourceChanged,
            "source_hash_changed_since_preview",
            "source content changed since the catalogue was previewed",
        );
    }

    // --- 2. Reconstruct and revalidate the destination fresh, via destination_safety. ---
    let Some(platform_hint) = record.source_platform.as_deref() else {
        // Unreachable for a genuinely eligible entry (the staging preview
        // never reaches install_new/already_installed/replace_different
        // without a resolvable platform), but this module never assumes
        // that invariant holds without checking.
        return reject(
            result,
            CheatInstallOutcome::FailedUnsafePath,
            "source_platform_unresolved",
            "no platform hint available to reconstruct the destination",
        );
    };
    let Some(canonical_platform) = canonical_platform_for_alias(platform_hint) else {
        return reject(
            result,
            CheatInstallOutcome::FailedUnsafePath,
            "source_platform_unresolved",
            "platform hint does not resolve to a known canonical platform",
        );
    };
    let file_name = format!("{}.cht", record.source_game_name);

    let assessment = match assess_destination(
        &options.destination_root,
        OsStr::new(canonical_platform),
        OsStr::new(&file_name),
    ) {
        Ok(assessment) => assessment,
        Err(error) => {
            return reject(
                result,
                CheatInstallOutcome::FailedUnsafePath,
                &destination_safety_reason_code(error.reason),
                &format!("destination safety check failed: {error}"),
            );
        }
    };

    let resolved_destination_path = assessment.proposed_destination.path().to_path_buf();
    let resolved_destination = CheatInstallPath::from_path(&resolved_destination_path);
    if result.destination_path.as_ref() != Some(&resolved_destination) {
        result.detail.push(
            "destination path re-resolved at apply time differs from the preview".to_string(),
        );
    }
    result.destination_path = Some(resolved_destination);

    // --- 3. Batch-level duplicate-destination defense in depth. ---
    if !claimed_destinations.insert(resolved_destination_path.clone()) {
        return reject(
            result,
            CheatInstallOutcome::SkippedConflict,
            "duplicate_destination_at_apply_time",
            "another entry in this run already claimed this destination",
        );
    }

    // --- 4. Confirm the destination's real state still matches what was planned. ---
    let matches_expected_state = matches!(
        (result.outcome, assessment.destination_state),
        (CheatInstallOutcome::InstalledNew, DestinationState::Absent)
            | (
                CheatInstallOutcome::AlreadyInstalled,
                DestinationState::RegularFile
            )
            | (
                CheatInstallOutcome::ReplacedWithBackup,
                DestinationState::RegularFile
            )
    );
    if !matches_expected_state {
        return reject(
            result,
            CheatInstallOutcome::SkippedDestinationChanged,
            "destination_state_changed_since_preview",
            &format!("destination is now {:?}", assessment.destination_state),
        );
    }

    let mut fresh_destination_hash = None;
    if assessment.destination_state == DestinationState::RegularFile {
        match filesystem.read_bounded(&resolved_destination_path, MAX_CATALOGUE_FILE_BYTES) {
            BoundedReadResult::Ok(bytes) => fresh_destination_hash = Some(hex_sha256(&bytes)),
            _ => {
                return reject(
                    result,
                    CheatInstallOutcome::SkippedDestinationChanged,
                    "destination_unreadable_at_apply_time",
                    "existing destination could not be re-read immediately before install",
                );
            }
        }
    }

    match result.outcome {
        CheatInstallOutcome::AlreadyInstalled => {
            if fresh_destination_hash.as_deref() != Some(observed_source_hash.as_str()) {
                return reject(
                    result,
                    CheatInstallOutcome::SkippedDestinationChanged,
                    "destination_hash_changed_since_preview",
                    "existing destination content no longer matches the source",
                );
            }
            result.previous_destination_hash = fresh_destination_hash;
            return result;
        }
        CheatInstallOutcome::ReplacedWithBackup
            if fresh_destination_hash.as_deref() != result.previous_destination_hash.as_deref() =>
        {
            return reject(
                result,
                CheatInstallOutcome::SkippedDestinationChanged,
                "destination_hash_changed_since_preview",
                "existing destination content changed since the catalogue was previewed",
            );
        }
        _ => {}
    }

    if effective_dry_run {
        // Every field above already reflects exactly what a real run would
        // attempt; `applied` stays `false` and nothing further happens.
        return result;
    }

    match result.outcome {
        CheatInstallOutcome::InstalledNew => {
            install_new_file(
                &mut result,
                assessment.proposed_destination.path(),
                &source_bytes,
                &observed_source_hash,
            );
        }
        CheatInstallOutcome::ReplacedWithBackup => {
            replace_with_backup(
                &mut result,
                &resolved_destination_path,
                &source_bytes,
                &observed_source_hash,
                &options.backup_directory,
                &options.run_id,
            );
        }
        _ => unreachable!("already_installed already returned above"),
    }
    result
}

fn reject(
    mut result: CheatInstallEntryResult,
    outcome: CheatInstallOutcome,
    reason_code: &str,
    detail: &str,
) -> CheatInstallEntryResult {
    result.outcome = outcome;
    result.reason_code = reason_code.to_string();
    result.detail.push(detail.to_string());
    result
}

fn destination_safety_reason_code(reason: DestinationSafetyFailureReason) -> String {
    serde_json::to_string(&reason)
        .map(|json| json.trim_matches('"').to_string())
        .unwrap_or_else(|_| "unsafe_destination".to_string())
}

// ---------------------------------------------------------------------
// Real writes: install_new
// ---------------------------------------------------------------------

fn install_new_file(
    result: &mut CheatInstallEntryResult,
    destination: &Path,
    source_bytes: &[u8],
    source_hash: &str,
) {
    let Some(parent) = destination.parent() else {
        result.outcome = CheatInstallOutcome::FailedUnsafePath;
        result.reason_code = "destination_has_no_parent".to_string();
        return;
    };

    if let Err((outcome, reason, detail)) = create_directory_chain(parent) {
        result.outcome = outcome;
        result.reason_code = reason;
        result.detail.push(detail);
        return;
    }

    let temp_path = parent.join(unique_temp_component("cheat-install"));

    if should_inject(FaultPoint::TempDestinationWrite) {
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "injected_temp_write_failure".to_string();
        return;
    }
    if let Err(error) = write_new_temp_file(&temp_path, source_bytes) {
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "temp_file_write_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }

    if let Err(error) = verify_temp_file_hash(&temp_path, source_hash) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedVerification;
        result.reason_code = "temp_file_hash_mismatch".to_string();
        result.detail.push(error);
        return;
    }

    if let Err(error) = set_sensible_file_permissions(&temp_path) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "permission_set_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }

    // Revalidate the exact final path immediately before rename: it must
    // still be genuinely absent.
    if fs::symlink_metadata(destination).is_ok() {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::SkippedDestinationChanged;
        result.reason_code = "destination_appeared_before_rename".to_string();
        result
            .detail
            .push("something occupied the destination immediately before rename".to_string());
        return;
    }

    if should_inject(FaultPoint::Rename) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "injected_rename_failure".to_string();
        return;
    }
    if let Err(error) = rename_no_replace(&temp_path, destination) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "rename_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }

    fsync_directory_best_effort(parent, &mut result.detail);

    result.resulting_destination_hash = Some(source_hash.to_string());
    result.previous_destination_state = PreviousDestinationState::Absent;
    result.applied = true;
}

// ---------------------------------------------------------------------
// Real writes: replace_different (backup, then replace)
// ---------------------------------------------------------------------

fn replace_with_backup(
    result: &mut CheatInstallEntryResult,
    destination: &Path,
    source_bytes: &[u8],
    source_hash: &str,
    backup_directory: &Path,
    run_id: &str,
) {
    let filesystem = HostReadOnlyFilesystem;
    let previous_hash = result.previous_destination_hash.clone().unwrap_or_default();
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cheat".to_string());
    let hash_prefix: String = previous_hash.chars().take(12).collect();
    let safe_run_id = sanitize_for_filename(run_id);

    if let Err(error) = fs::create_dir_all(backup_directory) {
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "backup_directory_creation_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }

    if should_inject(FaultPoint::BackupWrite) {
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "injected_backup_write_failure".to_string();
        return;
    }

    let current_destination_bytes =
        match filesystem.read_bounded(destination, MAX_CATALOGUE_FILE_BYTES) {
            BoundedReadResult::Ok(bytes) => bytes,
            _ => {
                result.outcome = CheatInstallOutcome::SkippedDestinationChanged;
                result.reason_code = "destination_unreadable_before_backup".to_string();
                return;
            }
        };
    if hex_sha256(&current_destination_bytes) != previous_hash {
        result.outcome = CheatInstallOutcome::SkippedDestinationChanged;
        result.reason_code = "destination_hash_changed_before_backup".to_string();
        return;
    }

    let backup_final_path =
        backup_directory.join(format!("{file_name}.{safe_run_id}.{hash_prefix}.bak"));
    let backup_temp_path = backup_directory.join(unique_temp_component("cheat-backup"));

    if let Err(error) = write_new_temp_file(&backup_temp_path, &current_destination_bytes) {
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "backup_temp_write_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }
    if let Err(error) = verify_backup_file_hash(&backup_temp_path, &previous_hash) {
        let _ = fs::remove_file(&backup_temp_path);
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "backup_hash_mismatch".to_string();
        result.detail.push(error);
        return;
    }
    if backup_final_path.exists() {
        let _ = fs::remove_file(&backup_temp_path);
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "backup_already_exists".to_string();
        return;
    }
    if let Err(error) = rename_no_replace(&backup_temp_path, &backup_final_path) {
        let _ = fs::remove_file(&backup_temp_path);
        result.outcome = CheatInstallOutcome::FailedBackup;
        result.reason_code = "backup_finalize_failed".to_string();
        result.detail.push(error.to_string());
        return;
    }
    fsync_directory_best_effort(backup_directory, &mut result.detail);
    result.backup_path = Some(CheatInstallPath::from_path(&backup_final_path));

    // A verified backup now durably exists. Only now install the
    // replacement, using the exact same temp-write/verify/revalidate
    // sequence as a new install - the original destination content is
    // never opened for writing at any point; it is only ever read (above,
    // to create the backup) or atomically replaced (below).
    let Some(parent) = destination.parent() else {
        result.outcome = CheatInstallOutcome::FailedUnsafePath;
        result.reason_code = "destination_has_no_parent".to_string();
        result.detail.push(format!(
            "backup preserved at {}",
            backup_final_path.display()
        ));
        return;
    };
    let temp_path = parent.join(unique_temp_component("cheat-install"));

    if should_inject(FaultPoint::TempDestinationWrite) {
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "injected_temp_write_failure".to_string();
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }
    if let Err(error) = write_new_temp_file(&temp_path, source_bytes) {
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "temp_file_write_failed".to_string();
        result.detail.push(error.to_string());
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }
    if let Err(error) = verify_temp_file_hash(&temp_path, source_hash) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedVerification;
        result.reason_code = "temp_file_hash_mismatch".to_string();
        result.detail.push(error);
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }
    if let Err(error) = set_sensible_file_permissions(&temp_path) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "permission_set_failed".to_string();
        result.detail.push(error.to_string());
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }

    match filesystem.read_bounded(destination, MAX_CATALOGUE_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) if hex_sha256(&bytes) == previous_hash => {}
        _ => {
            let _ = fs::remove_file(&temp_path);
            result.outcome = CheatInstallOutcome::SkippedDestinationChanged;
            result.reason_code = "destination_changed_before_replace".to_string();
            result.detail.push(format!(
                "backup preserved at {}",
                backup_final_path.display()
            ));
            return;
        }
    }

    if should_inject(FaultPoint::Rename) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "injected_rename_failure".to_string();
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }
    // A replacing rename is the one place a clobbering `fs::rename` is
    // correct - replacement is the whole point, and the content being
    // replaced was just verified above and is durably backed up either
    // way.
    if let Err(error) = fs::rename(&temp_path, destination) {
        let _ = fs::remove_file(&temp_path);
        result.outcome = CheatInstallOutcome::FailedWrite;
        result.reason_code = "rename_failed".to_string();
        result.detail.push(error.to_string());
        result.detail.push(format!(
            "original preserved; backup at {}",
            backup_final_path.display()
        ));
        return;
    }

    fsync_directory_best_effort(parent, &mut result.detail);
    result.resulting_destination_hash = Some(source_hash.to_string());
    result.applied = true;
}

// ---------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------

fn write_cheat_install_journal(
    run: &CheatInstallRun,
    journal_directory: &Path,
) -> Result<PathBuf, String> {
    if should_inject(FaultPoint::JournalWrite) {
        return Err("injected journal write failure".to_string());
    }
    fs::create_dir_all(journal_directory)
        .map_err(|error| format!("could not create journal directory: {error}"))?;

    let file_name = format!("{}.json", sanitize_for_filename(&run.run_id));
    let final_path = journal_directory.join(&file_name);
    if final_path.exists() {
        return Err(format!(
            "a journal already exists at {}",
            final_path.display()
        ));
    }

    let json = serde_json::to_string_pretty(run)
        .map_err(|error| format!("could not serialize journal: {error}"))?;
    let temp_path = journal_directory.join(unique_temp_component("cheat-journal"));
    write_new_temp_file(&temp_path, json.as_bytes())
        .map_err(|error| format!("could not write journal temp file: {error}"))?;

    if let Err(error) = rename_no_replace(&temp_path, &final_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("could not finalize journal: {error}"));
    }

    let mut detail = Vec::new();
    fsync_directory_best_effort(journal_directory, &mut detail);
    Ok(final_path)
}

// ---------------------------------------------------------------------
// Small filesystem/utility helpers
// ---------------------------------------------------------------------

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

static TEMP_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, non-predictable temporary filename component - never a shared
/// path, never reused between calls even within the same process/second.
fn unique_temp_component(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = TEMP_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        ".archivefs-{prefix}-{}-{nanos:x}-{sequence:x}",
        std::process::id()
    )
}

fn sanitize_for_filename(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "run".to_string()
    } else {
        sanitized
    }
}

fn write_new_temp_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Verifies the replacement/new content temp file - this is the "final
/// verification" fault point (it is the last check before the file
/// becomes visible at its real destination name).
fn verify_temp_file_hash(path: &Path, expected_hash: &str) -> Result<(), String> {
    if should_inject(FaultPoint::FinalVerification) {
        return Err("injected verification failure".to_string());
    }
    verify_file_hash_unconditionally(path, expected_hash)
}

/// Verifies a backup temp file - deliberately not gated by the "final
/// verification" fault point (that name refers to the replacement
/// content's own verification, not the backup's); a backup-specific
/// failure is instead covered by [`FaultPoint::BackupWrite`], checked
/// earlier in the backup sequence.
fn verify_backup_file_hash(path: &Path, expected_hash: &str) -> Result<(), String> {
    verify_file_hash_unconditionally(path, expected_hash)
}

fn verify_file_hash_unconditionally(path: &Path, expected_hash: &str) -> Result<(), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not re-read temp file for verification: {error}"))?;
    let actual = hex_sha256(&bytes);
    if actual == expected_hash {
        Ok(())
    } else {
        Err(format!(
            "verification hash mismatch: expected {expected_hash}, got {actual}"
        ))
    }
}

fn set_sensible_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn fsync_directory_best_effort(directory: &Path, detail: &mut Vec<String>) {
    match File::open(directory) {
        Ok(file) => {
            if let Err(error) = file.sync_all() {
                detail.push(format!("directory fsync failed (non-fatal): {error}"));
            }
        }
        Err(error) => detail.push(format!(
            "could not open directory for fsync (non-fatal): {error}"
        )),
    }
}

/// Creates every missing directory level from the filesystem root down
/// through `target_dir`, one level at a time, revalidating each with
/// `symlink_metadata` immediately after creating it. `fs::create_dir`
/// (never `create_dir_all`) is used for each level: on Unix it fails with
/// `AlreadyExists` if anything - including a symlink - already occupies
/// the path, so a symlink placed there by a concurrent process between our
/// check and our create is never silently traversed.
fn create_directory_chain(target_dir: &Path) -> Result<(), (CheatInstallOutcome, String, String)> {
    let mut current = PathBuf::new();
    for component in target_dir.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => continue,
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err((
                    CheatInstallOutcome::FailedUnsafePath,
                    "parent_symlink_rejected".to_string(),
                    format!("{} is a symlink, not a plain directory", current.display()),
                ));
            }
            Ok(_) => {
                return Err((
                    CheatInstallOutcome::FailedUnsafePath,
                    "parent_not_a_directory".to_string(),
                    format!("{} exists but is not a plain directory", current.display()),
                ));
            }
            Err(_) => create_one_directory_level(&current)?,
        }
    }
    Ok(())
}

fn create_one_directory_level(path: &Path) -> Result<(), (CheatInstallOutcome, String, String)> {
    if let Err(error) = fs::create_dir(path)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err((
            CheatInstallOutcome::FailedWrite,
            "directory_creation_failed".to_string(),
            format!("{}: {error}", path.display()),
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        _ => Err((
            CheatInstallOutcome::FailedUnsafePath,
            "directory_revalidation_failed".to_string(),
            format!(
                "{} is not a plain directory immediately after creation",
                path.display()
            ),
        )),
    }
}

/// Atomically renames `from` to `to`, refusing to clobber an unexpected
/// concurrent creation at `to`. Uses Linux's `renameat2(...,
/// RENAME_NOREPLACE)` where available; elsewhere, falls back to an
/// existence check immediately before a plain `rename` (a small, accepted
/// TOCTOU gap - see the module doc comment).
#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `from_c`/`to_c` are valid, NUL-terminated C strings owned for
    // the duration of this call; `AT_FDCWD` with absolute paths is a
    // documented, safe `renameat2` usage.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    fs::rename(from, to)
}

#[cfg(test)]
mod tests;
