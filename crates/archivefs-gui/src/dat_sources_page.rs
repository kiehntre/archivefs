//! The DAT Sources page: register local DAT catalogues, check them, audit
//! against them.
//!
//! # Why this page exists
//!
//! DAT parsing, indexing, and the read-only audit have shipped for a while,
//! reachable only through `archivefs-cli dat …`, and only ever for a path typed
//! afresh each time. There was nowhere to *keep* a DAT. This page is that
//! place, and everything it does calls the same core the CLI does.
//!
//! # The shape, following Cheat Sources
//!
//! Authoritative state becomes a [`DatSourcesPageView`] through a pure
//! function, and the drawing code only draws. The properties worth testing
//! here (that a disabled source is still listed, that removing one does not
//! delete a file, that an audit reports only verdicts the core produced) are
//! data questions, answerable without a frame buffer.
//!
//! [`DatSourcesPageState`] holds a `saved` registry and a `draft` one. Edits
//! touch the draft; the file is written only on Save. The difference between
//! the two *is* the unsaved-change state, so "is this dirty?" cannot drift from
//! "would saving change anything?".
//!
//! # One background job at a time
//!
//! Validating a 200 MB catalogue and auditing a library are both long enough to
//! freeze a window, so both run on a worker thread with a cancellation flag and
//! a bounded progress channel. One job runs at a time: a second concurrent
//! parse of the same source would race for no benefit, and the design this
//! follows calls for at most one source operation in flight.
//!
//! # Nothing here writes to a ROM
//!
//! The only file this page writes is its own registry, through the core's
//! durable-write path. Validation reads DAT files; an audit reads DAT files and
//! ROMs. Removing a source removes a registry entry. There is no rename, move,
//! delete, archive rewrite, or symlink change anywhere on this page, and none
//! is deferred behind a flag - the capability is simply not present.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use archivefs_core::dat::classification::{
    ContentSelectionPolicy, DatContentClassification, DatContentSummary,
};
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::parser::DiagnosticSeverity;
use archivefs_core::dat::policy::{
    ClonePolicy, DatPolicyConfig, EffectiveDatPolicy, LanguageId, LanguagePreference, PolicyField,
    RegionId, RevisionPolicy, participating_sources, resolve, validate_policy_config,
};
use archivefs_core::dat::rename_apply::{
    ApplyError, ApplyExecution, ApplyOutcome, EntryState, HardConflictMode, RollbackOutcome,
    RollbackResult, TransactionState, apply_transaction, rollback_transaction,
};
use archivefs_core::dat::rename_plan::{
    ProposalState, RenamePlan, RenamePlanContext, ReviewDecision, build_rename_plan,
};
use archivefs_core::dat::sources::audit_run::{
    DatAuditOutcome, DatAuditProgress, DatAuditRequest, run_dat_audit,
};
use archivefs_core::dat::sources::{
    DatFileOutcome, DatHealthState, DatSourceEntry, DatSourceKind, DatSourceRegistry,
    DatValidationReport, UnresolvedDatSetting, load_dat_sources_config_from,
    save_dat_sources_config_to, suggest_display_name, validate_dat_source,
};
use archivefs_core::safe_read::TrustedRoots;
use eframe::egui;

use crate::ui::{components as widgets, theme};

/// Said once on the page, because a DAT audit is the one place a user might
/// reasonably expect a "fix it" button and there is deliberately not one.
pub(crate) const READ_ONLY_PROMISE: &str = "Your files won't be renamed unless you approve it. A DAT audit only reads files and \
     reports what it found; it does not move, delete or rewrite them.";
/// The short, simple promise shown on the page. Longer detail stays available
/// as a tooltip (`READ_ONLY_PROMISE`) but never front and centre.
pub(crate) const SAFE_PROMISE: &str = "Your files won't be renamed unless you approve it.";
pub(crate) const GAMES_ONLY_EXPLANATION: &str = "Games only hides entries confidently identified as things like magazines, music, demos, manuals, or other non-game material. Unknown entries are kept for review.";
/// The prominent, repeated statement the rename-planning section must show.
pub(crate) const PLAN_ONLY_PROMISE: &str = "Planning only — EmuWiz will not rename any files. This section derives suggested names \
     from verified DAT matches and explains them; nothing here changes, moves, deletes or rewrites \
     a file.";

/// Batches larger than this require a typed confirmation phrase such as
/// "RENAME 42 FILES" before any rename happens.
pub(crate) const TYPED_CONFIRMATION_THRESHOLD: usize = 8;

/// The exact phrase a user must type to confirm a large batch.
pub(crate) fn typed_confirmation_phrase(count: usize) -> String {
    format!("RENAME {count} FILES")
}

/// What Stage 1 supports, stated rather than implied by what happens to work.
pub(crate) const SUPPORTED_FORMATS: &str = "Logiqx XML (No-Intro, Redump) and ClrMamePro text (TOSEC, generic). Other formats are not \
     supported and are not silently accepted.";

/// How many progress messages may be queued before older ones are dropped.
///
/// A run over 25,000 files produces a message per file; if the window is busy,
/// an unbounded queue would grow until the run finished. Dropping progress is
/// free - the next message supersedes it - so the send is non-blocking and a
/// full channel simply means the display is a little behind.
const PROGRESS_QUEUE_DEPTH: usize = 64;

/// Files that must be processed before an ETA can be trusted at all.
const ETA_MIN_FILES: u64 = 100;

/// Seconds that must have elapsed before an ETA can be trusted at all.
const ETA_MIN_SECONDS: f64 = 5.0;

/// Blend for the exponential moving average of throughput: a single frame's
/// speed moves the estimate by this fraction of the way, so one fast sample
/// cannot make the ETA jump.
const ETA_SMOOTHING_ALPHA: f64 = 0.2;

// ---------------------------------------------------------------------------
// View model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatSaveState {
    Idle,
    Saved,
    Failed(String),
}

/// One source's row, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DatSourceRowView {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) path: String,
    pub(crate) kind_label: &'static str,
    pub(crate) enabled: bool,
    /// The assigned platform's display name, or `None` when unassigned.
    pub(crate) platform_display: Option<String>,
    /// The raw assigned ID, needed to keep the picker's exclusion list right.
    pub(crate) platform_id: Option<String>,
    /// The assignment names a platform this build does not know.
    pub(crate) platform_unresolved: bool,
    /// Formats the last validation actually saw. Empty until validated - this
    /// is never guessed from the filename.
    pub(crate) formats: Vec<String>,
    pub(crate) health_state: DatHealthState,
    pub(crate) health_detail: Option<String>,
    pub(crate) last_validated: Option<String>,
    /// The DAT changed since the stored verdict was taken, so the verdict
    /// describes a file that is no longer there.
    pub(crate) health_stale: bool,
    pub(crate) entry_count: Option<u64>,
    pub(crate) rom_count: Option<u64>,
    /// This row differs from what is on disk.
    pub(crate) changed: bool,
    /// A background job is running for this source.
    pub(crate) busy: bool,
    /// The last validation run's per-file breakdown, if one has been run.
    pub(crate) detail: Option<InspectView>,
    /// The last validation's diagnostics, grouped by (severity, code, message)
    /// and sorted by severity then code then message. Deterministic, bounded,
    /// and never reparsed to build.
    pub(crate) groups: Vec<DiagnosticGroupView>,
    /// The bounded safety limit stopped the last validation part-way through a
    /// folder, so the verdict covers only part of it. Must never be presented
    /// as "everything was checked".
    pub(crate) incomplete_load: bool,
    /// How many DAT files the last (incomplete) validation actually read.
    pub(crate) dat_files_read: Option<u64>,
    /// How many DAT files the folder holds, when genuinely known.
    pub(crate) dat_files_total: Option<u64>,
    /// Whether the full warning details are recorded in History & Logs. The
    /// card only ever points there when this is true; today the details are
    /// kept inline instead, so this stays false.
    pub(crate) history_link_available: bool,
}

impl DatSourceRowView {
    /// The line describing an incomplete catalogue load, or `None` when the
    /// load was complete.
    ///
    /// "512 of 2,024 DAT files read" is shown only when both numbers are
    /// genuinely known; otherwise the safety limit is named without inventing
    /// a total.
    pub(crate) fn incomplete_load_line(&self) -> Option<String> {
        if !self.incomplete_load {
            return None;
        }
        match (self.dat_files_read, self.dat_files_total) {
            (Some(read), Some(total)) => Some(format!("{read} of {total} DAT files read")),
            _ => Some("Processing stopped at the configured safety limit".to_string()),
        }
    }

    /// The groups of one severity, in deterministic order.
    pub(crate) fn groups_of(&self, severity: DiagnosticSeverity) -> Vec<&DiagnosticGroupView> {
        self.groups
            .iter()
            .filter(|group| group.severity == severity)
            .collect()
    }

    /// How many distinct diagnostic types (groups) have this severity.
    pub(crate) fn diagnostic_types(&self, severity: DiagnosticSeverity) -> usize {
        self.groups_of(severity).len()
    }

    /// How many total occurrences of this severity were found.
    pub(crate) fn diagnostic_occurrences(&self, severity: DiagnosticSeverity) -> usize {
        self.groups_of(severity)
            .iter()
            .map(|group| group.occurrence_count)
            .sum()
    }
}

/// The policy fields of one scope, borrowed for editing.
///
/// This is what lets a policy edit act on "global or this platform's
/// override" through one code path; the field shapes of
/// [`DatPolicyConfig`] and its per-platform override differ, but the four
/// editable fields are the same.
struct PolicyTargets<'a> {
    region: &'a mut Option<Vec<String>>,
    language: &'a mut Option<Vec<String>>,
    revision: &'a mut Option<String>,
    clone: &'a mut Option<String>,
    content: &'a mut Option<String>,
}

impl<'a> PolicyTargets<'a> {
    fn new(
        region: &'a mut Option<Vec<String>>,
        language: &'a mut Option<Vec<String>>,
        revision: &'a mut Option<String>,
        clone: &'a mut Option<String>,
        content: &'a mut Option<String>,
    ) -> Self {
        Self {
            region,
            language,
            revision,
            clone,
            content,
        }
    }

    /// The region list, creating it when absent.
    fn region_list(&mut self) -> &mut Vec<String> {
        self.region.get_or_insert_with(Vec::new)
    }

    /// The language list, creating it when absent.
    fn language_list(&mut self) -> &mut Vec<String> {
        self.language.get_or_insert_with(Vec::new)
    }

    /// Turns a list that has become empty back into "absent".
    ///
    /// Removing the last preference should mean "no preference, inherit the
    /// parent scope" - the `None` state - not "this scope explicitly prefers
    /// nothing", which is a distinction only a hand-edited file needs.
    fn normalise_empty_lists(&mut self) {
        if self.region.as_ref().is_some_and(Vec::is_empty) {
            *self.region = None;
        }
        if self.language.as_ref().is_some_and(Vec::is_empty) {
            *self.language = None;
        }
    }
}

/// Moves `list[index]` to `index + delta` (`-1` up, `+1` down), refusing to
/// move out of range. This is a *move*, not a swap: an entry moved up two
/// places passes over both neighbours rather than exchanging places with the
/// first one.
fn move_index(list: &mut Vec<String>, index: usize, delta: i32) {
    let target = match index.checked_add_signed(delta as isize) {
        Some(target) if target < list.len() => target,
        _ => return,
    };
    if target == index {
        return;
    }
    let value = list.remove(index);
    list.insert(target, value);
}

/// One DAT file inside a source, as the Inspect panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectFileView {
    pub(crate) file_name: String,
    pub(crate) status: &'static str,
    /// The format and counts, or the parser's error.
    pub(crate) detail: String,
    pub(crate) errors: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) notes: Vec<String>,
}

/// How many occurrence rows a diagnostic group will show before truncating.
/// The drill-down is bounded so a folder of hundreds of affected files cannot
/// produce an unbounded list.
pub(crate) const MAX_DIAGNOSTIC_OCCURRENCES_SHOWN: usize = 50;

/// One occurrence of a diagnostic, with the safe DAT filename and the location
/// the parser recorded (when it records one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticOccurrenceView {
    /// The DAT file's name only - never an absolute path.
    pub(crate) file_name: String,
    pub(crate) line: Option<usize>,
    pub(crate) column: Option<usize>,
}

/// Repeated diagnostics from a validation run, grouped into one row per
/// distinct (severity, code, message) so 512 identical DOCTYPE notes render as
/// one group with an occurrence count instead of 512 lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticGroupView {
    pub(crate) severity: DiagnosticSeverity,
    /// The stable parser code, e.g. "doctype_ignored".
    pub(crate) code: &'static str,
    /// The diagnostic message, verbatim, deduplicated by the group key.
    pub(crate) message: String,
    /// Total occurrences across every DAT file in the source.
    pub(crate) occurrence_count: usize,
    /// Distinct DAT files that produced at least one occurrence.
    pub(crate) affected_file_count: usize,
    /// A bounded, deterministic list of occurrences (safe filename + location).
    pub(crate) occurrences: Vec<DiagnosticOccurrenceView>,
    /// More occurrences exist than are listed in `occurrences`.
    pub(crate) occurrences_truncated: bool,
    /// A stable id for the disclosure toggle.
    pub(crate) id: String,
}

/// What the last validation run found, in detail.
///
/// Present only after a source has actually been validated this session: the
/// persisted health carries a summary, but the per-file breakdown is not
/// written to the registry, because it can be several hundred lines and is
/// reproducible in a second by checking again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectView {
    pub(crate) files: Vec<InspectFileView>,
    /// Catalogue identities claimed by more than one file in a folder source.
    pub(crate) duplicate_identities: Vec<String>,
    /// Files in a folder source that were looked at and not taken, with why.
    pub(crate) skipped: Vec<String>,
    pub(crate) truncated: bool,
}

/// A setting kept but not understood, shown read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedDatRowView {
    pub(crate) explanation: String,
}

/// What a running job is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningJobView {
    pub(crate) source_id: String,
    pub(crate) what: &'static str,
    pub(crate) detail: String,
    pub(crate) cancellable: bool,
    /// True from the moment Cancel is pressed until the worker confirms it is
    /// gone. The card reads "Stopping…" while this is set, and the job stays
    /// busy until then - a stale progress line cannot restore an active look.
    pub(crate) cancellation_requested: bool,
    /// Structured audit progress, when the running job is an audit.
    pub(crate) progress: Option<AuditProgressView>,
    /// The source's assigned platform, shown only when it is authoritative
    /// (assigned and recognised by this build). Never guessed from the path.
    pub(crate) platform_display: Option<String>,
}

impl RunningJobView {
    /// The heading: "Auditing 'collection'" normally, "Stopping 'collection'…"
    /// the moment Cancel has been pressed.
    pub(crate) fn heading(&self) -> String {
        let verb = if self.cancellation_requested {
            "Stopping"
        } else {
            self.what
        };
        format!("{verb} '{}'", self.source_id)
    }
}

/// The ETA, in the only three states a running card can honestly show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EtaView {
    /// No estimate is possible yet - no samples, no total, or nothing left to
    /// estimate. Draw nothing.
    None,
    /// The run is progressing but has not gone far or long enough to trust a
    /// number. Draw "Estimating time remaining…".
    Estimating,
    /// A concrete estimate, in whole seconds remaining.
    About { seconds_remaining: u64 },
}

/// Structured progress for a running audit, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditProgressView {
    pub(crate) phase: &'static str,
    pub(crate) files_checked: u64,
    pub(crate) total_files: Option<u64>,
    /// The current folder or file, shortened for display. The full private
    /// path is never turned into a display string.
    pub(crate) current_path: Option<String>,
    pub(crate) elapsed_seconds: u64,
    pub(crate) percent: Option<u8>,
    pub(crate) eta: EtaView,
}

impl AuditProgressView {
    /// The position: "42 of 100" when the total is known, "42 files so far"
    /// when it is not. Never invents a count the run has not produced.
    pub(crate) fn position(&self) -> String {
        match self.total_files {
            Some(total) => format!("{} of {total}", self.files_checked),
            None => format!("{} files so far", self.files_checked),
        }
    }

    /// One line describing where the run is and how long it has taken.
    pub(crate) fn line(&self) -> String {
        let percentage = self
            .percent
            .map(|percent| format!(" ({percent}%)"))
            .unwrap_or_default();
        format!(
            "{} · {}{percentage} · {} elapsed",
            self.phase,
            self.position(),
            format_elapsed(self.elapsed_seconds)
        )
    }

    /// The ETA line, or `None` when nothing should be drawn.
    pub(crate) fn eta_line(&self) -> Option<String> {
        match &self.eta {
            EtaView::None => None,
            EtaView::Estimating => Some("Estimating time remaining…".to_string()),
            EtaView::About { seconds_remaining } => Some(format_eta_remaining(*seconds_remaining)),
        }
    }
}

/// Everything the page draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DatSourcesPageView {
    pub(crate) rows: Vec<DatSourceRowView>,
    pub(crate) unresolved: Vec<UnresolvedDatRowView>,
    /// Problems found while reading the file that this build could not act on
    /// (an unusable ID, a second entry claiming one ID).
    pub(crate) load_problems: Vec<String>,
    pub(crate) dirty: bool,
    pub(crate) config_path: PathBuf,
    pub(crate) save_state: DatSaveState,
    pub(crate) load_error: Option<String>,
    /// The last add/remove attempt that was refused, with its reason.
    pub(crate) action_error: Option<String>,
    pub(crate) pending_consequences: Vec<String>,
    pub(crate) running: Option<RunningJobView>,
    /// The folders offered as audit targets: the configured library source
    /// folders, in configuration order.
    pub(crate) library_folders: Vec<PathBuf>,
    pub(crate) audit: Option<Box<AuditResultView>>,
    pub(crate) audit_error: Option<String>,
    pub(crate) identity_enrichment: Option<Box<archivefs_core::PlatformIdentityEnrichmentSummary>>,
    /// The DAT Matching Policy section, including the Effective Policy
    /// Summary resolved for the scope the user is inspecting.
    pub(crate) policy: DatPolicyView,
    /// The read-only rename-planning section, present when the latest audit
    /// produced a plan.
    pub(crate) rename_plan: Option<RenamePlanView>,
    /// The gated apply flow: review, confirmation, results and crash recovery.
    pub(crate) rename_apply: RenameApplyView,
}

impl DatSourcesPageView {
    /// Whether the page has nothing registered yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Rename planning view model
// ---------------------------------------------------------------------------

/// How the plan rows are filtered for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RenamePlanFilter {
    #[default]
    All,
    Suggested,
    AlreadyCanonical,
    Ambiguous,
    Conflicts,
    Unsupported,
    Blocked,
}

impl RenamePlanFilter {
    pub(crate) const ALL: [RenamePlanFilter; 7] = [
        RenamePlanFilter::All,
        RenamePlanFilter::Suggested,
        RenamePlanFilter::AlreadyCanonical,
        RenamePlanFilter::Ambiguous,
        RenamePlanFilter::Conflicts,
        RenamePlanFilter::Unsupported,
        RenamePlanFilter::Blocked,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Suggested => "Suggested",
            Self::AlreadyCanonical => "Already canonical",
            Self::Ambiguous => "Ambiguous",
            Self::Conflicts => "Conflicts",
            Self::Unsupported => "Unsupported",
            Self::Blocked => "Blocked",
        }
    }

    fn matches(self, row: &RenamePlanRowView) -> bool {
        match self {
            Self::All => true,
            Self::Suggested => row.state == ProposalState::Suggested,
            Self::AlreadyCanonical => row.state == ProposalState::AlreadyCanonical,
            Self::Ambiguous => row.state == ProposalState::Ambiguous,
            Self::Conflicts => row.state == ProposalState::Conflict,
            Self::Unsupported => row.state == ProposalState::Unsupported,
            Self::Blocked => matches!(
                row.state,
                ProposalState::Blocked
                    | ProposalState::ExcludedByContentPolicy
                    | ProposalState::UnclassifiedContent
            ),
        }
    }
}

/// The rename-planning section: the read-only plan derived from the latest
/// audit, with its counts and every row (the active filter lives in
/// [`DatSourcesPageUi`] and selects which rows are drawn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenamePlanView {
    pub(crate) generation: u64,
    pub(crate) scan_root_short: String,
    pub(crate) platform_display: Option<String>,
    pub(crate) source_display_name: String,
    pub(crate) counts: archivefs_core::dat::rename_plan::RenamePlanCounts,
    pub(crate) audited_total: usize,
    pub(crate) verified_total: usize,
    pub(crate) truncated: bool,
    pub(crate) rows: Vec<RenamePlanRowView>,
    pub(crate) error: Option<String>,
}

/// One plan row, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenamePlanRowView {
    /// The source file's path. The row renders the basename and a shortened
    /// parent; the full path is kept only for identity and future apply work.
    pub(crate) source_path: PathBuf,
    pub(crate) current_basename: String,
    pub(crate) proposed_basename: Option<String>,
    pub(crate) platform_display: Option<String>,
    pub(crate) source_display_name: String,
    pub(crate) game_name: Option<String>,
    pub(crate) rom_name: Option<String>,
    pub(crate) verdict_label: String,
    pub(crate) content: ContentTechnicalView,
    pub(crate) state: ProposalState,
    pub(crate) object_kind_label: &'static str,
    pub(crate) explanations: Vec<String>,
    pub(crate) ambiguity_reason: Option<String>,
    pub(crate) collision_detail: Option<String>,
    pub(crate) blockers: Vec<String>,
    pub(crate) extension_preserved: bool,
    pub(crate) sanitisation_notes: Vec<String>,
    pub(crate) decision: Option<ReviewDecision>,
}

// ---------------------------------------------------------------------------
// Rename apply view model
// ---------------------------------------------------------------------------

/// The apply/recovery section of the rename-planning flow.
///
/// Everything the user sees here is derived by the core executor; the GUI only
/// renders the built transaction, records confirmations, and runs the core
/// apply/rollback functions on a worker thread. It never calls `fs::rename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameApplyView {
    /// The read-only review of the transaction the user is about to confirm.
    pub(crate) review: Option<ApplyReviewView>,
    /// The last apply outcome, with per-file results.
    pub(crate) outcome: Option<ApplyOutcomeView>,
    pub(crate) apply_error: Option<String>,
    /// Whether the safe-subset option is offered after an AbortAll conflict.
    pub(crate) subset_available: bool,
    pub(crate) rollback_result: Option<RollbackResultView>,
    pub(crate) rollback_error: Option<String>,
    pub(crate) apply_running: bool,
    pub(crate) rollback_running: bool,
    /// Interrupted transactions found on disk, offered for review.
    pub(crate) recovery: Vec<RecoveryTransactionView>,
    /// The journal directory, shown for transparency.
    pub(crate) journal_dir: String,
}

/// The review a user must confirm before any rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyReviewView {
    /// The transaction built at review time (identity snapshots included).
    pub(crate) transaction: archivefs_core::dat::rename_apply::RenameTransaction,
    /// The exact old → new pairs, in plan order.
    pub(crate) rows: Vec<ApplyReviewRow>,
    /// The trusted root the renames must stay inside, when configured.
    pub(crate) trusted_root: Option<String>,
    /// The exact phrase that must be typed to confirm, when required
    /// (`None` for small batches where a single confirm click suffices).
    pub(crate) required_phrase: Option<String>,
}

/// One old → new pair in the review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyReviewRow {
    pub(crate) current_basename: String,
    pub(crate) proposed_basename: String,
}

/// The per-file result of an apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyOutcomeView {
    pub(crate) transaction_id: String,
    pub(crate) state: TransactionState,
    pub(crate) requested: usize,
    pub(crate) applied: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
    pub(crate) rows: Vec<ApplyRowView>,
}

/// One per-file apply result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyRowView {
    pub(crate) current_basename: String,
    pub(crate) proposed_basename: String,
    pub(crate) state: EntryState,
    pub(crate) failure_reason: Option<String>,
}

/// The outcome of a rollback pass, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RollbackResultView {
    pub(crate) label: &'static str,
    pub(crate) detail: String,
}

/// One persisted transaction offered for rollback or crash recovery: either a
/// settled `Applied` batch (roll back the whole transaction) or an interrupted
/// batch (roll back completed steps or leave untouched).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryTransactionView {
    pub(crate) transaction_id: String,
    pub(crate) state: TransactionState,
    pub(crate) applied_count: usize,
    pub(crate) total_count: usize,
}

// ---------------------------------------------------------------------------
// DAT matching policy view model
// ---------------------------------------------------------------------------

/// The DAT Matching Policy section: what the user has set, at the scope they
/// are editing, plus the resolved Effective Policy Summary for that scope.
///
/// The GUI never implements policy logic: every resolved value in here is the
/// output of the core resolver, and every edit action maps onto the persisted
/// [`DatPolicyConfig`] through the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DatPolicyView {
    /// The scope being edited: `None` = global, otherwise a canonical
    /// platform id.
    pub(crate) scope: Option<String>,
    /// The scope's display label ("All platforms" or the platform's name).
    pub(crate) scope_label: String,
    /// The platforms a user can choose as a scope, derived from the platforms
    /// the registered sources cover.
    pub(crate) scopes_available: Vec<PolicyScopeOption>,
    pub(crate) region_preferences: Vec<PolicyPreferenceRowView>,
    pub(crate) language_preferences: Vec<PolicyPreferenceRowView>,
    pub(crate) revision_policy: RevisionPolicy,
    pub(crate) clone_policy: ClonePolicy,
    pub(crate) content_selection: ContentSelectionPolicy,
    pub(crate) effective: EffectivePolicySummaryView,
    /// Validation problems in the persisted policy (unknown values, bad
    /// platform keys). Never blocks a save; the values are preserved.
    pub(crate) problems: Vec<String>,
    /// False when the registry could not be read, so nothing can be edited.
    pub(crate) editable: bool,
}

/// One entry in an ordered preference list, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyPreferenceRowView {
    /// The stable value (`europe`, `en`, `multi`, `original`, …).
    pub(crate) value: String,
    /// What a person sees.
    pub(crate) label: String,
    /// 1-based position in the list.
    pub(crate) position: usize,
}

/// One selectable scope for the policy section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyScopeOption {
    /// `None` = "All platforms" (the global scope).
    pub(crate) id: Option<String>,
    pub(crate) label: String,
}

/// The Effective Policy Summary: the resolved policy for the current scope,
/// and where each value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectivePolicySummaryView {
    /// The resolved platform ("All platforms" or the platform's display name).
    pub(crate) platform: String,
    /// The sources consulted for this scope, in order.
    pub(crate) source_ordering: Vec<SourceOrderRow>,
    pub(crate) region: String,
    pub(crate) language: String,
    pub(crate) revision: String,
    pub(crate) clone: String,
    pub(crate) content: String,
    /// "field — where it came from" rows.
    pub(crate) source_of: Vec<(String, String)>,
}

/// One source in the summary's consultation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceOrderRow {
    pub(crate) display_name: String,
    pub(crate) priority: u32,
    /// 1-based: "consulted 1st, 2nd, …".
    pub(crate) consulted_position: usize,
}

/// One verdict category, as a countable row.
///
/// The categories are exactly the ones
/// [`archivefs_core::dat::audit::AuditSummary`] carries. None is invented and
/// none is merged: "Probable (multiple)" is not folded into "Exact (multiple)",
/// because a CRC32 agreeing is not the same evidence as a SHA-1 agreeing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditCategoryView {
    pub(crate) label: &'static str,
    pub(crate) count: usize,
    pub(crate) meaning: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditResultView {
    pub(crate) source_display_name: String,
    pub(crate) source_id: String,
    pub(crate) dat_path: String,
    pub(crate) scan_root: String,
    /// The scan folder shortened for display, so a long private path does not
    /// take over the summary.
    pub(crate) scan_root_short: String,
    pub(crate) catalogue_names: Vec<String>,
    pub(crate) catalogue_entries: usize,
    pub(crate) headline: String,
    /// How long the run took, when it completed.
    pub(crate) elapsed_seconds: Option<u64>,
    pub(crate) categories: Vec<AuditCategoryView>,
    /// Per-file lines, capped for display.
    pub(crate) entries: Vec<AuditEntryView>,
    pub(crate) entries_truncated: usize,
    pub(crate) unhashed: Vec<(String, String)>,
    pub(crate) unreadable_catalogues: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) files_scanned: usize,
    pub(crate) content_selection: ContentSelectionPolicy,
    pub(crate) content_summary: DatContentSummary,
    /// The Effective Policy Summary annotation, when the audit carried a
    /// policy. Never changes a verdict; it shows the preferred candidate
    /// order for the files whose hash matched several catalogue entries.
    pub(crate) policy: Option<AuditPolicyView>,
}

/// The policy annotation of one audit result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditPolicyView {
    /// The sources consulted for this platform, in order.
    pub(crate) source_ordering: Vec<String>,
    /// One row per multi-candidate file that was ranked.
    pub(crate) notes: Vec<AuditPolicyNoteView>,
    /// `None` when no file needed a ranking.
    pub(crate) notes_truncated: Option<usize>,
}

/// One ranked multi-candidate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditPolicyNoteView {
    pub(crate) file_name: String,
    pub(crate) verdict_label: String,
    /// The ranked candidates, in the deterministic display order.
    pub(crate) ranked: Vec<String>,
    /// The explanations, most decisive first.
    pub(crate) explanations: Vec<String>,
    /// Whether the policy picked a single winner, and its label.
    pub(crate) decided: bool,
    pub(crate) winner: Option<String>,
    /// Whether the policy could not decide, and why.
    pub(crate) ambiguous: bool,
    pub(crate) ambiguity_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditEntryView {
    pub(crate) file_name: String,
    pub(crate) verdict: &'static str,
    pub(crate) detail: String,
    pub(crate) content: Vec<ContentTechnicalView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentTechnicalView {
    pub(crate) classification: String,
    pub(crate) confidence: String,
    pub(crate) evidence: Vec<String>,
    pub(crate) original_metadata: Vec<(String, String)>,
    pub(crate) classifier_version: String,
}

/// How many audited files are listed individually.
///
/// The summary counts every file; the list is what a person reads, and 500
/// lines is already past the point of reading. The view says how many were
/// left out rather than implying the list is complete.
pub(crate) const MAX_AUDIT_ENTRIES_SHOWN: usize = 500;

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// One thing the page can ask for. Only `Save` writes the registry; only
/// `Validate` and `Audit` read anything else.
///
/// The policy actions all carry the scope they apply to (`None` = global,
/// otherwise a canonical platform id), so an edit made while inspecting one
/// platform's effective policy lands on that platform's override, never on
/// the global preferences by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatSourcesPageAction {
    AddFile {
        path: PathBuf,
    },
    AddFolder {
        path: PathBuf,
    },
    SetEnabled {
        id: String,
        enabled: bool,
    },
    SetPlatform {
        id: String,
        platform: Option<String>,
    },
    Remove {
        id: String,
    },
    Validate {
        id: String,
    },
    Audit {
        id: String,
        scan_root: PathBuf,
    },
    CancelJob,
    Save,
    Revert,
    SelectPolicyScope {
        scope: Option<String>,
    },
    MoveRegion {
        scope: Option<String>,
        index: usize,
        delta: i32,
    },
    AddRegion {
        scope: Option<String>,
        region: RegionId,
    },
    RemoveRegion {
        scope: Option<String>,
        index: usize,
    },
    MoveLanguage {
        scope: Option<String>,
        index: usize,
        delta: i32,
    },
    AddLanguage {
        scope: Option<String>,
        preference: LanguagePreference,
    },
    RemoveLanguage {
        scope: Option<String>,
        index: usize,
    },
    ClearRegion {
        scope: Option<String>,
    },
    ClearLanguage {
        scope: Option<String>,
    },
    SetRevisionPolicy {
        scope: Option<String>,
        policy: RevisionPolicy,
    },
    SetClonePolicy {
        scope: Option<String>,
        policy: ClonePolicy,
    },
    SetContentSelection {
        scope: Option<String>,
        policy: ContentSelectionPolicy,
    },
    /// Records or clears a review decision on one proposal. Decisions are
    /// session-only and never touch a file.
    SetReviewDecision {
        path: String,
        decision: Option<ReviewDecision>,
    },
    /// Clears every review decision for the current plan.
    ClearReviewDecisions,
    /// Build the transaction for the approved, applicable proposals and show
    /// the read-only review. No mutation.
    BeginApplyReview,
    /// Confirm the review and run the apply in AbortAll mode. `typed` is the
    /// user's typed confirmation phrase, validated before any mutation.
    ConfirmApply {
        typed: String,
    },
    /// Re-run the apply applying only the independently safe subset, after an
    /// AbortAll conflict was shown and the user explicitly chose it.
    ConfirmApplySafeSubset {
        typed: String,
    },
    CancelApplyReview,
    /// Roll back the last applied (or recovered) transaction.
    RollbackTransaction {
        id: String,
    },
    /// A crash-recovery choice for an interrupted transaction.
    RecoveryChoice {
        id: String,
        choice: RecoveryChoice,
    },
    /// Forget the apply outcome display.
    ClearApplyOutcome,
}

/// A crash-recovery choice, mirroring the journal recovery options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryChoice {
    RollBack,
    LeaveUntouched,
}

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

enum JobMessage {
    Progress(String),
    /// Structured audit progress, kept structured so the page can compute
    /// percentages and an ETA instead of only echoing text.
    AuditProgress(DatAuditProgress),
    Validated(Box<DatValidationReport>),
    Audited {
        /// The audit generation this result belongs to. A result from a stale
        /// generation is discarded so an older plan can never replace a newer
        /// one.
        generation: u64,
        outcome: Box<DatAuditOutcome>,
        enrichment: Option<Box<archivefs_core::PlatformIdentityEnrichmentSummary>>,
        /// The read-only rename plan derived from the audit, when one could be
        /// built (and was not cancelled).
        plan: Option<Box<RenamePlan>>,
    },
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Validate,
    Audit,
}

/// A rename apply or rollback worker result.
enum ApplyJobMessage {
    Applied(Box<ApplyOutcome>),
    RolledBack(Box<RollbackOutcome>),
    Failed(String),
    /// An AbortAll apply found hard conflicts; nothing was mutated and the
    /// safe-subset option is now offered.
    HardConflicts(String),
    Cancelled,
}

/// The dedicated worker for apply and rollback mutations. The GUI never calls
/// `std::fs::rename`; it sends the core executor's request here and renders the
/// result.
struct ApplyJob {
    cancel: Arc<AtomicBool>,
    messages: Receiver<ApplyJobMessage>,
}

/// A History & Logs record produced by an apply or rollback outcome. It carries
/// counts and the transaction id but never private paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameHistoryRecord {
    pub(crate) action: RenameHistoryAction,
    pub(crate) transaction_id: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenameHistoryAction {
    Apply,
    Rollback,
}

struct RunningJob {
    kind: JobKind,
    source_id: String,
    cancel: Arc<AtomicBool>,
    /// Set by [`DatSourcesPageAction::CancelJob`]. The visible card switches
    /// to "Stopping…" immediately; the job itself keeps running until the
    /// worker sends a terminal message, and anything that arrives afterwards
    /// is ignored rather than allowed to restore an active-looking state.
    cancel_requested: bool,
    messages: Receiver<JobMessage>,
    latest: String,
    /// When the job started, for elapsed time.
    started_at: Instant,
    /// Structured progress for audit jobs. `None` for validation.
    audit_progress: Option<AuditProgressTracker>,
    /// The source's resolved platform at job start, shown on the running card
    /// only when it is authoritative (assigned and recognised).
    platform_display: Option<String>,
}

/// Sends without blocking, dropping the message when the queue is full.
///
/// The worker must never wait on the UI: a stalled window would otherwise stall
/// the audit, and the only thing lost by dropping is a progress line the next
/// one replaces.
fn send_progress(sender: &SyncSender<JobMessage>, message: JobMessage) {
    match sender.try_send(message) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// The percentage, as a whole number, or `None` when the total is unknown or
/// zero. A total that is not known is never replaced by a guessed one.
fn format_percentage(checked: u64, total: u64) -> Option<u8> {
    if total == 0 {
        return None;
    }
    let percent = ((checked as f64 / total as f64) * 100.0).round() as i64;
    Some(percent.clamp(0, 100) as u8)
}

/// Seconds as a person would read an elapsed time: "42s", "3m 12s", "1h 5m".
fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Remaining seconds as an approximate ETA: "About 12 minutes remaining".
fn format_eta_remaining(seconds: u64) -> String {
    if seconds < 60 {
        format!("About {} seconds remaining", seconds.max(1))
    } else if seconds < 3600 {
        let minutes = ((seconds + 30) / 60).max(1);
        format!(
            "About {minutes} {} remaining",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        let hours = ((seconds + 1800) / 3600).max(1);
        format!(
            "About {hours} {} remaining",
            if hours == 1 { "hour" } else { "hours" }
        )
    }
}

/// A path shortened for display: the last two components, with the private
/// leading part elided.
///
/// The user picked the folder, so showing part of it on the card is fine; the
/// point is that a long absolute path never takes over the running card, and
/// that a full private path is never turned into a display string. Short paths
/// are returned as they are.
fn shorten_path(path: &str) -> String {
    let mut components: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    if components.len() <= 2 {
        return path.to_string();
    }
    let kept: Vec<&str> = components.split_off(components.len() - 2);
    format!("…/{}", kept.join("/"))
}

/// The source's platform, shown on the running card only when it is
/// authoritative: assigned by the user and recognised by this build. An
/// unassigned source has no platform to claim, and an unresolved one is not
/// presented as if it were real.
fn authoritative_platform(entry: &DatSourceEntry) -> Option<String> {
    entry
        .platform
        .as_ref()
        .filter(|_| entry.platform_is_resolved())
        .and_then(|_| entry.platform_display())
}

/// The region preference list as *authored* at `scope`: the global list, or a
/// platform override's own list (empty when that platform has no override).
fn authored_region_list(config: &DatPolicyConfig, scope: &Option<String>) -> Vec<String> {
    match scope {
        None => config.region_preferences.clone().unwrap_or_default(),
        Some(platform) => config
            .platforms
            .as_ref()
            .and_then(|overrides| overrides.get(platform))
            .and_then(|entry| entry.region_preferences.clone())
            .unwrap_or_default(),
    }
}

/// The language preference list as *authored* at `scope`.
fn authored_language_list(config: &DatPolicyConfig, scope: &Option<String>) -> Vec<String> {
    match scope {
        None => config.language_preferences.clone().unwrap_or_default(),
        Some(platform) => config
            .platforms
            .as_ref()
            .and_then(|overrides| overrides.get(platform))
            .and_then(|entry| entry.language_preferences.clone())
            .unwrap_or_default(),
    }
}

/// A preference list rendered as one line for the summary, or "Any" when the
/// list is empty (an empty preference list means no preference - all regions
/// or languages are treated as equal).
fn render_preference_list(values: Vec<String>) -> String {
    if values.is_empty() {
        "Any".to_string()
    } else {
        values.join(", ")
    }
}

/// The Effective Policy Summary for one scope, derived entirely from the core
/// resolver's output - the GUI renders, it never re-resolves.
fn effective_summary_view(
    effective: &EffectiveDatPolicy,
    scope: &Option<String>,
) -> EffectivePolicySummaryView {
    let platform = match scope {
        None => "All platforms".to_string(),
        Some(id) => archivefs_core::platform::display_name_for(id).to_string(),
    };
    let source_ordering = effective
        .source_ordering
        .iter()
        .enumerate()
        .map(|(index, source)| SourceOrderRow {
            display_name: source.display_name.clone(),
            priority: source.priority,
            consulted_position: index + 1,
        })
        .collect();
    let region = render_preference_list(
        effective
            .region_preferences
            .iter()
            .map(|region| region.label().to_string())
            .collect(),
    );
    let language = render_preference_list(
        effective
            .language_preferences
            .iter()
            .map(|preference| preference.label().to_string())
            .collect(),
    );
    let source_of = vec![
        (
            "Region preference".to_string(),
            effective.scope_of[&PolicyField::Region].label().to_string(),
        ),
        (
            "Language preference".to_string(),
            effective.scope_of[&PolicyField::Language]
                .label()
                .to_string(),
        ),
        (
            "Revision policy".to_string(),
            effective.scope_of[&PolicyField::Revision]
                .label()
                .to_string(),
        ),
        (
            "Clone policy".to_string(),
            effective.scope_of[&PolicyField::Clone].label().to_string(),
        ),
        (
            "Content selection".to_string(),
            effective.scope_of[&PolicyField::Content]
                .label()
                .to_string(),
        ),
    ];
    EffectivePolicySummaryView {
        platform,
        source_ordering,
        region,
        language,
        revision: effective.revision_policy.label().to_string(),
        clone: effective.clone_policy.label().to_string(),
        content: effective.content_selection.label().to_string(),
        source_of,
    }
}

/// Groups the last validation's diagnostics by (severity, code, message),
/// accumulating per-group occurrence and affected-file counts.
///
/// The result is sorted by severity (Error, Warning, Note) then code then
/// message, so two runs over the same folder produce identical output. Occurrence
/// rows are kept bounded; the message string is cloned once per distinct type,
/// never once per occurrence. `source_id` scopes each group's stable id so the
/// open-group state can never bleed across sources.
fn group_diagnostics(source_id: &str, report: &DatValidationReport) -> Vec<DiagnosticGroupView> {
    use std::collections::{BTreeMap, BTreeSet};

    fn severity_rank(severity: DiagnosticSeverity) -> u8 {
        match severity {
            DiagnosticSeverity::Error => 0,
            DiagnosticSeverity::Warning => 1,
            DiagnosticSeverity::Note => 2,
        }
    }

    struct GroupAcc {
        severity: DiagnosticSeverity,
        occurrence_count: usize,
        affected_files: BTreeSet<String>,
        occurrences: Vec<DiagnosticOccurrenceView>,
        occurrences_truncated: bool,
    }

    // The key is ordered so the BTreeMap yields severity- then code- then
    // message-sorted groups.
    let mut groups: BTreeMap<(u8, &'static str, String), GroupAcc> = BTreeMap::new();

    for file in &report.files {
        let DatFileOutcome::Parsed { diagnostics, .. } = &file.outcome else {
            continue;
        };
        for diagnostic in diagnostics {
            let key = (
                severity_rank(diagnostic.severity),
                diagnostic.code,
                diagnostic.message.clone(),
            );
            let acc = groups.entry(key).or_insert_with(|| GroupAcc {
                severity: diagnostic.severity,
                occurrence_count: 0,
                affected_files: BTreeSet::new(),
                occurrences: Vec::new(),
                occurrences_truncated: false,
            });
            acc.occurrence_count += 1;
            acc.affected_files.insert(file.file_name.clone());
            if acc.occurrences.len() < MAX_DIAGNOSTIC_OCCURRENCES_SHOWN {
                acc.occurrences.push(DiagnosticOccurrenceView {
                    file_name: file.file_name.clone(),
                    line: diagnostic.line,
                    column: diagnostic.column,
                });
            } else {
                acc.occurrences_truncated = true;
            }
        }
    }

    groups
        .into_iter()
        .map(|((_, code, message), acc)| DiagnosticGroupView {
            severity: acc.severity,
            code,
            message: message.clone(),
            occurrence_count: acc.occurrence_count,
            affected_file_count: acc.affected_files.len(),
            occurrences: acc.occurrences,
            occurrences_truncated: acc.occurrences_truncated,
            // The id is scoped by source and severity so expanding a group on
            // one source can never leave another source's same-typed group
            // open, and a code reused at two severities cannot collide.
            id: format!(
                "{source_id}:{}:{code}:{message}",
                severity_rank(acc.severity)
            ),
        })
        .collect()
}

/// How far a running audit has got, structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuditPhase {
    ReadingCatalogue,
    Scanning,
    Hashing,
    Comparing,
}

impl AuditPhase {
    fn label(self) -> &'static str {
        match self {
            Self::ReadingCatalogue => "Reading catalogue",
            Self::Scanning => "Scanning",
            Self::Hashing => "Checking files",
            Self::Comparing => "Comparing",
        }
    }
}

/// The GUI-side record of a running audit's progress.
///
/// Kept off the view so the drawing function only draws. The ETA's smoothing
/// lives here because it is state across frames; the pure formatting helpers
/// stay free functions so tests can drive them without a clock. The ETA view
/// is computed at update time and cached, so a frozen run (a stall, or a run
/// being cancelled) shows the estimate it had at its last update rather than
/// one that keeps drifting as the wall clock moves.
#[derive(Debug)]
struct AuditProgressTracker {
    phase: AuditPhase,
    files_checked: u64,
    total_files: Option<u64>,
    current_path: Option<String>,
    eta: EtaEstimator,
    eta_view: EtaView,
}

impl AuditProgressTracker {
    fn new() -> Self {
        Self {
            phase: AuditPhase::ReadingCatalogue,
            files_checked: 0,
            total_files: None,
            current_path: None,
            eta: EtaEstimator::new(),
            eta_view: EtaView::None,
        }
    }

    /// Feeds one progress event. `elapsed_seconds` is the time since the run
    /// started, read by the caller so tests can supply a controlled clock.
    fn update(&mut self, event: &DatAuditProgress, elapsed_seconds: f64) {
        match event {
            DatAuditProgress::ReadingCatalogue { .. } => {
                self.phase = AuditPhase::ReadingCatalogue;
                self.files_checked = 0;
                self.total_files = None;
                self.current_path = None;
                self.eta_view = EtaView::None;
            }
            DatAuditProgress::CatalogueReady { .. } => {
                // Between phases; nothing new about files.
            }
            DatAuditProgress::Scanning {
                files_found,
                current_dir,
            } => {
                self.phase = AuditPhase::Scanning;
                self.files_checked = *files_found as u64;
                // The discovery phase does not know the total yet; an ETA is
                // impossible and none is invented.
                self.total_files = None;
                self.current_path = current_dir.clone();
                self.eta = EtaEstimator::new();
                self.eta_view = EtaView::None;
            }
            DatAuditProgress::Hashing {
                index,
                total,
                file_name,
            } => {
                self.phase = AuditPhase::Hashing;
                self.files_checked = *index as u64;
                self.total_files = Some(*total as u64);
                self.current_path = Some(file_name.clone());
                self.eta.update(*index as u64, elapsed_seconds);
                self.eta_view = self.eta.eta(*index as u64, *total as u64, elapsed_seconds);
            }
            DatAuditProgress::Comparing { files } => {
                self.phase = AuditPhase::Comparing;
                self.files_checked = *files as u64;
                self.total_files = Some(*files as u64);
                self.eta_view = EtaView::None;
            }
        }
    }

    /// The view for one frame. `elapsed_seconds` is supplied by the caller so
    /// tests do not depend on a real clock; it only feeds the elapsed label.
    fn view(&self, elapsed_seconds: u64) -> AuditProgressView {
        let percent = match self.total_files {
            Some(total) if total > 0 => format_percentage(self.files_checked, total),
            _ => None,
        };
        AuditProgressView {
            phase: self.phase.label(),
            files_checked: self.files_checked,
            total_files: self.total_files,
            current_path: self.current_path.as_deref().map(shorten_path),
            elapsed_seconds,
            percent,
            eta: self.eta_view.clone(),
        }
    }
}

/// Exponential-moving-average throughput, so the ETA does not jump from one
/// frame's speed.
#[derive(Debug, Clone, PartialEq)]
struct EtaEstimator {
    smoothed_files_per_second: Option<f64>,
    last: Option<(u64, f64)>,
}

impl EtaEstimator {
    fn new() -> Self {
        Self {
            smoothed_files_per_second: None,
            last: None,
        }
    }

    /// Feeds one sample. `elapsed_seconds` is the time since the run started.
    ///
    /// A stall (no new files between two samples) or a non-advancing clock
    /// leaves the smoothed rate untouched: the estimate freezes rather than
    /// decaying, which is what "if progress stalls, stop updating the ETA"
    /// means.
    fn update(&mut self, checked: u64, elapsed_seconds: f64) {
        if let Some((last_checked, last_elapsed)) = self.last {
            let delta_seconds = elapsed_seconds - last_elapsed;
            let delta_files = checked.saturating_sub(last_checked) as f64;
            if delta_seconds > 0.0 && delta_files > 0.0 {
                let rate = delta_files / delta_seconds;
                self.smoothed_files_per_second = Some(match self.smoothed_files_per_second {
                    Some(previous) => {
                        ETA_SMOOTHING_ALPHA * rate + (1.0 - ETA_SMOOTHING_ALPHA) * previous
                    }
                    None => rate,
                });
            }
        }
        self.last = Some((checked, elapsed_seconds));
    }

    /// The ETA for the current position, applying the confidence gates.
    fn eta(&self, checked: u64, total: u64, elapsed_seconds: f64) -> EtaView {
        let Some(rate) = self.smoothed_files_per_second else {
            return EtaView::None;
        };
        if checked < ETA_MIN_FILES || elapsed_seconds < ETA_MIN_SECONDS {
            return EtaView::Estimating;
        }
        if rate <= 0.0 || total <= checked {
            // Nothing left to estimate, or no forward movement.
            return EtaView::None;
        }
        let seconds_remaining = ((total - checked) as f64 / rate).ceil() as u64;
        EtaView::About { seconds_remaining }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub(crate) struct DatSourcesPageState {
    config_path: PathBuf,
    /// Existing EmuWiz catalogue to enrich after a completed audit. Absent
    /// in injected tests and when no catalogue exists.
    database_path: Option<PathBuf>,
    /// What is on disk, as last read or last written.
    saved: DatSourceRegistry,
    /// What the user has edited but not yet saved.
    draft: DatSourceRegistry,
    load_error: Option<String>,
    load_problems: Vec<String>,
    save_state: DatSaveState,
    action_error: Option<String>,
    /// The last validation report for each source, this session.
    validations: BTreeMap<String, DatValidationReport>,
    /// The grouped diagnostics for each validated source, derived once when the
    /// report lands and cached so the per-frame view rebuild does not re-group
    /// thousands of diagnostics. Kept in lockstep with `validations`.
    diagnostic_groups: BTreeMap<String, Vec<DiagnosticGroupView>>,
    audit: Option<Box<DatAuditOutcome>>,
    audit_error: Option<String>,
    /// How long the most recent completed audit took, read from the job's start
    /// instant when its result arrived. `None` until an audit completes.
    audit_elapsed_seconds: Option<u64>,
    job: Option<RunningJob>,
    /// Decides whether a symlinked ROM may be followed while hashing, exactly
    /// as it does everywhere else in the build.
    trusted: TrustedRoots,
    library_folders: Vec<PathBuf>,
    limits: DatLimits,
    /// The DAT matching policy scope being edited and inspected. `None` is the
    /// global scope; a value is a canonical platform id. Persisted nowhere - it
    /// only decides which preferences the policy section reads and writes.
    policy_scope: Option<String>,
    /// The read-only rename plan derived from the latest audit.
    rename_plan: Option<RenamePlan>,
    /// Why the plan could not be produced, when it could not.
    rename_plan_error: Option<String>,
    /// Monotonically increasing audit generation. Each audit start bumps it;
    /// a result carrying an older generation is a stale plan and is dropped.
    audit_generation: u64,
    /// Set when a current audit has had a chance to enrich the catalogue; the
    /// shell consumes it to reload Library metadata once.
    identity_enrichment_completed: bool,
    identity_enrichment: Option<Box<archivefs_core::PlatformIdentityEnrichmentSummary>>,
    /// Session-only review decisions, keyed by source path. Recording one
    /// never touches a file; nothing here persists them (deferral documented).
    review_decisions: BTreeMap<String, ReviewDecision>,
    /// The durable transaction journal directory.
    transaction_dir: PathBuf,
    /// The transaction built at review time, awaiting confirmation.
    apply_review: Option<archivefs_core::dat::rename_apply::RenameTransaction>,
    /// The approved source paths the review was built from.
    apply_approved: std::collections::BTreeSet<String>,
    /// The last apply outcome (worker result).
    apply_outcome: Option<ApplyOutcome>,
    apply_error: Option<String>,
    /// Whether the safe-subset option is offered after an AbortAll conflict.
    subset_available: bool,
    apply_running: bool,
    rollback_running: bool,
    rollback_result: Option<RollbackResult>,
    rollback_error: Option<String>,
    /// The apply/rollback worker.
    apply_job: Option<ApplyJob>,
    /// Transactions found on disk that are still actionable: settled `Applied`
    /// batches (eligible for rollback after a restart) and interrupted batches
    /// (offered for crash recovery). `RolledBack` journals are neither.
    recovery_transactions: Vec<archivefs_core::dat::rename_apply::RenameTransaction>,
    /// History & Logs records produced by apply/rollback outcomes, drained by
    /// the shell. No private paths are included.
    history_records: Vec<RenameHistoryRecord>,
}

impl DatSourcesPageState {
    /// Loads the registry, falling back to an empty one when the file is absent.
    ///
    /// A parse failure is surfaced rather than swallowed, and saving is refused
    /// while it stands: writing an empty registry over a file that failed to
    /// parse would destroy content the user may still want to fix by hand.
    pub(crate) fn load(
        config_path: PathBuf,
        library_folders: Vec<PathBuf>,
        trusted: TrustedRoots,
    ) -> Self {
        let transaction_dir = archivefs_core::dat::rename_apply::default_rename_transaction_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("archivefs-rename-transactions"));
        Self::load_with_transaction_dir(config_path, library_folders, trusted, transaction_dir)
    }

    /// [`Self::load`] with the rename-transaction journal directory injected,
    /// so tests never read or write the real home directory.
    pub(crate) fn load_with_transaction_dir(
        config_path: PathBuf,
        library_folders: Vec<PathBuf>,
        trusted: TrustedRoots,
        transaction_dir: PathBuf,
    ) -> Self {
        let mut load_error = None;
        let mut load_problems = Vec::new();
        let saved = match load_dat_sources_config_from(&config_path) {
            Ok(config) => {
                let (registry, problems) = DatSourceRegistry::from_config(&config);
                load_problems = problems;
                registry
            }
            Err(error) => {
                load_error = Some(error.to_string());
                DatSourceRegistry::new()
            }
        };
        let draft = saved.clone();
        // The durable journal directory and any transactions found in it that
        // are still actionable: settled `Applied` batches offered for rollback
        // and interrupted batches offered for explicit crash recovery.
        let (recovery_transactions, _recovery_problems) =
            archivefs_core::dat::rename_apply::find_rollbackable_transactions(&transaction_dir);
        Self {
            config_path,
            database_path: None,
            saved,
            draft,
            load_error,
            load_problems,
            save_state: DatSaveState::Idle,
            action_error: None,
            validations: BTreeMap::new(),
            diagnostic_groups: BTreeMap::new(),
            audit: None,
            audit_error: None,
            audit_elapsed_seconds: None,
            job: None,
            trusted,
            library_folders,
            limits: DatLimits::default(),
            policy_scope: None,
            rename_plan: None,
            rename_plan_error: None,
            audit_generation: 0,
            identity_enrichment_completed: false,
            identity_enrichment: None,
            review_decisions: BTreeMap::new(),
            transaction_dir,
            apply_review: None,
            apply_approved: std::collections::BTreeSet::new(),
            apply_outcome: None,
            apply_error: None,
            subset_available: false,
            apply_running: false,
            rollback_running: false,
            rollback_result: None,
            rollback_error: None,
            apply_job: None,
            recovery_transactions,
            history_records: Vec::new(),
        }
    }

    pub(crate) fn with_database_path(mut self, database_path: Option<PathBuf>) -> Self {
        self.database_path = database_path.filter(|path| path.is_file());
        self
    }

    pub(crate) fn take_identity_enrichment_completed(&mut self) -> bool {
        std::mem::take(&mut self.identity_enrichment_completed)
    }

    /// How many sources are registered on disk, right now - what Home
    /// shows once this page has been visited this session. Reads `saved`,
    /// not `draft`: an unsaved edit should not change what Home reports.
    pub(crate) fn registered_source_count(&self) -> usize {
        self.saved.len()
    }

    /// Whether the draft differs from what is on disk.
    ///
    /// Compared as serialised configuration, because that is exactly what a
    /// save would write: an edit that round-trips to the same document is
    /// genuinely not a change.
    pub(crate) fn is_dirty(&self) -> bool {
        self.draft.to_config() != self.saved.to_config()
    }

    /// Whether a background job is running.
    pub(crate) fn is_busy(&self) -> bool {
        self.job.is_some() || self.apply_job.is_some()
    }

    /// Signals cancellation and immediately forgets the running job, whatever
    /// it targets.
    ///
    /// Dropping `self.job` drops the channel's receiving end, so any message
    /// the worker later sends - including one already in flight - fails
    /// silently on the sending side (every send in this module is `let _ =
    /// sender.send(...)`) rather than being read by a future `poll()`. That is
    /// what makes this safe to call even though the worker thread itself is
    /// not joined: it keeps running to whatever its own bound is (a parse
    /// bounded by `DatLimits`, or an audit that now observes `cancel`), but
    /// nothing it produces can reach page state again.
    fn abandon_running_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// [`Self::abandon_running_job`], but only when the running job targets
    /// `id` - so removing one source does not cancel an audit or validation
    /// legitimately running against a different one.
    fn abandon_job_for(&mut self, id: &str) {
        if self.job.as_ref().is_some_and(|job| job.source_id == id) {
            self.abandon_running_job();
        }
    }

    /// Drains whatever the worker has sent since the last frame.
    ///
    /// Called before [`Self::view`] so the view stays a pure function of state.
    /// Returns true when something arrived, so the caller can request a repaint
    /// only when there is a reason to.
    /// Drains the rename apply/rollback worker, if one is running.
    fn poll_apply(&mut self) -> bool {
        let Some(job) = self.apply_job.as_mut() else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        loop {
            match job.messages.try_recv() {
                Ok(ApplyJobMessage::Applied(outcome)) => {
                    let summary = outcome.summary.clone();
                    let transaction_id = outcome.transaction.transaction_id.clone();
                    self.history_records.push(RenameHistoryRecord {
                        action: RenameHistoryAction::Apply,
                        transaction_id,
                        message: format!(
                            "requested {}, applied {}, skipped {}, failed {}",
                            summary.requested, summary.applied, summary.skipped, summary.failed
                        ),
                    });
                    self.apply_outcome = Some(*outcome);
                    self.apply_error = None;
                    self.apply_running = false;
                    self.apply_review = None;
                    changed = true;
                    finished = true;
                }
                Ok(ApplyJobMessage::RolledBack(outcome)) => {
                    let transaction_id = outcome.transaction.transaction_id.clone();
                    let label = match &outcome.result {
                        RollbackResult::FullyRolledBack => "fully rolled back",
                        RollbackResult::PartiallyRolledBack { .. } => "partially rolled back",
                        RollbackResult::RollbackFailed { .. } => "rollback failed",
                    };
                    self.history_records.push(RenameHistoryRecord {
                        action: RenameHistoryAction::Rollback,
                        transaction_id,
                        message: label.to_string(),
                    });
                    self.rollback_result = Some(outcome.result.clone());
                    self.rollback_error = None;
                    self.rollback_running = false;
                    self.apply_outcome = Some(ApplyOutcome {
                        transaction: outcome.transaction.clone(),
                        summary:
                            archivefs_core::dat::rename_apply::TransactionSummary::from_transaction(
                                &outcome.transaction,
                            ),
                    });
                    changed = true;
                    finished = true;
                }
                Ok(ApplyJobMessage::HardConflicts(detail)) => {
                    self.apply_error = Some(detail);
                    self.subset_available = true;
                    self.apply_running = false;
                    changed = true;
                    finished = true;
                }
                Ok(ApplyJobMessage::Failed(error)) => {
                    if self.apply_running {
                        self.apply_error = Some(error);
                        self.apply_running = false;
                    } else if self.rollback_running {
                        self.rollback_error = Some(error);
                        self.rollback_running = false;
                    }
                    changed = true;
                    finished = true;
                }
                Ok(ApplyJobMessage::Cancelled) => {
                    self.apply_running = false;
                    self.rollback_running = false;
                    changed = true;
                    finished = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.apply_running = false;
                    self.rollback_running = false;
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.apply_job = None;
        }
        changed
    }

    /// Re-reads still-actionable transactions from the journal directory,
    /// reconciling any in-flight (`Applying`/`RollingBack`) entries against the
    /// filesystem so the counts and rollback eligibility reflect what actually
    /// happened. A settled `Applied` transaction is listed as-is, eligible for
    /// rollback.
    fn refresh_recovery(&mut self) {
        let (mut recovery, _) = archivefs_core::dat::rename_apply::find_rollbackable_transactions(
            &self.transaction_dir,
        );
        for transaction in &mut recovery {
            if transaction.entries.iter().any(|entry| {
                matches!(
                    entry.state,
                    archivefs_core::dat::rename_apply::EntryState::Applying
                        | archivefs_core::dat::rename_apply::EntryState::RollingBack
                )
            }) {
                let _ = archivefs_core::dat::rename_apply::reconcile_recovery(
                    transaction,
                    &self.transaction_dir,
                );
            }
        }
        // The transaction applied this session is already shown by the apply
        // outcome card; do not list it a second time in the recovery list.
        if let Some(outcome) = &self.apply_outcome {
            recovery.retain(|transaction| {
                transaction.transaction_id != outcome.transaction.transaction_id
            });
        }
        self.recovery_transactions = recovery;
    }

    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = self.poll_apply();
        // Surface (and reconcile) interrupted transactions on every frame so the
        // recovery banner appears as soon as the page is entered.
        self.refresh_recovery();
        let Some(job) = self.job.as_mut() else {
            return changed;
        };
        let mut finished = false;
        // Read the clock once per drain pass. Every queued message was produced
        // between this pass and the last one, so they all share one elapsed
        // value; timestamping each message afresh would make a drained backlog
        // look like an enormous files-per-second rate and collapse the ETA to
        // near zero. The `delta_seconds > 0` guard inside `EtaEstimator::update`
        // then skips every message after the first of the burst. The job stays
        // alive for the whole pass - terminal messages only flag `finished`,
        // which clears `self.job` after the loop - so reading `job.started_at`
        // here is safe.
        let elapsed = job.started_at.elapsed().as_secs_f64();
        // Coalescing: the detail line is derived once per drain pass from the
        // last progress event, so a backlog of a thousand messages builds one
        // string instead of a thousand. The tracker still ingests every event
        // (that is what keeps the ETA smooth); only the string work is shared.
        let mut last_audit_progress: Option<DatAuditProgress> = None;
        loop {
            match job.messages.try_recv() {
                Ok(JobMessage::Progress(line)) => {
                    // Once cancellation has been requested, stale progress must
                    // not restore an active-looking detail line.
                    if !job.cancel_requested {
                        job.latest = line;
                    }
                    changed = true;
                }
                Ok(JobMessage::AuditProgress(event)) => {
                    // Once cancellation is requested, progress is frozen: the
                    // detail line, the position, and the ETA all stop moving so
                    // a stale report cannot restore an active-looking state.
                    if !job.cancel_requested
                        && let Some(tracker) = job.audit_progress.as_mut()
                    {
                        tracker.update(&event, elapsed);
                        last_audit_progress = Some(event);
                    }
                    changed = true;
                }
                Ok(JobMessage::Validated(report)) => {
                    if job.cancel_requested {
                        // A result that lands after cancellation was requested
                        // must not repopulate state: the user stopped this job.
                        finished = true;
                    } else {
                        let id = report.source_id.clone();
                        // The health the run observed is written onto the
                        // *draft*, so it becomes an unsaved change like any
                        // other and the user chooses whether to keep it.
                        if let Some(entry) = self.draft.get_mut(&id) {
                            entry.health = report.to_health(&entry.path.clone(), entry.kind);
                        }
                        // The grouped diagnostics are derived once here, when
                        // the report lands, and cached: the view is rebuilt
                        // every frame, and re-grouping thousands of diagnostics
                        // per frame would be pure churn for an unchanged report.
                        let groups = group_diagnostics(&id, &report);
                        self.validations.insert(id.clone(), *report);
                        self.diagnostic_groups.insert(id, groups);
                        changed = true;
                        finished = true;
                    }
                }
                Ok(JobMessage::Audited {
                    generation,
                    outcome,
                    enrichment,
                    plan,
                }) => {
                    if job.cancel_requested {
                        // A cancelled audit never appears complete - even when
                        // the worker finished before it observed the flag, the
                        // page must not present the late result as a completed
                        // audit.
                        finished = true;
                    } else if generation != self.audit_generation {
                        // A stale generation (an older worker's result landing
                        // after a newer audit started) can never replace the
                        // current audit or plan.
                        finished = true;
                    } else {
                        // The elapsed time is measured from the job's own start
                        // instant, so the summary can say how long the run took
                        // without the worker carrying any timestamps.
                        self.audit_elapsed_seconds = Some(job.started_at.elapsed().as_secs());
                        self.audit = Some(outcome);
                        self.identity_enrichment_completed = self.database_path.is_some();
                        self.identity_enrichment = enrichment;
                        self.audit_error = None;
                        match plan {
                            Some(plan) => {
                                self.rename_plan = Some(*plan);
                                self.rename_plan_error = None;
                            }
                            None => {
                                self.rename_plan = None;
                                self.rename_plan_error = Some(
                                    "the rename plan could not be produced (cancelled or the \
                                     source files could not be inspected)"
                                        .to_string(),
                                );
                            }
                        }
                        changed = true;
                        finished = true;
                    }
                }
                Ok(JobMessage::Failed(error)) => {
                    match job.kind {
                        JobKind::Audit => {
                            self.audit = None;
                            self.audit_elapsed_seconds = None;
                            if !job.cancel_requested {
                                self.audit_error = Some(error);
                            }
                        }
                        JobKind::Validate => {
                            if !job.cancel_requested {
                                self.action_error = Some(error);
                            }
                        }
                    }
                    changed = true;
                    finished = true;
                }
                Ok(JobMessage::Cancelled) => {
                    // A cancelled audit produces no summary and no elapsed time.
                    self.audit = None;
                    self.audit_elapsed_seconds = None;
                    changed = true;
                    finished = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        // Derive the detail line once from the last progress event of the pass.
        if let Some(event) = last_audit_progress {
            job.latest = describe(&event);
        }
        if finished {
            self.job = None;
        }
        changed
    }

    /// Applies one action.
    pub(crate) fn apply(&mut self, action: DatSourcesPageAction) {
        // Any action clears the "saved" flash: leaving it up while the user
        // makes a new edit would say the current state is what is on disk.
        if !matches!(action, DatSourcesPageAction::Save) {
            self.save_state = DatSaveState::Idle;
        }
        match action {
            DatSourcesPageAction::AddFile { path } => self.add(path, DatSourceKind::File),
            DatSourcesPageAction::AddFolder { path } => self.add(path, DatSourceKind::Folder),
            DatSourcesPageAction::SetEnabled { id, enabled } => {
                if let Some(entry) = self.draft.get_mut(&id) {
                    entry.enabled = enabled;
                }
            }
            DatSourcesPageAction::SetPlatform { id, platform } => {
                if let Some(entry) = self.draft.get_mut(&id) {
                    entry.platform = platform.filter(|value| !value.trim().is_empty());
                }
            }
            DatSourcesPageAction::Remove { id } => {
                self.action_error = None;
                // A job in flight for exactly this source must not be allowed
                // to complete after removal: `poll()` would otherwise write a
                // validation or audit result for a source no longer in the
                // registry. The GUI already keeps Remove disabled while any
                // job runs, but that is a presentation-layer gate, not a
                // guarantee this state machine can rely on - so it is
                // enforced here too.
                self.abandon_job_for(&id);
                // Registry only. The DAT file, the folder, and everything in it
                // are untouched - see `DatSourceRegistry::remove`.
                if self.draft.remove(&id).is_some() {
                    self.validations.remove(&id);
                    self.diagnostic_groups.remove(&id);
                    if self
                        .audit
                        .as_ref()
                        .is_some_and(|outcome| outcome.source_id == id)
                    {
                        // A result attributed to a source that is no longer
                        // registered would have nothing to point at.
                        self.audit = None;
                        self.rename_plan = None;
                        self.rename_plan_error = None;
                        self.review_decisions.clear();
                        self.abandon_apply_work();
                    }
                }
            }
            DatSourcesPageAction::Validate { id } => self.start_validate(id),
            DatSourcesPageAction::Audit { id, scan_root } => self.start_audit(id, scan_root),
            DatSourcesPageAction::CancelJob => {
                if let Some(job) = self.job.as_mut() {
                    job.cancel.store(true, Ordering::Relaxed);
                    // The visible card flips to "Stopping…" this frame; the job
                    // stays busy until the worker confirms termination.
                    job.cancel_requested = true;
                }
            }
            DatSourcesPageAction::Revert => {
                // A running job's result would otherwise still land after the
                // discard it was supposed to be swept away by: `poll()` does
                // not check that the job's source survived the revert, so a
                // job left running here would populate `self.audit` (or a
                // stale `self.validations` entry) for a source the user just
                // discarded - including one that no longer exists in the
                // registry at all, if it had never been saved. Dropping the
                // job unconditionally, not just when its target vanished,
                // matches what "discard changes" means: nothing this job
                // would report is still trustworthy against the reverted
                // state, whether or not the row it targeted survives.
                self.abandon_running_job();
                self.draft = self.saved.clone();
                self.action_error = None;
                // Discard also forgets the session's validation records: a
                // re-added source (which reuses its auto-suggested id) must not
                // show yesterday's Inspect detail or diagnostic groups next to
                // a "Not checked" badge. Both maps are discarded together.
                self.validations.clear();
                self.diagnostic_groups.clear();
                // The policy scope is not persisted, so discarding returns it
                // to the global scope rather than leaving a platform selected
                // whose override the user just discarded.
                self.policy_scope = None;
                // The audit result, its plan and the session-only review
                // decisions describe a discarded state, so they go with it.
                self.audit = None;
                self.audit_elapsed_seconds = None;
                self.rename_plan = None;
                self.rename_plan_error = None;
                self.review_decisions.clear();
                self.abandon_apply_work();
            }
            DatSourcesPageAction::Save => self.save(),
            DatSourcesPageAction::SelectPolicyScope { scope } => {
                self.policy_scope = scope;
            }
            DatSourcesPageAction::MoveRegion {
                scope,
                index,
                delta,
            } => {
                self.with_policy_targets(&scope, |targets| {
                    move_index(targets.region_list(), index, delta);
                });
            }
            DatSourcesPageAction::AddRegion { scope, region } => {
                self.with_policy_targets(&scope, |targets| {
                    let list = targets.region_list();
                    if !list.iter().any(|value| value == region.as_str()) {
                        list.push(region.as_str().to_string());
                    }
                });
            }
            DatSourcesPageAction::RemoveRegion { scope, index } => {
                self.with_policy_targets(&scope, |targets| {
                    let list = targets.region_list();
                    if index < list.len() {
                        list.remove(index);
                    }
                });
            }
            DatSourcesPageAction::MoveLanguage {
                scope,
                index,
                delta,
            } => {
                self.with_policy_targets(&scope, |targets| {
                    move_index(targets.language_list(), index, delta);
                });
            }
            DatSourcesPageAction::AddLanguage { scope, preference } => {
                self.with_policy_targets(&scope, |targets| {
                    let list = targets.language_list();
                    if !list.iter().any(|value| value == preference.as_str()) {
                        list.push(preference.as_str().to_string());
                    }
                });
            }
            DatSourcesPageAction::RemoveLanguage { scope, index } => {
                self.with_policy_targets(&scope, |targets| {
                    let list = targets.language_list();
                    if index < list.len() {
                        list.remove(index);
                    }
                });
            }
            DatSourcesPageAction::ClearRegion { scope } => {
                self.with_policy_targets(&scope, |targets| {
                    targets.region_list().clear();
                });
            }
            DatSourcesPageAction::ClearLanguage { scope } => {
                self.with_policy_targets(&scope, |targets| {
                    targets.language_list().clear();
                });
            }
            DatSourcesPageAction::SetRevisionPolicy { scope, policy } => {
                self.with_policy_targets(&scope, |targets| {
                    *targets.revision = Some(policy.as_str().to_string());
                });
            }
            DatSourcesPageAction::SetClonePolicy { scope, policy } => {
                self.with_policy_targets(&scope, |targets| {
                    *targets.clone = Some(policy.as_str().to_string());
                });
            }
            DatSourcesPageAction::SetContentSelection { scope, policy } => {
                self.with_policy_targets(&scope, |targets| {
                    *targets.content = Some(policy.as_str().to_string());
                });
                // The completed audit remains authoritative, but its selection
                // annotation and any action plan were built for the old policy.
                self.audit = None;
                self.audit_error = None;
                self.rename_plan = None;
                self.rename_plan_error = None;
                self.review_decisions.clear();
                self.abandon_apply_work();
            }
            DatSourcesPageAction::SetReviewDecision { path, decision } => match decision {
                Some(decision) => {
                    self.review_decisions.insert(path, decision);
                }
                None => {
                    self.review_decisions.remove(&path);
                }
            },
            DatSourcesPageAction::ClearReviewDecisions => {
                self.review_decisions.clear();
            }
            DatSourcesPageAction::BeginApplyReview => self.begin_apply_review(),
            DatSourcesPageAction::ConfirmApply { typed } => {
                self.confirm_apply(HardConflictMode::AbortAll, typed)
            }
            DatSourcesPageAction::ConfirmApplySafeSubset { typed } => {
                self.confirm_apply(HardConflictMode::SkipUnsafeSubset, typed)
            }
            DatSourcesPageAction::CancelApplyReview => {
                self.apply_review = None;
                self.apply_error = None;
                self.subset_available = false;
            }
            DatSourcesPageAction::RollbackTransaction { id } => self.start_rollback(id),
            DatSourcesPageAction::RecoveryChoice { id, choice } => {
                self.handle_recovery_choice(id, choice)
            }
            DatSourcesPageAction::ClearApplyOutcome => {
                self.apply_outcome = None;
                self.rollback_result = None;
                self.apply_error = None;
                self.rollback_error = None;
            }
        }
    }

    /// Builds the transaction for the approved, applicable proposals and shows
    /// the read-only review. No mutation.
    fn begin_apply_review(&mut self) {
        let Some(plan) = self.rename_plan.as_ref() else {
            self.apply_error = Some("no rename plan is available".to_string());
            return;
        };
        let approved: std::collections::BTreeSet<String> = self
            .review_decisions
            .iter()
            .filter(|(_, decision)| archivefs_core::dat::rename_apply::is_approved(decision))
            .map(|(path, _)| path.clone())
            .collect();
        if approved.is_empty() {
            self.apply_error = Some(
                "no proposals are accepted for review; accept at least one Suggested proposal"
                    .to_string(),
            );
            return;
        }
        match archivefs_core::dat::rename_apply::build_transaction(plan, &approved, plan.generation)
        {
            Ok(transaction) => {
                self.apply_approved = approved;
                self.apply_review = Some(transaction);
                self.apply_error = None;
                self.subset_available = false;
            }
            Err(error) => {
                self.apply_error = Some(error.to_string());
            }
        }
    }

    /// Confirms the review and runs the apply on a worker thread. The typed
    /// confirmation is validated here (the drawing layer collects it), so a
    /// large batch cannot be confirmed without the exact phrase.
    fn confirm_apply(&mut self, mode: HardConflictMode, typed: String) {
        let Some(review) = self.apply_review.as_ref() else {
            return;
        };
        let count = review.entries.len();
        let required = typed_confirmation_phrase(count);
        if count > TYPED_CONFIRMATION_THRESHOLD && typed.trim() != required {
            self.apply_error = Some(format!("type {required} to confirm this batch"));
            return;
        }
        self.spawn_apply(mode);
    }

    /// Spawns the apply worker for the current review transaction.
    fn spawn_apply(&mut self, mode: HardConflictMode) {
        if self.apply_job.is_some() {
            return;
        }
        let Some(review) = self.apply_review.as_ref() else {
            return;
        };
        let mut transaction = review.clone();
        let approved = self.apply_approved.clone();
        let current_generation = self.rename_plan.as_ref().map(|p| p.generation).unwrap_or(0);
        let trusted = self.trusted.clone();
        let journal_dir = self.transaction_dir.clone();
        let (sender, messages) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let apply_sender = sender.clone();

        std::thread::spawn(move || {
            let result = apply_transaction(&mut ApplyExecution {
                transaction: &mut transaction,
                approved_paths: approved,
                current_generation,
                trusted,
                journal_dir,
                hard_conflict_mode: mode,
                cancel: &worker_cancel,
                directory_policy:
                    archivefs_core::dat::rename_apply::preflight::DirectoryPolicy::SameDirectory,
                allow_symlink_source: false,
            });
            let _ = match result {
                Ok(outcome) => apply_sender.send(ApplyJobMessage::Applied(Box::new(outcome))),
                Err(ApplyError::Cancelled) => apply_sender.send(ApplyJobMessage::Cancelled),
                Err(ApplyError::HardConflicts(conflicts)) => {
                    let detail = conflicts
                        .iter()
                        .map(|(path, reasons)| {
                            format!(
                                "{}: {}",
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string()),
                                reasons.join("; ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    apply_sender.send(ApplyJobMessage::HardConflicts(format!(
                        "preflight found {} hard conflict(s); nothing was renamed.\n{detail}",
                        conflicts.len()
                    )))
                }
                Err(error) => apply_sender.send(ApplyJobMessage::Failed(error.to_string())),
            };
        });

        self.apply_job = Some(ApplyJob { cancel, messages });
        self.apply_running = true;
        self.apply_error = None;
        self.subset_available = false;
    }

    /// Rolls back a transaction (the last applied one, or a recovered one).
    fn start_rollback(&mut self, transaction_id: String) {
        if self.apply_job.is_some() {
            return;
        }
        // Load the journal for this transaction id.
        let Some(path) =
            archivefs_core::dat::rename_apply::journal_path(&self.transaction_dir, &transaction_id)
        else {
            self.rollback_error = Some("transaction id cannot name a journal".to_string());
            return;
        };
        let mut transaction = match archivefs_core::dat::rename_apply::read_journal(&path) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.rollback_error = Some(format!("journal unreadable: {error}"));
                return;
            }
        };
        let journal_dir = self.transaction_dir.clone();
        let (sender, messages) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let rollback_sender = sender.clone();
        std::thread::spawn(move || {
            let result = rollback_transaction(&mut transaction, &journal_dir, &worker_cancel);
            let _ = match result {
                Ok(outcome) => rollback_sender.send(ApplyJobMessage::RolledBack(Box::new(outcome))),
                Err(error) => rollback_sender.send(ApplyJobMessage::Failed(error)),
            };
        });
        self.apply_job = Some(ApplyJob { cancel, messages });
        self.rollback_running = true;
        self.rollback_error = None;
    }

    /// Handles a crash-recovery choice for an interrupted transaction.
    fn handle_recovery_choice(&mut self, id: String, choice: RecoveryChoice) {
        match choice {
            RecoveryChoice::RollBack => self.start_rollback(id),
            RecoveryChoice::LeaveUntouched => {
                // The journal stays on disk; the user chose not to act now.
                self.recovery_transactions
                    .retain(|transaction| transaction.transaction_id != id);
            }
        }
    }

    /// History & Logs records produced by apply/rollback outcomes, drained by
    /// the shell. No private paths are included.
    pub(crate) fn drain_history_records(&mut self) -> Vec<RenameHistoryRecord> {
        std::mem::take(&mut self.history_records)
    }

    /// Abandons in-memory apply/review state (the journals and files are
    /// untouched; an in-flight worker simply stops delivering to this page).
    fn abandon_apply_work(&mut self) {
        if let Some(job) = self.apply_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.apply_review = None;
        self.apply_outcome = None;
        self.apply_error = None;
        self.subset_available = false;
        self.apply_running = false;
        self.rollback_running = false;
        self.rollback_result = None;
        self.rollback_error = None;
    }

    /// Applies `edit` to the four policy fields at `scope` (global or one
    /// platform's override).
    ///
    /// Editing a platform scope creates the override entry on demand; when
    /// the edit leaves it empty it is pruned again, so saving never writes an
    /// empty `[policy.platforms.X]` table the user did not ask for.
    fn with_policy_targets(
        &mut self,
        scope: &Option<String>,
        edit: impl FnOnce(&mut PolicyTargets<'_>),
    ) {
        match scope {
            None => {
                let policy = self.draft.policy_mut();
                let mut targets = PolicyTargets::new(
                    &mut policy.region_preferences,
                    &mut policy.language_preferences,
                    &mut policy.revision_policy,
                    &mut policy.clone_policy,
                    &mut policy.content_selection,
                );
                edit(&mut targets);
                targets.normalise_empty_lists();
            }
            Some(platform) => {
                let overrides = self
                    .draft
                    .policy_mut()
                    .platforms
                    .get_or_insert_with(BTreeMap::new);
                let entry = overrides.entry(platform.clone()).or_default();
                let mut targets = PolicyTargets::new(
                    &mut entry.region_preferences,
                    &mut entry.language_preferences,
                    &mut entry.revision_policy,
                    &mut entry.clone_policy,
                    &mut entry.content_selection,
                );
                edit(&mut targets);
                targets.normalise_empty_lists();
                self.prune_empty_overrides();
            }
        }
    }

    /// Removes platform override entries that no longer set anything, and the
    /// whole `platforms` map when it is empty.
    fn prune_empty_overrides(&mut self) {
        let policy = self.draft.policy_mut();
        let Some(overrides) = policy.platforms.as_mut() else {
            return;
        };
        overrides.retain(|_, entry| {
            entry.region_preferences.is_some()
                || entry.language_preferences.is_some()
                || entry.revision_policy.is_some()
                || entry.clone_policy.is_some()
                || entry.content_selection.is_some()
        });
        if overrides.is_empty() {
            policy.platforms = None;
        }
    }

    fn add(&mut self, path: PathBuf, kind: DatSourceKind) {
        self.action_error = None;
        let entry = DatSourceEntry {
            origin: Some("added on the DAT Sources page".to_string()),
            ..DatSourceEntry::new(
                self.draft.suggest_id(&path),
                suggest_display_name(&path),
                path,
                kind,
            )
        };
        if let Err(error) = self.draft.add(entry) {
            self.action_error = Some(error.to_string());
        }
    }

    fn save(&mut self) {
        if self.load_error.is_some() {
            self.save_state = DatSaveState::Failed(
                "Not saving: the existing registry file could not be read, and overwriting it \
                 would discard it."
                    .to_string(),
            );
            return;
        }
        match save_dat_sources_config_to(&self.config_path, &self.draft.to_config()) {
            Ok(()) => {
                self.saved = self.draft.clone();
                self.save_state = DatSaveState::Saved;
            }
            Err(error) => self.save_state = DatSaveState::Failed(error.to_string()),
        }
    }

    fn start_validate(&mut self, id: String) {
        if self.job.is_some() {
            return;
        }
        let Some(entry) = self.draft.get(&id).cloned() else {
            return;
        };
        self.action_error = None;
        let (sender, messages) = sync_channel(PROGRESS_QUEUE_DEPTH);
        let cancel = Arc::new(AtomicBool::new(false));
        let limits = self.limits;
        let name = entry.display_name.clone();
        let platform_display = authoritative_platform(&entry);

        std::thread::spawn(move || {
            send_progress(&sender, JobMessage::Progress(format!("Reading {name}…")));
            let report = validate_dat_source(&entry, limits);
            let _ = sender.send(JobMessage::Validated(Box::new(report)));
        });

        self.job = Some(RunningJob {
            kind: JobKind::Validate,
            source_id: id,
            cancel,
            cancel_requested: false,
            messages,
            latest: "Starting…".to_string(),
            started_at: Instant::now(),
            audit_progress: None,
            platform_display,
        });
    }

    fn start_audit(&mut self, id: String, scan_root: PathBuf) {
        if self.job.is_some() {
            return;
        }
        let Some(entry) = self.draft.get(&id).cloned() else {
            return;
        };
        self.audit = None;
        self.identity_enrichment_completed = false;
        self.identity_enrichment = None;
        self.audit_error = None;
        self.audit_elapsed_seconds = None;
        self.rename_plan = None;
        self.rename_plan_error = None;
        self.review_decisions.clear();
        self.abandon_apply_work();
        // Each audit is a generation. A result from an older generation is a
        // stale plan and is dropped, so an old plan can never replace a new one.
        self.audit_generation = self.audit_generation.wrapping_add(1);
        let generation = self.audit_generation;
        let (sender, messages) = sync_channel(PROGRESS_QUEUE_DEPTH);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let trusted = self.trusted.clone();
        let platform_display = authoritative_platform(&entry);
        let canonical_platform = entry
            .platform
            .as_deref()
            .and_then(|platform| archivefs_core::canonical_platform_for_alias(platform));
        // The audit is annotated with the effective DAT policy so multi-
        // candidate verdicts can be shown in the user's preferred order. The
        // policy is resolved now, from the draft, by the core resolver; the
        // worker thread only reads the resolution.
        let policy = resolve(
            self.draft.policy(),
            canonical_platform,
            participating_sources(&self.draft, canonical_platform),
        );
        let request = DatAuditRequest {
            source_id: entry.id.clone(),
            source_display_name: entry.display_name.clone(),
            dat_path: entry.path.clone(),
            dat_kind: entry.kind,
            scan_root,
            limits: self.limits,
            policy: Some(policy),
            platform: canonical_platform.map(str::to_string),
        };
        let database_path = self.database_path.clone();

        std::thread::spawn(move || {
            let report_sender = sender.clone();
            let outcome = run_dat_audit(&request, &trusted, &worker_cancel, &|progress| {
                send_progress(&report_sender, JobMessage::AuditProgress(progress));
            });
            let _ = match outcome {
                Ok(outcome) => {
                    // Cancellation may race with the audit's final comparison.
                    // Re-check at the metadata-write boundary so a result the
                    // page will discard cannot still enrich the catalogue.
                    let enrichment = if worker_cancel.load(Ordering::Acquire) {
                        None
                    } else if let Some(database_path) = database_path {
                        match archivefs_core::Database::open_or_create(&database_path).and_then(
                            |mut database| {
                                database.enrich_platforms_from_dat_audit(&outcome, generation)
                            },
                        ) {
                            Ok(enrichment) => {
                                send_progress(
                                    &sender,
                                    JobMessage::Progress(format!(
                                        "Platform identity enrichment: {} applied, {} already current, {} manual assignment(s) preserved, {} conflict(s) require review.",
                                        enrichment.applied,
                                        enrichment.unchanged,
                                        enrichment.manual_preserved,
                                        enrichment.conflicts,
                                    )),
                                );
                                Some(Box::new(enrichment))
                            }
                            Err(error) => {
                                send_progress(
                                    &sender,
                                    JobMessage::Progress(format!(
                                        "DAT audit completed, but platform identity metadata could not be updated: {error}"
                                    )),
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };
                    // Build the read-only rename plan from the finished audit.
                    // This is cheap (no re-scan, no hashing) but cancellable and
                    // runs on the worker, never on the UI thread.
                    send_progress(
                        &sender,
                        JobMessage::Progress("Building rename plan…".to_string()),
                    );
                    let plan = build_rename_plan(
                        &outcome,
                        &RenamePlanContext { generation },
                        &worker_cancel,
                    )
                    .ok()
                    .map(Box::new);
                    sender.send(JobMessage::Audited {
                        generation,
                        outcome: Box::new(outcome),
                        enrichment,
                        plan,
                    })
                }
                Err(archivefs_core::dat::sources::audit_run::DatAuditError::Cancelled) => {
                    sender.send(JobMessage::Cancelled)
                }
                Err(error) => sender.send(JobMessage::Failed(error.to_string())),
            };
        });

        self.job = Some(RunningJob {
            kind: JobKind::Audit,
            source_id: id,
            cancel,
            cancel_requested: false,
            messages,
            latest: "Starting…".to_string(),
            started_at: Instant::now(),
            audit_progress: Some(AuditProgressTracker::new()),
            platform_display,
        });
    }

    /// The validation report for one source, if it has been validated this
    /// session.
    pub(crate) fn validation(&self, id: &str) -> Option<&DatValidationReport> {
        self.validations.get(id)
    }

    /// Builds the view model. Pure: no I/O beyond a metadata check for
    /// staleness, no clock beyond formatting a stored timestamp (and the
    /// running job's elapsed time, read from the instant the job started).
    pub(crate) fn view(&self) -> DatSourcesPageView {
        let rows: Vec<DatSourceRowView> = self
            .draft
            .sorted_all()
            .into_iter()
            .map(|entry| self.row_view(entry))
            .collect();

        DatSourcesPageView {
            unresolved: self
                .draft
                .unresolved_settings()
                .iter()
                .map(|setting: &UnresolvedDatSetting| UnresolvedDatRowView {
                    explanation: setting.describe(),
                })
                .collect(),
            load_problems: self.load_problems.clone(),
            dirty: self.is_dirty(),
            config_path: self.config_path.clone(),
            save_state: self.save_state.clone(),
            load_error: self.load_error.clone(),
            action_error: self.action_error.clone(),
            pending_consequences: self.pending_consequences(&rows),
            running: self.job.as_ref().map(|job| RunningJobView {
                source_id: job.source_id.clone(),
                what: match job.kind {
                    JobKind::Validate => "Validating",
                    JobKind::Audit => "Auditing",
                },
                detail: job.latest.clone(),
                // Validation is bounded by `DatLimits` and finishes on its own;
                // offering a Cancel that the parser does not check would be a
                // button that lies.
                cancellable: job.kind == JobKind::Audit,
                cancellation_requested: job.cancel_requested,
                // The elapsed clock is read here rather than at poll time so
                // the running card keeps ticking between progress messages; it
                // is still a pure function of state with no I/O.
                progress: job
                    .audit_progress
                    .as_ref()
                    .map(|tracker| tracker.view(job.started_at.elapsed().as_secs())),
                platform_display: job.platform_display.clone(),
            }),
            library_folders: self.library_folders.clone(),
            audit: self
                .audit
                .as_ref()
                .map(|outcome| Box::new(audit_view(outcome, self.audit_elapsed_seconds))),
            audit_error: self.audit_error.clone(),
            identity_enrichment: self.identity_enrichment.clone(),
            policy: self.policy_view(),
            rename_plan: self.rename_plan_view(),
            rename_apply: self.rename_apply_view(),
            rows,
        }
    }

    /// Builds the apply/recovery view. Pure: it renders the core's built
    /// transaction, the worker's outcome, and the recovered journals.
    fn rename_apply_view(&self) -> RenameApplyView {
        let review = self.apply_review.as_ref().map(|transaction| {
            let count = transaction.entries.len();
            ApplyReviewView {
                transaction: transaction.clone(),
                rows: transaction
                    .entries
                    .iter()
                    .map(|entry| ApplyReviewRow {
                        current_basename: entry.original_basename.clone(),
                        proposed_basename: entry.proposed_basename.clone(),
                    })
                    .collect(),
                trusted_root: self
                    .trusted
                    .roots()
                    .first()
                    .map(|root| root.to_string_lossy().into_owned()),
                required_phrase: (count > TYPED_CONFIRMATION_THRESHOLD)
                    .then(|| typed_confirmation_phrase(count)),
            }
        });
        let outcome = self.apply_outcome.as_ref().map(|outcome| ApplyOutcomeView {
            transaction_id: outcome.transaction.transaction_id.clone(),
            state: outcome.transaction.state,
            requested: outcome.summary.requested,
            applied: outcome.summary.applied,
            skipped: outcome.summary.skipped,
            failed: outcome.summary.failed,
            rows: outcome
                .transaction
                .entries
                .iter()
                .map(|entry| ApplyRowView {
                    current_basename: entry.original_basename.clone(),
                    proposed_basename: entry.proposed_basename.clone(),
                    state: entry.state,
                    failure_reason: entry.failure_reason.clone(),
                })
                .collect(),
        });
        let rollback_result = self.rollback_result.as_ref().map(|result| match result {
            RollbackResult::FullyRolledBack => RollbackResultView {
                label: "Fully rolled back",
                detail: "every applied rename was reversed and confirmed.".to_string(),
            },
            RollbackResult::PartiallyRolledBack {
                rolled_back,
                failed,
            } => RollbackResultView {
                label: "Partially rolled back",
                detail: format!(
                    "{} rename(s) reversed; {} could not be: {}",
                    rolled_back.len(),
                    failed.len(),
                    failed
                        .iter()
                        .map(|(path, reason)| format!("{} ({reason})", path.display()))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
            RollbackResult::RollbackFailed { failed } => RollbackResultView {
                label: "Rollback failed",
                detail: failed
                    .iter()
                    .map(|(path, reason)| format!("{} ({reason})", path.display()))
                    .collect::<Vec<_>>()
                    .join("; "),
            },
        });
        let recovery = self
            .recovery_transactions
            .iter()
            .map(|transaction| RecoveryTransactionView {
                transaction_id: transaction.transaction_id.clone(),
                state: transaction.state,
                applied_count: transaction.applied_count(),
                total_count: transaction.entries.len(),
            })
            .collect();
        RenameApplyView {
            review,
            outcome,
            apply_error: self.apply_error.clone(),
            subset_available: self.subset_available,
            rollback_result,
            rollback_error: self.rollback_error.clone(),
            apply_running: self.apply_running,
            rollback_running: self.rollback_running,
            recovery,
            journal_dir: self.transaction_dir.to_string_lossy().into_owned(),
        }
    }

    /// Builds the rename-planning section view from the stored plan and the
    /// session-only review decisions. Renders the core's output; nothing here
    /// re-derives or re-ranks.
    fn rename_plan_view(&self) -> Option<RenamePlanView> {
        let Some(plan) = self.rename_plan.as_ref() else {
            // A plan build that failed is still worth showing, so the user
            // learns why rather than the section silently vanishing.
            return self.rename_plan_error.as_ref().map(|error| RenamePlanView {
                generation: self.audit_generation,
                scan_root_short: String::new(),
                platform_display: None,
                source_display_name: String::new(),
                counts: archivefs_core::dat::rename_plan::RenamePlanCounts::default(),
                audited_total: 0,
                verified_total: 0,
                truncated: false,
                rows: Vec::new(),
                error: Some(error.clone()),
            });
        };
        let rows = plan
            .proposals
            .iter()
            .map(|proposal| RenamePlanRowView {
                source_path: proposal.source_path.clone(),
                current_basename: proposal.current_basename.clone(),
                proposed_basename: proposal.proposed_basename.clone(),
                platform_display: proposal.platform_display.clone(),
                source_display_name: proposal.source_display_name.clone(),
                game_name: proposal.game_name.clone(),
                rom_name: proposal.rom_name.clone(),
                verdict_label: proposal.verdict_label.clone(),
                content: content_technical_view(
                    &proposal.content_classification,
                    &proposal.original_metadata,
                ),
                state: proposal.state,
                object_kind_label: proposal.object_kind.label(),
                explanations: proposal.explanations.clone(),
                ambiguity_reason: proposal.ambiguity_reason.clone(),
                collision_detail: proposal.collision.as_ref().map(|collision| {
                    if collision.colliding_is_symlink {
                        format!("{} (the colliding path is a symlink)", collision.detail)
                    } else {
                        collision.detail.clone()
                    }
                }),
                blockers: proposal.blockers.clone(),
                extension_preserved: proposal.extension_status
                    == Some(archivefs_core::dat::rename_plan::ExtensionStatus::Preserved),
                sanitisation_notes: proposal.sanitisation_notes.clone(),
                decision: self
                    .review_decisions
                    .get(&proposal.source_path.to_string_lossy().into_owned())
                    .copied(),
            })
            .collect();
        Some(RenamePlanView {
            generation: plan.generation,
            scan_root_short: shorten_path(&plan.scan_root),
            platform_display: plan.platform_display.clone(),
            source_display_name: plan.source_display_name.clone(),
            counts: plan.counts,
            audited_total: plan.audited_total,
            verified_total: plan.verified_total,
            truncated: plan.truncated,
            rows,
            error: self.rename_plan_error.clone(),
        })
    }

    /// Builds the DAT Matching Policy section view, resolving the effective
    /// policy for the scope being edited through the core resolver.
    fn policy_view(&self) -> DatPolicyView {
        // The scope the user selected can outlive the source whose platform
        // offered it: if that platform is no longer available (every source
        // for it removed), fall back to the global scope rather than showing
        // a scope with nothing to inspect.
        let scope = match &self.policy_scope {
            Some(platform) if self.scope_is_available(platform) => Some(platform.clone()),
            _ => None,
        };
        let config = self.draft.policy();
        let effective = resolve(
            config,
            scope.as_deref(),
            participating_sources(&self.draft, scope.as_deref()),
        );

        let scopes_available = self.policy_scopes_available();
        let scope_label = match &scope {
            None => "All platforms".to_string(),
            Some(platform) => archivefs_core::platform::display_name_for(platform).to_string(),
        };

        // The lists shown for editing are the scope's *authored* lists - what
        // is actually persisted there - not the resolved ones. The Effective
        // Policy Summary below is where the resolved (possibly inherited)
        // values appear, so editing a platform scope starts from an empty
        // override and never silently copies the whole global list into it.
        let region_preferences = authored_region_list(config, &scope)
            .into_iter()
            .filter_map(|value| {
                RegionId::parse(&value).map(|region| PolicyPreferenceRowView {
                    value: region.as_str().to_string(),
                    label: region.label().to_string(),
                    position: 0,
                })
            })
            .enumerate()
            .map(|(index, mut row)| {
                row.position = index + 1;
                row
            })
            .collect();
        let language_preferences = authored_language_list(config, &scope)
            .into_iter()
            .filter_map(|value| {
                LanguagePreference::parse(&value).map(|preference| PolicyPreferenceRowView {
                    value: preference.as_str().to_string(),
                    label: preference.label().to_string(),
                    position: 0,
                })
            })
            .enumerate()
            .map(|(index, mut row)| {
                row.position = index + 1;
                row
            })
            .collect();

        let problems = validate_policy_config(config)
            .into_iter()
            .map(|problem| problem.message)
            .collect();

        let summary = effective_summary_view(&effective, &scope);
        DatPolicyView {
            scope,
            scope_label,
            scopes_available,
            region_preferences,
            language_preferences,
            revision_policy: effective.revision_policy,
            clone_policy: effective.clone_policy,
            content_selection: effective.content_selection,
            effective: summary,
            problems,
            editable: self.load_error.is_none(),
        }
    }

    /// Whether `platform` is still offered as a policy scope: some source is
    /// assigned to it.
    fn scope_is_available(&self, platform: &str) -> bool {
        self.draft.entries().iter().any(|entry| {
            entry
                .platform
                .as_deref()
                .and_then(archivefs_core::canonical_platform_for_alias)
                == Some(platform)
        })
    }

    /// The platforms a policy scope can target: every platform some source is
    /// assigned to, canonicalised, deduplicated, sorted by display name.
    fn policy_scopes_available(&self) -> Vec<PolicyScopeOption> {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for entry in self.draft.entries() {
            if let Some(platform) = &entry.platform
                && let Some(canonical) = archivefs_core::canonical_platform_for_alias(platform)
            {
                seen.insert(canonical.to_string(), canonical.to_string());
            }
        }
        let mut options: Vec<PolicyScopeOption> = seen
            .into_values()
            .map(|platform| PolicyScopeOption {
                id: Some(platform.clone()),
                label: archivefs_core::platform::display_name_for(&platform).to_string(),
            })
            .collect();
        options.sort_by(|a, b| a.label.cmp(&b.label));
        options
    }

    fn row_view(&self, entry: &DatSourceEntry) -> DatSourceRowView {
        let saved = self.saved.get(&entry.id);
        let changed = match saved {
            None => true,
            Some(saved) => saved != entry,
        };
        let validation = self.validation(&entry.id);
        // Read the cached groups when the report landed through a validation
        // run; fall back to deriving them on demand so test-injected reports
        // (which bypass the poll path) still render.
        let groups = self
            .diagnostic_groups
            .get(&entry.id)
            .cloned()
            .unwrap_or_else(|| {
                validation
                    .map(|report| group_diagnostics(&entry.id, report))
                    .unwrap_or_default()
            });
        let incomplete_load = validation.is_some_and(|report| report.truncated);
        let dat_files_read = incomplete_load.then(|| validation.unwrap().files.len() as u64);
        let dat_files_total = incomplete_load
            .then(|| {
                validation
                    .and_then(|report| report.total_dat_files)
                    .map(|n| n as u64)
            })
            .flatten();
        DatSourceRowView {
            id: entry.id.clone(),
            display_name: entry.display_name.clone(),
            path: entry.path.to_string_lossy().into_owned(),
            kind_label: entry.kind.label(),
            enabled: entry.enabled,
            platform_display: entry.platform_display(),
            platform_id: entry.platform.clone(),
            platform_unresolved: !entry.platform_is_resolved(),
            formats: entry.health.formats.clone().unwrap_or_default(),
            health_state: entry.health.state(),
            health_detail: entry.health.detail.clone(),
            last_validated: entry
                .health
                .last_validated_unix_seconds
                .map(format_unix_timestamp),
            health_stale: entry.health.is_stale_for(&entry.path, entry.kind),
            entry_count: entry.health.entry_count,
            rom_count: entry.health.rom_count,
            changed,
            busy: self
                .job
                .as_ref()
                .is_some_and(|job| job.source_id == entry.id),
            detail: self.validation(&entry.id).map(inspect_view),
            groups,
            incomplete_load,
            dat_files_read,
            dat_files_total,
            // The full warning details are kept inline on this card; nothing is
            // recorded in History & Logs today, so nothing points there.
            history_link_available: false,
        }
    }

    /// Plain-language description of what saving would do, one line per change.
    fn pending_consequences(&self, rows: &[DatSourceRowView]) -> Vec<String> {
        if !self.is_dirty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for row in rows.iter().filter(|row| row.changed) {
            match self.saved.get(&row.id) {
                None => out.push(format!(
                    "'{}' will be registered, pointing at {}.",
                    row.display_name, row.path
                )),
                Some(saved) => {
                    if saved.enabled != row.enabled {
                        out.push(if row.enabled {
                            format!("'{}' will be used again.", row.display_name)
                        } else {
                            format!(
                                "'{}' will no longer be used. It stays registered.",
                                row.display_name
                            )
                        });
                    }
                    if saved.platform != self.draft.get(&row.id).and_then(|e| e.platform.clone()) {
                        out.push(match &row.platform_display {
                            Some(platform) => format!(
                                "'{}' will be treated as a {platform} catalogue.",
                                row.display_name
                            ),
                            None => format!(
                                "'{}' will no longer be tied to one platform.",
                                row.display_name
                            ),
                        });
                    }
                    if saved.health
                        != self
                            .draft
                            .get(&row.id)
                            .map(|e| e.health.clone())
                            .unwrap_or_default()
                    {
                        out.push(format!(
                            "'{}' will record the result of the check just run.",
                            row.display_name
                        ));
                    }
                }
            }
        }
        // Removals: named from the saved side, since they are gone from the draft.
        for saved in self.saved.sorted_all() {
            if self.draft.get(&saved.id).is_none() {
                out.push(format!(
                    "'{}' will be removed from the registry. The file at {} is not deleted.",
                    saved.display_name,
                    saved.path.display()
                ));
            }
        }
        if out.is_empty() {
            out.push("The registry will be rewritten with your changes.".to_string());
        }
        out
    }
}

/// Turns a validation report into the Inspect panel's rows.
fn inspect_view(report: &DatValidationReport) -> InspectView {
    InspectView {
        files: report
            .files
            .iter()
            .map(|file| match &file.outcome {
                DatFileOutcome::Parsed {
                    format,
                    ecosystem,
                    name,
                    version,
                    entry_count,
                    rom_count,
                    diagnostics,
                } => {
                    let errors: Vec<String> = diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                        .map(|diagnostic| diagnostic.message.clone())
                        .collect();
                    let warnings: Vec<String> = diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
                        .map(|diagnostic| diagnostic.message.clone())
                        .collect();
                    let notes: Vec<String> = diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Note)
                        .map(|diagnostic| diagnostic.message.clone())
                        .collect();
                    InspectFileView {
                        file_name: file.file_name.clone(),
                        status: if !errors.is_empty() {
                            "Failed"
                        } else if warnings.is_empty() {
                            "OK"
                        } else {
                            "OK, with warnings"
                        },
                        detail: {
                            let mut parts = vec![
                                format.label().to_string(),
                                ecosystem.label().to_string(),
                                format!("{entry_count} entries, {rom_count} ROMs"),
                            ];
                            if let Some(name) = name {
                                parts.insert(
                                    0,
                                    match version {
                                        Some(version) => format!("{name} ({version})"),
                                        None => name.clone(),
                                    },
                                );
                            }
                            parts.join(" · ")
                        },
                        errors,
                        warnings,
                        notes,
                    }
                }
                DatFileOutcome::Failed { error } => InspectFileView {
                    file_name: file.file_name.clone(),
                    status: "Failed",
                    detail: error.clone(),
                    errors: Vec::new(),
                    warnings: Vec::new(),
                    notes: Vec::new(),
                },
            })
            .collect(),
        duplicate_identities: report
            .duplicate_identities
            .iter()
            .map(|duplicate| {
                format!(
                    "'{}' is claimed by {}",
                    duplicate.identity,
                    duplicate.file_names.join(" and ")
                )
            })
            .collect(),
        skipped: report
            .skipped
            .iter()
            .map(|skipped| format!("{}: {}", skipped.file_name, skipped.reason))
            .collect(),
        truncated: report.truncated,
    }
}

fn describe(progress: &DatAuditProgress) -> String {
    match progress {
        DatAuditProgress::ReadingCatalogue { file_name } => {
            format!("Reading catalogue {file_name}…")
        }
        DatAuditProgress::CatalogueReady { entries, roms } => {
            format!("Catalogue ready: {entries} entries, {roms} ROMs")
        }
        DatAuditProgress::Scanning {
            files_found,
            current_dir,
        } => match current_dir {
            // The full directory is never put into the detail line: only a
            // shortened form, so no private path leaks into text that could be
            // logged.
            Some(dir) => format!(
                "Looking for files… {files_found} so far · in {}",
                shorten_path(dir)
            ),
            None => format!("Looking for files… {files_found} so far"),
        },
        DatAuditProgress::Hashing {
            index,
            total,
            file_name,
        } => format!("Checking {index} of {total}: {file_name}"),
        DatAuditProgress::Comparing { files } => {
            format!("Comparing {files} files against the catalogue…")
        }
    }
}

/// Turns a core outcome into rows, without adding or merging any category. The
/// in-memory outcome is the only input; nothing is re-scanned to build this.
fn audit_view(outcome: &DatAuditOutcome, elapsed_seconds: Option<u64>) -> AuditResultView {
    let summary = &outcome.report.summary;
    // Every category the core counts, each with the meaning the core documents
    // for it. Zero counts are kept: "0 ambiguous" is a result, and hiding it
    // would make the reader wonder whether it was checked.
    let categories = vec![
        AuditCategoryView {
            label: "Exact",
            count: summary.exact,
            meaning: "A cryptographic hash (SHA-256, SHA-1 or MD5) matched exactly one catalogue entry.",
        },
        AuditCategoryView {
            label: "Exact (multiple)",
            count: summary.exact_multiple,
            meaning: "A cryptographic hash matched several catalogue entries; all are listed.",
        },
        AuditCategoryView {
            label: "Probable",
            count: summary.probable,
            meaning: "CRC32 (with size, where known) matched one entry. A 32-bit checksum is weaker evidence than a hash.",
        },
        AuditCategoryView {
            label: "Probable (multiple)",
            count: summary.probable_multiple,
            meaning: "CRC32 matched several entries. Deliberately not called exact: a 32-bit collision is as likely as a real duplicate.",
        },
        AuditCategoryView {
            label: "Filename only",
            count: summary.filename_only,
            meaning: "The name is in the catalogue and no hash was available. This says a name matched, not that this file did.",
        },
        AuditCategoryView {
            label: "Ambiguous",
            count: summary.ambiguous,
            meaning: "Candidates exist but the evidence disagrees - for example a CRC32 match whose size does not fit.",
        },
        AuditCategoryView {
            label: "Not in catalogue",
            count: summary.not_in_dat,
            meaning: "Hashes were compared and matched nothing. The file is not in this catalogue.",
        },
        AuditCategoryView {
            label: "No usable evidence",
            count: summary.no_evidence,
            meaning: "No hash could be compared and the name matched nothing.",
        },
    ];

    let content_by_path: std::collections::HashMap<_, _> = outcome
        .content
        .matches
        .iter()
        .map(|matched| (matched.local_path.as_str(), matched))
        .collect();
    let entries: Vec<AuditEntryView> = outcome
        .report
        .entries
        .iter()
        .take(MAX_AUDIT_ENTRIES_SHOWN)
        .map(|entry| {
            let content = content_by_path
                .get(entry.local_path.as_str())
                .map(|matched| {
                    matched
                        .candidates
                        .iter()
                        .map(|candidate| {
                            content_technical_view(
                                &candidate.classification,
                                &candidate.original_metadata,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            AuditEntryView {
                file_name: entry.local_filename.clone(),
                verdict: entry.verdict.label(),
                detail: verdict_detail(&entry.verdict),
                content,
            }
        })
        .collect();
    let entries_truncated = outcome.report.entries.len().saturating_sub(entries.len());

    AuditResultView {
        source_display_name: outcome.source_display_name.clone(),
        source_id: outcome.source_id.clone(),
        dat_path: outcome.dat_path.clone(),
        scan_root: outcome.scan_root.clone(),
        scan_root_short: shorten_path(&outcome.scan_root),
        catalogue_names: outcome.catalogue_names.clone(),
        catalogue_entries: outcome.catalogue_entries,
        headline: outcome.headline(),
        elapsed_seconds,
        categories,
        entries,
        entries_truncated,
        unhashed: outcome
            .unhashed
            .iter()
            .map(|file| (file.file_name.clone(), file.detail.clone()))
            .collect(),
        unreadable_catalogues: outcome.unreadable_catalogues.clone(),
        truncated: outcome.truncated,
        files_scanned: outcome.files_scanned,
        content_selection: outcome.content.selection,
        content_summary: outcome.content.catalogue,
        policy: outcome.policy.as_ref().map(audit_policy_view),
    }
}

fn content_technical_view(
    classification: &DatContentClassification,
    metadata: &archivefs_core::dat::classification::DatOriginalMetadata,
) -> ContentTechnicalView {
    ContentTechnicalView {
        classification: classification.class.label().to_string(),
        confidence: classification.confidence.label().to_string(),
        evidence: classification
            .evidence
            .iter()
            .map(|item| {
                let upstream = item
                    .original_value
                    .as_deref()
                    .map(|value| format!(" — upstream: {value}"))
                    .unwrap_or_default();
                format!("{} ({}){upstream}", item.kind.label(), item.rule)
            })
            .collect(),
        original_metadata: metadata
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        classifier_version: classification.classifier_version.clone(),
    }
}

/// Turns the core policy annotation into rows, without re-ranking anything:
/// the resolution the core produced is rendered as-is.
fn audit_policy_view(
    policy: &archivefs_core::dat::sources::audit_run::DatAuditPolicyOutcome,
) -> AuditPolicyView {
    let mut notes = Vec::new();
    for note in policy.notes.iter() {
        notes.push(AuditPolicyNoteView {
            file_name: std::path::Path::new(&note.local_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| note.local_path.clone()),
            verdict_label: note.verdict_label.clone(),
            ranked: note
                .resolution
                .entries
                .iter()
                .map(|entry| format!("{}. {}", entry.position, entry.candidate.label()))
                .collect(),
            explanations: note.resolution.explanations.clone(),
            decided: note.resolution.decided,
            winner: note
                .resolution
                .winner_index
                .and_then(|index| note.resolution.entries.get(index))
                .map(|entry| entry.candidate.label()),
            ambiguous: note.resolution.ambiguous,
            ambiguity_reason: note.resolution.ambiguity_reason.clone(),
        });
    }
    AuditPolicyView {
        source_ordering: policy.source_ordering.clone(),
        notes,
        notes_truncated: None,
    }
}

fn verdict_detail(verdict: &archivefs_core::dat::audit::AuditVerdict) -> String {
    use archivefs_core::dat::audit::AuditVerdict as V;
    match verdict {
        V::Exact {
            game_name,
            algorithm,
            ..
        } => format!("{game_name} ({algorithm})"),
        V::ExactMultipleCandidates {
            algorithm,
            count,
            game_names,
        }
        | V::ProbableMultipleCandidates {
            algorithm,
            count,
            game_names,
        } => format!(
            "{count} candidates by {algorithm}: {}",
            game_names.join(", ")
        ),
        V::Probable { game_name, .. } | V::FilenameOnly { game_name, .. } => game_name.clone(),
        V::Ambiguous { detail } => detail.clone(),
        V::NotInDat | V::NoUsableEvidence => String::new(),
    }
}

/// A stored Unix timestamp as a date and time, in UTC.
///
/// Hand-rolled rather than pulling in a date library for one label: the build
/// has no date dependency, and the only requirement here is that the value be
/// readable and unambiguous.
fn format_unix_timestamp(seconds: u64) -> String {
    let days_total = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days_total);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60
    )
}

/// Days since 1970-01-01 to a civil date. Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Unsubmitted UI state
// ---------------------------------------------------------------------------

/// Text and disclosures that are not policy.
///
/// Deliberately not part of [`DatSourcesPageState`]: an open picker and a
/// half-chosen audit folder are not preferences, and neither belongs in
/// something whose difference from disk defines the unsaved-change state.
#[derive(Default)]
pub(crate) struct DatSourcesPageUi {
    /// Which source's detail disclosure is open.
    pub(crate) open_inspect: Option<String>,
    /// Which source's platform picker is open.
    pub(crate) open_platform_picker: Option<String>,
    pub(crate) platform_query: String,
    /// Which source's audit target chooser is open.
    pub(crate) open_audit_picker: Option<String>,
    /// Which source is awaiting removal confirmation.
    pub(crate) confirm_remove: Option<String>,
    /// Which diagnostic group's drill-down is open, as the group's stable id.
    /// One group expands at a time; expanding another collapses this one.
    pub(crate) open_diagnostic: Option<String>,
    /// Which rename-plan rows' review choices are open (source path).
    pub(crate) plan_review_open: Option<String>,
    /// Which rename-plan filter is active.
    pub(crate) plan_filter: RenamePlanFilter,
    /// The typed confirmation phrase for a large apply batch.
    pub(crate) plan_typed_confirmation: String,
}

impl DatSourcesPageUi {
    /// Forgets every unsubmitted choice.
    pub(crate) fn clear(&mut self) {
        self.open_inspect = None;
        self.open_platform_picker = None;
        self.platform_query.clear();
        self.open_audit_picker = None;
        self.confirm_remove = None;
        self.open_diagnostic = None;
        self.plan_review_open = None;
        self.plan_typed_confirmation.clear();
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draws the page and returns at most one requested action.
pub(crate) fn show_dat_sources_page(
    ui: &mut egui::Ui,
    view: &DatSourcesPageView,
    ui_state: &mut DatSourcesPageUi,
) -> Option<DatSourcesPageAction> {
    let mut action = None;

    widgets::page_header_with_icon(
        ui,
        crate::ui::icons::VERIFY,
        "Verify Games",
        "Use DAT catalogues to identify and check your games.",
    );

    if let Some(error) = &view.load_error {
        widgets::banner(
            ui,
            "Registry not read",
            &format!(
                "{error}\nShowing an empty list. Saving is disabled so the existing file is not \
                 overwritten."
            ),
            widgets::StatusTone::Blocked,
        );
        ui.add_space(8.0);
    }

    widgets::banner(
        ui,
        "Your files are safe",
        SAFE_PROMISE,
        widgets::StatusTone::Info,
    );
    ui.label(
        egui::RichText::new(READ_ONLY_PROMISE)
            .color(theme::muted(ui))
            .small(),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!("Supported formats: {SUPPORTED_FORMATS}"))
            .color(theme::muted(ui)),
    );
    ui.add_space(10.0);

    if let Some(bar_action) = show_toolbar(ui, view) {
        action = Some(bar_action);
    }
    ui.add_space(10.0);

    if let Some(running) = &view.running
        && let Some(job_action) = show_running_job(ui, running)
    {
        action = Some(job_action);
    }

    if let Some(error) = &view.action_error {
        widgets::banner(
            ui,
            "That could not be done",
            error,
            widgets::StatusTone::Blocked,
        );
        ui.add_space(8.0);
    }

    if view.is_empty() {
        widgets::empty_state(
            ui,
            &crate::ui::icons::with_icon(crate::ui::icons::VERIFY, "No DATs added"),
            "Add a DAT catalogue to verify your games. Nothing is downloaded and nothing is \
             changed.",
            None,
        );
    } else {
        for row in &view.rows {
            if action.is_none()
                && let Some(row_action) = show_source_row(ui, row, view, ui_state)
            {
                action = Some(row_action);
            }
            ui.add_space(8.0);
        }
    }

    // The policy section is always shown: its preferences and the Effective
    // Policy Summary are meaningful even before any source is registered.
    if action.is_none()
        && let Some(policy_action) = show_dat_policy_section(ui, &view.policy)
    {
        action = Some(policy_action);
    }

    if let Some(error) = &view.audit_error {
        ui.add_space(8.0);
        widgets::banner(
            ui,
            "Audit could not run",
            error,
            widgets::StatusTone::Blocked,
        );
    }
    if let Some(enrichment) = &view.identity_enrichment {
        ui.add_space(8.0);
        if enrichment.conflicts > 0 {
            let mut detail = String::new();
            for conflict in enrichment.conflict_details.iter().take(3) {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                let evidence = conflict
                    .evidence
                    .iter()
                    .map(|item| {
                        format!(
                            "{}: {}",
                            item.source.label(),
                            archivefs_core::platform::display_name_for(&item.platform)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                detail.push_str(&format!("{evidence} — Review required"));
            }
            if enrichment.conflict_details.len() > 3 {
                detail.push_str(&format!(
                    "\n…and {} more conflict(s)",
                    enrichment.conflict_details.len() - 3
                ));
            }
            widgets::banner(
                ui,
                "Platform conflict",
                &detail,
                widgets::StatusTone::Warning,
            );
        } else if enrichment.applied > 0 {
            widgets::banner(
                ui,
                "Platform identity enriched",
                &format!(
                    "{} library item(s) received verified DAT platform provenance. No ROM files or links were changed.",
                    enrichment.applied
                ),
                widgets::StatusTone::Success,
            );
        }
    }
    if let Some(audit) = &view.audit {
        ui.add_space(10.0);
        show_audit_result(ui, audit);
    }

    // The read-only rename-planning section, shown when the latest audit
    // produced a plan. It only ever displays and records review decisions.
    if let Some(plan) = &view.rename_plan
        && action.is_none()
        && let Some(plan_action) = show_rename_plan_section(ui, plan, ui_state)
    {
        action = Some(plan_action);
    }

    // The gated apply/recovery section. The user reviews the built transaction
    // and confirms; the core executor performs any rename on a worker thread.
    if action.is_none()
        && let Some(apply_action) = show_rename_apply_section(ui, &view.rename_apply, ui_state)
    {
        action = Some(apply_action);
    }

    if !view.load_problems.is_empty() || !view.unresolved.is_empty() {
        ui.add_space(10.0);
        show_kept_but_not_understood(ui, view);
    }

    action
}

fn show_toolbar(ui: &mut egui::Ui, view: &DatSourcesPageView) -> Option<DatSourcesPageAction> {
    let mut action = None;
    let busy = view.running.is_some();
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            // rfd's pickers are synchronous and return `None` on cancel or
            // failure; they never panic. Held here rather than in the state so
            // the state stays testable without a window.
            if widgets::action_button(ui, "Add DAT file…", widgets::ActionStyle::Primary, !busy)
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a DAT file")
                    .add_filter("DAT catalogues", &["dat", "xml"])
                    .pick_file()
            {
                action = Some(DatSourcesPageAction::AddFile { path });
            }
            if widgets::action_button(
                ui,
                "Add DAT folder…",
                widgets::ActionStyle::Secondary,
                !busy,
            )
            .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a folder of DAT files")
                    .pick_folder()
            {
                action = Some(DatSourcesPageAction::AddFolder { path });
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if view.dirty {
                widgets::status_badge(ui, "Unsaved changes", widgets::StatusTone::Warning);
            } else {
                widgets::status_badge(ui, "No unsaved changes", widgets::StatusTone::Success);
            }
            ui.add_space(8.0);
            let savable = view.dirty && view.load_error.is_none();
            if widgets::action_button(ui, "Save", widgets::ActionStyle::Primary, savable).clicked()
            {
                action = Some(DatSourcesPageAction::Save);
            }
            if widgets::action_button(
                ui,
                "Discard changes",
                widgets::ActionStyle::Secondary,
                view.dirty,
            )
            .clicked()
            {
                action = Some(DatSourcesPageAction::Revert);
            }
        });

        if view.dirty {
            ui.add_space(6.0);
            ui.label("Saving will:");
            for line in &view.pending_consequences {
                ui.label(format!("  • {line}"));
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Nothing is written until you save.").color(theme::muted(ui)),
            );
        }

        match &view.save_state {
            DatSaveState::Idle => {}
            DatSaveState::Saved => {
                ui.add_space(6.0);
                widgets::status_badge(ui, "Registry saved", widgets::StatusTone::Success);
            }
            DatSaveState::Failed(message) => {
                ui.add_space(6.0);
                widgets::banner(ui, "Save failed", message, widgets::StatusTone::Blocked);
            }
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("File: {}", view.config_path.display()))
                .color(theme::muted(ui))
                .small(),
        );
    });
    action
}

fn show_running_job(ui: &mut egui::Ui, running: &RunningJobView) -> Option<DatSourcesPageAction> {
    let mut action = None;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new(running.heading()).strong());
            if running.cancellation_requested {
                // Cancel has been pressed; the button is gone and the wording
                // says so. The job stays busy until the worker confirms.
                widgets::status_badge(ui, "Stopping…", widgets::StatusTone::Warning);
            } else if running.cancellable
                && widgets::action_button(ui, "Cancel", widgets::ActionStyle::Secondary, true)
                    .clicked()
            {
                action = Some(DatSourcesPageAction::CancelJob);
            }
        });
        ui.label(egui::RichText::new(&running.detail).color(theme::muted(ui)));
        if let Some(platform) = &running.platform_display {
            // Shown only when the source's assigned platform is authoritative;
            // never guessed from the path.
            ui.label(
                egui::RichText::new(format!("Platform: {platform}"))
                    .color(theme::muted(ui))
                    .small(),
            );
        }
        if let Some(progress) = &running.progress {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(progress.line()).color(theme::muted(ui)));
            if let Some(path) = &progress.current_path {
                ui.label(
                    egui::RichText::new(format!("In: {path}"))
                        .color(theme::muted(ui))
                        .small(),
                );
            }
            // No ETA while stopping: the run is ending, so a remaining-time is
            // meaningless, and the frozen estimate must not keep being shown
            // next to "Stopping…".
            if !running.cancellation_requested
                && let Some(eta) = progress.eta_line()
            {
                ui.label(egui::RichText::new(eta).color(theme::muted(ui)).small());
            }
        }
    });
    ui.add_space(8.0);
    action
}

fn show_source_row(
    ui: &mut egui::Ui,
    row: &DatSourceRowView,
    view: &DatSourcesPageView,
    ui_state: &mut DatSourcesPageUi,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    let busy_elsewhere = view.running.is_some();

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            let mut enabled = row.enabled;
            if ui.checkbox(&mut enabled, "").changed() {
                action = Some(DatSourcesPageAction::SetEnabled {
                    id: row.id.clone(),
                    enabled,
                });
            }
            ui.label(egui::RichText::new(&row.display_name).strong());
            if !row.enabled {
                widgets::status_badge(ui, "Disabled", widgets::StatusTone::Pending);
            }
            widgets::status_badge(ui, health_label(row), health_tone(row.health_state));
            if row.changed {
                widgets::status_badge(ui, "Changed", widgets::StatusTone::Warning);
            }
        });

        ui.label(
            egui::RichText::new(format!("ID: {}", row.id))
                .color(theme::muted(ui))
                .monospace(),
        );
        ui.label(
            egui::RichText::new(format!("{} · {}", row.kind_label, row.path))
                .color(theme::muted(ui)),
        );

        // Format is only ever what a check observed. An unvalidated source says
        // so rather than guessing from the file extension.
        let format_line = if row.formats.is_empty() {
            "Format: not checked yet".to_string()
        } else {
            format!("Format: {}", row.formats.join(", "))
        };
        ui.label(egui::RichText::new(format_line).color(theme::muted(ui)));

        if let Some(detail) = &row.health_detail {
            ui.label(detail);
        }
        if let Some(when) = &row.last_validated {
            ui.label(
                egui::RichText::new(if row.health_stale {
                    format!("Checked {when} — the file has changed since, so this is out of date.")
                } else {
                    format!("Checked {when}")
                })
                .color(if row.health_stale {
                    widgets::StatusTone::Warning.color(ui)
                } else {
                    theme::muted(ui)
                })
                .small(),
            );
        }

        // An incomplete catalogue load is a distinct, prominent result: the
        // safety limit stopped the check part-way, so the verdict covers only
        // part of the folder and nothing may imply all of it was read.
        if row.incomplete_load {
            ui.add_space(6.0);
            widgets::banner(
                ui,
                "Incomplete catalogue load",
                &row.incomplete_load_line().unwrap_or_default(),
                widgets::StatusTone::Warning,
            );
        }

        show_diagnostics_summary(ui, row, ui_state);

        ui.add_space(6.0);
        if action.is_none()
            && let Some(platform_action) = show_platform_control(ui, row, ui_state)
        {
            action = Some(platform_action);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::action_button(
                ui,
                "Validate",
                widgets::ActionStyle::Secondary,
                !busy_elsewhere,
            )
            .clicked()
                && action.is_none()
            {
                action = Some(DatSourcesPageAction::Validate { id: row.id.clone() });
            }
            let inspecting = ui_state.open_inspect.as_deref() == Some(row.id.as_str());
            if widgets::action_button(
                ui,
                if inspecting {
                    "Hide details"
                } else {
                    "Inspect"
                },
                widgets::ActionStyle::Quiet,
                true,
            )
            .clicked()
            {
                ui_state.open_inspect = if inspecting {
                    None
                } else {
                    Some(row.id.clone())
                };
            }
            let auditing = ui_state.open_audit_picker.as_deref() == Some(row.id.as_str());
            if widgets::action_button(
                ui,
                if auditing {
                    "Cancel audit setup"
                } else {
                    "Audit…"
                },
                widgets::ActionStyle::Secondary,
                !busy_elsewhere,
            )
            .clicked()
            {
                ui_state.open_audit_picker = if auditing { None } else { Some(row.id.clone()) };
            }
            if widgets::action_button(ui, "Remove", widgets::ActionStyle::Quiet, !busy_elsewhere)
                .clicked()
            {
                ui_state.confirm_remove = Some(row.id.clone());
            }
        });

        if ui_state.confirm_remove.as_deref() == Some(row.id.as_str()) {
            ui.add_space(6.0);
            widgets::banner(
                ui,
                "Remove this source?",
                &format!(
                    "'{}' will no longer be registered. The file at {} is not deleted, and no ROM \
                     is touched.",
                    row.display_name, row.path
                ),
                widgets::StatusTone::Warning,
            );
            ui.horizontal(|ui| {
                if widgets::action_button(
                    ui,
                    "Remove from registry",
                    widgets::ActionStyle::Primary,
                    true,
                )
                .clicked()
                    && action.is_none()
                {
                    action = Some(DatSourcesPageAction::Remove { id: row.id.clone() });
                }
                if widgets::action_button(ui, "Keep it", widgets::ActionStyle::Secondary, true)
                    .clicked()
                {
                    ui_state.confirm_remove = None;
                }
            });
        }

        if ui_state.open_audit_picker.as_deref() == Some(row.id.as_str())
            && action.is_none()
            && let Some(audit_action) = show_audit_target_picker(ui, row, view)
        {
            action = Some(audit_action);
        }

        if ui_state.open_inspect.as_deref() == Some(row.id.as_str()) {
            ui.add_space(6.0);
            show_inspect(ui, row);
        }
    });

    // Once the removal or the audit has been asked for, the disclosure has done
    // its job and stays open on a row that may no longer exist.
    if let Some(requested) = &action {
        match requested {
            DatSourcesPageAction::Remove { .. } => ui_state.confirm_remove = None,
            DatSourcesPageAction::Audit { .. } => ui_state.open_audit_picker = None,
            _ => {}
        }
    }
    action
}

fn health_label(row: &DatSourceRowView) -> String {
    if row.health_stale {
        format!("{} (out of date)", row.health_state.label())
    } else {
        row.health_state.label().to_string()
    }
}

/// Warnings and parser notes, each with a count, a concise inline summary, and
/// an expandable list, drawn directly on the source card so the health badge is
/// never the only thing the user can see.
///
/// Warnings are worth investigating; parser notes are expected behaviour and
/// are never presented as if something is wrong.
/// The pluralised, type-oriented label for a severity, as used in the section
/// badge: "error type", "warning type", "parser-note type".
fn severity_type_label(severity: DiagnosticSeverity, types: usize) -> String {
    let base = match severity {
        DiagnosticSeverity::Error => "error type",
        DiagnosticSeverity::Warning => "warning type",
        DiagnosticSeverity::Note => "parser-note type",
    };
    if types == 1 {
        base.to_string()
    } else {
        format!("{base}s")
    }
}

/// "line 3:12" / "line 3" / "Location unavailable", from what the parser
/// actually recorded. Never invents a location the parser did not provide.
fn format_location(line: Option<usize>, column: Option<usize>) -> String {
    match (line, column) {
        (Some(line), Some(column)) => format!("line {line}:{column}"),
        (Some(line), None) => format!("line {line}"),
        _ => "Location unavailable".to_string(),
    }
}

/// One group's drill-down: the verbatim message, the counts, and a bounded
/// occurrence list with safe filenames and parser-provided locations.
fn show_diagnostic_group(
    ui: &mut egui::Ui,
    group: &DiagnosticGroupView,
    ui_state: &mut DatSourcesPageUi,
) {
    let open = ui_state.open_diagnostic.as_deref() == Some(group.id.as_str());
    ui.horizontal_top(|ui| {
        ui.label(egui::RichText::new("•").color(theme::muted(ui)));
        ui.vertical(|ui| {
            // The original diagnostic message, preserved verbatim.
            ui.add(egui::Label::new(&group.message).wrap());
            ui.label(
                egui::RichText::new(format!(
                    "{} occurrence{} · affects {} DAT file{}",
                    group.occurrence_count,
                    if group.occurrence_count == 1 { "" } else { "s" },
                    group.affected_file_count,
                    if group.affected_file_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                ))
                .color(theme::muted(ui))
                .small(),
            );
            let toggle = if open {
                "Hide locations"
            } else {
                "View locations"
            };
            if widgets::action_button(ui, toggle, widgets::ActionStyle::Quiet, true).clicked() {
                ui_state.open_diagnostic = if open { None } else { Some(group.id.clone()) };
            }
            if open {
                for occurrence in &group.occurrences {
                    ui.horizontal_top(|ui| {
                        ui.label(
                            egui::RichText::new(format_location(
                                occurrence.line,
                                occurrence.column,
                            ))
                            .color(theme::muted(ui))
                            .small()
                            .monospace(),
                        );
                        ui.label(egui::RichText::new(&occurrence.file_name).small());
                    });
                }
                if group.occurrences_truncated {
                    ui.label(
                        egui::RichText::new(format!(
                            "… and {} more occurrence(s)",
                            group.occurrence_count - group.occurrences.len()
                        ))
                        .color(theme::muted(ui))
                        .small(),
                    );
                }
            }
        });
    });
}

/// Diagnostics grouped by type, drawn directly on the source card so the health
/// badge is never the only thing the user can see.
///
/// Each severity (Errors, Warnings, Parser notes) is one section showing how
/// many distinct types and total occurrences there are, followed by one row per
/// diagnostic type. Expanding a row reveals its locations. One row expands at a
/// time; parser notes carry the reassurance that no action is needed.
fn show_diagnostics_summary(
    ui: &mut egui::Ui,
    row: &DatSourceRowView,
    ui_state: &mut DatSourcesPageUi,
) {
    if row.groups.is_empty() {
        return;
    }

    // Occurrence lists are deliberately bounded for rendering, so never use
    // their length for the headline. The stored occurrence totals remain
    // exact even for very large folders.
    let error_issues = row.diagnostic_occurrences(DiagnosticSeverity::Error);
    let attention_issues = error_issues + row.diagnostic_occurrences(DiagnosticSeverity::Warning);
    if attention_issues > 0 {
        // An Error means core marked the source Invalid ("part of what they
        // asked for is unusable" - see `dat/sources/validation.rs`), which is
        // not the same claim as "still works, some files were skipped". Say
        // so truthfully instead of reassuring the user past a real problem.
        let has_errors = error_issues > 0;
        ui.add_space(6.0);
        widgets::status_badge(
            ui,
            format!(
                "{attention_issues} catalogue issue{} found",
                if attention_issues == 1 { "" } else { "s" }
            ),
            if has_errors {
                widgets::StatusTone::Blocked
            } else {
                widgets::StatusTone::Warning
            },
        );
        ui.label(
            egui::RichText::new(if has_errors {
                "Some files could not be used and need your attention."
            } else {
                "The catalogue still works. Files that could not be used were skipped."
            })
            .color(theme::muted(ui))
            .small(),
        );
    }

    egui::CollapsingHeader::new("What happened?")
        .id_salt(("dat-diagnostics-summary", row.id.as_str()))
        .default_open(false)
        .show(ui, |ui| {
            let notes = row.diagnostic_occurrences(DiagnosticSeverity::Note);
            if notes > 0 {
                ui.label(format!(
                    "{notes} additional catalogue note{} recorded. No action is needed for these.",
                    if notes == 1 { "" } else { "s" }
                ));
            }
            widgets::technical_details(
                ui,
                ("dat-parser-technical-details", row.id.as_str()),
                |ui| {
                    for severity in [
                        DiagnosticSeverity::Error,
                        DiagnosticSeverity::Warning,
                        DiagnosticSeverity::Note,
                    ] {
                        let groups = row.groups_of(severity);
                        if groups.is_empty() {
                            continue;
                        }
                        let types = row.diagnostic_types(severity);
                        let occurrences = row.diagnostic_occurrences(severity);
                        let tone = match severity {
                            DiagnosticSeverity::Error => widgets::StatusTone::Blocked,
                            DiagnosticSeverity::Warning => widgets::StatusTone::Warning,
                            DiagnosticSeverity::Note => widgets::StatusTone::Info,
                        };
                        widgets::status_badge(
                            ui,
                            format!(
                                "{types} {}, {occurrences} occurrence{}",
                                severity_type_label(severity, types),
                                if occurrences == 1 { "" } else { "s" }
                            ),
                            tone,
                        );
                        for group in groups {
                            ui.add_space(2.0);
                            show_diagnostic_group(ui, group, ui_state);
                        }
                    }
                },
            );
        });

    if row.history_link_available {
        // Only ever drawn when the full details really are recorded in
        // History & Logs; today they are kept inline, so this is off.
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Full details are recorded in History & Logs.")
                .color(theme::muted(ui))
                .small(),
        );
    }
}

fn health_tone(state: DatHealthState) -> widgets::StatusTone {
    match state {
        // Never rendered as healthy or failed: it is neither.
        DatHealthState::NotChecked => widgets::StatusTone::Pending,
        DatHealthState::Valid => widgets::StatusTone::Success,
        DatHealthState::ValidWithWarnings => widgets::StatusTone::Warning,
        DatHealthState::Invalid | DatHealthState::Unreadable => widgets::StatusTone::Blocked,
    }
}

/// The platform assignment, using the same canonical registry the rest of the
/// GUI picks from, so an assignment can only ever name a platform the resolver
/// will actually match.
fn show_platform_control(
    ui: &mut egui::Ui,
    row: &DatSourceRowView,
    ui_state: &mut DatSourcesPageUi,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    let is_open = ui_state.open_platform_picker.as_deref() == Some(row.id.as_str());

    ui.horizontal(|ui| {
        ui.label("Platform:");
        match &row.platform_display {
            Some(platform) => {
                ui.label(egui::RichText::new(platform).strong());
                if row.platform_unresolved {
                    ui.label(
                        egui::RichText::new("(not recognised by this build; kept as written)")
                            .color(widgets::StatusTone::Warning.color(ui))
                            .small(),
                    );
                }
            }
            None => {
                ui.label(
                    egui::RichText::new("any (the catalogue's own header decides)")
                        .color(theme::muted(ui)),
                );
            }
        }
        if widgets::action_button(
            ui,
            if is_open { "Cancel" } else { "Change…" },
            widgets::ActionStyle::Quiet,
            true,
        )
        .clicked()
        {
            ui_state.open_platform_picker = if is_open { None } else { Some(row.id.clone()) };
            ui_state.platform_query.clear();
        }
        if row.platform_display.is_some()
            && widgets::action_button(ui, "Clear", widgets::ActionStyle::Quiet, true).clicked()
        {
            action = Some(DatSourcesPageAction::SetPlatform {
                id: row.id.clone(),
                platform: None,
            });
        }
    });

    if !is_open || action.is_some() {
        return action;
    }

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Find a platform:");
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.platform_query)
                    .hint_text("e.g. PlayStation 2")
                    .desired_width(220.0),
            );
        });
        let choices = platform_choices(&ui_state.platform_query);
        let total = platform_choice_count(&ui_state.platform_query);
        if choices.is_empty() {
            ui.label(egui::RichText::new("No platform matches.").color(theme::muted(ui)));
            return;
        }
        for (id, display_name) in &choices {
            if widgets::action_button(ui, *display_name, widgets::ActionStyle::Secondary, true)
                .clicked()
                && action.is_none()
            {
                action = Some(DatSourcesPageAction::SetPlatform {
                    id: row.id.clone(),
                    platform: Some((*id).to_string()),
                });
            }
        }
        if total > choices.len() {
            ui.label(
                egui::RichText::new(format!(
                    "Showing {} of {total} matches. Type to narrow the search.",
                    choices.len()
                ))
                .color(theme::muted(ui))
                .small(),
            );
        }
    });

    if action.is_some() {
        ui_state.open_platform_picker = None;
        ui_state.platform_query.clear();
    }
    action
}

/// How many platform choices the picker shows at once.
pub(crate) const MAX_PLATFORM_CHOICES: usize = 12;

/// Canonical platforms matching `query`, drawn strictly from the same registry
/// `canonical_platform_for_alias` resolves against.
pub(crate) fn platform_choices(query: &str) -> Vec<(&'static str, &'static str)> {
    let needle = query.trim().to_lowercase();
    archivefs_core::platform::canonical_ids()
        .into_iter()
        .map(|id| (id, archivefs_core::platform::display_name_for(id)))
        .filter(|(id, display_name)| {
            needle.is_empty()
                || display_name.to_lowercase().contains(&needle)
                || id.to_lowercase().contains(&needle)
        })
        .take(MAX_PLATFORM_CHOICES)
        .collect()
}

/// How many canonical platforms match `query`, so the picker can say "showing
/// 12 of 30" honestly rather than implying the 12 are all there is.
pub(crate) fn platform_choice_count(query: &str) -> usize {
    let needle = query.trim().to_lowercase();
    archivefs_core::platform::canonical_ids()
        .into_iter()
        .filter(|id| {
            needle.is_empty()
                || archivefs_core::platform::display_name_for(id)
                    .to_lowercase()
                    .contains(&needle)
                || id.to_lowercase().contains(&needle)
        })
        .count()
}

/// A friendly name for a library folder button: its own name first, with the
/// full path shown as muted secondary text. Deep configured paths read as
/// giant buttons otherwise; a beginner recognises "GameCube" faster than
/// "/media/archives/library/GameCube".
fn friendly_folder_label(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| folder.display().to_string())
}

fn show_audit_target_picker(
    ui: &mut egui::Ui,
    row: &DatSourceRowView,
    view: &DatSourcesPageView,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    ui.add_space(6.0);
    widgets::card(ui, |ui| {
        widgets::section_header(
            ui,
            "Check which files?",
            Some(
                "Every file in the chosen folder is read and compared against this catalogue. \
                 Nothing is renamed, moved, or written.",
            ),
        );
        if view.library_folders.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No library source folders are configured, so there is nothing to offer here. \
                     Choose a folder instead.",
                )
                .color(theme::muted(ui)),
            );
        }
        for folder in &view.library_folders {
            // A two-line button: the friendly name first, the full path muted
            // underneath. Clicking anywhere on it starts the audit.
            let friendly = friendly_folder_label(folder);
            let full = folder.display().to_string();
            let clicked = egui::Frame::new()
                .fill(theme::card_fill(ui))
                .stroke(theme::border(ui))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Library folder: {friendly}")).strong(),
                        );
                        ui.label(egui::RichText::new(full).color(theme::muted(ui)).small());
                    });
                })
                .response
                .interact(egui::Sense::click());
            if clicked.clicked() && action.is_none() {
                action = Some(DatSourcesPageAction::Audit {
                    id: row.id.clone(),
                    scan_root: folder.clone(),
                });
            }
        }
        if widgets::action_button(
            ui,
            "Choose another folder…",
            widgets::ActionStyle::Quiet,
            true,
        )
        .clicked()
            && action.is_none()
            && let Some(path) = rfd::FileDialog::new()
                .set_title("Choose a folder to check")
                .pick_folder()
        {
            action = Some(DatSourcesPageAction::Audit {
                id: row.id.clone(),
                scan_root: path,
            });
        }
    });
    action
}

fn show_inspect(ui: &mut egui::Ui, row: &DatSourceRowView) {
    widgets::card(ui, |ui| {
        widgets::section_header(ui, "Source details", None);
        let mut rows: Vec<(&str, String)> = vec![
            ("ID", row.id.clone()),
            ("Kind", row.kind_label.to_string()),
            ("Path", row.path.clone()),
            (
                "Enabled",
                if row.enabled { "yes" } else { "no" }.to_string(),
            ),
            ("Health", row.health_state.label().to_string()),
        ];
        if let Some(platform) = &row.platform_display {
            rows.push(("Platform", platform.clone()));
        }
        if !row.formats.is_empty() {
            rows.push(("Formats", row.formats.join(", ")));
        }
        if let Some(count) = row.entry_count {
            rows.push(("Catalogue entries", count.to_string()));
        }
        if let Some(count) = row.rom_count {
            rows.push(("Catalogue ROMs", count.to_string()));
        }
        if let Some(when) = &row.last_validated {
            rows.push(("Last checked", when.clone()));
        }
        for (label, value) in rows {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{label}:")).color(theme::muted(ui)));
                ui.label(value);
            });
        }
        if !row.health_state.is_checked() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "This source has not been checked yet, so nothing above describes its \
                     contents. Use Validate to read it.",
                )
                .color(theme::muted(ui))
                .small(),
            );
        }

        let Some(detail) = &row.detail else {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Run Validate to see the individual DAT files, their formats, and anything \
                     the parser had to say.",
                )
                .color(theme::muted(ui))
                .small(),
            );
            return;
        };

        ui.add_space(8.0);
        widgets::section_header(ui, "DAT files read", None);
        for file in &detail.files {
            ui.horizontal_top(|ui| {
                widgets::status_badge(
                    ui,
                    file.status,
                    if file.status == "Failed" {
                        widgets::StatusTone::Blocked
                    } else if file.status == "OK" {
                        widgets::StatusTone::Success
                    } else {
                        widgets::StatusTone::Warning
                    },
                );
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&file.file_name).strong());
                    ui.label(
                        egui::RichText::new(&file.detail)
                            .color(theme::muted(ui))
                            .small(),
                    );
                    for error in file.errors.iter().take(20) {
                        ui.label(
                            egui::RichText::new(format!("error: {error}"))
                                .color(widgets::StatusTone::Blocked.color(ui))
                                .small(),
                        );
                    }
                    for warning in file.warnings.iter().take(20) {
                        ui.label(
                            egui::RichText::new(format!("warning: {warning}"))
                                .color(widgets::StatusTone::Warning.color(ui))
                                .small(),
                        );
                    }
                    for note in file.notes.iter().take(20) {
                        ui.label(
                            egui::RichText::new(format!("note: {note}"))
                                .color(theme::muted(ui))
                                .small(),
                        );
                    }
                });
            });
        }

        if !detail.duplicate_identities.is_empty() {
            ui.add_space(6.0);
            widgets::section_header(
                ui,
                "Conflicting catalogue identities",
                Some(
                    "More than one file claims to be the same catalogue. Both are still read; \
                     EmuWiz does not pick one for you.",
                ),
            );
            for line in &detail.duplicate_identities {
                ui.label(line);
            }
        }

        if !detail.skipped.is_empty() {
            ui.add_space(6.0);
            widgets::section_header(
                ui,
                "Files not used",
                Some("Looked at and left alone, so nothing is missed silently."),
            );
            for line in detail.skipped.iter().take(50) {
                ui.label(egui::RichText::new(line).small());
            }
        }

        if detail.truncated {
            ui.add_space(6.0);
            widgets::banner(
                ui,
                "Partial listing",
                "This folder holds more DAT files than one check reads. Split it, or register the \
                 subfolders separately, for a complete picture.",
                widgets::StatusTone::Warning,
            );
        }
    });
}

/// The policy annotation of an audit: which candidates the user's preferences
/// prefer for each multi-candidate file, and what the policy could not decide.
///
/// This is display of the core's resolution - nothing here ranks, and the
/// verdicts themselves are never touched.
fn show_audit_policy_notes(ui: &mut egui::Ui, policy: &AuditPolicyView) {
    if policy.notes.is_empty() {
        return;
    }
    widgets::section_header(
        ui,
        "Policy preference",
        Some(
            "Your DAT matching preferences, applied to the files whose hash matched several \
             catalogue entries. Nothing is renamed or changed; this is the preferred order only.",
        ),
    );
    ui.label(
        egui::RichText::new(format!(
            "Sources consulted: {}",
            if policy.source_ordering.is_empty() {
                "none".to_string()
            } else {
                policy.source_ordering.join(" → ")
            }
        ))
        .color(theme::muted(ui))
        .small(),
    );
    for note in &policy.notes {
        ui.add_space(4.0);
        ui.horizontal_top(|ui| {
            ui.label(egui::RichText::new(note.file_name.clone()).strong());
            ui.label(
                egui::RichText::new(note.verdict_label.clone())
                    .color(theme::muted(ui))
                    .small(),
            );
        });
        for line in &note.ranked {
            ui.label(egui::RichText::new(line.clone()).small().monospace());
        }
        for line in &note.explanations {
            ui.label(
                egui::RichText::new(format!("• {line}"))
                    .color(theme::muted(ui))
                    .small(),
            );
        }
        if let Some(winner) = &note.winner {
            ui.label(
                egui::RichText::new(format!("Preferred: {winner}"))
                    .color(theme::SUCCESS)
                    .small(),
            );
        }
        if note.ambiguous {
            ui.label(
                egui::RichText::new(format!(
                    "Ambiguous: {}",
                    note.ambiguity_reason
                        .as_deref()
                        .unwrap_or("the policy cannot decide")
                ))
                .color(theme::WARNING)
                .small(),
            );
        }
    }
}

/// The DAT Matching Policy section: preference editors plus the Effective
/// Policy Summary. Every value drawn here comes from the core resolver; the
/// drawing code only turns actions into edits.
fn show_dat_policy_section(
    ui: &mut egui::Ui,
    view: &DatPolicyView,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    ui.add_space(10.0);
    widgets::section_header(
        ui,
        "DAT matching policy",
        Some(
            "How EmuWiz prefers one verified candidate over another. Nothing here renames, \
             moves, deletes or rewrites a file.",
        ),
    );

    widgets::card(ui, |ui| {
        if !view.editable {
            ui.label(
                egui::RichText::new(
                    "The registry could not be read, so the policy cannot be edited until that \
                     is fixed.",
                )
                .color(theme::muted(ui)),
            );
            return;
        }

        // Scope selector: which platform the preferences and summary apply to.
        ui.horizontal(|ui| {
            ui.label("Applies to:");
            egui::ComboBox::from_id_salt("dat-policy-scope")
                .selected_text(&view.scope_label)
                .show_ui(ui, |ui| {
                    for option in &view.scopes_available {
                        if ui
                            .selectable_label(option.id == view.scope, &option.label)
                            .clicked()
                        {
                            action = Some(DatSourcesPageAction::SelectPolicyScope {
                                scope: option.id.clone(),
                            });
                        }
                    }
                });
        });
        ui.label(
            egui::RichText::new(match view.scope {
                None => "Editing: Global defaults".to_string(),
                Some(_) => format!("Editing: {} settings", view.scope_label),
            })
            .color(theme::muted(ui))
            .small(),
        );
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Show:");
            egui::ComboBox::from_id_salt("dat-policy-content")
                .selected_text(view.content_selection.label())
                .show_ui(ui, |ui| {
                    for policy in ContentSelectionPolicy::ALL {
                        if ui
                            .selectable_label(view.content_selection == policy, policy.label())
                            .clicked()
                        {
                            action = Some(DatSourcesPageAction::SetContentSelection {
                                scope: view.scope.clone(),
                                policy,
                            });
                        }
                    }
                });
        });
        ui.label(
            egui::RichText::new(GAMES_ONLY_EXPLANATION)
                .color(theme::muted(ui))
                .small(),
        );
        ui.add_space(10.0);

        if let Some(policy_action) = show_region_preference_editor(ui, view) {
            action = Some(policy_action);
        }
        ui.add_space(10.0);
        if let Some(policy_action) = show_language_preference_editor(ui, view) {
            action = Some(policy_action);
        }
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Revision policy:");
            egui::ComboBox::from_id_salt("dat-policy-revision")
                .selected_text(view.revision_policy.label())
                .show_ui(ui, |ui| {
                    for policy in RevisionPolicy::ALL {
                        if ui
                            .selectable_label(view.revision_policy == policy, policy.label())
                            .clicked()
                        {
                            action = Some(DatSourcesPageAction::SetRevisionPolicy {
                                scope: view.scope.clone(),
                                policy,
                            });
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Parent/clone handling:");
            egui::ComboBox::from_id_salt("dat-policy-clone")
                .selected_text(view.clone_policy.label())
                .show_ui(ui, |ui| {
                    for policy in ClonePolicy::ALL {
                        if ui
                            .selectable_label(view.clone_policy == policy, policy.label())
                            .clicked()
                        {
                            action = Some(DatSourcesPageAction::SetClonePolicy {
                                scope: view.scope.clone(),
                                policy,
                            });
                        }
                    }
                });
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(SAFE_PROMISE)
                .color(theme::muted(ui))
                .small(),
        );

        if !view.problems.is_empty() {
            ui.add_space(8.0);
            widgets::banner(
                ui,
                "Policy file kept as written",
                &format!(
                    "{} These values are preserved but not applied until fixed by hand or by a \
                     newer build.",
                    view.problems.join(" ")
                ),
                widgets::StatusTone::Warning,
            );
        }
    });

    ui.add_space(10.0);
    show_effective_policy_summary(ui, view);

    action
}

/// One ordered preference list with move/remove, plus an "Add" affordance.
fn show_preference_rows(
    ui: &mut egui::Ui,
    rows: &[PolicyPreferenceRowView],
    move_action: impl Fn(usize, i32) -> DatSourcesPageAction,
    remove_action: impl Fn(usize) -> DatSourcesPageAction,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    for row in rows {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}.", row.position)).monospace());
            ui.label(&row.label);
            if row.position > 1
                && ui
                    .add_enabled(true, egui::Button::new("↑").small())
                    .on_hover_text("Move up")
                    .clicked()
            {
                action = Some(move_action(row.position - 1, -1));
            }
            if row.position < rows.len()
                && ui
                    .add_enabled(true, egui::Button::new("↓").small())
                    .on_hover_text("Move down")
                    .clicked()
            {
                action = Some(move_action(row.position - 1, 1));
            }
            if ui
                .add_enabled(true, egui::Button::new("Remove").small())
                .on_hover_text("Remove from the preference list")
                .clicked()
            {
                action = Some(remove_action(row.position - 1));
            }
        });
    }
    action
}

fn show_region_preference_editor(
    ui: &mut egui::Ui,
    view: &DatPolicyView,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preferred regions").strong());
        if view.region_preferences.is_empty() {
            ui.label(
                egui::RichText::new("Any region — no preference")
                    .color(theme::muted(ui))
                    .small(),
            );
        }
    });
    ui.label(
        egui::RichText::new(
            "When several regions of a game are verified, prefer them in this order.",
        )
        .color(theme::muted(ui))
        .small(),
    );
    if let Some(policy_action) = show_preference_rows(
        ui,
        &view.region_preferences,
        |index, delta| DatSourcesPageAction::MoveRegion {
            scope: view.scope.clone(),
            index,
            delta,
        },
        |index| DatSourcesPageAction::RemoveRegion {
            scope: view.scope.clone(),
            index,
        },
    ) {
        action = Some(policy_action);
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Add:").color(theme::muted(ui)));
        for region in RegionId::ALL {
            let present = view
                .region_preferences
                .iter()
                .any(|row| row.value == region.as_str());
            if !present && ui.add(egui::Button::new(region.label()).small()).clicked() {
                action = Some(DatSourcesPageAction::AddRegion {
                    scope: view.scope.clone(),
                    region,
                });
            }
        }
        // "Any region" clears the ordering back to no preference.
        if !view.region_preferences.is_empty()
            && ui
                .add(egui::Button::new("Any region").small())
                .on_hover_text("Clear the region preference so every region is treated as equal.")
                .clicked()
        {
            action = Some(DatSourcesPageAction::ClearRegion {
                scope: view.scope.clone(),
            });
        }
    });
    action
}

fn show_language_preference_editor(
    ui: &mut egui::Ui,
    view: &DatPolicyView,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preferred languages").strong());
        if view.language_preferences.is_empty() {
            ui.label(
                egui::RichText::new("Any language — no preference")
                    .color(theme::muted(ui))
                    .small(),
            );
        }
    });
    ui.label(
        egui::RichText::new(
            "When several languages of a game are verified, prefer them in this order. \
             Multi-language matches any entry with more than one language tag; Original matches \
             the release region's own language.",
        )
        .color(theme::muted(ui))
        .small(),
    );
    if let Some(policy_action) = show_preference_rows(
        ui,
        &view.language_preferences,
        |index, delta| DatSourcesPageAction::MoveLanguage {
            scope: view.scope.clone(),
            index,
            delta,
        },
        |index| DatSourcesPageAction::RemoveLanguage {
            scope: view.scope.clone(),
            index,
        },
    ) {
        action = Some(policy_action);
    }
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Add:").color(theme::muted(ui)));
        let present: Vec<&str> = view
            .language_preferences
            .iter()
            .map(|row| row.value.as_str())
            .collect();
        egui::ComboBox::from_id_salt("dat-policy-add-language")
            .selected_text("Choose a language…")
            .width(220.0)
            .show_ui(ui, |ui| {
                if !present.contains(&"multi")
                    && ui
                        .selectable_label(false, LanguagePreference::MultiLanguage.label())
                        .clicked()
                {
                    action = Some(DatSourcesPageAction::AddLanguage {
                        scope: view.scope.clone(),
                        preference: LanguagePreference::MultiLanguage,
                    });
                }
                if !present.contains(&"original")
                    && ui
                        .selectable_label(false, LanguagePreference::OriginalLanguage.label())
                        .clicked()
                {
                    action = Some(DatSourcesPageAction::AddLanguage {
                        scope: view.scope.clone(),
                        preference: LanguagePreference::OriginalLanguage,
                    });
                }
                for language in LanguageId::ALL {
                    let present = present.contains(&language.as_str());
                    if !present && ui.selectable_label(false, language.label()).clicked() {
                        action = Some(DatSourcesPageAction::AddLanguage {
                            scope: view.scope.clone(),
                            preference: LanguagePreference::Language(language),
                        });
                    }
                }
            });
        // "Any language" clears the ordering back to no preference.
        if !view.language_preferences.is_empty()
            && ui
                .add(egui::Button::new("Any language").small())
                .on_hover_text(
                    "Clear the language preference so every language is treated as equal.",
                )
                .clicked()
        {
            action = Some(DatSourcesPageAction::ClearLanguage {
                scope: view.scope.clone(),
            });
        }
    });
    action
}

/// The Effective Policy Summary: the resolved policy for the current scope,
/// where each value came from, and the source consultation order.
fn show_effective_policy_summary(ui: &mut egui::Ui, view: &DatPolicyView) {
    widgets::section_header(
        ui,
        "Effective policy",
        Some(
            "What will actually be applied, after any platform overrides. Resolved by the same \
             core the CLI uses.",
        ),
    );
    widgets::card(ui, |ui| {
        ui.label(egui::RichText::new(format!("Platform: {}", view.effective.platform)).strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sources consulted:").strong());
            if view.effective.source_ordering.is_empty() {
                ui.label(
                    egui::RichText::new("none enabled for this scope")
                        .color(theme::muted(ui))
                        .small(),
                );
            } else {
                ui.vertical(|ui| {
                    for source in &view.effective.source_ordering {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}. {} (priority {})",
                                source.consulted_position, source.display_name, source.priority
                            ))
                            .small(),
                        );
                    }
                });
            }
        });
        ui.add_space(4.0);
        summary_row(ui, "Region preference", &view.effective.region);
        summary_row(ui, "Language preference", &view.effective.language);
        summary_row(ui, "Revision rule", &view.effective.revision);
        summary_row(ui, "Clone rule", &view.effective.clone);
        summary_row(ui, "Show", &view.effective.content);
        ui.add_space(6.0);
        ui.separator();
        ui.label(egui::RichText::new("Where each value comes from").strong());
        for (field, scope) in &view.effective.source_of {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(field).small());
                ui.label(egui::RichText::new(scope).color(theme::muted(ui)).small());
            });
        }
    });
}

fn summary_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{label}:")).strong());
        ui.label(value);
    });
}

/// The tone of a proposal-state badge.
fn plan_state_tone(state: ProposalState) -> widgets::StatusTone {
    match state {
        ProposalState::Suggested => widgets::StatusTone::Success,
        ProposalState::AlreadyCanonical => widgets::StatusTone::Info,
        ProposalState::Ambiguous => widgets::StatusTone::Warning,
        ProposalState::Conflict => widgets::StatusTone::Warning,
        ProposalState::Unsupported => widgets::StatusTone::Info,
        ProposalState::Blocked | ProposalState::UnclassifiedContent => widgets::StatusTone::Blocked,
        ProposalState::ExcludedByContentPolicy => widgets::StatusTone::Info,
    }
}

/// The gated apply and crash-recovery section.
///
/// This is the only place the page offers a rename: an explicitly approved,
/// actionable batch is built by the core, shown read-only, confirmed (with a
/// typed phrase for large batches), and executed by the core executor on a
/// worker thread. The GUI never calls `std::fs::rename`.
fn show_rename_apply_section(
    ui: &mut egui::Ui,
    apply: &RenameApplyView,
    ui_state: &mut DatSourcesPageUi,
) -> Option<DatSourcesPageAction> {
    let mut action = None;

    // Transactions found on disk first: a settled `Applied` batch is offered
    // for rollback, and an interrupted batch is surfaced for crash recovery -
    // never auto-resumed.
    if !apply.recovery.is_empty() {
        ui.add_space(10.0);
        widgets::section_header(ui, "Rename transactions", None);
        widgets::card(ui, |ui| {
            for recovery in &apply.recovery {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {} applied of {}",
                            recovery.transaction_id, recovery.applied_count, recovery.total_count
                        ))
                        .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("({})", recovery.state.label()))
                            .color(theme::muted(ui)),
                    );
                });
                let (explanation, rollback_label) = if recovery.state == TransactionState::Applied {
                    (
                        "A completed rename transaction is still applied and can be rolled \
                             back.",
                        "Roll back transaction",
                    )
                } else {
                    (
                        "An interrupted rename transaction was found. EmuWiz will never \
                             resume it automatically.",
                        "Roll back completed steps",
                    )
                };
                ui.label(
                    egui::RichText::new(explanation)
                        .color(theme::muted(ui))
                        .small(),
                );
                ui.horizontal(|ui| {
                    if widgets::action_button(
                        ui,
                        rollback_label,
                        widgets::ActionStyle::Destructive,
                        !apply.rollback_running,
                    )
                    .clicked()
                    {
                        action = Some(DatSourcesPageAction::RecoveryChoice {
                            id: recovery.transaction_id.clone(),
                            choice: RecoveryChoice::RollBack,
                        });
                    }
                    if widgets::action_button(
                        ui,
                        "Leave untouched",
                        widgets::ActionStyle::Quiet,
                        !apply.rollback_running,
                    )
                    .clicked()
                    {
                        action = Some(DatSourcesPageAction::RecoveryChoice {
                            id: recovery.transaction_id.clone(),
                            choice: RecoveryChoice::LeaveUntouched,
                        });
                    }
                });
                ui.add_space(6.0);
            }
        });
    }

    if let Some(review) = &apply.review {
        ui.add_space(10.0);
        widgets::section_header(
            ui,
            "Review approved renames",
            Some("Nothing is renamed until you confirm. The renames stay inside the trusted root."),
        );
        widgets::card(ui, |ui| {
            if let Some(root) = &review.trusted_root {
                ui.label(
                    egui::RichText::new(format!("Trusted root: {root}"))
                        .color(theme::muted(ui))
                        .small(),
                );
            }
            ui.label(
                egui::RichText::new(format!("{} rename(s) to apply", review.rows.len())).strong(),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .id_salt("dat-apply-review-rows")
                .show(ui, |ui| {
                    for row in &review.rows {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&row.current_basename).monospace());
                            ui.label(egui::RichText::new("→").color(theme::muted(ui)));
                            ui.label(egui::RichText::new(&row.proposed_basename).monospace());
                        });
                    }
                });
            ui.add_space(6.0);
            if let Some(phrase) = &review.required_phrase {
                ui.label(
                    egui::RichText::new(format!("Type {phrase} to confirm:"))
                        .color(theme::WARNING)
                        .small(),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut ui_state.plan_typed_confirmation)
                        .hint_text(phrase)
                        .id_salt("dat-apply-typed-confirmation"),
                );
                ui.add_space(4.0);
            }
            if apply.apply_running {
                ui.label(egui::RichText::new("Applying…").color(theme::muted(ui)));
            } else {
                let typed = ui_state.plan_typed_confirmation.clone();
                let can_confirm = match &review.required_phrase {
                    Some(phrase) => typed.trim() == *phrase,
                    None => true,
                };
                ui.horizontal(|ui| {
                    if widgets::action_button(
                        ui,
                        "Apply approved renames",
                        widgets::ActionStyle::Destructive,
                        can_confirm,
                    )
                    .clicked()
                    {
                        action = Some(DatSourcesPageAction::ConfirmApply {
                            typed: typed.clone(),
                        });
                    }
                    if apply.subset_available
                        && widgets::action_button(
                            ui,
                            "Apply only the independently safe subset",
                            widgets::ActionStyle::Secondary,
                            can_confirm,
                        )
                        .clicked()
                    {
                        action = Some(DatSourcesPageAction::ConfirmApplySafeSubset { typed });
                    }
                    if widgets::action_button(ui, "Cancel", widgets::ActionStyle::Quiet, true)
                        .clicked()
                    {
                        action = Some(DatSourcesPageAction::CancelApplyReview);
                    }
                });
            }
        });
    }

    if let Some(error) = &apply.apply_error {
        ui.add_space(8.0);
        widgets::banner(ui, "Apply did not run", error, widgets::StatusTone::Blocked);
    }

    if let Some(outcome) = &apply.outcome {
        ui.add_space(10.0);
        widgets::section_header(
            ui,
            "Rename transaction",
            Some(&format!(
                "requested {} · applied {} · skipped {} · failed {} · {}",
                outcome.requested,
                outcome.applied,
                outcome.skipped,
                outcome.failed,
                outcome.state.label()
            )),
        );
        widgets::card(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("Transaction: {}", outcome.transaction_id))
                    .color(theme::muted(ui))
                    .small(),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .id_salt("dat-apply-outcome-rows")
                .show(ui, |ui| {
                    for row in &outcome.rows {
                        ui.horizontal(|ui| {
                            widgets::status_badge(ui, row.state.label(), apply_row_tone(row.state));
                            ui.label(egui::RichText::new(&row.current_basename).monospace());
                            ui.label(egui::RichText::new("→").color(theme::muted(ui)));
                            ui.label(egui::RichText::new(&row.proposed_basename).monospace());
                        });
                        if let Some(reason) = &row.failure_reason {
                            ui.label(egui::RichText::new(reason).color(theme::WARNING).small());
                        }
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if widgets::action_button(
                    ui,
                    "Roll back transaction",
                    widgets::ActionStyle::Destructive,
                    !apply.rollback_running && outcome.applied > 0,
                )
                .clicked()
                {
                    action = Some(DatSourcesPageAction::RollbackTransaction {
                        id: outcome.transaction_id.clone(),
                    });
                }
                if widgets::action_button(ui, "Dismiss", widgets::ActionStyle::Quiet, true)
                    .clicked()
                {
                    action = Some(DatSourcesPageAction::ClearApplyOutcome);
                }
            });
        });
    }

    if let Some(result) = &apply.rollback_result {
        ui.add_space(8.0);
        widgets::banner(
            ui,
            result.label,
            &result.detail,
            match result.label {
                "Fully rolled back" => widgets::StatusTone::Success,
                _ => widgets::StatusTone::Warning,
            },
        );
    }

    if let Some(error) = &apply.rollback_error {
        ui.add_space(8.0);
        widgets::banner(
            ui,
            "Rollback could not run",
            error,
            widgets::StatusTone::Blocked,
        );
    }

    action
}

fn apply_row_tone(state: EntryState) -> widgets::StatusTone {
    match state {
        EntryState::Applied | EntryState::RolledBack => widgets::StatusTone::Success,
        EntryState::Skipped => widgets::StatusTone::Info,
        EntryState::ApplyFailed | EntryState::RollbackFailed => widgets::StatusTone::Blocked,
        _ => widgets::StatusTone::Info,
    }
}

/// The read-only rename-planning section.
///
/// Everything here displays the core plan; the only user actions are review
/// decisions (session-only, never touching a file) and copying a proposed
/// name. There is no Rename/Apply/Execute/Move/Delete control of any kind.
fn show_rename_plan_section(
    ui: &mut egui::Ui,
    plan: &RenamePlanView,
    ui_state: &mut DatSourcesPageUi,
) -> Option<DatSourcesPageAction> {
    let mut action = None;
    ui.add_space(10.0);
    widgets::section_header(ui, "Rename planning", None);

    widgets::banner(ui, "Planning only", SAFE_PROMISE, widgets::StatusTone::Info);
    ui.label(
        egui::RichText::new(PLAN_ONLY_PROMISE)
            .color(theme::muted(ui))
            .small(),
    );

    widgets::card(ui, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "Source '{}' · {} · {} of {} audited files verified",
                plan.source_display_name,
                plan.scan_root_short,
                plan.verified_total,
                plan.audited_total
            ))
            .color(theme::muted(ui))
            .small(),
        );
        if let Some(platform) = &plan.platform_display {
            ui.label(
                egui::RichText::new(format!("Platform: {platform}"))
                    .color(theme::muted(ui))
                    .small(),
            );
        }
        if plan.truncated {
            ui.add_space(4.0);
            widgets::banner(
                ui,
                "Partial plan",
                "The audit hit a ceiling, so this plan covers part of the folder.",
                widgets::StatusTone::Warning,
            );
        }
        if let Some(error) = &plan.error {
            ui.add_space(4.0);
            widgets::banner(ui, "Plan not produced", error, widgets::StatusTone::Warning);
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} suggested · {} already canonical · {} ambiguous · {} conflicts · {} \
                     unsupported · {} blocked · {} not selected · {} unknown",
                    plan.counts.suggested,
                    plan.counts.already_canonical,
                    plan.counts.ambiguous,
                    plan.counts.conflicts,
                    plan.counts.unsupported,
                    plan.counts.blocked,
                    plan.counts.excluded_by_content_policy,
                    plan.counts.unclassified_content
                ))
                .small(),
            );
        });

        // Filter row: which states are shown. Filters only change what is
        // drawn; they never decide anything about the plan.
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Show:").color(theme::muted(ui)));
            for filter in RenamePlanFilter::ALL {
                let selected = ui_state.plan_filter == filter;
                if ui.selectable_label(selected, filter.label()).clicked() {
                    ui_state.plan_filter = filter;
                }
            }
        });
    });

    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        let visible: Vec<&RenamePlanRowView> = plan
            .rows
            .iter()
            .filter(|row| ui_state.plan_filter.matches(row))
            .collect();
        if visible.is_empty() {
            ui.label(
                egui::RichText::new("No proposals match this filter.").color(theme::muted(ui)),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .id_salt("dat-rename-plan-rows")
            .show(ui, |ui| {
                for row in visible {
                    show_rename_plan_row(ui, row, &mut action);
                    ui.add_space(4.0);
                }
            });
    });

    if !plan.rows.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let approved_count = plan
                .rows
                .iter()
                .filter(|row| {
                    row.state == ProposalState::Suggested
                        && row.decision == Some(ReviewDecision::AcceptedForReview)
                })
                .count();
            if approved_count > 0 {
                if widgets::action_button(
                    ui,
                    format!("Apply approved renames ({approved_count})"),
                    widgets::ActionStyle::Destructive,
                    true,
                )
                .on_hover_text("Review and confirm before anything is renamed")
                .clicked()
                {
                    action = Some(DatSourcesPageAction::BeginApplyReview);
                }
                ui.add_space(8.0);
            }
            if widgets::action_button(
                ui,
                "Reset all review decisions",
                widgets::ActionStyle::Quiet,
                !plan.rows.is_empty(),
            )
            .clicked()
            {
                action = Some(DatSourcesPageAction::ClearReviewDecisions);
            }
        });
    }

    action
}

fn show_rename_plan_row(
    ui: &mut egui::Ui,
    row: &RenamePlanRowView,
    action: &mut Option<DatSourcesPageAction>,
) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                widgets::status_badge(ui, row.state.label(), plan_state_tone(row.state));
                if row.state == ProposalState::Suggested {
                    ui.label(
                        egui::RichText::new(row.current_basename.clone())
                            .monospace()
                            .strong(),
                    );
                } else {
                    ui.label(egui::RichText::new(row.current_basename.clone()).monospace());
                }
            });
            match &row.proposed_basename {
                Some(proposed) => {
                    ui.label(
                        egui::RichText::new(format!("→ {proposed}"))
                            .monospace()
                            .color(theme::muted(ui)),
                    );
                    if row.extension_preserved {
                        ui.label(
                            egui::RichText::new("extension preserved")
                                .color(theme::muted(ui))
                                .small(),
                        );
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("no proposed name")
                            .color(theme::muted(ui))
                            .small(),
                    );
                }
            }
            if let Some(platform) = &row.platform_display {
                ui.label(
                    egui::RichText::new(format!("{platform} · {}", row.source_display_name))
                        .color(theme::muted(ui))
                        .small(),
                );
            } else {
                ui.label(
                    egui::RichText::new(row.source_display_name.clone())
                        .color(theme::muted(ui))
                        .small(),
                );
            }
            if let Some(game) = &row.game_name {
                ui.label(
                    egui::RichText::new(format!(
                        "Matched: {game}{} · {}",
                        row.rom_name
                            .as_deref()
                            .filter(|rom| *rom != game)
                            .map(|rom| format!(" ({rom})"))
                            .unwrap_or_default(),
                        row.verdict_label
                    ))
                    .color(theme::muted(ui))
                    .small(),
                );
            }
            show_content_technical_details(ui, &row.content);
            if !row.explanations.is_empty() {
                ui.add_space(2.0);
                for line in &row.explanations {
                    ui.label(
                        egui::RichText::new(format!("• {line}"))
                            .color(theme::muted(ui))
                            .small(),
                    );
                }
            }
            if let Some(reason) = &row.ambiguity_reason {
                ui.label(
                    egui::RichText::new(format!("Ambiguous: {reason}"))
                        .color(theme::WARNING)
                        .small(),
                );
            }
            if let Some(detail) = &row.collision_detail {
                ui.label(
                    egui::RichText::new(format!("Conflict: {detail}"))
                        .color(theme::WARNING)
                        .small(),
                );
            }
            for blocker in &row.blockers {
                ui.label(
                    egui::RichText::new(format!("Blocked: {blocker}"))
                        .color(theme::DANGER)
                        .small(),
                );
            }
            for note in &row.sanitisation_notes {
                ui.label(
                    egui::RichText::new(note.clone())
                        .color(theme::muted(ui))
                        .small(),
                );
            }
            if let Some(decision) = row.decision {
                ui.label(
                    egui::RichText::new(format!("Your decision: {}", decision.label()))
                        .color(theme::SUCCESS)
                        .small(),
                );
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            if let Some(proposed) = &row.proposed_basename
                && ui
                    .add(egui::Button::new("Copy name").small())
                    .on_hover_text("Copy the proposed filename to the clipboard")
                    .clicked()
            {
                ui.ctx().copy_text(proposed.clone());
            }
            if ui
                .add(egui::Button::new("Needs review").small())
                .on_hover_text("Mark this proposal as needing manual review")
                .clicked()
            {
                *action = Some(DatSourcesPageAction::SetReviewDecision {
                    path: row.source_path.to_string_lossy().into_owned(),
                    decision: Some(ReviewDecision::NeedsManualReview),
                });
            }
            if ui
                .add(egui::Button::new("Ignore").small())
                .on_hover_text("Ignore this proposal for now")
                .clicked()
            {
                *action = Some(DatSourcesPageAction::SetReviewDecision {
                    path: row.source_path.to_string_lossy().into_owned(),
                    decision: Some(ReviewDecision::Ignored),
                });
            }
            if ui
                .add(egui::Button::new("Accept").small())
                .on_hover_text("Keep this proposal for a future review/apply stage")
                .clicked()
            {
                *action = Some(DatSourcesPageAction::SetReviewDecision {
                    path: row.source_path.to_string_lossy().into_owned(),
                    decision: Some(ReviewDecision::AcceptedForReview),
                });
            }
            if row.decision.is_some()
                && ui
                    .add(egui::Button::new("Clear").small())
                    .on_hover_text("Clear your decision; nothing on disk changes")
                    .clicked()
            {
                *action = Some(DatSourcesPageAction::SetReviewDecision {
                    path: row.source_path.to_string_lossy().into_owned(),
                    decision: None,
                });
            }
        });
    });
}

/// Renders unhashed (name-only) audit files grouped by reason, so a run with
/// thousands of symlink refusals shows one exact-count summary instead of
/// thousands of repeated lines. Raw details stay available via "Show all".
fn show_unhashed_groups(ui: &mut egui::Ui, unhashed: &[(String, String)]) {
    const EXAMPLES: usize = 10;
    let mut groups: std::collections::BTreeMap<&str, Vec<&(String, String)>> =
        std::collections::BTreeMap::new();
    for file in unhashed.iter() {
        groups.entry(file.1.as_str()).or_default().push(file);
    }
    // Most common reason first.
    let mut order: Vec<&str> = groups.keys().copied().collect();
    order.sort_by_key(|detail| std::cmp::Reverse(groups[*detail].len()));
    for detail in order {
        let files = &groups[detail];
        let count = files.len();
        let heading = if detail.contains("symlink") {
            format!("{count} symlinks could not be hashed")
        } else {
            format!("{count} files could not be hashed")
        };
        ui.label(egui::RichText::new(heading).strong());
        ui.label(egui::RichText::new(detail).color(theme::muted(ui)).small());
        ui.label(
            egui::RichText::new(format!(
                "Example{}: {}",
                if count == 1 { "" } else { "s" },
                files
                    .iter()
                    .take(EXAMPLES)
                    .map(|file| file.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .color(theme::muted(ui))
            .small(),
        );
        if count > EXAMPLES {
            egui::CollapsingHeader::new(format!("Show all {count}"))
                .id_salt(("dat-audit-unhashed", detail))
                .default_open(false)
                .show(ui, |ui| {
                    for file in files {
                        ui.label(egui::RichText::new(&file.0).monospace().small());
                    }
                });
        }
    }
    ui.label(
        egui::RichText::new(
            "These files were compared by name only, so their matches are not verified by \
             content.",
        )
        .color(theme::muted(ui))
        .small(),
    );
}

fn show_audit_result(ui: &mut egui::Ui, audit: &AuditResultView) {
    widgets::section_header(ui, "Audit result", Some(&audit.headline));
    widgets::card(ui, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "Source '{}' ({}) · checked {} · {} files read",
                audit.source_display_name,
                audit.source_id,
                audit.scan_root_short,
                audit.files_scanned
            ))
            .color(theme::muted(ui)),
        );
        ui.label(
            egui::RichText::new(format!(
                "Catalogue: {} ({} entries) from {}",
                audit.catalogue_names.join(", "),
                audit.catalogue_entries,
                audit.dat_path
            ))
            .color(theme::muted(ui))
            .small(),
        );
        ui.label(
            egui::RichText::new(format!(
                "Show: {} · full catalogue {} · games {} · compilations {} · required multidisc parts {} · non-game {} · unknown {}",
                audit.content_selection.label(),
                audit.content_summary.total,
                audit.content_summary.games,
                audit.content_summary.game_compilations,
                audit.content_summary.required_multidisc_parts,
                audit.content_summary.non_game,
                audit.content_summary.unknown,
            ))
            .color(theme::muted(ui))
            .small(),
        );
        if let Some(elapsed) = audit.elapsed_seconds {
            ui.label(
                egui::RichText::new(format!("Completed in {}", format_elapsed(elapsed)))
                    .color(theme::muted(ui))
                    .small(),
            );
        }
        ui.add_space(6.0);

        for category in &audit.categories {
            ui.horizontal_top(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:>6}", category.count))
                        .monospace()
                        .strong(),
                );
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(category.label).strong());
                    ui.label(
                        egui::RichText::new(category.meaning)
                            .color(theme::muted(ui))
                            .small(),
                    );
                });
            });
        }

        if audit.truncated {
            ui.add_space(6.0);
            widgets::banner(
                ui,
                "Partial result",
                "The folder held more files than one audit run reads, so this covers part of it. \
                 Audit a smaller folder for a complete answer.",
                widgets::StatusTone::Warning,
            );
        }
        if !audit.unreadable_catalogues.is_empty() {
            ui.add_space(6.0);
            widgets::banner(
                ui,
                "Some catalogues were not read",
                &audit.unreadable_catalogues.join("\n"),
                widgets::StatusTone::Warning,
            );
        }
        if !audit.unhashed.is_empty() {
            ui.add_space(6.0);
            widgets::section_header(
                ui,
                "Compared by name only",
                Some(
                    "These files could not be read for hashing, so any match below rests on the \
                     name alone.",
                ),
            );
            show_unhashed_groups(ui, &audit.unhashed);
        }

        if let Some(policy) = &audit.policy {
            ui.add_space(6.0);
            show_audit_policy_notes(ui, policy);
        }
    });

    ui.add_space(8.0);
    widgets::card(ui, |ui| {
        widgets::section_header(ui, "Files", None);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .id_salt("dat-audit-entries")
            .show(ui, |ui| {
                for entry in &audit.entries {
                    ui.horizontal_top(|ui| {
                        ui.label(egui::RichText::new(entry.verdict).monospace().small());
                        ui.vertical(|ui| {
                            ui.label(&entry.file_name);
                            if !entry.detail.is_empty() {
                                ui.label(
                                    egui::RichText::new(&entry.detail)
                                        .color(theme::muted(ui))
                                        .small(),
                                );
                            }
                            for content in &entry.content {
                                show_content_technical_details(ui, content);
                            }
                        });
                    });
                }
            });
        if audit.entries_truncated > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "{} more files are counted in the summary above but not listed here.",
                    audit.entries_truncated
                ))
                .color(theme::muted(ui))
                .small(),
            );
        }
    });
}

fn show_content_technical_details(ui: &mut egui::Ui, content: &ContentTechnicalView) {
    egui::CollapsingHeader::new("Technical classification details")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(format!("Classification: {}", content.classification));
            ui.label(format!("Confidence: {}", content.confidence));
            ui.label(format!("Classifier: {}", content.classifier_version));
            for evidence in &content.evidence {
                ui.label(format!("Evidence: {evidence}"));
            }
            if content.original_metadata.is_empty() {
                ui.label("Original metadata: none supplied by this DAT export");
            } else {
                for (field, value) in &content.original_metadata {
                    ui.label(format!("Original {field}: {value}"));
                }
            }
        });
}

fn show_kept_but_not_understood(ui: &mut egui::Ui, view: &DatSourcesPageView) {
    widgets::section_header(
        ui,
        "Kept but not recognised",
        Some(
            "These parts of your registry file name something this build does not know about. \
             They are preserved exactly as written, and saving from this page does not remove \
             them.",
        ),
    );
    widgets::card(ui, |ui| {
        for problem in &view.load_problems {
            ui.horizontal_top(|ui| {
                widgets::status_badge(ui, "Ignored", widgets::StatusTone::Warning);
                ui.add(egui::Label::new(problem).wrap());
            });
        }
        for row in &view.unresolved {
            ui.horizontal_top(|ui| {
                widgets::status_badge(ui, "Kept", widgets::StatusTone::Info);
                ui.add(egui::Label::new(&row.explanation).wrap());
            });
        }
    });
}

#[cfg(test)]
mod tests;
