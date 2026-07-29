use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    ManagedPnachCheat, Pcsx2GameIdentity, Pcsx2IdentityState, Pcsx2InstallPreviewRequest,
    Pcsx2InstallationType, Pcsx2PatchCategory, Pcsx2PatchDirectory, Pcsx2PatchDirectoryState,
    Pcsx2Profile, Pcsx2ProfileScope, PnachPatchLine, SharedApplyConfirmation, SharedApplyOptions,
    SharedApplyResult, SharedApplyStatus, SharedRollbackConfirmation, SharedRollbackOptions,
    SharedRollbackOutcome, build_pcsx2_install_preview, build_shared_transaction_plan,
    execute_shared_apply, execute_shared_rollback, preview_shared_rollback, stage_pcsx2_pnach,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-pcsx2-e2e-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("profile")).unwrap();
        fs::write(root.join("profile/PCSX2.ini"), b"[UI]\n").unwrap();
        fs::write(
            root.join("profile/game.iso"),
            b"immutable game image fixture",
        )
        .unwrap();
        Self(root)
    }

    fn profile_root(&self) -> PathBuf {
        self.0.join("profile")
    }

    fn profile(&self) -> Pcsx2Profile {
        let cheats = self.profile_root().join("cheats");
        Pcsx2Profile {
            profile_id: "fixture-profile".to_string(),
            installation_type: Pcsx2InstallationType::Portable,
            scope: Pcsx2ProfileScope::Portable,
            configuration_path: self.profile_root(),
            provenance: "disposable integration fixture",
            eligible: true,
            blockers: Vec::new(),
            patch_directories: vec![Pcsx2PatchDirectory {
                state: if cheats.exists() {
                    Pcsx2PatchDirectoryState::Available
                } else {
                    Pcsx2PatchDirectoryState::Missing
                },
                path: cheats,
                category: Pcsx2PatchCategory::Cheats,
                warning: None,
                identity: None,
            }],
            configuration_identity: None,
        }
    }

    fn identity(&self) -> Pcsx2GameIdentity {
        Pcsx2GameIdentity {
            archive_path: self.profile_root().join("game.iso"),
            title: "Fixture Game".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-20312".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
            state: Pcsx2IdentityState::Verified,
            evidence: vec!["exact fixture bytes".to_string()],
            plain_failure_reason: None,
        }
    }

    fn destination(&self) -> PathBuf {
        self.profile_root().join("cheats/A1B2C3D4.pnach")
    }

    fn history(&self) -> PathBuf {
        self.0.join("archivefs-history")
    }

    fn backups(&self) -> PathBuf {
        self.0.join("archivefs-backups")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cheat(id: &str, address: &str) -> ManagedPnachCheat {
    ManagedPnachCheat {
        id: id.to_string(),
        name: format!("Fixture {id}"),
        description: Some("Disposable test code".to_string()),
        patch_lines: vec![
            PnachPatchLine::parse(&format!("patch=1,EE,{address},word,00000001")).unwrap(),
        ],
    }
}

fn install(
    fixture: &Fixture,
    operation: &str,
    selected: &[ManagedPnachCheat],
) -> SharedApplyResult {
    let profile = fixture.profile();
    let identity = fixture.identity();
    let staged = stage_pcsx2_pnach(
        &fixture.0.join(format!("staging-{operation}")),
        &profile,
        identity.verified_crc().unwrap(),
        selected,
    )
    .unwrap();
    let preview = build_pcsx2_install_preview(&Pcsx2InstallPreviewRequest {
        selected_archive: identity.archive_path.clone(),
        profile,
        identity,
        staged,
    })
    .unwrap();
    assert_eq!(preview.report.summary.blocked, 0);
    let plan = build_shared_transaction_plan(
        &preview.report,
        "fixture-profile",
        "pcsx2-managed-pnach",
        &preview.staged.staging_root,
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
            operation_id: operation.to_string(),
            timestamp_unix_seconds: 1_700_000_000,
            current_context: plan.context.clone(),
            history_root: fixture.history(),
            backup_root: fixture.backups(),
        },
    )
}

fn undo(fixture: &Fixture, result: &SharedApplyResult, operation: &str) -> SharedApplyStatus {
    let journal = result.journal_path.as_ref().unwrap();
    let preview = preview_shared_rollback(journal, &fixture.profile_root(), &fixture.backups());
    assert!(preview.available);
    execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: operation.to_string(),
            timestamp_unix_seconds: 1_700_000_001,
            history_root: fixture.history(),
            backup_root: fixture.backups(),
        },
    )
    .status
}

#[test]
fn new_file_install_and_undo_remove_only_the_created_pnach() {
    let fixture = Fixture::new("new-file");
    let rom_before = fs::read(fixture.profile_root().join("game.iso")).unwrap();
    let result = install(&fixture, "new-file", &[cheat("health", "20123456")]);
    assert_eq!(
        result.journal.status,
        SharedApplyStatus::Success,
        "entries: {:#?}",
        result.journal.entries
    );
    assert!(fixture.destination().exists());
    assert_eq!(
        undo(&fixture, &result, "undo-new"),
        SharedApplyStatus::Success
    );
    assert!(!fixture.destination().exists());
    assert_eq!(
        fs::read(fixture.profile_root().join("game.iso")).unwrap(),
        rom_before
    );
}

#[test]
fn existing_file_install_preserves_content_and_undo_restores_exact_bytes() {
    let fixture = Fixture::new("existing");
    fs::create_dir(fixture.profile_root().join("cheats")).unwrap();
    let original = b"// user bytes\r\nunknown=preserve\r\npatch=0,EE,00100000,word,0\r\n";
    fs::write(fixture.destination(), original).unwrap();
    let result = install(&fixture, "existing", &[cheat("health", "20123456")]);
    let installed = fs::read(fixture.destination()).unwrap();
    assert!(installed.starts_with(original));
    assert_ne!(installed, original);
    assert_eq!(
        undo(&fixture, &result, "undo-existing"),
        SharedApplyStatus::Success
    );
    assert_eq!(fs::read(fixture.destination()).unwrap(), original);
}

#[test]
fn later_operation_is_never_destroyed_by_older_undo() {
    let fixture = Fixture::new("stacked");
    let first = install(&fixture, "first", &[cheat("health", "20123456")]);
    let second = install(&fixture, "second", &[cheat("ammo", "20123460")]);
    let bytes = String::from_utf8(fs::read(fixture.destination()).unwrap()).unwrap();
    assert!(bytes.contains("managed block: health"));
    assert!(bytes.contains("managed block: ammo"));

    let older_preview = preview_shared_rollback(
        first.journal_path.as_ref().unwrap(),
        &fixture.profile_root(),
        &fixture.backups(),
    );
    assert!(!older_preview.available);
    assert_eq!(
        older_preview.entries[0].outcome,
        SharedRollbackOutcome::DestinationChanged
    );
    assert_eq!(
        undo(&fixture, &second, "undo-second"),
        SharedApplyStatus::Success
    );
    let after_second_undo = String::from_utf8(fs::read(fixture.destination()).unwrap()).unwrap();
    assert!(after_second_undo.contains("managed block: health"));
    assert!(!after_second_undo.contains("managed block: ammo"));
    assert_eq!(
        undo(&fixture, &first, "undo-first"),
        SharedApplyStatus::Success
    );
    assert!(!fixture.destination().exists());
}

#[test]
fn missing_backup_and_external_change_block_undo() {
    let fixture = Fixture::new("rollback-blockers");
    fs::create_dir(fixture.profile_root().join("cheats")).unwrap();
    fs::write(fixture.destination(), b"original\n").unwrap();
    let result = install(&fixture, "replace", &[cheat("health", "20123456")]);
    let backup = result.journal.entries[0]
        .backup_path
        .as_ref()
        .unwrap()
        .to_path_buf()
        .unwrap();
    fs::remove_file(backup).unwrap();
    let missing = preview_shared_rollback(
        result.journal_path.as_ref().unwrap(),
        &fixture.profile_root(),
        &fixture.backups(),
    );
    assert!(!missing.available);

    let fresh = Fixture::new("external-change");
    let result = install(&fresh, "new", &[cheat("health", "20123456")]);
    fs::write(fresh.destination(), b"external user edit\n").unwrap();
    let changed = preview_shared_rollback(
        result.journal_path.as_ref().unwrap(),
        &fresh.profile_root(),
        &fresh.backups(),
    );
    assert!(!changed.available);
    assert_eq!(
        changed.entries[0].outcome,
        SharedRollbackOutcome::DestinationChanged
    );
}

#[test]
fn unwritable_profile_fails_without_touching_rom_or_creating_pnach() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new("permission");
        let rom_before = fs::read(fixture.profile_root().join("game.iso")).unwrap();
        fs::set_permissions(fixture.profile_root(), fs::Permissions::from_mode(0o500)).unwrap();
        let probe = fixture.profile_root().join("permission-probe");
        if fs::write(&probe, b"probe").is_ok() {
            let _ = fs::remove_file(probe);
            fs::set_permissions(fixture.profile_root(), fs::Permissions::from_mode(0o700)).unwrap();
            return; // privileged test runner cannot exercise Unix permission denial
        }
        let result = install(&fixture, "permission", &[cheat("health", "20123456")]);
        assert_eq!(result.journal.status, SharedApplyStatus::Failed);
        fs::set_permissions(fixture.profile_root(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!fixture.destination().exists());
        assert_eq!(
            fs::read(fixture.profile_root().join("game.iso")).unwrap(),
            rom_before
        );
    }
}
