//! End-to-end behaviour for the Dolphin Gecko cheat install workflow: from
//! a real GameSettings INI on disk, through candidate matching, opening
//! the matched file, selecting individual codes, staging the surgically
//! edited file, and the journal-backed apply, to rollback.
//!
//! These tests exercise real files in a temporary directory and assert on
//! what is actually on disk and in the journal, never on source strings.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    DolphinCandidateBlockedReason, DolphinCodeSelection, DolphinInstallPreviewRequest,
    DolphinProfileDiscoveryRoots, DolphinProviderCodeSelection, GeckoProviderEntry,
    GeckoProviderResult, GeckoRegion, GeckoRevisionApplicability, SharedApplyConfirmation,
    SharedApplyOptions, SharedApplyStatus, SharedRollbackConfirmation, SharedRollbackOptions,
    SharedRollbackOutcome, build_dolphin_candidate, build_dolphin_install_preview,
    build_shared_transaction_plan, discover_dolphin_profiles, execute_shared_apply,
    execute_shared_rollback, inspect_dolphin_profile, load_dolphin_destination, load_dolphin_ini,
    preview_shared_rollback, stage_dolphin_ini, stage_dolphin_provider_ini,
};

const REAL_WORLD_INI: &str = "[Core]\n\
FastDiscSpeed = True\n\
OverclockEnable = True\n\
[Video_Settings]\n\
MSAA = 8\n\
[Gecko]\n\
$Infinite Bells [Nayr]\n\
28134C58 00000001\n\
20C9F0D4 00060000\n\
*Gives you lots of bells\n\
$Instant Growth [Nayr]\n\
C913CEF5 00000000\n\
08002FC2 00000001\n\
";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-dolphin-e2e-{label}-{}-{}-{}",
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

/// A real, eligible Dolphin profile with one real GameSettings file for
/// GAFE01 (Animal Crossing, USA, revision 0 - the exact real game this
/// milestone's manual proof used).
struct Workflow {
    configuration_path: PathBuf,
    archive: PathBuf,
    staging_root: PathBuf,
    history_root: PathBuf,
    backup_root: PathBuf,
}

impl Workflow {
    fn new(fixture: &Fixture) -> Self {
        let configuration_path = fixture.dir("dolphin");
        fixture.write("dolphin/Dolphin.ini", "[Core]\n");
        fixture.write("dolphin/GameSettings/GAFE01.ini", REAL_WORLD_INI);
        Self {
            configuration_path,
            archive: fixture.write("library/Animal Crossing (USA).iso", "iso bytes"),
            staging_root: fixture.path("managed/generated-dolphin"),
            history_root: fixture.dir("managed/history"),
            backup_root: fixture.dir("managed/backups"),
        }
    }

    fn inventory(&self) -> archivefs_core::patch_manager::DolphinGameIniInventory {
        let mut roots = DolphinProfileDiscoveryRoots {
            home: self.configuration_path.parent().unwrap().to_path_buf(),
            xdg_config_home: self.configuration_path.parent().unwrap().to_path_buf(),
            xdg_data_home: self.configuration_path.parent().unwrap().to_path_buf(),
            flatpak_system_root: self.configuration_path.parent().unwrap().to_path_buf(),
            explicit_configuration_roots: Vec::new(),
            running_commands: Vec::new(),
            selected_launch_commands: Vec::new(),
            selected_executable: None,
        };
        roots
            .explicit_configuration_roots
            .push(self.configuration_path.clone());
        let discovery = discover_dolphin_profiles(&roots).expect("discovery");
        let profile = discovery
            .profiles
            .into_iter()
            .find(|profile| profile.configuration_path == self.configuration_path)
            .expect("profile discovered");
        inspect_dolphin_profile(&profile).expect("inventory")
    }

    fn destination(&self) -> PathBuf {
        self.configuration_path
            .join("GameSettings")
            .join("GAFE01.ini")
    }

    fn prepare(
        &self,
        choose: impl FnOnce(&mut DolphinCodeSelection),
    ) -> (archivefs_core::patch_manager::SharedPreviewReport, String) {
        let inventory = self.inventory();
        let outcome = build_dolphin_candidate(&inventory, Some("E"), Some("GAFE01"), Some(0));
        let candidate = outcome.candidate.expect("candidate found");
        let loaded = load_dolphin_ini(&candidate.path).expect("loads");

        let mut selection = DolphinCodeSelection::from_document(&loaded.document);
        choose(&mut selection);
        let names = selection
            .resolve_names(&loaded.document)
            .expect("selection resolves");

        let staged = stage_dolphin_ini(&self.staging_root, "GAFE01.ini", &loaded.document, &names)
            .expect("staging succeeds");

        let preview = build_dolphin_install_preview(&DolphinInstallPreviewRequest {
            selected_archive: self.archive.clone(),
            configuration_path: self.configuration_path.clone(),
            game_id: candidate.game_id.clone(),
            revision: candidate.revision,
            staged: staged.clone(),
        })
        .expect("preview builds");

        (preview.report, staged.contents)
    }

    fn apply(
        &self,
        report: &archivefs_core::patch_manager::SharedPreviewReport,
        confirmed: bool,
        replacement_approved: bool,
    ) -> archivefs_core::patch_manager::SharedApplyResult {
        let plan = build_shared_transaction_plan(
            report,
            "test-dolphin-profile",
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
                "test-dolphin-op-{}",
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            timestamp_unix_seconds: 1_700_000_300,
            current_context: plan.context.clone(),
            history_root: self.history_root.clone(),
            backup_root: self.backup_root.clone(),
        };
        execute_shared_apply(&plan, &options)
    }
}

fn select_infinite_bells(selection: &mut DolphinCodeSelection) {
    selection.clear_all();
    assert!(selection.set_selected(0, true));
}

#[test]
fn a_selected_code_installs_to_the_real_gamesettings_destination() {
    let fixture = Fixture::new("install");
    let workflow = Workflow::new(&fixture);
    let (report, staged_contents) = workflow.prepare(select_infinite_bells);

    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("original file present");
    assert!(!before.contains("Gecko_Enabled"));

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let installed = fs::read_to_string(&destination).expect("installed file exists");
    assert_eq!(installed, staged_contents, "exactly the previewed bytes");
    assert!(installed.contains("[Gecko_Enabled]\n$Infinite Bells [Nayr]\n"));
    // Unrelated sections and the untouched code body must survive exactly.
    assert!(installed.contains("[Core]\nFastDiscSpeed = True\nOverclockEnable = True\n"));
    assert!(installed.contains("[Video_Settings]\nMSAA = 8\n"));
    assert!(installed.contains("$Instant Growth [Nayr]\n"));
}

#[test]
fn a_cancelled_install_changes_nothing_on_disk() {
    let fixture = Fixture::new("cancel");
    let workflow = Workflow::new(&fixture);
    let (report, _) = workflow.prepare(select_infinite_bells);
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("original file");

    let result = workflow.apply(&report, false, false);
    assert_eq!(result.journal.status, SharedApplyStatus::DryRun);
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        before,
        "cancellation writes nothing"
    );
}

#[test]
fn replacement_without_separate_approval_leaves_the_file_untouched() {
    let fixture = Fixture::new("no-approval");
    let workflow = Workflow::new(&fixture);
    let (report, _) = workflow.prepare(select_infinite_bells);
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("original file");

    let result = workflow.apply(&report, true, false);
    assert_ne!(result.journal.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        before,
        "an unapproved replacement leaves the original intact"
    );
}

#[test]
fn install_and_rollback_round_trip_restores_the_exact_original_file() {
    let fixture = Fixture::new("rollback");
    let workflow = Workflow::new(&fixture);
    let destination = workflow.destination();
    let original = fs::read_to_string(&destination).expect("original file");

    let (report, _) = workflow.prepare(select_infinite_bells);
    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let entry = result.journal.entries.first().expect("one entry");
    assert!(
        entry.backup_path.is_some(),
        "replacing an existing file retains a backup"
    );

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
            rollback_operation_id: "test-dolphin-rollback".to_string(),
            timestamp_unix_seconds: 1_700_000_400,
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
        "the previous GameSettings file is back, byte for byte - including Instant Growth, \
         which was never disturbed"
    );
}

#[test]
fn installing_never_leaves_a_partially_written_destination_on_failure_path() {
    // The staged file itself is written atomically (temp + rename); this
    // asserts the staged artifact used for apply is never partially
    // present under the staging root either.
    let fixture = Fixture::new("atomic-stage");
    let workflow = Workflow::new(&fixture);
    let (_, staged_contents) = workflow.prepare(select_infinite_bells);
    let staged_path = workflow.staging_root.join("GAFE01.ini");
    assert_eq!(
        fs::read_to_string(&staged_path).expect("staged file exists"),
        staged_contents
    );
    assert!(
        !workflow.staging_root.join(".GAFE01.ini.partial").exists(),
        "no temporary artifact is left behind"
    );
}

#[test]
fn a_revision_mismatch_never_reaches_the_write_path() {
    let fixture = Fixture::new("revision-mismatch");
    let workflow = Workflow::new(&fixture);
    let inventory = workflow.inventory();
    let outcome = build_dolphin_candidate(&inventory, Some("E"), Some("GAFE01"), Some(2));
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::RevisionMismatch)
    );
}

#[test]
fn a_wrong_game_id_never_reaches_the_write_path() {
    let fixture = Fixture::new("wrong-game");
    let workflow = Workflow::new(&fixture);
    let inventory = workflow.inventory();
    let outcome = build_dolphin_candidate(&inventory, None, Some("GALE01"), Some(0));
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::NoMatchingIniFound)
    );
}

#[test]
fn selecting_zero_codes_blocks_before_any_write() {
    let fixture = Fixture::new("zero-selected");
    let workflow = Workflow::new(&fixture);
    let inventory = workflow.inventory();
    let outcome = build_dolphin_candidate(&inventory, Some("E"), Some("GAFE01"), Some(0));
    let candidate = outcome.candidate.expect("candidate");
    let loaded = load_dolphin_ini(&candidate.path).expect("loads");
    let selection = DolphinCodeSelection::from_document(&loaded.document);
    assert!(!selection.can_apply());
    assert!(selection.resolve_names(&loaded.document).is_err());
}

fn external_gafe01_provider() -> GeckoProviderResult {
    GeckoProviderResult {
        provider_id: "dolphin_upstream_gamesettings".to_string(),
        provider_display_name: "Dolphin upstream GameSettings".to_string(),
        source_identity: "fixture:GAFE01.ini".to_string(),
        retrieved_at_unix_seconds: 1,
        game_id: "GAFE01".to_string(),
        title: Some("Animal Crossing".to_string()),
        region: GeckoRegion::Usa,
        revision: 0,
        entries: vec![GeckoProviderEntry {
            provider_entry_id: "gafe01-widescreen".to_string(),
            name: "16:9 Widescreen".to_string(),
            code_lines: vec![
                "040037A0 3C608000".to_string(),
                "040037A4 C38337AC".to_string(),
                "040037A8 4805ACBC".to_string(),
                "040037AC 3FE38E39".to_string(),
                "0405E460 4BFA5340".to_string(),
            ],
            notes: Vec::new(),
            region: GeckoRegion::Usa,
            revision_applicability: GeckoRevisionApplicability::Uncertain,
            parse_warnings: vec!["revision applicability is uncertain".to_string()],
            safe_to_offer: true,
        }],
        warnings: Vec::new(),
        attribution: "Dolphin upstream".to_string(),
        license: "GPL-2.0-or-later".to_string(),
    }
}

struct AppliedExternalProvider {
    configuration_path: PathBuf,
    destination: PathBuf,
    history_root: PathBuf,
    backup_root: PathBuf,
    journal_path: PathBuf,
}

fn apply_external_provider(
    fixture: &Fixture,
    operation: &str,
    create_game_settings: bool,
    previous: Option<&[u8]>,
) -> AppliedExternalProvider {
    let configuration_path = fixture.dir("dolphin");
    let game_settings = configuration_path.join("GameSettings");
    if create_game_settings || previous.is_some() {
        fs::create_dir(&game_settings).unwrap();
    }
    let destination_path = game_settings.join("GAFE01.ini");
    if let Some(previous) = previous {
        fs::write(&destination_path, previous).unwrap();
    }
    let archive = fixture.write("library/Animal Crossing (USA).zip", "zip fixture");
    let staging_root = fixture.path("managed/generated-dolphin");
    let history_root = fixture.dir("managed/history");
    let backup_root = fixture.dir("managed/backups");
    let destination = load_dolphin_destination(&configuration_path, "GAFE01").unwrap();
    assert_eq!(destination.existed, previous.is_some());
    let provider = external_gafe01_provider();
    let mut selection = DolphinProviderCodeSelection::from_provider(&provider, &destination);
    selection.select_all();
    let staged =
        stage_dolphin_provider_ini(&staging_root, &destination, &provider, &selection).unwrap();
    let preview = build_dolphin_install_preview(&DolphinInstallPreviewRequest {
        selected_archive: archive,
        configuration_path: configuration_path.clone(),
        game_id: "GAFE01".to_string(),
        revision: Some(0),
        staged: staged.clone(),
    })
    .unwrap();
    let plan = build_shared_transaction_plan(
        &preview.report,
        "test-dolphin-profile",
        "Dolphin upstream GameSettings",
        &staging_root,
    )
    .unwrap();
    let applied = execute_shared_apply(
        &plan,
        &SharedApplyOptions {
            dry_run: false,
            confirmation: Some(SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: true,
                replacement_approved: previous.is_some(),
            }),
            operation_id: operation.to_string(),
            timestamp_unix_seconds: 1_700_000_500,
            current_context: plan.context.clone(),
            history_root: history_root.clone(),
            backup_root: backup_root.clone(),
        },
    );
    assert_eq!(
        applied.journal.status,
        SharedApplyStatus::Success,
        "{applied:?}"
    );
    assert_eq!(
        fs::read_to_string(&destination.path).unwrap(),
        staged.contents
    );
    let entry = &applied.journal.entries[0];
    assert_eq!(
        entry.destination_existed_before_apply,
        Some(previous.is_some())
    );
    assert_eq!(
        entry.destination_parent_existed_before_apply,
        Some(create_game_settings || previous.is_some())
    );
    if let Some(previous) = previous {
        let backup = entry.backup_path.as_ref().unwrap().to_path_buf().unwrap();
        assert_eq!(fs::read(backup).unwrap(), previous);
    } else {
        assert!(entry.backup_path.is_none());
    }
    AppliedExternalProvider {
        configuration_path,
        destination: destination_path,
        history_root,
        backup_root,
        journal_path: applied.journal_path.unwrap(),
    }
}

fn rollback_external_provider(
    applied: &AppliedExternalProvider,
    operation: &str,
) -> archivefs_core::patch_manager::SharedRollbackResult {
    let rollback_preview = preview_shared_rollback(
        &applied.journal_path,
        &applied.configuration_path,
        &applied.backup_root,
    );
    assert!(rollback_preview.available);
    execute_shared_rollback(
        &rollback_preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: rollback_preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: operation.to_string(),
            timestamp_unix_seconds: 1_700_000_600,
            history_root: applied.history_root.clone(),
            backup_root: applied.backup_root.clone(),
        },
    )
}

#[test]
fn absent_destination_in_existing_directory_is_removed_but_directory_remains() {
    let fixture = Fixture::new("provider-existing-parent");
    let applied = apply_external_provider(&fixture, "existing-parent", true, None);
    let rollback = rollback_external_provider(&applied, "existing-parent-rollback");
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(!applied.destination.exists());
    assert!(applied.destination.parent().unwrap().is_dir());

    let repeated = preview_shared_rollback(
        &applied.journal_path,
        &applied.configuration_path,
        &applied.backup_root,
    );
    assert!(!repeated.available);
    assert_eq!(
        repeated.entries[0].outcome,
        SharedRollbackOutcome::AlreadyRolledBack
    );
}

#[test]
fn absent_destination_and_directory_are_both_removed_on_rollback() {
    let fixture = Fixture::new("provider-missing-parent");
    let applied = apply_external_provider(&fixture, "missing-parent", false, None);
    let game_settings = applied.destination.parent().unwrap().to_path_buf();
    assert!(game_settings.is_dir());
    let rollback = rollback_external_provider(&applied, "missing-parent-rollback");
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(!applied.destination.exists());
    assert!(!game_settings.exists());
}

#[test]
fn preexisting_empty_destination_is_restored_and_kept() {
    let fixture = Fixture::new("provider-empty-existing");
    let applied = apply_external_provider(&fixture, "empty-existing", true, Some(b""));
    let rollback = rollback_external_provider(&applied, "empty-existing-rollback");
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(applied.destination.is_file());
    assert_eq!(fs::read(&applied.destination).unwrap(), b"");
    assert!(applied.destination.parent().unwrap().is_dir());
}

#[test]
fn preexisting_nonempty_destination_is_restored_byte_for_byte() {
    let fixture = Fixture::new("provider-nonempty-existing");
    let previous = b"[Core]\nFastDiscSpeed = True\n";
    let applied = apply_external_provider(&fixture, "nonempty-existing", true, Some(previous));
    let rollback = rollback_external_provider(&applied, "nonempty-existing-rollback");
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert_eq!(fs::read(&applied.destination).unwrap(), previous);
}

#[test]
fn unrelated_file_prevents_created_directory_removal() {
    let fixture = Fixture::new("provider-unrelated");
    let applied = apply_external_provider(&fixture, "unrelated", false, None);
    let game_settings = applied.destination.parent().unwrap();
    let unrelated = game_settings.join("OTHER01.ini");
    fs::write(&unrelated, b"unrelated").unwrap();
    let rollback = rollback_external_provider(&applied, "unrelated-rollback");
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(!applied.destination.exists());
    assert_eq!(fs::read(unrelated).unwrap(), b"unrelated");
    assert!(game_settings.is_dir());
}

#[cfg(unix)]
#[test]
fn symlinked_gamesettings_escape_remains_blocked() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("provider-symlink");
    let configuration_path = fixture.dir("dolphin");
    let outside = fixture.dir("outside");
    symlink(outside, configuration_path.join("GameSettings")).unwrap();
    let error = load_dolphin_destination(&configuration_path, "GAFE01").unwrap_err();
    assert_eq!(
        error.kind,
        archivefs_core::patch_manager::DolphinInstallPlanErrorKind::DestinationUnsafe
    );
}
