//! Doctor Stage 1A: shared finding model, adapters, and the read-only
//! runner.
//!
//! The no-mutation guarantee is tested two ways, deliberately:
//!
//! - **Structurally**, by reading `runner.rs`'s own source and asserting it
//!   mentions no filesystem, process, or network API at all. A pure
//!   function cannot mutate anything, so this is a stronger statement than
//!   any before/after comparison can make.
//! - **Behaviourally**, by snapshotting a real temporary directory tree
//!   (names, sizes, modification times) plus a real SQLite catalogue around
//!   the gatherers that do touch the filesystem, and asserting the snapshot
//!   is identical afterwards.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::database::{
    DatabaseCheckOutcome, DatabaseCheckStatus, DatabaseDiagnostic, DatabaseDiagnosticCode,
    DatabaseDiagnosticSeverity, DatabaseHealthReport, DatabaseOpenOutcome, diagnose_database,
};
use crate::emulator_environment::retroarch::{
    DiagnosticCategory, DiagnosticSeverity, ProfileKind, ProfileRef, ProfileScope,
};
use crate::patch_manager::{
    PreviewAdapter, SharedApplyContext, SharedApplyFailure, SharedApplyFailureKind,
    SharedApplyJournal, SharedApplyStatus, SharedJournalWarning, SharedTransactionPath,
};
use crate::{
    ArchiveHealth, ConfigIdentity, DoctorCheck, DoctorStatus, MountState, SetupDiagnostic,
};

// --- Fixtures -------------------------------------------------------------

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-doctor-stage1a-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp tree");
        Self { root }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Every entry beneath `root`, with the facts a mutation would disturb.
/// Sorted, so comparison is order-independent.
fn snapshot_tree(root: &Path) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&current) else {
            continue;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            entries.insert(
                relative,
                format!(
                    "dir={} symlink={} len={} modified={}",
                    metadata.is_dir(),
                    metadata.file_type().is_symlink(),
                    metadata.len(),
                    modified
                ),
            );
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                stack.push(path);
            }
        }
    }
    entries
}

fn doctor_report(checks: Vec<DoctorCheck>) -> DoctorReport {
    DoctorReport {
        config_path: PathBuf::from("/home/tester/.config/archivefs/config.toml"),
        checks,
        archives_found: 3,
        archives_with_platform: 2,
        archives_unknown_platform: 1,
        unknown_platform_examples: vec![PathBuf::from("/roms/mystery.zip")],
        platform_counts: vec![("SNES".to_string(), 2)],
        pending_archives: 1,
        mounted_archives: 2,
    }
}

fn check(name: &str, status: DoctorStatus, detail: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status,
        detail: detail.to_string(),
    }
}

fn setup_diagnostics(checks: Vec<SetupDiagnostic>) -> SetupDiagnostics {
    SetupDiagnostics {
        config_path: Some(PathBuf::from("/home/tester/.config/archivefs/config.toml")),
        config_path_error: None,
        config_missing: false,
        mount_root: Some(PathBuf::from("/home/tester/mnt/archivefs")),
        can_create_mount_root: true,
        ready_for_scanning: true,
        ready_for_actions: false,
        config_identity: ConfigIdentity {
            config_path: Some(PathBuf::from("/home/tester/.config/archivefs/config.toml")),
            content_digest: None,
        },
        checks,
    }
}

fn setup_check(
    name: &str,
    status: SetupDiagnosticStatus,
    detail: &str,
    why: &str,
    next: &str,
) -> SetupDiagnostic {
    SetupDiagnostic {
        name: name.to_string(),
        status,
        detail: detail.to_string(),
        why_it_matters: why.to_string(),
        next_step: next.to_string(),
    }
}

fn health_issue(path: &str, category: HealthCategory) -> HealthIssue {
    HealthIssue {
        path: PathBuf::from(path),
        platform: Some("SNES".to_string()),
        present: category != HealthCategory::Missing,
        mount_state: Some(MountState::Pending),
        category,
        reason: format!("reason for {}", category.label()),
        retryable: category.is_retryable(),
        recovery_action: match category {
            HealthCategory::RetryableFailure => Some(RecoveryAction::RetryMount),
            HealthCategory::RecoveryAvailable => Some(RecoveryAction::Remount),
            _ => None,
        },
        last_seen_at: Some("2026-07-31T00:00:00Z".to_string()),
        size_bytes: Some(4096),
        modified_time_unix_seconds: Some(1_700_000_000),
    }
}

fn database_report(diagnostics: Vec<DatabaseDiagnostic>) -> DatabaseHealthReport {
    DatabaseHealthReport {
        format_version: 1,
        database_path: EncodedPath::from_path(Path::new(
            "/home/tester/.local/share/archivefs/catalogue.sqlite3",
        )),
        database_present: true,
        main_file: None,
        sidecars: Vec::new(),
        open_outcome: DatabaseOpenOutcome::OpenedReadOnly,
        journal_mode: Some("wal".to_string()),
        quick_check: DatabaseCheckOutcome {
            status: DatabaseCheckStatus::Ok,
            messages: Vec::new(),
        },
        integrity_check: DatabaseCheckOutcome {
            status: DatabaseCheckStatus::Ok,
            messages: Vec::new(),
        },
        schema_version: Some(5),
        diagnostics,
    }
}

fn database_diagnostic(
    code: DatabaseDiagnosticCode,
    severity: DatabaseDiagnosticSeverity,
) -> DatabaseDiagnostic {
    DatabaseDiagnostic {
        code,
        severity,
        message: format!("{code:?} observed"),
        sqlite_extended_code: Some(11),
        raw_sqlite_message: Some("database disk image is malformed".to_string()),
    }
}

fn transaction_history(status: SharedApplyStatus) -> SharedHistoryReport {
    SharedHistoryReport {
        journals: vec![(
            SharedTransactionPath::from_path(Path::new("/history/op-1.json")),
            SharedApplyJournal {
                schema_version: 1,
                operation_id: "op-1".to_string(),
                plan_id: "plan-1".to_string(),
                timestamp_unix_seconds: 1_700_000_500,
                context: SharedApplyContext {
                    adapter: PreviewAdapter::Dolphin,
                    selected_archive: SharedTransactionPath::from_path(Path::new("/roms/game.rvz")),
                    verified_game_identity: "GLME01".to_string(),
                    profile_id: "profile-1".to_string(),
                    source_mode: "provider".to_string(),
                },
                approved_source_root: SharedTransactionPath::from_path(Path::new("/staging")),
                destination_root: SharedTransactionPath::from_path(Path::new("/dolphin")),
                dry_run: false,
                entries: Vec::new(),
                status,
                rollback_operation_id: None,
            },
        )],
        warnings: Vec::new(),
        complete: true,
    }
}

fn retroarch_report(diagnostics: Vec<RetroArchDiagnostic>) -> RetroArchEnvironmentReport {
    RetroArchEnvironmentReport {
        format_version: 1,
        profiles: Vec::new(),
        diagnostics,
    }
}

fn retroarch_diagnostic(
    code: &'static str,
    severity: DiagnosticSeverity,
    detail_kind: DiagnosticCategory,
) -> RetroArchDiagnostic {
    RetroArchDiagnostic {
        code,
        severity,
        detail_kind,
        profile: Some(ProfileRef {
            profile_kind: ProfileKind::Flatpak,
            scope: ProfileScope::User,
        }),
        purpose: None,
        path: Some(EncodedPath::from_path(Path::new(
            "/var/lib/retroarch/cores",
        ))),
        entry_index: None,
    }
}

/// Inputs with only the named subsystem populated, so an adapter can be
/// exercised in isolation.
macro_rules! only {
    ($field:ident = $value:expr) => {{
        let mut inputs = DoctorScanInputs::none_loaded();
        inputs.$field = Gathered::Ready($value);
        inputs
    }};
}

// --- 1-6. The read-only contract -----------------------------------------

/// The strongest form of "performs no mutation": the runner's source has no
/// filesystem, process, or network API in it at all. A pure function cannot
/// create a directory, scan archives, refresh an application, or reach the
/// network.
#[test]
fn runner_source_contains_no_io_or_mutation_calls() {
    let source = include_str!("runner.rs");
    // Strip documentation and comments, which legitimately *name* these
    // APIs in prose while explaining why the code must not use them.
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "fs::",
        "File::",
        "OpenOptions",
        "Command",
        "ureq",
        "reqwest",
        "TcpStream",
        "create_dir",
        "remove_file",
        "remove_dir",
        "std::io",
        "scan_archives",
        "refresh",
        "run_doctor(",
        "Database::open",
        "diagnose_database(",
        "discover_retroarch_environment(",
        "validate_destination_root(",
        "discover_shared_apply_history(",
    ] {
        assert!(
            !code.contains(forbidden),
            "runner.rs must stay pure but mentions `{forbidden}`; \
             move that work into a gatherer in diagnostics/mod.rs"
        );
    }
}

/// The mount-root gatherer is proven non-mutating against a real tree
/// containing a file, a nested directory, and a symlink.
#[test]
fn the_mount_root_gatherer_mutates_nothing() {
    let tree = TempTree::new("gatherer");
    let mount_root = tree.root.join("mnt/archivefs");
    fs::create_dir_all(mount_root.join("SNES")).expect("nested dirs");
    fs::write(mount_root.join("SNES/keep.txt"), b"user data").expect("file");
    #[cfg(unix)]
    std::os::unix::fs::symlink(mount_root.join("SNES"), mount_root.join("link")).expect("symlink");

    let before = snapshot_tree(&tree.root);
    let safety = assess_mount_root_safety(&mount_root);
    let scan = run_doctor_scan(&only!(mount_root_safety = &safety));
    let after = snapshot_tree(&tree.root);

    assert_eq!(before, after, "the gatherer or the runner changed the tree");
    assert!(!before.is_empty(), "the fixture tree must not be empty");
    assert!(safety.outcome.is_ok(), "{:?}", safety.outcome);
    assert!(scan.is_healthy(), "{:?}", scan.findings);
}

/// A missing mount root must not be created - the exact trap `run_doctor`
/// (as opposed to `run_doctor_read_only`) falls into.
#[test]
fn a_missing_mount_root_is_never_created_by_a_doctor_scan() {
    let tree = TempTree::new("absent-root");
    let mount_root = tree.root.join("does/not/exist");
    assert!(!mount_root.exists());

    let safety = assess_mount_root_safety(&mount_root);
    let scan = run_doctor_scan(&only!(mount_root_safety = &safety));

    assert!(
        !mount_root.exists(),
        "Doctor created the mount root; it must only ever report on it"
    );
    assert!(!tree.root.join("does").exists(), "no ancestor was created");
    // An absent root is already reported by DoctorReport/SetupDiagnostics,
    // so destination safety deliberately stays quiet about it.
    assert!(scan.findings.is_empty(), "{:?}", scan.findings);
    assert_eq!(
        safety.outcome.expect("absent is not a failure"),
        DestinationRootState::Absent
    );
}

/// A real SQLite catalogue is untouched by the database gatherer - no WAL
/// checkpoint, no migration, no recovery.
#[test]
fn the_database_gatherer_mutates_nothing() {
    let tree = TempTree::new("database");
    let database_path = tree.root.join("catalogue.sqlite3");
    {
        let database = crate::Database::open_or_create(&database_path).expect("create catalogue");
        let _ = database.load_archives().expect("load");
    }
    let before = snapshot_tree(&tree.root);
    let report = diagnose_database(&database_path);
    let scan = run_doctor_scan(&only!(database = &report));
    let after = snapshot_tree(&tree.root);

    assert_eq!(before, after, "diagnose_database changed the catalogue");
    assert!(report.database_present);
    assert_eq!(
        scan.count(DoctorSeverity::Critical),
        0,
        "{:?}",
        scan.findings
    );
}

/// Running the scan repeatedly changes nothing about the inputs and yields
/// the same answer - the runner holds no state and writes nothing back.
#[test]
fn repeated_scans_leave_inputs_and_results_unchanged() {
    let report = doctor_report(vec![check(
        "mount root writable",
        DoctorStatus::Fail,
        "/mnt/archivefs exists but is not writable by the current user",
    )]);
    let before = format!("{report:?}");
    let first = run_doctor_scan(&only!(doctor_report = &report));
    let second = run_doctor_scan(&only!(doctor_report = &report));
    let third = run_doctor_scan(&only!(doctor_report = &report));

    assert_eq!(before, format!("{report:?}"), "input was mutated");
    assert_eq!(first, second);
    assert_eq!(second, third);
}

// --- 7, 8. Determinism and stable ids ------------------------------------

#[test]
fn findings_are_ordered_by_severity_then_category_then_resource_then_id() {
    let issues = vec![
        health_issue("/roms/z-unknown.zip", HealthCategory::UnknownPlatform),
        health_issue("/roms/a-missing.zip", HealthCategory::Missing),
        health_issue("/roms/m-terminal.zip", HealthCategory::TerminalFailure),
        health_issue("/roms/b-missing.zip", HealthCategory::Missing),
    ];
    let scan = run_doctor_scan(&only!(health_issues = issues.as_slice()));

    let order: Vec<(&str, &str)> = scan
        .findings
        .iter()
        .map(|finding| {
            (
                finding.id.as_str(),
                finding
                    .affected
                    .as_ref()
                    .map(|path| path.display.as_str())
                    .unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("mounts.terminal_failure", "/roms/m-terminal.zip"),
            ("library.archive_missing", "/roms/a-missing.zip"),
            ("library.archive_missing", "/roms/b-missing.zip"),
            ("library.unknown_platform", "/roms/z-unknown.zip"),
        ]
    );
}

#[test]
fn the_same_inputs_always_produce_byte_identical_scans() {
    let issues = vec![
        health_issue("/roms/b.zip", HealthCategory::Missing),
        health_issue("/roms/a.zip", HealthCategory::CachedOnly),
    ];
    let sources = vec![SourceHealthIssue {
        path: PathBuf::from("/roms"),
        availability: SourceAvailability::Unavailable,
        reason: "Source unavailable.".to_string(),
        last_scan_error: None,
        archives_preserved: 12,
    }];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.health_issues = Gathered::Ready(issues.as_slice());
    inputs.source_health = Gathered::Ready(sources.as_slice());

    let reference = serde_json::to_string(&run_doctor_scan(&inputs)).expect("json");
    for _ in 0..10 {
        assert_eq!(
            serde_json::to_string(&run_doctor_scan(&inputs)).expect("json"),
            reference
        );
    }
}

#[test]
fn finding_ids_are_stable_and_namespaced_by_category() {
    let cases = [
        (HealthCategory::TerminalFailure, "mounts.terminal_failure"),
        (HealthCategory::RetryableFailure, "mounts.retryable_failure"),
        (
            HealthCategory::RecoveryAvailable,
            "mounts.recovery_available",
        ),
        (HealthCategory::Missing, "library.archive_missing"),
        (
            HealthCategory::AwaitingValidation,
            "library.awaiting_validation",
        ),
        (HealthCategory::CachedOnly, "library.cached_only"),
        (HealthCategory::UnknownPlatform, "library.unknown_platform"),
    ];
    for (category, expected) in cases {
        let issues = vec![health_issue("/roms/a.zip", category)];
        let findings = findings_from_health_issues(&issues);
        assert_eq!(findings[0].id, expected, "id changed for {category:?}");
    }
    // Doctor-report ids derive from the check name, not from its wording,
    // so rewording `detail` cannot change an id.
    let first = findings_from_doctor_report(&doctor_report(vec![check(
        "mount root writable",
        DoctorStatus::Fail,
        "first wording",
    )]));
    let second = findings_from_doctor_report(&doctor_report(vec![check(
        "mount root writable",
        DoctorStatus::Fail,
        "completely different wording",
    )]));
    assert_eq!(first[0].id, "mount_root.mount_root_writable");
    assert_eq!(first[0].id, second[0].id);
}

// --- 9, 10, 11. Duplicate suppression ------------------------------------

#[test]
fn identical_ids_about_the_same_resource_merge_and_keep_all_evidence() {
    let mut older = health_issue("/roms/a.zip", HealthCategory::Missing);
    older.last_seen_at = Some("2020-01-01T00:00:00Z".to_string());
    let newer = health_issue("/roms/a.zip", HealthCategory::Missing);

    let issues = vec![older, newer];
    let scan = run_doctor_scan(&only!(health_issues = issues.as_slice()));

    assert_eq!(scan.merged_duplicate_count, 1);
    assert_eq!(scan.findings.len(), 1);
    for expected in ["2020-01-01", "2026-07-31"] {
        assert!(
            scan.findings[0]
                .evidence
                .iter()
                .any(|item| item.contains(expected)),
            "evidence from both reports must survive: {:?}",
            scan.findings[0].evidence
        );
    }
}

#[test]
fn the_same_id_about_different_resources_is_never_merged() {
    let issues = vec![
        health_issue("/roms/a.zip", HealthCategory::Missing),
        health_issue("/roms/b.zip", HealthCategory::Missing),
    ];
    let scan = run_doctor_scan(&only!(health_issues = issues.as_slice()));
    assert_eq!(scan.merged_duplicate_count, 0);
    assert_eq!(scan.findings.len(), 2);
}

#[test]
fn distinct_faults_are_never_merged_merely_because_the_wording_is_similar() {
    // Two reports describing the *same* unwritable mount root, but each
    // under its own check name and therefore its own stable id. Both must
    // survive: suppression keys on identity, not on prose.
    let report = doctor_report(vec![check(
        "mount root writable",
        DoctorStatus::Warn,
        "not writable",
    )]);
    let setup = setup_diagnostics(vec![setup_check(
        "Mount root is writable",
        SetupDiagnosticStatus::Error,
        "Writable directory required: /mnt/archivefs",
        "ArchiveFS must create mount-point directories below mount_root.",
        "Grant the current user write access or choose another mount_root.",
    )]);
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.doctor_report = Gathered::Ready(&report);
    inputs.setup = Gathered::Ready(&setup);

    let scan = run_doctor_scan(&inputs);
    assert_eq!(scan.merged_duplicate_count, 0);
    assert_eq!(scan.findings.len(), 2);
}

#[test]
fn merging_keeps_the_highest_severity_the_guidance_and_the_canonical_subsystem() {
    let mut terse = Finding::new(
        "mount_root.not_writable",
        DoctorCategory::MountRoot,
        DoctorSubsystem::DoctorReport,
        DoctorSeverity::Warning,
        "mount root writable",
        "not writable",
    );
    terse.affected = Some(EncodedPath::from_path(Path::new("/mnt/archivefs")));

    let mut rich = Finding::new(
        "mount_root.not_writable",
        DoctorCategory::MountRoot,
        DoctorSubsystem::SetupDiagnostics,
        DoctorSeverity::Error,
        "Mount root is writable",
        "Writable directory required",
    );
    rich.affected = Some(EncodedPath::from_path(Path::new("/mnt/archivefs")));
    rich.why_it_matters = Some("ArchiveFS must create mount-point directories.".to_string());
    rich.next_step = Some("Grant write access.".to_string());
    rich.recovery = Some(KnownRecovery::new(
        None,
        "Settings → Validate configuration",
    ));

    let merged = runner::merge_duplicates(vec![terse, rich]);
    assert_eq!(merged.len(), 1);
    let finding = &merged[0];

    // Highest severity wins - a warning can never mask an error.
    assert_eq!(finding.severity, DoctorSeverity::Error);
    // The first reporter stays the canonical source subsystem.
    assert_eq!(finding.subsystem, DoctorSubsystem::DoctorReport);
    // Guidance and recovery are adopted from the richer report.
    assert_eq!(
        finding.why_it_matters.as_deref(),
        Some("ArchiveFS must create mount-point directories.")
    );
    assert_eq!(finding.next_step.as_deref(), Some("Grant write access."));
    assert!(finding.repair_may_exist());
    // Provenance of the merged-away report is not lost.
    assert!(
        finding
            .evidence
            .iter()
            .any(|item| item == "Also reported by: setup diagnostics"),
        "{:?}",
        finding.evidence
    );
}

// --- 12. Adapter failure ------------------------------------------------

#[test]
fn a_failed_adapter_input_becomes_a_finding_and_never_panics() {
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.database = Gathered::Failed("catalogue path could not be resolved".to_string());
    // A second subsystem still runs, proving one failure does not stop the
    // scan.
    let issues = vec![health_issue("/roms/a.zip", HealthCategory::Missing)];
    inputs.health_issues = Gathered::Ready(issues.as_slice());

    let scan = run_doctor_scan(&inputs);
    let failure = scan
        .finding("doctor.adapter_failed.database_diagnostics")
        .expect("the failure is reported as a finding");
    assert_eq!(failure.severity, DoctorSeverity::Error);
    assert_eq!(failure.category, DoctorCategory::Doctor);
    assert_eq!(failure.subsystem, DoctorSubsystem::DoctorRunner);
    assert!(
        failure
            .evidence
            .iter()
            .any(|item| item.contains("could not be resolved")),
        "{:?}",
        failure.evidence
    );
    assert!(scan.finding("library.archive_missing").is_some());
    assert!(
        scan.unavailable_subsystems()
            .iter()
            .any(|entry| entry.subsystem == DoctorSubsystem::DatabaseDiagnostics)
    );
}

// --- 13-17. Adapter correctness -----------------------------------------

#[test]
fn database_diagnostics_map_to_stable_ids_and_are_escalated_conservatively() {
    let cases = [
        (
            DatabaseDiagnosticCode::CorruptDatabase,
            DatabaseDiagnosticSeverity::Error,
            "database.corrupt",
            DoctorSeverity::Critical,
        ),
        (
            DatabaseDiagnosticCode::IntegrityCheckFailed,
            DatabaseDiagnosticSeverity::Error,
            "database.integrity_check_failed",
            DoctorSeverity::Critical,
        ),
        (
            DatabaseDiagnosticCode::PermissionDenied,
            DatabaseDiagnosticSeverity::Error,
            "database.permission_denied",
            DoctorSeverity::Error,
        ),
        (
            DatabaseDiagnosticCode::WalPresent,
            DatabaseDiagnosticSeverity::Info,
            "database.wal_present",
            DoctorSeverity::Info,
        ),
        (
            DatabaseDiagnosticCode::HotRollbackJournal,
            DatabaseDiagnosticSeverity::Warning,
            "database.hot_rollback_journal",
            DoctorSeverity::Warning,
        ),
    ];
    for (code, severity, expected_id, expected_severity) in cases {
        let report = database_report(vec![database_diagnostic(code, severity)]);
        let findings = findings_from_database_report(&report);
        assert_eq!(findings.len(), 1, "{code:?}");
        assert_eq!(findings[0].id, expected_id, "{code:?}");
        assert_eq!(findings[0].severity, expected_severity, "{code:?}");
        assert_eq!(findings[0].category, DoctorCategory::Database);
        assert!(
            findings[0]
                .evidence
                .iter()
                .any(|item| item.contains("extended result code: 11")),
            "{:?}",
            findings[0].evidence
        );
        // Database repair is never automatic in ArchiveFS, so nothing is
        // offered here at all.
        assert!(!findings[0].repair_may_exist(), "{code:?}");
    }
}

#[test]
fn no_database_error_is_ever_downgraded_below_error() {
    for code in [
        DatabaseDiagnosticCode::MissingDatabase,
        DatabaseDiagnosticCode::PermissionDenied,
        DatabaseDiagnosticCode::DatabaseLocked,
        DatabaseDiagnosticCode::DatabaseBusy,
        DatabaseDiagnosticCode::RollbackJournalPresent,
        DatabaseDiagnosticCode::HotRollbackJournal,
        DatabaseDiagnosticCode::NonHotRollbackJournal,
        DatabaseDiagnosticCode::MalformedRollbackJournal,
        DatabaseDiagnosticCode::RollbackRecoveryRequired,
        DatabaseDiagnosticCode::WalPresent,
        DatabaseDiagnosticCode::ShmPresent,
        DatabaseDiagnosticCode::CorruptDatabase,
        DatabaseDiagnosticCode::MalformedDatabase,
        DatabaseDiagnosticCode::IntegrityCheckFailed,
        DatabaseDiagnosticCode::SchemaVersionUnsupported,
        DatabaseDiagnosticCode::MigrationFailed,
        DatabaseDiagnosticCode::IoError,
        DatabaseDiagnosticCode::SqliteError,
    ] {
        let report = database_report(vec![database_diagnostic(
            code,
            DatabaseDiagnosticSeverity::Error,
        )]);
        let findings = findings_from_database_report(&report);
        assert!(
            findings[0].severity.rank() <= DoctorSeverity::Error.rank(),
            "{code:?} was downgraded to {:?}",
            findings[0].severity
        );
    }
}

#[test]
fn a_failing_sqlite_check_is_reported_even_with_no_diagnostic() {
    let mut report = database_report(Vec::new());
    report.integrity_check = DatabaseCheckOutcome {
        status: DatabaseCheckStatus::Failed,
        messages: vec!["*** in database main ***".to_string()],
    };
    let findings = findings_from_database_report(&report);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "database.integrity_check_reported_problems");
    assert_eq!(findings[0].severity, DoctorSeverity::Critical);
    assert_eq!(
        findings[0].evidence,
        vec!["*** in database main ***".to_string()]
    );
}

#[test]
fn health_issues_map_their_category_severity_and_known_recovery() {
    let issues = vec![
        health_issue("/roms/retry.zip", HealthCategory::RetryableFailure),
        health_issue("/roms/terminal.zip", HealthCategory::TerminalFailure),
        health_issue("/roms/missing.zip", HealthCategory::Missing),
    ];
    let findings = findings_from_health_issues(&issues);

    let retry = &findings[0];
    assert_eq!(retry.category, DoctorCategory::Mounts);
    assert_eq!(retry.severity, DoctorSeverity::Warning);
    assert_eq!(
        retry.recovery.as_ref().expect("recovery").action,
        Some(RecoveryAction::RetryMount)
    );
    assert!(
        retry
            .recovery
            .as_ref()
            .expect("recovery")
            .notice()
            .starts_with("A repair action already exists elsewhere in ArchiveFS")
    );

    assert_eq!(findings[1].severity, DoctorSeverity::Error);
    assert!(
        !findings[1].repair_may_exist(),
        "a terminal failure needs a person, not a retry"
    );

    // Missing has no RecoveryAction, but a repair does exist elsewhere.
    let missing = &findings[2];
    assert!(missing.repair_may_exist());
    assert_eq!(missing.recovery.as_ref().expect("recovery").action, None);

    for finding in &findings {
        assert!(finding.affected.is_some());
        assert!(
            finding
                .evidence
                .iter()
                .any(|item| item.starts_with("Classification: "))
        );
    }
}

#[test]
fn setup_diagnostics_carry_their_guidance_verbatim() {
    let setup = setup_diagnostics(vec![
        setup_check(
            "ratarmount is available",
            SetupDiagnosticStatus::Error,
            "ratarmount was not found.",
            "ArchiveFS uses ratarmount to expose archive contents as read-only folders.",
            "Install ratarmount and ensure it is available on PATH, then refresh diagnostics.",
        ),
        setup_check(
            "Config file exists",
            SetupDiagnosticStatus::Ready,
            "found",
            "why",
            "next",
        ),
    ]);
    let findings = findings_from_setup_diagnostics(&setup);

    assert_eq!(findings.len(), 1, "a Ready check produces no finding");
    assert_eq!(findings[0].severity, DoctorSeverity::Error);
    assert_eq!(
        findings[0].why_it_matters.as_deref(),
        Some("ArchiveFS uses ratarmount to expose archive contents as read-only folders.")
    );
    assert_eq!(
        findings[0].next_step.as_deref(),
        Some("Install ratarmount and ensure it is available on PATH, then refresh diagnostics.")
    );
    assert_eq!(findings[0].subsystem, DoctorSubsystem::SetupDiagnostics);
}

#[test]
fn a_config_path_error_from_setup_diagnostics_becomes_a_finding() {
    let mut setup = setup_diagnostics(Vec::new());
    setup.config_path_error = Some("HOME is not set".to_string());
    let findings = findings_from_setup_diagnostics(&setup);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "config.path_unresolvable");
    assert_eq!(findings[0].severity, DoctorSeverity::Error);
}

#[cfg(unix)]
#[test]
fn destination_safety_failures_map_to_stable_mount_root_ids() {
    let tree = TempTree::new("symlinked-root");
    let real = tree.root.join("real");
    fs::create_dir_all(&real).expect("real dir");
    let linked = tree.root.join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("symlink");

    let safety = assess_mount_root_safety(&linked);
    let findings = findings_from_mount_root_safety(&safety);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "mount_root.symlink");
    assert_eq!(findings[0].severity, DoctorSeverity::Error);
    assert_eq!(findings[0].category, DoctorCategory::MountRoot);
    assert_eq!(findings[0].subsystem, DoctorSubsystem::DestinationSafety);
    assert!(
        findings[0]
            .evidence
            .iter()
            .any(|item| item.contains("RootSymlink")),
        "{:?}",
        findings[0].evidence
    );
    assert!(findings[0].why_it_matters.is_some());
    assert!(findings[0].next_step.is_some());
}

#[test]
fn a_safe_mount_root_produces_no_destination_finding() {
    let tree = TempTree::new("safe-root");
    let mount_root = tree.root.join("mnt");
    fs::create_dir_all(&mount_root).expect("dir");
    let safety = assess_mount_root_safety(&mount_root);
    assert!(findings_from_mount_root_safety(&safety).is_empty());
}

#[test]
fn a_partial_failure_transaction_is_surfaced_with_its_rollback_location() {
    let history = transaction_history(SharedApplyStatus::PartialFailure);
    let scan = run_doctor_scan(&only!(transactions = &history));

    let finding = scan
        .finding("transactions.partial_failure")
        .expect("partial failure is surfaced");
    assert_eq!(finding.severity, DoctorSeverity::Error);
    assert_eq!(finding.category, DoctorCategory::Transactions);
    assert!(
        finding
            .evidence
            .iter()
            .any(|item| item.contains("Operation: op-1")),
        "{:?}",
        finding.evidence
    );
    assert_eq!(
        finding.recovery.as_ref().expect("recovery").available_at,
        "History & Logs → Preview rollback for this operation"
    );
}

#[test]
fn successful_and_dry_run_transactions_are_not_findings() {
    for status in [SharedApplyStatus::Success, SharedApplyStatus::DryRun] {
        let history = transaction_history(status);
        let scan = run_doctor_scan(&only!(transactions = &history));
        assert!(scan.is_healthy(), "{status:?} produced {:?}", scan.findings);
    }
}

#[test]
fn an_unreadable_journal_and_a_truncated_history_are_both_reported() {
    let mut history = transaction_history(SharedApplyStatus::Success);
    history.complete = false;
    history.warnings.push(SharedJournalWarning {
        path: SharedTransactionPath::from_path(Path::new("/history/broken.json")),
        failure: SharedApplyFailure {
            kind: SharedApplyFailureKind::InvalidJournal,
            path: None,
            detail: "invalid json".to_string(),
        },
    });
    let scan = run_doctor_scan(&only!(transactions = &history));
    assert!(scan.finding("transactions.journal_unreadable").is_some());
    let truncated = scan
        .finding("transactions.history_truncated")
        .expect("truncation is visible");
    assert_eq!(truncated.severity, DoctorSeverity::Info);
}

#[test]
fn retroarch_diagnostics_are_adapted_with_their_own_codes_and_paths() {
    let report = retroarch_report(vec![retroarch_diagnostic(
        "cores_directory_missing",
        DiagnosticSeverity::Warning,
        DiagnosticCategory::PathResolution,
    )]);
    let findings = findings_from_retroarch_environment(&report);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].id,
        "emulators.retroarch.cores_directory_missing"
    );
    assert_eq!(findings[0].severity, DoctorSeverity::Warning);
    assert_eq!(findings[0].category, DoctorCategory::Emulators);
    assert_eq!(
        findings[0]
            .affected
            .as_ref()
            .expect("path")
            .display
            .as_str(),
        "/var/lib/retroarch/cores"
    );
    assert!(
        findings[0]
            .evidence
            .iter()
            .any(|item| item.contains("Area: path resolution")),
        "{:?}",
        findings[0].evidence
    );
}

#[test]
fn source_health_issues_report_one_finding_per_folder_with_rows_preserved() {
    let issues = vec![
        SourceHealthIssue {
            path: PathBuf::from("/roms/offline"),
            availability: SourceAvailability::Unavailable,
            reason: "Source unavailable. Existing catalogue entries were preserved.".to_string(),
            last_scan_error: None,
            archives_preserved: 42,
        },
        SourceHealthIssue {
            path: PathBuf::from("/roms/broken"),
            availability: SourceAvailability::ScanFailed,
            reason: "The last scan of this source failed.".to_string(),
            last_scan_error: Some("permission denied".to_string()),
            archives_preserved: 7,
        },
    ];
    let findings = findings_from_source_health(&issues);
    assert_eq!(findings.len(), 2, "one per folder, never one per archive");
    assert_eq!(findings[0].id, "sources.unavailable");
    assert_eq!(findings[0].severity, DoctorSeverity::Error);
    assert!(
        findings[0]
            .evidence
            .iter()
            .any(|item| item == "Catalogue rows preserved: 42")
    );
    assert_eq!(findings[1].id, "sources.scan_failed");
    assert_eq!(findings[1].severity, DoctorSeverity::Warning);
    assert!(
        findings[1]
            .evidence
            .iter()
            .any(|item| item.contains("permission denied"))
    );
}

// --- 18, 19. Honest reporting -------------------------------------------

#[test]
fn a_clean_scan_is_healthy_but_never_claims_complete_coverage() {
    let report = doctor_report(vec![check("config file", DoctorStatus::Pass, "found")]);
    let scan = run_doctor_scan(&only!(doctor_report = &report));

    assert!(scan.is_healthy());
    assert_eq!(scan.overall_severity(), DoctorSeverity::Healthy);
    assert_eq!(scan.blocking_count(), 0);
    // Exactly one subsystem was checked; the rest are honestly unavailable.
    assert_eq!(scan.checked_subsystems().len(), 1);
    assert!(!scan.unavailable_subsystems().is_empty());
    // And the deferred list is always present.
    assert!(!scan.deferred.is_empty());
    assert!(
        scan.deferred
            .iter()
            .any(|deferred| deferred.name == "Free disk space")
    );
}

#[test]
fn every_deferred_check_names_itself_and_explains_why() {
    for deferred in DEFERRED_CHECKS {
        assert!(!deferred.name.is_empty());
        assert!(
            deferred.reason.len() > 20,
            "{} needs a real reason",
            deferred.name
        );
    }
    for expected in [
        "Free disk space",
        "Read-only filesystem detection",
        "Emulator profile writability",
        "Orphaned ArchiveFS-managed cheat entries",
        "GameHacking.org cache health",
        "Repairs",
    ] {
        assert!(
            DEFERRED_CHECKS
                .iter()
                .any(|deferred| deferred.name == expected),
            "{expected} must be listed as deferred"
        );
    }
}

#[test]
fn an_unloaded_subsystem_is_reported_as_unavailable_not_as_a_pass() {
    let scan = run_doctor_scan(&DoctorScanInputs::none_loaded());
    assert!(scan.is_healthy(), "no findings without inputs");
    assert!(
        scan.checked_subsystems().is_empty(),
        "nothing was actually checked"
    );
    assert_eq!(scan.unavailable_subsystems().len(), scan.coverage.len());
    for entry in scan.unavailable_subsystems() {
        match &entry.status {
            CoverageStatus::Unavailable { reason } => assert!(!reason.is_empty()),
            CoverageStatus::Checked => panic!("must not be reported as checked"),
        }
    }
}

// --- 23. No repair is reachable -----------------------------------------

/// The finding model cannot express an executable repair. [`KnownRecovery`]
/// holds a descriptive label and a location string and nothing else, so
/// there is no way for any UI to invoke something from a finding.
#[test]
fn the_finding_model_exposes_no_executable_repair() {
    let issues = vec![health_issue(
        "/roms/a.zip",
        HealthCategory::RetryableFailure,
    )];
    let findings = findings_from_health_issues(&issues);
    let recovery = findings[0].recovery.as_ref().expect("recovery metadata");

    assert_eq!(recovery.action, Some(RecoveryAction::RetryMount));
    assert_eq!(recovery.available_at, "Library → Health, Retry");

    let json = serde_json::to_string(&findings[0]).expect("json");
    for forbidden in ["command", "execute", "exec", "argv", "callback"] {
        assert!(
            !json.to_ascii_lowercase().contains(forbidden),
            "a serialised finding must not look executable: {forbidden}"
        );
    }
}

#[test]
fn no_finding_is_ever_emitted_with_healthy_severity() {
    let issues: Vec<HealthIssue> = [
        HealthCategory::TerminalFailure,
        HealthCategory::RetryableFailure,
        HealthCategory::RecoveryAvailable,
        HealthCategory::Missing,
        HealthCategory::AwaitingValidation,
        HealthCategory::CachedOnly,
        HealthCategory::UnknownPlatform,
    ]
    .iter()
    .enumerate()
    .map(|(index, category)| health_issue(&format!("/roms/{index}.zip"), *category))
    .collect();
    let report = doctor_report(vec![
        check("config file", DoctorStatus::Fail, "missing"),
        check("mount status", DoctorStatus::Warn, "skipped"),
    ]);
    let setup = setup_diagnostics(vec![setup_check(
        "Mount root is writable",
        SetupDiagnosticStatus::Warning,
        "detail",
        "why",
        "next",
    )]);
    let database = database_report(vec![database_diagnostic(
        DatabaseDiagnosticCode::CorruptDatabase,
        DatabaseDiagnosticSeverity::Error,
    )]);
    let history = transaction_history(SharedApplyStatus::PartialFailure);
    let retroarch = retroarch_report(vec![retroarch_diagnostic(
        "config_unparsable",
        DiagnosticSeverity::Error,
        DiagnosticCategory::ConfigParse,
    )]);
    let sources = vec![SourceHealthIssue {
        path: PathBuf::from("/roms"),
        availability: SourceAvailability::PermissionDenied,
        reason: "Permission denied.".to_string(),
        last_scan_error: None,
        archives_preserved: 1,
    }];

    let inputs = DoctorScanInputs {
        doctor_report: Gathered::Ready(&report),
        setup: Gathered::Ready(&setup),
        health_issues: Gathered::Ready(issues.as_slice()),
        source_health: Gathered::Ready(sources.as_slice()),
        database: Gathered::Ready(&database),
        mount_root_safety: Gathered::NotLoaded("not gathered in this test"),
        retroarch: Gathered::Ready(&retroarch),
        transactions: Gathered::Ready(&history),
        stale_mount_directories: Gathered::NotLoaded("not gathered in this test"),
        index_freshness: Gathered::NotLoaded("not gathered in this test"),
    };
    let scan = run_doctor_scan(&inputs);

    assert!(!scan.findings.is_empty());
    for finding in &scan.findings {
        assert_ne!(
            finding.severity,
            DoctorSeverity::Healthy,
            "{} claimed Healthy severity",
            finding.id
        );
        assert!(!finding.id.is_empty());
        assert!(!finding.title.is_empty());
        assert!(!finding.explanation.is_empty());
    }
    // Grouping is in stable category order.
    let categories: Vec<DoctorCategory> = scan
        .by_category()
        .into_iter()
        .map(|(category, _)| category)
        .collect();
    let mut sorted = categories.clone();
    sorted.sort_by_key(|category| {
        DoctorCategory::ALL
            .iter()
            .position(|candidate| candidate == category)
            .unwrap_or(usize::MAX)
    });
    assert_eq!(categories, sorted, "category grouping must be stable");
}

// --- 20. Counts ---------------------------------------------------------

#[test]
fn severity_counts_are_exact_and_include_zeroes() {
    let issues = vec![
        health_issue("/roms/a.zip", HealthCategory::TerminalFailure),
        health_issue("/roms/b.zip", HealthCategory::Missing),
        health_issue("/roms/c.zip", HealthCategory::CachedOnly),
        health_issue("/roms/d.zip", HealthCategory::UnknownPlatform),
    ];
    let scan = run_doctor_scan(&only!(health_issues = issues.as_slice()));
    assert_eq!(
        scan.counts(),
        vec![
            (DoctorSeverity::Critical, 0),
            (DoctorSeverity::Error, 1),
            (DoctorSeverity::Warning, 1),
            (DoctorSeverity::Info, 2),
        ]
    );
    assert_eq!(scan.blocking_count(), 1);
    assert_eq!(scan.overall_severity(), DoctorSeverity::Error);
}

// --- 22. Long paths and Unicode -----------------------------------------

#[test]
fn long_unicode_paths_survive_adaptation_and_serialisation() {
    let long_name = "ロング".repeat(200);
    let path = format!("/roms/{long_name}/ゲーム 💾 [!].zip");
    let issues = vec![health_issue(&path, HealthCategory::Missing)];
    let findings = findings_from_health_issues(&issues);
    let affected = findings[0].affected.as_ref().expect("path");
    assert_eq!(affected.display, path);
    assert!(!affected.lossy);
    let json = serde_json::to_string(&findings[0]).expect("json");
    assert!(json.contains("💾") || json.contains("\\ud83d"));
}

#[cfg(unix)]
#[test]
fn a_non_utf8_path_is_flagged_lossy_rather_than_silently_mangled() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let raw = OsStr::from_bytes(b"/roms/inva\xfflid.zip");
    let issues = vec![HealthIssue {
        path: PathBuf::from(raw),
        ..health_issue("/placeholder", HealthCategory::Missing)
    }];
    let findings = findings_from_health_issues(&issues);
    let affected = findings[0].affected.as_ref().expect("path");
    assert!(affected.lossy, "non-UTF-8 paths must be marked lossy");
    assert!(affected.display.contains('\u{fffd}'));
}

// --- 24. No migration ---------------------------------------------------

/// Stage 1A stores nothing, so it must not have added, renumbered, or
/// modified a migration. Asserted against the compiled-in migration set so
/// it holds wherever the tests run.
#[test]
fn stage_1a_introduces_no_database_migration() {
    const EXPECTED: [&str; 5] = [
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_platform_aliases.sql"),
        include_str!("../migrations/0003_source_folder_scan_status.sql"),
        include_str!("../migrations/0004_scan_skip_counts.sql"),
        include_str!("../migrations/0005_source_platform_assignment.sql"),
    ];
    assert_eq!(
        crate::latest_schema_version(),
        EXPECTED.len() as i64,
        "the schema version changed, so a migration was added or removed"
    );
    for source in [
        include_str!("mod.rs"),
        include_str!("runner.rs"),
        include_str!("repair.rs"),
    ] {
        assert!(!source.contains("migrations/"));
        assert!(!source.contains("apply_migrations"));
    }
}

// --- 26. No GameHacking or Wii dependency -------------------------------

/// Doctor must not gain a dependency on the GameHacking or Wii providers -
/// they are not part of this release, and Doctor must keep compiling
/// identically with or without those feature branches merged.
///
/// This checks for real *code* references (module paths and type names),
/// not for the word appearing anywhere: `DEFERRED_CHECKS` deliberately
/// contains the user-facing string "GameHacking.org cache health", which is
/// prose the product shows, not a dependency.
#[test]
fn core_diagnostics_reference_no_gamehacking_or_wii_symbol() {
    for source in [
        include_str!("mod.rs"),
        include_str!("runner.rs"),
        include_str!("repair.rs"),
    ] {
        // No import may mention them at all.
        for line in source
            .lines()
            .filter(|line| line.trim_start().starts_with("use "))
        {
            let lower = line.to_ascii_lowercase();
            for forbidden in ["gamehacking", "wii", "cloudflare", "browserimport"] {
                assert!(
                    !lower.contains(forbidden),
                    "import must not reference `{forbidden}`: {line}"
                );
            }
        }
        // No snake_case module path (`gamehacking_provider`,
        // `gamehacking_gamecube_provider`, ...).
        assert!(!source.contains("gamehacking_"));
        assert!(!source.contains("patch_manager::gamehacking"));
        // No CamelCase type name: every real type is `GameHacking` followed
        // by more identifier characters (`GameHackingError`,
        // `GameHackingGame`, ...), whereas the deferred-check prose is
        // always "GameHacking.org".
        for (index, _) in source.match_indices("GameHacking") {
            let next = source[index + "GameHacking".len()..].chars().next();
            assert!(
                !next.is_some_and(|character| character.is_alphanumeric() || character == '_'),
                "a GameHacking type name is referenced at byte {index}"
            );
        }
        assert!(
            source.match_indices("BrowserImport").next().is_none(),
            "a browser-import type is referenced"
        );
    }
}

// --- Model invariants ---------------------------------------------------

#[test]
fn severity_ranking_is_total_and_most_severe_first() {
    let mut ranked = vec![
        DoctorSeverity::Info,
        DoctorSeverity::Critical,
        DoctorSeverity::Healthy,
        DoctorSeverity::Error,
        DoctorSeverity::Warning,
    ];
    ranked.sort_by_key(|severity| severity.rank());
    assert_eq!(
        ranked,
        vec![
            DoctorSeverity::Critical,
            DoctorSeverity::Error,
            DoctorSeverity::Warning,
            DoctorSeverity::Info,
            DoctorSeverity::Healthy,
        ]
    );
    assert!(DoctorSeverity::Critical.is_blocking());
    assert!(DoctorSeverity::Error.is_blocking());
    assert!(!DoctorSeverity::Warning.is_blocking());
    assert!(!DoctorSeverity::Info.is_blocking());
}

#[test]
fn every_category_has_a_distinct_label() {
    let mut labels = Vec::new();
    for category in DoctorCategory::ALL {
        assert!(!category.label().is_empty());
        assert!(!labels.contains(&category.label()));
        labels.push(category.label());
    }
    assert_eq!(DoctorCategory::ALL.len(), labels.len());
}

/// `ArchiveHealth`'s retryability rules are the existing evidence boundary
/// for "can retrying help?". Doctor must not invent a second answer.
#[test]
fn doctor_reuses_the_existing_retryability_rules() {
    assert!(ArchiveHealth::Failed.is_retryable());
    assert!(ArchiveHealth::Corrupt.is_terminal_without_source_change());
    let terminal = findings_from_health_issues(&[health_issue(
        "/roms/a.zip",
        HealthCategory::TerminalFailure,
    )]);
    let retryable = findings_from_health_issues(&[health_issue(
        "/roms/b.zip",
        HealthCategory::RetryableFailure,
    )]);
    assert!(
        terminal[0].severity.rank() < retryable[0].severity.rank(),
        "a failure retrying cannot fix must outrank one it can"
    );
}

/// Every path-based gatherer, run together against a complete fake ArchiveFS
/// data directory: config file, catalogue database, mount root with real user
/// content, and an install journal. Nothing in the tree may change.
///
/// This is the whole-gather counterpart to the per-gatherer tests above and
/// to `runner_source_contains_no_io_or_mutation_calls`.
#[test]
fn a_complete_gather_and_scan_leaves_the_entire_data_directory_unchanged() {
    use crate::patch_manager::discover_shared_apply_history;

    let tree = TempTree::new("full-gather");
    let config_path = tree.root.join("config/config.toml");
    let database_path = tree.root.join("data/library.sqlite3");
    let mount_root = tree.root.join("mnt");
    let history_root = tree.root.join("data/history");
    fs::create_dir_all(mount_root.join("SNES")).expect("mount root");
    fs::create_dir_all(&history_root).expect("history root");
    fs::create_dir_all(config_path.parent().expect("parent")).expect("config dir");
    fs::create_dir_all(database_path.parent().expect("parent")).expect("data dir");
    fs::write(
        &config_path,
        format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            tree.root.join("roms").display(),
            mount_root.display()
        ),
    )
    .expect("config");
    fs::create_dir_all(tree.root.join("roms")).expect("roms");
    // Real user content that must survive untouched.
    fs::write(mount_root.join("SNES/user-notes.txt"), b"do not touch").expect("user file");
    // A journal that is deliberately unparseable, so the history gatherer
    // takes its warning path too.
    fs::write(history_root.join("broken.json"), b"{not json").expect("journal");
    {
        let database = crate::Database::open_or_create(&database_path).expect("catalogue");
        let _ = database.load_archives().expect("load");
    }

    let before = snapshot_tree(&tree.root);
    assert!(before.len() >= 6, "the fixture tree must be substantial");

    // Every gatherer the GUI and CLI use, with explicit paths.
    //
    // `run_setup_diagnostics` is deliberately absent: its "Mount root is
    // writable" check probes by creating and removing a file, which changes
    // the mount root's modification time. Doctor never calls it - the GUI
    // borrows an already-computed `SetupDiagnostics` instead - and this test
    // is what caught that write in the first place.
    let mount_root_safety = assess_mount_root_safety(&mount_root);
    let database = diagnose_database(&database_path);
    let source_views =
        crate::list_source_folder_views_at(&config_path, &database_path).expect("views");
    let source_health = crate::source_health_issues(&source_views);
    let transactions = discover_shared_apply_history(&history_root);
    // The two Stage 1B read-only gatherers: the leftover-mount-folder plan
    // and index freshness. Both must be as non-mutating as the rest.
    let config = crate::Config::load_from(&config_path).expect("config");
    let stale = crate::plan_stale_mount_directories(&config).expect("plan");
    let index = crate::ArchiveIndex {
        archives: Vec::new(),
    };
    let freshness = crate::check_archive_index_freshness(&index);
    let index_path = tree.root.join("data/index.json");

    let inputs = DoctorScanInputs {
        doctor_report: Gathered::NotLoaded("no snapshot in this test"),
        setup: Gathered::NotLoaded("not re-run: that check writes a probe file"),
        health_issues: Gathered::Ready(&[]),
        source_health: Gathered::Ready(source_health.as_slice()),
        database: Gathered::Ready(&database),
        mount_root_safety: Gathered::Ready(&mount_root_safety),
        retroarch: Gathered::NotLoaded("discovery is never started by Doctor"),
        transactions: Gathered::Ready(&transactions),
        stale_mount_directories: Gathered::Ready(stale.as_slice()),
        index_freshness: Gathered::Ready((&freshness, index_path.as_path())),
    };
    let scan = run_doctor_scan(&inputs);

    let after = snapshot_tree(&tree.root);
    assert_eq!(
        before, after,
        "gathering or scanning changed the data directory"
    );
    assert_eq!(
        fs::read(mount_root.join("SNES/user-notes.txt")).expect("user file"),
        b"do not touch".to_vec(),
        "user content must never be touched"
    );
    // The unparseable journal is surfaced rather than ignored or repaired.
    assert!(
        scan.finding("transactions.journal_unreadable").is_some(),
        "{:?}",
        scan.findings
    );
    // Seven subsystems were genuinely checked, and the rest are honestly
    // recorded as unavailable rather than as passes.
    assert_eq!(scan.checked_subsystems().len(), 7, "{:?}", scan.coverage);
    assert_eq!(
        scan.unavailable_subsystems().len(),
        3,
        "{:?}",
        scan.coverage
    );
}

// --- Stage 1B: probe-free setup diagnostics -----------------------------

/// The probe used by the normal path creates and removes a file inside the
/// mount root. The read-only variant must not, and this is the test that
/// caught it in Stage 1A.
#[test]
fn probe_free_setup_diagnostics_create_no_files_and_change_no_mtime() {
    let tree = TempTree::new("probe-free");
    let mount_root = tree.root.join("mnt");
    let source = tree.root.join("roms");
    fs::create_dir_all(&mount_root).expect("mount root");
    fs::create_dir_all(&source).expect("source");
    let config_path = tree.root.join("config.toml");
    fs::write(
        &config_path,
        format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            source.display(),
            mount_root.display()
        ),
    )
    .expect("config");

    let before = snapshot_tree(&tree.root);
    let read_only = crate::run_setup_diagnostics_read_only(&config_path);
    let after = snapshot_tree(&tree.root);

    assert_eq!(
        before, after,
        "the probe-free variant created a file or changed a modification time"
    );

    // The two variants differ exactly where they should: the probing one
    // establishes writability by writing, the read-only one declines to.
    let probed = crate::run_setup_diagnostics(&config_path);
    assert!(
        probed
            .checks
            .iter()
            .any(|check| check.name == "Mount root is writable"
                && check.status == SetupDiagnosticStatus::Ready),
        "the probing variant establishes writability"
    );
    assert!(
        read_only
            .checks
            .iter()
            .any(|check| check.name == "Mount root is writable"
                && check.status == SetupDiagnosticStatus::NotChecked),
        "the read-only variant declines to"
    );
}

#[test]
fn probe_free_setup_diagnostics_report_writability_as_not_checked() {
    let tree = TempTree::new("probe-free-status");
    let mount_root = tree.root.join("mnt");
    let source = tree.root.join("roms");
    fs::create_dir_all(&mount_root).expect("mount root");
    fs::create_dir_all(&source).expect("source");
    let config_path = tree.root.join("config.toml");
    fs::write(
        &config_path,
        format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            source.display(),
            mount_root.display()
        ),
    )
    .expect("config");

    let report = crate::run_setup_diagnostics_read_only(&config_path);
    let writable = report
        .checks
        .iter()
        .find(|check| check.name == "Mount root is writable")
        .expect("the check is still present");
    assert_eq!(writable.status, SetupDiagnosticStatus::NotChecked);
    assert!(
        writable.detail.starts_with("Not probed:"),
        "{}",
        writable.detail
    );
    assert!(!writable.next_step.is_empty());
    // Readiness is never asserted on an unprobed mount root.
    assert!(
        !report.ready_for_actions,
        "actions must not be reported ready without established writability"
    );
    // Everything else is still checked normally.
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "Config file exists"
                && check.status == SetupDiagnosticStatus::Ready)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "Configured source folder exists"
                && check.status == SetupDiagnosticStatus::Ready)
    );
}

/// The CLI's `doctor --findings` regains setup coverage: the checks produce
/// findings as usual, and the unprobed one is surfaced as a coverage gap
/// rather than as a pass or a problem.
#[test]
fn probe_free_setup_coverage_reaches_the_doctor_scan() {
    let tree = TempTree::new("probe-free-scan");
    let mount_root = tree.root.join("mnt");
    fs::create_dir_all(&mount_root).expect("mount root");
    let config_path = tree.root.join("config.toml");
    // A source folder that does not exist, so there is a real finding too.
    fs::write(
        &config_path,
        format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            tree.root.join("missing-roms").display(),
            mount_root.display()
        ),
    )
    .expect("config");

    let setup = crate::run_setup_diagnostics_read_only(&config_path);
    let scan = run_doctor_scan(&only!(setup = &setup));

    // The subsystem counts as checked.
    assert!(
        scan.checked_subsystems()
            .iter()
            .any(|entry| entry.subsystem == DoctorSubsystem::SetupDiagnostics)
    );
    // The missing source folder is a real finding, carrying its guidance.
    let finding = scan
        .findings
        .iter()
        .find(|finding| finding.category == DoctorCategory::Sources)
        .expect("the missing source folder is reported");
    assert_eq!(finding.severity, DoctorSeverity::Error);
    assert!(finding.why_it_matters.is_some());
    assert!(finding.next_step.is_some());
    // And the unprobed check is a coverage gap, not a finding.
    assert!(
        !scan
            .findings
            .iter()
            .any(|finding| finding.title == "Mount root is writable"),
        "an unrun check must not be reported as a problem"
    );
    let not_checked = scan
        .not_checked
        .iter()
        .find(|item| item.name == "Mount root is writable")
        .expect("the unrun check is surfaced");
    assert!(not_checked.reason.starts_with("Not probed:"));
    assert!(!not_checked.next_step.is_empty());
}
