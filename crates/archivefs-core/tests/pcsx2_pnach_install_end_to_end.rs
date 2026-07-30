use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    ManagedPnachCheat, Pcsx2GameIdentity, Pcsx2IdentityState, Pcsx2InstallPreviewRequest,
    Pcsx2InstallationType, Pcsx2PatchCategory, Pcsx2PatchDirectory, Pcsx2PatchDirectoryState,
    Pcsx2Profile, Pcsx2ProfileDiscoveryRoots, Pcsx2ProfileScope, PnachPatchLine,
    SharedApplyConfirmation, SharedApplyOptions, SharedApplyResult, SharedApplyStatus,
    SharedRollbackConfirmation, SharedRollbackOptions, SharedRollbackOutcome,
    build_pcsx2_install_preview, build_shared_transaction_plan, confirmed_pcsx2_profile,
    discover_pcsx2_profiles, execute_shared_apply, execute_shared_rollback,
    preview_shared_rollback, stage_pcsx2_pnach,
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
fn missing_target_file_is_created_atomically_with_a_journal() {
    let fixture = Fixture::new("missing-target");
    assert!(!fixture.destination().exists());
    let result = install(&fixture, "missing-target", &[cheat("health", "20123456")]);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let journal_path = result
        .journal_path
        .as_ref()
        .expect("a successful install writes a journal");
    assert!(journal_path.exists());
    assert!(fixture.destination().exists());
    // No stray temp/partial files were left in the destination directory.
    let leftovers: Vec<_> = fs::read_dir(fixture.destination().parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().contains(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
}

#[test]
fn zero_byte_existing_target_is_treated_as_valid_empty_content_and_populated_safely() {
    let fixture = Fixture::new("zero-byte-existing");
    fs::create_dir(fixture.profile_root().join("cheats")).unwrap();
    fs::write(fixture.destination(), b"").unwrap();
    assert_eq!(fs::metadata(fixture.destination()).unwrap().len(), 0);
    let result = install(
        &fixture,
        "zero-byte-existing",
        &[cheat("health", "20123456")],
    );
    assert_eq!(
        result.journal.status,
        SharedApplyStatus::Success,
        "entries: {:#?}",
        result.journal.entries
    );
    let installed = String::from_utf8(fs::read(fixture.destination()).unwrap()).unwrap();
    assert!(installed.contains("// ArchiveFS managed block: health"));
    assert!(installed.contains("patch=1,EE,20123456,word,00000001"));
    assert_eq!(
        undo(&fixture, &result, "undo-zero-byte-existing"),
        SharedApplyStatus::Success
    );
    // Undo restores the file to its original (empty) content, not deletion,
    // since the destination already existed before this operation.
    assert!(fixture.destination().exists());
    assert_eq!(fs::metadata(fixture.destination()).unwrap().len(), 0);
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

/// A disposable "AppImage" fixture: a directory that stands in for the one
/// containing a running `.AppImage`, with PCSX2 evidence written directly
/// beside it, matching PCSX2's documented portable-mode layout (a
/// `portable.ini` marker file next to the executable, data stored in that
/// same directory).
struct AppImageFixture(PathBuf);

impl AppImageFixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-pcsx2-appimage-e2e-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let appimage_dir = root.join("appimage-dir");
        fs::create_dir_all(appimage_dir.join("inis")).unwrap();
        fs::write(appimage_dir.join("portable.ini"), b"").unwrap();
        fs::write(
            appimage_dir.join("game.iso"),
            b"immutable game image fixture",
        )
        .unwrap();
        Self(root)
    }

    fn appimage_dir(&self) -> PathBuf {
        self.0.join("appimage-dir")
    }

    fn history(&self) -> PathBuf {
        self.0.join("archivefs-history")
    }

    fn backups(&self) -> PathBuf {
        self.0.join("archivefs-backups")
    }
}

impl Drop for AppImageFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn appimage_adjacent_portable_profile_installs_named_grouped_cheat_with_journal_and_rollback() {
    let fixture = AppImageFixture::new("portable-install");
    // An isolated root tree, not the real environment: discovery must find
    // only the AppImage-adjacent profile, so confirmation is unambiguous.
    let roots = Pcsx2ProfileDiscoveryRoots {
        home: fixture.0.join("isolated-home"),
        xdg_config_home: fixture.0.join("isolated-home/.config"),
        xdg_data_home: fixture.0.join("isolated-home/.local/share"),
        documents_home: fixture.0.join("isolated-home/Documents"),
        flatpak_system_root: fixture.0.join("isolated-system-flatpak"),
        appimage_directory: Some(fixture.appimage_dir()),
        portable_configuration_roots: Vec::new(),
    };
    let discovery = discover_pcsx2_profiles(&roots).unwrap();
    let profile = confirmed_pcsx2_profile(&discovery, None)
        .expect("the AppImage-adjacent portable profile is discovered and unambiguous")
        .clone();
    assert_eq!(profile.installation_type, Pcsx2InstallationType::Portable);
    assert_eq!(profile.configuration_path, fixture.appimage_dir());

    let identity = Pcsx2GameIdentity {
        archive_path: fixture.appimage_dir().join("game.iso"),
        title: "Fixture Game".to_string(),
        region: Some("NTSC-U".to_string()),
        serial: Some("SLUS-20312".to_string()),
        executable_crc: Some("A1B2C3D4".to_string()),
        state: Pcsx2IdentityState::Verified,
        evidence: vec!["exact fixture bytes".to_string()],
        plain_failure_reason: None,
    };
    let selected = vec![ManagedPnachCheat {
        id: "infinite-health".to_string(),
        name: "Infinite health".to_string(),
        description: Some("Author: Codejunkies | GameHacking game ID: 42".to_string()),
        patch_lines: vec![PnachPatchLine::parse("patch=1,EE,20123456,word,00000001").unwrap()],
    }];

    let staged = stage_pcsx2_pnach(
        &fixture.0.join("staging"),
        &profile,
        identity.verified_crc().unwrap(),
        &selected,
    )
    .unwrap();
    let preview = build_pcsx2_install_preview(&Pcsx2InstallPreviewRequest {
        selected_archive: identity.archive_path.clone(),
        profile: profile.clone(),
        identity,
        staged,
    })
    .unwrap();
    assert_eq!(preview.report.summary.blocked, 0);
    let plan = build_shared_transaction_plan(
        &preview.report,
        &profile.profile_id,
        "pcsx2-managed-pnach",
        &preview.staged.staging_root,
    )
    .unwrap();
    let result = execute_shared_apply(
        &plan,
        &SharedApplyOptions {
            dry_run: false,
            confirmation: Some(SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: true,
                replacement_approved: true,
            }),
            operation_id: "appimage-install".to_string(),
            timestamp_unix_seconds: 1_700_000_100,
            current_context: plan.context.clone(),
            history_root: fixture.history(),
            backup_root: fixture.backups(),
        },
    );
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    assert!(result.journal_path.is_some(), "install journal was written");

    let destination = fixture.appimage_dir().join("cheats/A1B2C3D4.pnach");
    let installed = String::from_utf8(fs::read(&destination).unwrap()).unwrap();
    assert!(installed.contains("// ArchiveFS managed block: infinite-health"));
    assert!(installed.contains("// Infinite health"));
    assert!(installed.contains("Author: Codejunkies"));
    assert!(installed.contains("patch=1,EE,20123456,word,00000001"));

    // Rollback removes exactly the file this operation created.
    let journal = result.journal_path.as_ref().unwrap();
    let rollback_preview =
        preview_shared_rollback(journal, &fixture.appimage_dir(), &fixture.backups());
    assert!(rollback_preview.available);
    let rollback = execute_shared_rollback(
        &rollback_preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: rollback_preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "appimage-undo".to_string(),
            timestamp_unix_seconds: 1_700_000_200,
            history_root: fixture.history(),
            backup_root: fixture.backups(),
        },
    );
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(!destination.exists());

    // The emulator's game image and settings were never touched.
    assert_eq!(
        fs::read(fixture.appimage_dir().join("game.iso")).unwrap(),
        b"immutable game image fixture"
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
