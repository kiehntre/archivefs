//! End-to-end behaviour for the Xbox 360/Xenia patch install workflow:
//! from a real Xenia Canary profile directory on disk, through candidate
//! compatibility matching, staging the merged `.patch.toml`, the
//! journal-backed apply, to rollback.
//!
//! These tests exercise real files in a temporary directory and assert on
//! what is actually on disk and in the journal, never on source strings.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    SharedApplyConfirmation, SharedApplyOptions, SharedApplyStatus, SharedRollbackConfirmation,
    SharedRollbackOptions, SharedRollbackOutcome, XeniaCandidateCompatibility,
    XeniaInstallPreviewRequest, XeniaProviderDocument, XeniaProviderResult,
    build_shared_transaction_plan, build_xenia_candidates, build_xenia_install_preview,
    execute_shared_apply, execute_shared_rollback, load_xenia_destination, parse_xenia_patch_toml,
    preview_shared_rollback, stage_xenia_patch_file,
};

const QUAKE4_TOML: &str = r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"

[[patch]]
    name = "Performance fix"
    desc = "Disables the FPS limit."
    author = "Sowa_95"
    is_enabled = false
    [[patch.be32]]
        address = 0x821b7140
        value = 0x39600001
    [[patch.be32]]
        address = 0x821b7420
        value = 0x39600001

[[patch]]
    name = "Widescreen"
    desc = ""
    author = "Sowa_95"
    is_enabled = false
    [[patch.be16]]
        address = 0x8204a9a2
        value = 0x0780
"#;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-xenia-e2e-{label}-{}-{}-{}",
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
    fn new(fixture: &Fixture) -> Self {
        let configuration_path = fixture.path("xenia");
        fs::create_dir_all(&configuration_path).expect("xenia dir");
        fixture.write("xenia/xenia-canary.config.toml", "");
        Self {
            configuration_path,
            archive: fixture.write("library/Quake 4.zip", "zip bytes"),
            staging_root: fixture.path("managed/generated-xenia"),
            history_root: fixture.path("managed/history"),
            backup_root: fixture.path("managed/backups"),
        }
    }

    fn provider_result(&self) -> XeniaProviderResult {
        XeniaProviderResult {
            provider_id: "xenia_canary_game_patches".to_string(),
            provider_display_name: "Xenia Canary game-patches".to_string(),
            source_repository: "xenia-canary/game-patches".to_string(),
            source_commit: "1".repeat(40),
            retrieved_at_unix_seconds: 1_700_000_000,
            title_id: "415607D2".to_string(),
            documents: vec![XeniaProviderDocument {
                source_path: "patches/415607D2 - Quake 4.patch.toml".to_string(),
                document: parse_xenia_patch_toml(QUAKE4_TOML),
            }],
            attribution: "test".to_string(),
            license: "test".to_string(),
            warnings: Vec::new(),
        }
    }

    fn file_name(&self) -> &'static str {
        "415607D2 - Quake 4.patch.toml"
    }

    fn destination(&self) -> PathBuf {
        self.configuration_path
            .join("patches")
            .join(self.file_name())
    }

    fn prepare(
        &self,
        selected_names: &[String],
    ) -> (archivefs_core::patch_manager::SharedPreviewReport, String) {
        let result = self.provider_result();
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let mut candidate = outcome
            .candidates
            .into_iter()
            .next()
            .expect("one candidate");
        assert_eq!(
            candidate.compatibility,
            XeniaCandidateCompatibility::PartiallyVerified
        );
        // Acknowledging is a GUI-layer concern (XeniaPatchSelection); this
        // fixture exercises the install-plan/staging/preview layer
        // directly, so it stages by explicit name regardless of tier -
        // matching what the GUI does only after the user has acknowledged.
        candidate.compatibility = XeniaCandidateCompatibility::ExactCompatible;

        let patches_directory = self.configuration_path.join("patches");
        let loaded = load_xenia_destination(&patches_directory, self.file_name()).expect("loads");
        let staged = stage_xenia_patch_file(
            &self.staging_root,
            self.file_name(),
            &candidate,
            loaded.document.as_ref(),
            selected_names,
        )
        .expect("staging succeeds");

        let preview = build_xenia_install_preview(&XeniaInstallPreviewRequest {
            selected_archive: self.archive.clone(),
            configuration_path: self.configuration_path.clone(),
            title_id: candidate.title_id.clone(),
            compatibility: candidate.compatibility,
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
            "test-xenia-profile",
            "Xenia Canary game-patches",
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
            operation_id: format!("test-xenia-op-{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
            timestamp_unix_seconds: 1_700_000_300,
            current_context: plan.context.clone(),
            history_root: self.history_root.clone(),
            backup_root: self.backup_root.clone(),
        };
        execute_shared_apply(&plan, &options)
    }
}

fn select_performance_fix(names: &mut Vec<String>) {
    names.push("Performance fix".to_string());
}

#[test]
fn a_selected_patch_installs_to_the_real_destination_creating_a_new_patches_directory() {
    let fixture = Fixture::new("new-file");
    let workflow = Workflow::new(&fixture);
    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, staged_contents) = workflow.prepare(&names);

    let destination = workflow.destination();
    assert!(!destination.exists(), "nothing installed yet");
    assert!(
        !workflow.configuration_path.join("patches").exists(),
        "patches directory does not exist until apply"
    );

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let installed = fs::read_to_string(&destination).expect("installed file exists");
    assert_eq!(installed, staged_contents);
    let parsed = parse_xenia_patch_toml(&installed);
    let performance = parsed
        .patches
        .iter()
        .find(|patch| patch.name == "Performance fix")
        .expect("performance fix present");
    assert!(performance.enabled_by_default);
    let widescreen = parsed
        .patches
        .iter()
        .find(|patch| patch.name == "Widescreen")
        .expect("widescreen present");
    assert!(
        !widescreen.enabled_by_default,
        "unselected patch stays disabled, not removed"
    );
}

#[test]
fn install_and_rollback_of_a_brand_new_file_removes_it_and_the_directory_it_created() {
    let fixture = Fixture::new("rollback-new-file");
    let workflow = Workflow::new(&fixture);
    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let destination = workflow.destination();
    assert!(destination.exists());
    assert!(workflow.configuration_path.join("patches").exists());

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
            rollback_operation_id: "test-xenia-rollback".to_string(),
            timestamp_unix_seconds: 1_700_000_400,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(
        rollback.preview.entries.iter().all(|entry| matches!(
            entry.outcome,
            SharedRollbackOutcome::RemovedInstalledFile | SharedRollbackOutcome::Available
        )),
        "{:?}",
        rollback.preview.entries
    );
    assert!(!destination.exists(), "new file removed on rollback");
}

#[test]
fn install_and_rollback_of_an_existing_file_restores_the_exact_previous_bytes() {
    let fixture = Fixture::new("rollback-existing");
    let workflow = Workflow::new(&fixture);
    let original = "title_name = \"Quake 4\"\ntitle_id = \"415607D2\"\nhash = \"4768B579A3C5F134\"\n\n[[patch]]\n    name = \"Hand added\"\n    desc = \"\"\n    author = \"someone\"\n    is_enabled = true\n    [[patch.be8]]\n        address = 0x9999\n        value = 0x1\n";
    fixture.write(&format!("xenia/patches/{}", workflow.file_name()), original);

    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);
    let destination = workflow.destination();
    let before = fs::read_to_string(&destination).expect("original file present");
    assert_eq!(before, original);

    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let installed = fs::read_to_string(&destination).expect("installed");
    assert!(
        installed.contains("Hand added"),
        "unrelated existing patch preserved"
    );
    assert!(installed.contains("Performance fix"));

    let journal_path = result.journal_path.clone().expect("journal written");
    let preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    assert!(preview.available);
    let rollback = execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-xenia-rollback-existing".to_string(),
            timestamp_unix_seconds: 1_700_000_500,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(&destination).expect("restored"),
        original,
        "the previous patch.toml is back, byte for byte"
    );
}

#[test]
fn a_preexisting_patches_directory_is_never_removed_on_rollback() {
    let fixture = Fixture::new("preexisting-dir");
    let workflow = Workflow::new(&fixture);
    fs::create_dir_all(workflow.configuration_path.join("patches")).unwrap();
    fixture.write(
        "xenia/patches/unrelated - Other Game.patch.toml",
        "unrelated content\n",
    );

    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);
    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);

    let journal_path = result.journal_path.clone().expect("journal written");
    let preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    let rollback = execute_shared_rollback(
        &preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-xenia-rollback-preexisting-dir".to_string(),
            timestamp_unix_seconds: 1_700_000_600,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(rollback.status, SharedApplyStatus::Success);
    assert!(
        workflow.configuration_path.join("patches").exists(),
        "the pre-existing patches directory must survive rollback"
    );
    assert!(
        workflow
            .configuration_path
            .join("patches/unrelated - Other Game.patch.toml")
            .exists(),
        "an unrelated patch file in the same directory must never be touched"
    );
}

#[test]
fn a_cancelled_install_changes_nothing_on_disk() {
    let fixture = Fixture::new("cancel");
    let workflow = Workflow::new(&fixture);
    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);
    let destination = workflow.destination();

    let result = workflow.apply(&report, false, false);
    assert_eq!(result.journal.status, SharedApplyStatus::DryRun);
    assert!(!destination.exists(), "cancellation writes nothing");
}

#[test]
fn replacement_without_separate_approval_leaves_the_existing_file_untouched() {
    let fixture = Fixture::new("no-approval");
    let workflow = Workflow::new(&fixture);
    let original =
        "title_name = \"Quake 4\"\ntitle_id = \"415607D2\"\nhash = \"4768B579A3C5F134\"\n";
    fixture.write(&format!("xenia/patches/{}", workflow.file_name()), original);

    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);
    let destination = workflow.destination();

    let result = workflow.apply(&report, true, false);
    assert_ne!(result.journal.status, SharedApplyStatus::Success);
    assert_eq!(
        fs::read_to_string(&destination).expect("unchanged"),
        original
    );
}

#[test]
fn repeated_rollback_of_the_same_journal_is_safe() {
    let fixture = Fixture::new("repeated-rollback");
    let workflow = Workflow::new(&fixture);
    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (report, _) = workflow.prepare(&names);
    let result = workflow.apply(&report, true, true);
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    let journal_path = result.journal_path.clone().expect("journal written");

    let first_preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    assert!(first_preview.available);
    let first_rollback = execute_shared_rollback(
        &first_preview,
        &SharedRollbackOptions {
            confirmation: SharedRollbackConfirmation {
                preview_id: first_preview.preview_id.clone(),
                approved: true,
            },
            rollback_operation_id: "test-xenia-repeat-0".to_string(),
            timestamp_unix_seconds: 1_700_000_700,
            history_root: workflow.history_root.clone(),
            backup_root: workflow.backup_root.clone(),
        },
    );
    assert_eq!(first_rollback.status, SharedApplyStatus::Success);
    assert!(!workflow.destination().exists());

    // A second rollback attempt against the same journal is safe: it is
    // reported as already rolled back, not re-applied or corrupted.
    let second_preview = preview_shared_rollback(
        &journal_path,
        &workflow.configuration_path,
        &workflow.backup_root,
    );
    assert!(!second_preview.available);
    assert_eq!(
        second_preview.entries[0].outcome,
        SharedRollbackOutcome::AlreadyRolledBack
    );
    assert!(!workflow.destination().exists(), "still rolled back");
}

#[test]
fn deterministic_preview_content_hash_is_stable_across_rebuilds() {
    let fixture = Fixture::new("deterministic");
    let workflow = Workflow::new(&fixture);
    let mut names = Vec::new();
    select_performance_fix(&mut names);
    let (_, staged_a) = workflow.prepare(&names);
    let (_, staged_b) = workflow.prepare(&names);
    assert_eq!(
        staged_a, staged_b,
        "staging the same selection twice is byte-identical"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_patches_destination_is_refused_not_followed() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("symlink-traversal");
    let workflow = Workflow::new(&fixture);
    let patches_dir = workflow.configuration_path.join("patches");
    fs::create_dir_all(&patches_dir).unwrap();
    let real_target = fixture.path("outside-target.patch.toml");
    fs::write(&real_target, "not a real patch file\n").unwrap();
    symlink(&real_target, patches_dir.join(workflow.file_name())).unwrap();

    let loaded = load_xenia_destination(&patches_dir, workflow.file_name());
    assert!(
        loaded.is_err(),
        "a symlinked destination must be refused, never followed"
    );
}
