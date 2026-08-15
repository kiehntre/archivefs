//! Data and render tests for the Repair Review page.
//!
//! The view model ([`super::build_rows`], [`super::summary_line`]) is a pure
//! function of a [`LibraryRepairPlan`] and the filter, so what the page says
//! is checkable without a frame buffer. Drawing is exercised once through a
//! headless egui context. No real plan, ROM, or DAT file is opened; every
//! fixture is constructed in memory or written to a per-test temp directory.

use std::path::PathBuf;
use std::rc::Rc;

use archivefs_core::repair::library::{
    LibraryRepairPlan, LibraryRepairReport, PlanItem, ReportCounts,
};
use archivefs_core::repair::plan::{RepairPlan, RepairPlanId};
use archivefs_core::repair::proposal::{
    RepairAction, RepairEvidence, RepairEvidenceKind, RepairProposal, RepairProposalId, SafetyState,
};

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A per-test temp directory under the system temp dir, removed on drop.
/// Mirrors the project's GUI test pattern (no `tempfile` dependency).
struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gui-repair-review-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        Self(root)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn make_proposal(index: usize) -> RepairProposal {
    RepairProposal {
        id: RepairProposalId::new(format!("p{index}")).unwrap(),
        action: RepairAction::RenamePath {
            destination: PathBuf::from(format!("/roms/sms/Game {index} (USA).sms")),
        },
        source_path: PathBuf::from(format!(
            "/roms/sms/Game {index} (USA, Europe, Brazil) (En).sms"
        )),
        reason: format!("verified DAT match: Game {index} (USA)"),
        evidence: vec![RepairEvidence::new(
            RepairEvidenceKind::CanonicalDatName,
            "canonical DAT name",
        )],
        expected_source_identity: None,
        originating_audit: None,
        safety: SafetyState::Safe,
        blockers: Vec::new(),
        warnings: Vec::new(),
        dat_source_id: Some("sms".to_string()),
        dat_source_display: Some("Sega - Master System - Mark III".to_string()),
        game_name: Some(format!("Game {index} (USA, Europe, Brazil) (En)")),
        rom_name: Some(format!("Game {index} (USA, Europe, Brazil) (En).sms")),
        verdict_label: Some("Exact".to_string()),
        match_confident: true,
        is_outer_archive: false,
        is_outer_archive_verified: false,
    }
}

/// The SMS acceptance fixture: exactly 50 DAT candidates / 24 canonical /
/// 26 safe / 0 needs review / 0 blocked / 0 unmatched / 151 ancillary, with
/// no non-executable rows.
fn fixture_plan() -> LibraryRepairPlan {
    let proposals: Vec<RepairProposal> = (0..26).map(make_proposal).collect();
    fixture_plan_with(proposals, Vec::new(), Vec::new(), 0, 0)
}

/// A mixed fixture with one NeedsReview row and one Blocked row, for filter
/// and ordering tests.
fn fixture_plan_mixed() -> LibraryRepairPlan {
    let proposals: Vec<RepairProposal> = (0..26).map(make_proposal).collect();
    let needs_review = vec![PlanItem {
        path: "/roms/sms/Ambiguous (USA).zip".to_string(),
        reason: "ambiguous DAT attribution".to_string(),
    }];
    let blocked = vec![PlanItem {
        path: "/roms/sms/Blocked (USA).zip".to_string(),
        reason: "blocked by a rename-plan conflict".to_string(),
    }];
    fixture_plan_with(proposals, needs_review, blocked, 1, 1)
}

fn fixture_plan_with(
    proposals: Vec<RepairProposal>,
    needs_review: Vec<PlanItem>,
    blocked: Vec<PlanItem>,
    needs_review_count: usize,
    blocked_count: usize,
) -> LibraryRepairPlan {
    let repair_plan = RepairPlan {
        id: RepairPlanId::new("fixture").unwrap(),
        generation: 1,
        created_at_unix: 10,
        source_scan_id: Some("/roms/sms".to_string()),
        proposals,
        conflicts: Vec::new(),
    };
    let counts = ReportCounts {
        dat_candidates: 50,
        already_canonical: 24,
        safe_repairs: repair_plan.proposals.len(),
        needs_review: needs_review_count,
        blocked_repair: blocked_count,
        unsupported: 0,
        unmatched_candidates: 0,
        ignored_ancillary: 151,
        ..Default::default()
    };
    let report = LibraryRepairReport {
        counts,
        needs_review,
        blocked,
        ..Default::default()
    };
    LibraryRepairPlan {
        profile: "canonical-in-place".to_string(),
        generation: 1,
        created_at_unix: 10,
        source_id: "sms".to_string(),
        source_display_name: "Sega - Master System - Mark III (20260809-210908)".to_string(),
        dat_path: "/mnt/Sega - Master System - Mark III (20260809-210908).dat".to_string(),
        scan_root: "/roms/sms".to_string(),
        truncated: false,
        files_scanned: 201,
        repair_plan,
        report,
    }
}

// ---------------------------------------------------------------------------
// View model
// ---------------------------------------------------------------------------

#[test]
fn safe_filter_returns_only_proposals() {
    let rows = build_rows(&fixture_plan_mixed(), Some(RepairFilter::Safe));
    assert_eq!(rows.len(), 26);
    assert!(rows.iter().all(|row| row.kind == RepairRowKind::Safe));
    assert!(rows.iter().all(|row| row.proposal_id.is_some()));
    assert!(rows.iter().all(|row| row.destination.is_some()));
}

#[test]
fn needs_review_filter_returns_only_report_needs_review_rows() {
    let rows = build_rows(&fixture_plan_mixed(), Some(RepairFilter::NeedsReview));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, RepairRowKind::NeedsReview);
    assert_eq!(rows[0].proposal_id, None, "PlanItems carry no proposal id");
    assert_eq!(rows[0].destination, None, "PlanItems carry no destination");
    assert_eq!(rows[0].source, "/roms/sms/Ambiguous (USA).zip");
}

#[test]
fn blocked_filter_returns_only_report_blocked_rows() {
    let rows = build_rows(&fixture_plan_mixed(), Some(RepairFilter::Blocked));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, RepairRowKind::Blocked);
    assert_eq!(rows[0].source, "/roms/sms/Blocked (USA).zip");
}

#[test]
fn all_filter_is_safe_then_needs_review_then_blocked() {
    let rows = build_rows(&fixture_plan_mixed(), None);
    assert_eq!(rows.len(), 28);
    assert!(rows[..26].iter().all(|row| row.kind == RepairRowKind::Safe));
    assert_eq!(rows[26].kind, RepairRowKind::NeedsReview);
    assert_eq!(rows[27].kind, RepairRowKind::Blocked);
}

#[test]
fn deterministic_ordering_is_stable_across_builds() {
    let first = build_rows(&fixture_plan_mixed(), None);
    let second = build_rows(&fixture_plan_mixed(), None);
    assert_eq!(first, second);
}

#[test]
fn the_acceptance_fixture_maps_to_the_expected_summary() {
    assert_eq!(
        summary_line(&fixture_plan().report.counts, CountsAvailability::CURRENT),
        "50 DAT candidates · 24 already canonical · 26 safe repairs · 0 needs review · 0 blocked · 0 unmatched · 151 ancillary ignored"
    );
}

#[test]
fn an_unavailable_count_reads_as_unavailable_not_zero() {
    let mut counts = fixture_plan().report.counts;
    counts.dat_candidates = 0;
    counts.ignored_ancillary = 0;
    let unavailable = CountsAvailability {
        dat_candidates: false,
        ignored_ancillary: false,
    };
    let line = summary_line(&counts, unavailable);
    assert!(line.contains("DAT candidates: unavailable in this saved plan"));
    assert!(line.contains("ancillary ignored: unavailable in this saved plan"));
    assert!(!line.contains("0 DAT candidates"));
    assert!(!line.contains("0 ancillary ignored"));
}

#[test]
fn loading_a_plan_saved_before_the_accounting_fields_existed_marks_them_unavailable() {
    let dir = TestDir::new("legacy-counts");
    let path = dir.path().join("legacy.json");
    // A plan JSON as it would have been written before `dat_candidates` and
    // `ignored_ancillary` existed on `ReportCounts`: the whole plan, minus
    // those two keys from `report.counts`.
    let mut value = serde_json::to_value(fixture_plan()).unwrap();
    let counts = value
        .get_mut("report")
        .unwrap()
        .get_mut("counts")
        .unwrap()
        .as_object_mut()
        .unwrap();
    counts.remove("dat_candidates");
    counts.remove("ignored_ancillary");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path);

    assert!(
        state.plan.is_some(),
        "still deserialises via #[serde(default)]"
    );
    assert!(!state.counts_availability.dat_candidates);
    assert!(!state.counts_availability.ignored_ancillary);
    // The deserialised struct itself can't tell the difference: this is
    // exactly why availability is tracked separately from the counts.
    assert_eq!(state.plan.as_ref().unwrap().report.counts.dat_candidates, 0);

    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(
        &output,
        "DAT candidates: unavailable in this saved plan"
    ));
    assert!(rendered_text_contains(
        &output,
        "ancillary ignored: unavailable in this saved plan"
    ));
}

#[test]
fn loading_a_current_schema_plan_shows_a_genuine_zero_not_unavailable() {
    let dir = TestDir::new("current-counts");
    let path = dir.path().join("current.json");
    std::fs::write(&path, serde_json::to_string(&fixture_plan()).unwrap()).unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path);

    assert!(state.counts_availability.dat_candidates);
    assert!(state.counts_availability.ignored_ancillary);
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[test]
fn only_safe_rows_can_be_selected() {
    let mut state = RepairReviewPageState::default();
    let rows = build_rows(&fixture_plan_mixed(), None);
    state.select_all(&rows);
    // Select all picks only Safe ids, never the NeedsReview/Blocked rows.
    assert_eq!(state.selected.len(), 26);
    let needs_review_id = rows[26].proposal_id.clone();
    assert!(needs_review_id.is_none());
    assert!(state.selected.iter().all(|id| {
        rows[..26]
            .iter()
            .any(|row| row.proposal_id.as_ref() == Some(id))
    }));
}

#[test]
fn selection_persists_across_filters() {
    let mut state = RepairReviewPageState::default();
    state.select_all(&build_rows(&fixture_plan_mixed(), Some(RepairFilter::Safe)));
    assert_eq!(state.selected.len(), 26);

    // Switching to a filter that shows no Safe rows must not clear selection.
    state.set_filter(Some(RepairFilter::NeedsReview));
    assert_eq!(state.selected.len(), 26);

    state.select_none();
    assert!(state.selected.is_empty());
}

#[test]
fn toggle_selected_only_accepts_proposal_ids() {
    let mut state = RepairReviewPageState::default();
    let rows = build_rows(&fixture_plan_mixed(), None);
    let safe_id = rows[0].proposal_id.clone().unwrap();
    state.toggle_selected(&safe_id);
    assert_eq!(state.selected.len(), 1);
    state.toggle_selected(&safe_id);
    assert!(state.selected.is_empty(), "toggling off removes the id");
}

#[test]
fn select_all_acts_on_the_safe_rows_visible_under_the_current_filter() {
    let mut state = RepairReviewPageState::default();
    // Under the NeedsReview filter there are no Safe rows, so select all
    // selects nothing.
    state.set_filter(Some(RepairFilter::NeedsReview));
    state.select_all(&build_rows(
        &fixture_plan_mixed(),
        Some(RepairFilter::NeedsReview),
    ));
    assert!(state.selected.is_empty());
}

// ---------------------------------------------------------------------------
// Row cache
// ---------------------------------------------------------------------------

#[test]
fn rows_are_cached_until_plan_or_filter_changes() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan_mixed()),
        ..RepairReviewPageState::default()
    };
    let first = state.rows();
    let second = state.rows();
    assert!(
        Rc::ptr_eq(&first, &second),
        "same plan and filter reuse the cached rows"
    );

    state.set_filter(Some(RepairFilter::Safe));
    let filtered = state.rows();
    assert!(
        !Rc::ptr_eq(&first, &filtered),
        "changing the filter rebuilds the rows"
    );
    assert_eq!(filtered.len(), 26);

    let unchanged = state.rows();
    assert!(
        Rc::ptr_eq(&filtered, &unchanged),
        "the same filter reuses the cache again"
    );
}

#[test]
fn reloading_a_plan_invalidates_the_row_cache() {
    let dir = TestDir::new("cache-reload");
    let path = dir.path().join("plan.json");
    std::fs::write(&path, serde_json::to_string(&fixture_plan()).unwrap()).unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path.clone());
    let first = state.rows();
    assert_eq!(first.len(), 26);

    // A fresh successful load bumps the plan version even when the file's
    // content is unchanged, so the cache must not be reused across it.
    state.load_plan(path);
    let second = state.rows();
    assert!(
        !Rc::ptr_eq(&first, &second),
        "a fresh load rebuilds the rows"
    );
    assert_eq!(
        *first, *second,
        "content is unchanged since the file didn't change"
    );
}

// ---------------------------------------------------------------------------
// Plan loading (read-only)
// ---------------------------------------------------------------------------

#[test]
fn library_repair_plan_round_trips_through_json() {
    let plan = fixture_plan();
    let json = serde_json::to_string(&plan).unwrap();
    let decoded: LibraryRepairPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, plan);
}

#[test]
fn load_plan_populates_state_and_never_touches_the_file() {
    let dir = TestDir::new("load");
    let path = dir.path().join("plan.json");
    let json = serde_json::to_string_pretty(&fixture_plan()).unwrap();
    std::fs::write(&path, &json).unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path.clone());

    assert!(state.plan.is_some());
    assert_eq!(state.plan_path.as_deref(), Some(path.as_path()));
    assert!(state.error.is_none());
    // The file is byte-identical after a "load": reading is all that happens.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), json);
}

#[test]
fn a_malformed_plan_file_reports_a_useful_error_and_keeps_prior_state() {
    let dir = TestDir::new("bad");
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not a plan {{").unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path.clone());

    assert!(state.plan.is_none());
    assert!(state.error.is_some());
    assert!(state.error.as_ref().unwrap().contains("could not parse"));
}

/// The malformed-file test above starts with no plan loaded, so it can't
/// prove the "keeps prior state" half of its own name. This covers the case
/// that actually matters: a valid plan is already loaded, a replacement file
/// fails to load, and the page must make it unambiguous that (a) the new
/// plan failed and (b) what's on screen is still the old one.
#[test]
fn a_failed_reload_keeps_the_prior_plan_visible_and_says_so() {
    let dir = TestDir::new("bad-reload");
    let good_path = dir.path().join("good.json");
    std::fs::write(&good_path, serde_json::to_string(&fixture_plan()).unwrap()).unwrap();
    let bad_path = dir.path().join("bad.json");
    std::fs::write(&bad_path, "not a plan {{").unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(good_path.clone());
    assert!(state.plan.is_some());
    let selected_before = {
        let id = state.rows()[0].proposal_id.clone().unwrap();
        state.toggle_selected(&id);
        id
    };

    state.load_plan(bad_path);

    // The prior valid plan, path, and selection are untouched; only `error`
    // is set.
    assert!(state.plan.is_some());
    assert_eq!(state.plan_path.as_deref(), Some(good_path.as_path()));
    assert!(state.error.is_some());
    assert!(state.selected.contains(&selected_before));

    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(
        &output,
        "Could not load the new repair plan"
    ));
    assert!(rendered_text_contains(&output, "previously loaded plan"));
    // The old plan's own summary is still visible underneath the error.
    assert!(rendered_text_contains(&output, "26 safe repairs"));
}

// ---------------------------------------------------------------------------
// No mutation surface
// ---------------------------------------------------------------------------

/// The page's only filesystem operation is a read of the plan file. This test
/// pins the read-only contract for a realistic saved plan: loading never
/// re-runs a scan, preflight, or re-proof, and leaves the source untouched.
#[test]
fn loading_does_not_mutate_anything() {
    let dir = TestDir::new("nomutate");
    let path = dir.path().join("plan.json");
    let json = serde_json::to_string_pretty(&fixture_plan()).unwrap();
    std::fs::write(&path, &json).unwrap();

    let mut state = RepairReviewPageState::default();
    state.load_plan(path.clone());

    assert_eq!(state.selected.len(), 0);
    assert_eq!(state.details_id, None);
    assert!(state.plan.is_some());
    // No journal, no new files, no writes anywhere in the temp dir.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["plan.json".to_string()]);
}

// ---------------------------------------------------------------------------
// Render smoke test
// ---------------------------------------------------------------------------

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

/// The `pos` (top-left) of the first text shape whose galley text contains
/// `needle`, searched in painting order.
fn text_shape_position(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
    fn find(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(text_shape) if text_shape.galley.text().contains(needle) => {
                Some(text_shape.pos)
            }
            egui::Shape::Vec(nested) => nested.iter().find_map(|s| find(s, needle)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, needle))
}

/// Regression test for a row-layout bug where each virtualised row's rect
/// was anchored at `ui.min_rect().min` - the top-left corner of everything
/// the `Ui` has laid out, which does not move as rows are added - instead of
/// `ui.cursor().min`. Every row landed at the same position and overlapped
/// into unreadable stacked text. This pins that rows of distinct proposals
/// render at distinct, monotonically increasing, row-height-spaced `y`
/// positions.
#[test]
fn safe_repair_rows_do_not_overlap() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        plan_path: Some(PathBuf::from("/roms/sms/plan.json")),
        ..RepairReviewPageState::default()
    };
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });

    // Only as many rows as fit the virtualised viewport actually paint; pull
    // out however many of the first several proposals are visible and check
    // every one of them, so this test doesn't depend on exact viewport math.
    let positions: Vec<egui::Pos2> = (0..10)
        .filter_map(|index| text_shape_position(&output, &format!("Game {index} (USA)")))
        .collect();
    assert!(
        positions.len() >= 2,
        "expected at least two visible Safe rows to compare, got {}",
        positions.len()
    );

    let row_height = 30.0_f32;
    for pair in positions.windows(2) {
        let [a, b] = pair else { unreachable!() };
        assert!(
            b.y > a.y,
            "rows must render top-to-bottom in order: {a:?} then {b:?}"
        );
        let gap = b.y - a.y;
        assert!(
            gap >= row_height - 1.0,
            "adjacent rows overlap: {a:?} then {b:?} (gap {gap}, expected >= {row_height})"
        );
    }
}

#[test]
fn the_page_renders_summary_rows_and_a_disabled_apply() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        plan_path: Some(PathBuf::from("/roms/sms/plan.json")),
        ..RepairReviewPageState::default()
    };
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Repair Review"));
    assert!(rendered_text_contains(&output, "26 safe repairs"));
    assert!(rendered_text_contains(&output, "151 ancillary ignored"));
    assert!(rendered_text_contains(&output, "Apply Selected (0)"));
    assert!(rendered_text_contains(&output, "Load repair plan"));
}
