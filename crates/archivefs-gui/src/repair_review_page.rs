//! The Repair Review page.
//!
//! Loads a saved whole-library [`LibraryRepairPlan`] (the exact JSON the
//! CLI's `repair scan --plan-out` / `repair plan --plan` contract produces),
//! summarises its [`ReportCounts`], renders its proposals as a filterable,
//! selectable, virtualised list, and can apply a user-selected subset of the
//! Safe proposals.
//!
//! # No mutation except through the trusted backend
//!
//! This module never calls `std::fs::rename` or any other filesystem
//! mutation directly, and never builds or executes its own [`RepairPlan`].
//! The only mutation path is [`apply_saved_plan_selected`], invoked on a
//! background thread with the exact `LibraryRepairPlan` this page loaded
//! from disk: that function re-runs the authoritative scan, fully re-proves
//! the *entire* saved plan against it, resolves the selected ids against the
//! freshly proven plan only, and executes through the existing
//! transaction/journal/reverify machinery. This page never weakens, skips,
//! or duplicates any of that — it only supplies the saved plan, the selected
//! ids, and the trusted scan inputs recorded on the plan itself, and renders
//! whatever the backend returns.
//!
//! [`RepairPlan`]: archivefs_core::repair::plan::RepairPlan
//!
//! # Rows come from the backend, verbatim
//!
//! The row view-model ([`build_rows`]) is a pure presentation adapter:
//! Safe rows come directly from `plan.repair_plan.proposals`, NeedsReview
//! rows from `plan.report.needs_review`, and Blocked rows from
//! `plan.report.blocked`. It never infers or recalculates safety, never
//! reclassifies, and never builds a second planner. Ordering is deterministic
//! and fixed: Safe -> NeedsReview -> Blocked for the All filter, each bucket
//! in the backend's own order.
//!
//! # Do not render a plan twice, or scan in-GUI
//!
//! Loading a plan never re-runs a scan, preflight, or re-proof. That is a
//! deliberate later step.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, TryRecvError};

use archivefs_core::Config;
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::rename_apply::model::EntryState;
use archivefs_core::dat::sources::{
    DatSourceEntry, DatSourceRegistry, default_dat_sources_config_path,
    load_dat_sources_config_from,
};
use archivefs_core::repair::execute::{
    RepairExecutionOptions, RepairReverifyOutcome, RepairTransactionResult,
};
use archivefs_core::repair::library::{
    ApplySavedPlanSelectedError, CombinedApplyResult, LibraryRepairPlan, LibraryScanRequest,
    RepairProfile, ReportCounts, apply_saved_plan_selected, plan_file_from_scan, run_library_scan,
};
use archivefs_core::repair::proposal::{RepairEvidenceKind, RepairProposal, RepairProposalId};
use archivefs_core::safe_read::TrustedRoots;
use eframe::egui;

use crate::ui::{components as widgets, theme};

/// The preview filter. `None` is "All" and is not a variant so "All" is the
/// default and the filter's absence is not confused with one of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairFilter {
    Safe,
    NeedsReview,
    Blocked,
}

impl RepairFilter {
    pub(crate) const ALL: [RepairFilter; 3] = [Self::Safe, Self::NeedsReview, Self::Blocked];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::NeedsReview => "Needs review",
            Self::Blocked => "Blocked",
        }
    }
}

/// The row kind. Maps 1:1 onto the backend's own buckets; never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairRowKind {
    Safe,
    NeedsReview,
    Blocked,
}

impl RepairRowKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::NeedsReview => "Needs review",
            Self::Blocked => "Blocked",
        }
    }

    pub(crate) fn tone(self) -> widgets::StatusTone {
        match self {
            Self::Safe => widgets::StatusTone::Success,
            Self::NeedsReview => widgets::StatusTone::Warning,
            Self::Blocked => widgets::StatusTone::Blocked,
        }
    }
}

/// One presentation row. Safe rows carry a `RepairProposalId` (the selection
/// key); NeedsReview and Blocked rows are the report's thin `PlanItem`s and
/// carry none - the backend provides no destination or evidence for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairReviewRow {
    pub(crate) kind: RepairRowKind,
    /// `Some` iff this is a Safe proposal from `plan.repair_plan.proposals`.
    pub(crate) proposal_id: Option<RepairProposalId>,
    pub(crate) source: String,
    /// `None` when the backend's row carries no destination (NeedsReview /
    /// Blocked buckets, which have no canonical name). For a duplicate
    /// quarantine `MovePath` row this is the quarantine destination, exactly
    /// as recorded on the proposal - never a second-guessed path.
    pub(crate) destination: Option<String>,
    pub(crate) reason: String,
    /// Mirrors `RepairProposal::is_duplicate_quarantine()` for a Safe row;
    /// always `false` for a NeedsReview/Blocked row, which carries no
    /// `RepairProposal` (and so no `survivor_path`) to check at all - see
    /// `build_rows`.
    pub(crate) is_duplicate_quarantine: bool,
    /// The kept survivor's path, for a duplicate quarantine row only.
    pub(crate) survivor: Option<String>,
    /// Whether the proposal's evidence includes `DuplicateContent` - shown as
    /// its own visual signal, distinct from the quarantine action label
    /// itself (a future proposal kind could in principle carry this evidence
    /// without being a quarantine move).
    pub(crate) has_duplicate_content_evidence: bool,
}

/// Builds the deterministic row list for the current filter, purely from the
/// plan's own data. `filter = None` is "All": Safe, then NeedsReview, then
/// Blocked, each in the backend's order.
pub(crate) fn build_rows(
    plan: &LibraryRepairPlan,
    filter: Option<RepairFilter>,
) -> Vec<RepairReviewRow> {
    let mut rows = Vec::new();
    if filter.is_none() || filter == Some(RepairFilter::Safe) {
        for proposal in &plan.repair_plan.proposals {
            rows.push(RepairReviewRow {
                kind: RepairRowKind::Safe,
                proposal_id: Some(proposal.id.clone()),
                source: proposal.source_path.display().to_string(),
                destination: proposal
                    .destination()
                    .map(|destination| destination.display().to_string()),
                reason: if proposal.reason.is_empty() {
                    "safe repair".to_string()
                } else {
                    proposal.reason.clone()
                },
                is_duplicate_quarantine: proposal.is_duplicate_quarantine(),
                survivor: proposal
                    .survivor_path
                    .as_ref()
                    .map(|survivor| survivor.display().to_string()),
                has_duplicate_content_evidence: proposal
                    .evidence
                    .iter()
                    .any(|evidence| evidence.kind == RepairEvidenceKind::DuplicateContent),
            });
        }
    }
    if filter.is_none() || filter == Some(RepairFilter::NeedsReview) {
        for item in &plan.report.needs_review {
            rows.push(RepairReviewRow {
                kind: RepairRowKind::NeedsReview,
                proposal_id: None,
                source: item.path.clone(),
                destination: None,
                reason: item.reason.clone(),
                is_duplicate_quarantine: false,
                survivor: None,
                has_duplicate_content_evidence: false,
            });
        }
    }
    if filter.is_none() || filter == Some(RepairFilter::Blocked) {
        for item in &plan.report.blocked {
            rows.push(RepairReviewRow {
                kind: RepairRowKind::Blocked,
                proposal_id: None,
                source: item.path.clone(),
                destination: None,
                reason: item.reason.clone(),
                is_duplicate_quarantine: false,
                survivor: None,
                has_duplicate_content_evidence: false,
            });
        }
    }
    rows
}

/// Which of [`ReportCounts`]'s additive fields were actually present in the
/// saved plan's JSON, as opposed to filled in by `#[serde(default)]`.
///
/// `dat_candidates` and `ignored_ancillary` were added to `ReportCounts`
/// after the field's `#[serde(default)]` fallback of `0` was already load-
/// bearing for older saved plans, so a `0` in either field is ambiguous: it
/// means either "the scan found none" or "this plan predates the field
/// entirely". The GUI must not present the second case as the first, so
/// this is computed from the raw JSON at load time - the strongly typed
/// [`LibraryRepairPlan`] has already lost the distinction by the time it
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CountsAvailability {
    pub(crate) dat_candidates: bool,
    pub(crate) ignored_ancillary: bool,
}

impl CountsAvailability {
    /// A plan whose JSON carries both fields - the common case, and always
    /// correct for a plan built in-process rather than loaded from disk.
    pub(crate) const CURRENT: Self = Self {
        dat_candidates: true,
        ignored_ancillary: true,
    };

    /// Inspects the raw JSON (not the deserialised struct, which cannot
    /// distinguish "present and 0" from "absent") for `report.counts`'s two
    /// additive fields.
    fn from_raw_json(text: &str) -> Self {
        let counts = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get("report")?.get("counts").cloned());
        let has_field = |name: &str| {
            counts
                .as_ref()
                .is_some_and(|counts| counts.get(name).is_some())
        };
        Self {
            dat_candidates: has_field("dat_candidates"),
            ignored_ancillary: has_field("ignored_ancillary"),
        }
    }
}

impl Default for CountsAvailability {
    fn default() -> Self {
        Self::CURRENT
    }
}

/// The one-line summary, read directly from [`ReportCounts`]. A field the
/// saved plan's schema predates reads as "unavailable" rather than a
/// misleading `0`; see [`CountsAvailability`].
pub(crate) fn summary_line(counts: &ReportCounts, availability: CountsAvailability) -> String {
    let dat_candidates = if availability.dat_candidates {
        format!("{} DAT candidates", counts.dat_candidates)
    } else {
        "DAT candidates: unavailable in this saved plan".to_string()
    };
    let ignored_ancillary = if availability.ignored_ancillary {
        format!("{} ancillary ignored", counts.ignored_ancillary)
    } else {
        "ancillary ignored: unavailable in this saved plan".to_string()
    };
    format!(
        "{dat_candidates} · {} already canonical · {} safe repairs · {} needs review · {} blocked · {} unmatched · {ignored_ancillary}",
        counts.already_canonical,
        counts.safe_repairs,
        counts.needs_review,
        counts.blocked_repair,
        counts.unmatched_candidates,
    )
}

/// A snapshot of what "Apply Selected" is about to do, frozen at the moment
/// the confirmation dialog opens.
///
/// Frozen rather than recomputed live so that the dialog's own text and the
/// ids actually sent to the backend can never drift apart, even if the user
/// changes the selection or loads a different plan while the dialog is open
/// (in which case [`RepairReviewPageState::confirm_apply`] simply refuses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairApplyConfirmation {
    /// The exact, deterministically ordered ids [`apply_saved_plan_selected`]
    /// will be called with.
    pub(crate) selected: Vec<RepairProposalId>,
    pub(crate) scan_root: String,
    pub(crate) dat_path: String,
    /// How many of `selected` are ordinary rename proposals, computed from
    /// the loaded plan at the moment the dialog opened (`is_duplicate_quarantine()`
    /// is `false`).
    pub(crate) rename_count: usize,
    /// How many of `selected` are duplicate-quarantine `MovePath` proposals
    /// (`is_duplicate_quarantine()` is `true`).
    pub(crate) quarantine_count: usize,
}

/// Why the background apply worker could not complete, with a short label a
/// caller does not need to match on the underlying error type to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairApplyFailure {
    pub(crate) label: &'static str,
    pub(crate) detail: String,
}

impl RepairApplyFailure {
    fn from_error(error: ApplySavedPlanSelectedError) -> Self {
        match error {
            ApplySavedPlanSelectedError::Scan(inner) => Self {
                label: "Re-scan failed",
                detail: inner.to_string(),
            },
            ApplySavedPlanSelectedError::NotAuthorized(detail) => Self {
                label: "The saved plan could not be re-proven",
                detail,
            },
            ApplySavedPlanSelectedError::InvalidSelection(detail) => Self {
                label: "The selection could not be safely applied",
                detail,
            },
            ApplySavedPlanSelectedError::Execute(inner) => Self {
                label: "Apply failed",
                detail: inner.to_string(),
            },
            // Unreachable via `spawn_apply`'s worker: it matches these two
            // variants explicitly and turns them into `RepairApplyMessage::Partial`
            // so the completed groups they carry are never dropped (see
            // `spawn_apply` and `apply_combined_result`). Handled here too,
            // rather than left an unmatched pattern, purely so this function
            // stays total if a future caller ever routes one of these
            // variants through it directly.
            ApplySavedPlanSelectedError::QuarantineBuild { detail, .. } => Self {
                label: "Quarantine apply could not be built",
                detail,
            },
            ApplySavedPlanSelectedError::QuarantineApply { detail, .. } => Self {
                label: "Quarantine apply failed",
                detail,
            },
        }
    }
}

/// The terminal message the background apply worker sends back.
enum RepairApplyMessage {
    /// The whole selection applied without error: `CombinedApplyResult` may
    /// still carry a rename batch, zero or more quarantine groups, or both -
    /// see [`CombinedApplyResult`]'s doc.
    Applied(Box<CombinedApplyResult>),
    /// A later duplicate-quarantine group could not be built or applied,
    /// after zero or more earlier groups (and any rename batch, which always
    /// runs first) already succeeded and were durably journaled.
    /// `completed` is never dropped - see
    /// [`ApplySavedPlanSelectedError::QuarantineBuild`]/[`ApplySavedPlanSelectedError::QuarantineApply`]'s
    /// doc.
    Partial {
        completed: Box<CombinedApplyResult>,
        failure: RepairApplyFailure,
    },
    /// Nothing at all was applied: the re-scan, re-proof, selection, or the
    /// rename batch itself refused before any quarantine group ran.
    Failed(RepairApplyFailure),
}

/// The running background apply job. Mirrors the `dat_sources_page` job
/// pattern: an unbounded channel drained once per frame by
/// [`RepairReviewPageState::poll_apply`]. This slice does not offer
/// mid-apply cancellation (the batch is small and short-lived by
/// construction — a caller-selected subset), so no cancel handle is kept
/// here; the worker still takes a cancel flag (required by
/// [`apply_saved_plan_selected`]'s signature) but it is never set.
struct RepairApplyJob {
    messages: Receiver<RepairApplyMessage>,
}

/// The one-time setup for a whole-library repair scan: which DAT catalogue
/// to audit against, and which directory on disk to scan. Neither input can
/// be resolved automatically today - no config or registry anywhere records
/// which library directory a given DAT source should be audited against -
/// so both are collected here before the scan itself is ever spawned.
///
/// Deliberately holds only what the picker UI needs (loaded once, when the
/// dialog opens): registered DAT sources (mirrors `dat_sources_page`'s own
/// registry load) and the configured library source folders (mirrors
/// `dat_sources_page`'s own `library_folders`, offered the same way -
/// buttons plus "Choose another folder…").
#[derive(Debug, Default)]
pub(crate) struct ScanSetupState {
    pub(crate) dat_sources: Vec<DatSourceEntry>,
    /// Surfaced, never swallowed: an unreadable/unparseable DAT sources
    /// config means the picker has nothing to offer, and that must be
    /// visible rather than presented as an empty registry.
    pub(crate) dat_load_error: Option<String>,
    pub(crate) library_folders: Vec<PathBuf>,
    pub(crate) selected_dat_id: Option<String>,
    pub(crate) chosen_scan_root: Option<PathBuf>,
}

/// The one terminal message a background whole-library scan sends back.
enum RepairScanMessage {
    Completed(Box<LibraryRepairPlan>),
    Failed(String),
}

/// The running background whole-library scan job. Same one-shot channel
/// shape as [`RepairApplyJob`].
struct RepairScanJob {
    messages: Receiver<RepairScanMessage>,
}

/// Simple status for the whole-library scan action. Deliberately no
/// elaborate progress reporting in this stage - `run_library_scan`'s
/// `on_progress` callback is passed a no-op, exactly as the CLI's own
/// `repair scan` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LibraryScanStatus {
    Scanning,
    Completed,
    Failed(String),
}

/// The page's authoritative state.
#[derive(Default)]
pub(crate) struct RepairReviewPageState {
    pub(crate) plan: Option<LibraryRepairPlan>,
    pub(crate) plan_path: Option<PathBuf>,
    pub(crate) filter: Option<RepairFilter>,
    /// Selected Safe proposal ids. Selection is keyed by the durable
    /// [`RepairProposalId`], never by path.
    pub(crate) selected: BTreeSet<RepairProposalId>,
    pub(crate) details_id: Option<RepairProposalId>,
    pub(crate) error: Option<String>,
    /// Which of the loaded plan's additive [`ReportCounts`] fields the saved
    /// JSON actually carried, computed from the raw text at load time.
    /// [`CountsAvailability::CURRENT`] (the `Default`) while no plan is
    /// loaded; harmless since nothing reads it in that state.
    pub(crate) counts_availability: CountsAvailability,
    /// Bumped on every successfully loaded plan; the row cache key. Never
    /// bumped on a failed load, since the plan (and thus its rows) didn't
    /// change.
    plan_version: u64,
    /// The last-built row list, keyed by the plan version and filter it was
    /// built from. `rows()` rebuilds only when either changes.
    rows_cache: Option<(u64, Option<RepairFilter>, Rc<Vec<RepairReviewRow>>)>,
    /// A pending "Apply Selected" confirmation, frozen when the dialog opens.
    /// `None` means no confirmation is showing.
    pub(crate) apply_confirm: Option<RepairApplyConfirmation>,
    /// Set once, right after the confirmation dialog opens, so its Cancel
    /// button can claim focus on the frame it first appears (favouring
    /// Cancel as the safe default) without re-stealing focus every frame.
    apply_confirm_focus_cancel: bool,
    apply_job: Option<RepairApplyJob>,
    pub(crate) apply_running: bool,
    /// The last apply's result, when the backend actually ran and produced at
    /// least one rename batch or quarantine group. Carries a *partial* result
    /// (some groups applied, a later one failed) as well as a full success -
    /// see [`RepairApplyMessage::Partial`]. Never populated when nothing at
    /// all ran - see `apply_failure`.
    pub(crate) apply_result: Option<CombinedApplyResult>,
    /// The last apply's refusal or error, when the backend refused or a
    /// worker error occurred. Cleared only by a new apply attempt, never
    /// automatically, so the reason stays visible until the user acts again.
    pub(crate) apply_failure: Option<RepairApplyFailure>,
    /// Set once an apply actually left entries applied on disk. The loaded
    /// plan was proven against the library *before* that mutation, so it no
    /// longer reflects the library's current state and must not be trusted
    /// as evidence for a second apply without reloading/rescanning.
    pub(crate) plan_stale: bool,
    /// Overrides the journal directory [`Self::spawn_apply`] passes to
    /// [`apply_saved_plan_selected`]. `None` (the `Default`, and always the
    /// case in production) means production behaviour is completely
    /// unchanged: [`spawn_apply`](Self::spawn_apply) resolves
    /// [`archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir`]
    /// exactly as before. `Some(dir)` exists only so a test can point a real
    /// `confirm_apply` -> `spawn_apply` run at an isolated temporary
    /// directory instead of the developer's real Repair History - never a
    /// second journal system, never a different transaction format, and
    /// never a different default location; it is the exact same
    /// `RepairExecutionOptions::journal_dir` the production path already
    /// threads through unchanged.
    pub(crate) journal_dir_override: Option<PathBuf>,
    /// The pending "Scan library for repairs" setup dialog. `None` means the
    /// dialog is closed; opening it loads the DAT registry and configured
    /// library folders once, up front (see [`ScanSetupState`]).
    pub(crate) scan_setup: Option<ScanSetupState>,
    scan_job: Option<RepairScanJob>,
    /// The whole-library scan's own status, entirely separate from
    /// [`Self::error`] (which is only ever set by [`Self::load_plan`]'s
    /// file-load path) - a scan failure must never be presented as, or
    /// confused with, a plan-file parse failure.
    pub(crate) scan_status: Option<LibraryScanStatus>,
}

impl RepairReviewPageState {
    /// Loads a saved [`LibraryRepairPlan`] from a plan file. Read-only: reads
    /// the file, never writes, and never runs a scan, preflight, or re-proof.
    pub(crate) fn load_plan(&mut self, path: PathBuf) {
        let result = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read '{}': {error}", path.display()))
            .and_then(|text| {
                serde_json::from_str::<LibraryRepairPlan>(&text)
                    .map_err(|error| format!("could not parse repair plan: {error}"))
                    .map(|plan| (plan, CountsAvailability::from_raw_json(&text)))
            });
        match result {
            Ok((plan, availability)) => self.adopt_loaded_plan(plan, Some(path), availability),
            Err(message) => self.error = Some(message),
        }
    }

    /// Adopts a freshly obtained [`LibraryRepairPlan`] as the page's current
    /// plan - shared by [`Self::load_plan`] (a plan read from a file) and
    /// [`Self::handle_scan_message`] (a plan `run_library_scan` just
    /// produced in-process). Resets exactly the same state either way: a
    /// freshly loaded plan supersedes any previous apply result, failure,
    /// staleness warning, pending confirmation, or stale load error - all of
    /// those describe the *previous* plan (or a previous failed attempt to
    /// load one), not this one. `self.error` is cleared here (not just in
    /// `load_plan`) so a successful scan clears a stale error from an
    /// earlier failed file load exactly as a successful file load already
    /// did - see the regression this fixes:
    /// `a_successful_scan_clears_a_stale_load_error`. A running apply job is
    /// left alone: it was started against the plan it holds its own clone
    /// of, and finishes independently of what the page loads next.
    fn adopt_loaded_plan(
        &mut self,
        plan: LibraryRepairPlan,
        plan_path: Option<PathBuf>,
        availability: CountsAvailability,
    ) {
        self.plan = Some(plan);
        self.plan_path = plan_path;
        self.selected.clear();
        self.details_id = None;
        self.error = None;
        self.counts_availability = availability;
        self.plan_version = self.plan_version.wrapping_add(1);
        self.apply_confirm = None;
        self.apply_result = None;
        self.apply_failure = None;
        self.plan_stale = false;
    }

    /// Opens the "Scan library for repairs" setup dialog, loading the
    /// registered DAT sources and the configured library folders once - the
    /// same two inputs, resolved the same way, `dat_sources_page` already
    /// uses for its own per-source audit action. A no-op while a scan is
    /// already running.
    pub(crate) fn open_scan_setup(&mut self) {
        if self.scan_job.is_some() {
            return;
        }
        let (dat_sources, dat_load_error) = match default_dat_sources_config_path() {
            Ok(path) => match load_dat_sources_config_from(&path) {
                Ok(config) => {
                    let (registry, _problems) = DatSourceRegistry::from_config(&config);
                    (
                        registry
                            .entries()
                            .iter()
                            .filter(|entry| entry.enabled)
                            .cloned()
                            .collect(),
                        None,
                    )
                }
                Err(error) => (Vec::new(), Some(error.to_string())),
            },
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let library_folders = Config::load_default()
            .map(|config| config.source_folders)
            .unwrap_or_default();
        self.scan_setup = Some(ScanSetupState {
            dat_sources,
            dat_load_error,
            library_folders,
            selected_dat_id: None,
            chosen_scan_root: None,
        });
        self.scan_status = None;
    }

    /// Dismisses the scan setup dialog without starting anything.
    pub(crate) fn cancel_scan_setup(&mut self) {
        self.scan_setup = None;
    }

    /// Whether a scan is currently running.
    pub(crate) fn is_scan_running(&self) -> bool {
        self.scan_job.is_some()
    }

    /// Whether the setup dialog has both required inputs chosen and no scan
    /// is already running.
    pub(crate) fn can_start_scan(&self) -> bool {
        self.scan_job.is_none()
            && self.scan_setup.as_ref().is_some_and(|setup| {
                setup.selected_dat_id.is_some() && setup.chosen_scan_root.is_some()
            })
    }

    /// Spawns the background whole-library repair scan. The GUI never plans
    /// or scans on the UI thread and never builds a second planner: this
    /// calls the *exact* existing engine path
    /// ([`run_library_scan`] + [`plan_file_from_scan`]) on a dedicated
    /// thread, then relays only the terminal result back. Read-only - the
    /// scan never renames, moves, or deletes anything; the only mutation
    /// path anywhere on this page remains [`apply_saved_plan_selected`],
    /// invoked solely from [`Self::spawn_apply`].
    pub(crate) fn start_scan(&mut self) {
        if self.scan_job.is_some() {
            return;
        }
        let ready = self.scan_setup.as_ref().is_some_and(|setup| {
            setup.selected_dat_id.is_some() && setup.chosen_scan_root.is_some()
        });
        if !ready {
            return;
        }
        let Some(setup) = self.scan_setup.take() else {
            return;
        };
        let (Some(dat_id), Some(scan_root)) = (setup.selected_dat_id, setup.chosen_scan_root)
        else {
            return;
        };
        let Some(entry) = setup
            .dat_sources
            .iter()
            .find(|entry| entry.id == dat_id)
            .cloned()
        else {
            return;
        };

        let request = LibraryScanRequest {
            source_id: entry.id,
            source_display_name: entry.display_name,
            dat_path: entry.path,
            dat_kind: entry.kind,
            scan_root,
            limits: DatLimits::default(),
            profile: RepairProfile::CanonicalInPlace,
        };
        let trusted = TrustedRoots::from_paths([&request.scan_root]);
        let cancel = AtomicBool::new(false);
        let (sender, messages) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let message = match run_library_scan(&request, &trusted, &cancel, &|_| {}) {
                Ok(outcome) => {
                    RepairScanMessage::Completed(Box::new(plan_file_from_scan(&outcome)))
                }
                Err(error) => RepairScanMessage::Failed(error.to_string()),
            };
            let _ = sender.send(message);
        });

        self.scan_job = Some(RepairScanJob { messages });
        self.scan_status = Some(LibraryScanStatus::Scanning);
    }

    /// Drains the background scan job's channel, if one is running. Returns
    /// whether anything changed (so the caller can request a repaint).
    pub(crate) fn poll_scan(&mut self) -> bool {
        let Some(job) = self.scan_job.as_mut() else {
            return false;
        };
        match job.messages.try_recv() {
            Ok(message) => {
                self.handle_scan_message(message);
                self.scan_job = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.scan_status = Some(LibraryScanStatus::Failed(
                    "the scan worker disconnected unexpectedly".to_string(),
                ));
                self.scan_job = None;
                true
            }
        }
    }

    /// Applies one terminal message from the scan worker to page state. A
    /// failure never touches `self.plan`/`self.plan_path` at all - whatever
    /// was loaded before (or nothing) is left exactly as it was, so a failed
    /// scan can never leave a stale or half-loaded plan in the review UI.
    fn handle_scan_message(&mut self, message: RepairScanMessage) {
        match message {
            RepairScanMessage::Completed(plan) => {
                self.adopt_loaded_plan(*plan, None, CountsAvailability::CURRENT);
                self.scan_status = Some(LibraryScanStatus::Completed);
            }
            RepairScanMessage::Failed(detail) => {
                self.scan_status = Some(LibraryScanStatus::Failed(detail));
            }
        }
    }

    pub(crate) fn set_filter(&mut self, filter: Option<RepairFilter>) {
        self.filter = filter;
    }

    /// The current filtered row view. Rebuilds via [`build_rows`] only when
    /// the loaded plan or the filter has changed since the last call; a
    /// cache hit is a cheap `Rc` clone rather than a full row rebuild. No
    /// plan means no rows and no cache to keep.
    pub(crate) fn rows(&mut self) -> Rc<Vec<RepairReviewRow>> {
        let Some(plan) = self.plan.as_ref() else {
            self.rows_cache = None;
            return Rc::new(Vec::new());
        };
        let cache_hit = matches!(
            &self.rows_cache,
            Some((version, filter, _)) if *version == self.plan_version && *filter == self.filter
        );
        if !cache_hit {
            self.rows_cache = Some((
                self.plan_version,
                self.filter,
                Rc::new(build_rows(plan, self.filter)),
            ));
        }
        Rc::clone(&self.rows_cache.as_ref().expect("just set above").2)
    }

    pub(crate) fn toggle_selected(&mut self, id: &RepairProposalId) {
        if !self.selected.remove(id) {
            self.selected.insert(id.clone());
        }
    }

    /// Selects every Safe row in the currently visible (filtered) list.
    pub(crate) fn select_all(&mut self, rows: &[RepairReviewRow]) {
        for row in rows {
            if let Some(id) = &row.proposal_id {
                self.selected.insert(id.clone());
            }
        }
    }

    pub(crate) fn select_none(&mut self) {
        self.selected.clear();
    }

    pub(crate) fn set_details(&mut self, id: Option<RepairProposalId>) {
        self.details_id = id;
    }

    pub(crate) fn proposal_by_id(&self, id: &RepairProposalId) -> Option<&RepairProposal> {
        self.plan
            .as_ref()
            .and_then(|plan| plan.repair_plan.proposals.iter().find(|p| &p.id == id))
    }

    /// Finds a selected proposal's id back from an applied entry's source
    /// path. Valid because [`apply_saved_plan_selected`]'s full-plan re-proof
    /// guarantees a fresh proposal's `source_path` is byte-identical to the
    /// saved one this page loaded - so a match against the loaded plan's own
    /// proposals is exact, never a guess.
    fn proposal_id_for_source(&self, source: &std::path::Path) -> Option<RepairProposalId> {
        self.plan.as_ref().and_then(|plan| {
            plan.repair_plan
                .proposals
                .iter()
                .find(|proposal| proposal.source_path == source)
                .map(|proposal| proposal.id.clone())
        })
    }

    /// The selected ids that are still an executable Safe proposal in the
    /// loaded plan - an ordinary rename *or* a duplicate-quarantine move
    /// alike - in deterministic (`BTreeSet`) order. This is a defensive
    /// re-check, not the safety boundary: [`apply_saved_plan_selected`]
    /// re-validates everything again against a fresh scan regardless (full
    /// re-proof, then [`archivefs_core::repair::plan::select_repair_plan_subset`]'s
    /// own conflict-free-whole-plan requirement, then per-backend re-proof
    /// immediately before mutation). It exists only so the enable rule and
    /// the confirmation dialog can never offer to "apply" an id the loaded
    /// plan itself no longer backs (already NeedsReview/Blocked, or absent).
    ///
    /// A NeedsReview or Blocked proposal is never actionable
    /// (`RepairProposal::actionable()` requires `SafetyState::Safe`), so it
    /// can never appear here regardless of whether it is a quarantine move -
    /// this page never has a `RepairProposalId` for one anyway, since
    /// NeedsReview/Blocked rows come from the report's id-less `PlanItem`s
    /// (see [`build_rows`]), never from `plan.repair_plan.proposals`.
    pub(crate) fn actionable_selected_ids(&self) -> Vec<RepairProposalId> {
        let Some(plan) = self.plan.as_ref() else {
            return Vec::new();
        };
        self.selected
            .iter()
            .filter(|id| {
                plan.repair_plan
                    .proposals
                    .iter()
                    .any(|proposal| &proposal.id == *id && proposal.actionable())
            })
            .cloned()
            .collect()
    }

    /// Whether "Apply Selected" may be invoked right now: a plan is loaded,
    /// at least one selected proposal is still actionable, and no apply is
    /// already running.
    pub(crate) fn can_apply(&self) -> bool {
        self.plan.is_some() && !self.apply_running && !self.actionable_selected_ids().is_empty()
    }

    /// Whether a background apply job is in flight.
    pub(crate) fn is_apply_running(&self) -> bool {
        self.apply_job.is_some()
    }

    /// Opens the confirmation dialog, freezing exactly what will be sent to
    /// the backend if the user confirms. A no-op when `can_apply()` does not
    /// hold, so a stale click (e.g. after the last selected id was
    /// deselected) can never open a confirmation for nothing.
    pub(crate) fn open_apply_confirmation(&mut self) {
        if !self.can_apply() {
            return;
        }
        let Some(plan) = self.plan.as_ref() else {
            return;
        };
        let selected = self.actionable_selected_ids();
        let quarantine_count = selected
            .iter()
            .filter(|id| {
                plan.repair_plan
                    .proposals
                    .iter()
                    .any(|proposal| &proposal.id == *id && proposal.is_duplicate_quarantine())
            })
            .count();
        let rename_count = selected.len() - quarantine_count;
        self.apply_confirm = Some(RepairApplyConfirmation {
            selected,
            scan_root: plan.scan_root.clone(),
            dat_path: plan.dat_path.clone(),
            rename_count,
            quarantine_count,
        });
        self.apply_confirm_focus_cancel = true;
    }

    /// Dismisses the confirmation dialog without applying anything.
    pub(crate) fn cancel_apply_confirmation(&mut self) {
        self.apply_confirm = None;
    }

    /// Confirms the pending apply: spawns the background worker with exactly
    /// the frozen [`RepairApplyConfirmation`], then closes the dialog.
    ///
    /// Refuses (closing the dialog without doing anything) if an apply is
    /// already running or the plan was unloaded while the dialog was open -
    /// both defensive, since the button that opens this dialog and the one
    /// that would start a second job are both meant to already be disabled.
    pub(crate) fn confirm_apply(&mut self) {
        let Some(confirmation) = self.apply_confirm.take() else {
            return;
        };
        if self.apply_running {
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        self.spawn_apply(plan, confirmation);
    }

    /// Spawns the background apply worker. The GUI never mutates the
    /// filesystem itself: this calls [`apply_saved_plan_selected`] on a
    /// dedicated thread with the *exact* saved plan this page loaded (never
    /// a GUI-built [`archivefs_core::repair::plan::RepairPlan`]) and the
    /// exact frozen selection, and relays only the result back.
    fn spawn_apply(&mut self, plan: LibraryRepairPlan, confirmation: RepairApplyConfirmation) {
        if self.apply_job.is_some() {
            return;
        }
        let root = PathBuf::from(&confirmation.scan_root);
        let dat = PathBuf::from(&confirmation.dat_path);
        let current_generation = plan.generation;
        let selected = confirmation.selected;
        let trusted = TrustedRoots::from_paths([&root]);
        // `None` in production: resolves the exact same default journal
        // directory as before. `Some(dir)` only in tests - see
        // `journal_dir_override`'s doc.
        let journal_dir = self.journal_dir_override.clone().unwrap_or_else(|| {
            archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir()
                .unwrap_or_else(|_| PathBuf::from("rename-transactions"))
        });
        let options = RepairExecutionOptions {
            trusted,
            journal_dir,
        };
        // Never exposed to cancellation in this slice (see `RepairApplyJob`);
        // still required by `apply_saved_plan_selected`'s signature.
        let cancel = AtomicBool::new(false);
        let (sender, messages) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let result = apply_saved_plan_selected(
                &plan,
                &root,
                &dat,
                current_generation,
                &selected,
                &options,
                &cancel,
            );
            let message = match result {
                Ok(outcome) => RepairApplyMessage::Applied(Box::new(outcome)),
                // A later duplicate-quarantine group failed to build/apply:
                // never discard whatever already succeeded (any rename batch,
                // plus zero or more earlier quarantine groups) - see
                // `ApplySavedPlanSelectedError::QuarantineBuild`/`QuarantineApply`'s
                // doc and `RepairApplyMessage::Partial`.
                Err(ApplySavedPlanSelectedError::QuarantineBuild { completed, detail }) => {
                    RepairApplyMessage::Partial {
                        completed,
                        failure: RepairApplyFailure {
                            label: "Quarantine apply could not be built",
                            detail,
                        },
                    }
                }
                Err(ApplySavedPlanSelectedError::QuarantineApply { completed, detail }) => {
                    RepairApplyMessage::Partial {
                        completed,
                        failure: RepairApplyFailure {
                            label: "Quarantine apply failed",
                            detail,
                        },
                    }
                }
                Err(error) => RepairApplyMessage::Failed(RepairApplyFailure::from_error(error)),
            };
            let _ = sender.send(message);
        });

        self.apply_job = Some(RepairApplyJob { messages });
        self.apply_running = true;
        self.apply_result = None;
        self.apply_failure = None;
    }

    /// Drains the background apply job's channel, if one is running. Returns
    /// whether anything changed (so the caller can request a repaint).
    ///
    /// On a successful apply, only the ids whose entry actually reached
    /// [`EntryState::Applied`] are cleared from the selection - an id whose
    /// entry was skipped, failed, or was rolled back stays selected, since it
    /// was not, in the end, applied. On any failure the selection is left
    /// entirely untouched: the caller decides what to do next.
    pub(crate) fn poll_apply(&mut self) -> bool {
        let Some(job) = self.apply_job.as_mut() else {
            return false;
        };
        // The job sends exactly one terminal message and then hangs up, so
        // one `try_recv` per frame is enough: there is never a backlog to
        // drain in a loop the way a progress-reporting job needs.
        let received = job.messages.try_recv();
        match received {
            Ok(message) => {
                self.handle_apply_message(message);
                self.apply_job = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.apply_running = false;
                self.apply_job = None;
                true
            }
        }
    }

    /// Applies one terminal message from the apply worker to page state.
    ///
    /// On a successful (or partially successful) apply, only the ids whose
    /// entry actually reached [`EntryState::Applied`] - across the rename
    /// batch and every quarantine group that ran - are cleared from the
    /// selection; an id whose entry was skipped, failed, rolled back, or
    /// never reached (a later group that never ran after an earlier one
    /// failed) stays selected. On a total failure (nothing ran at all) the
    /// selection is left entirely untouched: the caller decides what to do
    /// next.
    fn handle_apply_message(&mut self, message: RepairApplyMessage) {
        match message {
            RepairApplyMessage::Applied(outcome) => {
                self.apply_combined_result(*outcome, None);
            }
            RepairApplyMessage::Partial { completed, failure } => {
                self.apply_combined_result(*completed, Some(failure));
            }
            RepairApplyMessage::Failed(failure) => {
                self.apply_failure = Some(failure);
                self.apply_result = None;
                self.apply_running = false;
            }
        }
    }

    /// Shared by both the fully-successful and partial-failure arms of
    /// [`Self::handle_apply_message`]: records every applied source's id as
    /// no longer selected, marks the plan stale if anything actually landed
    /// on disk, and stores the result (and, for a partial failure, the
    /// reason the run stopped) - never silently dropping a completed
    /// rename batch or quarantine group just because a later one failed.
    fn apply_combined_result(
        &mut self,
        outcome: CombinedApplyResult,
        failure: Option<RepairApplyFailure>,
    ) {
        let mut applied_sources: Vec<PathBuf> = Vec::new();
        let mut any_applied = false;
        if let Some(rename) = &outcome.rename {
            any_applied |= rename.summary.applied > 0;
            applied_sources.extend(
                rename
                    .transaction
                    .entries
                    .iter()
                    .filter(|entry| entry.state == EntryState::Applied)
                    .map(|entry| entry.source_path.clone()),
            );
        }
        for group in &outcome.quarantine {
            any_applied |= group.result.summary.applied > 0;
            applied_sources.extend(
                group
                    .result
                    .transaction
                    .entries
                    .iter()
                    .filter(|entry| entry.state == EntryState::Applied)
                    .map(|entry| entry.source_path.clone()),
            );
        }
        for source in &applied_sources {
            if let Some(id) = self.proposal_id_for_source(source) {
                self.selected.remove(&id);
            }
        }
        // Entries that stayed applied (not rolled back) mean the library
        // changed under the loaded plan; a rescan/reload is required before
        // this plan can back another apply.
        if any_applied {
            self.plan_stale = true;
        }
        let has_any_result = outcome.rename.is_some() || !outcome.quarantine.is_empty();
        self.apply_result = has_any_result.then_some(outcome);
        self.apply_failure = failure;
        self.apply_running = false;
    }
}

/// Draws the page.
pub(crate) fn show_repair_review_page(ui: &mut egui::Ui, state: &mut RepairReviewPageState) {
    widgets::page_header_with_icon(
        ui,
        crate::ui::icons::VERIFY,
        "Repair Review",
        "Preview a saved whole-library plan, then apply exactly the repairs you select.",
    );

    // The confirmation dialog and the scan setup dialog float above
    // everything else on the page and are drawn unconditionally so either
    // stays visible (and actionable) no matter what else changed underneath
    // it this frame.
    show_apply_confirmation_dialog(ui, state);
    show_scan_setup_dialog(ui, state);

    // Load control.
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            if widgets::action_button(ui, "Load repair plan", widgets::ActionStyle::Primary, true)
                .clicked()
            {
                open_plan_dialog(state);
            }
            if widgets::action_button(
                ui,
                "Scan library for repairs",
                widgets::ActionStyle::Secondary,
                !state.is_scan_running(),
            )
            .clicked()
            {
                state.open_scan_setup();
            }
            if let Some(path) = &state.plan_path {
                ui.label(egui::RichText::new(path.display().to_string()).color(theme::muted(ui)));
            }
        });
        ui.label(
            egui::RichText::new(
                "'Load repair plan' loads a plan saved by the CLI's 'repair scan --plan-out' \
                 contract. 'Scan library for repairs' runs the same whole-library scan directly \
                 and loads its result here. Either way, this page only previews - nothing is \
                 changed on disk until you select repairs and apply them.",
            )
            .color(theme::muted(ui)),
        );
    });

    show_scan_status(ui, state);

    if let Some(error) = &state.error {
        ui.add_space(6.0);
        let message = match &state.plan {
            Some(plan) => format!(
                "{error} The plan shown below ('{}') is still the previously loaded plan — it was not replaced.",
                plan.source_display_name
            ),
            None => error.clone(),
        };
        widgets::banner(
            ui,
            "Could not load the new repair plan",
            &message,
            widgets::StatusTone::Blocked,
        );
    }

    let Some(plan) = state.plan.as_ref() else {
        ui.add_space(12.0);
        let load_requested = widgets::empty_state(
            ui,
            "No repair plan loaded",
            "Load a saved repair plan to preview its proposals.",
            Some("Load repair plan"),
        );
        if load_requested {
            open_plan_dialog(state);
        }
        return;
    };

    // Summary card.
    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        ui.label(
            egui::RichText::new(&plan.source_display_name)
                .size(18.0)
                .strong(),
        );
        ui.label(
            egui::RichText::new(summary_line(&plan.report.counts, state.counts_availability))
                .monospace(),
        );
        ui.label(
            egui::RichText::new(format!(
                "{} files scanned · {}",
                plan.files_scanned, plan.scan_root
            ))
            .color(theme::muted(ui)),
        );
        // Backend/provenance identifiers: not user-facing on their own (a raw
        // generation number reads as noise in the primary summary), so they
        // live in a collapsed technical-details section instead.
        ui.collapsing("Technical details", |ui| {
            detail_label(ui, "Generation", &plan.generation.to_string());
            detail_label(ui, "Profile", &plan.profile);
            detail_label(ui, "Source id", &plan.source_id);
            detail_label(ui, "DAT path", &plan.dat_path);
        });
    });

    if plan.truncated {
        ui.add_space(6.0);
        widgets::banner(
            ui,
            "Scan was truncated",
            "The plan covers only part of the library. Counts are provisional.",
            widgets::StatusTone::Warning,
        );
    }
    if plan.report.counts.scan_errors > 0 {
        ui.add_space(6.0);
        widgets::banner(
            ui,
            "Scan errors",
            &format!(
                "{} scan error(s) are reported; the plan may be incomplete.",
                plan.report.counts.scan_errors
            ),
            widgets::StatusTone::Warning,
        );
    }

    // Rows: filter, virtualised fixed-height list, selection, disabled apply.
    // Cached by (plan version, filter); rebuilt only when either changes.
    let rows = state.rows();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Filter:");
        let all = state.filter.is_none();
        if ui.selectable_label(all, "All").clicked() {
            state.set_filter(None);
        }
        for filter in RepairFilter::ALL {
            let selected = state.filter == Some(filter);
            if ui.selectable_label(selected, filter.label()).clicked() {
                state.set_filter(if selected { None } else { Some(filter) });
            }
        }
    });
    ui.add_space(6.0);

    if rows.is_empty() {
        ui.add_space(12.0);
        widgets::empty_state(
            ui,
            "Nothing to repair",
            "No rows match the current filter.",
            None,
        );
    } else {
        let row_height = ui.spacing().interact_size.y.max(30.0);
        // The list takes most of the remaining height but not all of it, so
        // the selection controls and details panel below stay reachable
        // without scrolling the whole page.
        let list_height = (ui.available_height() * 0.6).clamp(row_height * 2.0, row_height * 12.0);
        egui::ScrollArea::vertical()
            .id_salt("repair_review_rows")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows.len(), |ui, row_range| {
                for visible_index in row_range {
                    let row = &rows[visible_index];
                    show_row(ui, row, state);
                }
            });
        ui.add_space(8.0);

        let safe_count = rows
            .iter()
            .filter(|row| row.kind == RepairRowKind::Safe)
            .count();
        ui.horizontal(|ui| {
            if widgets::action_button(
                ui,
                "Select all",
                widgets::ActionStyle::Secondary,
                safe_count > 0,
            )
            .clicked()
            {
                state.select_all(&rows);
            }
            if widgets::action_button(
                ui,
                "Select none",
                widgets::ActionStyle::Secondary,
                !state.selected.is_empty(),
            )
            .clicked()
            {
                state.select_none();
            }
            ui.separator();
            let can_apply = state.can_apply();
            // The exact count `open_apply_confirmation` would freeze into
            // the confirmation dialog and send to the backend - never
            // `state.selected.len()`, which also counts stale/no-longer-
            // actionable ids (already applied, or no longer Safe in the
            // loaded plan) that will never actually be submitted.
            let actionable_count = state.actionable_selected_ids().len();
            let apply_label = if state.apply_running {
                "Applying…".to_string()
            } else {
                format!("Apply Selected ({actionable_count})")
            };
            let apply =
                widgets::action_button(ui, apply_label, widgets::ActionStyle::Primary, can_apply);
            let apply_clicked = apply.clicked();
            if !can_apply {
                let hover = if state.apply_running {
                    "An apply is already running."
                } else if actionable_count == 0 {
                    "Select at least one Safe repair to apply."
                } else {
                    "Apply Selected is not available."
                };
                apply.on_disabled_hover_text(hover);
            }
            if apply_clicked {
                state.open_apply_confirmation();
            }
        });

        show_apply_result(ui, state);
        show_apply_failure(ui, state);
    }

    // Details panel for the selected Safe proposal, outside the virtualised
    // list so rows stay fixed-height.
    if let Some(id) = state.details_id.clone() {
        ui.add_space(8.0);
        match state.proposal_by_id(&id) {
            Some(proposal) => show_details(ui, &id, proposal),
            None => state.details_id = None,
        }
    }
}

/// The "Apply Selected" confirmation dialog. A no-op draw when nothing is
/// pending. Cancel is favoured: its button claims focus the first frame the
/// dialog appears, and closing the window (e.g. Esc) is wired to Cancel, not
/// Apply.
fn show_apply_confirmation_dialog(ui: &mut egui::Ui, state: &mut RepairReviewPageState) {
    let Some(confirmation) = state.apply_confirm.clone() else {
        return;
    };
    let mut focus_cancel = state.apply_confirm_focus_cancel;
    let mut cancel_clicked = false;
    let mut apply_clicked = false;
    let mut open = true;

    egui::Window::new("Apply selected repairs?")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} repair(s) selected",
                    confirmation.selected.len()
                ))
                .strong(),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} rename action(s) · {} quarantine action(s)",
                    confirmation.rename_count, confirmation.quarantine_count
                ))
                .color(theme::muted(ui)),
            );
            detail_label(ui, "Scan root", &confirmation.scan_root);
            detail_label(ui, "DAT source", &confirmation.dat_path);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "The selected files will be renamed or moved on disk. The saved plan is \
                     re-proven against a fresh scan first; if anything has changed, nothing is \
                     touched.",
                )
                .color(theme::muted(ui)),
            );
            if confirmation.quarantine_count > 0 {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} of these are duplicate quarantine action(s): the redundant file is \
                         moved into a '.emuwiz-quarantine' folder inside its own trusted root, \
                         not permanently deleted. The move is fully reversible from Repair \
                         History's Undo, the same as any other repair.",
                        confirmation.quarantine_count
                    ))
                    .color(theme::muted(ui)),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let cancel = ui.add(egui::Button::new("Cancel"));
                if focus_cancel {
                    cancel.request_focus();
                    focus_cancel = false;
                }
                if cancel.clicked() {
                    cancel_clicked = true;
                }
                if ui.add(egui::Button::new("Apply")).clicked() {
                    apply_clicked = true;
                }
            });
        });

    state.apply_confirm_focus_cancel = focus_cancel;
    if cancel_clicked || !open {
        state.cancel_apply_confirmation();
    } else if apply_clicked {
        state.confirm_apply();
    }
}

/// Renders one [`RepairTransactionResult`]'s counts and reverify entries -
/// shared by the rename batch and every quarantine group in
/// [`show_apply_result`], since both are the same result shape.
fn show_transaction_result_body(ui: &mut egui::Ui, result: &RepairTransactionResult) {
    detail_label(ui, "Transaction id", &result.summary.transaction_id);
    detail_label(ui, "Requested", &result.summary.requested.to_string());
    detail_label(ui, "Applied", &result.summary.applied.to_string());
    detail_label(ui, "Failed", &result.summary.failed.to_string());
    detail_label(ui, "Skipped", &result.summary.skipped.to_string());
    detail_label(ui, "Rollback", result.summary.rollback.label());
    if !result.reverify.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Reverify").strong());
        for entry in &result.reverify {
            let tone = match entry.outcome {
                RepairReverifyOutcome::Verified => widgets::StatusTone::Success,
                RepairReverifyOutcome::Missing | RepairReverifyOutcome::Changed => {
                    widgets::StatusTone::Blocked
                }
            };
            ui.horizontal(|ui| {
                widgets::status_badge(ui, entry.outcome.label(), tone);
                ui.label(
                    egui::RichText::new(format!(
                        "{} → {}",
                        entry.source_path.display(),
                        entry.destination_path.display()
                    ))
                    .monospace()
                    .small(),
                );
            });
        }
    }
}

/// The headline for one duplicate-quarantine group's result card, chosen
/// from the group's own typed [`TransactionSummary`] counts
/// (`RepairTransactionResult::summary`) - never "complete" merely because a
/// [`QuarantineApplyResult`](archivefs_core::repair::library::QuarantineApplyResult)
/// exists to render.
///
/// A `QuarantineApplyResult` is present in [`CombinedApplyResult::quarantine`]
/// whenever [`apply_quarantine_transaction`](archivefs_core::repair::quarantine::apply_quarantine_transaction)
/// returned `Ok(_)`, but `Ok` only means the call completed cleanly, not that
/// every (or any) entry was actually applied: the per-entry Layer 2 re-proof
/// immediately before each move (or a mid-batch cancellation) can leave
/// `summary.applied == 0` even though the whole-batch Layer 1 pre-proof
/// already passed. `AbortAll` semantics also mean a single group's own
/// transaction can itself be partial - an earlier entry applied, a later one
/// failed within the same content-hash group - so "some but not all applied"
/// is reported distinctly from a genuine full completion.
fn quarantine_group_headline(result: &RepairTransactionResult) -> &'static str {
    let summary = &result.summary;
    if summary.applied == 0 {
        "Quarantine group produced no applied changes"
    } else if summary.failed > 0 || summary.skipped > 0 {
        "Quarantine group partially applied"
    } else {
        "Quarantine group complete"
    }
}

/// Post-apply feedback for the last completed (or partially completed) run:
/// the rename batch's result, if any proposal in the selection was an
/// ordinary rename, and each duplicate-quarantine group's own result, if any
/// were selected. A group applied through
/// [`archivefs_core::repair::quarantine::apply_quarantine_transaction`] is
/// rendered with its own transaction id and counts, distinct from the rename
/// batch - never merged into one summary, since they are independent
/// journaled transactions. Shown until superseded by the next apply or a
/// newly loaded plan; a partial result (some groups applied, a later one
/// failed - see [`RepairApplyMessage::Partial`]) is rendered exactly the same
/// way, so completed work is never hidden just because the run as a whole did
/// not fully succeed. [`show_apply_failure`] renders the reason it stopped.
fn show_apply_result(ui: &mut egui::Ui, state: &RepairReviewPageState) {
    let Some(result) = state.apply_result.as_ref() else {
        return;
    };
    if let Some(rename) = &result.rename {
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            ui.label(
                egui::RichText::new("Rename batch complete")
                    .size(16.0)
                    .strong(),
            );
            show_transaction_result_body(ui, rename);
        });
    }
    for group in &result.quarantine {
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(quarantine_group_headline(&group.result))
                        .size(16.0)
                        .strong(),
                );
                widgets::status_badge(ui, "Quarantine duplicate", widgets::StatusTone::Info);
            });
            detail_label(ui, "Survivor", &group.survivor_path.display().to_string());
            show_transaction_result_body(ui, &group.result);
        });
    }
    if state.plan_stale {
        ui.add_space(6.0);
        widgets::banner(
            ui,
            "This loaded plan is now stale",
            "The library changed on disk. Rescan (or reload a fresh saved plan) before applying \
             again — this plan no longer reflects the library's current state.",
            widgets::StatusTone::Warning,
        );
    }
}

/// The last apply's refusal or error, when there is one. Shown until
/// superseded by the next apply attempt or a newly loaded plan.
///
/// When `apply_result` is *also* set, this is a partial failure: one or more
/// groups shown above by [`show_apply_result`] already succeeded before this
/// one stopped the run. The banner says so explicitly, so this never reads
/// as "nothing happened" when something in fact did.
fn show_apply_failure(ui: &mut egui::Ui, state: &RepairReviewPageState) {
    let Some(failure) = state.apply_failure.as_ref() else {
        return;
    };
    ui.add_space(8.0);
    let message = if state.apply_result.is_some() {
        format!(
            "{} The result(s) shown above already completed and were journaled before this \
             happened; nothing already applied was undone.",
            failure.detail
        )
    } else {
        failure.detail.clone()
    };
    widgets::banner(ui, failure.label, &message, widgets::StatusTone::Blocked);
}

/// Opens the plan picker and loads the chosen file. Shared by the header
/// button and the empty-state action; read-only.
fn open_plan_dialog(state: &mut RepairReviewPageState) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Repair plan (JSON)", &["json"])
        .pick_file()
    {
        state.load_plan(path);
    }
}

/// The simple Scanning / Completed / Failed status banner for the
/// whole-library scan action. Deliberately minimal - no elaborate progress
/// reporting in this stage.
fn show_scan_status(ui: &mut egui::Ui, state: &RepairReviewPageState) {
    let Some(status) = &state.scan_status else {
        return;
    };
    ui.add_space(6.0);
    match status {
        LibraryScanStatus::Scanning => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Scanning library for repairs…");
            });
        }
        LibraryScanStatus::Completed => {
            widgets::banner(
                ui,
                "Scan complete",
                "The whole-library scan finished; its plan is shown below.",
                widgets::StatusTone::Success,
            );
        }
        LibraryScanStatus::Failed(detail) => {
            widgets::banner(ui, "Scan failed", detail, widgets::StatusTone::Blocked);
        }
    }
}

/// The "Scan library for repairs" setup dialog: choose a registered DAT
/// source, then a scan root, then start. A no-op draw when nothing is
/// pending. Cancel is favoured, same as the apply confirmation dialog:
/// closing the window (e.g. Esc) is wired to Cancel, not Start.
fn show_scan_setup_dialog(ui: &mut egui::Ui, state: &mut RepairReviewPageState) {
    if state.scan_setup.is_none() {
        return;
    }
    let mut open = true;
    let mut cancel_clicked = false;
    let mut start_clicked = false;

    let can_start = state.can_start_scan();
    egui::Window::new("Scan library for repairs")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            let Some(setup) = state.scan_setup.as_mut() else {
                return;
            };
            ui.label(egui::RichText::new("1. Choose a DAT catalogue").strong());
            if let Some(error) = &setup.dat_load_error {
                widgets::banner(
                    ui,
                    "Could not read the DAT sources registry",
                    error,
                    widgets::StatusTone::Blocked,
                );
            } else if setup.dat_sources.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No enabled DAT sources are registered. Add one on the DAT Sources page \
                         first.",
                    )
                    .color(theme::muted(ui)),
                );
            } else {
                for entry in &setup.dat_sources {
                    let selected = setup.selected_dat_id.as_deref() == Some(entry.id.as_str());
                    let clicked = egui::Frame::new()
                        .fill(if selected {
                            ui.visuals().selection.bg_fill.gamma_multiply(0.35)
                        } else {
                            theme::card_fill(ui)
                        })
                        .stroke(theme::border(ui))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(&entry.display_name).strong());
                                ui.label(
                                    egui::RichText::new(entry.path.display().to_string())
                                        .color(theme::muted(ui))
                                        .small(),
                                );
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    if clicked.clicked() {
                        setup.selected_dat_id = Some(entry.id.clone());
                    }
                }
            }

            ui.add_space(8.0);
            ui.label(egui::RichText::new("2. Choose a library folder to scan").strong());
            for folder in &setup.library_folders {
                let selected = setup.chosen_scan_root.as_deref() == Some(folder.as_path());
                let clicked = egui::Frame::new()
                    .fill(if selected {
                        ui.visuals().selection.bg_fill.gamma_multiply(0.35)
                    } else {
                        theme::card_fill(ui)
                    })
                    .stroke(theme::border(ui))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(folder.display().to_string());
                    })
                    .response
                    .interact(egui::Sense::click());
                if clicked.clicked() {
                    setup.chosen_scan_root = Some(folder.clone());
                }
            }
            if widgets::action_button(
                ui,
                "Choose another folder…",
                widgets::ActionStyle::Quiet,
                true,
            )
            .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a library folder to scan")
                    .pick_folder()
            {
                setup.chosen_scan_root = Some(path);
            }
            if let Some(root) = &setup.chosen_scan_root {
                ui.label(
                    egui::RichText::new(format!("Selected: {}", root.display()))
                        .color(theme::muted(ui)),
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("Cancel")).clicked() {
                    cancel_clicked = true;
                }
                if ui
                    .add_enabled(can_start, egui::Button::new("Start scan"))
                    .clicked()
                {
                    start_clicked = true;
                }
            });
        });

    if cancel_clicked || !open {
        state.cancel_scan_setup();
    } else if start_clicked {
        state.start_scan();
    }
}

/// One fixed-height virtualised row.
///
/// The row's rect must be anchored at [`egui::Ui::cursor`], not
/// [`egui::Ui::min_rect`]: `min_rect().min` is the *top-left corner* of
/// everything the `Ui` has laid out so far, which does not move as rows are
/// added underneath it. Anchoring there placed every row's rect at the same
/// position, so all 26 Safe rows painted on top of one another. `cursor()`
/// (or, equivalently, `next_widget_position()`) is where the *next* widget
/// actually goes, and it advances every time `allocate_rect` runs.
fn show_row(ui: &mut egui::Ui, row: &RepairReviewRow, state: &mut RepairReviewPageState) {
    let row_height = ui.spacing().interact_size.y.max(30.0);
    let rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), row_height),
    );
    // Advances the cursor past `rect`; nothing below should advance it again.
    ui.allocate_rect(rect, egui::Sense::hover());
    let selected = row
        .proposal_id
        .as_ref()
        .is_some_and(|id| state.selected.contains(id));
    if selected {
        ui.painter().rect_filled(
            rect,
            0.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.35),
        );
    }

    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );

    match row.kind {
        RepairRowKind::Safe => {
            let Some(id) = row.proposal_id.clone() else {
                return;
            };
            let mut checked = state.selected.contains(&id);
            if row_ui.checkbox(&mut checked, "").changed() {
                state.toggle_selected(&id);
            }
        }
        RepairRowKind::NeedsReview | RepairRowKind::Blocked => {
            // Align with the checkbox column; these rows are never selectable.
            row_ui.add_space(24.0);
        }
    }

    widgets::status_badge(&mut row_ui, row.kind.label(), row.kind.tone());
    // A duplicate-quarantine proposal is visually distinct from an ordinary
    // rename row: its own action-label badge, always immediately after the
    // safety badge, and (when the evidence is present) a second badge naming
    // the `DuplicateContent` evidence it rests on - never inferred, only
    // shown when `build_rows` actually found it on the proposal.
    if row.is_duplicate_quarantine {
        widgets::status_badge(
            &mut row_ui,
            "Quarantine duplicate",
            widgets::StatusTone::Info,
        );
    }
    if row.has_duplicate_content_evidence {
        widgets::status_badge(
            &mut row_ui,
            "Duplicate content evidence",
            widgets::StatusTone::Active,
        );
    }

    // Ordinary rename rows keep their exact prior "source → destination"
    // text. A quarantine row additionally names the kept survivor, so the
    // three paths this move involves (source, quarantine destination,
    // survivor) are all visible without opening Details.
    let path_text = match (&row.destination, &row.survivor) {
        (Some(destination), Some(survivor)) => {
            format!("{} → {} (survivor: {survivor})", row.source, destination)
        }
        (Some(destination), None) => format!("{} → {}", row.source, destination),
        (None, _) => row.source.clone(),
    };
    let path_width = (row_ui.available_width() * 0.5).max(140.0);
    row_ui
        .add_sized(
            [path_width, row_height],
            egui::Label::new(egui::RichText::new(path_text.clone()).monospace()).truncate(),
        )
        .on_hover_text(path_text);

    if !row.reason.is_empty() {
        let reason_width = (row_ui.available_width() - 78.0).max(60.0);
        row_ui
            .add_sized(
                [reason_width, row_height],
                egui::Label::new(
                    egui::RichText::new(format!("({})", row.reason))
                        .small()
                        .color(theme::muted(ui)),
                )
                .truncate(),
            )
            .on_hover_text(row.reason.clone());
    }

    if row.kind == RepairRowKind::Safe {
        let id = row
            .proposal_id
            .clone()
            .unwrap_or_else(|| unreachable!("Safe rows always carry a proposal id"));
        if widgets::action_button(&mut row_ui, "Details", widgets::ActionStyle::Quiet, true)
            .clicked()
        {
            state.set_details(Some(id));
        }
    }
}

/// The details panel for one Safe proposal. Only data the backend already
/// carries is shown; nothing is manufactured for thin PlanItem rows.
fn show_details(ui: &mut egui::Ui, id: &RepairProposalId, proposal: &RepairProposal) {
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Proposal details").size(17.0).strong());
            ui.label(egui::RichText::new(id.to_string()).color(theme::muted(ui)));
            widgets::status_badge(ui, "Safe", widgets::StatusTone::Success);
            if proposal.is_duplicate_quarantine() {
                widgets::status_badge(ui, "Quarantine duplicate", widgets::StatusTone::Info);
            }
        });
        ui.add_space(4.0);
        detail_label(ui, "Source", &proposal.source_path.display().to_string());
        if let Some(destination) = proposal.destination() {
            let label = if proposal.is_duplicate_quarantine() {
                "Quarantine destination"
            } else {
                "Destination"
            };
            detail_label(ui, label, &destination.display().to_string());
        }
        if let Some(survivor) = &proposal.survivor_path {
            detail_label(ui, "Survivor (kept file)", &survivor.display().to_string());
        }
        if !proposal.reason.is_empty() {
            detail_label(ui, "Reason", &proposal.reason);
        }
        if !proposal.warnings.is_empty() {
            ui.label(egui::RichText::new("Warnings").strong());
            for warning in &proposal.warnings {
                ui.add(egui::Label::new(format!("• {warning}")).wrap());
            }
        }
        if !proposal.evidence.is_empty() {
            ui.label(egui::RichText::new("Evidence").strong());
            for evidence in &proposal.evidence {
                ui.add(
                    egui::Label::new(format!("• {} — {}", evidence.kind.label(), evidence.detail))
                        .wrap(),
                );
            }
        }
        if let Some(verdict) = &proposal.verdict_label {
            detail_label(ui, "Verdict", verdict);
        }
        if let Some(game) = &proposal.game_name {
            detail_label(ui, "Game", game);
        }
        if let Some(rom) = &proposal.rom_name {
            detail_label(ui, "ROM", rom);
        }
        if proposal.is_outer_archive {
            ui.add(
                egui::Label::new(format!(
                    "Whole outer archive · set verification: {}",
                    if proposal.is_outer_archive_verified {
                        "verified"
                    } else {
                        "not verified"
                    }
                ))
                .wrap(),
            );
        }
    });
}

fn detail_label(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add_sized(
            [120.0, 0.0],
            egui::Label::new(egui::RichText::new(label).strong()),
        );
        ui.add(egui::Label::new(value).wrap());
    });
}

#[cfg(test)]
mod tests;
