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
use std::time::{Duration, Instant};

use archivefs_core::dat::sources::{
    DatHealthState, DatSourceRegistry, load_dat_sources_config_from,
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

    page.apply(DatSourcesPageAction::CancelJob);
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
