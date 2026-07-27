//! Behavioural tests for Dolphin candidate matching, selection, staging,
//! and preview.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::patch_manager::dolphin_local::{
    DolphinProfileDiscoveryRoots, discover_dolphin_profiles, inspect_dolphin_profile,
};

const REAL_WORLD_INI: &str = "[Core]\n\
FastDiscSpeed = True\n\
[Gecko]\n\
$Infinite Bells [Nayr]\n\
28134C58 00000001\n\
20C9F0D4 00060000\n\
*Gives you lots of bells\n\
$Instant Growth [Nayr]\n\
C913CEF5 00000000\n\
08002FC2 00000001\n\
$Broken Entry\n\
";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let unique = format!(
            "archivefs-dolphin-install-plan-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
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
        fs::write(&path, contents).expect("write fixture file");
        path
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("fixture dir");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A real, eligible Dolphin profile with one GameSettings file already in
/// place, matching a real audited installation's shape.
fn profile_with_ini(fixture: &Fixture, file_name: &str, contents: &str) -> (PathBuf, PathBuf) {
    let configuration_path = fixture.dir("dolphin");
    fixture.write("dolphin/Dolphin.ini", "[Core]\n");
    let ini_path = fixture.write(&format!("dolphin/GameSettings/{file_name}"), contents);
    (configuration_path, ini_path)
}

fn inventory_for(configuration_path: &Path) -> DolphinGameIniInventory {
    let mut roots = DolphinProfileDiscoveryRoots {
        home: configuration_path.parent().unwrap().to_path_buf(),
        xdg_config_home: configuration_path.parent().unwrap().to_path_buf(),
        xdg_data_home: configuration_path.parent().unwrap().to_path_buf(),
        flatpak_system_root: configuration_path.parent().unwrap().to_path_buf(),
        explicit_configuration_roots: Vec::new(),
    };
    roots
        .explicit_configuration_roots
        .push(configuration_path.to_path_buf());
    let discovery = discover_dolphin_profiles(&roots).expect("discovery");
    let profile = discovery
        .profiles
        .into_iter()
        .find(|profile| profile.configuration_path == configuration_path)
        .expect("profile discovered");
    inspect_dolphin_profile(&profile).expect("inventory")
}

// -----------------------------------------------------------------------
// Candidate matching
// -----------------------------------------------------------------------

#[test]
fn an_exact_game_id_and_revision_produces_an_installable_candidate() {
    let fixture = Fixture::new("exact");
    let (configuration_path, ini_path) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let inventory = inventory_for(&configuration_path);

    let outcome = build_dolphin_candidate(&inventory, Some("E"), Some("GAFE01"), Some(0));
    let candidate = outcome.candidate.expect("installable candidate");
    assert!(candidate.installable);
    assert_eq!(candidate.game_id, "GAFE01");
    assert_eq!(candidate.path, ini_path);
    assert_eq!(candidate.cheat_count, 3);
    assert!(
        candidate
            .evidence
            .iter()
            .any(|item| item.label == "game_id"),
    );
    assert!(outcome.blocked_reason.is_none());
}

#[test]
fn a_wrong_platform_or_missing_identity_never_produces_a_candidate() {
    let fixture = Fixture::new("no-identity");
    let (configuration_path, _) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let inventory = inventory_for(&configuration_path);

    let outcome = build_dolphin_candidate(&inventory, None, None, None);
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::NoVerifiedGameIdAvailable)
    );
}

#[test]
fn no_matching_ini_produces_no_candidate() {
    let fixture = Fixture::new("no-match");
    let (configuration_path, _) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let inventory = inventory_for(&configuration_path);

    let outcome = build_dolphin_candidate(&inventory, None, Some("GALE01"), Some(0));
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::NoMatchingIniFound)
    );
}

#[test]
fn a_revision_mismatch_blocks_the_candidate_with_an_exact_reason() {
    let fixture = Fixture::new("revision-mismatch");
    let (configuration_path, _) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let inventory = inventory_for(&configuration_path);

    let outcome = build_dolphin_candidate(&inventory, None, Some("GAFE01"), Some(3));
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::RevisionMismatch)
    );
    assert!(
        outcome
            .blocked_reason
            .unwrap()
            .message()
            .contains("revision")
    );
}

#[test]
fn multiple_matching_files_are_ambiguous_and_never_resolved_silently() {
    let fixture = Fixture::new("ambiguous");
    let configuration_path = fixture.dir("dolphin");
    fixture.write("dolphin/Dolphin.ini", "[Core]\n");
    fixture.write("dolphin/GameSettings/GAFE01.ini", REAL_WORLD_INI);
    fixture.write("dolphin/GameSettings/GAFE01r0.ini", REAL_WORLD_INI);
    let inventory = inventory_for(&configuration_path);

    let outcome = build_dolphin_candidate(&inventory, None, Some("GAFE01"), Some(0));
    assert!(outcome.candidate.is_none());
    assert_eq!(
        outcome.blocked_reason,
        Some(DolphinCandidateBlockedReason::MultipleIniFilesForGame)
    );
    assert_eq!(outcome.conflicting_paths.len(), 2);
}

// -----------------------------------------------------------------------
// Loading
// -----------------------------------------------------------------------

#[test]
fn loading_the_matched_file_parses_its_real_codes() {
    let fixture = Fixture::new("load");
    let (_, ini_path) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let loaded = load_dolphin_ini(&ini_path).expect("loads");
    assert_eq!(loaded.document.gecko_codes.len(), 3);
    assert_eq!(loaded.digest.len(), 64);
}

#[test]
fn loading_never_modifies_the_source_file() {
    let fixture = Fixture::new("immutable");
    let (_, ini_path) = profile_with_ini(&fixture, "GAFE01.ini", REAL_WORLD_INI);
    let before = fs::read(&ini_path).expect("read");
    let _ = load_dolphin_ini(&ini_path).expect("loads");
    assert_eq!(fs::read(&ini_path).expect("read"), before);
}

#[cfg(unix)]
#[test]
fn a_symlinked_matched_file_is_never_followed() {
    let fixture = Fixture::new("symlink");
    let outside = fixture.write("outside.ini", REAL_WORLD_INI);
    let link = fixture.path("linked.ini");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    let error = load_dolphin_ini(&link).expect_err("symlink rejected");
    assert_eq!(error.kind, DolphinInstallPlanErrorKind::CandidatePathUnsafe);
}

// -----------------------------------------------------------------------
// Selection
// -----------------------------------------------------------------------

fn document() -> DolphinIniDocument {
    parse_dolphin_ini(REAL_WORLD_INI)
}

#[test]
fn selection_preserves_the_files_own_already_enabled_codes() {
    let mut text = REAL_WORLD_INI.to_string();
    text.push_str("[Gecko_Enabled]\n$Infinite Bells [Nayr]\n");
    let document = parse_dolphin_ini(&text);
    let selection = DolphinCodeSelection::from_document(&document);
    assert!(selection.entries[0].already_enabled);
    assert!(
        selection.entries[0].selected,
        "an already-enabled code starts selected, not silently reset"
    );
    assert!(!selection.entries[1].selected);
}

#[test]
fn an_unsafe_entry_can_never_be_selected() {
    let document = document();
    let mut selection = DolphinCodeSelection::from_document(&document);
    let broken = &selection.entries[2];
    assert!(!broken.selectable, "the broken entry has no code lines");
    assert!(!selection.set_selected(2, true), "the toggle is refused");
    assert_eq!(selection.selected_count(), 0);
}

#[test]
fn select_all_and_clear_all_only_touch_selectable_entries() {
    let document = document();
    let mut selection = DolphinCodeSelection::from_document(&document);
    selection.select_all();
    assert_eq!(selection.selected_count(), 2);
    assert_eq!(selection.selectable_count(), 2);
    selection.clear_all();
    assert_eq!(selection.selected_count(), 0);
}

#[test]
fn resolving_an_empty_selection_blocks_apply() {
    let document = document();
    let selection = DolphinCodeSelection::from_document(&document);
    assert!(!selection.can_apply());
    let error = selection.resolve_names(&document).expect_err("blocked");
    assert_eq!(error.kind, DolphinInstallPlanErrorKind::NoSelectedCodes);
}

#[test]
fn resolving_returns_selected_names_in_catalogue_order() {
    let document = document();
    let mut selection = DolphinCodeSelection::from_document(&document);
    assert!(selection.set_selected(1, true));
    assert!(selection.set_selected(0, true));
    let names = selection.resolve_names(&document).expect("resolves");
    assert_eq!(
        names,
        vec![
            "Infinite Bells [Nayr]".to_string(),
            "Instant Growth [Nayr]".to_string()
        ]
    );
}

// -----------------------------------------------------------------------
// Staging and preview
// -----------------------------------------------------------------------

#[test]
fn staging_preserves_unrelated_sections_and_writes_only_the_selected_enabled_list() {
    let fixture = Fixture::new("stage");
    let document = document();
    let staged = stage_dolphin_ini(
        &fixture.path("staging"),
        "GAFE01.ini",
        &document,
        &["Infinite Bells [Nayr]".to_string()],
    )
    .expect("stages");
    assert!(staged.contents.contains("[Core]\nFastDiscSpeed = True\n"));
    // The [Gecko] body section - every code, whether selected or not -
    // is preserved exactly, since it holds the game's own trusted codes,
    // not just the ones this install enables.
    assert!(
        staged
            .contents
            .contains("[Gecko]\n$Infinite Bells [Nayr]\n")
    );
    assert!(staged.contents.contains("$Instant Growth [Nayr]\n"));
    // Only [Gecko_Enabled] reflects the selection.
    assert!(
        staged
            .contents
            .contains("[Gecko_Enabled]\n$Infinite Bells [Nayr]\n")
    );
    let enabled_index = staged.contents.find("[Gecko_Enabled]").unwrap();
    assert!(!staged.contents[enabled_index..].contains("Instant Growth"));
    let on_disk = fs::read_to_string(&staged.path).expect("staged file exists");
    assert_eq!(on_disk, staged.contents);
}

#[test]
fn staging_refuses_an_empty_selection() {
    let fixture = Fixture::new("stage-empty");
    let document = document();
    let error = stage_dolphin_ini(&fixture.path("staging"), "GAFE01.ini", &document, &[])
        .expect_err("blocked");
    assert_eq!(error.kind, DolphinInstallPlanErrorKind::NoSelectedCodes);
}

#[test]
fn a_preview_targets_the_real_gamesettings_layout() {
    let fixture = Fixture::new("preview");
    let document = document();
    let staged = stage_dolphin_ini(
        &fixture.path("staging"),
        "GAFE01.ini",
        &document,
        &["Infinite Bells [Nayr]".to_string()],
    )
    .expect("stages");
    let configuration_path = fixture.dir("dolphin-config");

    let preview = build_dolphin_install_preview(&DolphinInstallPreviewRequest {
        selected_archive: fixture.write("Animal Crossing (USA).iso", "x"),
        configuration_path: configuration_path.clone(),
        game_id: "GAFE01".to_string(),
        revision: Some(0),
        staged: staged.clone(),
    })
    .expect("preview builds");

    assert_eq!(preview.report.entries.len(), 1);
    let entry = &preview.report.entries[0];
    assert_eq!(entry.destination_root, configuration_path);
    assert_eq!(
        entry.destination_relative_path,
        Some(PathBuf::from("GameSettings/GAFE01.ini"))
    );
    assert_eq!(entry.source_path, Some(staged.path));
}

#[test]
fn deterministic_staging_produces_identical_bytes() {
    let fixture1 = Fixture::new("det-1");
    let fixture2 = Fixture::new("det-2");
    let document = document();
    let names = vec!["Infinite Bells [Nayr]".to_string()];
    let staged1 =
        stage_dolphin_ini(&fixture1.path("staging"), "GAFE01.ini", &document, &names).unwrap();
    let staged2 =
        stage_dolphin_ini(&fixture2.path("staging"), "GAFE01.ini", &document, &names).unwrap();
    assert_eq!(staged1.digest, staged2.digest);
    assert_eq!(staged1.contents, staged2.contents);
}
