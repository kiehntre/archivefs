//! Source-card and operation-dispatch tests.
//!
//! Most assertions are on [`RommCardView`] and [`RommResultView`], because the
//! properties that matter are about *what is said*: that a token never appears,
//! that a sample is never presented as readiness, that a failed operation does not
//! erase counts that were true. Those are data questions.
//!
//! Two tests do render, headless, through the same helper the shared widgets use -
//! because "the token is not drawn" is a claim about drawing, and only drawing can
//! settle it.

use super::*;
use archivefs_core::identity_source::artwork::ArtworkCacheStats;
use archivefs_core::identity_source::model::{IdentityImportCounts, IdentityProvider};
use archivefs_core::identity_source::path_map::{PathMapping, ProviderPathKind};
use archivefs_core::identity_source::romm::config::RommSourceConfig;
use archivefs_core::identity_source::romm::import::{
    AdaptivePagination, ImportProgress, PageSizeReduction,
};
use archivefs_core::identity_source::status::{ProviderState, ProviderStatus};
use std::path::PathBuf;

/// A value that must never be drawn or formatted anywhere.
const SECRET: &str = "romm-token-value-that-must-never-be-drawn";

// --- Fixtures -------------------------------------------------------------

fn artwork_stats(items: usize, bytes: u64) -> ArtworkCacheStats {
    ArtworkCacheStats {
        items,
        bytes,
        maximum_bytes: 1024 * 1024 * 1024,
        last_cleanup_unix_seconds: None,
        directory: PathBuf::from("/home/user/.local/share/archivefs/identity/romm/artwork"),
        format_version: 1,
    }
}

/// The real counts from the completed import, so the card is exercised against
/// numbers it will actually be asked to show.
fn real_counts() -> IdentityImportCounts {
    IdentityImportCounts {
        total: 36_259,
        confirmed: 0,
        strong: 25_672,
        probable: 502,
        ambiguous: 0,
        stale: 10_081,
        unmatched: 4,
        with_hashes: 36_000,
        with_artwork: 29_759,
        multi_file: 198,
    }
}

fn config(enabled: bool, configured: bool) -> RommSourceConfig {
    RommSourceConfig {
        enabled,
        url: if configured {
            "http://172.19.0.20:8080".to_string()
        } else {
            String::new()
        },
        mappings: if configured {
            vec![PathMapping {
                provider_prefix: "roms".to_string(),
                archivefs_prefix: PathBuf::from("/mnt/games/roms"),
            }]
        } else {
            Vec::new()
        },
        provider_path_kind: ProviderPathKind::ProviderRelative,
        token_path: configured.then(|| PathBuf::from("/home/user/.config/archivefs/romm-token")),
    }
}

fn snapshot(state: ProviderState, enabled: bool, configured: bool) -> RommSnapshot {
    let ready = matches!(
        state,
        ProviderState::Ready | ProviderState::ReadyOffline | ProviderState::Stale { .. }
    );
    let mut status = ProviderStatus::not_configured(IdentityProvider::Romm);
    status.state = state;
    if ready {
        status.counts = real_counts();
        status.records_imported = 36_259;
        status.platforms_imported = 482;
        status.unknown_platforms = 13;
        status.invalid_hashes = 0;
        status.multi_file_groups = 6_613;
        status.cache_path = Some(PathBuf::from(
            "/home/user/.local/share/archivefs/identity/romm/identity-cache.json",
        ));
        status.cache_size_bytes = Some(52_574_568);
        status.last_successful_refresh_unix_seconds = Some(1_785_595_944);
        status.server_version = Some("5.1.0".to_string());
        status.server_id = Some("http://172.19.0.20:8080".to_string());
    }
    RommSnapshot {
        settings: ProviderSettings {
            source: config(enabled, configured),
            page_size: Some(100),
        },
        status,
        artwork: artwork_stats(39, 2_434_474),
        token_available: configured,
        token_problem: None,
        cache_format_version: ready.then_some(1),
    }
}

fn all_values(view: &RommCardView) -> String {
    let mut text = format!("{} {:?}", view.state_label, view.state_detail);
    for group in [
        &view.summary_rows,
        &view.verdict_rows,
        &view.quality_rows,
        &view.cache_rows,
        &view.artwork_rows,
    ] {
        for CardRow { label, value } in group {
            text.push_str(label);
            text.push(' ');
            text.push_str(value);
            text.push(' ');
        }
    }
    for (badge, _) in &view.badges {
        text.push_str(badge);
        text.push(' ');
    }
    for action in &view.actions {
        text.push_str(&action.label);
        text.push(' ');
        if let Some(reason) = &action.disabled_reason {
            text.push_str(reason);
            text.push(' ');
        }
    }
    if let Some(error) = &view.last_error {
        text.push_str(error);
    }
    text
}

fn value_of(view: &RommCardView, label: &str) -> Option<String> {
    [
        &view.summary_rows,
        &view.verdict_rows,
        &view.quality_rows,
        &view.cache_rows,
        &view.artwork_rows,
    ]
    .into_iter()
    .flatten()
    .find(|row| row.label == label)
    .map(|row| row.value.clone())
}

fn action_named(view: &RommCardView, prefix: &str) -> CardAction {
    view.actions
        .iter()
        .find(|action| action.label.starts_with(prefix))
        .unwrap_or_else(|| panic!("no action starting with {prefix:?}: {:?}", view.actions))
        .clone()
}

// --- Card states ----------------------------------------------------------

#[test]
fn before_the_first_status_load_the_card_admits_it_knows_nothing() {
    let view = build_card_view(None, None, false);
    assert!(view.busy, "it should read as working, not as empty");
    assert!(view.summary_rows.is_empty(), "no invented zeroes");
    assert!(view.verdict_rows.is_empty());
    assert!(
        view.actions.is_empty(),
        "nothing should be clickable before the state is known"
    );
}

#[test]
fn a_not_configured_source_says_what_to_do() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::NotConfigured, false, false)),
        None,
        false,
    );
    assert_eq!(view.state_label, "Not configured");
    assert!(
        view.state_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("token file")),
        "{:?}",
        view.state_detail
    );
    assert_eq!(value_of(&view, "URL").as_deref(), Some("not configured"));
    assert_eq!(
        value_of(&view, "Token file").as_deref(),
        Some("not configured")
    );
    // Configuring is exactly what an unconfigured source needs, so it stays open.
    assert!(
        action_named(&view, "Configure").enabled,
        "an unconfigured source must still be configurable"
    );
    // Browsing needs something cached to browse.
    for prefix in ["Browse records", "View conflicts", "View stale summary"] {
        let action = action_named(&view, prefix);
        assert!(!action.enabled, "{prefix} has nothing to show yet");
        assert!(
            action
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("nothing cached to browse")),
            "{prefix}: {:?}",
            action.disabled_reason
        );
    }
    // Nothing that talks to RomM can be started.
    for prefix in ["Test connection", "Import sample", "Import full"] {
        let action = action_named(&view, prefix);
        assert!(!action.enabled, "{prefix} should be disabled");
        assert!(action.disabled_reason.is_some(), "{prefix} needs a reason");
    }
    assert!(!view.offline_browsing);
}

#[test]
fn a_never_imported_source_is_not_presented_as_a_fault() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::NeverImported, true, true)),
        None,
        false,
    );
    assert_eq!(view.state_label, "Enabled, nothing imported yet");
    let detail = view.state_detail.clone().expect("a detail");
    assert!(detail.contains("not a fault"), "{detail}");
    assert!(!view.offline_browsing, "there is nothing to browse yet");
    // The primary action is a first import rather than a refresh.
    assert_eq!(
        action_named(&view, "Import full").label,
        "Import full catalogue"
    );
    assert!(action_named(&view, "Import full").enabled);
}

#[test]
fn a_disabled_source_keeps_its_counts_and_offers_enable() {
    let mut snapshot = snapshot(ProviderState::Disabled, false, true);
    snapshot.status.counts = real_counts();
    snapshot.status.records_imported = 36_259;
    let view = build_card_view(Some(&snapshot), None, false);
    assert_eq!(view.state_label, "Disabled");
    assert!(view.badges.iter().any(|(label, _)| label == "Disabled"));
    assert_eq!(value_of(&view, "Records").as_deref(), Some("36259"));
    let toggle = action_named(&view, "Enable");
    assert!(toggle.enabled);
    assert_eq!(toggle.operation, Some(RommOperation::SetEnabled(true)));
    // Importing needs the source on first.
    let sample = action_named(&view, "Import sample");
    assert!(!sample.enabled);
    assert!(
        sample
            .disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Enable the source")),
        "{:?}",
        sample.disabled_reason
    );
}

#[test]
fn a_ready_source_shows_the_real_counts() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::Ready, true, true)),
        None,
        false,
    );
    assert_eq!(view.state_label, "Ready");
    assert!(view.offline_browsing);
    // Exactly the numbers the real import produced.
    assert_eq!(value_of(&view, "Records").as_deref(), Some("36259"));
    assert_eq!(value_of(&view, "Platforms").as_deref(), Some("482"));
    assert_eq!(value_of(&view, "Confirmed").as_deref(), Some("0"));
    assert_eq!(value_of(&view, "Strong").as_deref(), Some("25672"));
    assert_eq!(value_of(&view, "Probable").as_deref(), Some("502"));
    assert_eq!(value_of(&view, "Ambiguous").as_deref(), Some("0"));
    assert_eq!(value_of(&view, "Stale").as_deref(), Some("10081"));
    assert_eq!(value_of(&view, "Unmatched").as_deref(), Some("4"));
    assert_eq!(value_of(&view, "Unknown platforms").as_deref(), Some("13"));
    assert_eq!(value_of(&view, "Invalid hashes").as_deref(), Some("0"));
    assert_eq!(
        value_of(&view, "Multi-file groups").as_deref(),
        Some("6613")
    );
    assert_eq!(value_of(&view, "Cache size").as_deref(), Some("50.1 MiB"));
    assert_eq!(value_of(&view, "Cache version").as_deref(), Some("1"));
    assert_eq!(value_of(&view, "RomM version").as_deref(), Some("5.1.0"));
    assert_eq!(
        value_of(&view, "Path shape").as_deref(),
        Some("provider-relative (e.g. roms/gb/game.gb)")
    );
    assert_eq!(value_of(&view, "Mappings").as_deref(), Some("1 configured"));
    // With a cache, the primary action refreshes rather than imports afresh.
    assert_eq!(action_named(&view, "Refresh").label, "Refresh");
    assert_eq!(
        action_named(&view, "Refresh").operation,
        Some(RommOperation::Refresh)
    );
}

#[test]
fn a_ready_offline_source_says_that_is_working_as_intended() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::ReadyOffline, true, true)),
        None,
        false,
    );
    assert_eq!(view.state_label, "Ready (offline)");
    assert!(view.offline_browsing);
    let detail = view.state_detail.clone().expect("a detail");
    assert!(detail.contains("working as intended"), "{detail}");
    assert!(
        view.badges
            .iter()
            .any(|(label, _)| label == "Browsable offline")
    );
    // Counts are still real while offline.
    assert_eq!(value_of(&view, "Records").as_deref(), Some("36259"));
}

#[test]
fn a_stale_source_carries_the_reason_it_is_stale() {
    let view = build_card_view(
        Some(&snapshot(
            ProviderState::Stale {
                detail: "10081 of 36259 records point at files that are missing".to_string(),
            },
            true,
            true,
        )),
        None,
        false,
    );
    assert_eq!(view.state_label, "Stale");
    assert!(
        view.state_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("10081")),
        "{:?}",
        view.state_detail
    );
    assert!(view.offline_browsing, "stale identity is still browsable");
}

#[test]
fn an_importing_state_shows_as_busy_and_cancellable() {
    let snapshot = snapshot(ProviderState::Importing, true, true);
    let view = build_card_view(Some(&snapshot), Some(&RommOperation::FullImport), false);
    assert!(view.busy);
    assert_eq!(
        view.busy_label.as_deref(),
        Some("Importing the RomM catalogue")
    );
    assert!(view.cancellable);
    assert!(action_named(&view, "Cancel").enabled);
    // Everything else is refused while it runs.
    for prefix in [
        "Test connection",
        "Import sample",
        "Refresh",
        "Enable",
        "Disable",
    ] {
        if let Some(action) = view.actions.iter().find(|a| a.label.starts_with(prefix)) {
            assert!(!action.enabled, "{prefix} should be disabled while busy");
        }
    }
}

#[test]
fn an_error_state_shows_the_reason_without_erasing_what_was_known() {
    let mut snapshot = snapshot(ProviderState::Ready, true, true);
    snapshot.status.state = ProviderState::Error {
        detail: "could not reach RomM: an I/O error occurred (connection refused)".to_string(),
    };
    snapshot.status.last_error =
        Some("could not reach RomM: an I/O error occurred (connection refused)".to_string());
    let view = build_card_view(Some(&snapshot), None, false);
    assert_eq!(view.state_label, "Error");
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|error| error.contains("connection refused"))
    );
    // The counts that were true are still shown.
    assert_eq!(value_of(&view, "Records").as_deref(), Some("36259"));
    assert_eq!(value_of(&view, "Strong").as_deref(), Some("25672"));
}

#[test]
fn a_cancellation_already_requested_stops_offering_cancel() {
    let snapshot = snapshot(ProviderState::Importing, true, true);
    let view = build_card_view(Some(&snapshot), Some(&RommOperation::FullImport), true);
    assert!(!view.cancellable, "asking twice should not be possible");
    assert!(!action_named(&view, "Cancel").enabled);
}

#[test]
fn the_artwork_cache_counts_are_shown_with_the_limit() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::Ready, true, true)),
        None,
        false,
    );
    assert_eq!(
        value_of(&view, "Thumbnails").as_deref(),
        Some("39 cached, 2.3 MiB")
    );
    assert_eq!(
        value_of(&view, "Thumbnail limit").as_deref(),
        Some("1 GiB (least-recently-used eviction)")
    );
    assert_eq!(value_of(&view, "Last cleanup").as_deref(), Some("never"));
    assert!(action_named(&view, "Clear cover thumbnails").enabled);
}

#[test]
fn clearing_thumbnails_is_refused_when_there_are_none() {
    let mut snapshot = snapshot(ProviderState::Ready, true, true);
    snapshot.artwork = artwork_stats(0, 0);
    let view = build_card_view(Some(&snapshot), None, false);
    let clear = action_named(&view, "Clear cover thumbnails");
    assert!(!clear.enabled);
    assert!(
        clear
            .disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no cached thumbnails")),
        "{:?}",
        clear.disabled_reason
    );
}

#[test]
fn unfinished_actions_are_present_disabled_and_honestly_labelled() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::Ready, true, true)),
        None,
        false,
    );
    // Slice 3 finished the browse actions, so nothing on the card is deferred any
    // more. Each of these opens a panel rather than starting an operation, which is
    // why it dispatches no operation of its own.
    assert!(
        view.actions.iter().all(|action| !action.coming_next),
        "no action should still be labelled as coming next: {:?}",
        view.actions
            .iter()
            .filter(|action| action.coming_next)
            .map(|action| action.label.clone())
            .collect::<Vec<_>>()
    );
    for prefix in [
        "Configure",
        "Browse records",
        "View conflicts",
        "View stale summary",
    ] {
        let action = action_named(&view, prefix);
        assert!(action.enabled, "{prefix} should be available with a cache");
        assert!(action.operation.is_none(), "{prefix} opens a panel");
        assert!(!action.label.contains("coming next"), "{}", action.label);
    }
}

// --- Secrecy --------------------------------------------------------------

#[test]
fn the_card_shows_the_token_path_and_never_the_token() {
    let mut snapshot = snapshot(ProviderState::Ready, true, true);
    // Nothing in the snapshot carries the token - but if a future change put it in
    // a field, this is what would catch it.
    snapshot.token_problem = Some("the token file is readable by others (mode 0644)".to_string());
    snapshot.token_available = false;
    let view = build_card_view(Some(&snapshot), None, false);
    let text = all_values(&view);
    assert!(
        text.contains("/home/user/.config/archivefs/romm-token"),
        "the path is what a person needs to see"
    );
    assert!(!text.contains(SECRET), "the token must never be rendered");
    assert!(!text.to_lowercase().contains("bearer"), "{text}");
    assert!(
        text.contains("readable by others"),
        "the redacted problem should still be explained"
    );
}

#[test]
fn a_long_provider_error_is_shown_as_the_core_redacted_it() {
    let mut snapshot = snapshot(ProviderState::Ready, true, true);
    // What a core refusal looks like: long, specific, and free of the credential.
    let refusal = "the token was rejected (401). Check that the client token carries \
                   platforms.read and roms.read and has not expired.";
    snapshot.status.last_error = Some(refusal.to_string());
    let view = build_card_view(Some(&snapshot), None, false);
    let text = all_values(&view);
    assert!(text.contains("401"), "the status is useful");
    assert!(text.contains("platforms.read"), "the remedy is useful");
    assert!(!text.contains(SECRET));
    assert!(!text.to_lowercase().contains("authorization"));
}

#[test]
fn the_rendered_card_draws_no_token() {
    // The data assertions above are necessary but not sufficient: this is the one
    // that settles whether drawing leaks it.
    let mut snapshot = snapshot(ProviderState::Ready, true, true);
    snapshot.token_problem = Some("the file at that path is not usable".to_string());
    let view = build_card_view(Some(&snapshot), None, false);
    let mut state = RommCardState::default();
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_romm_source_card(ui, &view, &mut state, None);
        });
    });
    assert!(
        rendered_text_contains(&output, "RomM"),
        "the card should have drawn"
    );
    assert!(
        !rendered_text_contains(&output, SECRET),
        "the token was drawn"
    );
    assert!(!rendered_text_contains(&output, "Bearer"));
}

#[test]
fn the_rendered_card_draws_the_real_counts() {
    let view = build_card_view(
        Some(&snapshot(ProviderState::Ready, true, true)),
        None,
        false,
    );
    let mut state = RommCardState {
        show_verdicts: true,
        show_quality: true,
        ..RommCardState::default()
    };
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_romm_source_card(ui, &view, &mut state, None);
        });
    });
    for expected in ["36259", "State: Ready", "http://172.19.0.20:8080"] {
        assert!(
            rendered_text_contains(&output, expected),
            "the card did not draw {expected:?}"
        );
    }
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

// --- Operation taxonomy, which the dispatch relies on ---------------------

#[test]
fn only_a_status_load_is_non_mutating_and_only_imports_report_progress() {
    assert!(!RommOperation::LoadStatus.is_mutating());
    for operation in [
        RommOperation::TestConnection,
        RommOperation::SetEnabled(true),
        RommOperation::SampleImport { records: 25 },
        RommOperation::FullImport,
        RommOperation::Refresh,
        RommOperation::ClearArtwork,
    ] {
        assert!(operation.is_mutating(), "{operation:?}");
    }
    // Only the paginated walks can report progress, which is what decides whether
    // Cancel is offered.
    for operation in [
        RommOperation::SampleImport { records: 25 },
        RommOperation::FullImport,
        RommOperation::Refresh,
    ] {
        assert!(operation.reports_progress(), "{operation:?}");
        assert!(operation.uses_network(), "{operation:?}");
    }
    for operation in [
        RommOperation::LoadStatus,
        RommOperation::SetEnabled(false),
        RommOperation::ClearArtwork,
    ] {
        assert!(!operation.reports_progress(), "{operation:?}");
        assert!(!operation.uses_network(), "{operation:?}");
    }
    // A connection test talks to RomM but has no pages to report.
    assert!(RommOperation::TestConnection.uses_network());
    assert!(!RommOperation::TestConnection.reports_progress());
}

// --- Progress -------------------------------------------------------------

#[test]
fn progress_turns_an_adaptive_reduction_into_a_sentence() {
    let mut progress = RommProgress::default();
    progress.absorb(ImportProgress {
        pages_fetched: 44,
        records_fetched: 4_400,
        reported_total: Some(36_259),
        page_size: 50,
        reduction: Some(PageSizeReduction {
            offset: 4_400,
            from: 100,
            to: 50,
            ceiling_bytes: 8 * 1024 * 1024,
        }),
    });
    assert_eq!(progress.pages_fetched, 44);
    assert_eq!(progress.records_fetched, 4_400);
    assert_eq!(progress.page_size, 50);
    let note = progress.notes.first().expect("a note");
    assert!(note.contains("offset 4400"), "{note}");
    assert!(
        note.contains("smaller page size"),
        "it should explain what happens next: {note}"
    );
    assert!(
        note.contains("Retrying the same records"),
        "and that nothing is skipped: {note}"
    );
    let fraction = progress.fraction().expect("a fraction");
    assert!((0.11..0.13).contains(&fraction), "{fraction}");
}

#[test]
fn progress_notes_are_bounded_and_do_not_repeat() {
    let mut progress = RommProgress::default();
    for index in 0..MAX_PROGRESS_NOTES * 3 {
        progress.note(format!("note {index}"));
    }
    assert_eq!(progress.notes.len(), MAX_PROGRESS_NOTES);
    // The newest survive.
    assert!(
        progress
            .notes
            .last()
            .is_some_and(|note| note.contains(&(MAX_PROGRESS_NOTES * 3 - 1).to_string()))
    );
    // The same note twice in a row is not repeated.
    let before = progress.notes.len();
    let last = progress.notes.last().cloned().expect("a note");
    progress.note(last);
    assert_eq!(progress.notes.len(), before);
}

#[test]
fn a_total_that_cannot_be_a_total_yields_no_fraction() {
    let mut progress = RommProgress::default();
    progress.absorb(ImportProgress {
        pages_fetched: 1,
        records_fetched: 500,
        reported_total: Some(10),
        page_size: 100,
        reduction: None,
    });
    assert!(
        progress.fraction().is_none(),
        "an impossible total must not become a made-up percentage"
    );
}

// --- Result presentation --------------------------------------------------

fn import_summary(published: bool, records: usize) -> RommImportSummary {
    RommImportSummary {
        published,
        cache_path: published.then(|| PathBuf::from("/data/identity/romm/identity-cache.json")),
        cache_bytes: published.then_some(52_574_568),
        records,
        platforms: 482,
        confirmed: 0,
        strong: 21,
        probable: 3,
        ambiguous: 0,
        stale: 1,
        unmatched: 0,
        unknown_platforms: 0,
        invalid_hashes: 0,
        multi_file_groups: 5,
        pages_fetched: 1,
        elapsed_milliseconds: 612,
        adaptive: None,
        failure: None,
        failure_code: None,
        previous_cache_usable: true,
    }
}

#[test]
fn a_sample_result_says_plainly_that_nothing_was_published() {
    let summary = import_summary(false, 25);
    let view = build_result_view(
        &RommOperation::SampleImport { records: 25 },
        Ok(&RommOperationOutcome::Sample(Box::new(summary))),
    );
    assert!(view.succeeded);
    assert!(
        view.headline.contains("nothing published"),
        "{}",
        view.headline
    );
    let notes = view.notes.join(" ");
    assert!(notes.contains("preview"), "{notes}");
    assert!(
        notes.contains("exactly as it was"),
        "it must say the existing identity is untouched: {notes}"
    );
    assert!(
        notes.contains("state has not changed"),
        "and that this is not readiness: {notes}"
    );
    assert!(
        !notes.to_lowercase().contains("published atomically"),
        "a sample publishes nothing: {notes}"
    );
    // The verdict counts are still reported.
    let rows: Vec<String> = view.rows.iter().map(|row| row.label.clone()).collect();
    for label in ["Records", "Strong", "Probable", "Stale", "Unmatched"] {
        assert!(rows.contains(&label.to_string()), "{label} missing");
    }
    assert!(
        !rows.contains(&"Published to".to_string()),
        "a sample has no cache path"
    );
}

#[test]
fn a_published_import_states_atomic_publication_and_the_cache_path() {
    let view = build_result_view(
        &RommOperation::FullImport,
        Ok(&RommOperationOutcome::Import(Box::new(import_summary(
            true, 36_259,
        )))),
    );
    assert!(view.succeeded);
    assert!(view.headline.contains("36259"), "{}", view.headline);
    let notes = view.notes.join(" ");
    assert!(notes.contains("Published atomically"), "{notes}");
    assert!(
        notes.contains("never saw a half-written cache"),
        "the guarantee is the point: {notes}"
    );
    let labels: Vec<String> = view.rows.iter().map(|row| row.label.clone()).collect();
    assert!(labels.contains(&"Published to".to_string()));
    assert!(labels.contains(&"Cache size".to_string()));
}

#[test]
fn an_import_that_adapted_explains_it_in_words() {
    let mut summary = import_summary(true, 36_259);
    summary.adaptive = Some(AdaptivePagination {
        configured_page_size: 100,
        effective_page_size: 100,
        smallest_page_size: 1,
        reductions: 5,
        oversized_retries: 6,
        recoveries: 5,
        records_without_file_detail: vec!["43030".to_string()],
    });
    let view = build_result_view(
        &RommOperation::FullImport,
        Ok(&RommOperationOutcome::Import(Box::new(summary))),
    );
    let notes = view.notes.join(" ");
    assert!(notes.contains("stepped down from 100 to 1"), "{notes}");
    assert!(notes.contains("recovered 5 time(s)"), "{notes}");
    assert!(
        notes.contains("nothing was skipped"),
        "the reassurance matters: {notes}"
    );
    // The omitted file list is named, with the record it happened to.
    assert!(
        notes.contains("43030"),
        "the record should be named: {notes}"
    );
    assert!(
        notes.contains("detailed file list was \n             omitted")
            || notes.contains("file list was omitted")
            || notes.contains("omitted"),
        "{notes}"
    );
}

#[test]
fn a_failed_import_says_whether_the_old_cache_survived() {
    let mut summary = import_summary(false, 0);
    summary.failure = Some("could not reach RomM: connection refused".to_string());
    summary.failure_code = Some("transport".to_string());
    summary.previous_cache_usable = true;
    let view = build_result_view(
        &RommOperation::Refresh,
        Ok(&RommOperationOutcome::Import(Box::new(summary.clone()))),
    );
    assert!(!view.succeeded);
    let notes = view.notes.join(" ");
    assert!(notes.contains("untouched and still browsable"), "{notes}");
    assert!(
        view.rows
            .iter()
            .any(|row| row.value.contains("connection refused")),
        "{:?}",
        view.rows
    );

    // And the first-ever failure says the opposite, rather than implying a loss.
    summary.previous_cache_usable = false;
    let first = build_result_view(
        &RommOperation::FullImport,
        Ok(&RommOperationOutcome::Import(Box::new(summary))),
    );
    assert!(
        first.notes.join(" ").contains("no previous cache to lose"),
        "{:?}",
        first.notes
    );
}

#[test]
fn a_connection_result_reports_capability_and_says_it_changed_nothing() {
    let summary = RommConnectionSummary {
        server_id: "http://172.19.0.20:8080".to_string(),
        romm_version: Some("5.1.0".to_string()),
        api_version: Some("5.1.0".to_string()),
        version_supported: true,
        endpoints: vec!["/api/platforms".to_string(), "/api/roms".to_string()],
        missing_endpoints: Vec::new(),
        read_scopes: vec!["platforms.read".to_string(), "roms.read".to_string()],
        authenticated_reads: vec![
            ("/api/platforms".to_string(), true),
            ("/api/roms".to_string(), true),
        ],
        supports_pagination: true,
        hash_fields: vec!["md5_hash".to_string()],
        artwork_fields: vec!["url_cover".to_string()],
        exposes_file_list: true,
        supports_client_tokens: true,
        configured_path_kind: "relative".to_string(),
        observed_path_kind: Some("relative".to_string()),
        path_kind_agrees: true,
        can_import: true,
        blocking_reason: None,
    };
    let view = build_result_view(
        &RommOperation::TestConnection,
        Ok(&RommOperationOutcome::Connection(Box::new(summary))),
    );
    assert!(view.succeeded);
    let labels: Vec<String> = view.rows.iter().map(|row| row.label.clone()).collect();
    for expected in [
        "RomM version",
        "API version",
        "Endpoints",
        "Read scopes",
        "Pagination",
        "Hash fields",
        "Artwork fields",
        "Multi-file detail",
        "Client tokens",
        "Path shape",
        "Read /api/platforms",
        "Read /api/roms",
    ] {
        assert!(labels.contains(&expected.to_string()), "{expected} missing");
    }
    assert!(
        view.notes
            .join(" ")
            .contains("Nothing was imported, cached or changed in RomM")
    );
}

#[test]
fn a_path_shape_mismatch_is_called_out_in_the_connection_result() {
    let summary = RommConnectionSummary {
        configured_path_kind: "absolute".to_string(),
        observed_path_kind: Some("relative".to_string()),
        path_kind_agrees: false,
        can_import: true,
        ..RommConnectionSummary::default()
    };
    let view = build_result_view(
        &RommOperation::TestConnection,
        Ok(&RommOperationOutcome::Connection(Box::new(summary))),
    );
    let notes = view.notes.join(" ");
    assert!(notes.contains("reports relative paths"), "{notes}");
    assert!(notes.contains("set to absolute"), "{notes}");
    assert!(
        notes.contains("stay unmatched"),
        "the consequence should be stated: {notes}"
    );
}

#[test]
fn an_artwork_clear_result_says_what_it_did_not_touch() {
    let view = build_result_view(
        &RommOperation::ClearArtwork,
        Ok(&RommOperationOutcome::ArtworkCleared {
            items: 39,
            bytes: 2_434_474,
        }),
    );
    assert!(view.succeeded);
    assert!(view.headline.contains("39"), "{}", view.headline);
    let notes = view.notes.join(" ");
    assert!(notes.contains("imported identity"), "{notes}");
    assert!(notes.contains("ROM files were not touched"), "{notes}");
    assert!(
        view.rows.iter().any(|row| row.value.contains("MiB")),
        "the reclaimed space should be readable: {:?}",
        view.rows
    );
}

#[test]
fn a_failed_operation_is_reported_with_the_cores_own_redacted_message() {
    let view = build_result_view(
        &RommOperation::TestConnection,
        Err("the token file is readable by others (mode 0644); run chmod 600"),
    );
    assert!(!view.succeeded);
    assert!(view.headline.contains("failed"), "{}", view.headline);
    let text = format!("{:?} {:?}", view.rows, view.notes);
    assert!(
        text.contains("chmod 600"),
        "the remedy should survive: {text}"
    );
    assert!(!text.contains(SECRET));
}

#[test]
fn enabling_says_that_nothing_was_contacted() {
    let view = build_result_view(
        &RommOperation::SetEnabled(true),
        Ok(&RommOperationOutcome::Enabled(true)),
    );
    assert!(view.succeeded);
    assert!(view.headline.contains("enabled"));
    assert!(
        view.notes.join(" ").contains("Nothing was contacted"),
        "{:?}",
        view.notes
    );

    let disabled = build_result_view(
        &RommOperation::SetEnabled(false),
        Ok(&RommOperationOutcome::Enabled(false)),
    );
    assert!(
        disabled.notes.join(" ").contains("identity are kept"),
        "{:?}",
        disabled.notes
    );
}

// --- Byte formatting ------------------------------------------------------

#[test]
fn bytes_are_formatted_readably() {
    assert_eq!(human_bytes(0), "0 bytes");
    assert_eq!(human_bytes(512), "512 bytes");
    assert_eq!(human_bytes(2048), "2.0 KiB");
    assert_eq!(human_bytes(52_574_568), "50.1 MiB");
    assert_eq!(human_bytes(1024 * 1024 * 1024), "1 GiB");
    assert_eq!(human_bytes(1024 * 1024 * 1024 * 3 / 2), "1.5 GiB");
}
