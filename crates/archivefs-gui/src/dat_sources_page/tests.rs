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
use archivefs_core::dat::model::{DatEcosystem, DatFormat};
use archivefs_core::dat::sources::{
    DatFileOutcome, DatFileReport, DatHealthState, DatSourceKind, DatSourceRegistry,
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
        DatSourcesPageState::load(self.config_path.clone(), Vec::new(), TrustedRoots::none())
    }

    fn page_with_library(&self, folders: Vec<PathBuf>) -> DatSourcesPageState {
        DatSourcesPageState::load(self.config_path.clone(), folders, TrustedRoots::none())
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

/// A page holding one folder source plus a stored validation report built by
/// the test, so warning presentation can be driven without depending on parser
/// wording.
fn page_with_report(
    per_file_warnings: Vec<Vec<String>>,
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
    let files: Vec<DatFileReport> = per_file_warnings
        .iter()
        .enumerate()
        .map(|(index, warnings)| DatFileReport {
            path: format!("{}/warn-{index}.dat", folder.display()),
            file_name: format!("warn-{index}.dat"),
            outcome: DatFileOutcome::Parsed {
                format: DatFormat::Logiqx,
                ecosystem: DatEcosystem::GenericLogiqx,
                name: Some("Test Catalogue".to_string()),
                version: Some("2026-01-01".to_string()),
                entry_count: 1,
                rom_count: 1,
                warnings: warnings.clone(),
            },
        })
        .collect();
    let report = DatValidationReport {
        source_id: id.clone(),
        path: folder.to_string_lossy().into_owned(),
        kind: "DAT folder",
        state: DatHealthState::ValidWithWarnings,
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

#[test]
fn warnings_render_count_summary_and_expandable_details() {
    let warnings = vec![
        "The header version differs from the file's name".to_string(),
        "A ROM entry has no SHA-1 checksum; only CRC32 was compared".to_string(),
    ];
    let (_fixture, page) = page_with_report(vec![warnings.clone()], false, None);
    let view = page.view();
    assert_eq!(view.rows[0].warnings.len(), 2);
    assert_eq!(view.rows[0].health_state, DatHealthState::ValidWithWarnings);

    let mut ui_state = DatSourcesPageUi::default();
    let collapsed = render(&view, &mut ui_state);
    assert!(
        rendered_text_contains(&collapsed, "2 warnings"),
        "the count must sit on the card"
    );
    assert!(
        rendered_text_contains(&collapsed, "View warning details"),
        "an expandable control must be offered"
    );
    assert!(
        !rendered_text_contains(&collapsed, &warnings[1]),
        "the details list must stay hidden until the user expands it"
    );

    ui_state.open_warnings = Some(view.rows[0].id.clone());
    let expanded = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&expanded, "Hide warning details"));
    for warning in &warnings {
        assert!(
            rendered_text_contains(&expanded, warning),
            "the full original text must appear when expanded"
        );
    }
}

#[test]
fn zero_warnings_show_no_warning_details_control() {
    let (_fixture, page) = page_with_report(vec![Vec::new()], false, None);
    let view = page.view();
    assert!(view.rows[0].warnings.is_empty());

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(
        !rendered_text_contains(&output, "View warning details"),
        "no warnings means no details control"
    );
}

#[test]
fn a_safety_limit_stop_is_labelled_incomplete_and_counts_are_exact() {
    // Both numbers genuinely known: the read count and the folder's real total.
    let (_fixture, page) = page_with_report(vec![vec!["w".to_string()]], true, Some(2024));
    let row = &page.view().rows[0];
    assert!(row.incomplete_load);
    assert_eq!(row.dat_files_read, Some(1));
    assert_eq!(row.dat_files_total, Some(2024));
    assert_eq!(
        row.incomplete_load_line().as_deref(),
        Some("1 of 2024 DAT files read")
    );

    // An unknown total never invents one: the safety limit is named instead.
    let (_fixture, page) = page_with_report(vec![vec!["w".to_string()]], true, None);
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
    let (_fixture, page) = page_with_report(vec![vec!["w".to_string()]], true, Some(2024));
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

    let (_fixture, page) = page_with_report(vec![vec!["w".to_string()]], true, None);
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
        vec!["first-a".to_string(), "second-a".to_string()],
        vec!["first-b".to_string(), "second-b".to_string()],
    ];
    let (_fixture, page) = page_with_report(per_file, false, None);
    let row = &page.view().rows[0];
    assert_eq!(
        row.warnings,
        vec!["first-a", "second-a", "first-b", "second-b"],
        "warnings must come in file order, files in name order, never in read_dir order"
    );
}

#[test]
fn the_history_and_logs_reference_is_only_drawn_when_details_are_recorded_there() {
    let (_fixture, page) = page_with_report(vec![vec!["w".to_string()]], false, None);
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
    // nested per-file list. The count and an expandable "View warning details"
    // control now sit on the card itself.
    let warnings = vec![
        "A ROM entry has no SHA-1 checksum; only CRC32 was compared".to_string(),
        "The header declares a version that differs from the filename".to_string(),
    ];
    let (_fixture, page) = page_with_report(vec![warnings.clone()], false, None);
    let view = page.view();
    assert_eq!(view.rows[0].health_state, DatHealthState::ValidWithWarnings);

    let mut ui_state = DatSourcesPageUi::default();
    let output = render(&view, &mut ui_state);
    assert!(rendered_text_contains(&output, "Valid, with warnings"));
    assert!(
        rendered_text_contains(&output, "2 warnings"),
        "the count must be visible without any disclosure click"
    );
    assert!(
        rendered_text_contains(&output, "View warning details"),
        "the expandable control must be visible without opening Inspect"
    );
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
        .send(JobMessage::Audited(Box::new(minimal_outcome())))
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
