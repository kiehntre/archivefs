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

fn external_gafe01_result() -> GeckoProviderResult {
    GeckoProviderResult {
        provider_id: "dolphin_upstream_gamesettings".to_string(),
        provider_display_name: "Dolphin upstream GameSettings".to_string(),
        source_identity: "fixture:GAFE01.ini".to_string(),
        retrieved_at_unix_seconds: 1,
        game_id: "GAFE01".to_string(),
        title: Some("Animal Crossing".to_string()),
        region: super::super::dolphin_gecko_provider::GeckoRegion::Usa,
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
            region: super::super::dolphin_gecko_provider::GeckoRegion::Usa,
            revision_applicability:
                super::super::dolphin_gecko_provider::GeckoRevisionApplicability::Uncertain,
            parse_warnings: vec!["revision applicability is uncertain".to_string()],
            safe_to_offer: true,
        }],
        warnings: Vec::new(),
        attribution: "Dolphin upstream".to_string(),
        license: "GPL-2.0-or-later".to_string(),
    }
}

#[test]
fn external_provider_discovery_does_not_require_a_preexisting_ini() {
    let fixture = Fixture::new("provider-new");
    let configuration_path = fixture.dir("dolphin");
    let destination = load_dolphin_destination(&configuration_path, "GAFE01").unwrap();
    assert!(!destination.existed);
    let provider = external_gafe01_result();
    let mut selection = DolphinProviderCodeSelection::from_provider(&provider, &destination);
    selection.select_all();
    let staged = stage_dolphin_provider_ini(
        &fixture.path("staging"),
        &destination,
        &provider,
        &selection,
    )
    .unwrap();
    assert!(!staged.destination_existed);
    assert!(staged.contents.contains("$16:9 Widescreen\n"));
    assert!(
        staged
            .contents
            .contains("[Gecko_Enabled]\n$16:9 Widescreen\n")
    );
    assert_eq!(staged.selected_code_names, vec!["16:9 Widescreen"]);
}

#[test]
fn external_provider_merge_preserves_existing_settings_and_unrelated_gecko_codes() {
    let fixture = Fixture::new("provider-existing");
    let configuration_path = fixture.dir("dolphin");
    fixture.write(
        "dolphin/GameSettings/GAFE01.ini",
        "[Core]\nFastDiscSpeed = True\n[Gecko]\n$Existing Code\n04000000 60000000\n[Gecko_Enabled]\n$Existing Code\n",
    );
    let destination = load_dolphin_destination(&configuration_path, "GAFE01").unwrap();
    let provider = external_gafe01_result();
    let mut selection = DolphinProviderCodeSelection::from_provider(&provider, &destination);
    selection.select_all();
    let staged = stage_dolphin_provider_ini(
        &fixture.path("staging"),
        &destination,
        &provider,
        &selection,
    )
    .unwrap();
    assert!(staged.contents.contains("[Core]\nFastDiscSpeed = True\n"));
    assert!(
        staged
            .contents
            .contains("$Existing Code\n04000000 60000000\n")
    );
    assert!(
        staged
            .contents
            .contains("[Gecko_Enabled]\n$Existing Code\n$16:9 Widescreen\n")
    );
}

#[test]
fn an_identical_existing_provider_code_is_not_duplicated() {
    let fixture = Fixture::new("provider-duplicate");
    let configuration_path = fixture.dir("dolphin");
    let provider = external_gafe01_result();
    let body = provider.entries[0].code_lines.join("\n");
    fixture.write(
        "dolphin/GameSettings/GAFE01.ini",
        &format!("[Gecko]\n$16:9 Widescreen\n{body}\n"),
    );
    let destination = load_dolphin_destination(&configuration_path, "GAFE01").unwrap();
    let mut selection = DolphinProviderCodeSelection::from_provider(&provider, &destination);
    assert!(selection.entries[0].already_present);
    selection.select_all();
    let staged = stage_dolphin_provider_ini(
        &fixture.path("staging"),
        &destination,
        &provider,
        &selection,
    )
    .unwrap();
    assert_eq!(staged.contents.matches("$16:9 Widescreen").count(), 2);
}

#[test]
fn external_provider_preview_is_deterministic() {
    let fixture = Fixture::new("provider-deterministic");
    let configuration_path = fixture.dir("dolphin");
    let destination = load_dolphin_destination(&configuration_path, "GAFE01").unwrap();
    let provider = external_gafe01_result();
    let mut selection = DolphinProviderCodeSelection::from_provider(&provider, &destination);
    selection.select_all();
    let first = stage_dolphin_provider_ini(
        &fixture.path("stage-a"),
        &destination,
        &provider,
        &selection,
    )
    .unwrap();
    let second = stage_dolphin_provider_ini(
        &fixture.path("stage-b"),
        &destination,
        &provider,
        &selection,
    )
    .unwrap();
    assert_eq!(first.contents, second.contents);
    assert_eq!(first.digest, second.digest);
}
