//! End-to-end behaviour for the RetroArch cheat install workflow: from a
//! catalogue directory on disk, through candidate matching, individual
//! cheat selection, destination resolution, generation, and the
//! journal-backed apply, to rollback.
//!
//! These tests exercise real files in a temporary directory. They assert on
//! what is on disk and what the journal records, never on source strings.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::emulator_environment::HostReadOnlyFilesystem;
use archivefs_core::patch_manager::{
    CheatCandidateArchive, CheatCandidateClassification, CheatCandidateOptions,
    CheatDestinationRequest, CheatInstallPreviewRequest, CheatSelection, SharedApplyConfirmation,
    SharedApplyOptions, SharedApplyResult, SharedApplyStatus, SharedRollbackConfirmation,
    SharedRollbackOptions, SharedRollbackOutcome, build_cheat_candidates,
    build_cheat_install_preview, build_shared_transaction_plan, execute_shared_apply,
    execute_shared_rollback, load_candidate_document, load_cheat_catalogue_snapshot,
    match_strength_for_candidate, preview_shared_rollback, resolve_cheat_destination,
    stage_generated_cheat_file,
};

const PLATFORM: &str = "Nintendo - Nintendo Entertainment System";
const CATALOGUE_CHT: &str = "cheats = 3\n\
\n\
cheat0_desc = \"Infinite Health\"\n\
cheat0_code = \"NNVOSPVG\"\n\
cheat0_enable = false\n\
\n\
cheat1_desc = \"Infinite Lives\"\n\
cheat1_code = \"SZNKZOVK\"\n\
cheat1_enable = false\n\
\n\
cheat2_desc = \"Start with 9 Bombs\"\n\
cheat2_code = \"PANKGOLA\"\n\
cheat2_enable = true\n";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-cheat-e2e-{label}-{}-{}-{}",
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

/// One fully wired workflow, stopping just before apply. Mirrors exactly
/// what the GUI does between "archive selected" and "confirm".
struct Workflow {
    catalogue_root: PathBuf,
    archive: PathBuf,
    cheat_root: PathBuf,
    staging_root: PathBuf,
    history_root: PathBuf,
    backup_root: PathBuf,
}

impl Workflow {
    fn new(fixture: &Fixture) -> Self {
        let catalogue_root = fixture.dir("catalogue");
        fixture.write(
            &format!("catalogue/{PLATFORM}/Chrono Quest (USA).cht"),
            CATALOGUE_CHT,
        );
        Self {
            catalogue_root,
            archive: fixture.write("library/Chrono Quest (USA).zip", "archive bytes"),
            cheat_root: {
                let root = fixture.dir("retroarch/cheats");
                // A real RetroArch cheat root already has one directory per
                // libretro database name; the resolver must install into it.
                fixture.dir(&format!("retroarch/cheats/{PLATFORM}"));
                root
            },
            staging_root: fixture.path("managed/generated-cheats"),
            history_root: fixture.dir("managed/history"),
            backup_root: fixture.dir("managed/backups"),
        }
    }

    fn candidate_archive(&self) -> CheatCandidateArchive {
        CheatCandidateArchive {
            display_name: "Chrono Quest (USA)".to_string(),
            platform: Some(PLATFORM.to_string()),
            region: Some("USA".to_string()),
            content_basename: Some("Chrono Quest (USA)".to_string()),
            ..CheatCandidateArchive::default()
        }
    }

    /// Runs matching, opens the best candidate, applies `choose` to the
    /// picker, and returns the built preview plus the staged file.
    fn prepare(
        &self,
        choose: impl FnOnce(&mut CheatSelection),
    ) -> (
        archivefs_core::patch_manager::SharedPreviewReport,
        PathBuf,
        String,
    ) {
        let snapshot = load_cheat_catalogue_snapshot(
            &HostReadOnlyFilesystem,
            "test-catalogue",
            &self.catalogue_root,
        );
        let list = build_cheat_candidates(
            &snapshot,
            &self.candidate_archive(),
            &CheatCandidateOptions::default(),
        );
        let candidate = list
            .candidates
            .first()
            .expect("a candidate was found")
            .clone();
        let loaded = load_candidate_document(
            &self.catalogue_root,
            &candidate.catalogue_relative_path,
            candidate.source_file_hash.as_deref(),
        )
        .expect("candidate loads");

        let mut selection = CheatSelection::from_document(&loaded.document);
        choose(&mut selection);
        let entries = selection
            .resolve(&loaded.document)
            .expect("selection resolves");

        let destination = resolve_cheat_destination(&CheatDestinationRequest {
            profile_cheat_root: self.cheat_root.clone(),
            platform: Some(PLATFORM.to_string()),
            content_basename: Some("Chrono Quest (USA)".to_string()),
            playlist_name: None,
            catalogue_name: candidate.display_name.clone(),
        })
        .expect("destination resolves");

        let staged =
            stage_generated_cheat_file(&self.staging_root, "Chrono Quest (USA)", &entries, &[])
                .expect("staging succeeds");

        let preview = build_cheat_install_preview(&CheatInstallPreviewRequest {
            selected_archive: self.archive.clone(),
            platform: Some(PLATFORM.to_string()),
            verified_identity: format!("test:{}", loaded.digest),
            destination: destination.clone(),
            profile_cheat_root: self.cheat_root.clone(),
            staged: staged.clone(),
            match_strength: match_strength_for_candidate(&candidate).expect("installable"),
        })
        .expect("preview builds");

        (preview.report, destination.path, staged.contents)
    }

    fn apply(
        &self,
        report: &archivefs_core::patch_manager::SharedPreviewReport,
        confirmed: bool,
        replacement_approved: bool,
    ) -> SharedApplyResult {
        let plan = build_shared_transaction_plan(
            report,
            "test-profile",
            "ArchiveFS cached catalogue",
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
            operation_id: format!("test-op-{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
            timestamp_unix_seconds: 1_700_000_000,
            current_context: plan.context.clone(),
            history_root: self.history_root.clone(),
            backup_root: self.backup_root.clone(),
        };
        execute_shared_apply(&plan, &options)
    }
}

fn select_first_and_third(selection: &mut CheatSelection) {
    assert!(selection.set_selected(0, true));
    assert!(selection.set_selected(2, true));
}

// -----------------------------------------------------------------------

#[test]
fn a_selected_subset_installs_to_the_real_retroarch_destination() {
    let fixture = Fixture::new("install");
    let workflow = Workflow::new(&fixture);
    let (report, destination, staged_contents) = workflow.prepare(select_first_and_third);

    assert_eq!(
        destination,
        workflow
            .cheat_root
            .join(PLATFORM)
            .join("Chrono Quest (USA).cht"),
        "the file lands in the profile's own existing libretro cheat directory"
    );
    assert!(!destination.exists(), "nothing is written before apply");

    let result = workflow.apply(&report, true, false);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let installed = fs::read_to_string(&destination).expect("installed file exists");
    assert_eq!(installed, staged_contents, "exactly the previewed bytes");
    assert!(installed.contains("cheats = 2"));
    assert!(installed.contains("Infinite Health"));
    assert!(installed.contains("Start with 9 Bombs"));
    assert!(
        !installed.contains("Infinite Lives"),
        "an unselected cheat is never written"
    );
    assert!(
        installed.contains("cheat0_desc") && installed.contains("cheat1_desc"),
        "output indexes are contiguous from zero: {installed}"
    );
    assert!(!installed.contains("cheat2_desc"));
    assert!(installed.ends_with('\n'));
}

#[test]
fn an_explicitly_chosen_ambiguous_candidate_still_installs() {
    // Two catalogue files with the same normalized title on the same
    // platform tie in ranking and are both classified Ambiguous - real,
    // common shape for a multi-region ROM set (e.g. several "Metroid"
    // region variants all matching a title-only archive). Choosing one of
    // them explicitly must not then be blocked again by shared_preview's
    // own "ambiguous means unresolved" rule, which exists to stop an
    // *automatic* process from guessing, not to second-guess a human's
    // explicit choice.
    let fixture = Fixture::new("ambiguous-explicit-choice");
    let catalogue_root = fixture.dir("catalogue");
    fixture.write(
        &format!("catalogue/{PLATFORM}/Chrono Quest (Europe).cht"),
        CATALOGUE_CHT,
    );
    fixture.write(
        &format!("catalogue/{PLATFORM}/Chrono Quest (Beta).cht"),
        CATALOGUE_CHT,
    );
    let cheat_root = fixture.dir("retroarch/cheats");
    fixture.dir(&format!("retroarch/cheats/{PLATFORM}"));
    let staging_root = fixture.path("managed/generated-cheats");
    let history_root = fixture.dir("managed/history");
    let backup_root = fixture.dir("managed/backups");
    let archive = fixture.write("library/Chrono Quest.zip", "archive bytes");

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "test", &catalogue_root);
    let list = build_cheat_candidates(
        &snapshot,
        &CheatCandidateArchive {
            display_name: "Chrono Quest".to_string(),
            platform: Some(PLATFORM.to_string()),
            content_basename: Some("Chrono Quest".to_string()),
            ..CheatCandidateArchive::default()
        },
        &CheatCandidateOptions::default(),
    );
    assert_eq!(list.candidates.len(), 2, "the two entries must tie");
    assert!(
        list.candidates
            .iter()
            .all(|candidate| candidate.classification == CheatCandidateClassification::Ambiguous),
        "a genuine tie is never resolved silently: {:?}",
        list.candidates
    );
    assert!(list.automatic_choice().is_none());

    // The user explicitly picks one of the two tied candidates.
    let chosen = list
        .candidates
        .iter()
        .find(|candidate| candidate.catalogue_relative_path.contains("Beta"))
        .expect("the Beta candidate is present")
        .clone();
    assert!(chosen.manually_selectable);

    let loaded = load_candidate_document(
        &catalogue_root,
        &chosen.catalogue_relative_path,
        chosen.source_file_hash.as_deref(),
    )
    .expect("candidate loads");
    let mut selection = CheatSelection::from_document(&loaded.document);
    assert!(selection.set_selected(0, true));
    let entries = selection
        .resolve(&loaded.document)
        .expect("selection resolves");

    let destination = resolve_cheat_destination(&CheatDestinationRequest {
        profile_cheat_root: cheat_root.clone(),
        platform: Some(PLATFORM.to_string()),
        content_basename: Some("Chrono Quest".to_string()),
        playlist_name: None,
        catalogue_name: chosen.display_name.clone(),
    })
    .expect("destination resolves");

    let staged = stage_generated_cheat_file(&staging_root, "Chrono Quest", &entries, &[])
        .expect("staging succeeds");

    let match_strength =
        match_strength_for_candidate(&chosen).expect("an explicitly chosen tie is installable");
    assert_eq!(
        match_strength,
        archivefs_core::patch_manager::PreviewMatchStrength::Strong,
        "an explicit choice must not be re-blocked as unresolved ambiguity"
    );

    let preview = build_cheat_install_preview(&CheatInstallPreviewRequest {
        selected_archive: archive.clone(),
        platform: Some(PLATFORM.to_string()),
        verified_identity: format!("test:{}", loaded.digest),
        destination: destination.clone(),
        profile_cheat_root: cheat_root.clone(),
        staged: staged.clone(),
        match_strength,
    })
    .expect("preview builds");
    assert_eq!(preview.report.summary.blocked, 0, "must not be blocked");

    let plan = build_shared_transaction_plan(
        &preview.report,
        "test-profile",
        "ArchiveFS cached catalogue",
        &staging_root,
    )
    .expect("plan builds");
    let options = SharedApplyOptions {
        dry_run: false,
        confirmation: Some(SharedApplyConfirmation {
            plan_id: plan.plan_id.clone(),
            general_approved: true,
            replacement_approved: false,
        }),
        operation_id: "ambiguous-explicit-choice-op".to_string(),
        timestamp_unix_seconds: 1_700_000_200,
        current_context: plan.context.clone(),
        history_root,
        backup_root,
    };
    let result = execute_shared_apply(&plan, &options);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    assert!(
        fs::read_to_string(&destination.path)
            .expect("installed")
            .contains("Infinite Health"),
        "the explicitly chosen tied candidate's cheat is actually written"
    );
}

#[test]
fn the_enabled_flag_survives_into_the_installed_file() {
    let fixture = Fixture::new("enabled");
    let workflow = Workflow::new(&fixture);
    let (report, destination, _) = workflow.prepare(|selection| {
        assert!(selection.set_selected(2, true));
    });
    workflow.apply(&report, true, false);
    let installed = fs::read_to_string(&destination).expect("installed");
    assert!(
        installed.contains("cheat0_enable = true"),
        "cheat2_enable = true in the source is preserved: {installed}"
    );
}

#[test]
fn a_cancelled_install_changes_nothing_on_disk() {
    let fixture = Fixture::new("cancel");
    let workflow = Workflow::new(&fixture);
    let (report, destination, _) = workflow.prepare(select_first_and_third);

    // Not confirming is the cancel path: the plan exists, the apply runs as
    // a dry run, and nothing is written.
    let result = workflow.apply(&report, false, false);
    assert_eq!(result.journal.status, SharedApplyStatus::DryRun);
    assert!(!destination.exists(), "cancellation writes nothing");
    assert!(
        fs::read_dir(&workflow.backup_root)
            .expect("backup root")
            .next()
            .is_none(),
        "cancellation creates no backup either"
    );
}

#[test]
fn replacing_an_existing_file_backs_it_up_and_can_be_rolled_back() {
    let fixture = Fixture::new("replace");
    let workflow = Workflow::new(&fixture);
    let existing = "cheats = 1\n\ncheat0_desc = \"Old cheat\"\ncheat0_code = \"OLD\"\n";
    let destination_path = fixture.write("retroarch/cheats/NES/Chrono Quest (USA).cht", existing);

    let (report, destination, staged_contents) = workflow.prepare(select_first_and_third);
    assert_eq!(destination, destination_path);

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(&destination).expect("installed"),
        staged_contents
    );

    let entry = result.journal.entries.first().expect("one entry");
    assert!(
        entry.backup_path.is_some(),
        "replacing an existing file retains a backup"
    );

    // Rollback restores exactly what was there before.
    let journal_path = result.journal_path.clone().expect("journal written");
    let preview =
        preview_shared_rollback(&journal_path, &workflow.cheat_root, &workflow.backup_root);
    assert!(preview.available, "rollback is available: {preview:?}");
    let rollback = execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-rollback".to_string(),
            timestamp_unix_seconds: 1_700_000_100,
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
        existing,
        "the previous cheat file is back, byte for byte"
    );
}

#[test]
fn replacement_without_separate_approval_does_not_overwrite() {
    let fixture = Fixture::new("no-approval");
    let workflow = Workflow::new(&fixture);
    let existing = "cheats = 1\n\ncheat0_desc = \"Old cheat\"\ncheat0_code = \"OLD\"\n";
    fixture.write(
        &format!("retroarch/cheats/{PLATFORM}/Chrono Quest (USA).cht"),
        existing,
    );

    let (report, destination, _) = workflow.prepare(select_first_and_third);
    let result = workflow.apply(&report, true, false);

    assert_ne!(result.journal.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        existing,
        "an unapproved replacement leaves the original intact"
    );
}

#[test]
fn installing_never_modifies_the_trusted_catalogue() {
    let fixture = Fixture::new("catalogue");
    let workflow = Workflow::new(&fixture);
    let catalogue_file = workflow
        .catalogue_root
        .join(PLATFORM)
        .join("Chrono Quest (USA).cht");
    let before = fs::read(&catalogue_file).expect("read");

    let (report, _, _) = workflow.prepare(select_first_and_third);
    workflow.apply(&report, true, false);

    assert_eq!(
        fs::read(&catalogue_file).expect("read"),
        before,
        "the catalogue file is untouched by an install"
    );
}

#[test]
fn a_second_identical_install_produces_identical_bytes() {
    let first = Fixture::new("deterministic-1");
    let second = Fixture::new("deterministic-2");
    let (_, _, first_contents) = Workflow::new(&first).prepare(select_first_and_third);
    let (_, _, second_contents) = Workflow::new(&second).prepare(select_first_and_third);
    assert_eq!(first_contents, second_contents);
}

#[test]
fn a_cross_platform_archive_gets_no_installable_candidate() {
    let fixture = Fixture::new("wrong-platform");
    let workflow = Workflow::new(&fixture);
    let snapshot = load_cheat_catalogue_snapshot(
        &HostReadOnlyFilesystem,
        "test-catalogue",
        &workflow.catalogue_root,
    );
    let archive = CheatCandidateArchive {
        display_name: "Chrono Quest (USA)".to_string(),
        platform: Some("Sega - Mega Drive - Genesis".to_string()),
        content_basename: Some("Chrono Quest (USA)".to_string()),
        ..CheatCandidateArchive::default()
    };
    let list = build_cheat_candidates(&snapshot, &archive, &CheatCandidateOptions::default());
    assert_eq!(list.installable().count(), 0);
    assert!(
        list.candidates.iter().all(
            |candidate| candidate.classification == CheatCandidateClassification::CrossPlatform
        )
    );
}

#[test]
fn an_archive_with_no_catalogue_match_produces_no_candidates() {
    let fixture = Fixture::new("no-match");
    let workflow = Workflow::new(&fixture);
    let snapshot = load_cheat_catalogue_snapshot(
        &HostReadOnlyFilesystem,
        "test-catalogue",
        &workflow.catalogue_root,
    );
    let archive = CheatCandidateArchive {
        display_name: "A Totally Different Game".to_string(),
        platform: Some(PLATFORM.to_string()),
        content_basename: Some("A Totally Different Game".to_string()),
        ..CheatCandidateArchive::default()
    };
    let list = build_cheat_candidates(&snapshot, &archive, &CheatCandidateOptions::default());
    assert!(list.is_empty());
}

#[test]
fn a_malformed_catalogue_file_is_excluded_from_installable_candidates() {
    let fixture = Fixture::new("malformed");
    let workflow = Workflow::new(&fixture);
    fixture.write(
        &format!("catalogue/{PLATFORM}/Broken Game.cht"),
        "cheats = 2\nthis line is broken\ncheat0_desc = \"A\"\n",
    );
    let snapshot = load_cheat_catalogue_snapshot(
        &HostReadOnlyFilesystem,
        "test-catalogue",
        &workflow.catalogue_root,
    );
    let archive = CheatCandidateArchive {
        display_name: "Broken Game".to_string(),
        platform: Some(PLATFORM.to_string()),
        content_basename: Some("Broken Game".to_string()),
        ..CheatCandidateArchive::default()
    };
    let list = build_cheat_candidates(&snapshot, &archive, &CheatCandidateOptions::default());
    assert_eq!(
        list.installable().count(),
        0,
        "a file the indexer could not parse is never installable"
    );
}

#[test]
fn the_installed_file_is_reachable_at_the_path_retroarch_browses() {
    // RetroArch's cheat-file browser opens the profile's configured cheat
    // directory and shows one subdirectory per database name. This asserts
    // the exact shape of that path, since getting it wrong is the
    // difference between a cheat file RetroArch can find and one it cannot.
    let fixture = Fixture::new("layout");
    let workflow = Workflow::new(&fixture);
    let (report, destination, _) = workflow.prepare(select_first_and_third);
    workflow.apply(&report, true, false);

    let relative = destination
        .strip_prefix(&workflow.cheat_root)
        .expect("inside the profile cheat directory");
    assert_eq!(
        relative,
        Path::new(PLATFORM).join("Chrono Quest (USA).cht"),
        "the installed path matches the libretro layout RetroArch already uses"
    );
    assert!(destination.is_file());
}
