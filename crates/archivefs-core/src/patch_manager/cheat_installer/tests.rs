use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::canonical_platform_for_alias;
use crate::emulator_environment::EncodedPath;
use crate::patch_manager::cheat_catalogue::{
    CheatCatalogueFormat, CheatGameMatch, CheatGameRecord, CheatInstalledState,
    CheatMatchConfidence, CheatStagingAction, CheatStagingPlan,
};
use crate::patch_manager::parse_cheat_install_run;

// ---------------------------------------------------------------------
// Fixture helpers - every test uses its own temporary directories under
// `std::env::temp_dir()`; nothing here ever touches a real `$HOME`, a
// real RetroArch cheat directory, or the live ArchiveFS database.
// ---------------------------------------------------------------------

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "archivefs-cheat-installer-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn dummy_game_match(confidence: CheatMatchConfidence) -> CheatGameMatch {
    CheatGameMatch {
        confidence,
        evidence: Vec::new(),
        candidates: Vec::new(),
    }
}

/// Builds one `CheatAvailabilityEntry` whose `source_file_path` points at a
/// real file - the installer performs real bounded reads against it, so
/// every test that exercises apply/dry-run logic needs a genuine file on
/// disk, not a synthetic path.
#[allow(clippy::too_many_arguments)]
fn build_entry(
    game_name: &str,
    platform: &str,
    source_path: EncodedPath,
    source_hash: String,
    confidence: CheatMatchConfidence,
    action: CheatStagingAction,
    reason: &'static str,
    destination_root: &Path,
    existing_destination_hash: Option<String>,
) -> CheatAvailabilityEntry {
    let canonical = canonical_platform_for_alias(platform).unwrap_or(platform);
    let proposed_destination_path = if matches!(action, CheatStagingAction::NotEligible) {
        None
    } else {
        Some(EncodedPath::from_path(
            &destination_root
                .join(canonical)
                .join(format!("{game_name}.cht")),
        ))
    };
    let game = CheatGameRecord {
        source_game_name: game_name.to_string(),
        source_platform: Some(platform.to_string()),
        source_region: None,
        source_revision: None,
        source_identifier: None,
        source_content_hash: None,
        target_emulator: Some("retroarch".to_string()),
        cheat_count: 1,
        cheats: Vec::new(),
        enabled_by_default_count: 0,
        source_file_path: source_path.clone(),
        source_file_hash: Some(source_hash.clone()),
        format: CheatCatalogueFormat::RetroarchChtDirectory,
        parsing_complete: true,
        parsing_diagnostics: Vec::new(),
    };
    let staging_plan = CheatStagingPlan {
        source_cheat_path: source_path,
        proposed_destination_path,
        source_file_hash: Some(source_hash),
        existing_destination_hash,
        planned_action: action,
        reason,
    };
    CheatAvailabilityEntry {
        game,
        game_match: dummy_game_match(confidence),
        installed_state: CheatInstalledState::Unknown,
        installed_state_detail: Vec::new(),
        staging_candidate: false,
        destructive_if_applied: false,
        staging_plan,
    }
}

fn write_source(root: &Path, name: &str, content: &[u8]) -> (EncodedPath, String) {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    (EncodedPath::from_path(&path), hex_sha256(content))
}

fn install_new_entry(
    catalogue_root: &Path,
    destination_root: &Path,
    game_name: &str,
    content: &[u8],
) -> CheatAvailabilityEntry {
    let (source_path, hash) = write_source(catalogue_root, &format!("{game_name}.cht"), content);
    build_entry(
        game_name,
        "Atari - 2600",
        source_path,
        hash,
        CheatMatchConfidence::Strong,
        CheatStagingAction::InstallNew,
        "destination_missing",
        destination_root,
        None,
    )
}

fn options(
    destination_root: &Path,
    journal_directory: &Path,
    backup_directory: &Path,
    dry_run: bool,
    confirmed: bool,
    allow_replace_different: bool,
    run_id: &str,
) -> CheatInstallOptions {
    CheatInstallOptions {
        destination_root: destination_root.to_path_buf(),
        allow_replace_different,
        dry_run,
        confirmed,
        journal_directory: journal_directory.to_path_buf(),
        backup_directory: backup_directory.to_path_buf(),
        run_id: run_id.to_string(),
        started_at_unix_seconds: 1_700_000_000,
        catalogue_source: "Fixture Catalogue".to_string(),
    }
}

fn dir_entries(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

struct FaultGuard;

impl FaultGuard {
    fn new(point: FaultPoint) -> Self {
        inject_fault_for_test(Some(point));
        Self
    }
}

impl Drop for FaultGuard {
    fn drop(&mut self) {
        inject_fault_for_test(None);
    }
}

// ---------------------------------------------------------------------
// Dry-run / confirmation gate
// ---------------------------------------------------------------------

#[test]
fn dry_run_creates_nothing() {
    let catalogue_root = temp_root("dry-run-catalogue");
    let destination_root = temp_root("dry-run-dest");
    let journal_dir = temp_root("dry-run-journal");
    let backup_dir = temp_root("dry-run-backup");
    fs::remove_dir_all(&journal_dir).unwrap();
    fs::remove_dir_all(&backup_dir).unwrap();

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        true,
        true,
        false,
        "run-1",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert!(outcome.run.dry_run);
    assert_eq!(outcome.run.status, CheatInstallRunStatus::DryRun);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::InstalledNew
    );
    assert!(!outcome.run.entries[0].applied);
    assert_eq!(outcome.run.summary.writes_attempted, 0);
    assert_eq!(outcome.run.summary.writes_succeeded, 0);
    assert!(!destination_root.join("Atari2600").exists());
    assert!(!journal_dir.exists());
    assert!(outcome.journal_path.is_none());
}

#[test]
fn apply_without_yes_writes_nothing() {
    let catalogue_root = temp_root("no-yes-catalogue");
    let destination_root = temp_root("no-yes-dest");
    let journal_dir = temp_root("no-yes-journal");
    let backup_dir = temp_root("no-yes-backup");
    fs::remove_dir_all(&journal_dir).unwrap();
    fs::remove_dir_all(&backup_dir).unwrap();

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    // dry_run: false, confirmed: false - must still behave as a dry run.
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        false,
        false,
        "run-2",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert!(outcome.run.dry_run);
    assert!(!destination_root.join("Atari2600").exists());
    assert!(!journal_dir.exists());
}

// ---------------------------------------------------------------------
// install_new
// ---------------------------------------------------------------------

#[test]
fn install_new_creates_expected_file_with_matching_hash() {
    let catalogue_root = temp_root("install-new-catalogue");
    let destination_root = temp_root("install-new-dest");
    let journal_dir = temp_root("install-new-journal");
    let backup_dir = temp_root("install-new-backup");

    let content = b"cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n";
    let entry = install_new_entry(&catalogue_root, &destination_root, "Frogger", content);
    let source_hash = entry.staging_plan.source_file_hash.clone().unwrap();
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-3",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::InstalledNew);
    assert!(result.applied);

    let installed_path = destination_root.join("Atari2600").join("Frogger.cht");
    let installed_bytes = fs::read(&installed_path).unwrap();
    assert_eq!(installed_bytes, content);
    assert_eq!(hex_sha256(&installed_bytes), source_hash);
    assert_eq!(
        result.resulting_destination_hash.as_deref(),
        Some(source_hash.as_str())
    );

    // No stray temporary files left behind.
    let leftovers: Vec<String> = dir_entries(&destination_root.join("Atari2600"))
        .into_iter()
        .filter(|name| name.starts_with(".archivefs-"))
        .collect();
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
}

// ---------------------------------------------------------------------
// already_installed
// ---------------------------------------------------------------------

#[test]
fn already_installed_performs_no_write() {
    let catalogue_root = temp_root("already-installed-catalogue");
    let destination_root = temp_root("already-installed-dest");
    let journal_dir = temp_root("already-installed-journal");
    let backup_dir = temp_root("already-installed-backup");

    let content = b"cheats = 0\n";
    let (source_path, hash) = write_source(&catalogue_root, "Frogger.cht", content);
    let platform_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&platform_dir).unwrap();
    let destination_path = platform_dir.join("Frogger.cht");
    fs::write(&destination_path, content).unwrap();
    let before_metadata = fs::metadata(&destination_path).unwrap();

    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        hash.clone(),
        CheatMatchConfidence::Strong,
        CheatStagingAction::AlreadyInstalled,
        "hash_match",
        &destination_root,
        Some(hash),
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-4",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::AlreadyInstalled);
    assert!(!result.applied);
    assert!(!result.write_required);

    let after_metadata = fs::metadata(&destination_path).unwrap();
    assert_eq!(before_metadata.len(), after_metadata.len());
    assert_eq!(fs::read(&destination_path).unwrap(), content);
    assert!(outcome.run.entries[0].backup_path.is_none());
}

// ---------------------------------------------------------------------
// replace_different
// ---------------------------------------------------------------------

fn replace_different_entry(
    catalogue_root: &Path,
    destination_root: &Path,
    new_content: &[u8],
    old_content: &[u8],
) -> CheatAvailabilityEntry {
    let (source_path, source_hash) = write_source(catalogue_root, "Frogger.cht", new_content);
    let platform_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&platform_dir).unwrap();
    fs::write(platform_dir.join("Frogger.cht"), old_content).unwrap();
    let old_hash = hex_sha256(old_content);
    build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        source_hash,
        CheatMatchConfidence::Strong,
        CheatStagingAction::ReplaceDifferent,
        "hash_mismatch",
        destination_root,
        Some(old_hash),
    )
}

#[test]
fn replace_different_blocked_without_permission() {
    let catalogue_root = temp_root("replace-blocked-catalogue");
    let destination_root = temp_root("replace-blocked-dest");
    let journal_dir = temp_root("replace-blocked-journal");
    let backup_dir = temp_root("replace-blocked-backup");

    let entry = replace_different_entry(&catalogue_root, &destination_root, b"new\n", b"old\n");
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-5",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(
        result.outcome,
        CheatInstallOutcome::SkippedReplaceNotAllowed
    );
    assert!(!result.applied);
    assert!(result.backup_path.is_none());
    let destination_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&destination_path).unwrap(), b"old\n");
    assert!(backup_dir_is_empty(&backup_dir));
}

fn backup_dir_is_empty(dir: &Path) -> bool {
    !dir.exists() || fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn replacement_with_permission_creates_verified_backup_and_installs_new_content() {
    let catalogue_root = temp_root("replace-allowed-catalogue");
    let destination_root = temp_root("replace-allowed-dest");
    let journal_dir = temp_root("replace-allowed-journal");
    let backup_dir = temp_root("replace-allowed-backup");

    let entry = replace_different_entry(
        &catalogue_root,
        &destination_root,
        b"new content\n",
        b"old content\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        true,
        "run-6",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::ReplacedWithBackup);
    assert!(result.applied);

    let backup_path = result.backup_path.as_ref().expect("backup path recorded");
    let backup_bytes = fs::read(&backup_path.display).unwrap();
    assert_eq!(backup_bytes, b"old content\n");
    assert_eq!(hex_sha256(&backup_bytes), hex_sha256(b"old content\n"));

    let destination_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&destination_path).unwrap(), b"new content\n");
    assert_eq!(
        result.resulting_destination_hash.as_deref(),
        Some(hex_sha256(b"new content\n")).as_deref()
    );
}

#[test]
fn failed_replacement_preserves_original_or_verified_backup() {
    let catalogue_root = temp_root("replace-failed-catalogue");
    let destination_root = temp_root("replace-failed-dest");
    let journal_dir = temp_root("replace-failed-journal");
    let backup_dir = temp_root("replace-failed-backup");

    let entry = replace_different_entry(
        &catalogue_root,
        &destination_root,
        b"new content\n",
        b"old content\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        true,
        "run-7",
    );

    let _guard = FaultGuard::new(FaultPoint::Rename);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::FailedWrite);
    // The backup was already durably created before the injected rename
    // failure and must be preserved.
    let backup_path = result.backup_path.as_ref().expect("backup preserved");
    assert_eq!(fs::read(&backup_path.display).unwrap(), b"old content\n");
    // The original destination was never opened for writing and remains
    // exactly as it was.
    let destination_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&destination_path).unwrap(), b"old content\n");
}

// ---------------------------------------------------------------------
// Source/destination revalidation
// ---------------------------------------------------------------------

#[test]
fn changed_source_after_preview_is_rejected() {
    let catalogue_root = temp_root("source-changed-catalogue");
    let destination_root = temp_root("source-changed-dest");
    let journal_dir = temp_root("source-changed-journal");
    let backup_dir = temp_root("source-changed-backup");

    let (source_path, _original_hash) =
        write_source(&catalogue_root, "Frogger.cht", b"cheats = 0\n");
    // Simulate drift: the record claims a hash that no longer matches the
    // file's real, current content.
    let stale_hash = hex_sha256(b"a completely different original content");
    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        stale_hash,
        CheatMatchConfidence::Strong,
        CheatStagingAction::InstallNew,
        "destination_missing",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-8",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::SkippedSourceChanged);
    assert!(!destination_root.join("Atari2600").exists());
}

#[test]
fn changed_destination_after_preview_is_rejected() {
    let catalogue_root = temp_root("dest-changed-catalogue");
    let destination_root = temp_root("dest-changed-dest");
    let journal_dir = temp_root("dest-changed-journal");
    let backup_dir = temp_root("dest-changed-backup");

    let (source_path, source_hash) = write_source(&catalogue_root, "Frogger.cht", b"new content\n");
    let platform_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&platform_dir).unwrap();
    fs::write(
        platform_dir.join("Frogger.cht"),
        b"actual current content\n",
    )
    .unwrap();

    // The plan claims a *different* previous hash than what is really
    // there now (simulating something else having changed it since
    // preview).
    let stale_previous_hash = hex_sha256(b"content the preview saw, which is now gone");
    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        source_hash,
        CheatMatchConfidence::Strong,
        CheatStagingAction::ReplaceDifferent,
        "hash_mismatch",
        &destination_root,
        Some(stale_previous_hash),
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        true,
        "run-9",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(
        result.outcome,
        CheatInstallOutcome::SkippedDestinationChanged
    );
    assert!(result.backup_path.is_none());
    assert_eq!(
        fs::read(platform_dir.join("Frogger.cht")).unwrap(),
        b"actual current content\n"
    );
}

// ---------------------------------------------------------------------
// Eligibility gates
// ---------------------------------------------------------------------

#[test]
fn weak_match_writes_nothing() {
    let catalogue_root = temp_root("weak-catalogue");
    let destination_root = temp_root("weak-dest");
    let journal_dir = temp_root("weak-journal");
    let backup_dir = temp_root("weak-backup");

    let (source_path, hash) = write_source(&catalogue_root, "Frogger.cht", b"cheats = 0\n");
    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        hash,
        CheatMatchConfidence::Weak,
        CheatStagingAction::NotEligible,
        "weak_match_not_eligible",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-10",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::SkippedNotEligible
    );
    assert!(!destination_root.exists() || dir_entries(&destination_root).is_empty());
}

#[test]
fn ambiguous_match_writes_nothing() {
    let catalogue_root = temp_root("ambiguous-catalogue");
    let destination_root = temp_root("ambiguous-dest");
    let journal_dir = temp_root("ambiguous-journal");
    let backup_dir = temp_root("ambiguous-backup");

    let (source_path, hash) = write_source(&catalogue_root, "Frogger.cht", b"cheats = 0\n");
    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        hash,
        CheatMatchConfidence::Ambiguous,
        CheatStagingAction::NotEligible,
        "ambiguous_match_not_eligible",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-11",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::SkippedNotEligible
    );
    assert!(!destination_root.exists() || dir_entries(&destination_root).is_empty());
}

#[test]
fn duplicate_destination_writes_nothing() {
    let catalogue_root = temp_root("duplicate-catalogue");
    let destination_root = temp_root("duplicate-dest");
    let journal_dir = temp_root("duplicate-journal");
    let backup_dir = temp_root("duplicate-backup");

    // Already flagged as a conflict upstream (by the staging preview).
    let (source_path_a, hash_a) = write_source(&catalogue_root, "a.cht", b"cheats = 0\n");
    let entry_a = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path_a,
        hash_a,
        CheatMatchConfidence::Exact,
        CheatStagingAction::Conflict,
        "duplicate_destination",
        &destination_root,
        None,
    );
    let (source_path_b, hash_b) = write_source(&catalogue_root, "b.cht", b"cheats = 1\n");
    let entry_b = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path_b,
        hash_b,
        CheatMatchConfidence::Exact,
        CheatStagingAction::Conflict,
        "duplicate_destination",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-12",
    );

    let outcome = execute_cheat_install_run(&[entry_a, entry_b], &opts);
    assert!(
        outcome
            .run
            .entries
            .iter()
            .all(|entry| entry.outcome == CheatInstallOutcome::SkippedConflict)
    );
    assert!(!destination_root.exists() || dir_entries(&destination_root).is_empty());
}

#[test]
fn installer_own_duplicate_defense_blocks_a_second_entry_at_apply_time() {
    // Two entries that (hypothetically) both resolved to `install_new` for
    // the exact same destination - the installer's own batch-level
    // duplicate tracking must still catch this even if upstream somehow
    // did not.
    let catalogue_root = temp_root("apply-duplicate-catalogue");
    let destination_root = temp_root("apply-duplicate-dest");
    let journal_dir = temp_root("apply-duplicate-journal");
    let backup_dir = temp_root("apply-duplicate-backup");

    let entry_a = install_new_entry(&catalogue_root, &destination_root, "Frogger", b"one\n");
    let (source_path_b, hash_b) = write_source(&catalogue_root, "Frogger-alt.cht", b"two\n");
    let entry_b = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path_b,
        hash_b,
        CheatMatchConfidence::Strong,
        CheatStagingAction::InstallNew,
        "destination_missing",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-13",
    );

    let outcome = execute_cheat_install_run(&[entry_a, entry_b], &opts);
    let outcomes: Vec<CheatInstallOutcome> = outcome
        .run
        .entries
        .iter()
        .map(|entry| entry.outcome)
        .collect();
    assert_eq!(outcomes[0], CheatInstallOutcome::InstalledNew);
    assert_eq!(outcomes[1], CheatInstallOutcome::SkippedConflict);
    assert_eq!(
        outcome.run.entries[1].reason_code,
        "duplicate_destination_at_apply_time"
    );
}

// ---------------------------------------------------------------------
// Destination/path/symlink safety
// ---------------------------------------------------------------------

#[test]
fn traversal_rejected() {
    let catalogue_root = temp_root("traversal-catalogue");
    let destination_root = temp_root("traversal-dest");
    let journal_dir = temp_root("traversal-journal");
    let backup_dir = temp_root("traversal-backup");

    let (source_path, hash) = write_source(&catalogue_root, "evil.cht", b"cheats = 0\n");
    let entry = build_entry(
        "../../../etc/passwd",
        "Atari - 2600",
        source_path,
        hash,
        CheatMatchConfidence::Exact,
        CheatStagingAction::InstallNew,
        "destination_missing",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-14",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedUnsafePath
    );
    assert!(!Path::new("/etc/passwd.cht").exists());
}

#[cfg(unix)]
#[test]
fn parent_symlink_rejected() {
    let catalogue_root = temp_root("parent-symlink-catalogue");
    let destination_root = temp_root("parent-symlink-dest");
    let journal_dir = temp_root("parent-symlink-journal");
    let backup_dir = temp_root("parent-symlink-backup");
    let outside = temp_root("parent-symlink-outside");

    fs::create_dir_all(&destination_root).unwrap();
    std::os::unix::fs::symlink(&outside, destination_root.join("Atari2600")).unwrap();

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-15",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedUnsafePath
    );
    assert!(dir_entries(&outside).is_empty());
}

#[cfg(unix)]
#[test]
fn final_symlink_rejected() {
    let catalogue_root = temp_root("final-symlink-catalogue");
    let destination_root = temp_root("final-symlink-dest");
    let journal_dir = temp_root("final-symlink-journal");
    let backup_dir = temp_root("final-symlink-backup");
    let outside = temp_root("final-symlink-outside");
    fs::write(outside.join("secret.cht"), b"do not touch\n").unwrap();

    let platform_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&platform_dir).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.cht"), platform_dir.join("Frogger.cht"))
        .unwrap();

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-16",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedUnsafePath
    );
    assert_eq!(
        fs::read(outside.join("secret.cht")).unwrap(),
        b"do not touch\n"
    );
}

#[cfg(unix)]
#[test]
fn root_symlink_rejected() {
    let catalogue_root = temp_root("root-symlink-catalogue");
    let real_root = temp_root("root-symlink-real");
    let outside = temp_root("root-symlink-outside");
    let journal_dir = temp_root("root-symlink-journal");
    let backup_dir = temp_root("root-symlink-backup");
    fs::remove_dir_all(&real_root).unwrap();
    std::os::unix::fs::symlink(&outside, &real_root).unwrap();

    let entry = install_new_entry(&catalogue_root, &real_root, "Frogger", b"cheats = 0\n");
    let opts = options(
        &real_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-17",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedUnsafePath
    );
    assert!(dir_entries(&outside).is_empty());
}

#[test]
fn absent_root_created_only_during_approved_apply() {
    let catalogue_root = temp_root("absent-root-catalogue");
    let destination_root = temp_root("absent-root-dest");
    fs::remove_dir_all(&destination_root).unwrap();
    let journal_dir = temp_root("absent-root-journal");
    let backup_dir = temp_root("absent-root-backup");

    // Dry run first: root must not be created.
    let dry_entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let dry_opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        true,
        false,
        false,
        "run-18a",
    );
    let dry_outcome = execute_cheat_install_run(&[dry_entry], &dry_opts);
    assert_eq!(
        dry_outcome.run.entries[0].outcome,
        CheatInstallOutcome::InstalledNew
    );
    assert!(!destination_root.exists());

    // Real, confirmed apply: root and platform directory are created.
    let apply_entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let apply_opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-18b",
    );
    let apply_outcome = execute_cheat_install_run(&[apply_entry], &apply_opts);
    assert_eq!(
        apply_outcome.run.entries[0].outcome,
        CheatInstallOutcome::InstalledNew
    );
    assert!(
        destination_root
            .join("Atari2600")
            .join("Frogger.cht")
            .exists()
    );
}

// ---------------------------------------------------------------------
// Failure injection
// ---------------------------------------------------------------------

#[test]
fn no_partial_final_file_after_injected_write_failure() {
    let catalogue_root = temp_root("partial-write-catalogue");
    let destination_root = temp_root("partial-write-dest");
    let journal_dir = temp_root("partial-write-journal");
    let backup_dir = temp_root("partial-write-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-19",
    );

    let _guard = FaultGuard::new(FaultPoint::TempDestinationWrite);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedWrite
    );
    assert!(
        !destination_root
            .join("Atari2600")
            .join("Frogger.cht")
            .exists()
    );
}

#[test]
fn temporary_file_cleaned_after_failure() {
    let catalogue_root = temp_root("temp-cleanup-catalogue");
    let destination_root = temp_root("temp-cleanup-dest");
    let journal_dir = temp_root("temp-cleanup-journal");
    let backup_dir = temp_root("temp-cleanup-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-20",
    );

    let _guard = FaultGuard::new(FaultPoint::FinalVerification);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedVerification
    );
    assert!(
        !destination_root
            .join("Atari2600")
            .join("Frogger.cht")
            .exists()
    );
    let leftovers = dir_entries(&destination_root.join("Atari2600"));
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
}

#[test]
fn failed_verification_reported() {
    let catalogue_root = temp_root("failed-verify-catalogue");
    let destination_root = temp_root("failed-verify-dest");
    let journal_dir = temp_root("failed-verify-journal");
    let backup_dir = temp_root("failed-verify-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-21",
    );

    let _guard = FaultGuard::new(FaultPoint::FinalVerification);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::FailedVerification
    );
    assert_eq!(outcome.run.summary.failed, 1);
}

#[test]
fn backup_retained_after_verification_failure() {
    let catalogue_root = temp_root("backup-retained-catalogue");
    let destination_root = temp_root("backup-retained-dest");
    let journal_dir = temp_root("backup-retained-journal");
    let backup_dir = temp_root("backup-retained-backup");

    let entry = replace_different_entry(
        &catalogue_root,
        &destination_root,
        b"new content\n",
        b"old content\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        true,
        "run-22",
    );

    let _guard = FaultGuard::new(FaultPoint::FinalVerification);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::FailedVerification);
    let backup_path = result
        .backup_path
        .as_ref()
        .expect("backup preserved despite verification failure");
    assert_eq!(fs::read(&backup_path.display).unwrap(), b"old content\n");
    let destination_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&destination_path).unwrap(), b"old content\n");
}

#[test]
fn backup_write_failure_is_reported_and_original_untouched() {
    let catalogue_root = temp_root("backup-write-fail-catalogue");
    let destination_root = temp_root("backup-write-fail-dest");
    let journal_dir = temp_root("backup-write-fail-journal");
    let backup_dir = temp_root("backup-write-fail-backup");

    let entry = replace_different_entry(
        &catalogue_root,
        &destination_root,
        b"new content\n",
        b"old content\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        true,
        "run-23",
    );

    let _guard = FaultGuard::new(FaultPoint::BackupWrite);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::FailedBackup);
    assert!(result.backup_path.is_none());
    let destination_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&destination_path).unwrap(), b"old content\n");
}

// ---------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------

#[test]
fn real_apply_writes_one_journal_that_parses() {
    let catalogue_root = temp_root("journal-catalogue");
    let destination_root = temp_root("journal-dest");
    let journal_dir = temp_root("journal-journal");
    let backup_dir = temp_root("journal-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-24",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let journal_path = outcome
        .journal_path
        .expect("journal written for a real apply run");
    assert!(outcome.journal_error.is_none());

    let entries_in_journal_dir = dir_entries(&journal_dir);
    assert_eq!(
        entries_in_journal_dir.len(),
        1,
        "exactly one journal file: {entries_in_journal_dir:?}"
    );

    let json = fs::read_to_string(&journal_path).unwrap();
    let parsed =
        parse_cheat_install_run(&json).expect("journal parses through parse_cheat_install_run");
    assert_eq!(parsed.run_id, outcome.run.run_id);
    assert_eq!(parsed.entries.len(), outcome.run.entries.len());
    assert_eq!(parsed.status, outcome.run.status);
}

#[test]
fn dry_run_writes_no_journal() {
    let catalogue_root = temp_root("dry-journal-catalogue");
    let destination_root = temp_root("dry-journal-dest");
    let journal_dir = temp_root("dry-journal-journal");
    let backup_dir = temp_root("dry-journal-backup");
    fs::remove_dir_all(&journal_dir).unwrap();

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        true,
        true,
        false,
        "run-25",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    assert!(outcome.journal_path.is_none());
    assert!(outcome.journal_error.is_none());
    assert!(!journal_dir.exists());
}

#[test]
fn journal_write_failure_is_surfaced_without_hiding_real_success() {
    let catalogue_root = temp_root("journal-fail-catalogue");
    let destination_root = temp_root("journal-fail-dest");
    let journal_dir = temp_root("journal-fail-journal");
    let backup_dir = temp_root("journal-fail-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-26",
    );

    let _guard = FaultGuard::new(FaultPoint::JournalWrite);
    let outcome = execute_cheat_install_run(&[entry], &opts);
    drop(_guard);

    assert!(outcome.journal_path.is_none());
    assert!(outcome.journal_error.is_some());
    // The file itself was genuinely installed - the journal failure must
    // not be papered over as if nothing happened, but it also must not
    // retroactively mark the successful file write as failed.
    assert_eq!(
        outcome.run.entries[0].outcome,
        CheatInstallOutcome::InstalledNew
    );
    assert!(outcome.run.entries[0].applied);
    assert_eq!(outcome.run.status, CheatInstallRunStatus::Success);
    assert!(
        destination_root
            .join("Atari2600")
            .join("Frogger.cht")
            .exists()
    );
}

#[test]
fn journal_never_overwrites_an_existing_journal() {
    let catalogue_root = temp_root("journal-no-overwrite-catalogue");
    let destination_root = temp_root("journal-no-overwrite-dest");
    let journal_dir = temp_root("journal-no-overwrite-journal");
    let backup_dir = temp_root("journal-no-overwrite-backup");

    let entry_one = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "same-run-id",
    );
    let first = execute_cheat_install_run(&[entry_one], &opts);
    assert!(first.journal_path.is_some());

    // A second run reusing the exact same run_id/journal_directory must
    // never clobber the first journal.
    let entry_two = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let second = execute_cheat_install_run(&[entry_two], &opts);
    assert!(second.journal_path.is_none());
    assert!(second.journal_error.is_some());
    assert_eq!(dir_entries(&journal_dir).len(), 1);
}

// ---------------------------------------------------------------------
// Lossless paths
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn non_utf8_destination_root_remains_lossless() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let catalogue_root = temp_root("non-utf8-root-catalogue");
    let mut raw_root = std::env::temp_dir().into_os_string().into_vec();
    raw_root.push(b'/');
    raw_root.extend_from_slice(
        format!("archivefs-cheat-installer-nonutf8-{}-", std::process::id()).as_bytes(),
    );
    raw_root.extend_from_slice(b"bad-\xFF-root");
    let destination_root = PathBuf::from(OsString::from_vec(raw_root));
    let _ = fs::remove_dir_all(&destination_root);
    fs::create_dir_all(&destination_root).unwrap();
    let journal_dir = temp_root("non-utf8-root-journal");
    let backup_dir = temp_root("non-utf8-root-backup");

    let entry = install_new_entry(
        &catalogue_root,
        &destination_root,
        "Frogger",
        b"cheats = 0\n",
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-27",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::InstalledNew);
    assert!(result.applied);

    let destination = result.destination_path.as_ref().unwrap();
    assert!(destination.lossy);
    assert!(destination.display.contains('\u{FFFD}'));

    // The real installed file exists at the real (non-UTF-8) path, even
    // though its JSON-safe representation is necessarily lossy.
    let real_installed_path = destination_root.join("Atari2600").join("Frogger.cht");
    assert_eq!(fs::read(&real_installed_path).unwrap(), b"cheats = 0\n");

    // A round-trip through the run's own JSON must not panic and must
    // still mark the path lossy - never silently "fix" it into something
    // that looks like a real, reversible path.
    let json = serde_json::to_string(&outcome.run).unwrap();
    assert!(json.contains("\u{FFFD}") || json.contains("\\ufffd"));
    let round_tripped: super::super::cheat_install_result::CheatInstallRun =
        serde_json::from_str(&json).unwrap();
    assert!(
        round_tripped.entries[0]
            .destination_path
            .as_ref()
            .unwrap()
            .lossy
    );
}

#[test]
fn lossy_source_path_is_rejected_rather_than_misread() {
    let catalogue_root = temp_root("lossy-source-catalogue");
    let destination_root = temp_root("lossy-source-dest");
    let journal_dir = temp_root("lossy-source-journal");
    let backup_dir = temp_root("lossy-source-backup");

    // A real file exists, but the record's own `source_file_path` claims
    // to be lossy (as if it had been round-tripped through a lossy
    // representation upstream) - the installer must refuse to guess at
    // the real path rather than silently opening whatever the lossy
    // display string happens to point at.
    let (mut source_path, hash) = write_source(&catalogue_root, "Frogger.cht", b"cheats = 0\n");
    source_path.lossy = true;

    let entry = build_entry(
        "Frogger",
        "Atari - 2600",
        source_path,
        hash,
        CheatMatchConfidence::Strong,
        CheatStagingAction::InstallNew,
        "destination_missing",
        &destination_root,
        None,
    );
    let opts = options(
        &destination_root,
        &journal_dir,
        &backup_dir,
        false,
        true,
        false,
        "run-28",
    );

    let outcome = execute_cheat_install_run(&[entry], &opts);
    let result = &outcome.run.entries[0];
    assert_eq!(result.outcome, CheatInstallOutcome::SkippedSourceChanged);
    assert_eq!(result.reason_code, "source_path_lossy_cannot_revalidate");
    assert!(!destination_root.join("Atari2600").exists());
}

// ---------------------------------------------------------------------
// Architectural guardrail: no live HOME/database/RetroArch access
// ---------------------------------------------------------------------

#[test]
fn module_source_never_touches_live_home_or_database() {
    let source = include_str!("../cheat_installer.rs");
    for forbidden in [
        "env::var(\"HOME\")",
        "env::var(\"XDG_",
        "std::env::home_dir",
        "Database::open",
        "Database::create",
        "default_database_path",
        "ureq::",
        "reqwest",
        "TcpStream",
    ] {
        assert!(
            !source.contains(forbidden),
            "cheat_installer.rs must never reference `{forbidden}` - every path/root/journal \
             location must be supplied explicitly via `CheatInstallOptions`"
        );
    }
}
