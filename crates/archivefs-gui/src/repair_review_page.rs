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

use archivefs_core::dat::rename_apply::model::EntryState;
use archivefs_core::repair::execute::{
    RepairExecutionOptions, RepairReverifyOutcome, RepairTransactionResult,
};
use archivefs_core::repair::library::{
    ApplySavedPlanSelectedError, LibraryRepairPlan, ReportCounts, apply_saved_plan_selected,
};
use archivefs_core::repair::proposal::{RepairProposal, RepairProposalId};
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
    /// Blocked buckets, which have no canonical name).
    pub(crate) destination: Option<String>,
    pub(crate) reason: String,
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
        }
    }
}

/// The terminal message the background apply worker sends back.
enum RepairApplyMessage {
    Applied(Box<RepairTransactionResult>),
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
    /// The last apply's result, when the backend actually ran and returned.
    /// Never populated for a refused/pre-mutation error - see `apply_failure`.
    pub(crate) apply_result: Option<RepairTransactionResult>,
    /// The last apply's refusal or error, when the backend refused or a
    /// worker error occurred. Cleared only by a new apply attempt, never
    /// automatically, so the reason stays visible until the user acts again.
    pub(crate) apply_failure: Option<RepairApplyFailure>,
    /// Set once an apply actually left entries applied on disk. The loaded
    /// plan was proven against the library *before* that mutation, so it no
    /// longer reflects the library's current state and must not be trusted
    /// as evidence for a second apply without reloading/rescanning.
    pub(crate) plan_stale: bool,
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
            Ok((plan, availability)) => {
                self.plan = Some(plan);
                self.plan_path = Some(path);
                self.selected.clear();
                self.details_id = None;
                self.error = None;
                self.counts_availability = availability;
                self.plan_version = self.plan_version.wrapping_add(1);
                // A freshly loaded plan supersedes any previous apply result,
                // failure, staleness warning, or pending confirmation - all of
                // those describe the *previous* plan, not this one. A running
                // job is left alone: it was started against the plan it holds
                // its own clone of, and finishes independently of what the
                // page loads next.
                self.apply_confirm = None;
                self.apply_result = None;
                self.apply_failure = None;
                self.plan_stale = false;
            }
            Err(message) => self.error = Some(message),
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
    /// loaded plan, in deterministic (`BTreeSet`) order. This is a defensive
    /// re-check, not the safety boundary: [`apply_saved_plan_selected`]
    /// re-validates everything again against a fresh scan regardless. It
    /// exists so the enable rule and the confirmation dialog can never offer
    /// to "apply" an id the loaded plan itself no longer backs.
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
        self.apply_confirm = Some(RepairApplyConfirmation {
            selected: self.actionable_selected_ids(),
            scan_root: plan.scan_root.clone(),
            dat_path: plan.dat_path.clone(),
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
        let journal_dir =
            archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir()
                .unwrap_or_else(|_| PathBuf::from("rename-transactions"));
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
    /// On a successful apply, only the ids whose entry actually reached
    /// [`EntryState::Applied`] are cleared from the selection - an id whose
    /// entry was skipped, failed, or was rolled back stays selected, since it
    /// was not, in the end, applied. On any failure the selection is left
    /// entirely untouched: the caller decides what to do next.
    fn handle_apply_message(&mut self, message: RepairApplyMessage) {
        match message {
            RepairApplyMessage::Applied(outcome) => {
                let applied_sources: Vec<PathBuf> = outcome
                    .transaction
                    .entries
                    .iter()
                    .filter(|entry| entry.state == EntryState::Applied)
                    .map(|entry| entry.source_path.clone())
                    .collect();
                for source in &applied_sources {
                    if let Some(id) = self.proposal_id_for_source(source) {
                        self.selected.remove(&id);
                    }
                }
                // Entries that stayed applied (not rolled back) mean the
                // library changed under the loaded plan; a rescan/reload is
                // required before this plan can back another apply.
                if outcome.summary.applied > 0 {
                    self.plan_stale = true;
                }
                self.apply_result = Some(*outcome);
                self.apply_failure = None;
                self.apply_running = false;
            }
            RepairApplyMessage::Failed(failure) => {
                self.apply_failure = Some(failure);
                self.apply_result = None;
                self.apply_running = false;
            }
        }
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

    // The confirmation dialog floats above everything else on the page and
    // is drawn unconditionally so it stays visible (and actionable) no
    // matter what else changed underneath it this frame.
    show_apply_confirmation_dialog(ui, state);

    // Load control.
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            if widgets::action_button(ui, "Load repair plan", widgets::ActionStyle::Primary, true)
                .clicked()
            {
                open_plan_dialog(state);
            }
            if let Some(path) = &state.plan_path {
                ui.label(egui::RichText::new(path.display().to_string()).color(theme::muted(ui)));
            }
        });
        ui.label(
            egui::RichText::new(
                "Loads a plan saved by the CLI's 'repair scan --plan-out' contract. Preview only.",
            )
            .color(theme::muted(ui)),
        );
    });

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
            let apply_label = if state.apply_running {
                "Applying…".to_string()
            } else {
                format!("Apply Selected ({})", state.selected.len())
            };
            let apply =
                widgets::action_button(ui, apply_label, widgets::ActionStyle::Primary, can_apply);
            let apply_clicked = apply.clicked();
            if !can_apply {
                let hover = if state.apply_running {
                    "An apply is already running."
                } else if state.actionable_selected_ids().is_empty() {
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

/// Post-apply feedback for the last completed run: transaction id, counts,
/// rollback status, and reverify results. Shown until superseded by the next
/// apply or a newly loaded plan.
fn show_apply_result(ui: &mut egui::Ui, state: &RepairReviewPageState) {
    let Some(result) = state.apply_result.as_ref() else {
        return;
    };
    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        ui.label(egui::RichText::new("Apply complete").size(16.0).strong());
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
    });
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
fn show_apply_failure(ui: &mut egui::Ui, state: &RepairReviewPageState) {
    let Some(failure) = state.apply_failure.as_ref() else {
        return;
    };
    ui.add_space(8.0);
    widgets::banner(
        ui,
        failure.label,
        &failure.detail,
        widgets::StatusTone::Blocked,
    );
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

    let path_text = match &row.destination {
        Some(destination) => format!("{} → {}", row.source, destination),
        None => row.source.clone(),
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
        });
        ui.add_space(4.0);
        detail_label(ui, "Source", &proposal.source_path.display().to_string());
        if let Some(destination) = proposal.destination() {
            detail_label(ui, "Destination", &destination.display().to_string());
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
