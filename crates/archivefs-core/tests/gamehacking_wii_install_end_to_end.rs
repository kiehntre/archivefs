//! Offline end-to-end Wii GameHacking installation through the existing
//! Dolphin GameSettings transaction, semantic verification and Undo path.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    GameCubeGameHackingInstallPreviewRequest, GameHackingWiiCheat, LoadedDolphinDestination,
    SharedApplyConfirmation, SharedApplyOptions, SharedApplyStatus, SharedRollbackConfirmation,
    SharedRollbackOptions, WiiCheatSafety, WiiCodeFormat, build_shared_transaction_plan,
    build_wii_gamehacking_install_preview, execute_shared_apply, execute_shared_rollback,
    load_dolphin_destination, managed_names, parse_dolphin_ini, preview_shared_rollback,
    require_dolphin_managed_gamehacking_verification, stage_wii_gamehacking_install,
    stage_wii_gamehacking_removal,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-wii-gamehacking-e2e-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self, value: &str) -> PathBuf {
        self.root.join(value)
    }

    fn write(&self, value: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(value);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn cheat(id: &str, name: &str, format: WiiCodeFormat, line: &str) -> GameHackingWiiCheat {
    GameHackingWiiCheat {
        id: id.to_string(),
        name: name.to_string(),
        author: Some("Fixture Author".to_string()),
        description: None,
        code_format: format,
        safety: WiiCheatSafety::Installable,
        safety_warnings: Vec::new(),
        code_lines: vec![line.to_string()],
        source_game_id: 131936,
        source_url: "https://gamehacking.org/game/131936".to_string(),
    }
}

struct Workflow {
    fixture: Fixture,
    configuration_path: PathBuf,
    archive: PathBuf,
    staging_root: PathBuf,
    history_root: PathBuf,
    backup_root: PathBuf,
}

impl Workflow {
    fn new(existing: Option<&str>) -> Self {
        let fixture = Fixture::new("workflow");
        let configuration_path = fixture.path("Dolphin/User");
        fs::create_dir_all(&configuration_path).unwrap();
        if let Some(existing) = existing {
            fixture.write("Dolphin/User/GameSettings/R3HX6Z.ini", existing.as_bytes());
        }
        let archive = fixture.write(
            "library/agent-hugo.iso",
            b"fixture identity lives elsewhere",
        );
        let staging_root = fixture.path("managed/generated-wii-gamehacking");
        let history_root = fixture.path("managed/history");
        let backup_root = fixture.path("managed/backups");
        fs::create_dir_all(&history_root).unwrap();
        fs::create_dir_all(&backup_root).unwrap();
        Self {
            fixture,
            configuration_path,
            archive,
            staging_root,
            history_root,
            backup_root,
        }
    }

    fn destination(&self) -> PathBuf {
        self.configuration_path.join("GameSettings/R3HX6Z.ini")
    }

    fn loaded(&self) -> LoadedDolphinDestination {
        load_dolphin_destination(&self.configuration_path, "R3HX6Z").unwrap()
    }

    fn cheats(&self) -> Vec<GameHackingWiiCheat> {
        vec![
            cheat(
                "1",
                "Infinite Tickets",
                WiiCodeFormat::Gecko,
                "040D30C8 3860270F",
            ),
            cheat(
                "2",
                "Infinite Health",
                WiiCodeFormat::ActionReplay,
                "04123456 60000000",
            ),
        ]
    }

    fn preview(&self) -> archivefs_core::patch_manager::GameCubeGameHackingInstallPreview {
        let loaded = self.loaded();
        let staged = stage_wii_gamehacking_install(
            &self.staging_root,
            "R3HX6Z.ini",
            &loaded.document,
            loaded.existed,
            &self.cheats(),
            &[0, 1],
        )
        .unwrap();
        build_wii_gamehacking_install_preview(&GameCubeGameHackingInstallPreviewRequest {
            selected_archive: self.archive.clone(),
            configuration_path: self.configuration_path.clone(),
            game_id: "R3HX6Z".to_string(),
            revision: None,
            staged,
        })
        .unwrap()
    }

    fn apply(
        &self,
        preview: &archivefs_core::patch_manager::GameCubeGameHackingInstallPreview,
    ) -> archivefs_core::patch_manager::SharedApplyResult {
        let mut plan = build_shared_transaction_plan(
            &preview.report,
            "test-wii-dolphin-profile",
            "Dolphin Wii GameSettings",
            &self.staging_root,
        )
        .unwrap();
        require_dolphin_managed_gamehacking_verification(
            &mut plan,
            managed_names(&parse_dolphin_ini(&preview.staged.contents))
                .into_iter()
                .collect(),
        )
        .unwrap();
        execute_shared_apply(
            &plan,
            &SharedApplyOptions {
                dry_run: false,
                confirmation: Some(SharedApplyConfirmation {
                    plan_id: plan.plan_id.clone(),
                    general_approved: true,
                    replacement_approved: true,
                }),
                operation_id: format!("wii-install-{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
                timestamp_unix_seconds: 1_700_001_000,
                current_context: plan.context.clone(),
                history_root: self.history_root.clone(),
                backup_root: self.backup_root.clone(),
            },
        )
    }
}

#[test]
fn wii_mixed_install_preserves_existing_ini_and_verifies_exact_live_target() {
    let workflow = Workflow::new(Some("[Core]\nCPUThread = True\n"));
    let preview = workflow.preview();
    assert_eq!(preview.report.request_archive, workflow.archive);
    assert_eq!(
        preview.report.entries[0].destination_path.as_deref(),
        Some(workflow.destination().as_path())
    );
    assert_ne!(preview.staged.path, workflow.destination());
    let result = workflow.apply(&preview);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let live = fs::read_to_string(workflow.destination()).unwrap();
    assert_eq!(live, preview.staged.contents);
    assert!(live.contains("[Core]\nCPUThread = True"));
    assert!(live.contains("[Gecko]"));
    assert!(live.contains("[ActionReplay]"));
    assert!(live.contains("[ArchiveFS_Managed_GameHacking]"));
}

#[test]
fn wii_reinstall_is_idempotent_and_managed_only_removal_preserves_user_data() {
    let workflow = Workflow::new(Some("[Core]\nCPUThread = True\n"));
    let first = workflow.preview();
    assert_eq!(
        workflow.apply(&first).journal.status,
        SharedApplyStatus::Success
    );
    let installed = fs::read(workflow.destination()).unwrap();
    let second = workflow.preview();
    assert_eq!(
        second.report.entries[0].proposed_action,
        archivefs_core::patch_manager::PreviewProposedAction::Skip
    );
    assert_eq!(
        workflow.apply(&second).journal.status,
        SharedApplyStatus::Success
    );
    assert_eq!(fs::read(workflow.destination()).unwrap(), installed);

    let loaded = workflow.loaded();
    let managed = managed_names(&loaded.document)
        .into_iter()
        .collect::<Vec<_>>();
    let staged = stage_wii_gamehacking_removal(
        &workflow.staging_root,
        "R3HX6Z.ini",
        &loaded.document,
        loaded.existed,
        &managed[..1],
    )
    .unwrap();
    assert!(staged.contents.contains("[Core]\nCPUThread = True"));
    assert_eq!(managed_names(&parse_dolphin_ini(&staged.contents)).len(), 1);
}

#[test]
fn wii_install_undo_restores_exact_previous_ini() {
    let original = "[Core]\nCPUThread = True\n[Video_Settings]\nMSAA = 4\n";
    let workflow = Workflow::new(Some(original));
    let applied = workflow.apply(&workflow.preview());
    let journal = applied.journal_path.unwrap();
    let rollback = preview_shared_rollback(
        &journal,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    let result = execute_shared_rollback(
        &rollback,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: rollback.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "wii-rollback".to_string(),
            timestamp_unix_seconds: 1_700_001_100,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(result.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(workflow.destination()).unwrap(),
        original
    );
    let _ = &workflow.fixture;
}
