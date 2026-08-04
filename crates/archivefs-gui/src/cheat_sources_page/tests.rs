//! Cheat Sources page tests.
//!
//! Assertions are on [`CheatSourcesPageView`] and on what reaches disk,
//! because the properties that matter are about what is *said* and what is
//! *kept*: that a disabled source is still listed in its resolved place,
//! that a preferences line this build cannot act on is visible rather than
//! quietly dropped, that nothing calls upstream cheat content reviewed, and
//! that nothing is written until the user saves.
//!
//! Every test that touches a file uses its own temporary directory. None of
//! them reads or writes the real per-user configuration: the page is always
//! constructed with an explicit path, which is the reason
//! `CheatSourcesPageState::load` takes one.

use super::*;
use archivefs_core::patch_manager::{
    CheatSourcesConfig, PlatformOverrideEntry, ProviderConfigEntry, ProviderPriorityOverride,
};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const KNOWN_ID: &str = "bsfree-archive";
const PS2_SOURCE: &str = "gamehacking.org-ps2";
const UNKNOWN_ID: &str = "a-provider-from-another-build";

/// A private directory for one test. Named per test so parallel runs cannot
/// collide, and removed first so a rerun starts clean.
fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "archivefs-cheat-sources-page-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn config_path(name: &str) -> PathBuf {
    test_root(name).join("cheat_sources.toml")
}

/// A page over a path that does not exist: built-in defaults, nothing saved.
fn fresh(name: &str) -> CheatSourcesPageState {
    CheatSourcesPageState::load(config_path(name))
}

fn write_config(path: &Path, cfg: &CheatSourcesConfig) {
    save_cheat_sources_config_to(path, cfg).unwrap();
}

// --- Listing --------------------------------------------------------------

#[test]
fn every_registered_source_is_listed() {
    let view = fresh("lists-all").view();
    assert_eq!(
        view.rows.len(),
        9,
        "all nine registered sources must appear, got {:?}",
        view.rows.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
}

#[test]
fn each_row_carries_identity_kind_and_coverage() {
    let view = fresh("row-fields").view();
    let ps2 = view
        .rows
        .iter()
        .find(|r| r.id == PS2_SOURCE)
        .expect("the PS2 source");

    assert!(!ps2.display_name.is_empty());
    assert_eq!(ps2.id, PS2_SOURCE, "the stable ID must be shown as-is");
    assert_eq!(ps2.emulator, "PCSX2");
    assert_eq!(ps2.platform_coverage, "PS2");
    assert!(!ps2.provider_kind.is_empty());
    assert!(!ps2.description.is_empty());
}

#[test]
fn a_cross_platform_source_says_all_platforms_not_none() {
    let view = fresh("coverage-all").view();
    let row = view
        .rows
        .iter()
        .find(|r| r.id == KNOWN_ID)
        .expect("bsfree is registered with no platform list");
    assert_eq!(
        row.platform_coverage, "All platforms",
        "an empty platform list means everywhere, and must never read as covering nothing"
    );
}

#[test]
fn a_disabled_source_is_still_listed_and_stays_in_place() {
    let mut state = fresh("disabled-visible");
    let before: Vec<String> = state.view().rows.iter().map(|r| r.id.clone()).collect();

    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    let view = state.view();
    let after: Vec<String> = view.rows.iter().map(|r| r.id.clone()).collect();

    assert_eq!(before, after, "disabling must not hide or move a source");
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert!(!row.enabled);
    assert_eq!(
        row.consulted_position, None,
        "a disabled source has no place in the consult order"
    );
}

#[test]
fn consulted_position_counts_only_enabled_sources_lowest_first() {
    let mut state = fresh("positions");
    let view = state.view();
    let first = view
        .rows
        .iter()
        .find(|r| r.consulted_position == Some(1))
        .expect("something must be consulted first");
    assert_eq!(
        first.priority,
        view.rows.iter().map(|r| r.priority).min().unwrap(),
        "the lowest number must be consulted first"
    );

    // Disabling the first source promotes the next one, and does not leave a gap.
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: first.id.clone(),
        enabled: false,
    });
    let view = state.view();
    let positions: Vec<usize> = view
        .rows
        .iter()
        .filter_map(|r| r.consulted_position)
        .collect();
    assert_eq!(
        positions,
        (1..=8).collect::<Vec<_>>(),
        "positions must stay contiguous over enabled sources"
    );
}

// --- Wording --------------------------------------------------------------

#[test]
fn no_row_claims_the_upstream_content_was_reviewed() {
    let view = fresh("wording").view();
    for row in &view.rows {
        assert_eq!(row.trust_label, BUILT_IN_INTEGRATION_LABEL);
        assert!(
            row.trust_label.contains("upstream content not reviewed"),
            "the scope must travel with the label: {}",
            row.trust_label
        );
        let bare_claim = row.trust_label == "Reviewed"
            || row.trust_label == "Trusted"
            || row.trust_label == "Verified";
        assert!(
            !bare_claim,
            "a bare trust badge would assert something untrue"
        );
    }
}

#[test]
fn the_page_states_the_ordering_rule_in_plain_language() {
    assert!(
        ORDERING_EXPLANATION.contains("lowest number first"),
        "priority reads backwards to most people and must be spelled out"
    );
    assert!(UPSTREAM_CONTENT_CAVEAT.contains("does not endorse"));
}

// --- Editing and dirty state ---------------------------------------------

#[test]
fn a_fresh_page_has_no_unsaved_changes() {
    let state = fresh("clean");
    assert!(!state.is_dirty());
    assert!(state.view().pending_consequences.is_empty());
}

#[test]
fn an_edit_marks_the_page_dirty_and_the_row_changed() {
    let mut state = fresh("dirty");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });

    let view = state.view();
    assert!(view.dirty);
    assert!(view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap().changed);
    assert!(
        view.rows.iter().filter(|r| r.changed).count() == 1,
        "only the edited row may be marked changed"
    );
}

#[test]
fn editing_back_to_the_saved_value_clears_the_dirty_state() {
    let mut state = fresh("dirty-round-trip");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    assert!(state.is_dirty());
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: true,
    });
    assert!(
        !state.is_dirty(),
        "returning to the saved value is not a pending change"
    );
}

#[test]
fn priority_can_be_edited_and_reorders_the_list() {
    let mut state = fresh("priority-edit");
    // bsfree defaults to 100, the highest number, so it is consulted last.
    let before = state.view();
    assert_eq!(
        before
            .rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .consulted_position,
        Some(9)
    );

    state.apply(CheatSourcesPageAction::SetPriority {
        id: KNOWN_ID.to_string(),
        priority: 1,
    });

    let after = state.view();
    let row = after.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert_eq!(row.priority, 1);
    assert_eq!(
        row.consulted_position,
        Some(1),
        "the lowest number must now be consulted first"
    );
}

#[test]
fn an_out_of_range_priority_is_refused_not_clamped() {
    // Matches the CLI, which rejects rather than clamping so a confirmation
    // never reports a value the caller did not ask for.
    let mut state = fresh("priority-range");
    let original = state
        .view()
        .rows
        .iter()
        .find(|r| r.id == KNOWN_ID)
        .unwrap()
        .priority;

    for bad in [0_u32, 1000, 5000] {
        state.apply(CheatSourcesPageAction::SetPriority {
            id: KNOWN_ID.to_string(),
            priority: bad,
        });
        let now = state
            .view()
            .rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .priority;
        assert_eq!(now, original, "{bad} must be refused, not clamped");
    }
    assert!(!state.is_dirty());
}

#[test]
fn discarding_changes_restores_the_saved_state() {
    let mut state = fresh("revert");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::SetPriority {
        id: PS2_SOURCE.to_string(),
        priority: 5,
    });
    assert!(state.is_dirty());

    state.apply(CheatSourcesPageAction::Revert);

    let view = state.view();
    assert!(!view.dirty);
    assert!(view.rows.iter().all(|r| !r.changed));
    assert!(view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap().enabled);
}

#[test]
fn pending_changes_are_explained_in_plain_language() {
    let mut state = fresh("consequences");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });

    let lines = state.view().pending_consequences;
    assert!(!lines.is_empty());
    let joined = lines.join(" ");
    assert!(
        joined.contains("no longer be consulted"),
        "the effect must be stated, not just the field name: {joined}"
    );
    assert!(
        joined.contains("cached data is kept"),
        "the user needs to know disabling is not deletion: {joined}"
    );
}

// --- Persistence ----------------------------------------------------------

#[test]
fn nothing_is_written_until_the_user_saves() {
    let path = config_path("no-write-before-save");
    let mut state = CheatSourcesPageState::load(path.clone());
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::SetPriority {
        id: PS2_SOURCE.to_string(),
        priority: 3,
    });

    assert!(
        !path.exists(),
        "editing must not touch disk; only Save may write"
    );
}

#[test]
fn saving_persists_and_clears_the_dirty_state() {
    let path = config_path("save");
    let mut state = CheatSourcesPageState::load(path.clone());
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    assert!(path.exists());
    assert!(!state.is_dirty());
    assert_eq!(state.view().save_state, SaveState::Saved);

    // And it is what a fresh load sees.
    let reloaded = CheatSourcesPageState::load(path);
    assert!(
        !reloaded
            .view()
            .rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .enabled
    );
}

#[test]
fn discarding_after_a_save_returns_to_what_was_saved_not_to_defaults() {
    let path = config_path("revert-after-save");
    let mut state = CheatSourcesPageState::load(path);
    state.apply(CheatSourcesPageAction::SetPriority {
        id: KNOWN_ID.to_string(),
        priority: 7,
    });
    state.apply(CheatSourcesPageAction::Save);

    state.apply(CheatSourcesPageAction::SetPriority {
        id: KNOWN_ID.to_string(),
        priority: 9,
    });
    state.apply(CheatSourcesPageAction::Revert);

    assert_eq!(
        state
            .view()
            .rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .priority,
        7,
        "discard returns to the last save, not to the built-in default"
    );
}

#[test]
fn an_untouched_page_that_saves_writes_only_defaults() {
    let path = config_path("save-untouched");
    let mut state = CheatSourcesPageState::load(path.clone());
    state.apply(CheatSourcesPageAction::Save);

    let reloaded = load_cheat_sources_config_from(&path).unwrap();
    assert_eq!(
        reloaded,
        CheatSourcesConfig::default(),
        "an untouched page must not start recording preferences"
    );
}

// --- Per-platform participation ------------------------------------------

#[test]
fn a_platform_specific_source_offers_its_platforms() {
    let view = fresh("platform-list").view();
    let ps2 = view.rows.iter().find(|r| r.id == PS2_SOURCE).unwrap();
    assert_eq!(ps2.platforms.len(), 1);
    assert_eq!(ps2.platforms[0].platform, "PS2");
    assert!(
        ps2.platforms[0].participating,
        "participation is on by default"
    );
}

#[test]
fn per_platform_participation_can_be_turned_off_without_disabling_the_source() {
    let mut state = fresh("participation-off");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: PS2_SOURCE.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == PS2_SOURCE).unwrap();
    assert!(row.enabled, "the source itself stays enabled");
    assert!(!row.platforms[0].participating);
    assert!(row.changed);
    assert!(view.dirty);

    let joined = view.pending_consequences.join(" ");
    assert!(
        joined.contains("stays enabled elsewhere"),
        "the distinction from a full disable must be stated: {joined}"
    );
}

#[test]
fn a_source_disabled_everywhere_reports_that_the_platform_toggle_cannot_help() {
    let mut state = fresh("participation-overridden");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: PS2_SOURCE.to_string(),
        enabled: false,
    });

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == PS2_SOURCE).unwrap();
    assert!(
        row.platforms[0].overridden_by_source_level,
        "the control must be shown inactive with a reason, not silently ignored"
    );
}

#[test]
fn participation_survives_a_save_and_reload() {
    let path = config_path("participation-persist");
    let mut state = CheatSourcesPageState::load(path.clone());
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: PS2_SOURCE.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    let reloaded = CheatSourcesPageState::load(path);
    let row = reloaded
        .view()
        .rows
        .into_iter()
        .find(|r| r.id == PS2_SOURCE)
        .unwrap();
    assert!(!row.platforms[0].participating);
}

// --- Unresolved entries ---------------------------------------------------

#[test]
fn an_unknown_provider_is_shown_not_hidden() {
    let path = config_path("unknown-shown");
    write_config(
        &path,
        &CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: Some(50),
            }]),
            platform_overrides: None,
        },
    );

    let view = CheatSourcesPageState::load(path).view();
    assert_eq!(view.unresolved.len(), 1);
    assert_eq!(view.unresolved[0].detail, UNKNOWN_ID);
    assert!(
        view.unresolved[0].explanation.contains("Kept as written"),
        "the user must be told it was preserved: {}",
        view.unresolved[0].explanation
    );
    assert!(
        view.rows.iter().all(|r| r.id != UNKNOWN_ID),
        "an unknown entry is not a source and must not appear as an editable row"
    );
}

#[test]
fn an_unresolved_platform_override_is_shown() {
    let path = config_path("unknown-platform-shown");
    write_config(
        &path,
        &CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "NotAPlatformThisBuildKnows".to_string(),
                disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                priority_overrides: None,
            }]),
        },
    );

    let view = CheatSourcesPageState::load(path).view();
    assert_eq!(view.unresolved.len(), 1);
    assert_eq!(view.unresolved[0].detail, "NotAPlatformThisBuildKnows");
}

#[test]
fn saving_from_the_page_preserves_every_unresolved_entry() {
    // The property the whole round-trip fix exists for, exercised the way a
    // user would hit it: open the page, change something unrelated, save.
    let path = config_path("preserve-on-save");
    let original = CheatSourcesConfig {
        providers: Some(vec![ProviderConfigEntry {
            id: UNKNOWN_ID.to_string(),
            enabled: Some(false),
            priority: Some(42),
        }]),
        platform_overrides: Some(vec![PlatformOverrideEntry {
            platform: "AlsoUnknown".to_string(),
            disabled_providers: Some(vec!["whoever".to_string()]),
            priority_overrides: Some(vec![ProviderPriorityOverride {
                id: "whoever".to_string(),
                priority: 4,
            }]),
        }]),
    };
    write_config(&path, &original);

    let mut state = CheatSourcesPageState::load(path.clone());
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::Save);
    assert_eq!(state.view().save_state, SaveState::Saved);

    let after = load_cheat_sources_config_from(&path).unwrap();
    let providers = after.providers.expect("providers");
    let kept = providers
        .iter()
        .find(|p| p.id == UNKNOWN_ID)
        .expect("the unknown provider must survive an unrelated edit");
    assert_eq!(kept.enabled, Some(false));
    assert_eq!(kept.priority, Some(42));
    assert_eq!(
        after.platform_overrides.expect("overrides"),
        original.platform_overrides.unwrap(),
        "unresolved platform blocks must be re-emitted verbatim"
    );
}

#[test]
fn an_unreadable_file_is_reported_and_never_overwritten() {
    let path = config_path("unreadable");
    fs::write(&path, "this is not valid toml {{[").unwrap();
    let before = fs::read_to_string(&path).unwrap();

    let mut state = CheatSourcesPageState::load(path.clone());
    assert!(
        state.view().load_error.is_some(),
        "a parse failure must be surfaced, not swallowed"
    );

    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    assert!(matches!(state.view().save_state, SaveState::Failed(_)));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        before,
        "a file that failed to parse must never be overwritten with defaults"
    );
}

// --- Rendering ------------------------------------------------------------
//
// The list above is about data. These two prove the drawing does not
// contradict it, since "is it visible?" is ultimately a claim about drawing.

/// Draws the page headlessly, the way the RomM card's tests do.
fn render(view: &CheatSourcesPageView) -> egui::FullOutput {
    let mut drafts: HashMap<String, String> = HashMap::new();
    let context = egui::Context::default();
    context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_cheat_sources_page(ui, view, &mut drafts);
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
fn the_rendered_page_draws_every_source_with_its_id() {
    let state = fresh("render-all");
    let view = state.view();
    let output = render(&view);

    for row in &view.rows {
        assert!(
            rendered_text_contains(&output, &row.display_name),
            "did not draw {}",
            row.display_name
        );
        assert!(
            rendered_text_contains(&output, &row.id),
            "did not draw the stable ID {}",
            row.id
        );
    }
}

#[test]
fn the_rendered_page_draws_the_scoped_trust_wording_and_never_a_bare_one() {
    let view = fresh("render-wording").view();
    let output = render(&view);

    assert!(
        rendered_text_contains(&output, BUILT_IN_INTEGRATION_LABEL),
        "the required wording must actually be drawn"
    );
    assert!(
        rendered_text_contains(&output, "lowest number first"),
        "the ordering rule must be drawn, not just modelled"
    );
    assert!(
        !rendered_text_contains(&output, "upstream content reviewed"),
        "nothing may state the upstream content was reviewed"
    );
}

#[test]
fn a_disabled_source_is_still_drawn() {
    let mut state = fresh("render-disabled");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    let view = state.view();
    let output = render(&view);

    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert!(
        rendered_text_contains(&output, &row.display_name),
        "disabling must not remove a source from the page"
    );
    assert!(rendered_text_contains(&output, "Disabled"));
}

#[test]
fn the_rendered_page_draws_unsaved_state_and_its_consequences() {
    let mut state = fresh("render-dirty");
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    let output = render(&state.view());

    assert!(rendered_text_contains(&output, "Unsaved changes"));
    assert!(rendered_text_contains(
        &output,
        "Nothing is written until you save."
    ));
    assert!(rendered_text_contains(&output, "no longer be consulted"));
}

#[test]
fn a_clean_page_draws_that_there_is_nothing_to_save() {
    let output = render(&fresh("render-clean").view());
    assert!(rendered_text_contains(&output, "No unsaved changes"));
}

#[test]
fn the_rendered_page_draws_unresolved_entries_rather_than_hiding_them() {
    let path = config_path("render-unresolved");
    write_config(
        &path,
        &CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            platform_overrides: None,
        },
    );
    let view = CheatSourcesPageState::load(path).view();
    let output = render(&view);

    assert!(rendered_text_contains(&output, "Kept but not recognised"));
    assert!(
        rendered_text_contains(&output, UNKNOWN_ID),
        "the unrecognised ID must be named so the user can correct it"
    );
}
