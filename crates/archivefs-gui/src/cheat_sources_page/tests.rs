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
    CheatSourcesPageState::load(config_path(name), None)
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
    let mut state = CheatSourcesPageState::load(path.clone(), None);
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
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    assert!(path.exists());
    assert!(!state.is_dirty());
    assert_eq!(state.view().save_state, SaveState::Saved);

    // And it is what a fresh load sees.
    let reloaded = CheatSourcesPageState::load(path, None);
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
    let mut state = CheatSourcesPageState::load(path, None);
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
    let mut state = CheatSourcesPageState::load(path.clone(), None);
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
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: PS2_SOURCE.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    let reloaded = CheatSourcesPageState::load(path, None);
    let row = reloaded
        .view()
        .rows
        .into_iter()
        .find(|r| r.id == PS2_SOURCE)
        .unwrap();
    assert!(!row.platforms[0].participating);
}

// --- Platform exceptions for cross-platform sources ----------------------
//
// A source with no declared platforms applies everywhere. Before this, the
// page showed platform toggles only for platforms already named in the file,
// so the *first* exception could not be created from the GUI at all - the
// feature existed but was reachable only by hand-editing the TOML.

#[test]
fn a_cross_platform_source_offers_the_picker_and_a_specific_one_does_not() {
    let view = fresh("picker-offered").view();

    let cross = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert!(
        cross.supports_platform_exceptions,
        "a source with no declared platforms needs a way to create the first exception"
    );
    assert!(
        cross.platforms.is_empty(),
        "with no exceptions yet there is nothing to list"
    );

    let specific = view.rows.iter().find(|r| r.id == PS2_SOURCE).unwrap();
    assert!(
        !specific.supports_platform_exceptions,
        "a source that declares its platforms already shows a toggle for each"
    );
}

#[test]
fn the_picker_offers_only_canonical_platforms() {
    let choices = available_platform_choices(&[], "");
    assert!(!choices.is_empty());

    let canonical = archivefs_core::platform::canonical_ids();
    for choice in &choices {
        assert!(
            canonical.contains(&choice.id),
            "{} is not in the canonical registry - nothing may be invented",
            choice.id
        );
        assert!(
            archivefs_core::canonical_platform_for_alias(choice.id).is_some(),
            "{} must resolve, or an override built from it could never match",
            choice.id
        );
    }
}

#[test]
fn the_picker_is_bounded_and_reports_what_it_truncated() {
    let all = available_platform_choices(&[], "");
    assert!(
        all.len() <= MAX_PLATFORM_CHOICES,
        "the list shown must stay bounded, got {}",
        all.len()
    );
    assert!(
        available_platform_count(&[], "") > all.len(),
        "the registry has more platforms than the picker shows, so the count must differ \
         and the UI can say 'showing N of M'"
    );
}

#[test]
fn the_picker_searches_by_display_name_and_by_id() {
    let by_name = available_platform_choices(&[], "PlayStation");
    assert!(
        !by_name.is_empty(),
        "a search for a real platform family must match something"
    );
    for choice in &by_name {
        assert!(
            choice.display_name.to_lowercase().contains("playstation")
                || choice.id.to_lowercase().contains("playstation"),
            "{choice:?} does not match the query"
        );
    }

    assert!(
        available_platform_choices(&[], "PS2")
            .iter()
            .any(|c| c.id == "PS2"),
        "searching the canonical id must find it"
    );
    assert!(
        available_platform_choices(&[], "zzzz-no-such-platform").is_empty(),
        "an unmatched query must offer nothing rather than falling back to everything"
    );
}

#[test]
fn creating_the_first_exception_from_the_picker_works() {
    let mut state = fresh("first-exception");
    // No exception recorded yet for the cross-platform source. (Sources that
    // declare platforms legitimately list those; they are not exceptions.)
    assert!(
        state
            .view()
            .rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .platforms
            .is_empty()
    );

    let choice = available_platform_choices(&[], "PS2")
        .into_iter()
        .find(|c| c.id == "PS2")
        .expect("PS2 is canonical");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: choice.id.to_string(),
        participating: false,
    });

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert_eq!(row.platforms.len(), 1);
    assert_eq!(row.platforms[0].platform, "PS2");
    assert!(!row.platforms[0].participating);
    assert!(
        row.platforms[0].is_exception,
        "a platform the source does not declare is an exception, and removable"
    );
    assert!(
        row.enabled,
        "an exception must not disable the source everywhere"
    );
    assert!(view.dirty);
}

#[test]
fn a_platform_already_excepted_is_not_offered_again() {
    let mut state = fresh("no-duplicates");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    let existing: Vec<String> = row.platforms.iter().map(|p| p.platform.clone()).collect();

    assert!(
        !available_platform_choices(&existing, "")
            .iter()
            .any(|c| c.id == "PS2"),
        "an existing exception must be excluded from the picker"
    );
    assert!(
        !available_platform_choices(&existing, "PS2")
            .iter()
            .any(|c| c.id == "PS2"),
        "searching for it explicitly must not resurrect it either"
    );
}

#[test]
fn applying_the_same_exception_twice_records_it_once() {
    // Belt and braces: the picker excludes it, and the state layer refuses to
    // double it even if something else asked.
    let mut state = fresh("idempotent-exception");
    for _ in 0..3 {
        state.apply(CheatSourcesPageAction::SetPlatformParticipation {
            id: KNOWN_ID.to_string(),
            platform: "PS2".to_string(),
            participating: false,
        });
    }
    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert_eq!(row.platforms.len(), 1, "got {:?}", row.platforms);
}

#[test]
fn an_alias_cannot_create_a_second_exception_for_one_platform() {
    let mut state = fresh("alias-no-duplicate");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    let existing = vec!["PS2".to_string()];
    // Every canonical id the picker could still offer must resolve to
    // something other than the one already taken.
    for choice in available_platform_choices(&existing, "") {
        assert_ne!(
            archivefs_core::canonical_platform_for_alias(choice.id),
            Some("PS2"),
            "{} canonicalises onto an exception that already exists",
            choice.id
        );
    }
}

#[test]
fn removing_an_exception_restores_participation_and_leaves_no_residue() {
    let path = config_path("remove-exception");
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: true,
    });
    state.apply(CheatSourcesPageAction::Save);

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert!(
        row.platforms.is_empty(),
        "removing the exception must take the row away, not leave a checked stub"
    );

    let on_disk = load_cheat_sources_config_from(&path).unwrap();
    assert!(
        on_disk.platform_overrides.is_none(),
        "an emptied block must not linger in the file: {on_disk:?}"
    );
}

#[test]
fn an_exception_survives_save_and_reload() {
    let path = config_path("exception-persist");
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "GameCube".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);
    assert_eq!(state.view().save_state, SaveState::Saved);

    let reloaded = CheatSourcesPageState::load(path, None);
    let view = reloaded.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert_eq!(row.platforms.len(), 1);
    assert_eq!(row.platforms[0].platform, "GameCube");
    assert!(!row.platforms[0].participating);
    assert!(!reloaded.is_dirty());
}

#[test]
fn an_unsaved_exception_never_reaches_disk() {
    let path = config_path("exception-not-saved");
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    assert!(state.is_dirty());
    assert!(!path.exists(), "only Save may write");
}

#[test]
fn discarding_removes_an_unsaved_exception() {
    let mut state = fresh("exception-discard");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Revert);

    let view = state.view();
    assert!(!view.dirty);
    assert!(
        view.rows
            .iter()
            .find(|r| r.id == KNOWN_ID)
            .unwrap()
            .platforms
            .is_empty(),
        "discard must return to the original state"
    );
}

#[test]
fn discarding_returns_to_a_saved_exception_not_to_none() {
    let path = config_path("exception-discard-after-save");
    let mut state = CheatSourcesPageState::load(path, None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "Wii".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Revert);

    let view = state.view();
    let row = view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap();
    assert_eq!(
        row.platforms.len(),
        1,
        "discard must keep the saved exception and drop only the unsaved one"
    );
    assert_eq!(row.platforms[0].platform, "PS2");
}

#[test]
fn the_consequence_of_an_exception_is_stated_before_saving() {
    let mut state = fresh("exception-consequence");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });

    let joined = state.view().pending_consequences.join(" ");
    assert!(
        joined.contains("will not be used for Sony PlayStation 2 games"),
        "the affected platform must be named, by its display name: {joined}"
    );
    assert!(
        joined.contains("stays enabled elsewhere"),
        "the difference from a full disable must be stated: {joined}"
    );
}

#[test]
fn an_already_saved_exception_is_not_announced_as_a_pending_change() {
    // The consequence list said what saving *would do*. Listing every
    // non-participating platform on a changed row meant editing a source's
    // priority announced an exception the user saved long ago as though
    // saving would newly apply it.
    let path = config_path("consequence-only-pending");
    let mut state = CheatSourcesPageState::load(path, None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    // Now change something unrelated on the same source.
    state.apply(CheatSourcesPageAction::SetPriority {
        id: KNOWN_ID.to_string(),
        priority: 7,
    });

    let lines = state.view().pending_consequences;
    let joined = lines.join(" ");
    assert!(
        joined.contains("moves to priority 7"),
        "the real pending change must be stated: {joined}"
    );
    assert!(
        !joined.contains("will not be used for"),
        "a saved exception is not a pending change: {joined}"
    );
}

#[test]
fn a_platform_exception_under_a_disabled_source_does_not_promise_it_stays_enabled() {
    // The line claimed the source "stays enabled elsewhere". When the source is
    // switched off at source level it is consulted nowhere, so that sentence
    // described behaviour the user would never see.
    let path = config_path("consequence-disabled-source");
    let mut state = CheatSourcesPageState::load(path, None);
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });

    let joined = state.view().pending_consequences.join(" ");
    assert!(
        !joined.contains("stays enabled elsewhere"),
        "a source that is off everywhere does not stay enabled anywhere: {joined}"
    );
    assert!(
        joined.contains("has no effect until it is turned back on"),
        "the inert setting must be stated plainly: {joined}"
    );
}

#[test]
fn removing_a_saved_exception_is_announced() {
    let path = config_path("consequence-removal");
    let mut state = CheatSourcesPageState::load(path, None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: true,
    });

    let joined = state.view().pending_consequences.join(" ");
    assert!(
        joined.contains("will be used for Sony PlayStation 2 games again"),
        "undoing an exception must be stated too, not left silent: {joined}"
    );
}

#[test]
fn a_toggle_takes_effect_even_with_duplicate_platform_blocks() {
    // Resolution reads the last matching block. Writing to the first left a
    // later block still disabling the source, so the checkbox moved, the
    // resolved state did not, and the next repaint drew it as still disabled.
    let path = config_path("duplicate-blocks");
    write_config(
        &path,
        &CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
            ]),
        },
    );

    let mut state = CheatSourcesPageState::load(path.clone(), None);
    let row = state
        .view()
        .rows
        .into_iter()
        .find(|r| r.id == KNOWN_ID)
        .unwrap();
    assert_eq!(
        row.platforms.len(),
        1,
        "one platform must produce one row, not one per block: {:?}",
        row.platforms
    );
    assert!(!row.platforms[0].participating);

    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: true,
    });

    let row = state
        .view()
        .rows
        .into_iter()
        .find(|r| r.id == KNOWN_ID)
        .unwrap();
    assert!(
        row.platforms.is_empty(),
        "re-enabling must actually take effect: {:?}",
        row.platforms
    );

    state.apply(CheatSourcesPageAction::Save);
    let on_disk = load_cheat_sources_config_from(&path).unwrap();
    assert!(
        on_disk.platform_overrides.is_none(),
        "both emptied blocks should be gone: {on_disk:?}"
    );
}

#[test]
fn removing_an_exception_preserves_unrelated_unknown_data() {
    // Cleanup must not reach past the platform being edited.
    let path = config_path("removal-preserves-unknown");
    let unresolvable = PlatformOverrideEntry {
        platform: "SomePlatformThisBuildLacks".to_string(),
        disabled_providers: Some(vec!["whoever".to_string()]),
        priority_overrides: None,
    };
    write_config(
        &path,
        &CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: Some(42),
            }]),
            platform_overrides: Some(vec![
                unresolvable.clone(),
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
            ]),
        },
    );

    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: true,
    });
    state.apply(CheatSourcesPageAction::Save);

    let after = load_cheat_sources_config_from(&path).unwrap();
    assert!(
        after
            .platform_overrides
            .as_ref()
            .expect("the unresolvable block must remain")
            .contains(&unresolvable),
        "removing one exception must not delete unrelated blocks: {after:?}"
    );
    assert_eq!(
        after
            .providers
            .expect("providers")
            .iter()
            .filter(|p| p.id == UNKNOWN_ID)
            .count(),
        1,
        "nor the unknown provider"
    );
}

#[test]
fn a_failed_save_keeps_the_changes_pending_and_does_not_claim_success() {
    // A write that cannot complete must leave the user with their edits and
    // an honest error, not a cleared dirty flag implying the work is safe.
    let root = test_root("failed-save");
    let blocked = root.join("cheat_sources.toml");
    fs::create_dir_all(&blocked).expect("a directory where the file should be");

    let mut state = CheatSourcesPageState::load(blocked, None);
    state.apply(CheatSourcesPageAction::SetEnabled {
        id: KNOWN_ID.to_string(),
        enabled: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    let view = state.view();
    assert!(
        matches!(view.save_state, SaveState::Failed(_)),
        "got {:?}",
        view.save_state
    );
    assert!(
        view.dirty,
        "a failed save must leave the changes pending, not look saved"
    );
    assert!(
        !view.rows.iter().find(|r| r.id == KNOWN_ID).unwrap().enabled,
        "and must not discard what the user edited"
    );
}

#[test]
fn adding_an_exception_preserves_unknown_entries() {
    let path = config_path("exception-preserves-unknown");
    let original = CheatSourcesConfig {
        providers: Some(vec![ProviderConfigEntry {
            id: UNKNOWN_ID.to_string(),
            enabled: Some(false),
            priority: Some(42),
        }]),
        platform_overrides: Some(vec![PlatformOverrideEntry {
            platform: "SomePlatformThisBuildLacks".to_string(),
            disabled_providers: Some(vec!["whoever".to_string()]),
            priority_overrides: Some(vec![ProviderPriorityOverride {
                id: "whoever".to_string(),
                priority: 4,
            }]),
        }]),
    };
    write_config(&path, &original);

    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    let after = load_cheat_sources_config_from(&path).unwrap();
    let kept = after
        .providers
        .expect("providers")
        .into_iter()
        .find(|p| p.id == UNKNOWN_ID)
        .expect("the unknown provider must survive adding an exception");
    assert_eq!(kept.priority, Some(42));

    let blocks = after.platform_overrides.expect("overrides");
    assert!(
        blocks.contains(&original.platform_overrides.unwrap()[0]),
        "the unresolvable block must be re-emitted verbatim: {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| b.platform == "PS2"),
        "and the new exception must be there too"
    );
}

#[test]
fn an_exception_does_not_change_priority_order() {
    let mut state = fresh("exception-keeps-order");
    let before: Vec<(String, u32)> = state
        .view()
        .rows
        .iter()
        .map(|r| (r.id.clone(), r.priority))
        .collect();

    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });

    let after: Vec<(String, u32)> = state
        .view()
        .rows
        .iter()
        .map(|r| (r.id.clone(), r.priority))
        .collect();
    assert_eq!(
        before, after,
        "a platform exception must not touch priorities or their order"
    );
}

#[test]
fn the_picker_writes_no_new_config_keys() {
    // The Milestone 1 floor: this flow uses `disabled_providers`, which has
    // always existed. It must not introduce a field.
    let path = config_path("exception-no-new-keys");
    let mut state = CheatSourcesPageState::load(path.clone(), None);
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    state.apply(CheatSourcesPageAction::Save);

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("disabled_providers"));
    assert!(!text.contains("format_version"));
    for forbidden in ["exceptions", "participation", "trust_level", "platforms ="] {
        assert!(
            !text.contains(forbidden),
            "unexpected key {forbidden:?} in:\n{text}"
        );
    }
}

#[test]
fn the_rendered_picker_lists_platforms_and_says_they_are_currently_used() {
    let state = fresh("render-picker");
    let view = state.view();
    let mut ui_state = CheatSourcesPageUi {
        open_picker: Some(KNOWN_ID.to_string()),
        picker_query: "PlayStation".to_string(),
        ..CheatSourcesPageUi::default()
    };
    let output = render_with(&view, &mut ui_state, false);

    assert!(
        rendered_text_contains(&output, "Find a platform:"),
        "the search box must be drawn when the picker is open"
    );
    assert!(
        rendered_text_contains(&output, "Currently used"),
        "participation on the offered platform must be stated, not inferred"
    );
    assert!(
        rendered_text_contains(&output, "Stop using for"),
        "the action must say what it does"
    );
}

#[test]
fn the_picker_is_closed_by_default_and_offers_an_opener() {
    let view = fresh("render-picker-closed").view();
    let output = render(&view);

    assert!(
        rendered_text_contains(&output, "Don't use for a platform…"),
        "there must be a visible way to create the first exception"
    );
    assert!(
        !rendered_text_contains(&output, "Find a platform:"),
        "the search box must stay closed until asked for"
    );
}

#[test]
fn the_rendered_exception_row_offers_removal() {
    let mut state = fresh("render-exception-row");
    state.apply(CheatSourcesPageAction::SetPlatformParticipation {
        id: KNOWN_ID.to_string(),
        platform: "PS2".to_string(),
        participating: false,
    });
    let output = render(&state.view());

    assert!(rendered_text_contains(
        &output,
        "not used for this platform"
    ));
    assert!(
        rendered_text_contains(&output, "Remove exception"),
        "an exception the user added must be removable from the page"
    );
}

#[test]
fn discarding_clears_unsubmitted_picker_state() {
    let mut ui_state = CheatSourcesPageUi {
        open_picker: Some(KNOWN_ID.to_string()),
        picker_query: "half typed".to_string(),
        ..CheatSourcesPageUi::default()
    };
    ui_state.priority_drafts.insert("x".into(), "12".into());

    ui_state.clear();

    assert!(ui_state.open_picker.is_none());
    assert!(ui_state.picker_query.is_empty());
    assert!(ui_state.priority_drafts.is_empty());
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

    let view = CheatSourcesPageState::load(path, None).view();
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

    let view = CheatSourcesPageState::load(path, None).view();
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

    let mut state = CheatSourcesPageState::load(path.clone(), None);
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

    let mut state = CheatSourcesPageState::load(path.clone(), None);
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

/// Draws the page headlessly, the way the RomM card's tests do. Advanced View
/// by default, so the established assertions on IDs, consultation order and
/// the review wording keep exercising the full technical layout.
fn render(view: &CheatSourcesPageView) -> egui::FullOutput {
    render_with(view, &mut CheatSourcesPageUi::default(), false)
}

/// Draws in Gamer View, where the beginner-facing simplification applies.
fn render_gamer(view: &CheatSourcesPageView) -> egui::FullOutput {
    render_with(view, &mut CheatSourcesPageUi::default(), true)
}

/// Draws with explicit UI state, for the picker's open/closed cases.
fn render_with(
    view: &CheatSourcesPageView,
    ui_state: &mut CheatSourcesPageUi,
    gamer_view: bool,
) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_cheat_sources_page(ui, view, ui_state, gamer_view);
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
    let view = CheatSourcesPageState::load(path, None).view();
    let output = render(&view);

    assert!(rendered_text_contains(&output, "Kept but not recognised"));
    assert!(
        rendered_text_contains(&output, UNKNOWN_ID),
        "the unrecognised ID must be named so the user can correct it"
    );
}

// ---------------------------------------------------------------------------
// Gamer View simplification
// ---------------------------------------------------------------------------

#[test]
fn gamer_view_hides_numeric_and_internal_metadata_by_default() {
    let state = fresh("gamer-hides");
    let view = state.view();
    let output = render_gamer(&view);

    // The beginner-facing essentials are present.
    for row in &view.rows {
        assert!(
            rendered_text_contains(&output, &row.display_name),
            "name must be shown: {}",
            row.display_name
        );
        assert!(
            rendered_text_contains(&output, row.provider_kind),
            "capability must be shown for {}",
            row.display_name
        );
        assert!(
            rendered_text_contains(&output, &row.platform_coverage),
            "platform scope must be shown for {}",
            row.display_name
        );
    }

    // Numeric/internal metadata is not on the page by default: no raw source
    // IDs, no consultation order, no bare priority, no "Multi" family label.
    for row in &view.rows {
        assert!(
            !rendered_text_contains(&output, &format!("ID: {}", row.id)),
            "source ID {} must not be foregrounded in Gamer View",
            row.id
        );
    }
    assert!(
        !rendered_text_contains(&output, "Consulted"),
        "order is technical"
    );
    assert!(
        !rendered_text_contains(&output, "Priority:"),
        "numeric priority is technical"
    );
    assert!(
        !rendered_text_contains(&output, "not reviewed"),
        "upstream review wording is technical"
    );
    // The internal family label "Multi" is spelled as what it means for a
    // beginner instead of surfacing as raw parser provenance.
    assert!(
        rendered_text_contains(&output, "Multi-system"),
        "the friendly family label should be shown in Gamer View"
    );
}

#[test]
fn gamer_view_still_reaches_technical_details_and_keeps_controls() {
    let state = fresh("gamer-technical");
    let output = render_gamer(&state.view());

    // The disclosure labels are present even when collapsed, so a beginner can
    // find the advanced fields; the underlying controls are not removed.
    assert!(
        rendered_text_contains(&output, "Technical details"),
        "the advanced fields must be reachable one disclosure down"
    );
}

#[test]
fn advanced_view_still_shows_the_full_technical_layout() {
    let state = fresh("advanced-full");
    let view = state.view();
    let output = render(&view);

    for row in &view.rows {
        assert!(
            rendered_text_contains(&output, &format!("ID: {}", row.id)),
            "Advanced View keeps the stable ID for {}",
            row.id
        );
    }
    assert!(
        rendered_text_contains(&output, "Consulted"),
        "order badge in Advanced View"
    );
    assert!(
        rendered_text_contains(&output, "lowest number first"),
        "the ordering rule stays visible in Advanced View"
    );
}

// ---------------------------------------------------------------------------
// Health display and refresh
// ---------------------------------------------------------------------------

#[test]
fn a_probed_ready_source_is_drawn_with_its_status_and_entry_count() {
    let root = test_root("health-ready");
    let data_root = root.join("data");
    let source_root = data_root
        .join("cheat-sources")
        .join("libretro-buildbot-cheats");
    std::fs::create_dir_all(source_root.join("snapshots").join("abc123")).unwrap();
    std::fs::write(
        source_root.join("metadata.json"),
        r#"{
  "format_version": 1,
  "source_id": "libretro-buildbot-cheats",
  "current_snapshot": "abc123",
  "manifest": {
    "fetched_at_unix_seconds": 1000,
    "valid_cheat_count": 42
  },
  "last_fetch_succeeded": true,
  "last_error": null
}
"#,
    )
    .unwrap();

    let page = CheatSourcesPageState::load(root.join("cheat_sources.toml"), Some(data_root));
    let rows = page.view().rows;
    let libretro = rows
        .iter()
        .find(|row| row.id == "libretro-buildbot-cheats")
        .expect("libretro source row");
    let health = libretro.health.as_ref().expect("libretro health is probed");
    assert_eq!(
        health.state,
        archivefs_core::patch_manager::CheatProviderSourceState::Ready
    );
    assert_eq!(health.entry_count, Some(42));
    assert!(health.last_checked_unix_seconds.is_some());

    let output = render(&page.view());
    assert!(rendered_text_contains(&output, "Ready"));
    assert!(rendered_text_contains(&output, "42 entries"));
}

#[test]
fn refresh_health_reprobes_after_a_source_is_fetched() {
    let root = test_root("health-refresh");
    let data_root = root.join("data");
    let page_path = root.join("cheat_sources.toml");
    let mut page = CheatSourcesPageState::load(page_path.clone(), Some(data_root.clone()));
    assert!(
        page.view()
            .rows
            .iter()
            .find(|row| row.id == "libretro-buildbot-cheats")
            .expect("libretro row")
            .health
            .as_ref()
            .is_none_or(|health| {
                health.state != archivefs_core::patch_manager::CheatProviderSourceState::Ready
            }),
        "nothing fetched yet, so libretro must not read ready"
    );

    // Simulate a fetch completing between visits.
    let source_root = data_root
        .join("cheat-sources")
        .join("libretro-buildbot-cheats");
    std::fs::create_dir_all(source_root.join("snapshots").join("abc123")).unwrap();
    std::fs::write(
        source_root.join("metadata.json"),
        r#"{
  "format_version": 1,
  "source_id": "libretro-buildbot-cheats",
  "current_snapshot": "abc123",
  "manifest": {
    "fetched_at_unix_seconds": 1000,
    "valid_cheat_count": 42
  },
  "last_fetch_succeeded": true,
  "last_error": null
}
"#,
    )
    .unwrap();

    page.apply(CheatSourcesPageAction::RefreshHealth);
    let health = page
        .view()
        .rows
        .iter()
        .find(|row| row.id == "libretro-buildbot-cheats")
        .expect("libretro row")
        .health
        .clone()
        .expect("health after refresh");
    assert_eq!(
        health.state,
        archivefs_core::patch_manager::CheatProviderSourceState::Ready
    );
    assert_eq!(health.entry_count, Some(42));
    assert!(
        !page.is_dirty(),
        "refreshing health must not dirty the page"
    );
}

#[test]
fn an_unreadable_existing_cache_is_drawn_as_invalid_not_not_checked() {
    let root = test_root("health-unreadable");
    let data_root = root.join("data");
    // A directory where metadata.json belongs makes reads fail
    // deterministically, including when the test runs as root.
    std::fs::create_dir_all(
        data_root
            .join("cheat-sources")
            .join("libretro-buildbot-cheats"),
    )
    .unwrap();
    std::fs::create_dir(
        data_root
            .join("cheat-sources")
            .join("libretro-buildbot-cheats")
            .join("metadata.json"),
    )
    .unwrap();

    let page = CheatSourcesPageState::load(root.join("cheat_sources.toml"), Some(data_root));
    let view = page.view();
    let libretro = view
        .rows
        .iter()
        .find(|row| row.id == "libretro-buildbot-cheats")
        .expect("libretro row");
    let health = libretro
        .health
        .as_ref()
        .expect("an unreadable existing cache must report a health, not None");
    assert_eq!(
        health.state,
        archivefs_core::patch_manager::CheatProviderSourceState::Invalid
    );
    assert!(
        health.last_error.as_ref().is_some(),
        "the invalid health must explain the read failure"
    );

    let output = render(&view);
    assert!(
        rendered_text_contains(&output, "Invalid"),
        "the page must draw the Invalid state, not 'Status: not checked'"
    );
}

#[test]
fn bsfree_is_labeled_with_its_honest_gamecube_capability() {
    let registry = archivefs_core::patch_manager::build_default_registry();
    let bsfree = registry.get("bsfree-archive").expect("bsfree entry");
    assert_eq!(
        super::provider_kind_label(bsfree),
        "Downloads and installs (GameCube via Dolphin)",
        "BSFree must state its GameCube install capability without claiming other formats install"
    );
    assert!(
        bsfree
            .spec
            .description
            .contains("GameCube hex-pair codes are installable"),
        "the row description must carry the same honest capability"
    );
}

#[test]
fn the_page_header_shows_its_icon_alongside_the_title() {
    let view = fresh("icon-header").view();
    let output = render(&view);
    assert!(rendered_text_contains(&output, "<3 x99"));
    assert!(rendered_text_contains(&output, "Cheat sources"));
}
