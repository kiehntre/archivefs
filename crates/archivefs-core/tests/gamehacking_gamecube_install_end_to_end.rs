//! End-to-end behaviour for installing classified GameHacking.org
//! GameCube cheats into a real Dolphin GameSettings INI: staging,
//! journal-backed apply, and rollback. Exercises real files in a
//! temporary directory and asserts on what is actually on disk and in
//! the journal, never on source strings.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    GameCubeCheatSelection, GameCubeCodeFormat, GameCubeGameHackingInstallPreviewRequest,
    GameHackingGameCubeCheat, LoadedDolphinDestination, SharedApplyConfirmation,
    SharedApplyOptions, SharedApplyStatus, SharedRollbackConfirmation, SharedRollbackOptions,
    SharedRollbackOutcome, build_gamecube_gamehacking_install_preview,
    build_shared_transaction_plan, execute_shared_apply, execute_shared_rollback,
    load_dolphin_destination, preview_shared_rollback, stage_gamecube_gamehacking_install,
    stage_gamecube_gamehacking_removal,
};

const EXISTING_INI: &str = "[Core]\nFastDiscSpeed = True\n[Video_Settings]\nMSAA = 8\n";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamecube-gamehacking-e2e-{label}-{}-{}-{}",
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
            staging_root: fixture.path("managed/generated-gamecube-gamehacking"),
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

    fn cheats(&self) -> Vec<GameHackingGameCubeCheat> {
        vec![
            GameHackingGameCubeCheat {
                id: "1".to_string(),
                name: "999 Cash".to_string(),
                author: Some("Codejunkies".to_string()),
                description: None,
                code_format: GameCubeCodeFormat::ActionReplay,
                code_lines: vec![
                    "040AE4D0 3C00270F".to_string(),
                    "040AE4E8 60000000".to_string(),
                ],
                source_game_id: 42,
                source_url: "https://gamehacking.org/game/42".to_string(),
            },
            GameHackingGameCubeCheat {
                id: "2".to_string(),
                name: "Infinite Health".to_string(),
                author: Some("Link Master".to_string()),
                description: None,
                code_format: GameCubeCodeFormat::Gecko,
                code_lines: vec!["04123456 00000001".to_string()],
                source_game_id: 42,
                source_url: "https://gamehacking.org/game/42".to_string(),
            },
            GameHackingGameCubeCheat {
                id: "3".to_string(),
                name: "Mystery Code".to_string(),
                author: None,
                description: None,
                code_format: GameCubeCodeFormat::RawUnknown,
                code_lines: vec!["0A0A0A0A 0B0B0B0B".to_string()],
                source_game_id: 42,
                source_url: "https://gamehacking.org/game/42".to_string(),
            },
        ]
    }

    fn prepare_install(
        &self,
    ) -> (
        archivefs_core::patch_manager::SharedPreviewReport,
        archivefs_core::patch_manager::StagedGameCubeIni,
    ) {
        let destination = self.loaded_destination();
        let cheats = self.cheats();
        let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &destination.document);
        selection.select_all();
        assert_eq!(selection.selected_count(), 2, "RawUnknown never selects");

        let staged = stage_gamecube_gamehacking_install(
            &self.staging_root,
            "GLME01.ini",
            &destination.document,
            destination.existed,
            &cheats,
            &selection,
        )
        .expect("install stages cleanly");

        let preview =
            build_gamecube_gamehacking_install_preview(&GameCubeGameHackingInstallPreviewRequest {
                selected_archive: self.archive.clone(),
                configuration_path: self.configuration_path.clone(),
                game_id: "GLME01".to_string(),
                revision: None,
                staged: staged.clone(),
            })
            .expect("preview builds");

        (preview.report, staged)
    }

    fn apply(
        &self,
        report: &archivefs_core::patch_manager::SharedPreviewReport,
        confirmed: bool,
        replacement_approved: bool,
    ) -> archivefs_core::patch_manager::SharedApplyResult {
        let plan = build_shared_transaction_plan(
            report,
            "test-gamecube-gamehacking-profile",
            "Dolphin GameSettings",
            &self.staging_root,
        )
        .expect("plan builds");
        let options = SharedApplyOptions {
            dry_run: !confirmed,
            confirmation: confirmed.then(|| SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: true,
                replacement_approved,
            }),
            operation_id: format!(
                "test-gamecube-gamehacking-op-{}",
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            timestamp_unix_seconds: 1_700_000_500,
            current_context: plan.context.clone(),
            history_root: self.history_root.clone(),
            backup_root: self.backup_root.clone(),
        };
        execute_shared_apply(&plan, &options)
    }
}

#[test]
fn selected_action_replay_and_gecko_cheats_install_to_the_real_destination() {
    let fixture = Fixture::new("install");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let (report, staged) = workflow.prepare_install();

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let destination = workflow.destination();
    let installed = fs::read_to_string(&destination).expect("installed file exists");
    assert_eq!(installed, staged.contents, "exactly the previewed bytes");
    assert!(installed.contains("[Core]\nFastDiscSpeed = True\n"));
    assert!(installed.contains("[Video_Settings]\nMSAA = 8\n"));
    assert!(installed.contains("999 Cash [Codejunkies]"));
    assert!(installed.contains("Infinite Health [Link Master]"));
    assert!(
        !installed.contains("Mystery Code"),
        "RawUnknown is never written"
    );
}

#[test]
fn install_and_rollback_round_trip_restores_the_exact_original_file() {
    let fixture = Fixture::new("rollback");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let destination = workflow.destination();
    let original = fs::read_to_string(&destination).expect("original file");

    let (report, _staged) = workflow.prepare_install();
    let result = workflow.apply(&report, true, true);
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
            rollback_operation_id: "test-gamecube-gamehacking-rollback".to_string(),
            timestamp_unix_seconds: 1_700_000_600,
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
        "the previous GameSettings file is back, byte for byte"
    );
}

#[test]
fn install_creates_a_new_destination_file_when_none_existed() {
    let fixture = Fixture::new("create");
    let workflow = Workflow::new(&fixture, None);
    let (report, staged) = workflow.prepare_install();
    assert!(!workflow.destination().exists());

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(workflow.destination()).expect("created file exists"),
        staged.contents
    );
}

#[test]
fn removal_only_deletes_archivefs_managed_codes_from_the_real_file() {
    let fixture = Fixture::new("removal");
    let workflow = Workflow::new(&fixture, Some(EXISTING_INI));
    let (report, _staged) = workflow.prepare_install();
    let apply_result = workflow.apply(&report, true, true);
    assert_eq!(apply_result.journal.status, SharedApplyStatus::Success);

    let destination = workflow.loaded_destination();
    let removal_staged = stage_gamecube_gamehacking_removal(
        &workflow.staging_root,
        "GLME01.ini",
        &destination.document,
        true,
        &["999 Cash [Codejunkies]".to_string()],
    )
    .expect("removal stages cleanly");

    let removal_preview =
        build_gamecube_gamehacking_install_preview(&GameCubeGameHackingInstallPreviewRequest {
            selected_archive: workflow.archive.clone(),
            configuration_path: workflow.configuration_path.clone(),
            game_id: "GLME01".to_string(),
            revision: None,
            staged: removal_staged,
        })
        .expect("removal preview builds");

    let removal_apply = workflow.apply(&removal_preview.report, true, true);
    assert_eq!(removal_apply.journal.status, SharedApplyStatus::Success);

    let final_contents = fs::read_to_string(workflow.destination()).expect("file still exists");
    assert!(!final_contents.contains("999 Cash [Codejunkies]"));
    assert!(final_contents.contains("Infinite Health [Link Master]"));
    assert!(final_contents.contains("[Core]\nFastDiscSpeed = True\n"));
}
