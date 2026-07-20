use super::*;
use crate::patch_manager::{
    CHEAT_INSTALL_RUN_SCHEMA_VERSION, CHEAT_ROLLBACK_RUN_SCHEMA_VERSION, CheatInstallRunStatus,
    CheatRollbackEntryResult, CheatRollbackSummary,
};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    journals: PathBuf,
    backups: PathBuf,
    rollbacks: PathBuf,
    destinations: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "archivefs-cheat-history-{label}-{}-{sequence}",
            std::process::id()
        ));
        let journals = root.join("cheat-install-runs");
        let backups = root.join("cheat-install-backups");
        let rollbacks = root.join("cheat-rollback-runs");
        let destinations = root.join("cheats");
        for path in [&journals, &backups, &rollbacks, &destinations] {
            fs::create_dir_all(path).unwrap();
        }
        Self {
            root,
            journals,
            backups,
            rollbacks,
            destinations,
        }
    }

    fn options(&self) -> CheatHistoryOptions {
        CheatHistoryOptions {
            journal_root: self.journals.clone(),
            backup_root: self.backups.clone(),
            rollback_journal_root: self.rollbacks.clone(),
        }
    }

    fn destination(&self, name: &str) -> PathBuf {
        self.destinations.join("NES").join(name)
    }

    fn write_install(&self, name: &str, run: &CheatInstallRun) -> PathBuf {
        let path = self.journals.join(name);
        fs::write(&path, serde_json::to_vec_pretty(run).unwrap()).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn installed_entry(fixture: &Fixture, name: &str, bytes: &[u8]) -> CheatInstallEntryResult {
    let destination = fixture.destination(name);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, bytes).unwrap();
    let installed_hash = hash(bytes);
    CheatInstallEntryResult {
        source_path: CheatInstallPath::from_path(Path::new(name)),
        expected_source_hash: Some(installed_hash.clone()),
        observed_source_hash: Some(installed_hash.clone()),
        destination_path: Some(CheatInstallPath::from_path(&destination)),
        previous_destination_state:
            super::super::cheat_install_result::PreviousDestinationState::Absent,
        previous_destination_hash: None,
        backup_path: None,
        resulting_destination_hash: Some(installed_hash),
        outcome: CheatInstallOutcome::InstalledNew,
        reason_code: "installed_new".into(),
        detail: Vec::new(),
        applied: true,
        eligible: true,
        write_required: true,
    }
}

fn replaced_entry(
    fixture: &Fixture,
    name: &str,
    old: &[u8],
    new: &[u8],
) -> CheatInstallEntryResult {
    let mut entry = installed_entry(fixture, name, new);
    let backup = fixture.backups.join(format!("{name}.backup"));
    fs::write(&backup, old).unwrap();
    entry.previous_destination_state =
        super::super::cheat_install_result::PreviousDestinationState::PresentDifferent;
    entry.previous_destination_hash = Some(hash(old));
    entry.backup_path = Some(CheatInstallPath::from_path(&backup));
    entry.outcome = CheatInstallOutcome::ReplacedWithBackup;
    entry.reason_code = "replaced_with_backup".into();
    entry
}

fn install_run(
    fixture: &Fixture,
    id: &str,
    timestamp: u64,
    entries: Vec<CheatInstallEntryResult>,
) -> CheatInstallRun {
    let summary = CheatInstallSummary::from_entries(&entries, false);
    CheatInstallRun {
        schema_version: CHEAT_INSTALL_RUN_SCHEMA_VERSION,
        run_id: id.into(),
        started_at_unix_seconds: timestamp,
        completed_at_unix_seconds: Some(timestamp + 1),
        dry_run: false,
        allow_replace_different: true,
        destination_root: Some(CheatInstallPath::from_path(&fixture.destinations)),
        catalogue_source: "fixture catalogue".into(),
        entries,
        summary,
        status: CheatInstallRunStatus::derive(&summary, false),
    }
}

fn completed_rollback(
    fixture: &Fixture,
    install_path: &Path,
    install: &CheatInstallRun,
    id: &str,
) -> CheatRollbackRun {
    let entries = install
        .entries
        .iter()
        .map(|entry| CheatRollbackEntryResult {
            original_outcome: entry.outcome,
            destination_path: entry.destination_path.clone(),
            expected_installed_hash: entry
                .resulting_destination_hash
                .clone()
                .or_else(|| entry.expected_source_hash.clone()),
            expected_previous_hash: entry.previous_destination_hash.clone(),
            observed_current_hash: entry.resulting_destination_hash.clone(),
            backup_path: entry.backup_path.clone(),
            outcome: if entry.outcome == CheatInstallOutcome::InstalledNew {
                CheatRollbackOutcome::RemovedInstalledFile
            } else {
                CheatRollbackOutcome::RestoredBackup
            },
            wrote: true,
            error_code: None,
            message: String::new(),
            retryable: false,
        })
        .collect::<Vec<_>>();
    let summary = CheatRollbackSummary::from_entries(&entries);
    CheatRollbackRun {
        schema_version: CHEAT_ROLLBACK_RUN_SCHEMA_VERSION,
        run_id: id.into(),
        original_install_run_id: install.run_id.clone(),
        original_journal_path: CheatInstallPath::from_path(install_path),
        started_at_unix_seconds: 30,
        completed_at_unix_seconds: Some(31),
        dry_run: false,
        confirmed: true,
        destination_root: CheatInstallPath::from_path(&fixture.destinations),
        entries,
        summary,
        status: CheatRollbackRunStatus::derive(&summary, false),
        rollback_journal_path: None,
        journal_write_error: None,
    }
}

#[test]
fn missing_history_root_is_successfully_empty_and_not_created() {
    let fixture = Fixture::new("missing-root");
    let root = fixture.root.join("absent");
    let report = discover_cheat_history(&CheatHistoryOptions {
        journal_root: root.clone(),
        ..fixture.options()
    });
    assert!(report.entries.is_empty());
    assert!(report.warnings.is_empty());
    assert!(!root.exists());
}

#[test]
fn valid_installed_new_reports_unchanged_and_available() {
    let fixture = Fixture::new("installed");
    let run = install_run(
        &fixture,
        "run-installed",
        10,
        vec![installed_entry(&fixture, "Mario.cht", b"new")],
    );
    let path = fixture.write_install("run-installed.json", &run);
    let inspection = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    let entry = &inspection.entries[0];
    assert_eq!(entry.platform.as_deref(), Some("NES"));
    assert_eq!(entry.display_title.as_deref(), Some("Mario"));
    assert_eq!(
        entry.destination,
        CheatDestinationAssessment::UnchangedSinceInstall
    );
    assert_eq!(
        entry.destination_observed_hash.as_deref(),
        entry.installed_hash.as_deref()
    );
    assert_eq!(entry.backup, CheatBackupAssessment::NotApplicable);
    assert_eq!(
        entry.rollback_availability,
        CheatRollbackAvailability::Available
    );
}

#[test]
fn destination_missing_changed_and_symlink_are_distinguished() {
    let fixture = Fixture::new("destination-states");
    let run = install_run(
        &fixture,
        "run-destinations",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"installed")],
    );
    let path = fixture.write_install("run.json", &run);
    fs::write(fixture.destination("Game.cht"), b"changed").unwrap();
    let changed = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(
        changed.entries[0].destination,
        CheatDestinationAssessment::Changed
    );
    assert_eq!(
        changed.entries[0].rollback_availability,
        CheatRollbackAvailability::BlockedDestinationChanged
    );
    fs::remove_file(fixture.destination("Game.cht")).unwrap();
    let missing = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(
        missing.entries[0].destination,
        CheatDestinationAssessment::Missing
    );
    assert_eq!(
        missing.entries[0].rollback_availability,
        CheatRollbackAvailability::Unnecessary
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("elsewhere", fixture.destination("Game.cht")).unwrap();
        let unsafe_result = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
        assert_eq!(
            unsafe_result.entries[0].destination,
            CheatDestinationAssessment::UnsafePath
        );
        assert_eq!(
            unsafe_result.entries[0].rollback_availability,
            CheatRollbackAvailability::BlockedUnsafePath
        );
    }
}

#[test]
fn replacement_backup_valid_missing_changed_and_symlink_are_distinguished() {
    let fixture = Fixture::new("backup-states");
    let entry = replaced_entry(&fixture, "Zelda.cht", b"old", b"new");
    let backup = PathBuf::from(&entry.backup_path.as_ref().unwrap().display);
    let run = install_run(&fixture, "run-replaced", 10, vec![entry]);
    let path = fixture.write_install("run.json", &run);
    let valid = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(
        valid.entries[0].backup,
        CheatBackupAssessment::PresentAndValid
    );
    assert_eq!(
        valid.entries[0].rollback_availability,
        CheatRollbackAvailability::Available
    );
    fs::write(fixture.destination("Zelda.cht"), b"old").unwrap();
    let already_restored = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(
        already_restored.entries[0].destination,
        CheatDestinationAssessment::Changed
    );
    assert_eq!(
        already_restored.entries[0].rollback_availability,
        CheatRollbackAvailability::Unnecessary
    );
    fs::write(fixture.destination("Zelda.cht"), b"new").unwrap();
    fs::write(&backup, b"tampered").unwrap();
    let changed = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(changed.entries[0].backup, CheatBackupAssessment::Changed);
    assert_eq!(
        changed.entries[0].rollback_availability,
        CheatRollbackAvailability::BlockedBackupChanged
    );
    fs::remove_file(&backup).unwrap();
    let missing = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(missing.entries[0].backup, CheatBackupAssessment::Missing);
    assert_eq!(
        missing.entries[0].rollback_availability,
        CheatRollbackAvailability::BlockedMissingBackup
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing-target", &backup).unwrap();
        let unsafe_result = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
        assert_eq!(
            unsafe_result.entries[0].backup,
            CheatBackupAssessment::UnsafePath
        );
    }
}

#[test]
fn history_sorts_newest_first_with_path_fallback() {
    let fixture = Fixture::new("sorting");
    for (file, id, timestamp) in [("c.json", "c", 0), ("b.json", "b", 20), ("a.json", "a", 20)] {
        let entry = installed_entry(&fixture, &format!("{id}.cht"), id.as_bytes());
        fixture.write_install(file, &install_run(&fixture, id, timestamp, vec![entry]));
    }
    let report = discover_cheat_history(&fixture.options());
    assert_eq!(
        report
            .entries
            .iter()
            .map(|entry| entry.run_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}

#[test]
fn malformed_and_unsupported_journals_are_skipped_without_crashing_history() {
    let fixture = Fixture::new("invalid-json");
    let malformed_path = fixture.journals.join("malformed.json");
    fs::write(&malformed_path, b"{not json").unwrap();
    let run = install_run(
        &fixture,
        "future",
        10,
        vec![installed_entry(&fixture, "Future.cht", b"new")],
    );
    let future = serde_json::to_string_pretty(&run).unwrap().replacen(
        "\"schema_version\": 1",
        "\"schema_version\": 999",
        1,
    );
    let future_path = fixture.journals.join("future.json");
    fs::write(&future_path, future).unwrap();
    assert_eq!(
        inspect_cheat_install_journal(&malformed_path, &fixture.options())
            .unwrap_err()
            .code,
        "malformed_journal"
    );
    assert_eq!(
        inspect_cheat_install_journal(&future_path, &fixture.options())
            .unwrap_err()
            .code,
        "unsupported_journal_version"
    );
    let report = discover_cheat_history(&fixture.options());
    assert!(report.entries.is_empty());
    assert_eq!(report.warnings.len(), 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.code == "malformed_journal")
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.code == "unsupported_journal_version")
    );
}

#[test]
fn non_journal_files_are_ignored() {
    let fixture = Fixture::new("ignored-files");
    fs::write(fixture.journals.join("notes.txt"), b"not a journal").unwrap();
    let report = discover_cheat_history(&fixture.options());
    assert!(report.entries.is_empty());
    assert!(report.warnings.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_journal_filename_is_preserved_as_lossy_metadata() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let fixture = Fixture::new("non-utf8");
    let run = install_run(
        &fixture,
        "non-utf8",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let mut name = std::ffi::OsString::from_vec(b"run-\xff.json".to_vec());
    let path = fixture.journals.join(&name);
    fs::write(&path, serde_json::to_vec_pretty(&run).unwrap()).unwrap();
    name.clear();
    let report = discover_cheat_history(&fixture.options());
    assert_eq!(report.entries.len(), 1);
    assert!(report.entries[0].journal_path.lossy);
    assert_eq!(
        report.entries[0].journal_path.raw_bytes.as_deref(),
        Some(path.as_os_str().as_bytes())
    );
}

#[cfg(unix)]
#[test]
fn journal_and_root_symlinks_are_rejected() {
    let fixture = Fixture::new("journal-symlink");
    let run = install_run(
        &fixture,
        "run",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let real = fixture.write_install("real.json", &run);
    let link = fixture.journals.join("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let report = discover_cheat_history(&fixture.options());
    assert_eq!(report.entries.len(), 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.code == "unsafe_journal_path")
    );

    let root_link = fixture.root.join("journal-root-link");
    std::os::unix::fs::symlink(&fixture.journals, &root_link).unwrap();
    let linked = discover_cheat_history(&CheatHistoryOptions {
        journal_root: root_link,
        ..fixture.options()
    });
    assert!(linked.entries.is_empty());
    assert_eq!(linked.warnings[0].code, "unsafe_journal_root");
}

#[test]
fn inspect_rejects_outside_traversal_and_destination_root_mismatch() {
    let fixture = Fixture::new("path-binding");
    let mut run = install_run(
        &fixture,
        "run",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let outside = fixture.root.join("outside.json");
    fs::write(&outside, serde_json::to_vec_pretty(&run).unwrap()).unwrap();
    assert_eq!(
        inspect_cheat_install_journal(&outside, &fixture.options())
            .unwrap_err()
            .code,
        "journal_outside_root"
    );
    run.entries[0].destination_path = Some(CheatInstallPath {
        display: fixture
            .destinations
            .join("../escape.cht")
            .display()
            .to_string(),
        lossy: false,
    });
    let path = fixture.write_install("traversal.json", &run);
    assert_eq!(
        inspect_cheat_install_journal(&path, &fixture.options())
            .unwrap_err()
            .code,
        "invalid_destination_path"
    );
}

#[cfg(unix)]
#[test]
fn unsafe_destination_and_backup_parent_symlinks_are_never_hashed() {
    let fixture = Fixture::new("parent-symlinks");
    let entry = replaced_entry(&fixture, "Game.cht", b"old", b"new");
    let run = install_run(&fixture, "run", 10, vec![entry]);
    let path = fixture.write_install("run.json", &run);
    fs::remove_dir_all(fixture.destinations.join("NES")).unwrap();
    std::os::unix::fs::symlink(&fixture.backups, fixture.destinations.join("NES")).unwrap();
    let inspection = inspect_cheat_install_journal(&path, &fixture.options()).unwrap();
    assert_eq!(
        inspection.entries[0].destination,
        CheatDestinationAssessment::UnsafePath
    );

    let backup_link = fixture.root.join("backup-root-link");
    std::os::unix::fs::symlink(&fixture.backups, &backup_link).unwrap();
    let unsafe_backup = inspect_cheat_install_journal(
        &path,
        &CheatHistoryOptions {
            backup_root: backup_link,
            ..fixture.options()
        },
    );
    assert_eq!(unsafe_backup.unwrap_err().code, "invalid_backup_path");
}

#[test]
fn completed_rollback_requires_strong_binding() {
    let fixture = Fixture::new("rollback-match");
    let run = install_run(
        &fixture,
        "install-run",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let install_path = fixture.write_install("install.json", &run);
    let rollback = completed_rollback(&fixture, &install_path, &run, "rollback-run");
    fs::write(
        fixture.rollbacks.join("rollback.json"),
        serde_json::to_vec_pretty(&rollback).unwrap(),
    )
    .unwrap();
    let inspection = inspect_cheat_install_journal(&install_path, &fixture.options()).unwrap();
    assert_eq!(inspection.rollback.completed_successfully, Some(true));
    assert_eq!(
        inspection.entries[0].rollback_availability,
        CheatRollbackAvailability::AlreadyCompleted
    );
}

#[test]
fn unrelated_and_ambiguous_rollbacks_never_claim_completion() {
    let fixture = Fixture::new("rollback-ambiguity");
    let run = install_run(
        &fixture,
        "install-run",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let install_path = fixture.write_install("install.json", &run);
    let mut unrelated = completed_rollback(&fixture, &install_path, &run, "unrelated");
    unrelated.original_install_run_id = "someone-else".into();
    fs::write(
        fixture.rollbacks.join("unrelated.json"),
        serde_json::to_vec_pretty(&unrelated).unwrap(),
    )
    .unwrap();
    let absent = inspect_cheat_install_journal(&install_path, &fixture.options()).unwrap();
    assert!(!absent.rollback.exists);

    for id in ["rollback-a", "rollback-b"] {
        let rollback = completed_rollback(&fixture, &install_path, &run, id);
        fs::write(
            fixture.rollbacks.join(format!("{id}.json")),
            serde_json::to_vec_pretty(&rollback).unwrap(),
        )
        .unwrap();
    }
    let ambiguous = inspect_cheat_install_journal(&install_path, &fixture.options()).unwrap();
    assert!(ambiguous.rollback.ambiguous);
    assert_eq!(ambiguous.rollback.completed_successfully, None);
    assert_ne!(
        ambiguous.entries[0].rollback_availability,
        CheatRollbackAvailability::AlreadyCompleted
    );
    assert_eq!(
        ambiguous.entries[0].rollback_availability,
        CheatRollbackAvailability::Unknown
    );
}

#[test]
fn inspection_does_not_modify_journals_destinations_or_backups() {
    let fixture = Fixture::new("read-only");
    let entry = replaced_entry(&fixture, "Game.cht", b"old", b"new");
    let backup = PathBuf::from(&entry.backup_path.as_ref().unwrap().display);
    let destination = fixture.destination("Game.cht");
    let run = install_run(&fixture, "run", 10, vec![entry]);
    let journal = fixture.write_install("run.json", &run);
    let before = [
        fs::read(&journal).unwrap(),
        fs::read(&destination).unwrap(),
        fs::read(&backup).unwrap(),
    ];
    inspect_cheat_install_journal(&journal, &fixture.options()).unwrap();
    let after = [
        fs::read(&journal).unwrap(),
        fs::read(&destination).unwrap(),
        fs::read(&backup).unwrap(),
    ];
    assert_eq!(before, after);
}

#[cfg(unix)]
#[test]
fn inaccessible_journal_is_reported_by_history_and_fails_inspect() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("inaccessible");
    let run = install_run(
        &fixture,
        "run",
        10,
        vec![installed_entry(&fixture, "Game.cht", b"new")],
    );
    let path = fixture.write_install("run.json", &run);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let inspected = inspect_cheat_install_journal(&path, &fixture.options());
    let report = discover_cheat_history(&fixture.options());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(inspected.unwrap_err().code, "journal_inaccessible");
    assert!(report.entries.is_empty());
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.code == "journal_inaccessible")
    );
}
