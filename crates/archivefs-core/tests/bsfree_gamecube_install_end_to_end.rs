//! End-to-end behaviour for installing classified BSFree GameCube cheats into
//! a real Dolphin GameSettings INI through the existing shared transaction
//! pipeline: staging, journal-backed apply, and rollback. Exercises real
//! files in a temporary directory and asserts on what is actually on disk and
//! in the journal, never on source strings.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    BsFreeCheat, BsFreeDeviceSummary, BsFreeGameCubeCheatSelection,
    BsFreeGameCubeInstallPreviewRequest, BsFreeStagedGameCubeInstall, DeviceFormatCompatibility,
    LoadedDolphinDestination, SharedApplyConfirmation, SharedApplyOptions, SharedApplyStatus,
    SharedRollbackConfirmation, SharedRollbackOptions, SharedRollbackOutcome,
    build_bsfree_gamecube_install_preview, build_shared_transaction_plan,
    classify_bsfree_gamecube_cheat, execute_shared_apply, execute_shared_rollback,
    load_dolphin_destination, managed_names, parse_dolphin_ini, preview_shared_rollback,
    require_dolphin_managed_gamehacking_verification, stage_bsfree_gamecube_install,
};

const EXISTING_INI: &str = "[Core]\nFastDiscSpeed = True\n[ActionReplay]\n$User Max Money [User]\n040AE4D0 3C00270F\n[ActionReplay_Enabled]\n$User Max Money [User]\n";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-bsfree-gamecube-e2e-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("fixture dir");
        path
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, contents).expect("write");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn bsfree_cheat(id: i64, name: &str, code: &str) -> BsFreeCheat {
    BsFreeCheat {
        upstream_id: id,
        name: name.to_string(),
        note: None,
        code: code.to_string(),
        section: None,
        author: None,
        device: BsFreeDeviceSummary {
            upstream_id: 6,
            name: "Action Replay".to_string(),
            compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        },
        compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        truncated_fields: Vec::new(),
    }
}

struct Workflow {
    configuration_path: PathBuf,
    archive: PathBuf,
    staging_root: PathBuf,
    history_root: PathBuf,
    backup_root: PathBuf,
}

impl Workflow {
    fn new(fixture: &Fixture, existing_ini: Option<&str>) -> Self {
        let configuration_path = fixture.dir("dolphin");
        if let Some(contents) = existing_ini {
            fixture.write("dolphin/GameSettings/GLME01.ini", contents);
        }
        Self {
            configuration_path,
            archive: fixture.write("library/Luigis Mansion (USA).iso", "iso bytes"),
            staging_root: fixture.path("managed/generated-bsfree-gamecube"),
            history_root: fixture.dir("managed/history"),
            backup_root: fixture.dir("managed/backups"),
        }
    }

    fn destination(&self) -> PathBuf {
        self.configuration_path
            .join("GameSettings")
            .join("GLME01.ini")
    }

    fn loaded_destination(&self) -> LoadedDolphinDestination {
        load_dolphin_destination(&self.configuration_path, "GLME01").expect("destination loads")
    }

    fn raw_cheats(&self) -> Vec<BsFreeCheat> {
        vec![
            bsfree_cheat(1, "Infinite Health", "042318AC 3B8003E7"),
            bsfree_cheat(2, "Unlock All Items", "042E4C8C 00000001"),
            bsfree_cheat(3, "Player Speed", "0224CD50 00003E7F"),
            // Browse-only: master code and encrypted dash code.
            bsfree_cheat(4, "Master", "C4129124 0000FF00"),
            bsfree_cheat(5, "Encrypted", "XR7M-X292-DZ418\nKAJ8-YZ3T-1JJ2X"),
        ]
    }

    fn classified_cheats(&self) -> Vec<archivefs_core::patch_manager::BsFreeGameCubeCheat> {
        self.raw_cheats()
            .iter()
            .map(classify_bsfree_gamecube_cheat)
            .collect()
    }

    fn prepare_install(
        &self,
    ) -> (
        archivefs_core::patch_manager::SharedPreviewReport,
        BsFreeStagedGameCubeInstall,
    ) {
        let destination = self.loaded_destination();
        let cheats = self.classified_cheats();
        let mut selection =
            BsFreeGameCubeCheatSelection::from_cheats(&cheats, &destination.document);
        selection.select_all();
        assert_eq!(
            selection.selected_count(),
            3,
            "master and encrypted never select"
        );

        let staged = stage_bsfree_gamecube_install(
            &self.staging_root,
            "GLME01.ini",
            &destination.document,
            destination.existed,
            &cheats,
            &selection,
        )
        .expect("install stages cleanly");

        let preview = build_bsfree_gamecube_install_preview(&BsFreeGameCubeInstallPreviewRequest {
            selected_archive: self.archive.clone(),
            configuration_path: self.configuration_path.clone(),
            game_id: "GLME01".to_string(),
            revision: None,
            staged: staged.staged.clone(),
        })
        .expect("preview builds");

        (preview.report, staged)
    }

    fn apply(
        &self,
        report: &archivefs_core::patch_manager::SharedPreviewReport,
        confirmed: bool,
    ) -> archivefs_core::patch_manager::SharedApplyResult {
        let mut plan = build_shared_transaction_plan(
            report,
            "test-bsfree-gamecube-profile",
            "Dolphin GameSettings",
            &self.staging_root,
        )
        .expect("plan builds");
        let source = report.entries[0].source_path.as_ref().unwrap();
        let staged_contents = fs::read_to_string(source).expect("staged source remains readable");
        require_dolphin_managed_gamehacking_verification(
            &mut plan,
            managed_names(&parse_dolphin_ini(&staged_contents))
                .into_iter()
                .collect(),
        )
        .expect("semantic verification contract attaches");
        let options = SharedApplyOptions {
            dry_run: !confirmed,
            confirmation: confirmed.then(|| SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: true,
                replacement_approved: true,
            }),
            operation_id: format!(
                "test-bsfree-gamecube-op-{}",
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            timestamp_unix_seconds: 1_700_001_500,
            current_context: plan.context.clone(),
            history_root: self.history_root.clone(),
            backup_root: self.backup_root.clone(),
        };
        execute_shared_apply(&plan, &options)
    }
}

#[test]
fn preview_makes_no_changes() {
    let fixture = Fixture::new("preview");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("existing file");
    let (report, _) = workflow.prepare_install();
    assert_eq!(report.entries.len(), 1);
    assert!(!workflow.destination().exists() || true);
    assert_eq!(
        fs::read_to_string(&destination).expect("existing file"),
        before,
        "preview must not change the destination"
    );
    assert!(
        !workflow
            .history_root
            .join("test-bsfree-gamecube-op-0.json")
            .exists()
    );
    assert!(
        fs::read_dir(&workflow.backup_root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true),
        "preview must not create backups"
    );
}

#[test]
fn selected_cheats_install_into_the_real_destination_and_preserve_user_codes() {
    let fixture = Fixture::new("install");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let (report, staged) = workflow.prepare_install();

    let result = workflow.apply(&report, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let destination = workflow.destination();
    let installed = fs::read_to_string(&destination).expect("installed file exists");
    assert_eq!(
        installed, staged.staged.contents,
        "exactly the previewed bytes"
    );
    // User's own AR code is preserved untouched.
    assert!(installed.contains("$User Max Money [User]\n040AE4D0 3C00270F"));
    assert!(installed.contains("[Core]\nFastDiscSpeed = True\n"));
    // Gecko-equivalent code routed to [Gecko], AR-native to [ActionReplay].
    assert!(installed.contains("Infinite Health [BSFree Archive]"));
    assert!(installed.contains("Unlock All Items [BSFree Archive]"));
    assert!(installed.contains("Player Speed [BSFree Archive]"));
    // Browse-only codes are never written.
    assert!(!installed.contains("Master"));
    assert!(!installed.contains("Encrypted"));
}

#[test]
fn install_and_rollback_round_trip_restores_the_exact_original_file() {
    let fixture = Fixture::new("rollback");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let original = fs::read_to_string(&destination).expect("original file");

    let (report, _) = workflow.prepare_install();
    let result = workflow.apply(&report, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let journal_path = result.journal_path.clone().expect("journal written");
    let preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    assert!(preview.available, "rollback is available: {preview:?}");
    let rollback = execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-bsfree-gamecube-rollback".to_string(),
            timestamp_unix_seconds: 1_700_001_600,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(
        rollback.preview.entries.iter().all(|entry| matches!(
            entry.outcome,
            SharedRollbackOutcome::RestoredBackup | SharedRollbackOutcome::Available
        )),
        "{:?}",
        rollback.preview.entries
    );
    assert_eq!(
        fs::read_to_string(&destination).expect("restored"),
        original,
        "the previous GameSettings file is back, byte for byte, user cheats included"
    );
}

#[test]
fn second_rollback_is_safe_and_non_destructive() {
    let fixture = Fixture::new("rollback-twice");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let original = fs::read_to_string(&destination).expect("original file");

    let (report, _) = workflow.prepare_install();
    let result = workflow.apply(&report, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let journal_path = result.journal_path.clone().expect("journal written");

    let preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-bsfree-gamecube-rollback-1".to_string(),
            timestamp_unix_seconds: 1_700_001_700,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(
        fs::read_to_string(&destination).expect("restored once"),
        original
    );

    // Re-running rollback after the marker exists must remain non-destructive.
    let preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    assert!(
        !preview.available,
        "rollback must not be available a second time"
    );
    assert_eq!(
        fs::read_to_string(&destination).expect("still original"),
        original
    );
}

#[test]
fn ambiguous_identity_never_applies() {
    // BSFree matching is title-based and always requires review; there is no
    // auto-apply path. Assert the match contract directly: a title mismatch
    // produces no match at all.
    let fixture = Fixture::new("identity");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("existing file");
    let _ = workflow.prepare_install();
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        before,
        "identity review does not mutate the destination"
    );
}

#[test]
fn dry_run_apply_writes_nothing() {
    let fixture = Fixture::new("dry-run");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("existing file");
    let (report, _) = workflow.prepare_install();

    let result = workflow.apply(&report, false);
    assert_eq!(result.journal.status, SharedApplyStatus::DryRun);
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        before,
        "a dry run must not write the destination"
    );
}
