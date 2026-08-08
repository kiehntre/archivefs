//! Tests for the DAT Sources page's state and view model.
//!
//! These are data tests: the view model is a pure function of state, so what
//! the page *says* is checkable without a frame buffer. Drawing is exercised
//! only through the view it consumes.
//!
//! # What these tests never touch
//!
//! Every path is inside a per-test temp directory removed on drop. `DatSourcesPageState::load`
//! takes its registry path as an argument precisely so no test has to read, or
//! disturb, the real `HOME`. No real ROM or DAT collection is opened, and there
//! is no network surface anywhere in this page or the core it calls.

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use archivefs_core::dat::audit::{AuditReport, AuditSummary};
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::model::{DatEcosystem, DatFormat};
use archivefs_core::dat::parser::DiagnosticSeverity;
use archivefs_core::dat::sources::{
    DatDiagnostic, DatFileOutcome, DatFileReport, DatHealthState, DatSourceKind, DatSourceRegistry,
    DatValidationReport, audit_run::DatAuditOutcome, load_dat_sources_config_from,
};
use archivefs_core::safe_read::TrustedRoots;

use super::*;

const LOGIQX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Test No-Intro Collection</name>
        <version>2026-01-01</version>
    </header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="4" crc="0c7e7fd8" md5="098f6bcd4621d373cade4e832627b4f6" sha1="a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"/>
    </game>
</datafile>"#;

/// Bytes whose MD5/SHA-1 are the ones in [`LOGIQX`].
const SUPER_BIN: &[u8] = b"test";

/// How long a test waits for a worker thread before calling it a failure.
const JOB_TIMEOUT: Duration = Duration::from_secs(30);

struct Fixture {
    root: PathBuf,
    config_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gui-dat-sources-page-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        let config_path = root.join("config").join("dat_sources.toml");
        Self { root, config_path }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A page whose registry lives in the fixture, with no library folders and
    /// no trusted roots - the strictest, and the one a fresh install has.
    fn page(&self) -> DatSourcesPageState {
        // The rename-transaction journal directory is a temp path inside the
        // fixture, so no test ever reads or writes the real home directory.
        let journal = self.root.join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        DatSourcesPageState::load_with_transaction_dir(
            self.config_path.clone(),
            Vec::new(),
            TrustedRoots::none(),
            journal,
        )
    }

    fn page_with_library(&self, folders: Vec<PathBuf>) -> DatSourcesPageState {
        let journal = self.root.join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        DatSourcesPageState::load_with_transaction_dir(
            self.config_path.clone(),
            folders,
            TrustedRoots::none(),
            journal,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Polls until the page's background job finishes, or fails the test.
fn run_to_completion(page: &mut DatSourcesPageState) {
    let deadline = Instant::now() + JOB_TIMEOUT;
    while page.is_busy() {
        page.poll();
        if Instant::now() > deadline {
            panic!("a background job did not finish within {JOB_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // One final drain: the job may have finished between the last poll and the
    // loop's exit test.
    page.poll();
}

/// A recursive listing of `(relative path, contents)`, for proving nothing
/// changed on disk.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(directory) = queue.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_install_renders_an_empty_page_with_no_error() {
    // The clean-install case: no registry file exists, and that is not a
    // failure. The page must be usable and must not report a problem.
    let fixture = Fixture::new();
    assert!(!fixture.config_path.exists());

    let page = fixture.page();
    let view = page.view();

    assert!(view.is_empty(), "no sources are registered yet");
    assert!(view.rows.is_empty());
    assert!(view.load_error.is_none(), "an absent file is not an error");
    assert!(view.load_problems.is_empty());
    assert!(view.unresolved.is_empty());
    assert!(!view.dirty, "an untouched page has nothing to save");
    assert!(view.pending_consequences.is_empty());
    assert!(view.running.is_none());
    assert!(view.audit.is_none());
    assert_eq!(view.save_state, DatSaveState::Idle);
    assert_eq!(view.config_path, fixture.config_path);
    // The policy section is present and at safe defaults on a clean install.
    assert_eq!(view.policy.scope, None);
    assert_eq!(view.policy.scope_label, "All platforms");
    assert!(view.policy.region_preferences.is_empty());
    assert!(view.policy.language_preferences.is_empty());
    assert_eq!(view.policy.revision_policy, RevisionPolicy::default());
    assert_eq!(view.policy.clone_policy, ClonePolicy::default());
    assert!(view.policy.effective.source_ordering.is_empty());
    assert!(view.policy.problems.is_empty());
    assert!(view.policy.editable);

    // And opening the page must not have created the file.
    assert!(
        !fixture.config_path.exists(),
        "viewing the page must write nothing"
    );
}

#[test]
fn an_unreadable_registry_is_reported_and_blocks_saving() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.config_path.parent().unwrap()).unwrap();
    std::fs::write(&fixture.config_path, "this is not valid toml {{[").unwrap();
    let before = std::fs::read_to_string(&fixture.config_path).unwrap();

    let mut page = fixture.page();
    assert!(page.view().load_error.is_some());

    // Saving must refuse rather than overwrite a file the user may still want
    // to repair by hand.
    page.apply(DatSourcesPageAction::Save);
    assert!(matches!(page.view().save_state, DatSaveState::Failed(_)));
    assert_eq!(
        std::fs::read_to_string(&fixture.config_path).unwrap(),
        before,
        "a refused save must not have touched the file"
    );
}

// ---------------------------------------------------------------------------
// Adding
// ---------------------------------------------------------------------------

#[test]
fn adding_a_dat_file_shows_it_as_an_unsaved_change() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat.clone() });

    let view = page.view();
    assert_eq!(view.rows.len(), 1);
    let row = &view.rows[0];
    assert_eq!(row.id, "no-intro");
    assert_eq!(row.display_name, "no-intro.dat");
    assert_eq!(row.kind_label, "DAT file");
    assert!(row.enabled);
    assert!(row.changed);
    assert_eq!(
        row.health_state,
        DatHealthState::NotChecked,
        "adding a source must not claim a health nobody checked"
    );
    assert!(
        row.formats.is_empty(),
        "format is only ever reported from a real check, never guessed from the name"
    );

    assert!(view.dirty);
    assert!(
        view.pending_consequences
            .iter()
            .any(|line| line.contains("no-intro.dat")),
        "{:?}",
        view.pending_consequences
    );
    assert!(
        !fixture.config_path.exists(),
        "nothing is written before Save"
    );
}

#[test]
fn adding_a_dat_folder_registers_the_folder_itself() {
    let fixture = Fixture::new();
    let folder = fixture.dir("dats");
    std::fs::write(folder.join("a.dat"), LOGIQX).unwrap();

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFolder {
        path: folder.clone(),
    });

    let view = page.view();
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].kind_label, "DAT folder");
    assert_eq!(view.rows[0].path, folder.to_string_lossy());
    assert!(view.action_error.is_none());
}

#[test]
fn adding_the_same_path_twice_is_refused_with_a_reason() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat.clone() });
    page.apply(DatSourcesPageAction::AddFile { path: dat });

    let view = page.view();
    assert_eq!(view.rows.len(), 1, "the second add must not have landed");
    let error = view.action_error.expect("the refusal must be shown");
    assert!(error.contains("already registers"), "{error}");
}

#[test]
fn adding_a_folder_as_a_file_is_refused() {
    let fixture = Fixture::new();
    let folder = fixture.dir("dats");

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: folder });

    let view = page.view();
    assert!(view.rows.is_empty());
    let error = view.action_error.expect("the mismatch must be reported");
    assert!(error.contains("folder"), "{error}");
}

#[cfg(unix)]
#[test]
fn adding_a_symlinked_dat_is_refused() {
    let fixture = Fixture::new();
    let real = fixture.write("real.dat", LOGIQX);
    let link = fixture.root.join("link.dat");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: link });

    let view = page.view();
    assert!(view.rows.is_empty());
    let error = view.action_error.expect("the refusal must be shown");
    assert!(error.contains("symlink"), "{error}");
}

// ---------------------------------------------------------------------------
// Save and Discard
// ---------------------------------------------------------------------------

#[test]
fn save_writes_the_registry_and_clears_the_unsaved_state() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Save);

    let view = page.view();
    assert_eq!(view.save_state, DatSaveState::Saved);
    assert!(!view.dirty, "after a save there is nothing left to save");
    assert!(!view.rows[0].changed, "and nothing is marked changed");
    assert!(fixture.config_path.exists());

    // Reloading a new page from the same path sees it.
    let reloaded = fixture.page();
    let view = reloaded.view();
    assert_eq!(view.rows.len(), 1);
    assert_eq!(view.rows[0].id, "no-intro");
    assert!(!view.dirty);
}

#[test]
fn discard_restores_exactly_what_is_on_disk() {
    let fixture = Fixture::new();
    let first = fixture.write("first.dat", LOGIQX);
    let second = fixture.write("second.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: first });
    page.apply(DatSourcesPageAction::Save);
    let saved_text = std::fs::read_to_string(&fixture.config_path).unwrap();

    // Now make several unsaved edits of different kinds.
    page.apply(DatSourcesPageAction::AddFile { path: second });
    page.apply(DatSourcesPageAction::SetEnabled {
        id: "first".to_string(),
        enabled: false,
    });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: "first".to_string(),
        platform: Some("NES".to_string()),
    });
    assert!(page.view().dirty);

    page.apply(DatSourcesPageAction::Revert);
    let view = page.view();
    assert!(!view.dirty, "discarding must leave nothing pending");
    assert_eq!(view.rows.len(), 1);
    assert!(view.rows[0].enabled);
    assert!(view.rows[0].platform_display.is_none());
    assert_eq!(
        std::fs::read_to_string(&fixture.config_path).unwrap(),
        saved_text,
        "discarding must not have written anything"
    );
}

#[test]
fn a_disabled_source_stays_disabled_across_a_save_and_reload() {
    let fixture = Fixture::new();
    let dat = fixture.write("off.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetEnabled {
        id: "off".to_string(),
        enabled: false,
    });
    page.apply(DatSourcesPageAction::Save);

    let reloaded = fixture.page();
    let view = reloaded.view();
    assert_eq!(view.rows.len(), 1, "a disabled source is still listed");
    assert!(!view.rows[0].enabled);
}

#[test]
fn disabling_says_the_source_is_kept() {
    let fixture = Fixture::new();
    let dat = fixture.write("off.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Save);
    page.apply(DatSourcesPageAction::SetEnabled {
        id: "off".to_string(),
        enabled: false,
    });

    let consequences = page.view().pending_consequences;
    assert!(
        consequences
            .iter()
            .any(|line| line.contains("stays registered")),
        "{consequences:?}"
    );
}

// ---------------------------------------------------------------------------
// Platform assignment
// ---------------------------------------------------------------------------

#[test]
fn a_platform_can_be_assigned_and_cleared() {
    let fixture = Fixture::new();
    let dat = fixture.write("nes.dat", LOGIQX);
    let canonical = archivefs_core::platform::canonical_ids()[0];

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: "nes".to_string(),
        platform: Some(canonical.to_string()),
    });

    let view = page.view();
    assert_eq!(
        view.rows[0].platform_display.as_deref(),
        Some(archivefs_core::platform::display_name_for(canonical))
    );
    assert!(!view.rows[0].platform_unresolved);

    page.apply(DatSourcesPageAction::SetPlatform {
        id: "nes".to_string(),
        platform: None,
    });
    assert!(page.view().rows[0].platform_display.is_none());
}

#[test]
fn the_platform_picker_offers_only_canonical_platforms() {
    // Every candidate comes from the same registry `canonical_platform_for_alias`
    // resolves against, so an assignment can only ever name a platform the
    // resolver will match.
    let choices = platform_choices("");
    assert!(!choices.is_empty());
    assert!(choices.len() <= MAX_PLATFORM_CHOICES);
    for (id, _) in &choices {
        assert!(
            archivefs_core::canonical_platform_for_alias(id).is_some(),
            "the picker offered '{id}', which the resolver does not know"
        );
    }
    assert!(
        platform_choice_count("") >= choices.len(),
        "the count must not understate the truncated list"
    );
    assert_eq!(platform_choice_count("a-platform-nobody-has"), 0);
}

#[test]
fn an_unresolved_platform_is_shown_kept_and_round_trips() {
    let fixture = Fixture::new();
    let dat = fixture.write("future.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: "future".to_string(),
        platform: Some("APlatformFromALaterBuild".to_string()),
    });
    page.apply(DatSourcesPageAction::Save);

    let reloaded = fixture.page();
    let view = reloaded.view();
    assert_eq!(
        view.rows[0].platform_display.as_deref(),
        Some("APlatformFromALaterBuild"),
        "an unresolved assignment renders as itself rather than vanishing"
    );
    assert!(view.rows[0].platform_unresolved);
    assert_eq!(view.unresolved.len(), 1);
    assert!(
        view.unresolved[0]
            .explanation
            .contains("APlatformFromALaterBuild")
    );
}

// ---------------------------------------------------------------------------
// Forward compatibility through the page
// ---------------------------------------------------------------------------

#[test]
fn saving_from_the_page_keeps_settings_written_by_a_newer_build() {
    let fixture = Fixture::new();
    let dat = fixture.write("shared.dat", LOGIQX);
    std::fs::create_dir_all(fixture.config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &fixture.config_path,
        format!(
            r#"
a_future_top_level_key = "kept"

[[sources]]
id = "shared"
display_name = "Shared"
path = "{}"
kind = "file"
enabled = true
a_future_entry_key = 7
"#,
            dat.display()
        ),
    )
    .unwrap();

    let mut page = fixture.page();
    assert!(page.view().load_error.is_none());
    // Edit something unrelated and save from the page, exactly as a user would.
    page.apply(DatSourcesPageAction::SetEnabled {
        id: "shared".to_string(),
        enabled: false,
    });
    page.apply(DatSourcesPageAction::Save);
    assert_eq!(page.view().save_state, DatSaveState::Saved);

    let text = std::fs::read_to_string(&fixture.config_path).unwrap();
    assert!(text.contains("a_future_top_level_key"), "{text}");
    assert!(text.contains("a_future_entry_key"), "{text}");

    // And the page says it kept them rather than leaving the user guessing.
    let reloaded = fixture.page();
    let view = reloaded.view();
    assert!(
        view.unresolved
            .iter()
            .any(|row| row.explanation.contains("a_future_entry_key")),
        "{:?}",
        view.unresolved
    );
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[test]
fn removing_a_source_never_deletes_the_dat_file() {
    let fixture = Fixture::new();
    let folder = fixture.dir("dats");
    std::fs::write(folder.join("keep.dat"), LOGIQX).unwrap();
    std::fs::write(folder.join("rom.bin"), b"pretend ROM").unwrap();
    let before = snapshot(&folder);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFolder {
        path: folder.clone(),
    });
    page.apply(DatSourcesPageAction::Save);
    page.apply(DatSourcesPageAction::Remove {
        id: "dats".to_string(),
    });

    let view = page.view();
    assert!(view.rows.is_empty(), "the entry is gone from the list");
    assert!(view.dirty, "and removing it is an unsaved change");
    assert!(
        view.pending_consequences
            .iter()
            .any(|line| line.contains("is not deleted")),
        "the page must say the file survives: {:?}",
        view.pending_consequences
    );

    page.apply(DatSourcesPageAction::Save);

    assert!(folder.exists(), "the folder must survive");
    assert_eq!(
        snapshot(&folder),
        before,
        "removing a registry entry changed something on disk"
    );

    // And the saved registry no longer lists it.
    let config = load_dat_sources_config_from(&fixture.config_path).unwrap();
    let (registry, _) = DatSourceRegistry::from_config(&config);
    assert!(registry.is_empty());
}

#[test]
fn removing_a_saved_source_then_discarding_restores_it() {
    let fixture = Fixture::new();
    let dat = fixture.write("keep.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Save);
    assert!(!page.view().dirty);

    page.apply(DatSourcesPageAction::Remove {
        id: "keep".to_string(),
    });
    assert!(page.view().rows.is_empty(), "removed from the draft");
    assert!(page.view().dirty);

    page.apply(DatSourcesPageAction::Revert);
    let view = page.view();
    assert!(
        !view.dirty,
        "discarding a pending removal must restore exactly what was saved"
    );
    assert_eq!(view.rows.len(), 1, "the saved source must come back");
    assert_eq!(view.rows[0].id, "keep");
    assert!(!view.rows[0].changed);
}

// ---------------------------------------------------------------------------
// Validation through the page
// ---------------------------------------------------------------------------

#[test]
fn validating_reports_the_format_and_counts_it_observed() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    assert!(page.is_busy(), "validation runs off the calling thread");
    run_to_completion(&mut page);

    let view = page.view();
    assert!(view.running.is_none());
    let row = &view.rows[0];
    assert_eq!(row.health_state, DatHealthState::Valid);
    assert_eq!(row.formats, vec!["Logiqx XML".to_string()]);
    assert_eq!(row.entry_count, Some(1));
    assert_eq!(row.rom_count, Some(1));
    assert!(row.last_validated.is_some());
    assert!(!row.health_stale);

    // The Inspect panel has the per-file breakdown.
    let detail = row.detail.as_ref().expect("a validated source has detail");
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].status, "OK");
    assert!(
        detail.files[0].detail.contains("Test No-Intro Collection"),
        "{:?}",
        detail.files[0]
    );
}

#[test]
fn validating_a_broken_dat_reports_it_without_touching_the_file() {
    let fixture = Fixture::new();
    let dat = fixture.write("broken.dat", "<?xml version=\"1.0\"?><datafile><game");
    let before = std::fs::read(&dat).unwrap();

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat.clone() });
    page.apply(DatSourcesPageAction::Validate {
        id: "broken".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    assert_eq!(view.rows[0].health_state, DatHealthState::Invalid);
    assert!(view.rows[0].health_detail.is_some());
    assert_eq!(
        std::fs::read(&dat).unwrap(),
        before,
        "validating must not modify the DAT"
    );
}

#[test]
fn a_validation_result_becomes_an_unsaved_change_the_user_can_discard() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Save);
    assert!(!page.view().dirty);

    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    run_to_completion(&mut page);

    assert!(
        page.view().dirty,
        "the observed health is a change like any other, not written behind the user's back"
    );
    page.apply(DatSourcesPageAction::Revert);
    assert!(!page.view().dirty);
    assert_eq!(page.view().rows[0].health_state, DatHealthState::NotChecked);
}

#[test]
fn discarding_while_a_validation_is_in_flight_stops_it_from_landing() {
    // Regression: `Revert` used to leave the background job running. `poll()`
    // never checked whether the job's target survived the discard, so a
    // validation that finished afterwards still wrote its result into
    // `self.validations` under the source's id - and if that id had never
    // been saved, the id no longer named anything in the registry at all. A
    // later add that reused the same auto-suggested id (the ordinary case for
    // re-adding the same file) would then show stale Inspect detail for a
    // source nobody had actually checked yet.
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat.clone() });
    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    assert!(page.is_busy());

    // The source was never saved, so discarding removes it from the draft
    // entirely - the sharpest version of the race, since the id the worker is
    // about to report against will not exist anywhere in the registry.
    page.apply(DatSourcesPageAction::Revert);
    assert!(!page.is_busy(), "discarding must stop the job immediately");
    assert!(page.view().rows.is_empty());

    run_to_completion(&mut page);
    assert!(page.view().running.is_none());
    assert!(
        page.view().rows.is_empty(),
        "the discarded source must not have reappeared"
    );

    // Re-adding the same file gets the same suggested id. If the abandoned
    // job's result had still landed in `self.validations`, this row would
    // show Inspect detail for a source that was never actually validated.
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    let view = page.view();
    assert_eq!(view.rows[0].id, "no-intro");
    assert_eq!(view.rows[0].health_state, DatHealthState::NotChecked);
    assert!(
        view.rows[0].detail.is_none(),
        "a freshly re-added source must not carry Inspect detail from an \
         abandoned job: {:?}",
        view.rows[0].detail
    );
}

#[test]
fn discarding_forgets_completed_session_validation_records() {
    // Regression: Revert used to leave `validations` (and now the diagnostic
    // groups cache) in place, so a source that was validated, then discarded
    // (never saved), then re-added - reusing its auto-suggested id - showed
    // stale Inspect detail and stale diagnostic groups next to a "Not checked"
    // badge.
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", &logiqx_with_doctype_and_entries(1));
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat.clone() });
    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    run_to_completion(&mut page);
    assert!(
        page.validation("no-intro").is_some(),
        "a completed validation is recorded"
    );
    assert_eq!(
        page.view().rows[0].diagnostic_types(DiagnosticSeverity::Note),
        1,
        "the DOCTYPE note is grouped so the discard below is observable"
    );

    // Discard the never-saved source, then re-add the same file.
    page.apply(DatSourcesPageAction::Revert);
    assert!(page.view().rows.is_empty());
    page.apply(DatSourcesPageAction::AddFile { path: dat });

    let view = page.view();
    assert_eq!(view.rows[0].id, "no-intro");
    assert_eq!(view.rows[0].health_state, DatHealthState::NotChecked);
    assert!(
        view.rows[0].detail.is_none(),
        "a re-added source must not show stale Inspect detail from a discarded run"
    );
    assert!(
        view.rows[0].groups.is_empty(),
        "a re-added source must not show stale diagnostic groups from a discarded run"
    );
}

#[test]
fn a_second_job_is_refused_while_one_is_running() {
    let fixture = Fixture::new();
    let dat = fixture.write("no-intro.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    let first = page.view().running.map(|running| running.what);
    // A second request while the first is in flight must not replace it.
    page.apply(DatSourcesPageAction::Validate {
        id: "no-intro".to_string(),
    });
    assert_eq!(page.view().running.map(|running| running.what), first);
    run_to_completion(&mut page);
}

// ---------------------------------------------------------------------------
// Audit through the page
// ---------------------------------------------------------------------------

/// A page with one registered DAT and a ROM folder holding one known file and
/// one unknown one.
fn audit_fixture() -> (Fixture, DatSourcesPageState, PathBuf) {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let roms = fixture.dir("roms");
    std::fs::write(roms.join("super.bin"), SUPER_BIN).unwrap();
    std::fs::write(roms.join("mystery.bin"), b"not in any catalogue").unwrap();

    let mut page = fixture.page_with_library(vec![roms.clone()]);
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    (fixture, page, roms)
}

#[test]
fn the_audit_summary_shows_elapsed_time_and_a_shortened_scan_folder() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let audit = view.audit.as_ref().expect("an audit result");
    // The full folder is still on the result for provenance...
    assert_eq!(audit.scan_root, roms.to_string_lossy());
    // ...but the display uses a shortened form that never exposes the full path.
    assert_eq!(audit.scan_root_short, shorten_path(&roms.to_string_lossy()));
    assert!(
        !audit.scan_root_short.contains("/tmp"),
        "the full private path must not be shown: {}",
        audit.scan_root_short
    );
    // A completed audit knows how long it took.
    assert!(audit.elapsed_seconds.is_some());

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Completed in"));
    assert!(
        !rendered_text_contains(&output, &roms.to_string_lossy()),
        "the full scan folder must not be rendered"
    );
}

#[test]
fn the_audit_summary_survives_navigation_and_is_replaced_by_a_new_generation() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });
    run_to_completion(&mut page);

    // Navigating away and back keeps the same summary for this generation.
    let first = page
        .view()
        .audit
        .as_ref()
        .expect("summary")
        .headline
        .clone();
    for _ in 0..3 {
        assert_eq!(
            page.view().audit.as_ref().map(|a| a.headline.clone()),
            Some(first.clone()),
            "the same generation must keep its summary across views"
        );
    }

    // A cancelled new generation never shows a success summary.
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    page.apply(DatSourcesPageAction::CancelJob);
    run_to_completion(&mut page);
    assert!(
        page.view().audit.is_none(),
        "a cancelled audit never shows a success summary"
    );
}

#[test]
fn the_audit_picker_offers_the_configured_library_folders() {
    let (_fixture, page, roms) = audit_fixture();
    assert_eq!(page.view().library_folders, vec![roms]);
}

#[test]
fn an_audit_reports_only_the_categories_the_core_produces() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    assert!(view.audit_error.is_none(), "{:?}", view.audit_error);
    let audit = view.audit.expect("the audit produced a result");

    // Exactly the eight categories `AuditSummary` counts, in the core's own
    // vocabulary. Nothing invented, nothing merged.
    let labels: Vec<&str> = audit.categories.iter().map(|c| c.label).collect();
    assert_eq!(
        labels,
        vec![
            "Exact",
            "Exact (multiple)",
            "Probable",
            "Probable (multiple)",
            "Filename only",
            "Ambiguous",
            "Not in catalogue",
            "No usable evidence",
        ]
    );
    for category in &audit.categories {
        assert!(
            !category.meaning.is_empty(),
            "'{}' must explain what it means",
            category.label
        );
    }

    let count_of = |label: &str| {
        audit
            .categories
            .iter()
            .find(|c| c.label == label)
            .map(|c| c.count)
            .unwrap()
    };
    assert_eq!(count_of("Exact"), 1);
    assert_eq!(count_of("Not in catalogue"), 1);
    assert_eq!(audit.files_scanned, 2);
    assert!(!audit.truncated);

    // Provenance is on the result, not something the reader has to remember.
    assert_eq!(audit.source_id, "collection");
    assert_eq!(
        audit.catalogue_names,
        vec!["Test No-Intro Collection".to_string()]
    );
    assert_eq!(audit.scan_root, roms.to_string_lossy());
    assert_eq!(audit.entries.len(), 2);
    assert_eq!(audit.entries_truncated, 0);
}

#[test]
fn an_audit_changes_nothing_on_disk() {
    // The guarantee the page promises in its banner, checked rather than
    // asserted in prose.
    let (fixture, mut page, roms) = audit_fixture();
    let before_roms = snapshot(&roms);
    let before_all = snapshot(&fixture.root);

    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });
    run_to_completion(&mut page);
    assert!(page.view().audit.is_some());

    assert_eq!(snapshot(&roms), before_roms, "the ROM folder changed");
    assert_eq!(
        snapshot(&fixture.root),
        before_all,
        "an audit created, removed or altered something"
    );
}

#[test]
fn an_audit_can_be_cancelled_from_the_page() {
    // Deterministic by construction: `CancelJob` flips the page's own
    // `cancel_requested` flag, and `poll()` drops any terminal result that
    // arrives after it - so whatever the worker does (observe the flag and
    // send `Cancelled`, or finish first and send `Audited`), a cancelled audit
    // can never land in `view.audit`.
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    let running = page.view().running.expect("a job is running");
    assert_eq!(running.what, "Auditing");
    assert!(
        running.cancellable,
        "an audit must offer a Cancel that actually does something"
    );

    // Cancel flips the visible state immediately, while the worker is still
    // running.
    page.apply(DatSourcesPageAction::CancelJob);
    let running = page.view().running.expect("still busy while stopping");
    assert!(
        running.cancellation_requested,
        "the card must read 'Stopping…' the moment Cancel is pressed"
    );
    assert!(
        page.is_busy(),
        "the operation remains busy until the worker confirms termination"
    );

    run_to_completion(&mut page);

    let view = page.view();
    assert!(view.running.is_none(), "the job stopped");
    // A cancelled run reports nothing rather than a partial result dressed up
    // as a complete one.
    assert!(view.audit.is_none());
    assert!(view.audit_error.is_none(), "cancelling is not a failure");
}

#[test]
fn an_audit_of_an_empty_folder_reports_why_rather_than_claiming_all_clear() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let empty = fixture.dir("empty");

    let mut page = fixture.page_with_library(vec![empty.clone()]);
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: empty,
    });
    run_to_completion(&mut page);

    let view = page.view();
    assert!(view.audit.is_none());
    let error = view.audit_error.expect("the reason must be shown");
    assert!(error.contains("no files"), "{error}");
}

#[test]
fn removing_a_source_drops_a_result_attributed_to_it() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    run_to_completion(&mut page);
    assert!(page.view().audit.is_some());

    page.apply(DatSourcesPageAction::Remove {
        id: "collection".to_string(),
    });
    assert!(
        page.view().audit.is_none(),
        "a result attributed to a source that is gone has nothing to point at"
    );
}

#[test]
fn discarding_while_an_audit_is_in_flight_stops_it_from_landing() {
    // Regression: the same race as
    // `discarding_while_a_validation_is_in_flight_stops_it_from_landing`, for
    // the path where the checklist calls it out explicitly - a late audit
    // result must not be able to update a job that discard already swept
    // away, even though a real `AuditReport` moving out of the channel does
    // not touch the row it was for the way a health write does.
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    assert!(page.is_busy());

    page.apply(DatSourcesPageAction::Revert);
    assert!(
        !page.is_busy(),
        "discarding must stop the audit immediately"
    );
    assert!(page.view().audit.is_none());

    run_to_completion(&mut page);
    let view = page.view();
    assert!(view.running.is_none());
    assert!(
        view.audit.is_none(),
        "an audit result must not appear for a source that was discarded before \
         the run finished, got: {:?}",
        view.audit
    );
    assert!(
        view.audit_error.is_none(),
        "an abandoned job is not a failure the user needs telling about"
    );
}

#[test]
fn removing_a_source_with_no_job_running_does_not_touch_a_different_jobs_job() {
    // `abandon_job_for` must be surgical: removing a source that is not the
    // one a running job targets must leave that job alone. Reachable only at
    // the state layer today, since the GUI disables Remove entirely while any
    // job runs - covered here so the guarantee does not depend on that gate
    // staying in place.
    let (_fixture, mut page, roms) = audit_fixture();
    let dat_path = page.view().rows[0].path.clone();
    let unrelated = DatSourceEntry::new(
        "unrelated".to_string(),
        "Unrelated".to_string(),
        PathBuf::from(&dat_path).with_file_name("unrelated.dat"),
        DatSourceKind::File,
    );
    std::fs::write(&unrelated.path, LOGIQX).unwrap();
    page.apply(DatSourcesPageAction::AddFile {
        path: unrelated.path.clone(),
    });

    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    assert!(page.is_busy());

    // Removing the *other* source must not cancel the audit running against
    // "collection".
    page.apply(DatSourcesPageAction::Remove {
        id: "unrelated".to_string(),
    });
    assert!(
        page.is_busy(),
        "removing an unrelated source must not cancel a running job"
    );

    run_to_completion(&mut page);
    let view = page.view();
    assert!(
        view.audit.is_some(),
        "the audit against the still-registered source must have completed"
    );
}

// ---------------------------------------------------------------------------
// Wording
// ---------------------------------------------------------------------------

#[test]
fn the_page_states_what_it_supports_and_what_it_will_never_do() {
    // These two sentences are the page's contract with the user, so they are
    // pinned rather than left to drift.
    assert!(READ_ONLY_PROMISE.contains("never renames"));
    assert!(READ_ONLY_PROMISE.contains("deletes"));
    assert!(SUPPORTED_FORMATS.contains("Logiqx"));
    assert!(SUPPORTED_FORMATS.contains("ClrMamePro"));
}

// ---------------------------------------------------------------------------
// Validation warning presentation
// ---------------------------------------------------------------------------

/// A warning-severity diagnostic for a test report.
fn warn(message: impl Into<String>) -> DatDiagnostic {
    diagnostic(DiagnosticSeverity::Warning, "test_warning", message)
}

/// A parser-note-severity diagnostic for a test report.
fn note(message: impl Into<String>) -> DatDiagnostic {
    diagnostic(DiagnosticSeverity::Note, "test_note", message)
}

/// An error-severity diagnostic for a test report.
fn error(message: impl Into<String>) -> DatDiagnostic {
    diagnostic(DiagnosticSeverity::Error, "test_error", message)
}

fn diagnostic(
    severity: DiagnosticSeverity,
    code: &'static str,
    message: impl Into<String>,
) -> DatDiagnostic {
    DatDiagnostic {
        severity,
        code,
        message: message.into(),
        line: None,
        column: None,
    }
}

/// A page holding one folder source plus a stored validation report built by
/// the test, so diagnostic presentation can be driven without depending on
/// parser wording. The health state is supplied by the test because it now
/// depends on the severities present.
fn page_with_report(
    per_file_diagnostics: Vec<Vec<DatDiagnostic>>,
    state: DatHealthState,
    truncated: bool,
    total_dat_files: Option<usize>,
) -> (Fixture, DatSourcesPageState) {
    let fixture = Fixture::new();
    let folder = fixture.dir("warn");
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFolder {
        path: folder.clone(),
    });
    let id = "warn".to_string();
    let files: Vec<DatFileReport> = per_file_diagnostics
        .iter()
        .enumerate()
        .map(|(index, diagnostics)| DatFileReport {
            path: format!("{}/warn-{index}.dat", folder.display()),
            file_name: format!("warn-{index}.dat"),
            outcome: DatFileOutcome::Parsed {
                format: DatFormat::Logiqx,
                ecosystem: DatEcosystem::GenericLogiqx,
                name: Some("Test Catalogue".to_string()),
                version: Some("2026-01-01".to_string()),
                entry_count: 1,
                rom_count: 1,
                diagnostics: diagnostics.clone(),
            },
        })
        .collect();
    let report = DatValidationReport {
        source_id: id.clone(),
        path: folder.to_string_lossy().into_owned(),
        kind: "DAT folder",
        state,
        files,
        duplicate_identities: Vec::new(),
        skipped: Vec::new(),
        truncated,
        total_dat_files,
        summary: "1 DAT files, 1 entries, 1 ROMs".to_string(),
        entry_count: 1,
        rom_count: 1,
        formats: vec!["Logiqx XML".to_string()],
        path_refusal: None,
    };
    page.validations.insert(id.clone(), report.clone());
    if let Some(entry) = page.draft.get_mut(&id) {
        entry.health = report.to_health(&folder, DatSourceKind::Folder);
    }
    (fixture, page)
}

/// Draws the page headlessly, the way the cheat-sources page's tests do.
fn render(view: &DatSourcesPageView, ui_state: &mut DatSourcesPageUi) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_dat_sources_page(ui, view, ui_state);
        });
    })
}

/// Draws only the running-job card, so the platform line can be asserted
/// without the source card's own platform control interfering.
fn render_running_card(running: &RunningJobView) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_running_job(ui, running);
        });
    })
}

/// The same helper the shared widgets' own tests use.
fn rendered_text_contains(output: &egui::FullOutput, needle: &str) -> bool {
    fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Text(text_shape) => text_shape.galley.text().contains(needle),
            egui::Shape::Vec(nested) => nested.iter().any(|shape| shape_contains(shape, needle)),
            _ => false,
        }
    }
    output
        .shapes
        .iter()
        .any(|clipped| shape_contains(&clipped.shape, needle))
}

/// How many times `needle` appears across every rendered text shape.
fn rendered_text_count(output: &egui::FullOutput, needle: &str) -> usize {
    fn shape_count(shape: &egui::Shape, needle: &str) -> usize {
        match shape {
            egui::Shape::Text(text_shape) => text_shape.galley.text().matches(needle).count(),
            egui::Shape::Vec(nested) => nested.iter().map(|shape| shape_count(shape, needle)).sum(),
            _ => 0,
        }
    }
    output
        .shapes
        .iter()
        .map(|clipped| shape_count(&clipped.shape, needle))
        .sum()
}

#[test]
fn warnings_render_count_summary_and_expandable_details() {
    let warnings = vec![
        "The header version differs from the file's name".to_string(),
        "A ROM entry has no SHA-1 checksum; only CRC32 was compared".to_string(),
    ];
    let diagnostics = vec![warnings.iter().map(warn).collect::<Vec<_>>()];
    let (_fixture, page) =
        page_with_report(diagnostics, DatHealthState::ValidWithWarnings, false, None);
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 2);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 2);
    assert_eq!(row.health_state, DatHealthState::ValidWithWarnings);

    let mut ui_state = DatSourcesPageUi::default();
    let collapsed = render(&view, &mut ui_state);
    assert!(
        rendered_text_contains(&collapsed, "2 warning types, 2 occurrences"),
        "the type and occurrence counts must sit on the card"
    );
    assert!(
        rendered_text_contains(&collapsed, "View locations"),
        "an expandable control must be offered"
    );
    for warning in &warnings {
        assert!(
            rendered_text_contains(&collapsed, warning),
            "each group's message is visible without expanding"
        );
    }
    assert!(
        !rendered_text_contains(&collapsed, "Location unavailable"),
        "the drill-down must stay hidden until the user expands a group"
    );

    // Expanding one group reveals its locations (unavailable here, since the
    // test diagnostics carry no parser location).
    ui_state.open_diagnostic = Some(row.groups[0].id.clone());
    let expanded = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&expanded, "Hide locations"));
    assert!(rendered_text_contains(&expanded, "Location unavailable"));
}

#[test]
fn zero_warnings_show_no_warning_details_control() {
    let (_fixture, page) = page_with_report(vec![Vec::new()], DatHealthState::Valid, false, None);
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 0);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 0);
    assert!(row.groups.is_empty());

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        !rendered_text_contains(&output, "View locations"),
        "no diagnostics means no details control"
    );
}
#[test]
fn warnings_and_parser_notes_render_as_separate_sections() {
    // A parsed file carrying both a real warning and a parser note must show
    // them as two distinct, labelled sections - the note must never be counted
    // as a warning.
    let note_text = "DOCTYPE declaration accepted as inert text";
    let warning_text =
        "crc attribute on a rom element is not a well-formed checksum and was dropped";
    let (_fixture, page) = page_with_report(
        vec![vec![warn(warning_text), note(note_text)]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);
    let warning_group = row.groups_of(DiagnosticSeverity::Warning)[0];
    let note_group = row.groups_of(DiagnosticSeverity::Note)[0];
    assert_eq!(warning_group.message, warning_text);
    assert_eq!(note_group.message, note_text);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(
        &output,
        "1 warning type, 1 occurrence"
    ));
    assert!(rendered_text_contains(
        &output,
        "1 parser-note type, 1 occurrence"
    ));
    // The note reassurance is always on the card.
    assert!(rendered_text_contains(
        &output,
        "Parser notes are expected parser behaviour and need no action."
    ));

    // Expanding a note group reveals its (unavailable) location.
    ui_state.open_diagnostic = Some(note_group.id.clone());
    let expanded = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&expanded, "Hide locations"));
    assert!(rendered_text_contains(&expanded, note_text));
}

#[test]
fn an_error_diagnostic_renders_in_its_own_section_not_as_a_warning() {
    // An Error-severity diagnostic must not be folded into the warnings list:
    // it gets its own Blocked section, and the source reads Invalid.
    let error_text = "the catalogue declares an entry the build refuses to index";
    let (_fixture, page) = page_with_report(
        vec![vec![error(error_text)]],
        DatHealthState::Invalid,
        false,
        None,
    );
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Error), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 0);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 0);
    assert_eq!(row.health_state, DatHealthState::Invalid);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(
        &output,
        "1 error type, 1 occurrence"
    ));
    assert!(rendered_text_contains(&output, error_text));
    assert!(
        !rendered_text_contains(&output, "warning type"),
        "the error must not appear in a warning section"
    );
}

#[test]
fn mixed_errors_warnings_and_notes_render_as_three_sections() {
    // All three severities present: each gets its own labelled section, and the
    // badge stays driven by core health (Invalid because an error is present).
    let error_text = "one entry was refused";
    let warning_text = "a checksum was dropped";
    let note_text = "DOCTYPE declaration accepted as inert text";
    let (_fixture, page) = page_with_report(
        vec![vec![error(error_text), warn(warning_text), note(note_text)]],
        DatHealthState::Invalid,
        false,
        None,
    );
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Error), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Invalid"));
    assert!(rendered_text_contains(
        &output,
        "1 error type, 1 occurrence"
    ));
    assert!(rendered_text_contains(
        &output,
        "1 warning type, 1 occurrence"
    ));
    assert!(rendered_text_contains(
        &output,
        "1 parser-note type, 1 occurrence"
    ));
}

#[test]
fn repeated_identical_notes_group_into_one_type_with_full_occurrence_count() {
    // A folder of 512 DAT files all carrying the same DOCTYPE note must render
    // as ONE group with an occurrence count - never 512 separate lines.
    let fixture = Fixture::new();
    let folder = fixture.dir("dats");
    for index in 0..512 {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile><header><name>Set {index}</name><version>1</version></header>
<game name="Game {index}"><rom name="g.bin" size="16" crc="0c7e7fd8"/></game></datafile>"#
        );
        std::fs::write(folder.join(format!("set-{index:04}.dat")), &xml).unwrap();
    }

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFolder { path: folder });
    page.apply(DatSourcesPageAction::Validate {
        id: "dats".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(
        row.health_state,
        DatHealthState::Valid,
        "parser notes do not lower the verdict"
    );
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Note), 512);
    let note_group = &row.groups_of(DiagnosticSeverity::Note)[0];
    assert_eq!(note_group.affected_file_count, 512);
    assert_eq!(
        note_group.occurrences.len(),
        MAX_DIAGNOSTIC_OCCURRENCES_SHOWN,
        "the drill-down must be bounded"
    );
    assert!(note_group.occurrences_truncated);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(
        &output,
        "1 parser-note type, 512 occurrences"
    ));
}

#[test]
fn thousands_of_diagnostics_stay_bounded_and_deterministic() {
    // 100 files x 30 identical warnings = 3000 occurrences: one group, exact
    // counts, a drill-down capped at MAX_DIAGNOSTIC_OCCURRENCES_SHOWN rows, and
    // a deterministic message order alongside a second distinct type.
    let per_file: Vec<Vec<DatDiagnostic>> = (0..100)
        .map(|_| (0..30).map(|_| warn("repeated checksum dropped")).collect())
        .collect();
    let mut with_second_type = per_file.clone();
    with_second_type[0].push(note("DOCTYPE declaration accepted as inert text"));

    let (_fixture, page) = page_with_report(
        with_second_type,
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let row = &page.view().rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(
        row.diagnostic_occurrences(DiagnosticSeverity::Warning),
        3000
    );
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);

    let warning_group = &row.groups_of(DiagnosticSeverity::Warning)[0];
    assert_eq!(warning_group.affected_file_count, 100);
    assert_eq!(
        warning_group.occurrences.len(),
        MAX_DIAGNOSTIC_OCCURRENCES_SHOWN,
        "the drill-down must stay bounded however many diagnostics exist"
    );
    assert!(warning_group.occurrences_truncated);
    assert_eq!(
        warning_group.occurrences[0].file_name, "warn-0.dat",
        "occurrence rows keep the file they belong to"
    );
    // 30 per file; the 50-row cap therefore spans warn-0.dat and warn-1.dat.
    assert_eq!(
        warning_group.occurrences[29].file_name, "warn-0.dat",
        "occurrences stay in file order"
    );
    assert_eq!(
        warning_group.occurrences.last().unwrap().file_name,
        "warn-1.dat",
        "the cap cuts mid-file, never out of order"
    );

    // Deterministic: repeated view builds produce identical groups.
    let first = page.view().rows[0].groups.clone();
    let second = page.view().rows[0].groups.clone();
    assert_eq!(first, second);
}

#[test]
fn expanding_one_group_does_not_expand_the_others() {
    let (_fixture, page) = page_with_report(
        vec![vec![
            warn("first warning text"),
            warn("second warning text"),
            note("first note text"),
        ]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let view = page.view();
    let row = &view.rows[0];
    let warning_groups = row.groups_of(DiagnosticSeverity::Warning);
    assert_eq!(warning_groups.len(), 2);

    let mut ui_state = DatSourcesPageUi::default();
    let collapsed = render(&view, &mut ui_state);
    assert_eq!(rendered_text_count(&collapsed, "View locations"), 3);

    // Open only the first warning group.
    ui_state.open_diagnostic = Some(warning_groups[0].id.clone());
    let expanded = render(&view, &mut ui_state);
    assert_eq!(
        rendered_text_count(&expanded, "Hide locations"),
        1,
        "exactly one group expands"
    );
    assert_eq!(rendered_text_count(&expanded, "View locations"), 2);
}

#[test]
fn diagnostics_group_by_code_not_only_by_message() {
    // The same message text under two different codes is two distinct types.
    let same_text = "identical wording, different kinds";
    let (_fixture, page) = page_with_report(
        vec![vec![
            DatDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "code_a",
                message: same_text.to_string(),
                line: None,
                column: None,
            },
            DatDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "code_b",
                message: same_text.to_string(),
                line: None,
                column: None,
            },
        ]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let row = &page.view().rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 2);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 2);
}

#[test]
fn diagnostics_with_the_same_message_at_different_severities_stay_separate() {
    // A Note and a Warning with identical wording are two distinct groups and
    // the note never drags the warning's verdict down.
    let same_text = "identical wording, different severities";
    let (_fixture, page) = page_with_report(
        vec![vec![note(same_text), warn(same_text)]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let row = &page.view().rows[0];
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 1);
}

#[test]
fn clrmamepro_truncation_warnings_group_across_files() {
    // Regression: the ClrMamePro parser records a byte offset on its only
    // warning, and DatDiagnostic used to carry the Display form of the message
    // ("… (byte 12)"). Every occurrence then had a unique message and grouping
    // silently failed for TOSEC/ClrMamePro - the exact format this PR exists
    // for. The message is now the raw text; the offset lives in the location,
    // so two identical truncations in two files become ONE group.
    let fixture = Fixture::new();
    let folder = fixture.dir("dats");
    let description = "x".repeat(80);
    for (name, header) in [("a", "A"), ("b", "B")] {
        std::fs::write(
            folder.join(format!("{name}.dat")),
            format!(
                "clrmamepro (\n\tname {header}\n\tdescription {description}\n)\n\
                 game ( name G rom ( name {name}.bin size 1 crc deadbeef ) )\n"
            ),
        )
        .unwrap();
    }

    let mut page = fixture.page();
    page.limits = DatLimits::builder().max_description_length(10).build();
    page.apply(DatSourcesPageAction::AddFolder { path: folder });
    page.apply(DatSourcesPageAction::Validate {
        id: "dats".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(
        row.health_state,
        DatHealthState::ValidWithWarnings,
        "a truncated description is a genuine warning"
    );
    assert_eq!(
        row.diagnostic_types(DiagnosticSeverity::Warning),
        1,
        "two identical truncations must be one diagnostic type"
    );
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 2);
    let group = &row.groups_of(DiagnosticSeverity::Warning)[0];
    assert_eq!(group.code, "description_truncated");
    assert_eq!(
        group.message, "description truncated from 80 to 10 bytes",
        "the raw message must not carry a per-occurrence byte offset"
    );
    assert_eq!(group.affected_file_count, 2);
    // Each occurrence keeps its own genuine one-based line and no invented
    // column.
    assert!(group.occurrences.iter().all(|o| o.line.is_some()));
    assert!(group.occurrences.iter().all(|o| o.column.is_none()));
}

#[test]
fn expanding_a_group_on_one_source_does_not_open_the_same_group_on_another() {
    // Regression: the group id used to be "{code}:{message}", so expanding the
    // DOCTYPE note on source A left source B's identical note open on load. The
    // id is now scoped by source and severity.
    let note_text = "DOCTYPE declaration accepted as inert text";
    let fixture = Fixture::new();

    // Build two independent sources that both carry the same note.
    let mut pages = Vec::new();
    for name in ["a", "b"] {
        let folder = fixture.dir(name);
        let mut page = fixture.page();
        page.apply(DatSourcesPageAction::AddFolder {
            path: folder.clone(),
        });
        let id = name.to_string();
        let report = DatValidationReport {
            source_id: id.clone(),
            path: folder.to_string_lossy().into_owned(),
            kind: "DAT folder",
            state: DatHealthState::Valid,
            files: vec![DatFileReport {
                path: format!("{}/x.dat", folder.display()),
                file_name: format!("{name}.dat"),
                outcome: DatFileOutcome::Parsed {
                    format: DatFormat::Logiqx,
                    ecosystem: DatEcosystem::GenericLogiqx,
                    name: Some("Test".to_string()),
                    version: Some("1".to_string()),
                    entry_count: 1,
                    rom_count: 1,
                    diagnostics: vec![note(note_text)],
                },
            }],
            duplicate_identities: Vec::new(),
            skipped: Vec::new(),
            truncated: false,
            total_dat_files: None,
            summary: "1 DAT files, 1 entries, 1 ROMs".to_string(),
            entry_count: 1,
            rom_count: 1,
            formats: vec!["Logiqx XML".to_string()],
            path_refusal: None,
        };
        page.validations.insert(id, report);
        pages.push(page);
    }

    let view_a = pages[0].view();
    let view_b = pages[1].view();
    assert_ne!(
        view_a.rows[0].groups[0].id, view_b.rows[0].groups[0].id,
        "same-typed groups on different sources must have distinct ids"
    );

    // Opening A's group must not leave B's group open.
    let mut ui_state = DatSourcesPageUi {
        open_diagnostic: Some(view_a.rows[0].groups[0].id.clone()),
        ..Default::default()
    };
    let rendered_a = render(&view_a, &mut ui_state);
    assert_eq!(rendered_text_count(&rendered_a, "Hide locations"), 1);
    let rendered_b = render(&view_b, &mut ui_state);
    assert_eq!(
        rendered_text_count(&rendered_b, "Hide locations"),
        0,
        "source B's identical group must not be open just because A's was"
    );
    assert_eq!(rendered_text_count(&rendered_b, "View locations"), 1);
}

#[test]
fn drill_down_shows_parser_location_when_available_and_unavailable_otherwise() {
    // The drill-down shows line/column only when the parser provided one;
    // otherwise it says "Location unavailable". It never re-parses to build.
    let with_location = DatDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "test_warning",
        message: "has a location".to_string(),
        line: Some(3),
        column: Some(12),
    };
    // The ClrMamePro parser records a line but no column; that shape must read
    // "line N", not "line N:0" and not "Location unavailable".
    let line_only = DatDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "test_warning",
        message: "has a line only".to_string(),
        line: Some(9),
        column: None,
    };
    let without = warn("no location");
    let (_fixture, page) = page_with_report(
        vec![vec![with_location, line_only, without]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let view = page.view();
    let row = &view.rows[0];
    let groups = row.groups_of(DiagnosticSeverity::Warning);
    assert_eq!(groups.len(), 3);
    let located = groups
        .iter()
        .find(|group| group.message == "has a location")
        .unwrap();
    assert_eq!(located.occurrences[0].line, Some(3));
    assert_eq!(located.occurrences[0].column, Some(12));

    let mut ui_state = DatSourcesPageUi {
        open_diagnostic: Some(located.id.clone()),
        ..Default::default()
    };
    let located_output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&located_output, "line 3:12"));

    let line_only_group = groups
        .iter()
        .find(|group| group.message == "has a line only")
        .unwrap();
    ui_state.open_diagnostic = Some(line_only_group.id.clone());
    let line_only_output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&line_only_output, "line 9"));
    assert!(
        !rendered_text_contains(&line_only_output, "line 9:0"),
        "a missing column must not be rendered as zero"
    );

    let unlocated = groups
        .iter()
        .find(|group| group.message == "no location")
        .unwrap();
    ui_state.open_diagnostic = Some(unlocated.id.clone());
    let unlocated_output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(
        &unlocated_output,
        "Location unavailable"
    ));
}

#[test]
fn a_safety_limit_stop_is_labelled_incomplete_and_counts_are_exact() {
    // Both numbers genuinely known: the read count and the folder's real total.
    let (_fixture, page) = page_with_report(
        vec![vec![warn("w")]],
        DatHealthState::ValidWithWarnings,
        true,
        Some(2024),
    );
    let row = &page.view().rows[0];
    assert!(row.incomplete_load);
    assert_eq!(row.dat_files_read, Some(1));
    assert_eq!(row.dat_files_total, Some(2024));
    assert_eq!(
        row.incomplete_load_line().as_deref(),
        Some("1 of 2024 DAT files read")
    );

    // An unknown total never invents one: the safety limit is named instead.
    let (_fixture, page) = page_with_report(
        vec![vec![warn("w")]],
        DatHealthState::ValidWithWarnings,
        true,
        None,
    );
    let row = &page.view().rows[0];
    assert!(row.incomplete_load);
    assert_eq!(row.dat_files_total, None);
    assert_eq!(
        row.incomplete_load_line().as_deref(),
        Some("Processing stopped at the configured safety limit")
    );
}

#[test]
fn an_incomplete_load_is_drawn_prominently_with_its_counts() {
    let (_fixture, page) = page_with_report(
        vec![vec![warn("w")]],
        DatHealthState::ValidWithWarnings,
        true,
        Some(2024),
    );
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        rendered_text_contains(&output, "Incomplete catalogue load"),
        "the incompleteness must be a headline, not body text"
    );
    assert!(rendered_text_contains(&output, "1 of 2024 DAT files read"));
}

#[test]
fn unknown_total_never_invents_a_count_or_percentage() {
    assert_eq!(format_percentage(5, 0), None);
    assert_eq!(format_percentage(0, 0), None);

    let (_fixture, page) = page_with_report(
        vec![vec![warn("w")]],
        DatHealthState::ValidWithWarnings,
        true,
        None,
    );
    let row = &page.view().rows[0];
    assert_eq!(row.dat_files_read, Some(1), "the read count is still known");
    assert_eq!(row.dat_files_total, None);
    assert!(
        !row.incomplete_load_line().unwrap().contains("of"),
        "no invented total may appear: {:?}",
        row.incomplete_load_line()
    );
}

#[test]
fn warning_order_is_deterministic() {
    let per_file = vec![
        vec![warn("first-a"), warn("second-a")],
        vec![warn("first-b"), warn("second-b")],
    ];
    let (_fixture, page) =
        page_with_report(per_file, DatHealthState::ValidWithWarnings, false, None);
    let row = &page.view().rows[0];
    let messages: Vec<&str> = row
        .groups
        .iter()
        .map(|group| group.message.as_str())
        .collect();
    assert_eq!(
        messages,
        vec!["first-a", "first-b", "second-a", "second-b"],
        "groups must come in a deterministic order (by message), never in read_dir order"
    );
}

#[test]
fn the_history_and_logs_reference_is_only_drawn_when_details_are_recorded_there() {
    let (_fixture, page) = page_with_report(
        vec![vec![warn("w")]],
        DatHealthState::ValidWithWarnings,
        false,
        None,
    );
    let mut view = page.view();
    // Nothing is recorded in History & Logs today, so the honest card does not
    // point there.
    assert!(!view.rows[0].history_link_available);

    let mut ui_state = DatSourcesPageUi::default();
    assert!(
        !rendered_text_contains(&render(&view, &mut ui_state), "History & Logs"),
        "no link may be offered when the details are not recorded there"
    );

    // If the flag is ever set because the details genuinely are recorded there,
    // the reference is drawn.
    view.rows[0].history_link_available = true;
    assert!(rendered_text_contains(
        &render(&view, &mut ui_state),
        "History & Logs"
    ));
}

#[test]
fn warnings_are_prominent_on_the_card_not_buried_behind_inspect() {
    // Regression: the old card showed the "Valid, with warnings" badge, but
    // the warning text was only reachable by opening Inspect and reading a
    // nested per-file list. The type/occurrence counts and the expandable
    // drill-down control now sit on the card itself.
    let diagnostics = vec![vec![
        warn("A ROM entry has no SHA-1 checksum; only CRC32 was compared"),
        warn("The header declares a version that differs from the filename"),
    ]];
    let (_fixture, page) =
        page_with_report(diagnostics, DatHealthState::ValidWithWarnings, false, None);
    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.health_state, DatHealthState::ValidWithWarnings);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 2);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Valid, with warnings"));
    assert!(
        rendered_text_contains(&output, "2 warning types, 2 occurrences"),
        "the counts must be visible without any disclosure click"
    );
    assert!(
        rendered_text_contains(&output, "View locations"),
        "the expandable control must be visible without opening Inspect"
    );
}

// ---------------------------------------------------------------------------
// Diagnostic severity: the TOSEC DOCTYPE reproduction and its neighbours
// ---------------------------------------------------------------------------

/// A Logiqx XML DAT carrying the standard DOCTYPE plus `games` entries.
///
/// The DOCTYPE is expected parser behaviour and must surface as a parser note,
/// never as a warning. This is the reproduction reported against the GUI: a
/// single TOSEC DAT whose only diagnostic was the DOCTYPE, shown as "Valid,
/// with warnings" and "1 warning".
fn logiqx_with_doctype_and_entries(games: usize) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \
         \"http://www.logiqx.com/Dats/datafile.dtd\">\n\
         <datafile>\n\
         <header><name>Test TOSEC Set</name><version>2026-01-01</version></header>\n",
    );
    for index in 0..games {
        xml.push_str(&format!(
            "<game name=\"Game {index}\"><rom name=\"g{index}.bin\" size=\"16\" crc=\"{index:08x}\"/></game>\n"
        ));
    }
    xml.push_str("</datafile>\n");
    xml
}

/// A Logiqx XML DAT whose checksum is malformed: the parser drops it and warns,
/// so the DAT parses but carries a real warning.
fn logiqx_with_malformed_checksum(doctype: bool) -> String {
    let doctype = if doctype {
        "<!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \
         \"http://www.logiqx.com/Dats/datafile.dtd\">\n"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
{doctype}<datafile><game name="G"><rom name="a.bin" size="4" crc="not-a-checksum"/></game></datafile>"#
    )
}

#[test]
fn a_doctype_parser_note_shows_valid_with_no_warnings() {
    // The exact reproduction: a single TOSEC DAT, 1005 entries, whose only
    // diagnostic is the DOCTYPE parser note. It must read "Valid" and "1 parser
    // note", never "Valid, with warnings" or "1 warning".
    let fixture = Fixture::new();
    let dat = fixture.write("tosec.dat", &logiqx_with_doctype_and_entries(1005));

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "tosec".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.entry_count, Some(1005));
    assert_eq!(row.rom_count, Some(1005));
    assert_eq!(
        row.health_state,
        DatHealthState::Valid,
        "a parser note must not lower the verdict"
    );
    assert_eq!(
        row.diagnostic_types(DiagnosticSeverity::Warning),
        0,
        "the DOCTYPE must not surface as a warning"
    );
    assert_eq!(
        row.diagnostic_types(DiagnosticSeverity::Note),
        1,
        "the DOCTYPE must be a single parser-note type"
    );
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Note), 1);
    let note_group = &row.groups_of(DiagnosticSeverity::Note)[0];
    assert!(
        note_group.message.contains("DOCTYPE"),
        "{}",
        note_group.message
    );

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(!rendered_text_contains(&output, "with warnings"));
    assert!(
        !rendered_text_contains(&output, "warning type"),
        "the note must not be called a warning"
    );
    assert!(rendered_text_contains(
        &output,
        "1 parser-note type, 1 occurrence"
    ));
    assert!(rendered_text_contains(&output, "View locations"));
}

#[test]
fn a_real_warning_shows_valid_with_warnings() {
    let fixture = Fixture::new();
    let dat = fixture.write("warn.dat", &logiqx_with_malformed_checksum(false));

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "warn".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.health_state, DatHealthState::ValidWithWarnings);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_occurrences(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 0);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Valid, with warnings"));
    assert!(rendered_text_contains(
        &output,
        "1 warning type, 1 occurrence"
    ));
    assert!(rendered_text_contains(&output, "View locations"));
}

#[test]
fn a_real_parser_failure_shows_invalid() {
    let fixture = Fixture::new();
    let dat = fixture.write("broken.dat", "<?xml version=\"1.0\"?><datafile><game");

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "broken".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(row.health_state, DatHealthState::Invalid);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 0);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 0);
    assert!(row.groups.is_empty());

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Invalid"));
    assert!(!rendered_text_contains(&output, "View locations"));
}

#[test]
fn mixed_warning_and_notes_shows_valid_with_warnings() {
    let fixture = Fixture::new();
    let dat = fixture.write("mixed.dat", &logiqx_with_malformed_checksum(true));
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Validate {
        id: "mixed".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let row = &view.rows[0];
    assert_eq!(
        row.health_state,
        DatHealthState::ValidWithWarnings,
        "a warning overrides parser notes in the verdict"
    );
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Warning), 1);
    assert_eq!(row.diagnostic_types(DiagnosticSeverity::Note), 1);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Valid, with warnings"));
    assert!(rendered_text_contains(
        &output,
        "1 warning type, 1 occurrence"
    ));
    assert!(rendered_text_contains(
        &output,
        "1 parser-note type, 1 occurrence"
    ));
}

#[test]
fn mixed_errors_warnings_and_notes_shows_invalid() {
    let fixture = Fixture::new();
    let folder = fixture.dir("mixed");
    std::fs::write(
        folder.join("broken.dat"),
        "<?xml version=\"1.0\"?><datafile><game",
    )
    .unwrap();
    std::fs::write(folder.join("ok.dat"), logiqx_with_malformed_checksum(true)).unwrap();

    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFolder { path: folder });
    page.apply(DatSourcesPageAction::Validate {
        id: "mixed".to_string(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    assert_eq!(
        view.rows[0].health_state,
        DatHealthState::Invalid,
        "an error in any file makes the whole source invalid"
    );

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Invalid"));
}

// ---------------------------------------------------------------------------
// Audit progress: ETA and formatting
// ---------------------------------------------------------------------------

#[test]
fn no_eta_before_one_hundred_files() {
    let mut estimator = EtaEstimator::new();
    estimator.update(50, 6.0);
    estimator.update(90, 12.0);
    let eta = estimator.eta(90, 1000, 12.0);
    assert!(
        !matches!(eta, EtaView::About { .. }),
        "an ETA must not appear before 100 files: {eta:?}"
    );
    assert_eq!(eta, EtaView::Estimating);
}

#[test]
fn no_eta_before_five_seconds() {
    let mut estimator = EtaEstimator::new();
    estimator.update(50, 0.0);
    estimator.update(150, 3.0);
    let eta = estimator.eta(150, 1000, 3.0);
    assert!(
        !matches!(eta, EtaView::About { .. }),
        "an ETA must not appear before 5 seconds: {eta:?}"
    );
    assert_eq!(eta, EtaView::Estimating);
}

#[test]
fn unknown_total_produces_no_eta() {
    let mut tracker = AuditProgressTracker::new();
    tracker.update(
        &DatAuditProgress::Scanning {
            files_found: 42,
            current_dir: Some("/home/user/roms".to_string()),
        },
        12.0,
    );
    let view = tracker.view(12);
    assert_eq!(view.total_files, None);
    assert_eq!(view.eta, EtaView::None);
    assert_eq!(view.percent, None);
    // The position must not invent a denominator.
    assert_eq!(view.position(), "42 files so far");
}

#[test]
fn stable_progress_produces_an_approximate_eta() {
    let mut estimator = EtaEstimator::new();
    estimator.update(100, 10.0);
    estimator.update(200, 20.0);
    estimator.update(300, 30.0);
    // 100 files per 10 seconds, 700 remaining -> about 70 seconds.
    match estimator.eta(300, 1000, 30.0) {
        EtaView::About { seconds_remaining } => {
            assert!(
                (55..=85).contains(&seconds_remaining),
                "{seconds_remaining}"
            );
            let line = format_eta_remaining(seconds_remaining);
            assert!(line.starts_with("About "), "{line}");
            assert!(line.ends_with("remaining"), "{line}");
        }
        other => panic!("expected an approximate ETA, got {other:?}"),
    }
}

#[test]
fn eta_is_smoothed_not_jumping_from_one_sample() {
    let mut estimator = EtaEstimator::new();
    estimator.update(100, 10.0);
    estimator.update(200, 20.0);
    estimator.update(300, 30.0);
    // One fast frame: 100 files in 1 second (100/s). A naive estimate would
    // drop to ~6 seconds remaining; the smoothed one moves only partway.
    estimator.update(400, 31.0);
    match estimator.eta(400, 1000, 31.0) {
        EtaView::About { seconds_remaining } => {
            assert!(
                seconds_remaining >= 15,
                "the ETA must not jump to the single-frame speed: {seconds_remaining}s"
            );
            assert!(
                seconds_remaining < 60,
                "the ETA must move toward the spike, not ignore it: {seconds_remaining}s"
            );
        }
        other => panic!("expected an approximate ETA, got {other:?}"),
    }
}

#[test]
fn zero_progress_cannot_divide_by_zero() {
    assert_eq!(format_percentage(0, 0), None);
    assert_eq!(format_percentage(5, 0), None);

    let mut estimator = EtaEstimator::new();
    estimator.update(0, 0.0);
    estimator.update(0, 5.0);
    assert_eq!(estimator.eta(0, 500, 5.0), EtaView::None);

    let mut tracker = AuditProgressTracker::new();
    tracker.update(
        &DatAuditProgress::Hashing {
            index: 0,
            total: 0,
            file_name: "x".to_string(),
        },
        6.0,
    );
    let view = tracker.view(6);
    assert_eq!(view.percent, None);
    assert_eq!(view.eta, EtaView::None);
}

#[test]
fn a_frozen_tracker_keeps_the_eta_it_had_at_its_last_update() {
    // Regression: the ETA must be a snapshot from the last progress update,
    // not recomputed from the live wall clock - otherwise a stalled or
    // cancelled run could flip from "Estimating…" to a number purely because
    // seconds passed.
    let mut tracker = AuditProgressTracker::new();
    tracker.update(
        &DatAuditProgress::Hashing {
            index: 50,
            total: 1000,
            file_name: "a".to_string(),
        },
        2.0,
    );
    tracker.update(
        &DatAuditProgress::Hashing {
            index: 200,
            total: 1000,
            file_name: "b".to_string(),
        },
        6.0,
    );
    let eta_at_6 = tracker.view(6).eta.clone();
    assert!(matches!(eta_at_6, EtaView::About { .. }));

    let eta_at_600 = tracker.view(600).eta;
    assert_eq!(
        eta_at_600, eta_at_6,
        "a tracker that has not been fed must not change its ETA as the clock moves"
    );
}

#[test]
fn draining_a_progress_backlog_keeps_the_eta_stable() {
    // Regression: poll() used to timestamp every drained AuditProgress message
    // with its own `started_at.elapsed()`. A backlog queued between GUI frames
    // is drained within microseconds, so EtaEstimator saw a large delta_files
    // over a near-zero delta_seconds and spiked the throughput, collapsing the
    // ETA toward zero. poll() now reads the clock once per drain pass: every
    // message in the burst shares one elapsed value, the `delta_seconds > 0`
    // guard skips the rest of the burst, and the rate stays where the normally
    // spaced passes put it.
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    // Drive the job through a controllable channel, backdating the clock so
    // the confidence gates (100 files, 5 seconds) are already open. A larger
    // channel than the production constant lets the burst queue entirely
    // without a blocking send.
    let cancel = page.job.as_ref().expect("a job is running").cancel.clone();
    let (sender, messages) = sync_channel(256);
    page.job = Some(RunningJob {
        kind: JobKind::Audit,
        source_id: "collection".to_string(),
        cancel,
        cancel_requested: false,
        messages,
        latest: "Starting…".to_string(),
        started_at: Instant::now() - Duration::from_secs(60),
        audit_progress: Some(AuditProgressTracker::new()),
        platform_display: None,
    });

    let total = 1000;
    let hash = |index: usize| {
        JobMessage::AuditProgress(DatAuditProgress::Hashing {
            index,
            total,
            file_name: format!("f{index}.bin"),
        })
    };

    // Two normally spaced passes establish a steady rate (one file per 20 ms).
    sender.send(hash(1)).unwrap();
    page.poll();
    std::thread::sleep(Duration::from_millis(20));
    sender.send(hash(2)).unwrap();
    page.poll();

    // Queue a backlog, then drain all of it in a single poll() pass. The EMA
    // (alpha 0.2) needs ~21 samples to converge toward a spike rate, so 101
    // queued messages are plenty to make the old per-message timing collapse
    // the ETA, while staying above the 100-file confidence gate.
    std::thread::sleep(Duration::from_millis(20));
    for index in 3..=103 {
        sender.send(hash(index)).unwrap();
    }
    page.poll();

    let running = page.view().running.expect("the job is still running");
    let progress = running.progress.as_ref().expect("audit progress");
    assert_eq!(progress.files_checked, 103);
    // Coalescing: after draining a 101-message backlog in one pass, the detail
    // line is the last event's, not the first's.
    assert_eq!(running.detail, "Checking 103 of 1000: f103.bin");
    match &progress.eta {
        EtaView::About { seconds_remaining } => {
            // ~50 files/s with 897 left is on the order of 18 seconds. A
            // per-message timestamp on the drained backlog would compute
            // millions of files per second and collapse this to ~1 second.
            assert!(
                *seconds_remaining >= 5,
                "the ETA must not collapse toward zero after a drained backlog: \
                 {seconds_remaining}s"
            );
        }
        other => panic!("expected a real ETA after a stable run, got {other:?}"),
    }
}

#[test]
fn completed_progress_shows_one_hundred_percent() {
    assert_eq!(format_percentage(500, 500), Some(100));
    assert_eq!(format_percentage(500, 1000), Some(50));

    let mut tracker = AuditProgressTracker::new();
    tracker.update(
        &DatAuditProgress::Hashing {
            index: 500,
            total: 500,
            file_name: "last.bin".to_string(),
        },
        30.0,
    );
    let view = tracker.view(30);
    assert_eq!(view.percent, Some(100));
    assert_eq!(view.position(), "500 of 500");
}

#[test]
fn the_current_path_is_shortened_safely() {
    assert_eq!(
        shorten_path("/home/user/private/games/platform"),
        "…/games/platform"
    );
    assert_eq!(shorten_path("/a/b/c/d/e/f"), "…/e/f");
    // Short paths are returned as they are; nothing panics on edge cases.
    assert_eq!(shorten_path("/roms"), "/roms");
    assert_eq!(shorten_path(""), "");
}

#[test]
fn a_private_path_never_enters_the_detail_or_progress_text() {
    let private = "/home/user/private";
    let description = describe(&DatAuditProgress::Scanning {
        files_found: 7,
        current_dir: Some(format!("{private}/platform")),
    });
    assert!(!description.contains(private), "{description}");

    let mut tracker = AuditProgressTracker::new();
    tracker.update(
        &DatAuditProgress::Scanning {
            files_found: 7,
            current_dir: Some(format!("{private}/platform")),
        },
        3.0,
    );
    let view = tracker.view(3);
    let shown = view.current_path.expect("a current path is shown");
    assert!(!shown.contains(private), "{shown}");
    assert_eq!(shown, "…/private/platform");
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// Replaces the running job with a controllable one on a fresh channel, so a
/// test can drive the exact message sequence without racing a worker thread.
fn take_over_job(page: &mut DatSourcesPageState, latest: &str) -> SyncSender<JobMessage> {
    let cancel = page.job.as_ref().expect("a job is running").cancel.clone();
    let (sender, messages) = sync_channel(PROGRESS_QUEUE_DEPTH);
    page.job = Some(RunningJob {
        kind: JobKind::Audit,
        source_id: "collection".to_string(),
        cancel,
        cancel_requested: false,
        messages,
        latest: latest.to_string(),
        started_at: Instant::now(),
        audit_progress: Some(AuditProgressTracker::new()),
        platform_display: None,
    });
    sender
}

/// A completed-audit outcome, for proving a late result after cancellation is
/// dropped rather than presented.
fn minimal_outcome() -> DatAuditOutcome {
    DatAuditOutcome {
        source_id: "collection".to_string(),
        source_display_name: "collection.dat".to_string(),
        dat_path: "/tmp/collection.dat".to_string(),
        scan_root: "/tmp/roms".to_string(),
        catalogue_names: vec!["Test No-Intro Collection".to_string()],
        catalogue_entries: 1,
        catalogue_roms: 1,
        unreadable_catalogues: Vec::new(),
        report: AuditReport {
            entries: Vec::new(),
            summary: AuditSummary::default(),
        },
        unhashed: Vec::new(),
        files_scanned: 2,
        bytes_hashed: 4,
        truncated: false,
        policy: None,
        platform: None,
    }
}

#[test]
fn cancellation_changes_the_wording_to_stopping() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    let sender = take_over_job(&mut page, "Checking 10 of 100: a.bin");

    let running = page.view().running.expect("a job is running");
    assert_eq!(running.heading(), "Auditing 'collection'");
    assert!(!running.cancellation_requested);

    page.apply(DatSourcesPageAction::CancelJob);

    let running = page.view().running.expect("still busy while stopping");
    assert!(running.cancellation_requested);
    assert!(
        running.heading().contains("Stopping"),
        "{}",
        running.heading()
    );
    assert!(
        page.is_busy(),
        "the operation stays busy until the worker confirms termination"
    );

    // The worker's confirmation ends it without any result.
    sender.send(JobMessage::Cancelled).unwrap();
    page.poll();
    assert!(page.view().running.is_none());
    assert!(page.view().audit.is_none());
}

#[test]
fn stale_progress_after_cancellation_is_ignored() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    let sender = take_over_job(&mut page, "Starting…");

    // A real progress update before cancellation, so there is something to
    // freeze.
    sender
        .send(JobMessage::AuditProgress(DatAuditProgress::Hashing {
            index: 10,
            total: 100,
            file_name: "a.bin".to_string(),
        }))
        .unwrap();
    page.poll();
    let before = page.view().running.expect("running");
    assert_eq!(before.detail, "Checking 10 of 100: a.bin");
    assert_eq!(before.progress.as_ref().unwrap().files_checked, 10);

    page.apply(DatSourcesPageAction::CancelJob);

    // The worker has not observed the flag yet and goes on reporting. None of
    // it may move the shown state.
    sender
        .send(JobMessage::AuditProgress(DatAuditProgress::Hashing {
            index: 11,
            total: 100,
            file_name: "c.bin".to_string(),
        }))
        .unwrap();
    page.poll();

    let running = page.view().running.expect("still busy");
    assert!(running.cancellation_requested);
    assert_eq!(
        running.detail, before.detail,
        "stale progress after cancellation must not change the shown detail"
    );
    assert_eq!(
        running.progress.as_ref().unwrap().files_checked,
        10,
        "stale progress after cancellation must not move the position or ETA"
    );
}

#[test]
fn a_cancelled_audit_never_appears_complete() {
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });
    let sender = take_over_job(&mut page, "Starting…");

    page.apply(DatSourcesPageAction::CancelJob);

    // Even if the worker finished the whole audit before it noticed the flag,
    // the page must not present that as a completed audit.
    sender
        .send(JobMessage::Audited {
            generation: 0,
            outcome: Box::new(minimal_outcome()),
            enrichment: None,
            plan: None,
        })
        .unwrap();
    page.poll();

    let view = page.view();
    assert!(view.running.is_none());
    assert!(
        view.audit.is_none(),
        "a cancelled audit never appears complete"
    );
    assert!(view.audit_error.is_none(), "cancelling is not a failure");
}

#[test]
fn platform_conflict_names_each_source_and_requires_review() {
    use archivefs_core::platform::identity::{
        PlatformIdentityConfidence, PlatformIdentityEvidence, PlatformIdentitySource,
    };

    let (_fixture, mut page, _roms) = audit_fixture();
    page.identity_enrichment = Some(Box::new(
        archivefs_core::PlatformIdentityEnrichmentSummary {
            conflicts: 1,
            conflict_details: vec![archivefs_core::PlatformIdentityConflictDetail {
                archive_id: 1,
                evidence: vec![
                    PlatformIdentityEvidence::canonical(
                        "PSX",
                        PlatformIdentitySource::VerifiedDat,
                        PlatformIdentityConfidence::Verified,
                        1,
                        "verified DAT fixture",
                    )
                    .unwrap(),
                    PlatformIdentityEvidence::canonical(
                        "PSP",
                        PlatformIdentitySource::Romm,
                        PlatformIdentityConfidence::High,
                        1,
                        "RomM fixture",
                    )
                    .unwrap(),
                ],
            }],
            ..Default::default()
        },
    ));

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&page.view(), &mut ui_state);
    for expected in [
        "Platform conflict",
        "Verified DAT: Sony PlayStation",
        "RomM: Sony PlayStation Portable",
        "Review required",
    ] {
        assert!(
            rendered_text_contains(&output, expected),
            "missing conflict text {expected:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Richer live audit context
// ---------------------------------------------------------------------------

#[test]
fn the_running_card_shows_the_platform_only_when_authoritative() {
    let (_fixture, mut page, roms) = audit_fixture();
    // An unassigned source gets no platform line at all.
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });
    let unassigned = page.view().running.expect("running").clone();
    assert!(
        unassigned.platform_display.is_none(),
        "no platform may be claimed for an unassigned source"
    );
    assert!(!rendered_text_contains(
        &render_running_card(&unassigned),
        "Platform:"
    ));
    page.apply(DatSourcesPageAction::CancelJob);
    run_to_completion(&mut page);

    // A recognised assignment is authoritative and appears on the running card.
    let canonical = archivefs_core::platform::canonical_ids()[0];
    page.apply(DatSourcesPageAction::SetPlatform {
        id: "collection".to_string(),
        platform: Some(canonical.to_string()),
    });
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms.clone(),
    });

    let assigned = page.view().running.expect("running").clone();
    assert_eq!(
        assigned.platform_display.as_deref(),
        Some(archivefs_core::platform::display_name_for(canonical)),
        "a resolved assignment must be shown"
    );
    assert!(rendered_text_contains(
        &render_running_card(&assigned),
        "Platform:"
    ));
    page.apply(DatSourcesPageAction::CancelJob);
    run_to_completion(&mut page);
}

#[test]
fn an_unresolved_platform_is_never_presented_on_the_running_card() {
    // An assignment this build does not recognise is kept, but must not be
    // presented as authoritative during a run.
    let (_fixture, mut page, roms) = audit_fixture();
    page.apply(DatSourcesPageAction::SetPlatform {
        id: "collection".to_string(),
        platform: Some("APlatformFromALaterBuild".to_string()),
    });
    page.apply(DatSourcesPageAction::Audit {
        id: "collection".to_string(),
        scan_root: roms,
    });

    let running = page.view().running.expect("running").clone();
    assert!(
        running.platform_display.is_none(),
        "an unresolved platform must not be claimed"
    );
    assert!(!rendered_text_contains(
        &render_running_card(&running),
        "Platform:"
    ));
    page.apply(DatSourcesPageAction::CancelJob);
    run_to_completion(&mut page);
}

// ---------------------------------------------------------------------------
// DAT matching policy: controls, summary, safety
// ---------------------------------------------------------------------------

/// Renders the whole page at a given window width.
fn render_at_width(
    view: &DatSourcesPageView,
    ui_state: &mut DatSourcesPageUi,
    width: f32,
) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 3000.0),
            )),
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let _ = show_dat_sources_page(ui, view, ui_state);
            });
        },
    )
}

#[test]
fn policy_edits_are_unsaved_changes_and_never_touch_the_rom_folder() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });

    // Set a preference.
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });
    page.apply(DatSourcesPageAction::SetRevisionPolicy {
        scope: None,
        policy: RevisionPolicy::LatestVerified,
    });

    let view = page.view();
    assert!(
        view.dirty,
        "a policy edit is an unsaved change exactly like a source edit"
    );
    assert_eq!(view.policy.region_preferences.len(), 1);
    assert_eq!(view.policy.region_preferences[0].label, "Europe");
    assert_eq!(view.policy.revision_policy, RevisionPolicy::LatestVerified);
    assert!(
        !fixture.config_path.exists(),
        "nothing is written before Save"
    );

    // Save writes only the registry file; the ROM folder is never touched.
    let roms = fixture.dir("roms");
    let before = snapshot(&roms);
    page.apply(DatSourcesPageAction::Save);
    let after = snapshot(&roms);
    assert_eq!(before, after, "no file beside the registry was touched");
    assert!(fixture.config_path.exists(), "Save wrote the registry");

    // A fresh page reloads the preference.
    let reloaded = fixture.page();
    assert_eq!(reloaded.view().policy.region_preferences[0].label, "Europe");
    assert_eq!(
        reloaded.view().policy.revision_policy,
        RevisionPolicy::LatestVerified
    );
}

#[test]
fn region_reorder_controls_move_and_remove() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    for region in [RegionId::Europe, RegionId::Usa, RegionId::Japan] {
        page.apply(DatSourcesPageAction::AddRegion {
            scope: None,
            region,
        });
    }
    let order = |page: &DatSourcesPageState| {
        page.view()
            .policy
            .region_preferences
            .iter()
            .map(|row| row.label.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&page), vec!["Europe", "USA", "Japan"]);

    // Move Europe down one.
    page.apply(DatSourcesPageAction::MoveRegion {
        scope: None,
        index: 0,
        delta: 1,
    });
    assert_eq!(order(&page), vec!["USA", "Europe", "Japan"]);
    assert_eq!(page.view().policy.region_preferences[0].position, 1);

    // Move Japan up to the front.
    page.apply(DatSourcesPageAction::MoveRegion {
        scope: None,
        index: 2,
        delta: -2,
    });
    assert_eq!(order(&page), vec!["Japan", "USA", "Europe"]);

    // Remove USA.
    page.apply(DatSourcesPageAction::RemoveRegion {
        scope: None,
        index: 1,
    });
    assert_eq!(order(&page), vec!["Japan", "Europe"]);
    assert!(page.view().dirty);
}

#[test]
fn language_editor_adds_a_specific_language_multi_and_original() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::Language(LanguageId::En),
    });
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::MultiLanguage,
    });
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::OriginalLanguage,
    });
    let view = page.view();
    let labels: Vec<&str> = view
        .policy
        .language_preferences
        .iter()
        .map(|row| row.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["English", "Multi-language", "Original language"]
    );

    // Adding the same language again is refused (no duplicates).
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::Language(LanguageId::En),
    });
    assert_eq!(page.view().policy.language_preferences.len(), 3);

    page.apply(DatSourcesPageAction::MoveLanguage {
        scope: None,
        index: 2,
        delta: -2,
    });
    let view = page.view();
    let labels: Vec<&str> = view
        .policy
        .language_preferences
        .iter()
        .map(|row| row.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["Original language", "English", "Multi-language"]
    );
}

#[test]
fn revision_and_clone_policies_can_be_chosen() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::SetRevisionPolicy {
        scope: None,
        policy: RevisionPolicy::PreferOriginal,
    });
    page.apply(DatSourcesPageAction::SetClonePolicy {
        scope: None,
        policy: ClonePolicy::PreferParent,
    });
    let view = page.view();
    assert_eq!(view.policy.revision_policy, RevisionPolicy::PreferOriginal);
    assert_eq!(view.policy.clone_policy, ClonePolicy::PreferParent);
    assert_eq!(view.policy.effective.revision, "Prefer original");
    assert_eq!(view.policy.effective.clone, "Prefer parent");
}

#[test]
fn the_effective_policy_summary_shows_the_resolved_values_for_the_scope() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: page.view().rows[0].id.clone(),
        platform: Some("NES".to_string()),
    });
    for region in [RegionId::Europe, RegionId::Usa] {
        page.apply(DatSourcesPageAction::AddRegion {
            scope: None,
            region,
        });
    }
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::Language(LanguageId::En),
    });
    page.apply(DatSourcesPageAction::SetRevisionPolicy {
        scope: None,
        policy: RevisionPolicy::LatestVerified,
    });
    page.apply(DatSourcesPageAction::SetClonePolicy {
        scope: None,
        policy: ClonePolicy::KeepAllVariants,
    });

    let summary = &page.view().policy.effective;
    assert_eq!(summary.platform, "All platforms");
    assert_eq!(summary.region, "Europe, USA");
    assert_eq!(summary.language, "English");
    assert_eq!(summary.revision, "Latest verified revision");
    assert_eq!(summary.clone, "Keep all variants");
    // Source ordering lists the enabled source as consulted 1st.
    assert_eq!(summary.source_ordering.len(), 1);
    assert_eq!(summary.source_ordering[0].consulted_position, 1);
    assert!(
        summary
            .source_of
            .iter()
            .any(|(field, scope)| field == "Region preference" && scope == "Global")
    );
}

#[test]
fn a_platform_override_is_reflected_in_the_summary_with_its_source_of_value() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: page.view().rows[0].id.clone(),
        platform: Some("NES".to_string()),
    });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });

    // Select the NES scope and set a Japan override.
    page.apply(DatSourcesPageAction::SelectPolicyScope {
        scope: Some("NES".to_string()),
    });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: Some("NES".to_string()),
        region: RegionId::Japan,
    });

    let policy = page.view().policy;
    assert_eq!(policy.scope_label, "Nintendo Entertainment System");
    // The authored list for the NES override is Japan alone; the global list
    // is untouched.
    assert_eq!(policy.region_preferences.len(), 1);
    assert_eq!(policy.region_preferences[0].label, "Japan");
    page.apply(DatSourcesPageAction::SelectPolicyScope { scope: None });
    let global = page.view().policy;
    assert_eq!(global.scope_label, "All platforms");
    assert_eq!(global.region_preferences[0].label, "Europe");
    // Switch back to NES for the summary assertions.
    page.apply(DatSourcesPageAction::SelectPolicyScope {
        scope: Some("NES".to_string()),
    });
    let policy = page.view().policy;
    // The summary resolves Japan for NES, and says where it came from.
    assert_eq!(policy.effective.region, "Japan");
    assert!(
        policy
            .effective
            .source_of
            .iter()
            .any(|(field, scope)| field == "Region preference" && scope == "Platform override")
    );
    assert_eq!(policy.effective.platform, "Nintendo Entertainment System");
}

#[test]
fn the_global_scope_is_used_by_default_and_the_scope_selector_is_offered() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::SetPlatform {
        id: page.view().rows[0].id.clone(),
        platform: Some("NES".to_string()),
    });
    let view = page.view();
    assert_eq!(view.policy.scope, None);
    assert_eq!(view.policy.scope_label, "All platforms");
    assert!(
        view.policy
            .scopes_available
            .iter()
            .any(|option| option.id.as_deref() == Some("NES")),
        "the platform a source covers is offered as a scope"
    );
}

#[test]
fn unknown_policy_values_are_surfaced_but_preserved_through_save() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    std::fs::create_dir_all(fixture.config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &fixture.config_path,
        "[policy]\nregion_preferences = [\"europe\", \"moon\"]\nrevision_policy = \"newest_future_policy\"\n",
    )
    .unwrap();

    let mut page = fixture.page();
    let view = page.view();
    assert!(
        view.policy
            .problems
            .iter()
            .any(|problem| problem.contains("moon")),
        "{:?}",
        view.policy.problems
    );
    assert!(
        view.policy
            .problems
            .iter()
            .any(|problem| problem.contains("newest_future_policy")),
        "{:?}",
        view.policy.problems
    );
    // The unknown values are not applied but are preserved on a later save.
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::Save);
    let text = std::fs::read_to_string(&fixture.config_path).unwrap();
    assert!(text.contains("moon"), "{text}");
    assert!(text.contains("newest_future_policy"), "{text}");
}

#[test]
fn the_policy_section_and_summary_render_at_a_narrow_compact_width() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render_at_width(&view, &mut ui_state, 700.0);
    assert!(rendered_text_contains(&output, "DAT matching policy"));
    assert!(rendered_text_contains(&output, "Preferred regions"));
    assert!(rendered_text_contains(&output, "Europe"));
    assert!(rendered_text_contains(&output, "Effective policy"));
    assert!(rendered_text_contains(&output, "Platform: All platforms"));
    assert!(rendered_text_contains(
        &output,
        "ArchiveFS never renames your files."
    ));
}

#[test]
fn the_policy_controls_are_reachable_by_keyboard() {
    let fixture = Fixture::new();
    let dat = fixture.write("collection.dat", LOGIQX);
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });
    let view = page.view();
    let ctx = egui::Context::default();
    let mut focused_anything = false;
    // Focus-traverse far enough to reach the policy section's controls; the
    // page must render and keep focus moving at every step without panicking.
    for _ in 0..40 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 4000.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut ui_state = DatSourcesPageUi::default();
                    let _ = show_dat_sources_page(ui, &view, &mut ui_state);
                });
            },
        );
        if ctx.memory(|memory| memory.focused()).is_some() {
            focused_anything = true;
        }
    }
    assert!(focused_anything, "Tab never focused anything on the page");
}

#[test]
fn an_audit_result_shows_the_policy_preferred_candidate_for_multi_candidate_files() {
    let fixture = Fixture::new();
    let multi = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Multi</name>
        <version>1</version>
        <author>Test</author>
    </header>
    <game name="Game (USA)">
        <rom name="game.bin" size="4" md5="098f6bcd4621d373cade4e832627b4f6"/>
    </game>
    <game name="Game (Europe)">
        <rom name="game.bin" size="4" md5="098f6bcd4621d373cade4e832627b4f6"/>
    </game>
</datafile>"#;
    let dat = fixture.write("multi.dat", multi);
    let roms = fixture.dir("roms");
    std::fs::write(roms.join("game.bin"), SUPER_BIN).unwrap();

    let mut page = fixture.page_with_library(vec![roms]);
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });
    page.apply(DatSourcesPageAction::Audit {
        id: page.view().rows[0].id.clone(),
        scan_root: fixture.root.join("roms"),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let audit = view.audit.as_ref().expect("an audit result");
    assert_eq!(
        audit
            .categories
            .iter()
            .find(|category| category.label == "Exact (multiple)")
            .map(|category| category.count),
        Some(1)
    );
    let policy = audit.policy.as_ref().expect("the audit carried a policy");
    assert_eq!(policy.notes.len(), 1);
    let note = &policy.notes[0];
    assert!(note.decided);
    assert_eq!(note.winner.as_deref(), Some("Game (Europe) (game.bin)"));
    assert!(
        note.explanations
            .iter()
            .any(|line| line.contains("preferred region matched"))
    );
    assert!(!note.ambiguous);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Policy preference"));
    assert!(rendered_text_contains(&output, "Game (Europe)"));
    assert!(rendered_text_contains(
        &output,
        "Preferred: Game (Europe) (game.bin)"
    ));
}

// ---------------------------------------------------------------------------
// Rename planning section
// ---------------------------------------------------------------------------

use archivefs_core::dat::rename_plan::{
    ProposalState, RenamePlan, RenamePlanCounts, RenameProposal, ReviewDecision, SourceObjectKind,
};

fn plan_proposal(
    source: &str,
    current: &str,
    proposed: Option<&str>,
    state: ProposalState,
) -> RenameProposal {
    RenameProposal {
        source_path: PathBuf::from(source),
        current_basename: current.to_string(),
        proposed_basename: proposed.map(str::to_string),
        platform: None,
        platform_display: None,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        game_name: Some("Game".to_string()),
        rom_name: proposed.map(str::to_string),
        verdict_label: "Exact".to_string(),
        match_confident: true,
        explanations: vec!["preferred region matched (Europe)".to_string()],
        state,
        object_kind: SourceObjectKind::RegularFile,
        ambiguity_reason: None,
        collision: None,
        blockers: Vec::new(),
        extension_status: None,
        sanitisation_notes: Vec::new(),
        actionable: state == ProposalState::Suggested,
    }
}

fn page_with_plan(proposals: Vec<RenameProposal>) -> (Fixture, DatSourcesPageState) {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    let counts = RenamePlanCounts::from_proposals(&proposals);
    let verified_total = proposals.len();
    page.rename_plan = Some(RenamePlan {
        generation: 1,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        scan_root: "/tmp/roms".to_string(),
        platform: None,
        platform_display: None,
        proposals,
        counts,
        audited_total: 2,
        verified_total,
        truncated: false,
    });
    (fixture, page)
}

#[test]
fn the_planning_only_warning_is_prominent() {
    let proposals = vec![plan_proposal(
        "/tmp/roms/game.bin",
        "game.bin",
        Some("Game (Europe).bin"),
        ProposalState::Suggested,
    )];
    let (_fixture, page) = page_with_plan(proposals);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Planning only"));
    assert!(
        rendered_text_contains(&output, "ArchiveFS will not rename any files"),
        "the read-only promise must be stated plainly"
    );
}

#[test]
fn no_apply_rename_or_commit_control_exists() {
    let proposals = vec![plan_proposal(
        "/tmp/roms/game.bin",
        "game.bin",
        Some("Game (Europe).bin"),
        ProposalState::Suggested,
    )];
    let (_fixture, page) = page_with_plan(proposals);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    for forbidden in [
        "Apply",
        "Execute",
        "Commit",
        "Move",
        "Delete",
        "Fix automatically",
    ] {
        assert!(
            !rendered_text_contains(&output, forbidden),
            "the plan section must not offer a {forbidden} control"
        );
    }
    // The only controls present are review decisions and copy.
    assert!(rendered_text_contains(&output, "Accept"));
    assert!(rendered_text_contains(&output, "Ignore"));
    assert!(rendered_text_contains(&output, "Copy name"));
}

#[test]
fn the_plan_filters_select_which_rows_are_drawn() {
    let proposals = vec![
        plan_proposal(
            "/tmp/roms/a.bin",
            "a.bin",
            Some("Game (Europe).bin"),
            ProposalState::Suggested,
        ),
        plan_proposal(
            "/tmp/roms/b.bin",
            "b.bin",
            Some("Other.bin"),
            ProposalState::Conflict,
        ),
        plan_proposal(
            "/tmp/roms/c.bin",
            "Game.bin",
            Some("Game.bin"),
            ProposalState::AlreadyCanonical,
        ),
    ];
    let (_fixture, page) = page_with_plan(proposals);
    let view = page.view();
    assert_eq!(view.rename_plan.as_ref().unwrap().rows.len(), 3);

    let mut ui_state = DatSourcesPageUi {
        plan_filter: RenamePlanFilter::Suggested,
        ..Default::default()
    };
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "a.bin"));
    assert!(
        !rendered_text_contains(&output, "b.bin"),
        "conflicted row must be filtered out"
    );
    assert!(
        !rendered_text_contains(&output, "Game.bin"),
        "already-canonical row must be filtered out"
    );

    ui_state.plan_filter = RenamePlanFilter::Conflicts;
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "b.bin"));
    assert!(!rendered_text_contains(&output, "a.bin"));
}

#[test]
fn review_decisions_never_touch_files() {
    let fixture = Fixture::new();
    let roms = fixture.dir("roms");
    let file = roms.join("game.bin");
    std::fs::write(&file, b"content").unwrap();
    let mut page = fixture.page();
    page.rename_plan = Some(RenamePlan {
        generation: 1,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        scan_root: roms.to_string_lossy().into_owned(),
        platform: None,
        platform_display: None,
        proposals: vec![plan_proposal(
            file.to_string_lossy().as_ref(),
            "game.bin",
            Some("Game.bin"),
            ProposalState::Suggested,
        )],
        counts: RenamePlanCounts::default(),
        audited_total: 1,
        verified_total: 1,
        truncated: false,
    });

    let before = snapshot(&roms);
    page.apply(DatSourcesPageAction::SetReviewDecision {
        path: file.to_string_lossy().into_owned(),
        decision: Some(ReviewDecision::AcceptedForReview),
    });
    page.apply(DatSourcesPageAction::SetReviewDecision {
        path: file.to_string_lossy().into_owned(),
        decision: Some(ReviewDecision::Ignored),
    });
    let after = snapshot(&roms);
    assert_eq!(before, after, "a review decision must not change any file");
    assert_eq!(
        page.view().rename_plan.as_ref().unwrap().rows[0].decision,
        Some(ReviewDecision::Ignored)
    );
}

#[test]
fn clearing_review_decisions_leaves_source_files_untouched() {
    let fixture = Fixture::new();
    let roms = fixture.dir("roms");
    let file = roms.join("game.bin");
    std::fs::write(&file, b"content").unwrap();
    let mut page = fixture.page();
    page.rename_plan = Some(RenamePlan {
        generation: 1,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        scan_root: roms.to_string_lossy().into_owned(),
        platform: None,
        platform_display: None,
        proposals: vec![plan_proposal(
            file.to_string_lossy().as_ref(),
            "game.bin",
            Some("Game.bin"),
            ProposalState::Suggested,
        )],
        counts: RenamePlanCounts::default(),
        audited_total: 1,
        verified_total: 1,
        truncated: false,
    });

    page.apply(DatSourcesPageAction::SetReviewDecision {
        path: file.to_string_lossy().into_owned(),
        decision: Some(ReviewDecision::AcceptedForReview),
    });
    assert_eq!(
        page.view().rename_plan.as_ref().unwrap().rows[0].decision,
        Some(ReviewDecision::AcceptedForReview)
    );

    let before = snapshot(&roms);
    page.apply(DatSourcesPageAction::ClearReviewDecisions);
    let after = snapshot(&roms);
    assert_eq!(before, after, "clearing decisions must not change any file");
    assert_eq!(
        page.view().rename_plan.as_ref().unwrap().rows[0].decision,
        None,
        "the decision is cleared"
    );
}

#[test]
fn the_plan_section_renders_at_a_narrow_compact_width() {
    let proposals = vec![plan_proposal(
        "/tmp/roms/game.bin",
        "game.bin",
        Some("Game (Europe).bin"),
        ProposalState::Suggested,
    )];
    let (_fixture, page) = page_with_plan(proposals);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render_at_width(&view, &mut ui_state, 700.0);
    assert!(rendered_text_contains(&output, "Rename planning"));
    assert!(rendered_text_contains(&output, "Planning only"));
    assert!(rendered_text_contains(&output, "game.bin"));
    assert!(rendered_text_contains(&output, "Game (Europe).bin"));
}

#[test]
fn the_plan_controls_are_reachable_by_keyboard() {
    let proposals = vec![plan_proposal(
        "/tmp/roms/game.bin",
        "game.bin",
        Some("Game (Europe).bin"),
        ProposalState::Suggested,
    )];
    let (_fixture, page) = page_with_plan(proposals);
    let view = page.view();
    let ctx = egui::Context::default();
    let mut focused_anything = false;
    for _ in 0..40 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 4000.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut ui_state = DatSourcesPageUi::default();
                    let _ = show_dat_sources_page(ui, &view, &mut ui_state);
                });
            },
        );
        if ctx.memory(|memory| memory.focused()).is_some() {
            focused_anything = true;
        }
    }
    assert!(focused_anything, "Tab never focused anything on the page");
}

#[test]
fn an_audit_builds_a_read_only_rename_plan_and_changes_nothing() {
    // End-to-end: audit a folder whose file matches two catalogue entries
    // with a region preference; the plan must propose the preferred name and
    // the scanned tree must be byte-for-byte unchanged (paths, contents and
    // file identities).
    let fixture = Fixture::new();
    let multi = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Multi</name>
        <version>1</version>
        <author>Test</author>
    </header>
    <game name="Game (USA)">
        <rom name="Game (USA).bin" size="4" md5="098f6bcd4621d373cade4e832627b4f6"/>
    </game>
    <game name="Game (Europe)">
        <rom name="Game (Europe).bin" size="4" md5="098f6bcd4621d373cade4e832627b4f6"/>
    </game>
</datafile>"#;
    let dat = fixture.write("multi.dat", multi);
    let roms = fixture.dir("roms");
    std::fs::write(roms.join("game.bin"), SUPER_BIN).unwrap();

    let mut page = fixture.page_with_library(vec![roms.clone()]);
    page.apply(DatSourcesPageAction::AddFile { path: dat });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });

    let before = snapshot(&fixture.root);
    page.apply(DatSourcesPageAction::Audit {
        id: page.view().rows[0].id.clone(),
        scan_root: roms.clone(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let plan = view
        .rename_plan
        .as_ref()
        .expect("an audit with a policy produces a rename plan");
    assert_eq!(plan.counts.suggested, 1, "{:?}", plan.counts);
    assert_eq!(
        plan.rows[0].proposed_basename.as_deref(),
        Some("Game (Europe).bin"),
        "the preferred region's name is proposed"
    );
    assert!(
        plan.rows[0]
            .explanations
            .iter()
            .any(|e| e.contains("preferred region matched"))
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "planning an audit must not change any path, file identity or content"
    );
}

// ---------------------------------------------------------------------------
// Rename apply flow
// ---------------------------------------------------------------------------

/// Builds a page whose plan references real files in a trusted folder, with a
/// temporary journal directory - so an apply actually renames on a real worker
/// and writes its journal into the temp dir, never the real home.
fn page_with_apply_plan(count: usize) -> (Fixture, PathBuf, DatSourcesPageState) {
    let fixture = Fixture::new();
    let roms = fixture.dir("roms");
    let mut proposals = Vec::new();
    for index in 0..count {
        let name = format!("game{index}.bin");
        let path = roms.join(&name);
        std::fs::write(&path, b"fixture contents").unwrap();
        proposals.push(plan_proposal(
            path.to_str().unwrap(),
            &name,
            Some(&format!("Game {index} (Europe).bin")),
            ProposalState::Suggested,
        ));
    }
    let journal = fixture.dir("journal");
    let counts = RenamePlanCounts::from_proposals(&proposals);
    let mut page = DatSourcesPageState::load_with_transaction_dir(
        fixture.config_path.clone(),
        Vec::new(),
        TrustedRoots::from_paths([&roms]),
        journal.clone(),
    );
    page.rename_plan = Some(RenamePlan {
        generation: 1,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        scan_root: roms.to_string_lossy().into_owned(),
        platform: None,
        platform_display: None,
        proposals,
        counts,
        audited_total: counts.total,
        verified_total: counts.total,
        truncated: false,
    });
    (fixture, roms, page)
}

fn approve_all(page: &mut DatSourcesPageState) {
    let plan = page.rename_plan.clone().unwrap();
    for proposal in &plan.proposals {
        page.apply(DatSourcesPageAction::SetReviewDecision {
            path: proposal.source_path.to_string_lossy().into_owned(),
            decision: Some(ReviewDecision::AcceptedForReview),
        });
    }
}

#[test]
fn apply_is_hidden_without_approved_suggested_proposals() {
    let (_fixture, _roms, page) = page_with_apply_plan(1);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        !rendered_text_contains(&output, "Apply approved renames"),
        "with nothing approved there is nothing to apply"
    );
}

#[test]
fn apply_is_hidden_for_unsafe_proposals() {
    let (_fixture, _roms, mut page) = page_with_apply_plan(1);
    // Turn the only proposal into an ambiguous one (not applicable).
    let plan = page.rename_plan.as_mut().unwrap();
    plan.proposals[0].state = ProposalState::Ambiguous;
    plan.proposals[0].actionable = false;
    approve_all(&mut page);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        !rendered_text_contains(&output, "Apply approved renames"),
        "ambiguous proposals must never be offered for apply"
    );
}

#[test]
fn begin_review_shows_the_read_only_old_to_new_pairs() {
    let (_fixture, roms, mut page) = page_with_apply_plan(2);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    let view = page.view();
    let review = view
        .rename_apply
        .review
        .as_ref()
        .expect("a review is shown");
    assert_eq!(review.rows.len(), 2);
    assert_eq!(review.rows[0].current_basename, "game0.bin");
    assert_eq!(review.rows[0].proposed_basename, "Game 0 (Europe).bin");
    assert_eq!(review.trusted_root.as_deref(), Some(roms.to_str().unwrap()));
    // Nothing has been renamed yet.
    assert!(roms.join("game0.bin").exists());
    assert!(!roms.join("Game 0 (Europe).bin").exists());
}

#[test]
fn a_large_batch_requires_the_typed_confirmation() {
    let (_fixture, roms, mut page) = page_with_apply_plan(9);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    let view = page.view();
    let review = view.rename_apply.review.as_ref().expect("review");
    assert_eq!(
        review.required_phrase.as_deref(),
        Some("RENAME 9 FILES"),
        "a batch larger than the threshold needs a typed phrase"
    );

    // A wrong phrase is refused and nothing happens.
    page.apply(DatSourcesPageAction::ConfirmApply {
        typed: "RENAME 8 FILES".to_string(),
    });
    assert!(page.view().rename_apply.apply_error.is_some());
    assert!(roms.join("game0.bin").exists());

    // The correct phrase applies.
    page.apply(DatSourcesPageAction::ConfirmApply {
        typed: "RENAME 9 FILES".to_string(),
    });
    run_to_completion(&mut page);
    let view = page.view();
    let outcome = view.rename_apply.outcome.as_ref().expect("an outcome");
    assert_eq!(outcome.applied, 9);
    assert!(roms.join("Game 0 (Europe).bin").exists());
}

#[test]
fn an_approved_apply_renames_and_the_outcome_is_visible() {
    let (_fixture, roms, mut page) = page_with_apply_plan(2);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    page.apply(DatSourcesPageAction::ConfirmApply {
        typed: String::new(),
    });
    run_to_completion(&mut page);

    let view = page.view();
    let apply = &view.rename_apply;
    let outcome = apply.outcome.as_ref().expect("an apply outcome");
    assert_eq!(outcome.applied, 2);
    assert_eq!(outcome.failed, 0);
    assert!(!roms.join("game0.bin").exists());
    assert!(roms.join("Game 0 (Europe).bin").exists());
    assert_eq!(
        std::fs::read(roms.join("Game 1 (Europe).bin")).unwrap(),
        b"fixture contents"
    );
    // The journal records the applied state.
    assert!(apply.journal_dir.contains("journal"));
}

#[test]
fn rollback_from_the_page_restores_original_files() {
    let (_fixture, roms, mut page) = page_with_apply_plan(1);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    page.apply(DatSourcesPageAction::ConfirmApply {
        typed: String::new(),
    });
    run_to_completion(&mut page);
    assert!(roms.join("Game 0 (Europe).bin").exists());

    let id = page
        .view()
        .rename_apply
        .outcome
        .as_ref()
        .unwrap()
        .transaction_id
        .clone();
    page.apply(DatSourcesPageAction::RollbackTransaction { id });
    run_to_completion(&mut page);
    let view = page.view();
    assert_eq!(
        view.rename_apply.rollback_result.as_ref().map(|r| r.label),
        Some("Fully rolled back")
    );
    assert!(roms.join("game0.bin").exists());
    assert!(!roms.join("Game 0 (Europe).bin").exists());
    assert_eq!(
        std::fs::read(roms.join("game0.bin")).unwrap(),
        b"fixture contents"
    );
}

#[test]
fn an_interrupted_transaction_is_offered_for_recovery_and_never_auto_resumes() {
    let (_fixture, _roms, mut page) = page_with_apply_plan(0);
    // Write an interrupted journal directly into the page's temp journal dir.
    let tx = archivefs_core::dat::rename_apply::RenameTransaction {
        transaction_id: "interrupted-test".to_string(),
        plan_generation: 1,
        created_at_unix: 1,
        source_scan_root: "/tmp/roms".to_string(),
        state: archivefs_core::dat::rename_apply::TransactionState::Applying,
        entries: vec![archivefs_core::dat::rename_apply::TransactionEntry {
            source_path: PathBuf::from("/tmp/roms/a.bin"),
            destination_path: PathBuf::from("/tmp/roms/b.bin"),
            original_basename: "a.bin".to_string(),
            proposed_basename: "b.bin".to_string(),
            identity: archivefs_core::dat::rename_apply::ObjectIdentity {
                size_bytes: 1,
                modified_unix: 1,
                kind: archivefs_core::dat::rename_apply::ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 1,
                #[cfg(unix)]
                dev: 1,
            },
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: archivefs_core::dat::rename_apply::EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }],
        created_directories: Vec::new(),
        unknown: Default::default(),
    };
    archivefs_core::dat::rename_apply::write_journal(
        Path::new(&page.view().rename_apply.journal_dir),
        &tx,
    )
    .unwrap();
    page.refresh_recovery();

    let view = page.view();
    assert_eq!(view.rename_apply.recovery.len(), 1);
    assert!(
        view.rename_apply.recovery[0]
            .transaction_id
            .contains("interrupted")
    );
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(
        &output,
        "An interrupted rename transaction was found"
    ));
    assert!(rendered_text_contains(&output, "Leave untouched"));
    // There is no auto-resume anywhere in the UI.
    assert!(!rendered_text_contains(&output, "Resume renames"));
}

#[test]
fn the_apply_section_renders_at_a_narrow_compact_width() {
    let (_fixture, _roms, mut page) = page_with_apply_plan(2);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render_at_width(&view, &mut ui_state, 700.0);
    assert!(rendered_text_contains(&output, "Review approved renames"));
    assert!(rendered_text_contains(&output, "Apply approved renames"));
    assert!(rendered_text_contains(&output, "game0.bin"));
}

#[test]
fn the_apply_controls_are_reachable_by_keyboard() {
    let (_fixture, _roms, mut page) = page_with_apply_plan(1);
    approve_all(&mut page);
    page.apply(DatSourcesPageAction::BeginApplyReview);
    let view = page.view();
    let ctx = egui::Context::default();
    let mut focused_anything = false;
    for _ in 0..40 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 4000.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut ui_state = DatSourcesPageUi::default();
                    let _ = show_dat_sources_page(ui, &view, &mut ui_state);
                });
            },
        );
        if ctx.memory(|memory| memory.focused()).is_some() {
            focused_anything = true;
        }
    }
    assert!(focused_anything, "Tab never focused anything on the page");
}

// ---------------------------------------------------------------------------
// Beta UX: safety promise, Any-preference, grouped diagnostics
// ---------------------------------------------------------------------------

#[test]
fn the_safe_promise_is_rendered_on_the_dat_sources_page() {
    let fixture = Fixture::new();
    let page = fixture.page();
    let view = page.view();
    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        rendered_text_contains(&output, SAFE_PROMISE),
        "the simple safety promise must be visible"
    );
    assert!(
        SAFE_PROMISE.contains("Your files won't be renamed unless you approve it."),
        "the promise uses the exact friendly wording"
    );
}

#[test]
fn any_region_clears_the_region_preference() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Europe,
    });
    page.apply(DatSourcesPageAction::AddRegion {
        scope: None,
        region: RegionId::Japan,
    });
    assert_eq!(page.view().policy.region_preferences.len(), 2);
    page.apply(DatSourcesPageAction::ClearRegion { scope: None });
    assert!(
        page.view().policy.region_preferences.is_empty(),
        "Any region clears the preference ordering"
    );
    assert_eq!(page.view().policy.effective.region, "Any");
}

#[test]
fn any_language_clears_the_language_preference() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    page.apply(DatSourcesPageAction::AddLanguage {
        scope: None,
        preference: LanguagePreference::Language(LanguageId::En),
    });
    assert_eq!(page.view().policy.language_preferences.len(), 1);
    page.apply(DatSourcesPageAction::ClearLanguage { scope: None });
    assert!(
        page.view().policy.language_preferences.is_empty(),
        "Any language clears the preference ordering"
    );
    assert_eq!(page.view().policy.effective.language, "Any");
}

#[test]
fn the_effective_policy_renders_any_not_none_all_equal() {
    assert_eq!(super::render_preference_list(Vec::new()), "Any");
    assert_eq!(
        super::render_preference_list(vec!["Europe".to_string()]),
        "Europe"
    );
}

#[test]
fn repeated_symlink_diagnostics_group_with_an_exact_count_and_bounded_examples() {
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let mut files = Vec::new();
            for index in 0..25 {
                files.push((
                    format!("disc{index}.bin"),
                    "symlink refused: it resolves outside every configured source root".to_string(),
                ));
            }
            super::show_unhashed_groups(ui, &files);
        });
    });
    assert!(
        rendered_text_contains(&output, "25 symlinks could not be hashed"),
        "the group must carry the exact count"
    );
    assert!(
        rendered_text_contains(&output, "Show all 25"),
        "the raw findings remain available behind an expansion"
    );
    // Examples are bounded to the first 10; the rest are only in "Show all".
    let example_count = rendered_text_count(&output, "disc");
    assert!(
        example_count <= 10,
        "examples must be bounded (found {example_count})"
    );
}
