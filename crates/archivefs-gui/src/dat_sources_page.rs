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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use archivefs_core::dat::limits::DatLimits;
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
pub(crate) const READ_ONLY_PROMISE: &str = "ArchiveFS never renames, moves, deletes or rewrites your ROMs. An audit reads files and \
     reports what it found; nothing is changed, and nothing is written beside them.";

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
    /// Every warning the last validation found, flattened across files in the
    /// deterministic order the files were read. Empty when there were none.
    pub(crate) warnings: Vec<String>,
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
}

/// One DAT file inside a source, as the Inspect panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectFileView {
    pub(crate) file_name: String,
    pub(crate) status: &'static str,
    /// The format and counts, or the parser's error.
    pub(crate) detail: String,
    pub(crate) warnings: Vec<String>,
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
}

impl DatSourcesPageView {
    /// Whether the page has nothing registered yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
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
    pub(crate) catalogue_names: Vec<String>,
    pub(crate) catalogue_entries: usize,
    pub(crate) headline: String,
    pub(crate) categories: Vec<AuditCategoryView>,
    /// Per-file lines, capped for display.
    pub(crate) entries: Vec<AuditEntryView>,
    pub(crate) entries_truncated: usize,
    pub(crate) unhashed: Vec<String>,
    pub(crate) unreadable_catalogues: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) files_scanned: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditEntryView {
    pub(crate) file_name: String,
    pub(crate) verdict: &'static str,
    pub(crate) detail: String,
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
    Audited(Box<DatAuditOutcome>),
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Validate,
    Audit,
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

/// A warning's text cut to a bounded length so the inline summary stays one
/// readable line. The full text is always preserved in the expandable details.
fn truncate_inline(text: &str) -> String {
    const MAX_INLINE_CHARS: usize = 120;
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_INLINE_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// Every warning the last validation found, flattened across files in the
/// deterministic order the files were read (files are sorted by name; each
/// file's warnings keep the parser's own order).
fn flatten_warnings(report: &DatValidationReport) -> Vec<String> {
    report
        .files
        .iter()
        .filter_map(|file| match &file.outcome {
            DatFileOutcome::Parsed { warnings, .. } => Some(warnings.as_slice()),
            DatFileOutcome::Failed { .. } => None,
        })
        .flatten()
        .cloned()
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
    audit: Option<Box<DatAuditOutcome>>,
    audit_error: Option<String>,
    job: Option<RunningJob>,
    /// Decides whether a symlinked ROM may be followed while hashing, exactly
    /// as it does everywhere else in the build.
    trusted: TrustedRoots,
    library_folders: Vec<PathBuf>,
    limits: DatLimits,
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
        Self {
            config_path,
            saved,
            draft,
            load_error,
            load_problems,
            save_state: DatSaveState::Idle,
            action_error: None,
            validations: BTreeMap::new(),
            audit: None,
            audit_error: None,
            job: None,
            trusted,
            library_folders,
            limits: DatLimits::default(),
        }
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
        self.job.is_some()
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
    pub(crate) fn poll(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        let mut changed = false;
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
                        job.latest = describe(&event);
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
                        self.validations.insert(id, *report);
                        changed = true;
                        finished = true;
                    }
                }
                Ok(JobMessage::Audited(outcome)) => {
                    if job.cancel_requested {
                        // A cancelled audit never appears complete - even when
                        // the worker finished before it observed the flag, the
                        // page must not present the late result as a completed
                        // audit.
                        finished = true;
                    } else {
                        self.audit = Some(outcome);
                        self.audit_error = None;
                        changed = true;
                        finished = true;
                    }
                }
                Ok(JobMessage::Failed(error)) => {
                    match job.kind {
                        JobKind::Audit => {
                            self.audit = None;
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
                    if self
                        .audit
                        .as_ref()
                        .is_some_and(|outcome| outcome.source_id == id)
                    {
                        // A result attributed to a source that is no longer
                        // registered would have nothing to point at.
                        self.audit = None;
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
            }
            DatSourcesPageAction::Save => self.save(),
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
        self.audit_error = None;
        let (sender, messages) = sync_channel(PROGRESS_QUEUE_DEPTH);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let trusted = self.trusted.clone();
        let request = DatAuditRequest {
            source_id: entry.id.clone(),
            source_display_name: entry.display_name.clone(),
            dat_path: entry.path.clone(),
            dat_kind: entry.kind,
            scan_root,
            limits: self.limits,
        };

        std::thread::spawn(move || {
            let report_sender = sender.clone();
            let outcome = run_dat_audit(&request, &trusted, &worker_cancel, &|progress| {
                send_progress(&report_sender, JobMessage::AuditProgress(progress));
            });
            let _ = match outcome {
                Ok(outcome) => sender.send(JobMessage::Audited(Box::new(outcome))),
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
            }),
            library_folders: self.library_folders.clone(),
            audit: self
                .audit
                .as_ref()
                .map(|outcome| Box::new(audit_view(outcome))),
            audit_error: self.audit_error.clone(),
            rows,
        }
    }

    fn row_view(&self, entry: &DatSourceEntry) -> DatSourceRowView {
        let saved = self.saved.get(&entry.id);
        let changed = match saved {
            None => true,
            Some(saved) => saved != entry,
        };
        let validation = self.validation(&entry.id);
        let warnings = validation.map(flatten_warnings).unwrap_or_default();
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
            warnings,
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
                    warnings,
                } => InspectFileView {
                    file_name: file.file_name.clone(),
                    status: if warnings.is_empty() {
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
                    warnings: warnings.clone(),
                },
                DatFileOutcome::Failed { error } => InspectFileView {
                    file_name: file.file_name.clone(),
                    status: "Failed",
                    detail: error.clone(),
                    warnings: Vec::new(),
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

/// Turns a core outcome into rows, without adding or merging any category.
fn audit_view(outcome: &DatAuditOutcome) -> AuditResultView {
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

    let entries: Vec<AuditEntryView> = outcome
        .report
        .entries
        .iter()
        .take(MAX_AUDIT_ENTRIES_SHOWN)
        .map(|entry| AuditEntryView {
            file_name: entry.local_filename.clone(),
            verdict: entry.verdict.label(),
            detail: verdict_detail(&entry.verdict),
        })
        .collect();
    let entries_truncated = outcome.report.entries.len().saturating_sub(entries.len());

    AuditResultView {
        source_display_name: outcome.source_display_name.clone(),
        source_id: outcome.source_id.clone(),
        dat_path: outcome.dat_path.clone(),
        scan_root: outcome.scan_root.clone(),
        catalogue_names: outcome.catalogue_names.clone(),
        catalogue_entries: outcome.catalogue_entries,
        headline: outcome.headline(),
        categories,
        entries,
        entries_truncated,
        unhashed: outcome
            .unhashed
            .iter()
            .map(|file| format!("{}: {}", file.file_name, file.detail))
            .collect(),
        unreadable_catalogues: outcome.unreadable_catalogues.clone(),
        truncated: outcome.truncated,
        files_scanned: outcome.files_scanned,
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
    /// Which source's warning-details disclosure is open.
    pub(crate) open_warnings: Option<String>,
}

impl DatSourcesPageUi {
    /// Forgets every unsubmitted choice.
    pub(crate) fn clear(&mut self) {
        self.open_inspect = None;
        self.open_platform_picker = None;
        self.platform_query.clear();
        self.open_audit_picker = None;
        self.confirm_remove = None;
        self.open_warnings = None;
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

    widgets::page_header(
        ui,
        "DAT sources",
        "Local DAT catalogues ArchiveFS can check your files against.",
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
        "Read-only",
        READ_ONLY_PROMISE,
        widgets::StatusTone::Info,
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
            "No DAT sources yet",
            "Add a DAT file, or a folder of them, to check your library against a published \
             catalogue. Nothing is downloaded and nothing is changed.",
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

    if let Some(error) = &view.audit_error {
        ui.add_space(8.0);
        widgets::banner(
            ui,
            "Audit could not run",
            error,
            widgets::StatusTone::Blocked,
        );
    }
    if let Some(audit) = &view.audit {
        ui.add_space(10.0);
        show_audit_result(ui, audit);
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

        show_warning_summary(ui, row, ui_state);

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

/// The warning count, a concise inline summary, and the expandable warning
/// details, drawn directly on the source card so "Valid, with warnings" is
/// never a bare badge with the reasons hidden behind Inspect.
fn show_warning_summary(
    ui: &mut egui::Ui,
    row: &DatSourceRowView,
    ui_state: &mut DatSourcesPageUi,
) {
    if row.warnings.is_empty() {
        return;
    }
    let open = ui_state.open_warnings.as_deref() == Some(row.id.as_str());
    let count = row.warnings.len();

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        widgets::status_badge(
            ui,
            format!(
                "{count} {}",
                if count == 1 { "warning" } else { "warnings" }
            ),
            widgets::StatusTone::Warning,
        );
        let label = if open {
            "Hide warning details"
        } else {
            "View warning details"
        };
        if widgets::action_button(ui, label, widgets::ActionStyle::Quiet, true).clicked() {
            ui_state.open_warnings = if open { None } else { Some(row.id.clone()) };
        }
        if row.history_link_available {
            // Only ever drawn when the full details really are recorded in
            // History & Logs; today they are kept inline, so this is off.
            ui.label(
                egui::RichText::new("Full details are recorded in History & Logs.")
                    .color(theme::muted(ui))
                    .small(),
            );
        }
    });

    // Concise inline summary: the first warning, kept to one line. The full
    // text of every warning is in the expandable list below.
    if let Some(first) = row.warnings.first() {
        ui.label(
            egui::RichText::new(format!("Summary: {}", truncate_inline(first)))
                .color(theme::muted(ui))
                .small(),
        );
    }

    if open {
        for warning in &row.warnings {
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new("•").color(widgets::StatusTone::Warning.color(ui)));
                // The original warning text is preserved verbatim; only the
                // inline summary is ever truncated.
                ui.add(egui::Label::new(warning).wrap());
            });
        }
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
            if widgets::action_button(
                ui,
                format!("Library folder: {}", folder.display()),
                widgets::ActionStyle::Secondary,
                true,
            )
            .clicked()
                && action.is_none()
            {
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
                    for warning in file.warnings.iter().take(20) {
                        ui.label(
                            egui::RichText::new(format!("warning: {warning}"))
                                .color(widgets::StatusTone::Warning.color(ui))
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
                     ArchiveFS does not pick one for you.",
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

fn show_audit_result(ui: &mut egui::Ui, audit: &AuditResultView) {
    widgets::section_header(ui, "Audit result", Some(&audit.headline));
    widgets::card(ui, |ui| {
        ui.label(
            egui::RichText::new(format!(
                "Source '{}' ({}) · checked {} · {} files read",
                audit.source_display_name, audit.source_id, audit.scan_root, audit.files_scanned
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
            for line in audit.unhashed.iter().take(50) {
                ui.label(egui::RichText::new(line).small());
            }
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
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(entry.verdict).monospace().small());
                        ui.label(&entry.file_name);
                        if !entry.detail.is_empty() {
                            ui.label(
                                egui::RichText::new(&entry.detail)
                                    .color(theme::muted(ui))
                                    .small(),
                            );
                        }
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
