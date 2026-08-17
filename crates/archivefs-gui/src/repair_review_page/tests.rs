//! Data and render tests for the Repair Review page.
//!
//! The view model ([`super::build_rows`], [`super::summary_line`]) is a pure
//! function of a [`LibraryRepairPlan`] and the filter, so what the page says
//! is checkable without a frame buffer. Drawing is exercised once through a
//! headless egui context. No real plan, ROM, or DAT file is opened; every
//! fixture is constructed in memory or written to a per-test temp directory.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::sources::DatSourceKind;
use archivefs_core::repair::execute::RepairReverifyOutcome;
use archivefs_core::repair::library::{
    LibraryRepairPlan, LibraryRepairReport, LibraryScanRequest, PlanItem, RepairProfile,
    ReportCounts, run_library_scan,
};
use archivefs_core::repair::plan::{RepairPlan, RepairPlanId};
use archivefs_core::repair::proposal::{
    RepairAction, RepairEvidence, RepairEvidenceKind, RepairProposal, RepairProposalId, SafetyState,
};
use archivefs_core::safe_read::TrustedRoots;

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
        // A real identity, not `None`: `RepairProposal::actionable()` (and so
        // `RepairReviewPageState::actionable_selected_ids`) requires one, and
        // several apply-enable-rule tests select fixture proposals.
        expected_source_identity: Some(archivefs_core::dat::rename_apply::ObjectIdentity {
            size_bytes: 1,
            modified_unix: 1,
            kind: archivefs_core::dat::rename_apply::ObjectKind::RegularFile,
            #[cfg(unix)]
            ino: 1,
            #[cfg(unix)]
            dev: 1,
        }),
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
        survivor_path: None,
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

// ---------------------------------------------------------------------------
// Apply Selected: real scans, a real (disposable, temp-dir) library, and the
// real trusted backend. Every fixture below is a fresh `TestDir`; nothing
// here ever touches a real ROM library.
// ---------------------------------------------------------------------------

/// SHA-1 of `b"test"` (4 bytes).
const SHA1_TEST: &str = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";
/// SHA-1 of `b"abc"` (3 bytes).
const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

/// A two-game DAT and two wrongly-named loose ROMs under `dir`, so a real
/// scan produces exactly two independent, non-conflicting Safe proposals.
fn write_apply_fixture(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("two.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<datafile><header><name>Two</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("a.bin"), b"test").unwrap();
    std::fs::write(roms.join("b.bin"), b"abc").unwrap();
    (dat, roms)
}

/// Runs a real, read-only scan over `write_apply_fixture`'s layout and
/// returns the saved-plan document exactly as `repair scan --plan-out`
/// would, so apply tests exercise the real trust boundary
/// (`apply_saved_plan_selected` re-scans and re-proves this) rather than a
/// hand-built plan.
fn scan_apply_fixture(dat: &std::path::Path, roms: &std::path::Path) -> LibraryRepairPlan {
    let request = LibraryScanRequest {
        source_id: "test".to_string(),
        source_display_name: "Test catalogue".to_string(),
        dat_path: dat.to_path_buf(),
        dat_kind: DatSourceKind::File,
        scan_root: roms.to_path_buf(),
        limits: DatLimits::default(),
        profile: RepairProfile::CanonicalInPlace,
    };
    let outcome = run_library_scan(
        &request,
        &TrustedRoots::none(),
        &std::sync::atomic::AtomicBool::new(false),
        &|_| {},
    )
    .expect("the fixture scan runs");
    archivefs_core::repair::library::plan_file_from_scan(&outcome)
}

/// Blocks the calling test thread (never the egui/render thread - there is
/// none in these tests) until the page's background apply job settles or a
/// generous deadline passes, polling exactly the way the real render loop
/// does (`poll_apply` once per tick).
fn wait_for_apply(state: &mut RepairReviewPageState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.is_apply_running() {
        state.poll_apply();
        if Instant::now() > deadline {
            panic!("the background apply job did not finish in time");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn proposal_id_for(plan: &LibraryRepairPlan, source_basename: &str) -> RepairProposalId {
    plan.repair_plan
        .proposals
        .iter()
        .find(|p| p.source_path.file_name().unwrap() == source_basename)
        .expect("a proposal for the given source exists")
        .id
        .clone()
}

/// An isolated journal directory inside a test's own [`TestDir`], created
/// eagerly (journal writing requires the directory to already exist).
/// Every test that drives a real `confirm_apply` -> `spawn_apply` run must
/// use this - never the production default - so it can never write a fake
/// transaction into the developer's real Repair History. See
/// `RepairReviewPageState::journal_dir_override`'s doc.
fn isolated_journal_dir(dir: &std::path::Path) -> PathBuf {
    let journal_dir = dir.join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    journal_dir
}

/// A page state with `plan` loaded and its journal directory overridden to
/// `journal_dir` - see [`isolated_journal_dir`].
fn isolated_state(plan: LibraryRepairPlan, journal_dir: PathBuf) -> RepairReviewPageState {
    RepairReviewPageState {
        plan: Some(plan),
        journal_dir_override: Some(journal_dir),
        ..RepairReviewPageState::default()
    }
}

// --- enable/disable rules ---------------------------------------------------

#[test]
fn apply_is_disabled_with_no_plan_loaded() {
    let state = RepairReviewPageState::default();
    assert!(!state.can_apply());
}

#[test]
fn apply_is_disabled_with_a_plan_but_no_selection() {
    let state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        ..RepairReviewPageState::default()
    };
    assert!(!state.can_apply());
}

#[test]
fn apply_is_enabled_with_a_plan_and_a_safe_selection() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        ..RepairReviewPageState::default()
    };
    let id = state.plan.as_ref().unwrap().repair_plan.proposals[0]
        .id
        .clone();
    state.toggle_selected(&id);
    assert!(state.can_apply());
}

#[test]
fn apply_is_disabled_while_an_apply_is_already_running() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        ..RepairReviewPageState::default()
    };
    let id = state.plan.as_ref().unwrap().repair_plan.proposals[0]
        .id
        .clone();
    state.toggle_selected(&id);
    assert!(state.can_apply());
    state.apply_running = true;
    assert!(!state.can_apply(), "a second apply must not be offered");
}

// --- confirmation is required, and cancelling it never applies -------------

#[test]
fn clicking_apply_opens_a_confirmation_and_never_starts_a_job() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        ..RepairReviewPageState::default()
    };
    let id = state.plan.as_ref().unwrap().repair_plan.proposals[0]
        .id
        .clone();
    state.toggle_selected(&id);

    state.open_apply_confirmation();

    assert!(state.apply_confirm.is_some(), "the dialog is pending");
    assert!(
        !state.is_apply_running(),
        "opening the dialog must never itself start work"
    );
    assert_eq!(state.apply_confirm.as_ref().unwrap().selected, vec![id]);
}

#[test]
fn cancelling_the_confirmation_never_touches_the_filesystem() {
    let dir = TestDir::new("apply-cancel");
    let (dat, roms) = write_apply_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    state.toggle_selected(&alpha_id);
    state.open_apply_confirmation();
    assert!(state.apply_confirm.is_some());

    state.cancel_apply_confirmation();

    assert!(state.apply_confirm.is_none());
    assert!(!state.is_apply_running());
    assert!(state.selected.contains(&alpha_id), "selection is kept");
    assert!(roms.join("a.bin").exists(), "nothing was renamed");
    assert!(roms.join("b.bin").exists(), "nothing was renamed");
    assert!(!roms.join("alpha.bin").exists());
}

// --- selected ids are passed exactly once -----------------------------------

#[test]
fn only_the_confirmed_selection_is_sent_and_applied() {
    let dir = TestDir::new("apply-selected-once");
    let (dat, roms) = write_apply_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");
    let beta_id = proposal_id_for(&plan, "b.bin");

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    // Select only Alpha; Beta is a known-good proposal that must be left
    // completely alone.
    state.toggle_selected(&alpha_id);
    state.open_apply_confirmation();
    assert_eq!(
        state.apply_confirm.as_ref().unwrap().selected,
        vec![alpha_id.clone()],
        "exactly the confirmed id, exactly once"
    );
    state.confirm_apply();
    wait_for_apply(&mut state);

    let result = state.apply_result.as_ref().expect("the apply succeeded");
    let rename = result.rename.as_ref().expect("a rename batch ran");
    assert_eq!(rename.summary.requested, 1, "exactly one proposal ran");
    assert_eq!(rename.summary.applied, 1);
    assert!(result.quarantine.is_empty(), "no quarantine proposal ran");
    assert!(roms.join("alpha.bin").exists(), "Alpha was renamed");
    assert!(roms.join("b.bin").exists(), "Beta was never touched");
    assert!(!roms.join("beta.bin").exists());
    assert!(!state.selected.contains(&alpha_id), "applied id is cleared");
    let _ = beta_id;
}

// --- double-click / re-entry is blocked while running -----------------------

#[test]
fn a_second_apply_attempt_while_running_is_a_no_op() {
    let dir = TestDir::new("apply-double-click");
    let (dat, roms) = write_apply_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    state.toggle_selected(&alpha_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    assert!(state.is_apply_running());

    // Simulate a second click landing while the first job is in flight: the
    // button is supposed to be disabled by then, but the state methods
    // themselves must also refuse, never spawning a second worker.
    assert!(!state.can_apply());
    state.open_apply_confirmation();
    assert!(
        state.apply_confirm.is_none(),
        "a confirmation cannot open while an apply is already running"
    );
    state.confirm_apply(); // no-op: no pending confirmation to consume
    assert!(state.is_apply_running(), "the original job is untouched");

    wait_for_apply(&mut state);
    let result = state.apply_result.as_ref().expect("the single apply ran");
    let rename = result.rename.as_ref().expect("a rename batch ran");
    assert_eq!(
        rename.summary.requested, 1,
        "only the one originally confirmed proposal ever ran"
    );
    assert!(roms.join("alpha.bin").exists());
}

// --- successful result state -------------------------------------------------

#[test]
fn a_successful_apply_reports_counts_reverify_and_clears_the_selection() {
    let dir = TestDir::new("apply-success");
    let (dat, roms) = write_apply_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");
    let beta_id = proposal_id_for(&plan, "b.bin");

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    state.toggle_selected(&alpha_id);
    state.toggle_selected(&beta_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    let result = state.apply_result.as_ref().expect("apply succeeded");
    let rename = result.rename.as_ref().expect("a rename batch ran");
    assert_eq!(rename.summary.requested, 2);
    assert_eq!(rename.summary.applied, 2);
    assert_eq!(rename.summary.failed, 0);
    assert!(!rename.transaction.transaction_id.is_empty());
    assert_eq!(rename.reverify.len(), 2);
    assert!(
        rename
            .reverify
            .iter()
            .all(|entry| entry.outcome == RepairReverifyOutcome::Verified)
    );
    assert!(result.quarantine.is_empty(), "no quarantine proposal ran");
    assert!(state.apply_failure.is_none());
    assert!(!state.apply_running);
    assert!(state.selected.is_empty(), "both applied ids are cleared");
    assert!(state.plan_stale, "the loaded plan no longer reflects disk");
    let transaction_id = rename.summary.transaction_id.clone();

    // Rendered feedback: counts, reverify, and the stale-plan warning.
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Rename batch complete"));
    assert!(rendered_text_contains(&output, &transaction_id));
    assert!(rendered_text_contains(&output, "now stale"));
}

// --- failed/refused result state --------------------------------------------

#[test]
fn a_refused_apply_reports_the_reason_and_mutates_nothing() {
    let dir = TestDir::new("apply-refused");
    let (dat, roms) = write_apply_fixture(dir.path());
    let mut plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");
    // Point the plan's own recorded DAT at a path that does not exist: the
    // background worker re-scans from exactly this path (never the real
    // fixture DAT), so the re-scan itself refuses before anything is
    // proven or touched.
    plan.dat_path = dir.path().join("does-not-exist.dat").display().to_string();

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    state.toggle_selected(&alpha_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    let failure = state.apply_failure.as_ref().expect("the apply refused");
    assert_eq!(failure.label, "Re-scan failed");
    assert!(state.apply_result.is_none());
    assert!(!state.apply_running);
    assert!(
        state.selected.contains(&alpha_id),
        "a refusal never clears the selection"
    );
    assert!(!state.plan_stale, "a refusal never mutates the library");
    assert!(roms.join("a.bin").exists(), "nothing was renamed");
    assert!(roms.join("b.bin").exists(), "nothing was renamed");

    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Re-scan failed"));
}

// --- the previous plan is retained on failure -------------------------------

#[test]
fn the_loaded_plan_is_retained_after_a_refused_apply() {
    let dir = TestDir::new("apply-refused-keeps-plan");
    let (dat, roms) = write_apply_fixture(dir.path());
    let mut plan = scan_apply_fixture(&dat, &roms);
    let alpha_id = proposal_id_for(&plan, "a.bin");
    let original_source_id = plan.source_id.clone();
    plan.dat_path = dir.path().join("does-not-exist.dat").display().to_string();

    let mut state = isolated_state(plan, isolated_journal_dir(dir.path()));
    state.toggle_selected(&alpha_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    assert!(state.apply_failure.is_some());
    let plan_after = state.plan.as_ref().expect("the plan was never discarded");
    assert_eq!(plan_after.source_id, original_source_id);
}

// --- no direct filesystem mutation path in this page ------------------------

/// The page's own doc comment claims every mutation goes through
/// `apply_saved_plan_selected`, never a direct `fs::rename`. This pins that
/// claim structurally, not just behaviourally: the source of this page
/// module must never spell a direct rename/move call.
#[test]
fn the_page_module_never_calls_fs_rename_directly() {
    let source = include_str!("../repair_review_page.rs");
    assert!(
        !source.contains("fs::rename("),
        "the Repair Review page must route every mutation through \
         apply_saved_plan_selected, never a direct fs::rename"
    );
    assert!(
        source.contains("apply_saved_plan_selected"),
        "the page must call the trusted selected-apply backend"
    );
}

// ---------------------------------------------------------------------------
// Duplicate quarantine: display, selection, confirmation, apply, partial
// failure, and its downstream visibility in Repair History / rollback.
//
// Every fixture below is a fresh, disposable `TestDir`; the quarantine
// proposals come from the real duplicate-scan/planning path
// (`run_library_scan` -> `plan_file_from_scan`), never hand-built, so these
// tests exercise the exact shape `RepairProposal::survivor_path` /
// `is_duplicate_quarantine()` actually produces.
// ---------------------------------------------------------------------------

use crate::repair_history_page::RepairHistoryPageState;

/// A DAT with one game/rom, and a library with the canonical keeper plus one
/// byte-identical redundant copy under an unrelated name - the real scan
/// produces exactly one Safe duplicate-quarantine `MovePath` proposal.
fn write_duplicate_fixture(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("dup.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<datafile><header><name>Dup</name></header>
<game name="Game"><rom name="canon.bin" size="4" sha1="{SHA1_TEST}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("canon.bin"), b"test").unwrap();
    std::fs::write(roms.join("dup-copy.bin"), b"test").unwrap();
    (dat, roms)
}

/// One ordinary wrongly-named rename target plus one already-canonical
/// survivor with a redundant duplicate: a real scan produces exactly one
/// `RenamePath` proposal and one duplicate-quarantine `MovePath` proposal,
/// sharing no source or destination.
fn write_mixed_fixture(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("mixed.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<datafile><header><name>Mixed</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("a.bin"), b"test").unwrap();
    std::fs::write(roms.join("beta.bin"), b"abc").unwrap();
    std::fs::write(roms.join("beta-dup.bin"), b"abc").unwrap();
    (dat, roms)
}

/// Two independent duplicate-content groups in two subdirectories, each with
/// an already-canonical survivor and one redundant copy - a selection
/// spanning both produces two independent quarantine transactions.
fn write_two_group_duplicate_fixture(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("two-groups.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header><name>TwoGroups</name></header>
    <game name="GameA"><rom name="canon-a.bin" size="4" sha1="{SHA1_TEST}"/></game>
    <game name="GameB"><rom name="canon-b.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    let group_a = roms.join("groupa");
    let group_b = roms.join("groupb");
    std::fs::create_dir_all(&group_a).unwrap();
    std::fs::create_dir_all(&group_b).unwrap();
    std::fs::write(group_a.join("canon-a.bin"), b"test").unwrap();
    std::fs::write(group_a.join("redundant-a.bin"), b"test").unwrap();
    std::fs::write(group_b.join("canon-b.bin"), b"abc").unwrap();
    std::fs::write(group_b.join("redundant-b.bin"), b"abc").unwrap();
    (dat, roms)
}

/// The (unique, in these fixtures) quarantine `MovePath` proposal's id.
fn quarantine_proposal_id(plan: &LibraryRepairPlan) -> RepairProposalId {
    plan.repair_plan
        .proposals
        .iter()
        .find(|p| p.survivor_path.is_some())
        .expect("a quarantine proposal exists")
        .id
        .clone()
}

// --- A. distinct action label -----------------------------------------------

#[test]
fn a_quarantine_proposal_row_renders_a_distinct_action_label() {
    let dir = TestDir::new("quarantine-row-label");
    let (dat, roms) = write_duplicate_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);

    let rows = build_rows(&plan, Some(RepairFilter::Safe));
    let row = rows
        .iter()
        .find(|row| row.proposal_id.as_ref() == Some(&quarantine_id))
        .expect("the quarantine row exists");
    assert!(row.is_duplicate_quarantine);
    assert!(row.survivor.is_some());
    assert!(row.has_duplicate_content_evidence);

    let mut state = RepairReviewPageState {
        plan: Some(plan),
        plan_path: Some(PathBuf::from("/roms/plan.json")),
        ..RepairReviewPageState::default()
    };
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Quarantine duplicate"));
    assert!(rendered_text_contains(&output, "survivor:"));
}

// --- B. a Safe quarantine proposal is selectable ----------------------------

#[test]
fn a_safe_quarantine_proposal_is_selectable_and_actionable() {
    let dir = TestDir::new("quarantine-selectable");
    let (dat, roms) = write_duplicate_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);

    let mut state = RepairReviewPageState {
        plan: Some(plan),
        ..RepairReviewPageState::default()
    };
    state.toggle_selected(&quarantine_id);
    assert_eq!(state.actionable_selected_ids(), vec![quarantine_id.clone()]);
    assert!(state.can_apply());

    // Select all also picks it up: it is a Safe row like any other.
    let mut state2 = RepairReviewPageState {
        plan: Some(state.plan.clone().unwrap()),
        ..RepairReviewPageState::default()
    };
    let rows = build_rows(state2.plan.as_ref().unwrap(), None);
    state2.select_all(&rows);
    assert!(state2.selected.contains(&quarantine_id));
}

// --- C. a NeedsReview row is never selectable, quarantine or not -----------

#[test]
fn a_needs_review_row_mentioning_a_duplicate_is_never_selectable() {
    let mut plan = fixture_plan_mixed();
    plan.report.needs_review[0].reason =
        "no unique survivor among a duplicate-content group".to_string();
    let rows = build_rows(&plan, None);
    let needs_review_row = rows
        .iter()
        .find(|row| row.kind == RepairRowKind::NeedsReview)
        .expect("the NeedsReview row exists");
    // The report's `PlanItem` carries no proposal id at all - a NeedsReview
    // row can never be selected regardless of what its reason says, and it
    // is never presented as a duplicate-quarantine row (`build_rows` never
    // fabricates a `survivor_path` it was not handed).
    assert!(needs_review_row.proposal_id.is_none());
    assert!(!needs_review_row.is_duplicate_quarantine);

    // Select All (via `state.select_all`) only ever picks up a row's
    // `proposal_id`; the NeedsReview row has none, so it is structurally
    // impossible for it to end up selected, however many ordinary Safe rows
    // are also selected alongside it.
    let mut state = RepairReviewPageState::default();
    state.select_all(&rows);
    assert_eq!(
        state.selected.len(),
        26,
        "only the fixture's 26 Safe rows are selected"
    );
    for id in &state.selected {
        assert_ne!(
            Some(id),
            needs_review_row.proposal_id.as_ref(),
            "the NeedsReview row has no id to begin with, so it cannot appear here"
        );
    }
}

// --- D. confirmation counts rename vs quarantine ----------------------------

#[test]
fn the_confirmation_dialog_counts_rename_and_quarantine_actions_separately() {
    let dir = TestDir::new("quarantine-confirm-counts");
    let (dat, roms) = write_mixed_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let rename_id = plan
        .repair_plan
        .proposals
        .iter()
        .find(|p| !p.is_duplicate_quarantine())
        .expect("the ordinary rename proposal exists")
        .id
        .clone();
    let quarantine_id = quarantine_proposal_id(&plan);

    let mut state = RepairReviewPageState {
        plan: Some(plan),
        ..RepairReviewPageState::default()
    };
    state.toggle_selected(&rename_id);
    state.toggle_selected(&quarantine_id);
    state.open_apply_confirmation();

    let confirmation = state.apply_confirm.as_ref().expect("the dialog opened");
    assert_eq!(confirmation.rename_count, 1);
    assert_eq!(confirmation.quarantine_count, 1);
    assert_eq!(confirmation.selected.len(), 2);

    let ctx = egui::Context::default();
    // The confirmation dialog is an `egui::Window`; like any floating area,
    // its content is only guaranteed painted from the second frame onward
    // (the first frame establishes its size/position). Run twice, exactly
    // as a real event loop would across two frames.
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "1 rename action(s)"));
    assert!(rendered_text_contains(&output, "1 quarantine action(s)"));
    assert!(rendered_text_contains(&output, ".emuwiz-quarantine"));
    assert!(rendered_text_contains(&output, "not permanently deleted"));
    assert!(rendered_text_contains(&output, "Repair History"));
}

// --- E (numbering per the review). "Apply Selected" button count and the
// confirmation dialog both reflect only the currently actionable selection,
// never the raw (possibly stale) `selected` set size.
#[test]
fn apply_selected_button_count_and_confirmation_match_only_the_actionable_ids() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        ..RepairReviewPageState::default()
    };
    let actionable_id = state.plan.as_ref().unwrap().repair_plan.proposals[0]
        .id
        .clone();
    let stale_id = state.plan.as_ref().unwrap().repair_plan.proposals[1]
        .id
        .clone();
    state.toggle_selected(&actionable_id);
    state.toggle_selected(&stale_id);
    assert_eq!(state.selected.len(), 2, "both ids are selected");

    // The second selected proposal is no longer actionable in the loaded
    // plan (e.g. it became NeedsReview) - `selected` itself is left
    // completely untouched, exactly like a real stale selection.
    state.plan.as_mut().unwrap().repair_plan.proposals[1].safety = SafetyState::NeedsReview;
    assert_eq!(
        state.actionable_selected_ids(),
        vec![actionable_id.clone()],
        "only the still-Safe proposal is actionable"
    );

    // The button label uses the actionable count (1), never the raw
    // selection size (2).
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Apply Selected (1)"));
    assert!(!rendered_text_contains(&output, "Apply Selected (2)"));

    // The confirmation dialog, and the exact ids it would submit, agree.
    state.open_apply_confirmation();
    let confirmation = state.apply_confirm.as_ref().expect("confirmation opens");
    assert_eq!(
        confirmation.selected,
        vec![actionable_id],
        "only the actionable id is ever sent to the backend"
    );
    assert_eq!(
        state.selected.len(),
        2,
        "the stale id remains selected in the UI state, it is just never submitted"
    );
}

// --- zero-applied quarantine group wording ----------------------------------

fn transaction_stub() -> archivefs_core::dat::rename_apply::RenameTransaction {
    archivefs_core::dat::rename_apply::RenameTransaction {
        transaction_id: "stub".to_string(),
        plan_generation: 0,
        classifier_version: None,
        created_at_unix: 0,
        source_scan_root: String::new(),
        state: archivefs_core::dat::rename_apply::TransactionState::ApplyFailed,
        entries: Vec::new(),
        created_directories: Vec::new(),
        unknown: Default::default(),
    }
}

/// A `RepairTransactionResult` whose typed counts are exactly what the
/// caller asks for - never derived from real entries, since
/// [`quarantine_group_headline`] only ever reads `result.summary`.
fn result_stub(applied: usize, failed: usize, skipped: usize) -> RepairTransactionResult {
    RepairTransactionResult {
        transaction: transaction_stub(),
        summary: archivefs_core::dat::rename_apply::TransactionSummary {
            transaction_id: "stub".to_string(),
            requested: applied + failed + skipped,
            applied,
            failed,
            skipped,
            rollback: Default::default(),
            started_at_unix: None,
            ended_at_unix: None,
        },
        reverify: Vec::new(),
    }
}

#[test]
fn quarantine_group_headline_reflects_the_actual_applied_count() {
    assert_eq!(
        quarantine_group_headline(&result_stub(1, 0, 0)),
        "Quarantine group complete"
    );
    assert_eq!(
        quarantine_group_headline(&result_stub(0, 1, 0)),
        "Quarantine group produced no applied changes",
        "applied == 0 must never be reported as complete, even with a failure"
    );
    assert_eq!(
        quarantine_group_headline(&result_stub(0, 0, 0)),
        "Quarantine group produced no applied changes",
        "applied == 0 must never be reported as complete, even with nothing else recorded either"
    );
    assert_eq!(
        quarantine_group_headline(&result_stub(1, 1, 0)),
        "Quarantine group partially applied",
        "some applied and some failed within one group is neither a full success nor a total failure"
    );
    assert_eq!(
        quarantine_group_headline(&result_stub(1, 0, 1)),
        "Quarantine group partially applied"
    );
}

#[test]
fn a_zero_applied_quarantine_group_is_never_rendered_as_complete() {
    let mut state = RepairReviewPageState {
        plan: Some(fixture_plan()),
        apply_result: Some(CombinedApplyResult {
            rename: None,
            quarantine: vec![archivefs_core::repair::library::QuarantineApplyResult {
                survivor_path: PathBuf::from("/roms/canon.bin"),
                result: result_stub(0, 1, 0),
            }],
        }),
        ..RepairReviewPageState::default()
    };
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(
        &output,
        "Quarantine group produced no applied changes"
    ));
    assert!(!rendered_text_contains(
        &output,
        "Quarantine group complete"
    ));
}

// --- E. a successful GUI quarantine apply creates a transaction ------------

#[test]
fn a_successful_quarantine_apply_creates_a_journaled_transaction() {
    let dir = TestDir::new("quarantine-apply-success");
    let (dat, roms) = write_duplicate_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);
    let journal_dir = isolated_journal_dir(dir.path());

    let mut state = isolated_state(plan, journal_dir.clone());
    state.toggle_selected(&quarantine_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    let result = state.apply_result.as_ref().expect("the apply succeeded");
    assert!(result.rename.is_none(), "no ordinary rename was selected");
    assert_eq!(result.quarantine.len(), 1);
    let group = &result.quarantine[0];
    assert_eq!(group.survivor_path, roms.join("canon.bin"));
    assert_eq!(group.result.summary.applied, 1);
    assert_eq!(group.result.summary.failed, 0);
    assert!(!group.result.summary.transaction_id.is_empty());

    assert!(roms.join(".emuwiz-quarantine").exists());
    assert!(
        !roms.join("dup-copy.bin").exists(),
        "the duplicate moved out of its original location"
    );
    assert!(roms.join("canon.bin").exists(), "the survivor is untouched");
    assert!(state.apply_failure.is_none());
    assert!(state.plan_stale);
    assert!(!state.selected.contains(&quarantine_id));
    let group_transaction_id = group.result.summary.transaction_id.clone();

    // Regression: the real `confirm_apply` -> `spawn_apply` path actually
    // used the isolated `journal_dir_override`, not the production default -
    // the journal exists exactly where this test pointed it.
    assert!(
        journal_dir
            .join(format!("{group_transaction_id}.json"))
            .exists(),
        "the journal was written to the overridden test journal directory"
    );

    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Quarantine group complete"));
    assert!(rendered_text_contains(&output, &group_transaction_id));
}

// --- F. partial multi-group apply surfaces completed + failed state --------

#[test]
fn a_partial_multi_group_apply_surfaces_completed_and_failed_state() {
    let dir = TestDir::new("quarantine-partial");
    let (dat, roms) = write_two_group_duplicate_fixture(dir.path());
    let plan = scan_apply_fixture(&dat, &roms);

    let quarantine_ids: Vec<RepairProposalId> = plan
        .repair_plan
        .proposals
        .iter()
        .filter(|p| p.is_duplicate_quarantine())
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(quarantine_ids.len(), 2, "{quarantine_ids:?}");

    // Deterministically break group B (never a race): its content-hash
    // bucket directory is replaced with a symlink pointing outside the
    // trust boundary before any apply runs, so `apply_quarantine_transaction`
    // refuses it outright the instant it needs that directory - group A is
    // completely unaffected. Mirrors
    // `archivefs_core::repair::library_tests::a_later_quarantine_group_failure_does_not_lose_an_earlier_groups_success`.
    let group_b_proposal = plan
        .repair_plan
        .proposals
        .iter()
        .find(|p| p.is_duplicate_quarantine() && p.source_path.ends_with("redundant-b.bin"))
        .expect("group B's quarantine proposal exists");
    let group_b_destination = group_b_proposal
        .destination()
        .expect("a MovePath destination")
        .clone();
    let group_b_bucket = group_b_destination
        .parent()
        .expect("the destination has a content-hash bucket parent")
        .to_path_buf();
    let outside = TestDir::new("quarantine-partial-outside");
    std::fs::create_dir_all(group_b_bucket.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &group_b_bucket).unwrap();

    let journal_dir = isolated_journal_dir(dir.path());
    let mut state = isolated_state(plan, journal_dir.clone());
    for id in &quarantine_ids {
        state.toggle_selected(id);
    }
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    let result = state
        .apply_result
        .as_ref()
        .expect("group A's completed result is surfaced, not discarded");
    assert_eq!(result.quarantine.len(), 1, "{result:?}");
    assert_eq!(
        result.quarantine[0].survivor_path,
        roms.join("groupa").join("canon-a.bin")
    );
    assert_eq!(result.quarantine[0].result.summary.applied, 1);
    let group_a_transaction_id = result.quarantine[0].result.summary.transaction_id.clone();

    // Regression: the isolated `journal_dir_override` was actually used by
    // the real `spawn_apply` worker for group A's successful transaction.
    assert!(
        journal_dir
            .join(format!("{group_a_transaction_id}.json"))
            .exists(),
        "the journal was written to the overridden test journal directory"
    );

    // The failure state is asserted by its typed category (`label`, set by
    // the page's own worker match arm - see `RepairApplyMessage::Partial`'s
    // construction in `spawn_apply`) rather than by matching a substring of
    // the underlying error's free-text `detail`, which is the core
    // executor's implementation detail (the exact wording of a symlink
    // refusal), not a GUI-level contract.
    let failure = state
        .apply_failure
        .as_ref()
        .expect("group B's failure is surfaced");
    assert_eq!(failure.label, "Quarantine apply failed", "{failure:?}");
    assert!(!failure.detail.is_empty(), "{failure:?}");

    assert!(!roms.join("groupa").join("redundant-a.bin").exists());
    assert!(roms.join("groupa").join("canon-a.bin").exists());
    assert!(
        roms.join("groupb").join("redundant-b.bin").exists(),
        "group B never moved"
    );
    assert!(!group_b_destination.exists());

    // Rendered feedback: the completed group is shown, and the failure
    // banner explicitly says something already completed rather than
    // reading as "nothing applied".
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_review_page(ui, &mut state);
        });
    });
    assert!(rendered_text_contains(&output, "Quarantine group complete"));
    assert!(rendered_text_contains(&output, "already completed"));
}

// --- G/H. history sees the resulting transaction, and rollback restores ----
//
// Both of these drive the *real* production state machine end to end -
// `open_apply_confirmation` -> `confirm_apply` -> `spawn_apply` ->
// `poll_apply` - never a direct call to `apply_saved_plan_selected`, so they
// prove the actual `RepairReviewPageState` GUI path (not just the backend
// it calls) leaves a journal `RepairHistoryPageState` can see and undo. The
// journal directory is overridden to an isolated path (see
// `isolated_journal_dir`) so this never touches the real user data
// directory.

/// Runs a real scan and a real GUI quarantine apply through
/// [`RepairReviewPageState`]'s own confirmation/spawn/poll cycle, leaving a
/// genuine `Applied` quarantine journal on disk in an isolated directory.
fn scan_and_apply_quarantine_through_the_gui(
    dir: &std::path::Path,
) -> (
    PathBuf,
    PathBuf,
    archivefs_core::repair::library::QuarantineApplyResult,
) {
    let (dat, roms) = write_duplicate_fixture(dir);
    let plan = scan_apply_fixture(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);
    let journal_dir = isolated_journal_dir(dir);

    let mut state = isolated_state(plan, journal_dir.clone());
    state.toggle_selected(&quarantine_id);
    state.open_apply_confirmation();
    state.confirm_apply();
    wait_for_apply(&mut state);

    let result = state
        .apply_result
        .expect("the real GUI apply path succeeds");
    assert!(result.rename.is_none(), "no ordinary rename was selected");
    let group = result
        .quarantine
        .into_iter()
        .next()
        .expect("exactly one quarantine group applied");
    assert_eq!(group.result.summary.applied, 1);
    (roms, journal_dir, group)
}

#[test]
fn repair_history_sees_a_quarantine_transaction_produced_from_the_gui_apply_path() {
    let dir = TestDir::new("quarantine-history-visibility");
    let (_roms, journal_dir, group) = scan_and_apply_quarantine_through_the_gui(dir.path());

    let history = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert!(
        history
            .transactions
            .iter()
            .any(|transaction| transaction.transaction_id == group.result.summary.transaction_id),
        "the quarantine transaction appears in ordinary Repair History, with no special-casing"
    );
}

#[test]
fn a_quarantine_transaction_from_the_gui_rolls_back_through_repair_history_undo() {
    let dir = TestDir::new("quarantine-history-rollback");
    let (roms, journal_dir, group) = scan_and_apply_quarantine_through_the_gui(dir.path());
    assert!(
        !roms.join("dup-copy.bin").exists(),
        "the duplicate was quarantined"
    );
    assert!(
        roms.join("canon.bin").exists(),
        "the survivor is untouched by the apply"
    );
    let quarantine_destination = group.result.transaction.entries[0].destination_path.clone();
    assert!(
        quarantine_destination.exists(),
        "the quarantine destination exists right after apply"
    );

    let mut history = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    let transaction_id = group.result.summary.transaction_id.clone();
    // The transaction id the real GUI apply reported is exactly the one
    // Repair History resolves against - the same journal, no translation.
    assert!(history.can_undo(&transaction_id));

    history.open_undo_confirmation(&transaction_id);
    assert!(history.undo_confirm.is_some());
    history.confirm_undo();
    wait_for_undo(&mut history);

    let outcome = history.undo_outcome.as_ref().expect("the undo ran");
    assert!(
        matches!(
            outcome.result,
            archivefs_core::dat::rename_apply::RollbackResult::FullyRolledBack
        ),
        "{outcome:?}"
    );
    assert!(
        roms.join("dup-copy.bin").exists(),
        "the quarantined file was restored to its original location"
    );
    assert!(
        !quarantine_destination.exists(),
        "the quarantine destination is gone after undo"
    );
    assert!(history.undo_error.is_none());

    // The default (live) journal directory was never touched by any of this.
    if let Ok(default_dir) = archivefs_core::dat::rename_apply::default_rename_transaction_dir() {
        assert!(
            !default_dir.join(format!("{transaction_id}.json")).exists(),
            "the real user journal directory must never see a test transaction"
        );
    }
}

/// Blocks the test thread until the history page's background undo job
/// settles or a generous deadline passes, polling exactly the way the real
/// render loop does. Mirrors `wait_for_apply` above and
/// `repair_history_page::tests::wait_for_undo`.
fn wait_for_undo(state: &mut RepairHistoryPageState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.is_undo_running() {
        state.poll_undo();
        if Instant::now() > deadline {
            panic!("the background undo job did not finish in time");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// --- I. ordinary rename-only Repair Review behaviour is unchanged ----------

#[test]
fn ordinary_rename_only_rows_carry_no_quarantine_signal_and_render_unchanged() {
    let rows = build_rows(&fixture_plan(), Some(RepairFilter::Safe));
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| !row.is_duplicate_quarantine));
    assert!(rows.iter().all(|row| row.survivor.is_none()));
    assert!(rows.iter().all(|row| !row.has_duplicate_content_evidence));

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
    assert!(!rendered_text_contains(&output, "Quarantine duplicate"));
    assert!(rendered_text_contains(&output, "26 safe repairs"));
}
