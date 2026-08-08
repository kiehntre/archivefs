use super::*;
use archivefs_core::SetupDiagnosticStatus;

fn passing_check(name: &str) -> SetupDiagnostic {
    SetupDiagnostic {
        name: name.to_string(),
        status: SetupDiagnosticStatus::Ready,
        detail: String::new(),
        why_it_matters: String::new(),
        next_step: String::new(),
    }
}

fn not_configured_check(name: &str) -> SetupDiagnostic {
    SetupDiagnostic {
        name: name.to_string(),
        status: SetupDiagnosticStatus::NotConfigured,
        detail: String::new(),
        why_it_matters: String::new(),
        next_step: String::new(),
    }
}

fn error_check(name: &str) -> SetupDiagnostic {
    SetupDiagnostic {
        name: name.to_string(),
        status: SetupDiagnosticStatus::Error,
        detail: "something is broken".to_string(),
        why_it_matters: String::new(),
        next_step: String::new(),
    }
}

fn warning_check(name: &str) -> SetupDiagnostic {
    SetupDiagnostic {
        name: name.to_string(),
        status: SetupDiagnosticStatus::Warning,
        detail: "something looks off".to_string(),
        why_it_matters: String::new(),
        next_step: String::new(),
    }
}

fn fresh_install_inputs(checks: &[SetupDiagnostic]) -> HomeInputs<'_> {
    HomeInputs {
        source_folder_count: 0,
        has_database: false,
        diagnostics: Some(checks),
        config_missing: true,
        first_run: true,
        cheat_sources_enabled_count: None,
        dat_sources_registered_count: None,
        romm_state_label: None,
    }
}

fn established_inputs(checks: &[SetupDiagnostic]) -> HomeInputs<'_> {
    HomeInputs {
        source_folder_count: 3,
        has_database: true,
        diagnostics: Some(checks),
        config_missing: false,
        first_run: false,
        cheat_sources_enabled_count: Some(5),
        dat_sources_registered_count: Some(2),
        romm_state_label: Some(RommReadinessLabel::Ready("Ready")),
    }
}

fn rendered_text_contains(output: &egui::FullOutput, needle: &str) -> bool {
    fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Text(text_shape) => text_shape.galley.text().contains(needle),
            egui::Shape::Vec(nested) => nested.iter().any(|s| shape_contains(s, needle)),
            _ => false,
        }
    }
    output
        .shapes
        .iter()
        .any(|clipped| shape_contains(&clipped.shape, needle))
}

fn render(view: &HomeView, width: f32) -> (egui::FullOutput, Option<HomeCard>) {
    let ctx = egui::Context::default();
    let mut clicked = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 2000.0),
            )),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                clicked = show_home_page(ui, view);
            });
        },
    );
    (output, clicked)
}

// --- Fresh / empty HOME rendering ---

#[test]
fn fresh_install_shows_the_welcome_banner_and_not_configured_cards() {
    let checks = [not_configured_check("config file")];
    let view = build_home_view(&fresh_install_inputs(&checks));
    assert_eq!(view.banner, HomeBanner::FreshInstall);
    for card in &view.cards {
        if let Some(readiness) = &card.readiness {
            assert!(
                !matches!(readiness, CardReadiness::Ready(_)),
                "{:?} claimed Ready on a fresh install: {readiness:?}",
                card.card
            );
        }
    }
    let (output, _) = render(&view, 1100.0);
    assert!(rendered_text_contains(&output, "Welcome to ArchiveFS"));
    assert!(rendered_text_contains(&output, "No source folders yet"));
}

// --- Established configuration rendering ---

#[test]
fn established_install_shows_no_banner_and_ready_cards() {
    let checks = [passing_check("config file"), passing_check("mount root")];
    let view = build_home_view(&established_inputs(&checks));
    assert_eq!(view.banner, HomeBanner::None);
    let library = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::BuildLibrary)
        .unwrap();
    assert!(matches!(library.readiness, Some(CardReadiness::Ready(_))));
    let (output, _) = render(&view, 1100.0);
    assert!(rendered_text_contains(
        &output,
        "3 source folders configured"
    ));
    assert!(rendered_text_contains(&output, "All checks passed"));
}

// --- Disappeared-config warning behaviour ---

#[test]
fn config_disappeared_after_being_confirmed_shows_the_warning_banner_not_the_welcome_one() {
    let checks = [not_configured_check("config file")];
    let mut inputs = fresh_install_inputs(&checks);
    inputs.first_run = false; // previously confirmed, now gone
    let view = build_home_view(&inputs);
    assert_eq!(view.banner, HomeBanner::ConfigDisappeared);
    let (output, _) = render(&view, 1100.0);
    assert!(rendered_text_contains(
        &output,
        "Configuration file is no longer found"
    ));
    assert!(!rendered_text_contains(&output, "Welcome to ArchiveFS"));
}

// --- Purity: identical inputs produce byte-identical (structurally equal) views ---

#[test]
fn the_same_inputs_always_produce_the_same_view() {
    let checks = [passing_check("config file")];
    let inputs = established_inputs(&checks);
    let first = build_home_view(&inputs);
    let second = build_home_view(&inputs);
    assert_eq!(first, second);
}

// --- Every card navigates to the correct destination ---

#[test]
fn every_primary_card_action_reports_its_own_card() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    let expected = [
        (HomeCard::BuildLibrary, "Open Sources"),
        (HomeCard::BrowseGames, "Open Library"),
        (HomeCard::CheatsAndMods, "Open Cheats & Mods"),
        (
            HomeCard::CanonicalOrganisation,
            "Open Canonical Organisation",
        ),
        (HomeCard::CleanUpLibrary, "Open DAT Sources"),
        (HomeCard::DatSources, "Open DAT Sources"),
        (HomeCard::RomM, "Open Sources"),
        (HomeCard::CheckSetup, "Open Doctor"),
        (HomeCard::Settings, "Open Settings"),
    ];
    assert_eq!(view.cards.len(), expected.len());
    for (card, (expected_card, expected_label)) in view.cards.iter().zip(expected.iter()) {
        assert_eq!(card.card, *expected_card);
        assert_eq!(card.action_label, *expected_label);
    }
}

#[test]
fn cheat_sources_secondary_link_is_present_and_distinct_from_the_primary_action() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    let cheats = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::CheatsAndMods)
        .unwrap();
    assert_eq!(
        cheats.secondary,
        Some((HomeCard::CheatSources, "Manage Cheat Sources"))
    );
}

// --- Unavailable tasks use honest wording ---

#[test]
fn a_romm_error_state_is_reported_as_unavailable_never_as_ready_or_not_configured() {
    let checks = [passing_check("config file")];
    let mut inputs = established_inputs(&checks);
    inputs.romm_state_label = Some(RommReadinessLabel::Unavailable("Error"));
    let view = build_home_view(&inputs);
    let romm = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::RomM)
        .unwrap();
    assert!(matches!(
        romm.readiness,
        Some(CardReadiness::Unavailable(_))
    ));
    assert_ne!(romm.readiness.as_ref().unwrap().label(), "Ready");
}

#[test]
fn setup_checks_with_errors_are_reported_as_unavailable() {
    let checks = [error_check("mount root"), warning_check("ratarmount")];
    let view = build_home_view(&established_inputs(&checks));
    let setup = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::CheckSetup)
        .unwrap();
    assert!(matches!(
        setup.readiness,
        Some(CardReadiness::Unavailable(_))
    ));
    assert_eq!(
        setup.readiness.as_ref().unwrap().label(),
        "1 check needs attention"
    );
}

#[test]
fn lazily_loaded_pages_not_yet_visited_are_reported_as_unknown_not_not_configured() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&fresh_install_inputs(&checks));
    for card_kind in [
        HomeCard::CheatsAndMods,
        HomeCard::DatSources,
        HomeCard::RomM,
    ] {
        let card = view.cards.iter().find(|c| c.card == card_kind).unwrap();
        assert!(
            matches!(card.readiness, Some(CardReadiness::Unknown(_))),
            "{card_kind:?} should be Unknown before its page has been visited, got {:?}",
            card.readiness
        );
    }
}

// --- No filesystem writes or network requests merely from rendering ---
// `build_home_view` and `show_home_page` take no I/O-capable handle (no
// `Path`, no client, no thread spawn) - the type signatures themselves are
// the guarantee. This test only pins the observable behaviour: rendering
// twice from identical inputs never changes the view, i.e. rendering has no
// side effect that could feed back into a later `build_home_view` call.
#[test]
fn rendering_does_not_mutate_anything_observable_by_a_later_build() {
    let checks = [passing_check("config file")];
    let inputs = established_inputs(&checks);
    let view = build_home_view(&inputs);
    let _ = render(&view, 1100.0);
    let view_after = build_home_view(&inputs);
    assert_eq!(view, view_after);
}

// --- Minimum-size headless render ---

#[test]
fn renders_without_panicking_at_a_narrow_compact_width() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    let (output, _) = render(&view, 700.0);
    assert!(rendered_text_contains(&output, "Home"));
    assert!(rendered_text_contains(&output, "Build my library"));
}

// --- Keyboard focus order ---

#[test]
fn tabbing_through_the_page_visits_cards_top_to_bottom() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    let ctx = egui::Context::default();
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 3000.0),
            )),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = show_home_page(ui, &view);
            });
        },
    );
    let mut last_top: Option<f32> = None;
    for _ in 0..view.cards.len() {
        let _output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 3000.0),
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
                    let _ = show_home_page(ui, &view);
                });
            },
        );
        if let Some(id) = ctx.memory(|memory| memory.focused())
            && let Some(response) = ctx.read_response(id)
        {
            let top = response.rect.top();
            if let Some(previous) = last_top {
                assert!(
                    top >= previous,
                    "focus moved upward ({top} came after {previous}); tab order is not top-to-bottom"
                );
            }
            last_top = Some(top);
        }
    }
    assert!(last_top.is_some(), "Tab never focused anything on Home");
}

#[test]
fn rename_and_organise_are_discoverable_from_home() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    let organise = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::CanonicalOrganisation)
        .unwrap();
    // "Organise by platform" is the primary action; "Review filename
    // suggestions" (the DAT rename-planning workflow) is the secondary route.
    assert_eq!(organise.action_label, "Open Canonical Organisation");
    assert_eq!(
        organise.secondary,
        Some((HomeCard::CleanUpLibrary, "Review filename suggestions"))
    );
    let clean = view
        .cards
        .iter()
        .find(|c| c.card == HomeCard::CleanUpLibrary)
        .unwrap();
    assert_eq!(clean.title, "Clean up my library");
    assert_eq!(clean.action_label, "Open DAT Sources");
}

#[test]
fn every_home_card_shows_its_icon_alongside_its_title() {
    let checks = [passing_check("config file")];
    let view = build_home_view(&established_inputs(&checks));
    for card in &view.cards {
        assert!(!card.icon.is_empty(), "{} must carry an icon", card.title);
    }
    // The icons are drawn next to the text labels - the labels never become
    // icon-only.
    let (output, _) = render(&view, 1200.0);
    for expected in [
        "🗂️",
        "🧹",
        "🎮",
        "📚",
        "🩺",
        "⚙️",
        "Clean up my library",
        "Organise my library",
        "Browse my games",
    ] {
        assert!(
            rendered_text_contains(&output, expected),
            "expected {expected:?} to be drawn"
        );
    }
}
