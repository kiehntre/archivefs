use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::{
    ArchiveKind, ArchiveMountSession, ArchiveRecord, ArchiveSnapshot, ArchiveStats, ArchiveStatus,
    ArchiveUnmountSession, Config, DoctorReport, DoctorStatus, LazyUnmountCleanupResult,
    MountOneOutcome, MountState, UnmountOneOutcome, cleanup_selected_mount_tree,
    lazy_unmount_one_archive_path_with_progress, load_read_only_snapshot_default,
    mount_one_archive_path, remount_one_archive_path, unmount_one_archive_path,
};
use eframe::egui;

const COLUMN_WIDTHS: [f32; 4] = [120.0, 120.0, 440.0, 520.0];
const COLUMN_HEADERS: [&str; 4] = ["Platform", "State", "Archive path", "Mount path"];
const HISTORY_LIMIT: usize = 50;
const NORMAL_UNMOUNT_FAILURE_SUMMARY: &str = "ArchiveFS could not unmount this archive normally.\n\nA program may still be using files from this mount, or this may indicate that the mount is not responding correctly.";
const NORMAL_UNMOUNT_RECOVERY_GUIDANCE: &str = "Before using Lazy Unmount:\n\n1. Close any emulator, file manager, terminal, media player, or other application that may be using this mount.\n2. Wait a few seconds.\n3. Try Normal Unmount again.\n\nUse Lazy Unmount only when the mount will not release normally.";
const LAZY_UNMOUNT_WARNING: &str = "Lazy Unmount removes the mount from the visible filesystem immediately, even if a program still has files open.\n\nThis can interrupt applications using the mount and may cause unsaved work or incomplete file operations to be lost.\n\nClose applications using this mount before continuing.\n\nUse this only when Normal Unmount repeatedly fails.";
const LAZY_UNMOUNT_SUCCESS: &str = "Lazy unmount completed.\n\nThe mount is no longer visible. Some applications may still hold references to files that were open before the unmount. Close and reopen those applications before remounting.";
const LAZY_CLEANUP_SUCCESS: &str = "Empty mount directories were cleaned safely.";
const LAZY_CLEANUP_FAILURE: &str = "The mount was detached successfully, but ArchiveFS could not remove one or more empty directories. No non-empty directory was removed.";
const REMOUNT_GUIDANCE: &str = "Make sure applications that used the previous mount have been closed. Remounting while an application still holds the old mount may cause confusing or stale file access.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityAction {
    Refresh,
    Mount,
    MountAll,
    UnmountAll,
    Unmount,
    LazyUnmount,
    Remount,
    Cleanup,
}

impl std::fmt::Display for ActivityAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Refresh => "Refresh",
            Self::Mount => "Mount",
            Self::MountAll => "Mount All",
            Self::UnmountAll => "Unmount All",
            Self::Unmount => "Unmount",
            Self::LazyUnmount => "Lazy unmount",
            Self::Remount => "Remount",
            Self::Cleanup => "Cleanup",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityOutcome {
    Started,
    Offered,
    Retried,
    Confirmed,
    Cancelled,
    Skipped,
    Completed,
    Failed,
    Rejected,
}

impl std::fmt::Display for ActivityOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Started => "Started",
            Self::Offered => "Offered",
            Self::Retried => "Retried",
            Self::Confirmed => "Confirmed",
            Self::Cancelled => "Cancelled",
            Self::Skipped => "Skipped",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Rejected => "Rejected",
        })
    }
}

#[derive(Clone, Debug)]
struct HistoryEntry {
    timestamp: SystemTime,
    action: ActivityAction,
    archive_path: Option<PathBuf>,
    outcome: ActivityOutcome,
    message: String,
}

impl HistoryEntry {
    fn new(
        action: ActivityAction,
        archive_path: Option<PathBuf>,
        outcome: ActivityOutcome,
        message: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: SystemTime::now(),
            action,
            archive_path,
            outcome,
            message: message.into(),
        }
    }
}

#[derive(Default)]
struct OperationHistory {
    entries: VecDeque<HistoryEntry>,
}

impl OperationHistory {
    fn record(&mut self, entry: HistoryEntry) {
        self.entries.push_front(entry);
        self.entries.truncate(HISTORY_LIMIT);
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn entries(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter()
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ArchiveFS",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(ArchiveFsApp::new(
                creation_context.egui_ctx.clone(),
            )))
        }),
    )
}

struct LoadedData {
    mount_root: PathBuf,
    records: Vec<ArchiveRecord>,
    rows: Vec<ArchiveRow>,
    stats: ArchiveStats,
    doctor: DoctorReport,
}

impl LoadedData {
    fn from_snapshot(snapshot: ArchiveSnapshot) -> Self {
        let rows = snapshot
            .records
            .iter()
            .zip(&snapshot.statuses)
            .map(|(record, status)| ArchiveRow::new(record, status))
            .collect();

        Self {
            mount_root: snapshot.mount_root,
            records: snapshot.records,
            rows,
            stats: snapshot.stats,
            doctor: snapshot.doctor,
        }
    }
}

struct ArchiveRow {
    archive_path: String,
    mount_path: String,
    platform: String,
    state: String,
    search_text: String,
}

impl ArchiveRow {
    fn new(record: &ArchiveRecord, status: &ArchiveStatus) -> Self {
        let archive_path = status.archive_path.display().to_string();
        let mount_path = status.mount_path.display().to_string();
        let platform = record
            .metadata
            .platform
            .as_deref()
            .or(record.identity.platform.as_deref())
            .unwrap_or("Unknown")
            .to_string();
        let state = status.state.to_string();
        let search_text =
            format!("{archive_path}\n{mount_path}\n{platform}\n{state}").to_lowercase();

        Self {
            archive_path,
            mount_path,
            platform,
            state,
            search_text,
        }
    }

    fn matches(&self, normalized_filter: &str) -> bool {
        self.search_text.contains(normalized_filter)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountAllItem {
    archive_path: PathBuf,
    mount_path: PathBuf,
    display_name: String,
}

fn mount_all_available(pending_count: usize, busy: bool) -> bool {
    pending_count > 0 && !busy
}

fn pending_mount_items(records: &[ArchiveRecord]) -> Vec<MountAllItem> {
    records
        .iter()
        .filter(|record| record.mount_state == MountState::Pending)
        .map(|record| MountAllItem {
            archive_path: record.mount_plan.archive.path.clone(),
            mount_path: record.mount_plan.mount_path.clone(),
            display_name: record.identity.display_name.clone(),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BatchMountAttempt {
    Mounted(PathBuf),
    AlreadyMounted(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountAllFailure {
    archive_path: PathBuf,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MountAllSkipped {
    archive_path: PathBuf,
    reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MountAllResult {
    total: usize,
    successful: usize,
    failures: Vec<MountAllFailure>,
    skipped: Vec<MountAllSkipped>,
    unattempted: usize,
    stopped: bool,
    setup_failure: Option<String>,
}

impl MountAllResult {
    fn setup_failed(total: usize, error: impl Into<String>) -> Self {
        Self {
            total,
            unattempted: total,
            setup_failure: Some(error.into()),
            ..Self::default()
        }
    }

    fn attempted(&self) -> usize {
        self.successful + self.failures.len()
    }

    fn failed(&self) -> usize {
        self.failures.len()
    }

    fn skipped(&self) -> usize {
        self.skipped.len()
    }

    fn completion_message(&self) -> String {
        if self.setup_failure.is_some() {
            "Mount All could not start.".to_string()
        } else if self.stopped {
            format!(
                "Mount All stopped after the current archive. {} archives were not attempted.",
                self.unattempted
            )
        } else if self.failed() > 0 {
            format!(
                "Mount All completed with {} failure{}.",
                self.failed(),
                if self.failed() == 1 { "" } else { "s" }
            )
        } else {
            "Mount All completed successfully.".to_string()
        }
    }
}

#[derive(Clone, Debug)]
enum MountAllEvent {
    ArchiveStarted {
        index: usize,
        total: usize,
        item: MountAllItem,
    },
    ArchiveCompleted(MountAllItem),
    ArchiveFailed {
        item: MountAllItem,
        message: String,
    },
    ArchiveSkipped {
        item: MountAllItem,
        reason: String,
    },
    Finished(MountAllResult),
}

#[derive(Clone, Debug, Default)]
struct MountAllProgress {
    current_index: usize,
    total: usize,
    current_archive: Option<String>,
    successful: usize,
    failed: usize,
    skipped: usize,
    stop_requested: bool,
}

struct RunningMountAll {
    receiver: Receiver<MountAllEvent>,
    stop: Arc<AtomicBool>,
    progress: MountAllProgress,
}

#[derive(Clone)]
struct MountAllConfirmation;

fn run_mount_all_coordinator<E, V, M, P>(
    items: Vec<MountAllItem>,
    stop: &AtomicBool,
    mut archive_exists: E,
    mut validate: V,
    mut mount: M,
    mut publish: P,
) -> MountAllResult
where
    E: FnMut(&Path) -> bool,
    V: FnMut(&Path) -> Result<(), String>,
    M: FnMut(&Path) -> Result<BatchMountAttempt, String>,
    P: FnMut(MountAllEvent),
{
    let total = items.len();
    let mut result = MountAllResult {
        total,
        ..MountAllResult::default()
    };
    for (offset, mut item) in items.into_iter().enumerate() {
        if stop.load(Ordering::Acquire) {
            result.stopped = true;
            result.unattempted = total - offset;
            break;
        }

        if !archive_exists(&item.archive_path) {
            let reason = "archive disappeared before execution".to_string();
            result.skipped.push(MountAllSkipped {
                archive_path: item.archive_path.clone(),
                reason: reason.clone(),
            });
            publish(MountAllEvent::ArchiveSkipped { item, reason });
            continue;
        }

        if let Err(reason) = validate(&item.archive_path) {
            result.skipped.push(MountAllSkipped {
                archive_path: item.archive_path.clone(),
                reason: reason.clone(),
            });
            publish(MountAllEvent::ArchiveSkipped { item, reason });
            continue;
        }

        publish(MountAllEvent::ArchiveStarted {
            index: offset + 1,
            total,
            item: item.clone(),
        });
        match mount(&item.archive_path) {
            Ok(BatchMountAttempt::Mounted(actual_mount_path)) => {
                item.mount_path = actual_mount_path;
                result.successful += 1;
                publish(MountAllEvent::ArchiveCompleted(item));
            }
            Ok(BatchMountAttempt::AlreadyMounted(actual_mount_path)) => {
                item.mount_path = actual_mount_path;
                let reason = "archive is already mounted".to_string();
                result.skipped.push(MountAllSkipped {
                    archive_path: item.archive_path.clone(),
                    reason: reason.clone(),
                });
                publish(MountAllEvent::ArchiveSkipped { item, reason });
            }
            Err(message) => {
                result.failures.push(MountAllFailure {
                    archive_path: item.archive_path.clone(),
                    message: message.clone(),
                });
                publish(MountAllEvent::ArchiveFailed { item, message });
            }
        }
    }

    publish(MountAllEvent::Finished(result.clone()));
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnmountAllItem {
    archive_path: PathBuf,
    mount_path: PathBuf,
    display_name: String,
}

fn pending_unmount_items(records: &[ArchiveRecord]) -> Vec<UnmountAllItem> {
    records
        .iter()
        .filter(|record| record.mount_state == MountState::Mounted)
        .map(|record| UnmountAllItem {
            archive_path: record.mount_plan.archive.path.clone(),
            mount_path: record.mount_plan.mount_path.clone(),
            display_name: record.identity.display_name.clone(),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnmountAllFailure {
    archive_path: PathBuf,
    message: String,
    offer_lazy_unmount: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnmountAllSkip {
    archive_path: PathBuf,
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnmountAllCleanupFailure {
    mount_path: PathBuf,
    message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UnmountAllResult {
    total: usize,
    successful: usize,
    failures: Vec<UnmountAllFailure>,
    skipped: Vec<UnmountAllSkip>,
    unattempted: usize,
    cleanup_successes: usize,
    cleanup_failures: Vec<UnmountAllCleanupFailure>,
    stopped: bool,
    setup_failure: Option<String>,
}

impl UnmountAllResult {
    fn setup_failed(total: usize, error: impl Into<String>) -> Self {
        Self {
            total,
            unattempted: total,
            setup_failure: Some(error.into()),
            ..Self::default()
        }
    }

    fn attempted(&self) -> usize {
        self.successful + self.failures.len()
    }

    fn completion_message(&self) -> String {
        if self.setup_failure.is_some() {
            "Unmount All could not start.".to_string()
        } else if self.stopped {
            format!(
                "Unmount All stopped after the current archive. {} archives were not attempted.",
                self.unattempted
            )
        } else if !self.failures.is_empty() {
            format!(
                "Unmount All completed with {} failure{}.",
                self.failures.len(),
                if self.failures.len() == 1 { "" } else { "s" }
            )
        } else if !self.cleanup_failures.is_empty() {
            format!(
                "Unmount All completed, but cleanup failed for {} mount{}.",
                self.cleanup_failures.len(),
                if self.cleanup_failures.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            "Unmount All completed successfully.".to_string()
        }
    }
}

#[derive(Clone, Debug)]
enum UnmountAllEvent {
    ArchiveStarted {
        index: usize,
        total: usize,
        item: UnmountAllItem,
    },
    ArchiveCompleted(UnmountAllItem),
    ArchiveFailed {
        item: UnmountAllItem,
        message: String,
        offer_lazy_unmount: bool,
    },
    ArchiveSkipped {
        item: UnmountAllItem,
        reason: String,
    },
    CleanupStarted(PathBuf),
    CleanupCompleted(PathBuf),
    CleanupFailed {
        mount_path: PathBuf,
        message: String,
    },
    Finished(UnmountAllResult),
}

#[derive(Clone, Debug, Default)]
struct UnmountAllProgress {
    current_index: usize,
    total: usize,
    current_archive: Option<String>,
    successful: usize,
    failed: usize,
    skipped: usize,
    cleanup_successes: usize,
    cleanup_failures: usize,
    stop_requested: bool,
}

struct RunningUnmountAll {
    receiver: Receiver<UnmountAllEvent>,
    stop: Arc<AtomicBool>,
    progress: UnmountAllProgress,
}

#[derive(Clone)]
struct UnmountAllConfirmation;

#[derive(Debug)]
enum BatchUnmountAttempt {
    Unmounted,
    NotMounted,
}

#[derive(Debug)]
struct BatchUnmountError {
    message: String,
    offer_lazy_unmount: bool,
}

fn run_unmount_all_coordinator<U, C, P>(
    items: Vec<UnmountAllItem>,
    stop: &AtomicBool,
    mut unmount: U,
    mut cleanup: C,
    mut publish: P,
) -> UnmountAllResult
where
    U: FnMut(&UnmountAllItem) -> Result<BatchUnmountAttempt, BatchUnmountError>,
    C: FnMut(&UnmountAllItem, &mut dyn FnMut(UnmountAllEvent)) -> Option<Result<(), String>>,
    P: FnMut(UnmountAllEvent),
{
    let total = items.len();
    let mut result = UnmountAllResult {
        total,
        ..Default::default()
    };
    for (offset, item) in items.into_iter().enumerate() {
        if stop.load(Ordering::Acquire) {
            result.stopped = true;
            result.unattempted = total - offset;
            break;
        }
        publish(UnmountAllEvent::ArchiveStarted {
            index: offset + 1,
            total,
            item: item.clone(),
        });
        match unmount(&item) {
            Ok(BatchUnmountAttempt::Unmounted) => {
                result.successful += 1;
                publish(UnmountAllEvent::ArchiveCompleted(item.clone()));
                match cleanup(&item, &mut publish) {
                    Some(Ok(())) => {
                        result.cleanup_successes += 1;
                        publish(UnmountAllEvent::CleanupCompleted(item.mount_path));
                    }
                    Some(Err(message)) => {
                        result.cleanup_failures.push(UnmountAllCleanupFailure {
                            mount_path: item.mount_path.clone(),
                            message: message.clone(),
                        });
                        publish(UnmountAllEvent::CleanupFailed {
                            mount_path: item.mount_path,
                            message,
                        });
                    }
                    None => {}
                }
            }
            Ok(BatchUnmountAttempt::NotMounted) => {
                let reason = "archive is no longer mounted".to_string();
                result.skipped.push(UnmountAllSkip {
                    archive_path: item.archive_path.clone(),
                    reason: reason.clone(),
                });
                publish(UnmountAllEvent::ArchiveSkipped { item, reason });
            }
            Err(error) => {
                result.failures.push(UnmountAllFailure {
                    archive_path: item.archive_path.clone(),
                    message: error.message.clone(),
                    offer_lazy_unmount: error.offer_lazy_unmount,
                });
                publish(UnmountAllEvent::ArchiveFailed {
                    item,
                    message: error.message,
                    offer_lazy_unmount: error.offer_lazy_unmount,
                });
            }
        }
    }
    publish(UnmountAllEvent::Finished(result.clone()));
    result
}

type LoadResult = Result<LoadedData, String>;

enum LoadState {
    Loading(Receiver<LoadResult>),
    Ready(Box<LoadedData>),
    Error(String),
}

struct ArchiveFsApp {
    state: LoadState,
    filter: String,
    filtered_rows: Option<Vec<usize>>,
    selected_archive: Option<PathBuf>,
    operation: Option<RunningOperation>,
    mount_all: Option<RunningMountAll>,
    unmount_all: Option<RunningUnmountAll>,
    confirm_mount_all: Option<MountAllConfirmation>,
    focus_mount_all_cancel: bool,
    mount_all_result: Option<MountAllResult>,
    confirm_unmount_all: Option<UnmountAllConfirmation>,
    focus_unmount_all_cancel: bool,
    unmount_all_result: Option<UnmountAllResult>,
    feedback: Option<ActionFeedback>,
    confirm_unmount: Option<PathBuf>,
    confirm_lazy_unmount: Option<PathBuf>,
    confirm_lazy_unmount_final: Option<PathBuf>,
    focus_lazy_cancel: bool,
    focus_final_lazy_cancel: bool,
    lazy_unmount_offers: HashSet<PathBuf>,
    remount_offers: HashSet<PathBuf>,
    history: OperationHistory,
    cleanup_after_unmount: bool,
}

impl ArchiveFsApp {
    fn is_busy(&self) -> bool {
        self.operation.is_some() || self.mount_all.is_some() || self.unmount_all.is_some()
    }

    fn new(context: egui::Context) -> Self {
        let mut history = OperationHistory::default();
        history.record(HistoryEntry::new(
            ActivityAction::Refresh,
            None,
            ActivityOutcome::Started,
            "Loading archive snapshot.",
        ));
        Self {
            state: start_load(context),
            filter: String::new(),
            filtered_rows: None,
            selected_archive: None,
            operation: None,
            mount_all: None,
            unmount_all: None,
            confirm_mount_all: None,
            focus_mount_all_cancel: false,
            mount_all_result: None,
            confirm_unmount_all: None,
            focus_unmount_all_cancel: false,
            unmount_all_result: None,
            feedback: None,
            confirm_unmount: None,
            confirm_lazy_unmount: None,
            confirm_lazy_unmount_final: None,
            focus_lazy_cancel: false,
            focus_final_lazy_cancel: false,
            lazy_unmount_offers: HashSet::new(),
            remount_offers: HashSet::new(),
            history,
            cleanup_after_unmount: false,
        }
    }

    fn refresh(&mut self, context: &egui::Context) {
        self.history.record(HistoryEntry::new(
            ActivityAction::Refresh,
            None,
            ActivityOutcome::Started,
            "Refreshing archive snapshot.",
        ));
        self.state = start_load(context.clone());
    }

    fn poll_load(&mut self) {
        let result = match &self.state {
            LoadState::Loading(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(
                    "background data loader stopped unexpectedly".to_string(),
                )),
            },
            LoadState::Ready(_) | LoadState::Error(_) => None,
        };

        if let Some(result) = result {
            self.state = match result {
                Ok(data) => {
                    self.filtered_rows = matching_row_indices(&data.rows, &self.filter);
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Refresh,
                        None,
                        ActivityOutcome::Completed,
                        "Archive snapshot refreshed.",
                    ));
                    LoadState::Ready(Box::new(data))
                }
                Err(error) => {
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Refresh,
                        None,
                        ActivityOutcome::Failed,
                        error.clone(),
                    ));
                    LoadState::Error(error)
                }
            };
        }
    }

    fn start_operation(
        &mut self,
        context: egui::Context,
        action: ArchiveAction,
        archive_path: PathBuf,
        cleanup_after_unmount: bool,
    ) -> bool {
        self.start_operation_with_worker(
            context,
            action,
            archive_path,
            cleanup_after_unmount,
            |action, archive_path, cleanup_after_unmount, progress_sender| {
                perform_archive_action(
                    action,
                    &archive_path,
                    cleanup_after_unmount,
                    progress_sender,
                )
            },
        )
    }

    fn start_operation_with_worker<F>(
        &mut self,
        context: egui::Context,
        action: ArchiveAction,
        archive_path: PathBuf,
        cleanup_after_unmount: bool,
        worker: F,
    ) -> bool
    where
        F: FnOnce(ArchiveAction, PathBuf, bool, mpsc::Sender<OperationProgress>) -> OperationResult
            + Send
            + 'static,
    {
        if self.is_busy() {
            let message = "Another archive operation is already running.".to_string();
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message: message.clone(),
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.history.record(HistoryEntry::new(
                ActivityAction::from(action),
                Some(archive_path),
                ActivityOutcome::Rejected,
                message,
            ));
            return false;
        }

        let (sender, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        self.confirm_mount_all = None;
        self.confirm_unmount_all = None;
        self.focus_mount_all_cancel = false;
        self.confirm_unmount = None;
        self.confirm_lazy_unmount = None;
        self.confirm_lazy_unmount_final = None;
        self.focus_lazy_cancel = false;
        self.focus_final_lazy_cancel = false;
        self.feedback = None;
        self.history.record(HistoryEntry::new(
            ActivityAction::from(action),
            Some(archive_path.clone()),
            ActivityOutcome::Started,
            match action {
                ArchiveAction::Mount => "Mount started.",
                ArchiveAction::Unmount => "Unmount started.",
                ArchiveAction::LazyUnmount => "Lazy unmount started.",
                ArchiveAction::Remount => "Remount started.",
            },
        ));
        self.operation = Some(RunningOperation {
            action,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver,
        });
        thread::spawn(move || {
            let result = worker(action, archive_path, cleanup_after_unmount, progress_sender);
            let _ = sender.send(result);
            context.request_repaint();
        });
        true
    }

    fn record_pending_operation_progress(&mut self) {
        let progress = self
            .operation
            .as_ref()
            .map(|operation| operation.progress_receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in progress {
            match event {
                OperationProgress::CleanupStarted(mount_path) => {
                    record_cleanup_started_activity(&mut self.history, &mount_path);
                }
            }
        }
    }

    fn poll_operation(&mut self, context: &egui::Context) {
        self.record_pending_operation_progress();

        let result = self.operation.as_ref().and_then(|operation| {
            let result = match operation.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(OperationFailure {
                    message: "background archive operation stopped unexpectedly".to_string(),
                    offer_lazy_unmount: false,
                })),
            };
            result.map(|result| (operation.action, operation.archive_path.clone(), result))
        });

        if result.is_some() {
            self.record_pending_operation_progress();
        }

        if let Some((action, archive_path, result)) = result {
            self.operation = None;
            match result {
                Ok(success) => {
                    self.history.record(HistoryEntry::new(
                        ActivityAction::from(action),
                        Some(archive_path.clone()),
                        ActivityOutcome::Completed,
                        success.message.clone(),
                    ));
                    let cleanup_feedback = success.cleanup.as_ref().map(|cleanup| {
                        record_cleanup_finished_activity(&mut self.history, cleanup);
                        CleanupFeedback {
                            succeeded: matches!(cleanup, CleanupOutcome::Completed { .. }),
                            message: cleanup.message().to_string(),
                        }
                    });
                    self.feedback = Some(ActionFeedback {
                        succeeded: true,
                        message: success.message,
                        cleanup: cleanup_feedback,
                        warning: success.warning,
                        more_information: None,
                    });
                    match action {
                        ArchiveAction::Unmount | ArchiveAction::LazyUnmount => {
                            self.lazy_unmount_offers.remove(&archive_path);
                            self.remount_offers.insert(archive_path.clone());
                            self.history.record(HistoryEntry::new(
                                ActivityAction::Remount,
                                Some(archive_path),
                                ActivityOutcome::Offered,
                                "Remount offered after successful unmount.",
                            ));
                        }
                        ArchiveAction::Remount => {
                            self.remount_offers.remove(&archive_path);
                        }
                        ArchiveAction::Mount => {}
                    }
                    self.refresh(context);
                }
                Err(failure) => {
                    let normal_unmount_recovery =
                        action == ArchiveAction::Unmount && failure.offer_lazy_unmount;
                    let activity_message = if normal_unmount_recovery {
                        format!("Normal unmount failed: {}", failure.message)
                    } else {
                        failure.message.clone()
                    };
                    self.history.record(HistoryEntry::new(
                        ActivityAction::from(action),
                        Some(archive_path.clone()),
                        ActivityOutcome::Failed,
                        activity_message,
                    ));
                    if normal_unmount_recovery {
                        self.lazy_unmount_offers.insert(archive_path.clone());
                        self.history.record(HistoryEntry::new(
                            ActivityAction::LazyUnmount,
                            Some(archive_path),
                            ActivityOutcome::Offered,
                            "Lazy unmount offered after normal unmount failed.",
                        ));
                    }
                    self.feedback = Some(ActionFeedback {
                        succeeded: false,
                        message: if normal_unmount_recovery {
                            NORMAL_UNMOUNT_FAILURE_SUMMARY.to_string()
                        } else {
                            failure.message.clone()
                        },
                        cleanup: None,
                        warning: None,
                        more_information: normal_unmount_recovery.then(|| {
                            format!(
                                "{NORMAL_UNMOUNT_RECOVERY_GUIDANCE}\n\nArchiveFS detail: {}",
                                failure.message
                            )
                        }),
                    });
                }
            }
        }
    }

    fn start_mount_all(&mut self, context: egui::Context, items: Vec<MountAllItem>) -> bool {
        if self.is_busy() {
            let message = "Another archive operation is already running.".to_string();
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message: message.clone(),
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.history.record(HistoryEntry::new(
                ActivityAction::MountAll,
                None,
                ActivityOutcome::Rejected,
                message,
            ));
            return false;
        }

        let total = items.len();
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        self.confirm_mount_all = None;
        self.confirm_unmount_all = None;
        self.focus_mount_all_cancel = false;
        self.confirm_unmount = None;
        self.confirm_lazy_unmount = None;
        self.confirm_lazy_unmount_final = None;
        self.mount_all_result = None;
        self.unmount_all_result = None;
        self.feedback = None;
        self.history.record(HistoryEntry::new(
            ActivityAction::MountAll,
            None,
            ActivityOutcome::Started,
            format!("Mount All started for {total} pending archives."),
        ));
        self.mount_all = Some(RunningMountAll {
            receiver,
            stop,
            progress: MountAllProgress {
                total,
                ..MountAllProgress::default()
            },
        });

        thread::spawn(move || {
            let archive_paths = items
                .iter()
                .map(|item| item.archive_path.clone())
                .collect::<Vec<_>>();
            let setup = Config::load_default()
                .and_then(|config| ArchiveMountSession::new(&config))
                .map_err(|error| error.to_string())
                .and_then(|session| {
                    session
                        .validate_batch_targets(&archive_paths)
                        .map_err(|error| error.to_string())
                        .map(|validations| {
                            let validations = validations
                                .into_iter()
                                .map(|validation| {
                                    (validation.archive_path().to_path_buf(), validation)
                                })
                                .collect::<HashMap<_, _>>();
                            (session, validations)
                        })
                });
            let (session, validations) = match setup {
                Ok(setup) => setup,
                Err(error) => {
                    let _ = sender.send(MountAllEvent::Finished(MountAllResult::setup_failed(
                        total, error,
                    )));
                    context.request_repaint();
                    return;
                }
            };
            let repaint_context = context.clone();
            run_mount_all_coordinator(
                items,
                &worker_stop,
                |archive_path| archive_path.is_file(),
                |archive_path| match validations.get(archive_path) {
                    Some(validation) => validation
                        .skip_reason()
                        .map_or(Ok(()), |reason| Err(reason.to_string())),
                    None => Err("archive was not included in batch validation".to_string()),
                },
                |archive_path| {
                    let validation = validations.get(archive_path).ok_or_else(|| {
                        "archive was not included in batch validation".to_string()
                    })?;
                    match session
                        .mount_validated_batch_target(validation)
                        .map_err(|error| error.to_string())?
                    {
                        MountOneOutcome::Mounted(plan) => {
                            Ok(BatchMountAttempt::Mounted(plan.mount_path))
                        }
                        MountOneOutcome::AlreadyMounted(plan) => {
                            Ok(BatchMountAttempt::AlreadyMounted(plan.mount_path))
                        }
                    }
                },
                |event| {
                    let _ = sender.send(event);
                    repaint_context.request_repaint();
                },
            );
        });
        true
    }

    fn request_mount_all_stop(&mut self) {
        let Some(batch) = self.mount_all.as_mut() else {
            return;
        };
        if batch.progress.stop_requested {
            return;
        }
        batch.progress.stop_requested = true;
        batch.stop.store(true, Ordering::Release);
        self.history.record(HistoryEntry::new(
            ActivityAction::MountAll,
            None,
            ActivityOutcome::Cancelled,
            "Stop requested; the current archive will finish before Mount All stops.",
        ));
    }

    fn poll_mount_all(&mut self, context: &egui::Context) {
        let mut disconnected = false;
        let mut events = Vec::new();
        if let Some(batch) = self.mount_all.as_ref() {
            loop {
                match batch.receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = None;
        for event in events {
            let Some(batch) = self.mount_all.as_mut() else {
                break;
            };
            match event {
                MountAllEvent::ArchiveStarted { index, total, item } => {
                    batch.progress.current_index = index;
                    batch.progress.total = total;
                    batch.progress.current_archive = Some(item.display_name.clone());
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Mount,
                        Some(item.archive_path),
                        ActivityOutcome::Started,
                        format!("Mounting archive {index} of {total}."),
                    ));
                }
                MountAllEvent::ArchiveCompleted(item) => {
                    batch.progress.successful += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Mount,
                        Some(item.archive_path),
                        ActivityOutcome::Completed,
                        format!("Mounted at {}.", item.mount_path.display()),
                    ));
                }
                MountAllEvent::ArchiveFailed { item, message } => {
                    batch.progress.failed += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Mount,
                        Some(item.archive_path),
                        ActivityOutcome::Failed,
                        message,
                    ));
                }
                MountAllEvent::ArchiveSkipped { item, reason } => {
                    batch.progress.skipped += 1;
                    batch.progress.current_index =
                        batch.progress.successful + batch.progress.failed + batch.progress.skipped;
                    batch.progress.current_archive = Some(item.display_name.clone());
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Mount,
                        Some(item.archive_path),
                        ActivityOutcome::Skipped,
                        reason,
                    ));
                }
                MountAllEvent::Finished(result) => {
                    finished = Some(result);
                }
            }
        }

        if let Some(result) = finished {
            let message = result.completion_message();
            let setup_failure = result.setup_failure.as_deref();
            let activity_message = match setup_failure {
                Some(error) => format!("{message} Setup error: {error}"),
                None => format!(
                    "{message} Successful: {}, failed: {}, skipped: {}, unattempted: {}.",
                    result.successful,
                    result.failed(),
                    result.skipped(),
                    result.unattempted
                ),
            };
            self.history.record(HistoryEntry::new(
                ActivityAction::MountAll,
                None,
                if setup_failure.is_some() {
                    ActivityOutcome::Failed
                } else {
                    ActivityOutcome::Completed
                },
                activity_message,
            ));
            self.feedback = Some(ActionFeedback {
                succeeded: setup_failure.is_none(),
                message: match setup_failure {
                    Some(error) => format!("{message} {error}"),
                    None => message.clone(),
                },
                cleanup: None,
                warning: None,
                more_information: None,
            });
            let should_refresh = result.setup_failure.is_none();
            self.mount_all_result = Some(result);
            self.mount_all = None;
            if should_refresh {
                self.refresh(context);
            }
        } else if disconnected && self.mount_all.is_some() {
            let message = "Mount All background worker stopped unexpectedly.".to_string();
            self.history.record(HistoryEntry::new(
                ActivityAction::MountAll,
                None,
                ActivityOutcome::Failed,
                message.clone(),
            ));
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message,
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.mount_all = None;
            self.refresh(context);
        }
    }

    fn start_unmount_all(
        &mut self,
        context: egui::Context,
        items: Vec<UnmountAllItem>,
        cleanup_after_unmount: bool,
    ) -> bool {
        if self.is_busy() {
            let message = "Another archive operation is already running.".to_string();
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message: message.clone(),
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.history.record(HistoryEntry::new(
                ActivityAction::UnmountAll,
                None,
                ActivityOutcome::Rejected,
                message,
            ));
            return false;
        }

        let total = items.len();
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        self.confirm_mount_all = None;
        self.confirm_unmount_all = None;
        self.confirm_unmount = None;
        self.confirm_lazy_unmount = None;
        self.confirm_lazy_unmount_final = None;
        self.unmount_all_result = None;
        self.mount_all_result = None;
        self.feedback = None;
        self.history.record(HistoryEntry::new(
            ActivityAction::UnmountAll,
            None,
            ActivityOutcome::Started,
            format!("Unmount All started for {total} mounted archives."),
        ));
        self.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop,
            progress: UnmountAllProgress {
                total,
                ..Default::default()
            },
        });

        thread::spawn(move || {
            let setup = Config::load_default()
                .and_then(|config| {
                    ArchiveUnmountSession::new(&config).map(|session| (config, session))
                })
                .map_err(|error| error.to_string());
            let (config, session) = match setup {
                Ok(setup) => setup,
                Err(error) => {
                    let _ = sender.send(UnmountAllEvent::Finished(UnmountAllResult::setup_failed(
                        total, error,
                    )));
                    context.request_repaint();
                    return;
                }
            };
            let repaint_context = context.clone();
            run_unmount_all_coordinator(
                items,
                &worker_stop,
                |item| match session
                    .unmount_archive_path(&item.archive_path, &item.mount_path)
                    .map_err(|error| BatchUnmountError {
                        offer_lazy_unmount: error.allows_lazy_unmount_recovery(),
                        message: error.to_string(),
                    })? {
                    UnmountOneOutcome::NotMounted(_) => Ok(BatchUnmountAttempt::NotMounted),
                    UnmountOneOutcome::Unmounted(_) => Ok(BatchUnmountAttempt::Unmounted),
                },
                |item, publish| {
                    cleanup_after_unmount.then(|| {
                        publish(UnmountAllEvent::CleanupStarted(item.mount_path.clone()));
                        cleanup_selected_mount_tree(&config, &item.mount_path)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    })
                },
                |event| {
                    let _ = sender.send(event);
                    repaint_context.request_repaint();
                },
            );
        });
        true
    }

    fn request_unmount_all_stop(&mut self) {
        let Some(batch) = self.unmount_all.as_mut() else {
            return;
        };
        if batch.progress.stop_requested {
            return;
        }
        batch.progress.stop_requested = true;
        batch.stop.store(true, Ordering::Release);
        self.history.record(HistoryEntry::new(
            ActivityAction::UnmountAll,
            None,
            ActivityOutcome::Cancelled,
            "Stop requested; the current archive will finish before Unmount All stops.",
        ));
    }

    fn poll_unmount_all(&mut self, context: &egui::Context) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(batch) = self.unmount_all.as_ref() {
            loop {
                match batch.receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let mut finished = None;
        for event in events {
            let Some(batch) = self.unmount_all.as_mut() else {
                break;
            };
            match event {
                UnmountAllEvent::ArchiveStarted { index, total, item } => {
                    batch.progress.current_index = index;
                    batch.progress.total = total;
                    batch.progress.current_archive = Some(item.display_name.clone());
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Unmount,
                        Some(item.archive_path),
                        ActivityOutcome::Started,
                        format!("Unmounting archive {index} of {total}."),
                    ));
                }
                UnmountAllEvent::ArchiveCompleted(item) => {
                    batch.progress.successful += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Unmount,
                        Some(item.archive_path),
                        ActivityOutcome::Completed,
                        format!("Unmounted {}.", item.mount_path.display()),
                    ));
                }
                UnmountAllEvent::ArchiveFailed {
                    item,
                    message,
                    offer_lazy_unmount,
                } => {
                    batch.progress.failed += 1;
                    if offer_lazy_unmount {
                        self.lazy_unmount_offers.insert(item.archive_path.clone());
                        self.history.record(HistoryEntry::new(
                            ActivityAction::LazyUnmount,
                            Some(item.archive_path.clone()),
                            ActivityOutcome::Offered,
                            "Lazy unmount offered for individual recovery after normal unmount failed.",
                        ));
                    }
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Unmount,
                        Some(item.archive_path),
                        ActivityOutcome::Failed,
                        format!("Normal unmount failed: {message}"),
                    ));
                }
                UnmountAllEvent::ArchiveSkipped { item, reason } => {
                    batch.progress.skipped += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Unmount,
                        Some(item.archive_path),
                        ActivityOutcome::Skipped,
                        reason,
                    ));
                }
                UnmountAllEvent::CleanupStarted(path) => {
                    record_cleanup_started_activity(&mut self.history, &path);
                }
                UnmountAllEvent::CleanupCompleted(path) => {
                    batch.progress.cleanup_successes += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Cleanup,
                        Some(path.clone()),
                        ActivityOutcome::Completed,
                        format!("Cleanup completed for {}.", path.display()),
                    ));
                }
                UnmountAllEvent::CleanupFailed {
                    mount_path,
                    message,
                } => {
                    batch.progress.cleanup_failures += 1;
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Cleanup,
                        Some(mount_path),
                        ActivityOutcome::Failed,
                        message,
                    ));
                }
                UnmountAllEvent::Finished(result) => finished = Some(result),
            }
        }
        if let Some(result) = finished {
            let message = result.completion_message();
            let setup_failed = result.setup_failure.is_some();
            let setup_failure = result.setup_failure.as_deref();
            self.history.record(HistoryEntry::new(
                ActivityAction::UnmountAll,
                None,
                if setup_failed {
                    ActivityOutcome::Failed
                } else {
                    ActivityOutcome::Completed
                },
                setup_failure.map_or_else(
                    || message.clone(),
                    |error| format!("{message} Setup error: {error}"),
                ),
            ));
            self.feedback = Some(ActionFeedback {
                succeeded: !setup_failed,
                message: setup_failure
                    .map_or_else(|| message.clone(), |error| format!("{message} {error}")),
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.unmount_all_result = Some(result);
            self.unmount_all = None;
            if !setup_failed {
                self.refresh(context);
            }
        } else if disconnected && self.unmount_all.is_some() {
            let message = "Unmount All background worker stopped unexpectedly.".to_string();
            self.history.record(HistoryEntry::new(
                ActivityAction::UnmountAll,
                None,
                ActivityOutcome::Failed,
                message.clone(),
            ));
            self.feedback = Some(ActionFeedback {
                succeeded: false,
                message,
                cleanup: None,
                warning: None,
                more_information: None,
            });
            self.unmount_all = None;
            self.refresh(context);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveAction {
    Mount,
    Unmount,
    LazyUnmount,
    Remount,
}

struct OperationRequest {
    action: ArchiveAction,
    archive_path: PathBuf,
    cleanup_after_unmount: bool,
}

enum AppOperationRequest {
    Archive(OperationRequest),
    MountAll(Vec<MountAllItem>),
    UnmountAll {
        items: Vec<UnmountAllItem>,
        cleanup_after_unmount: bool,
    },
}

impl From<ArchiveAction> for ActivityAction {
    fn from(action: ArchiveAction) -> Self {
        match action {
            ArchiveAction::Mount => Self::Mount,
            ArchiveAction::Unmount => Self::Unmount,
            ArchiveAction::LazyUnmount => Self::LazyUnmount,
            ArchiveAction::Remount => Self::Remount,
        }
    }
}

type OperationResult = Result<OperationSuccess, OperationFailure>;

#[derive(Debug)]
enum OperationProgress {
    CleanupStarted(PathBuf),
}

#[derive(Debug)]
struct OperationFailure {
    message: String,
    offer_lazy_unmount: bool,
}

#[derive(Debug)]
struct OperationSuccess {
    message: String,
    cleanup: Option<CleanupOutcome>,
    warning: Option<String>,
}

#[derive(Debug)]
enum CleanupOutcome {
    Completed {
        mount_path: PathBuf,
        message: String,
    },
    Failed {
        mount_path: PathBuf,
        message: String,
    },
}

impl CleanupOutcome {
    fn mount_path(&self) -> &Path {
        match self {
            Self::Completed { mount_path, .. } | Self::Failed { mount_path, .. } => mount_path,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Completed { message, .. } | Self::Failed { message, .. } => message,
        }
    }
}

fn record_cleanup_started_activity(history: &mut OperationHistory, mount_path: &Path) {
    history.record(HistoryEntry::new(
        ActivityAction::Cleanup,
        Some(mount_path.to_path_buf()),
        ActivityOutcome::Started,
        format!("Cleanup started for {}.", mount_path.display()),
    ));
}

fn record_cleanup_finished_activity(history: &mut OperationHistory, cleanup: &CleanupOutcome) {
    history.record(HistoryEntry::new(
        ActivityAction::Cleanup,
        Some(cleanup.mount_path().to_path_buf()),
        match cleanup {
            CleanupOutcome::Completed { .. } => ActivityOutcome::Completed,
            CleanupOutcome::Failed { .. } => ActivityOutcome::Failed,
        },
        cleanup.message(),
    ));
}

struct RunningOperation {
    action: ArchiveAction,
    archive_path: PathBuf,
    receiver: Receiver<OperationResult>,
    progress_receiver: Receiver<OperationProgress>,
}

struct ActionFeedback {
    succeeded: bool,
    message: String,
    cleanup: Option<CleanupFeedback>,
    warning: Option<String>,
    more_information: Option<String>,
}

struct CleanupFeedback {
    succeeded: bool,
    message: String,
}

impl eframe::App for ArchiveFsApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_load();
        self.poll_operation(context);
        self.poll_mount_all(context);
        self.poll_unmount_all(context);
        let loading = matches!(self.state, LoadState::Loading(_));
        let busy = self.is_busy();
        if loading || busy {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ArchiveFS");
                ui.separator();
                ui.label("Library overview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!loading && !busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh(context);
                    }
                    if loading || busy {
                        ui.spinner();
                    }
                });
            });
        });
        show_activity_panel(context, &mut self.history);

        let mut retry = false;
        let mut requested_action = None;
        let mut stop_mount_all = false;
        let mut stop_unmount_all = false;
        egui::CentralPanel::default().show(context, |ui| {
            if let Some(batch) = self.mount_all.as_ref() {
                stop_mount_all = show_mount_all_progress(ui, &batch.progress);
                ui.separator();
            }
            if let Some(batch) = self.unmount_all.as_ref() {
                stop_unmount_all = show_unmount_all_progress(ui, &batch.progress);
                ui.separator();
            }
            match &self.state {
                LoadState::Loading(_) => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.spinner();
                        ui.heading("Loading ArchiveFS data...");
                        ui.label("Scanning runs in the background.");
                    });
                }
                LoadState::Error(error) => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.colored_label(ui.visuals().error_fg_color, "Could not load ArchiveFS");
                        ui.label(error);
                        ui.add_space(8.0);
                        retry = ui.button("Try again").clicked();
                    });
                }
                LoadState::Ready(data) => {
                    requested_action = show_loaded_data(
                        ui,
                        data,
                        LoadedViewState {
                            filter: &mut self.filter,
                            filtered_rows: &mut self.filtered_rows,
                            selected_archive: &mut self.selected_archive,
                            operation: self.operation.as_ref(),
                            busy,
                            feedback: self.feedback.as_ref(),
                            confirm_unmount: &mut self.confirm_unmount,
                            confirm_lazy_unmount: &mut self.confirm_lazy_unmount,
                            confirm_lazy_unmount_final: &mut self.confirm_lazy_unmount_final,
                            confirm_mount_all: &mut self.confirm_mount_all,
                            focus_mount_all_cancel: &mut self.focus_mount_all_cancel,
                            confirm_unmount_all: &mut self.confirm_unmount_all,
                            focus_unmount_all_cancel: &mut self.focus_unmount_all_cancel,
                            focus_lazy_cancel: &mut self.focus_lazy_cancel,
                            focus_final_lazy_cancel: &mut self.focus_final_lazy_cancel,
                            lazy_unmount_offers: &self.lazy_unmount_offers,
                            remount_offers: &self.remount_offers,
                            cleanup_after_unmount: &mut self.cleanup_after_unmount,
                            mount_all_result: self.mount_all_result.as_ref(),
                            unmount_all_result: self.unmount_all_result.as_ref(),
                            history: &mut self.history,
                        },
                    );
                }
            }
        });
        if stop_mount_all {
            self.request_mount_all_stop();
        }
        if stop_unmount_all {
            self.request_unmount_all_stop();
        }
        if retry {
            self.refresh(context);
        }
        if let Some(request) = requested_action {
            match request {
                AppOperationRequest::Archive(request) => {
                    self.start_operation(
                        context.clone(),
                        request.action,
                        request.archive_path,
                        request.cleanup_after_unmount,
                    );
                }
                AppOperationRequest::MountAll(items) => {
                    self.start_mount_all(context.clone(), items);
                }
                AppOperationRequest::UnmountAll {
                    items,
                    cleanup_after_unmount,
                } => {
                    self.start_unmount_all(context.clone(), items, cleanup_after_unmount);
                }
            }
        }
    }
}

fn start_load(context: egui::Context) -> LoadState {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = load_data();
        let _ = sender.send(result);
        context.request_repaint();
    });
    LoadState::Loading(receiver)
}

fn load_data() -> LoadResult {
    load_read_only_snapshot_default()
        .map(LoadedData::from_snapshot)
        .map_err(|error| error.to_string())
}

fn perform_archive_action(
    action: ArchiveAction,
    archive_path: &Path,
    cleanup_after_unmount: bool,
    progress_sender: mpsc::Sender<OperationProgress>,
) -> OperationResult {
    let config = Config::load_default().map_err(|error| OperationFailure {
        message: error.to_string(),
        offer_lazy_unmount: false,
    })?;
    match action {
        ArchiveAction::Mount => {
            let plan = mount_one_archive_path(&config, archive_path).map_err(|error| {
                OperationFailure {
                    message: error.to_string(),
                    offer_lazy_unmount: false,
                }
            })?;
            Ok(OperationSuccess {
                message: format!("Mounted at {}", plan.mount_path.display()),
                cleanup: None,
                warning: None,
            })
        }
        ArchiveAction::Unmount => run_unmount_with_cleanup(
            cleanup_after_unmount,
            || {
                let plan = unmount_one_archive_path(&config, archive_path).map_err(|error| {
                    OperationFailure {
                        message: error.to_string(),
                        offer_lazy_unmount: error.allows_lazy_unmount_recovery(),
                    }
                })?;
                Ok((
                    format!("Unmounted {}", plan.mount_path.display()),
                    plan.mount_path,
                ))
            },
            |mount_path| {
                cleanup_selected_mount_tree(&config, mount_path).map_err(|error| error.to_string())
            },
            |mount_path| send_cleanup_started(&progress_sender, mount_path),
        ),
        ArchiveAction::LazyUnmount => {
            let result = lazy_unmount_one_archive_path_with_progress(
                &config,
                archive_path,
                cleanup_after_unmount,
                |mount_path| send_cleanup_started(&progress_sender, mount_path),
            )
            .map_err(|error| OperationFailure {
                message: error.to_string(),
                offer_lazy_unmount: false,
            })?;
            let cleanup = result.cleanup.map(|cleanup| match cleanup {
                LazyUnmountCleanupResult::Completed(removed) => CleanupOutcome::Completed {
                    message: format!(
                        "{LAZY_CLEANUP_SUCCESS} Removed {} empty director{} from {}.",
                        removed.len(),
                        if removed.len() == 1 { "y" } else { "ies" },
                        result.mount_path.display()
                    ),
                    mount_path: result.mount_path.clone(),
                },
                LazyUnmountCleanupResult::Failed(error) => CleanupOutcome::Failed {
                    message: format!(
                        "{LAZY_CLEANUP_FAILURE} Path: {}. Detail: {error}",
                        result.mount_path.display(),
                    ),
                    mount_path: result.mount_path.clone(),
                },
            });
            Ok(OperationSuccess {
                message: LAZY_UNMOUNT_SUCCESS.to_string(),
                cleanup,
                warning: Some(format!(
                    "Emergency recovery used {} for {}.",
                    result.tool,
                    result.mount_path.display()
                )),
            })
        }
        ArchiveAction::Remount => {
            let plan = remount_one_archive_path(&config, archive_path).map_err(|error| {
                OperationFailure {
                    message: error.to_string(),
                    offer_lazy_unmount: false,
                }
            })?;
            Ok(OperationSuccess {
                message: format!("Remounted at {}", plan.mount_path.display()),
                cleanup: None,
                warning: None,
            })
        }
    }
}

fn send_cleanup_started(progress_sender: &mpsc::Sender<OperationProgress>, mount_path: &Path) {
    let _ = progress_sender.send(OperationProgress::CleanupStarted(mount_path.to_path_buf()));
}

fn run_unmount_with_cleanup<U, C>(
    cleanup_after_unmount: bool,
    unmount: U,
    cleanup: C,
    cleanup_started: impl FnOnce(&Path),
) -> OperationResult
where
    U: FnOnce() -> Result<(String, PathBuf), OperationFailure>,
    C: FnOnce(&Path) -> Result<Vec<PathBuf>, String>,
{
    let (message, mount_path) = unmount()?;
    if !cleanup_after_unmount {
        return Ok(OperationSuccess {
            message,
            cleanup: None,
            warning: None,
        });
    }

    cleanup_started(&mount_path);
    let cleanup = match cleanup(&mount_path) {
        Ok(removed) => CleanupOutcome::Completed {
            message: cleanup_completed_message(&mount_path, removed.len()),
            mount_path,
        },
        Err(error) => CleanupOutcome::Failed {
            message: format!("Cleanup failed for {}: {error}", mount_path.display()),
            mount_path,
        },
    };
    Ok(OperationSuccess {
        message,
        cleanup: Some(cleanup),
        warning: None,
    })
}

fn cleanup_completed_message(mount_path: &Path, removed_count: usize) -> String {
    format!(
        "Cleanup completed for {}: removed {} empty director{}.",
        mount_path.display(),
        removed_count,
        if removed_count == 1 { "y" } else { "ies" }
    )
}

fn show_mount_all_progress(ui: &mut egui::Ui, progress: &MountAllProgress) -> bool {
    egui::Frame::group(ui.style())
        .show(ui, |ui| {
            ui.strong(format!(
                "Mounting {} of {}",
                progress.current_index, progress.total
            ));
            if let Some(archive) = &progress.current_archive {
                ui.label(archive);
            } else {
                ui.label("Preparing Mount All...");
            }
            ui.horizontal(|ui| {
                ui.label(format!("Successful: {}", progress.successful));
                ui.label(format!("Failed: {}", progress.failed));
                ui.label(format!("Skipped: {}", progress.skipped));
            });
            let fraction = if progress.total == 0 {
                0.0
            } else {
                progress.current_index as f32 / progress.total as f32
            };
            ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).show_percentage());
            ui.add_enabled(
                !progress.stop_requested,
                egui::Button::new(if progress.stop_requested {
                    "Stop requested"
                } else {
                    "Stop After Current Archive"
                }),
            )
            .clicked()
        })
        .inner
}

fn show_mount_all_result(ui: &mut egui::Ui, result: &MountAllResult) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(result.completion_message());
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Attempted: {}", result.attempted()));
            ui.label(format!("Successful: {}", result.successful));
            ui.label(format!("Failed: {}", result.failed()));
            ui.label(format!("Skipped: {}", result.skipped()));
            if result.unattempted > 0 {
                ui.label(format!("Not attempted: {}", result.unattempted));
            }
        });
        if let Some(error) = &result.setup_failure {
            ui.colored_label(ui.visuals().error_fg_color, format!("Setup error: {error}"));
        }
        if !result.failures.is_empty() {
            egui::CollapsingHeader::new("Failed archives")
                .default_open(false)
                .show(ui, |ui| {
                    for failure in &result.failures {
                        let text =
                            format!("{} — {}", failure.archive_path.display(), failure.message);
                        ui.add(egui::Label::new(&text).truncate())
                            .on_hover_text(text);
                    }
                });
        }
    });
}

fn show_unmount_all_progress(ui: &mut egui::Ui, progress: &UnmountAllProgress) -> bool {
    egui::Frame::group(ui.style())
        .show(ui, |ui| {
            ui.strong(format!(
                "Unmounting {} of {}",
                progress.current_index, progress.total
            ));
            ui.label(
                progress
                    .current_archive
                    .as_deref()
                    .unwrap_or("Preparing Unmount All..."),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Successful: {}", progress.successful));
                ui.label(format!("Failed: {}", progress.failed));
                ui.label(format!("Skipped: {}", progress.skipped));
                ui.label(format!(
                    "Cleanup successful: {}",
                    progress.cleanup_successes
                ));
                ui.label(format!("Cleanup failed: {}", progress.cleanup_failures));
            });
            let fraction = if progress.total == 0 {
                0.0
            } else {
                progress.current_index as f32 / progress.total as f32
            };
            ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).show_percentage());
            ui.add_enabled(
                !progress.stop_requested,
                egui::Button::new(if progress.stop_requested {
                    "Stop requested"
                } else {
                    "Stop After Current Archive"
                }),
            )
            .clicked()
        })
        .inner
}

fn show_unmount_all_result(ui: &mut egui::Ui, result: &UnmountAllResult) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(result.completion_message());
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Attempted: {}", result.attempted()));
            ui.label(format!("Successful: {}", result.successful));
            ui.label(format!("Failed: {}", result.failures.len()));
            ui.label(format!("Skipped: {}", result.skipped.len()));
            ui.label(format!("Not attempted: {}", result.unattempted));
            ui.label(format!("Cleanup successful: {}", result.cleanup_successes));
            ui.label(format!("Cleanup failed: {}", result.cleanup_failures.len()));
        });
        if let Some(error) = &result.setup_failure {
            ui.colored_label(ui.visuals().error_fg_color, format!("Setup error: {error}"));
        }
        if !result.failures.is_empty() {
            egui::CollapsingHeader::new("Failed archives")
                .default_open(false)
                .show(ui, |ui| {
                    for failure in &result.failures {
                        let text = format!(
                            "{} — {}{}",
                            failure.archive_path.display(),
                            failure.message,
                            if failure.offer_lazy_unmount {
                                " — individual Lazy Unmount recovery available"
                            } else {
                                ""
                            }
                        );
                        ui.add(egui::Label::new(&text).truncate())
                            .on_hover_text(text);
                    }
                });
        }
        if !result.cleanup_failures.is_empty() {
            egui::CollapsingHeader::new("Cleanup failures")
                .default_open(false)
                .show(ui, |ui| {
                    for failure in &result.cleanup_failures {
                        let text =
                            format!("{} — {}", failure.mount_path.display(), failure.message);
                        ui.add(egui::Label::new(&text).truncate())
                            .on_hover_text(text);
                    }
                });
        }
    });
}

fn show_activity_panel(context: &egui::Context, history: &mut OperationHistory) {
    egui::SidePanel::right("activity")
        .default_width(300.0)
        .min_width(220.0)
        .resizable(true)
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Activity");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!history.entries.is_empty(), egui::Button::new("Clear"))
                        .clicked()
                    {
                        history.clear();
                    }
                });
            });
            ui.separator();

            if history.entries.is_empty() {
                ui.weak("No recent activity.");
                return;
            }

            let row_height = ui.text_style_height(&egui::TextStyle::Body);
            egui::ScrollArea::vertical()
                .id_salt("activity_history")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for entry in history.entries() {
                        let text = history_entry_text(entry);
                        ui.add_sized(
                            [ui.available_width(), row_height],
                            egui::Label::new(&text).truncate(),
                        )
                        .on_hover_text(text);
                    }
                });
        });
}

fn history_entry_text(entry: &HistoryEntry) -> String {
    let archive = entry
        .archive_path
        .as_deref()
        .map(|path| format!(" · {}", path.display()))
        .unwrap_or_default();
    format!(
        "[{}] {} · {}{} — {}",
        format_history_timestamp(entry.timestamp),
        entry.action,
        entry.outcome,
        archive,
        entry.message
    )
}

fn format_history_timestamp(timestamp: SystemTime) -> String {
    let seconds = timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

struct LoadedViewState<'a> {
    filter: &'a mut String,
    filtered_rows: &'a mut Option<Vec<usize>>,
    selected_archive: &'a mut Option<PathBuf>,
    operation: Option<&'a RunningOperation>,
    busy: bool,
    feedback: Option<&'a ActionFeedback>,
    confirm_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount_final: &'a mut Option<PathBuf>,
    confirm_mount_all: &'a mut Option<MountAllConfirmation>,
    focus_mount_all_cancel: &'a mut bool,
    confirm_unmount_all: &'a mut Option<UnmountAllConfirmation>,
    focus_unmount_all_cancel: &'a mut bool,
    focus_lazy_cancel: &'a mut bool,
    focus_final_lazy_cancel: &'a mut bool,
    lazy_unmount_offers: &'a HashSet<PathBuf>,
    remount_offers: &'a HashSet<PathBuf>,
    cleanup_after_unmount: &'a mut bool,
    mount_all_result: Option<&'a MountAllResult>,
    unmount_all_result: Option<&'a UnmountAllResult>,
    history: &'a mut OperationHistory,
}

fn show_loaded_data(
    ui: &mut egui::Ui,
    data: &LoadedData,
    view_state: LoadedViewState<'_>,
) -> Option<AppOperationRequest> {
    let LoadedViewState {
        filter,
        filtered_rows,
        selected_archive,
        operation,
        busy,
        feedback,
        confirm_unmount,
        confirm_lazy_unmount,
        confirm_lazy_unmount_final,
        confirm_mount_all,
        focus_mount_all_cancel,
        confirm_unmount_all,
        focus_unmount_all_cancel,
        focus_lazy_cancel,
        focus_final_lazy_cancel,
        lazy_unmount_offers,
        remount_offers,
        cleanup_after_unmount,
        mount_all_result,
        unmount_all_result,
        history,
    } = view_state;
    let mut requested_action = None;
    let pending_count = data.stats.pending_count;
    let mounted_count = data.stats.mounted_count;
    ui.horizontal_wrapped(|ui| {
        summary_value(ui, "Total archives", data.stats.total_archives);
        summary_value(ui, "Mounted", data.stats.mounted_count);
        summary_value(ui, "Pending", data.stats.pending_count);
        if ui
            .add_enabled(
                mount_all_available(pending_count, busy),
                egui::Button::new("Mount All"),
            )
            .clicked()
        {
            *confirm_mount_all = Some(MountAllConfirmation);
            *focus_mount_all_cancel = true;
            history.record(HistoryEntry::new(
                ActivityAction::MountAll,
                None,
                ActivityOutcome::Offered,
                format!("Mount All offered for {} pending archives.", pending_count),
            ));
        }
        if ui
            .add_enabled(mounted_count > 0 && !busy, egui::Button::new("Unmount All"))
            .clicked()
        {
            *confirm_unmount_all = Some(UnmountAllConfirmation);
            *focus_unmount_all_cancel = true;
            history.record(HistoryEntry::new(
                ActivityAction::UnmountAll,
                None,
                ActivityOutcome::Offered,
                format!("Unmount All offered for {mounted_count} mounted archives."),
            ));
        }
        ui.separator();
        let (readiness, color) = if data.doctor.is_ready() {
            ("Ready", ui.visuals().selection.bg_fill)
        } else {
            ("Needs attention", ui.visuals().error_fg_color)
        };
        ui.label("Doctor:");
        ui.colored_label(color, readiness);
    });

    if let Some(result) = mount_all_result {
        show_mount_all_result(ui, result);
    }
    if let Some(result) = unmount_all_result {
        show_unmount_all_result(ui, result);
    }

    ui.add_space(8.0);
    egui::CollapsingHeader::new("Doctor checks")
        .default_open(!data.doctor.is_ready())
        .show(ui, |ui| {
            egui::Grid::new("doctor_checks")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Status");
                    ui.strong("Check");
                    ui.strong("Detail");
                    ui.end_row();

                    for check in &data.doctor.checks {
                        ui.colored_label(
                            doctor_status_color(ui, check.status),
                            check.status.to_string(),
                        );
                        ui.label(&check.name);
                        ui.label(&check.detail);
                        ui.end_row();
                    }
                });
        });

    ui.separator();
    if let Some(feedback) = feedback {
        let color = if feedback.succeeded {
            egui::Color32::from_rgb(70, 170, 90)
        } else {
            ui.visuals().error_fg_color
        };
        ui.colored_label(color, &feedback.message);
        if let Some(warning) = &feedback.warning {
            ui.colored_label(egui::Color32::from_rgb(210, 140, 40), warning);
        }
        if let Some(more_information) = &feedback.more_information {
            egui::CollapsingHeader::new("More information")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(more_information);
                });
        }
        if let Some(cleanup) = &feedback.cleanup {
            let color = if cleanup.succeeded {
                egui::Color32::from_rgb(70, 170, 90)
            } else {
                ui.visuals().error_fg_color
            };
            ui.colored_label(color, &cleanup.message);
        }
    }
    if confirm_mount_all.is_some() {
        let actions_available = !busy;
        egui::Window::new("Mount All pending archives?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} pending archives will be mounted under {}.",
                    pending_count,
                    data.mount_root.display()
                ));
                ui.label(
                    "Archives are mounted one at a time. Large libraries may take several minutes.",
                );
                ui.label("A failure will be recorded, and later archives will still be attempted.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.add_enabled(
                        actions_available,
                        egui::Button::new("Cancel").fill(ui.visuals().selection.bg_fill),
                    );
                    if *focus_mount_all_cancel {
                        cancel.request_focus();
                        *focus_mount_all_cancel = false;
                    }
                    if cancel.clicked() {
                        history.record(HistoryEntry::new(
                            ActivityAction::MountAll,
                            None,
                            ActivityOutcome::Cancelled,
                            "Mount All cancelled before starting.",
                        ));
                        *confirm_mount_all = None;
                    }
                    if ui
                        .add_enabled(
                            mount_all_available(pending_count, busy),
                            egui::Button::new("Mount All"),
                        )
                        .clicked()
                    {
                        requested_action = Some(AppOperationRequest::MountAll(
                            pending_mount_items(&data.records),
                        ));
                        *confirm_mount_all = None;
                    }
                });
            });
    }

    if confirm_unmount_all.is_some() {
        let actions_available = !busy;
        egui::Window::new("Unmount All mounted archives?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{mounted_count} mounted archives under {} will be unmounted one at a time.",
                    data.mount_root.display()
                ));
                ui.label("Close applications using these mounts before continuing. Files that are still open may prevent normal unmounting.");
                ui.label("Close emulators, file managers, terminals, media players, and other applications using mounted files.");
                ui.label("A failure will be recorded, and later archives will still be attempted.");
                ui.label(format!(
                    "Cleanup after each successful unmount: {}.",
                    if *cleanup_after_unmount { "enabled" } else { "disabled" }
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.add_enabled(
                        actions_available,
                        egui::Button::new("Cancel").fill(ui.visuals().selection.bg_fill),
                    );
                    if *focus_unmount_all_cancel {
                        cancel.request_focus();
                        *focus_unmount_all_cancel = false;
                    }
                    if cancel.clicked() {
                        history.record(HistoryEntry::new(
                            ActivityAction::UnmountAll,
                            None,
                            ActivityOutcome::Cancelled,
                            "Unmount All cancelled before starting.",
                        ));
                        *confirm_unmount_all = None;
                    }
                    if ui
                        .add_enabled(mounted_count > 0 && !busy, egui::Button::new("Unmount All"))
                        .clicked()
                    {
                        requested_action = Some(AppOperationRequest::UnmountAll {
                            items: pending_unmount_items(&data.records),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        });
                        *confirm_unmount_all = None;
                    }
                });
            });
    }

    if let Some(request) = show_selected_archive(
        ui,
        selected_record(&data.records, selected_archive.as_deref()),
        SelectedArchiveViewState {
            operation,
            busy,
            confirm_unmount,
            confirm_lazy_unmount,
            focus_lazy_cancel,
            lazy_unmount_offers,
            remount_offers,
            cleanup_after_unmount,
        },
    ) {
        requested_action = Some(AppOperationRequest::Archive(request));
    }

    if let Some(archive_path) = confirm_lazy_unmount.clone() {
        let actions_available =
            lazy_confirmation_available(&archive_path, lazy_unmount_offers, busy);
        egui::Window::new("Use Lazy Unmount?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(LAZY_UNMOUNT_WARNING);
                ui.label(archive_path.display().to_string());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.add_enabled(
                        actions_available,
                        egui::Button::new("Cancel").fill(ui.visuals().selection.bg_fill),
                    );
                    if *focus_lazy_cancel {
                        cancel.request_focus();
                        *focus_lazy_cancel = false;
                    }
                    if cancel.clicked() {
                        record_recovery_activity(
                            history,
                            ActivityAction::LazyUnmount,
                            &archive_path,
                            ActivityOutcome::Cancelled,
                            "User cancelled lazy unmount.",
                        );
                        *confirm_lazy_unmount = None;
                    }
                    if ui
                        .add_enabled(
                            actions_available,
                            egui::Button::new("Try Normal Unmount Again"),
                        )
                        .clicked()
                    {
                        record_recovery_activity(
                            history,
                            ActivityAction::Unmount,
                            &archive_path,
                            ActivityOutcome::Retried,
                            "Normal unmount retried.",
                        );
                        requested_action = Some(AppOperationRequest::Archive(OperationRequest {
                            action: ArchiveAction::Unmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        }));
                        *confirm_lazy_unmount = None;
                    }
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Lazy Unmount"))
                        .clicked()
                    {
                        advance_to_final_lazy_confirmation(
                            confirm_lazy_unmount,
                            confirm_lazy_unmount_final,
                            focus_final_lazy_cancel,
                            &archive_path,
                        );
                    }
                });
            });
    }

    if let Some(archive_path) = confirm_lazy_unmount_final.clone() {
        let actions_available =
            lazy_confirmation_available(&archive_path, lazy_unmount_offers, busy);
        egui::Window::new("Confirm Lazy Unmount")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("This is the final confirmation. Close applications using this mount before continuing.");
                ui.label(archive_path.display().to_string());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let cancel = ui.add_enabled(
                        actions_available,
                        egui::Button::new("Cancel").fill(ui.visuals().selection.bg_fill),
                    );
                    if *focus_final_lazy_cancel {
                        cancel.request_focus();
                        *focus_final_lazy_cancel = false;
                    }
                    if cancel.clicked() {
                        record_recovery_activity(
                            history,
                            ActivityAction::LazyUnmount,
                            &archive_path,
                            ActivityOutcome::Cancelled,
                            "User cancelled lazy unmount.",
                        );
                        *confirm_lazy_unmount_final = None;
                    }
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Confirm Lazy Unmount"))
                        .clicked()
                    {
                        record_recovery_activity(
                            history,
                            ActivityAction::LazyUnmount,
                            &archive_path,
                            ActivityOutcome::Confirmed,
                            "Lazy unmount confirmed.",
                        );
                        requested_action = Some(AppOperationRequest::Archive(OperationRequest {
                            action: ArchiveAction::LazyUnmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        }));
                        *confirm_lazy_unmount_final = None;
                    }
                });
            });
    }

    if let Some(archive_path) = confirm_unmount.clone() {
        let actions_available = confirmation_actions_available(busy);
        egui::Window::new("Confirm unmount")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Unmount this archive?");
                ui.label(archive_path.display().to_string());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        *confirm_unmount = None;
                    }
                    if ui
                        .add_enabled(actions_available, egui::Button::new("Unmount"))
                        .clicked()
                    {
                        requested_action = Some(AppOperationRequest::Archive(OperationRequest {
                            action: ArchiveAction::Unmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        }));
                        *confirm_unmount = None;
                    }
                });
            });
    }

    ui.separator();
    let mut filter_changed = false;
    ui.horizontal(|ui| {
        ui.label("Search:");
        filter_changed |= ui
            .add(
                egui::TextEdit::singleline(filter)
                    .hint_text("archive, mount path, platform, or state")
                    .desired_width(360.0),
            )
            .changed();
        if !filter.is_empty() && ui.small_button("Clear").clicked() {
            filter.clear();
            filter_changed = true;
        }
    });
    if filter_changed {
        *filtered_rows = matching_row_indices(&data.rows, filter);
    }

    let visible_count = filtered_rows.as_ref().map_or(data.rows.len(), Vec::len);
    ui.label(format!(
        "Showing {} of {} archives",
        visible_count,
        data.rows.len()
    ));
    ui.add_space(4.0);
    let row_height = fixed_row_height(
        ui.text_style_height(&egui::TextStyle::Body),
        ui.spacing().interact_size.y,
    );
    let horizontal_spacing = ui.spacing().item_spacing.x;
    let selected_index = selected_record_index(&data.records, selected_archive.as_deref());
    let mut clicked_index = None;
    egui::ScrollArea::horizontal()
        .id_salt("archive_status_horizontal")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(table_width(horizontal_spacing));
            show_table_cells(ui, &COLUMN_HEADERS, row_height, true, false);
            ui.separator();

            let body_height = ui.available_height().max(row_height);
            egui::ScrollArea::vertical()
                .id_salt("archive_status_vertical")
                .max_height(body_height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, visible_count, |ui, row_range| {
                    clicked_index = show_archive_rows(
                        ui,
                        &data.rows,
                        filtered_rows.as_deref(),
                        row_range,
                        row_height,
                        selected_index,
                    );
                });
        });
    if let Some(index) = clicked_index {
        *selected_archive = Some(data.records[index].mount_plan.archive.path.clone());
    }

    requested_action
}

fn fixed_row_height(text_height: f32, interact_height: f32) -> f32 {
    text_height.max(interact_height)
}

fn table_width(horizontal_spacing: f32) -> f32 {
    COLUMN_WIDTHS.iter().sum::<f32>()
        + horizontal_spacing * (COLUMN_WIDTHS.len().saturating_sub(1) as f32)
}

fn show_table_cells(
    ui: &mut egui::Ui,
    cells: &[&str; 4],
    row_height: f32,
    strong: bool,
    selected: bool,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        for (text, width) in cells.iter().zip(COLUMN_WIDTHS) {
            let widget_text: egui::WidgetText = if strong {
                egui::RichText::new(*text).strong().into()
            } else {
                (*text).into()
            };
            let response = if strong {
                ui.add_sized(
                    [width, row_height],
                    egui::Label::new(widget_text).truncate(),
                )
            } else {
                ui.add_sized(
                    [width, row_height],
                    egui::Button::new(widget_text)
                        .selected(selected)
                        .frame(false)
                        .truncate(),
                )
            };
            clicked |= response.on_hover_text(*text).clicked();
        }
    });
    clicked
}

fn show_archive_rows(
    ui: &mut egui::Ui,
    rows: &[ArchiveRow],
    filtered_rows: Option<&[usize]>,
    row_range: Range<usize>,
    row_height: f32,
    selected_index: Option<usize>,
) -> Option<usize> {
    let mut clicked_index = None;
    for visible_index in row_range {
        let row_index = filtered_rows
            .map(|indices| indices[visible_index])
            .unwrap_or(visible_index);
        let row = &rows[row_index];
        let cells = [
            row.platform.as_str(),
            row.state.as_str(),
            row.archive_path.as_str(),
            row.mount_path.as_str(),
        ];
        if show_table_cells(
            ui,
            &cells,
            row_height,
            false,
            selected_index == Some(row_index),
        ) {
            clicked_index = Some(row_index);
        }
    }
    clicked_index
}

fn selected_record<'a>(
    records: &'a [ArchiveRecord],
    selected_archive: Option<&Path>,
) -> Option<&'a ArchiveRecord> {
    selected_record_index(records, selected_archive).map(|index| &records[index])
}

fn selected_record_index(
    records: &[ArchiveRecord],
    selected_archive: Option<&Path>,
) -> Option<usize> {
    let selected_archive = selected_archive?;
    records
        .iter()
        .position(|record| record.mount_plan.archive.path == selected_archive)
}

fn available_action(mount_state: MountState) -> ArchiveAction {
    match mount_state {
        MountState::Mounted => ArchiveAction::Unmount,
        MountState::Pending | MountState::MountPathExists => ArchiveAction::Mount,
    }
}

fn individual_actions_available(busy: bool) -> bool {
    !busy
}

fn confirmation_actions_available(busy: bool) -> bool {
    individual_actions_available(busy)
}

fn record_recovery_activity(
    history: &mut OperationHistory,
    action: ActivityAction,
    archive_path: &Path,
    outcome: ActivityOutcome,
    message: &'static str,
) {
    history.record(HistoryEntry::new(
        action,
        Some(archive_path.to_path_buf()),
        outcome,
        message,
    ));
}

fn advance_to_final_lazy_confirmation(
    warning_confirmation: &mut Option<PathBuf>,
    final_confirmation: &mut Option<PathBuf>,
    focus_final_cancel: &mut bool,
    archive_path: &Path,
) {
    *final_confirmation = Some(archive_path.to_path_buf());
    *warning_confirmation = None;
    *focus_final_cancel = true;
}

fn lazy_confirmation_available(
    confirmed_archive: &Path,
    offered_archives: &HashSet<PathBuf>,
    busy: bool,
) -> bool {
    !busy && offered_archives.contains(confirmed_archive)
}

fn lazy_unmount_available(
    record: &ArchiveRecord,
    offered_archives: &HashSet<PathBuf>,
    busy: bool,
) -> bool {
    !busy
        && record.mount_state == MountState::Mounted
        && offered_archives.contains(&record.mount_plan.archive.path)
}

fn remount_available(
    record: &ArchiveRecord,
    offered_archives: &HashSet<PathBuf>,
    busy: bool,
) -> bool {
    !busy
        && record.mount_state != MountState::Mounted
        && offered_archives.contains(&record.mount_plan.archive.path)
}

fn remount_is_offered(record: &ArchiveRecord, offered_archives: &HashSet<PathBuf>) -> bool {
    record.mount_state != MountState::Mounted
        && offered_archives.contains(&record.mount_plan.archive.path)
}

struct SelectedArchiveViewState<'a> {
    operation: Option<&'a RunningOperation>,
    busy: bool,
    confirm_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount: &'a mut Option<PathBuf>,
    focus_lazy_cancel: &'a mut bool,
    lazy_unmount_offers: &'a HashSet<PathBuf>,
    remount_offers: &'a HashSet<PathBuf>,
    cleanup_after_unmount: &'a mut bool,
}

fn show_selected_archive(
    ui: &mut egui::Ui,
    record: Option<&ArchiveRecord>,
    view_state: SelectedArchiveViewState<'_>,
) -> Option<OperationRequest> {
    let SelectedArchiveViewState {
        operation,
        busy,
        confirm_unmount,
        confirm_lazy_unmount,
        focus_lazy_cancel,
        lazy_unmount_offers,
        remount_offers,
        cleanup_after_unmount,
    } = view_state;
    let mut request = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong("Selected archive");
        let Some(record) = record else {
            ui.label("Select an archive row to view details.");
            return;
        };

        egui::Grid::new("selected_archive_details")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                detail_row(
                    ui,
                    "Archive path",
                    &record.mount_plan.archive.path.display().to_string(),
                );
                detail_row(
                    ui,
                    "Mount path",
                    &record.mount_plan.mount_path.display().to_string(),
                );
                detail_row(
                    ui,
                    "Platform",
                    record
                        .metadata
                        .platform
                        .as_deref()
                        .or(record.identity.platform.as_deref())
                        .unwrap_or("Unknown"),
                );
                detail_row(
                    ui,
                    "Archive format",
                    archive_kind_name(record.mount_plan.archive.kind),
                );
                detail_row(ui, "Size", &format_size(record.identity.size_bytes));
                detail_row(ui, "Mount state", &record.mount_state.to_string());
                detail_row(ui, "Health", &record.health.to_string());
                optional_detail_row(ui, "Title", record.metadata.title.as_deref());
                optional_detail_row(ui, "Region", record.metadata.region.as_deref());
                optional_detail_row(ui, "Version", record.metadata.version.as_deref());
                optional_detail_row(ui, "Disc", record.metadata.disc.as_deref());
                optional_detail_row(ui, "Publisher", record.metadata.publisher.as_deref());
                optional_detail_row(ui, "Developer", record.metadata.developer.as_deref());
                optional_detail_row(ui, "Genre", record.metadata.genre.as_deref());
                optional_detail_row(ui, "Notes", record.metadata.notes.as_deref());
                optional_detail_row(ui, "Metadata source", record.metadata.source.as_deref());
                if let Some(year) = record.metadata.release_year {
                    detail_row(ui, "Release year", &year.to_string());
                }
                if let Some(languages) = &record.metadata.languages {
                    detail_row(ui, "Languages", &languages.join(", "));
                }
            });

        ui.add_space(6.0);
        let can_lazy_unmount = lazy_unmount_available(record, lazy_unmount_offers, busy);
        let remount_offered = remount_is_offered(record, remount_offers);
        let action = if remount_offered {
            ArchiveAction::Remount
        } else {
            available_action(record.mount_state)
        };
        ui.strong("Options");
        ui.add_enabled_ui(!busy, |ui| {
            ui.checkbox(
                cleanup_after_unmount,
                "Clean empty mount directories after unmount",
            );
        });
        ui.add_space(4.0);
        if remount_offered {
            ui.colored_label(egui::Color32::from_rgb(210, 140, 40), REMOUNT_GUIDANCE);
        }
        let label = match action {
            ArchiveAction::Mount => "Mount",
            ArchiveAction::Unmount => "Unmount",
            ArchiveAction::LazyUnmount => "Lazy Unmount",
            ArchiveAction::Remount => "Remount",
        };
        let primary_enabled = match action {
            ArchiveAction::Remount => remount_available(record, remount_offers, busy),
            ArchiveAction::Mount | ArchiveAction::Unmount | ArchiveAction::LazyUnmount => {
                individual_actions_available(busy)
            }
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(primary_enabled, egui::Button::new(label))
                .clicked()
            {
                let archive_path = record.mount_plan.archive.path.clone();
                match action {
                    ArchiveAction::Mount => {
                        request = Some(OperationRequest {
                            action,
                            archive_path,
                            cleanup_after_unmount: false,
                        })
                    }
                    ArchiveAction::Unmount => *confirm_unmount = Some(archive_path),
                    ArchiveAction::LazyUnmount => unreachable!("lazy unmount uses recovery button"),
                    ArchiveAction::Remount => {
                        request = Some(OperationRequest {
                            action,
                            archive_path,
                            cleanup_after_unmount: false,
                        })
                    }
                }
            }
            if can_lazy_unmount
                && ui
                    .add(egui::Button::new("Lazy Unmount"))
                    .on_hover_text(
                        "Emergency recovery option available because normal unmount failed.",
                    )
                    .clicked()
            {
                *confirm_lazy_unmount = Some(record.mount_plan.archive.path.clone());
                *focus_lazy_cancel = true;
            }
            if let Some(operation) = operation {
                ui.spinner();
                ui.label(match operation.action {
                    ArchiveAction::Mount => "Mounting...",
                    ArchiveAction::Unmount => "Unmounting...",
                    ArchiveAction::LazyUnmount => "Lazy unmounting...",
                    ArchiveAction::Remount => "Remounting...",
                });
            }
        });
    });
    request
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.strong(label);
    ui.label(value);
    ui.end_row();
}

fn optional_detail_row(ui: &mut egui::Ui, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        detail_row(ui, label, value);
    }
}

fn archive_kind_name(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Zip => "ZIP",
        ArchiveKind::SevenZip => "7z",
        ArchiveKind::Rar => "RAR",
    }
}

fn format_size(size_bytes: Option<u64>) -> String {
    size_bytes
        .map(|size| format!("{size} bytes"))
        .unwrap_or_else(|| "Unknown".to_string())
}

fn summary_value(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.group(|ui| {
        ui.vertical_centered(|ui| {
            ui.strong(value.to_string());
            ui.small(label);
        });
    });
}

fn doctor_status_color(ui: &egui::Ui, status: DoctorStatus) -> egui::Color32 {
    match status {
        DoctorStatus::Pass => egui::Color32::from_rgb(70, 170, 90),
        DoctorStatus::Warn => egui::Color32::from_rgb(220, 170, 40),
        DoctorStatus::Fail => ui.visuals().error_fg_color,
    }
}

fn matching_row_indices(rows: &[ArchiveRow], filter: &str) -> Option<Vec<usize>> {
    let normalized_filter = filter.trim().to_lowercase();
    if normalized_filter.is_empty() {
        return None;
    }

    Some(
        rows.iter()
            .enumerate()
            .filter_map(|(index, row)| row.matches(&normalized_filter).then_some(index))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use archivefs_core::{Archive, ArchiveHealth, ArchiveMetadata, MountPlan};

    fn row(search_text: &str) -> ArchiveRow {
        ArchiveRow {
            archive_path: String::new(),
            mount_path: String::new(),
            platform: String::new(),
            state: String::new(),
            search_text: search_text.to_lowercase(),
        }
    }

    fn record(archive_path: &str, mount_state: MountState) -> ArchiveRecord {
        let archive = Archive::from_path(archive_path).unwrap();
        ArchiveRecord::new(
            MountPlan::new(archive, PathBuf::from("/mnt/archivefs/Test")),
            mount_state,
            ArchiveMetadata {
                title: None,
                platform: None,
                region: None,
                languages: None,
                version: None,
                disc: None,
                publisher: None,
                developer: None,
                release_year: None,
                genre: None,
                notes: None,
                source: None,
            },
            ArchiveHealth::Pending,
        )
    }

    fn app_for_operation_tests() -> ArchiveFsApp {
        ArchiveFsApp {
            state: LoadState::Error("not loaded in this test".to_string()),
            filter: String::new(),
            filtered_rows: None,
            selected_archive: None,
            operation: None,
            mount_all: None,
            unmount_all: None,
            confirm_mount_all: None,
            focus_mount_all_cancel: false,
            mount_all_result: None,
            confirm_unmount_all: None,
            focus_unmount_all_cancel: false,
            unmount_all_result: None,
            feedback: None,
            confirm_unmount: None,
            confirm_lazy_unmount: None,
            confirm_lazy_unmount_final: None,
            focus_lazy_cancel: false,
            focus_final_lazy_cancel: false,
            lazy_unmount_offers: HashSet::new(),
            remount_offers: HashSet::new(),
            history: OperationHistory::default(),
            cleanup_after_unmount: false,
        }
    }

    fn history_entry(outcome: ActivityOutcome, message: impl Into<String>) -> HistoryEntry {
        HistoryEntry::new(ActivityAction::Mount, None, outcome, message)
    }

    fn mount_all_item(name: &str, target: &str) -> MountAllItem {
        MountAllItem {
            archive_path: PathBuf::from(format!("/roms/{name}.zip")),
            mount_path: PathBuf::from(format!("/mount/{target}")),
            display_name: name.to_string(),
        }
    }

    fn unmount_all_item(name: &str) -> UnmountAllItem {
        UnmountAllItem {
            archive_path: PathBuf::from(format!("/roms/{name}.zip")),
            mount_path: PathBuf::from(format!("/mount/{name}")),
            display_name: name.to_string(),
        }
    }

    #[test]
    fn fixed_row_height_matches_the_larger_rendering_constraint() {
        assert_eq!(fixed_row_height(14.0, 18.0), 18.0);
        assert_eq!(fixed_row_height(22.0, 18.0), 22.0);
    }

    #[test]
    fn table_width_uses_all_shared_columns_and_spacing() {
        let spacing = 8.0;
        let expected = COLUMN_WIDTHS.iter().sum::<f32>() + spacing * 3.0;

        assert_eq!(COLUMN_HEADERS.len(), COLUMN_WIDTHS.len());
        assert_eq!(table_width(spacing), expected);
    }

    #[test]
    fn empty_filter_uses_all_rows_without_an_index_allocation() {
        let rows = vec![row("Halo Xbox Mounted")];

        assert_eq!(matching_row_indices(&rows, "  "), None);
    }

    #[test]
    fn filter_indices_match_each_displayed_field_case_insensitively() {
        let rows = vec![
            row("/roms/Halo.zip /mnt/archivefs/Xbox/Halo Xbox Mounted"),
            row("/roms/Ridge.7z /mnt/archivefs/PSP/Ridge PSP Pending"),
        ];

        for query in ["HALO", "archivefs/xbox", "xBoX", "mounted"] {
            assert_eq!(matching_row_indices(&rows, query), Some(vec![0]));
        }
        assert_eq!(matching_row_indices(&rows, "playstation"), Some(Vec::new()));
    }

    #[test]
    fn ordinary_mount_all_render_state_uses_only_pending_count() {
        assert!(mount_all_available(500_000, false));
        assert!(!mount_all_available(0, false));
        assert!(!mount_all_available(500_000, true));
    }

    #[test]
    fn mount_all_selects_only_pending_archives() {
        let records = vec![
            record("/roms/Pending.zip", MountState::Pending),
            record("/roms/Mounted.zip", MountState::Mounted),
            record("/roms/Existing.zip", MountState::MountPathExists),
        ];

        let selected = pending_mount_items(&records);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].archive_path, PathBuf::from("/roms/Pending.zip"));
    }

    #[test]
    fn mount_all_processes_sequentially_and_continues_after_failure() {
        let items = vec![
            mount_all_item("First", "First"),
            mount_all_item("Second", "Second"),
            mount_all_item("Third", "Third"),
        ];
        let order = std::cell::RefCell::new(Vec::new());
        let events = std::cell::RefCell::new(Vec::new());
        let stop = AtomicBool::new(false);

        let result = run_mount_all_coordinator(
            items,
            &stop,
            |_| true,
            |_| Ok(()),
            |archive_path| {
                order.borrow_mut().push(archive_path.to_path_buf());
                if archive_path.ends_with("Second.zip") {
                    Err("second failed".to_string())
                } else {
                    Ok(BatchMountAttempt::Mounted(PathBuf::from("/mount/actual")))
                }
            },
            |event| events.borrow_mut().push(event),
        );

        assert_eq!(
            order.into_inner(),
            vec![
                PathBuf::from("/roms/First.zip"),
                PathBuf::from("/roms/Second.zip"),
                PathBuf::from("/roms/Third.zip"),
            ]
        );
        assert_eq!(result.attempted(), 3);
        assert_eq!(result.successful, 2);
        assert_eq!(result.failed(), 1);
        assert_eq!(result.skipped(), 0);
        assert_eq!(result.unattempted, 0);
        let events = events.into_inner();
        assert!(matches!(
            events[0],
            MountAllEvent::ArchiveStarted { index: 1, .. }
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            MountAllEvent::ArchiveFailed { item, .. }
                if item.archive_path == Path::new("/roms/Second.zip")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            MountAllEvent::ArchiveCompleted(item)
                if item.archive_path == Path::new("/roms/Third.zip")
        )));
    }

    #[test]
    fn mount_all_counts_already_mounted_missing_and_duplicate_targets_as_skipped() {
        let items = vec![
            mount_all_item("Mounted", "Mounted"),
            mount_all_item("Missing", "Missing"),
            mount_all_item("FirstDuplicate", "Shared"),
            mount_all_item("SecondDuplicate", "Shared"),
        ];
        let stop = AtomicBool::new(false);
        let mount_calls = std::cell::RefCell::new(Vec::new());

        let result = run_mount_all_coordinator(
            items,
            &stop,
            |archive_path| !archive_path.ends_with("Missing.zip"),
            |archive_path| {
                if archive_path.ends_with("SecondDuplicate.zip") {
                    Err("duplicate target after resolution".to_string())
                } else {
                    Ok(())
                }
            },
            |archive_path| {
                mount_calls.borrow_mut().push(archive_path.to_path_buf());
                if archive_path.ends_with("Mounted.zip") {
                    Ok(BatchMountAttempt::AlreadyMounted(PathBuf::from(
                        "/mount/already",
                    )))
                } else {
                    Ok(BatchMountAttempt::Mounted(PathBuf::from("/mount/actual")))
                }
            },
            |_| {},
        );

        assert_eq!(result.total, 4);
        assert_eq!(result.attempted(), 1);
        assert_eq!(result.successful, 1);
        assert_eq!(result.failed(), 0);
        assert_eq!(result.skipped(), 3);
        assert!(
            !mount_calls
                .borrow()
                .iter()
                .any(|path| path.ends_with("SecondDuplicate.zip"))
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|entry| entry.reason.contains("already mounted"))
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|entry| entry.reason.contains("disappeared"))
        );
        assert!(
            result
                .skipped
                .iter()
                .any(|entry| entry.reason.contains("duplicate target"))
        );
    }

    #[test]
    fn mount_all_stop_after_current_prevents_later_mounts() {
        let items = vec![
            mount_all_item("First", "First"),
            mount_all_item("Second", "Second"),
            mount_all_item("Third", "Third"),
        ];
        let stop = AtomicBool::new(false);
        let attempted = std::cell::Cell::new(0);

        let result = run_mount_all_coordinator(
            items,
            &stop,
            |_| true,
            |_| Ok(()),
            |_| {
                attempted.set(attempted.get() + 1);
                stop.store(true, Ordering::Release);
                Ok(BatchMountAttempt::Mounted(PathBuf::from("/mount/actual")))
            },
            |_| {},
        );

        assert_eq!(attempted.get(), 1);
        assert_eq!(result.successful, 1);
        assert_eq!(result.unattempted, 2);
        assert!(result.stopped);
    }

    #[test]
    fn mount_all_setup_failure_is_terminal_and_truthful() {
        let result = MountAllResult::setup_failed(12, "mount root is unavailable");

        assert_eq!(result.completion_message(), "Mount All could not start.");
        assert_ne!(
            result.completion_message(),
            "Mount All completed successfully."
        );
        assert_eq!(result.attempted(), 0);
        assert_eq!(result.successful, 0);
        assert_eq!(result.failed(), 0);
        assert_eq!(result.skipped(), 0);
        assert!(result.skipped.is_empty());
        assert_eq!(result.unattempted, 12);
        assert_eq!(
            result.setup_failure.as_deref(),
            Some("mount root is unavailable")
        );
    }

    #[test]
    fn mount_all_setup_failure_records_failed_activity_and_feedback() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.mount_all = Some(RunningMountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: MountAllProgress {
                total: 4,
                ..MountAllProgress::default()
            },
        });
        sender
            .send(MountAllEvent::Finished(MountAllResult::setup_failed(
                4,
                "configuration could not be loaded",
            )))
            .unwrap();

        app.poll_mount_all(&egui::Context::default());

        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("Mount All could not start"));
        assert!(
            feedback
                .message
                .contains("configuration could not be loaded")
        );
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::MountAll
                && entry.outcome == ActivityOutcome::Failed
                && entry.message.contains("configuration could not be loaded")
        }));
        let result = app.mount_all_result.as_ref().unwrap();
        assert_eq!(result.unattempted, 4);
        assert!(result.skipped.is_empty());
    }

    #[test]
    fn partial_mount_all_failure_is_not_a_total_failure() {
        let result = MountAllResult {
            total: 102,
            successful: 100,
            failures: vec![
                MountAllFailure {
                    archive_path: PathBuf::from("/roms/One.zip"),
                    message: "failed".to_string(),
                },
                MountAllFailure {
                    archive_path: PathBuf::from("/roms/Two.zip"),
                    message: "failed".to_string(),
                },
            ],
            skipped: Vec::new(),
            unattempted: 0,
            stopped: false,
            setup_failure: None,
        };

        assert_eq!(
            result.completion_message(),
            "Mount All completed with 2 failures."
        );
        assert_eq!(result.attempted(), 102);
    }

    #[test]
    fn action_availability_follows_mount_state() {
        assert_eq!(available_action(MountState::Pending), ArchiveAction::Mount);
        assert_eq!(
            available_action(MountState::MountPathExists),
            ArchiveAction::Mount
        );
        assert_eq!(
            available_action(MountState::Mounted),
            ArchiveAction::Unmount
        );
    }

    #[test]
    fn selected_record_lookup_uses_the_exact_archive_path() {
        let records = vec![
            record("/roms/Alpha.zip", MountState::Pending),
            record("/roms/Beta.7z", MountState::Mounted),
        ];

        assert_eq!(
            selected_record_index(&records, Some(Path::new("/roms/Beta.7z"))),
            Some(1)
        );
        assert_eq!(
            selected_record(&records, Some(Path::new("/roms/Beta.7z")))
                .unwrap()
                .mount_state,
            MountState::Mounted
        );
        assert!(selected_record(&records, Some(Path::new("/roms/Missing.rar"))).is_none());
        assert!(selected_record(&records, None).is_none());
    }

    #[test]
    fn mount_all_is_rejected_while_an_individual_operation_is_active() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Mount,
            archive_path: PathBuf::from("/roms/Active.zip"),
            receiver,
            progress_receiver: mpsc::channel().1,
        });

        assert!(!app.start_mount_all(
            egui::Context::default(),
            vec![mount_all_item("Pending", "Pending")],
        ));
        assert!(app.mount_all.is_none());
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::MountAll && entry.outcome == ActivityOutcome::Rejected
        }));
    }

    #[test]
    fn individual_actions_are_unavailable_during_mount_all() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.mount_all = Some(RunningMountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: MountAllProgress {
                total: 1,
                ..MountAllProgress::default()
            },
        });
        let mounted = record("/roms/Game.zip", MountState::Mounted);
        let pending = record("/roms/Pending.zip", MountState::Pending);

        assert!(app.is_busy());
        assert!(!individual_actions_available(app.is_busy()));
        assert!(!lazy_unmount_available(
            &mounted,
            &HashSet::from([PathBuf::from("/roms/Game.zip")]),
            app.is_busy(),
        ));
        assert!(!remount_available(
            &pending,
            &HashSet::from([PathBuf::from("/roms/Pending.zip")]),
            app.is_busy(),
        ));
    }

    #[test]
    fn mount_all_stop_request_is_recorded_and_signalled() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        app.mount_all = Some(RunningMountAll {
            receiver,
            stop: Arc::clone(&stop),
            progress: MountAllProgress {
                total: 3,
                ..MountAllProgress::default()
            },
        });

        app.request_mount_all_stop();

        assert!(stop.load(Ordering::Acquire));
        assert!(app.mount_all.as_ref().unwrap().progress.stop_requested);
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::MountAll
                && entry.outcome == ActivityOutcome::Cancelled
                && entry.message.contains("current archive")
        }));
    }

    #[test]
    fn mount_all_activity_records_batch_and_archive_outcomes() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        let first = mount_all_item("First", "First");
        let second = mount_all_item("Second", "Second");
        let third = mount_all_item("Third", "Third");
        app.history.record(HistoryEntry::new(
            ActivityAction::MountAll,
            None,
            ActivityOutcome::Started,
            "Mount All started.",
        ));
        app.mount_all = Some(RunningMountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: MountAllProgress {
                total: 3,
                ..MountAllProgress::default()
            },
        });
        sender
            .send(MountAllEvent::ArchiveStarted {
                index: 1,
                total: 3,
                item: first.clone(),
            })
            .unwrap();
        sender.send(MountAllEvent::ArchiveCompleted(first)).unwrap();
        sender
            .send(MountAllEvent::ArchiveFailed {
                item: second,
                message: "mount failed".to_string(),
            })
            .unwrap();
        sender
            .send(MountAllEvent::ArchiveSkipped {
                item: third,
                reason: "archive disappeared".to_string(),
            })
            .unwrap();
        sender
            .send(MountAllEvent::Finished(MountAllResult {
                total: 3,
                successful: 1,
                failures: vec![MountAllFailure {
                    archive_path: PathBuf::from("/roms/Second.zip"),
                    message: "mount failed".to_string(),
                }],
                skipped: vec![MountAllSkipped {
                    archive_path: PathBuf::from("/roms/Third.zip"),
                    reason: "archive disappeared".to_string(),
                }],
                unattempted: 0,
                stopped: false,
                setup_failure: None,
            }))
            .unwrap();

        app.poll_mount_all(&egui::Context::default());

        assert!(app.mount_all.is_none());
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::MountAll && entry.outcome == ActivityOutcome::Completed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Mount && entry.outcome == ActivityOutcome::Completed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Mount && entry.outcome == ActivityOutcome::Failed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Mount && entry.outcome == ActivityOutcome::Skipped
        }));
        assert_eq!(
            app.feedback.as_ref().unwrap().message,
            "Mount All completed with 1 failure."
        );
    }

    #[test]
    fn start_operation_rejects_a_second_operation_without_replacing_the_receiver() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Mount,
            archive_path: PathBuf::from("/roms/Alpha.zip"),
            receiver,
            progress_receiver: mpsc::channel().1,
        });

        assert!(!app.start_operation(
            egui::Context::default(),
            ArchiveAction::Unmount,
            PathBuf::from("/roms/Beta.7z"),
            true,
        ));
        assert_eq!(app.operation.as_ref().unwrap().action, ArchiveAction::Mount);

        sender
            .send(Ok(OperationSuccess {
                message: "original result".to_string(),
                cleanup: None,
                warning: None,
            }))
            .unwrap();
        let result = app
            .operation
            .as_ref()
            .unwrap()
            .receiver
            .try_recv()
            .unwrap()
            .unwrap();
        assert_eq!(result.message, "original result");
        assert!(result.cleanup.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("already running"));
        let rejected = app.history.entries().next().unwrap();
        assert_eq!(rejected.outcome, ActivityOutcome::Rejected);
        assert_eq!(rejected.action, ActivityAction::Unmount);
        assert_eq!(
            rejected.archive_path.as_deref(),
            Some(Path::new("/roms/Beta.7z"))
        );
        assert!(rejected.message.contains("already running"));
    }

    #[test]
    fn starting_an_operation_clears_pending_unmount_confirmation() {
        let mut app = app_for_operation_tests();
        app.confirm_unmount = Some(PathBuf::from("/roms/Alpha.zip"));

        assert!(app.start_operation_with_worker(
            egui::Context::default(),
            ArchiveAction::Mount,
            PathBuf::from("/roms/Beta.7z"),
            false,
            |_, _, _, _| {
                Ok(OperationSuccess {
                    message: "mounted".to_string(),
                    cleanup: None,
                    warning: None,
                })
            },
        ));
        assert!(app.confirm_unmount.is_none());
        assert!(app.operation.is_some());
    }

    #[test]
    fn unmount_confirmation_actions_are_unavailable_while_busy() {
        assert!(confirmation_actions_available(false));
        assert!(!confirmation_actions_available(true));
    }

    #[test]
    fn history_keeps_newest_entries_first() {
        let mut history = OperationHistory::default();
        history.record(history_entry(ActivityOutcome::Started, "first"));
        history.record(history_entry(ActivityOutcome::Completed, "second"));

        let messages = history
            .entries()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["second", "first"]);
    }

    #[test]
    fn history_is_capped_at_fifty_entries() {
        let mut history = OperationHistory::default();
        for index in 0..60 {
            history.record(history_entry(ActivityOutcome::Started, index.to_string()));
        }

        assert_eq!(history.entries.len(), HISTORY_LIMIT);
        assert_eq!(history.entries.front().unwrap().message, "59");
        assert_eq!(history.entries.back().unwrap().message, "10");
    }

    #[test]
    fn clearing_history_removes_every_entry() {
        let mut history = OperationHistory::default();
        history.record(history_entry(ActivityOutcome::Started, "one"));
        history.record(history_entry(ActivityOutcome::Completed, "two"));

        history.clear();

        assert!(history.entries.is_empty());
    }

    #[test]
    fn history_preserves_success_and_failure_messages() {
        let mut history = OperationHistory::default();
        history.record(history_entry(
            ActivityOutcome::Completed,
            "mounted successfully",
        ));
        history.record(history_entry(
            ActivityOutcome::Failed,
            "ratarmount returned an error",
        ));

        let entries = history.entries().collect::<Vec<_>>();
        assert_eq!(entries[0].outcome, ActivityOutcome::Failed);
        assert_eq!(entries[0].message, "ratarmount returned an error");
        assert_eq!(entries[1].outcome, ActivityOutcome::Completed);
        assert_eq!(entries[1].message, "mounted successfully");
    }

    #[test]
    fn cleanup_is_skipped_when_the_option_is_off() {
        let cleanup_called = std::cell::Cell::new(false);
        let cleanup_started = std::cell::Cell::new(false);
        let success = run_unmount_with_cleanup(
            false,
            || Ok(("unmounted".to_string(), PathBuf::from("/mount/Game"))),
            |_| {
                cleanup_called.set(true);
                Ok(Vec::new())
            },
            |_| cleanup_started.set(true),
        )
        .unwrap();

        assert!(!cleanup_started.get());
        assert!(!cleanup_called.get());
        assert!(success.cleanup.is_none());
    }

    #[test]
    fn cleanup_runs_after_a_successful_unmount_when_enabled() {
        let cleanup_called = std::cell::Cell::new(false);
        let cleanup_started = std::cell::Cell::new(false);
        let success = run_unmount_with_cleanup(
            true,
            || Ok(("unmounted".to_string(), PathBuf::from("/mount/Game"))),
            |mount_path| {
                assert!(cleanup_started.get());
                cleanup_called.set(true);
                assert_eq!(mount_path, Path::new("/mount/Game"));
                Ok(vec![mount_path.to_path_buf()])
            },
            |_| cleanup_started.set(true),
        )
        .unwrap();

        assert!(cleanup_started.get());
        assert!(cleanup_called.get());
        assert!(matches!(
            success.cleanup,
            Some(CleanupOutcome::Completed { .. })
        ));
    }

    #[test]
    fn cleanup_does_not_run_after_a_failed_unmount() {
        let cleanup_called = std::cell::Cell::new(false);
        let cleanup_started = std::cell::Cell::new(false);
        let result = run_unmount_with_cleanup(
            true,
            || {
                Err(OperationFailure {
                    message: "unmount failed".to_string(),
                    offer_lazy_unmount: true,
                })
            },
            |_| {
                cleanup_called.set(true);
                Ok(Vec::new())
            },
            |_| cleanup_started.set(true),
        );

        assert_eq!(result.unwrap_err().message, "unmount failed");
        assert!(!cleanup_started.get());
        assert!(!cleanup_called.get());
    }

    #[test]
    fn cleanup_failure_preserves_successful_unmount_outcome() {
        let success = run_unmount_with_cleanup(
            true,
            || {
                Ok((
                    "unmounted successfully".to_string(),
                    PathBuf::from("/mount/Game"),
                ))
            },
            |_| Err("directory is busy".to_string()),
            |_| {},
        )
        .unwrap();

        assert_eq!(success.message, "unmounted successfully");
        let Some(CleanupOutcome::Failed { message, .. }) = success.cleanup else {
            panic!("expected a separate cleanup failure");
        };
        assert!(message.contains("directory is busy"));
    }

    #[test]
    fn cleanup_started_progress_is_recorded_before_the_final_result() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let mount_path = PathBuf::from("/mount/Game");
        let (result_sender, result_receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Unmount,
            archive_path: archive_path.clone(),
            receiver: result_receiver,
            progress_receiver,
        });

        progress_sender
            .send(OperationProgress::CleanupStarted(mount_path.clone()))
            .unwrap();
        app.poll_operation(&egui::Context::default());

        assert!(app.operation.is_some());
        let latest = app.history.entries().next().unwrap();
        assert_eq!(latest.action, ActivityAction::Cleanup);
        assert_eq!(latest.outcome, ActivityOutcome::Started);
        assert_eq!(latest.archive_path.as_deref(), Some(mount_path.as_path()));
        assert!(!app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup
                && matches!(
                    entry.outcome,
                    ActivityOutcome::Completed | ActivityOutcome::Failed
                )
        }));

        result_sender
            .send(Ok(OperationSuccess {
                message: "unmounted".to_string(),
                cleanup: Some(CleanupOutcome::Completed {
                    mount_path: mount_path.clone(),
                    message: "cleanup completed".to_string(),
                }),
                warning: None,
            }))
            .unwrap();
        app.poll_operation(&egui::Context::default());

        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup
                && entry.outcome == ActivityOutcome::Completed
                && entry.archive_path.as_deref() == Some(mount_path.as_path())
        }));
    }

    #[test]
    fn cleanup_progress_is_not_lost_when_the_final_result_is_already_ready() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let mount_path = PathBuf::from("/mount/Game");
        let (result_sender, result_receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Unmount,
            archive_path,
            receiver: result_receiver,
            progress_receiver,
        });

        progress_sender
            .send(OperationProgress::CleanupStarted(mount_path.clone()))
            .unwrap();
        result_sender
            .send(Ok(OperationSuccess {
                message: "unmounted".to_string(),
                cleanup: Some(CleanupOutcome::Completed {
                    mount_path: mount_path.clone(),
                    message: "cleanup completed".to_string(),
                }),
                warning: None,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup
                && entry.outcome == ActivityOutcome::Started
                && entry.archive_path.as_deref() == Some(mount_path.as_path())
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup
                && entry.outcome == ActivityOutcome::Completed
                && entry.archive_path.as_deref() == Some(mount_path.as_path())
        }));
    }

    #[test]
    fn activity_records_cleanup_success_and_failure_with_mount_paths() {
        let mount_path = PathBuf::from("/mount/Platform/Game");
        let mut history = OperationHistory::default();
        record_cleanup_started_activity(&mut history, &mount_path);
        record_cleanup_finished_activity(
            &mut history,
            &CleanupOutcome::Completed {
                mount_path: mount_path.clone(),
                message: "cleanup succeeded".to_string(),
            },
        );
        record_cleanup_started_activity(&mut history, &mount_path);
        record_cleanup_finished_activity(
            &mut history,
            &CleanupOutcome::Failed {
                mount_path: mount_path.clone(),
                message: "cleanup failed".to_string(),
            },
        );

        let entries = history.entries().collect::<Vec<_>>();
        assert_eq!(entries[0].action, ActivityAction::Cleanup);
        assert_eq!(entries[0].outcome, ActivityOutcome::Failed);
        assert_eq!(
            entries[0].archive_path.as_deref(),
            Some(mount_path.as_path())
        );
        assert_eq!(entries[0].message, "cleanup failed");
        assert_eq!(entries[2].outcome, ActivityOutcome::Completed);
        assert_eq!(entries[2].message, "cleanup succeeded");
        assert!(
            entries[1]
                .message
                .contains(&mount_path.display().to_string())
        );
        assert!(
            entries[3]
                .message
                .contains(&mount_path.display().to_string())
        );
    }

    #[test]
    fn lazy_unmount_is_unavailable_before_normal_unmount_failure() {
        let mounted = record("/roms/Game.zip", MountState::Mounted);

        assert!(!lazy_unmount_available(&mounted, &HashSet::new(), false));
        assert!(!lazy_unmount_available(
            &mounted,
            &HashSet::from([PathBuf::from("/roms/Other.zip")]),
            false
        ));
        assert!(lazy_unmount_available(
            &mounted,
            &HashSet::from([PathBuf::from("/roms/Game.zip")]),
            false
        ));
    }

    #[test]
    fn lazy_unmount_requires_matching_confirmation_and_is_blocked_while_busy() {
        let archive = Path::new("/roms/Game.zip");

        assert!(!lazy_confirmation_available(
            archive,
            &HashSet::new(),
            false
        ));
        assert!(!lazy_confirmation_available(
            archive,
            &HashSet::from([PathBuf::from("/roms/Other.zip")]),
            false
        ));
        let offered = HashSet::from([archive.to_path_buf()]);
        assert!(lazy_confirmation_available(archive, &offered, false));
        assert!(!lazy_confirmation_available(archive, &offered, true));
    }

    #[test]
    fn remount_is_available_only_for_the_successfully_unmounted_archive() {
        let pending = record("/roms/Game.zip", MountState::Pending);
        let mounted = record("/roms/Game.zip", MountState::Mounted);
        let no_offers = HashSet::new();
        let other_offer = HashSet::from([PathBuf::from("/roms/Other.zip")]);
        let offer = HashSet::from([PathBuf::from("/roms/Game.zip")]);

        assert!(!remount_available(&pending, &no_offers, false));
        assert!(!remount_available(&pending, &other_offer, false));
        assert!(remount_available(&pending, &offer, false));
        assert!(!remount_available(&mounted, &offer, false));
        assert!(!remount_available(&pending, &offer, true));
        assert!(remount_is_offered(&pending, &offer));
    }

    #[test]
    fn normal_unmount_failure_offers_lazy_recovery_and_records_activity() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let (sender, receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Unmount,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Err(OperationFailure {
                message: "mount is busy".to_string(),
                offer_lazy_unmount: true,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.lazy_unmount_offers.contains(&archive_path));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Unmount
                && entry.outcome == ActivityOutcome::Failed
                && entry.message.contains("busy")
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::LazyUnmount && entry.outcome == ActivityOutcome::Offered
        }));
        let feedback = app.feedback.as_ref().unwrap();
        assert_eq!(feedback.message, NORMAL_UNMOUNT_FAILURE_SUMMARY);
        assert!(
            feedback
                .more_information
                .as_deref()
                .unwrap()
                .contains("Try Normal Unmount again")
        );
    }

    #[test]
    fn successful_lazy_unmount_with_cleanup_failure_still_offers_remount() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let mount_path = PathBuf::from("/mount/Game");
        let (sender, receiver) = mpsc::channel();
        app.lazy_unmount_offers.insert(archive_path.clone());
        app.operation = Some(RunningOperation {
            action: ArchiveAction::LazyUnmount,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Ok(OperationSuccess {
                message: "lazy unmount completed".to_string(),
                cleanup: Some(CleanupOutcome::Failed {
                    mount_path,
                    message: "cleanup failed".to_string(),
                }),
                warning: Some("lazy warning".to_string()),
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.remount_offers.contains(&archive_path));
        assert!(!app.lazy_unmount_offers.contains(&archive_path));
        assert!(app.feedback.as_ref().unwrap().succeeded);
        assert!(
            !app.feedback
                .as_ref()
                .unwrap()
                .cleanup
                .as_ref()
                .unwrap()
                .succeeded
        );
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::LazyUnmount
                && entry.outcome == ActivityOutcome::Completed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup && entry.outcome == ActivityOutcome::Failed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Remount && entry.outcome == ActivityOutcome::Offered
        }));
    }

    #[test]
    fn successful_remount_clears_offer_and_records_completion() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let other_archive = PathBuf::from("/roms/Other.zip");
        let (sender, receiver) = mpsc::channel();
        app.remount_offers.insert(archive_path.clone());
        app.remount_offers.insert(other_archive.clone());
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Remount,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Ok(OperationSuccess {
                message: "remounted".to_string(),
                cleanup: None,
                warning: None,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(!app.remount_offers.contains(&archive_path));
        assert!(app.remount_offers.contains(&other_archive));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Remount && entry.outcome == ActivityOutcome::Completed
        }));
    }

    #[test]
    fn successful_normal_unmount_offers_remount() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let (sender, receiver) = mpsc::channel();
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Unmount,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Ok(OperationSuccess {
                message: "unmounted".to_string(),
                cleanup: None,
                warning: None,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.remount_offers.contains(&archive_path));
    }

    #[test]
    fn failed_remount_preserves_offer_and_records_failure() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/Game.zip");
        let (sender, receiver) = mpsc::channel();
        app.remount_offers.insert(archive_path.clone());
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Remount,
            archive_path: archive_path.clone(),
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Err(OperationFailure {
                message: "mount path is still active".to_string(),
                offer_lazy_unmount: false,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.remount_offers.contains(&archive_path));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Remount
                && entry.outcome == ActivityOutcome::Failed
                && entry.message.contains("still active")
        }));
    }

    #[test]
    fn mounting_another_archive_preserves_existing_remount_offer() {
        let mut app = app_for_operation_tests();
        let offered_archive = PathBuf::from("/roms/Game.zip");
        let mounted_archive = PathBuf::from("/roms/Other.zip");
        let (sender, receiver) = mpsc::channel();
        app.remount_offers.insert(offered_archive.clone());
        app.operation = Some(RunningOperation {
            action: ArchiveAction::Mount,
            archive_path: mounted_archive,
            receiver,
            progress_receiver: mpsc::channel().1,
        });
        sender
            .send(Ok(OperationSuccess {
                message: "mounted".to_string(),
                cleanup: None,
                warning: None,
            }))
            .unwrap();

        app.poll_operation(&egui::Context::default());

        assert!(app.remount_offers.contains(&offered_archive));
    }

    #[test]
    fn recovery_wording_is_explicit_and_avoids_aggressive_terms() {
        let wording = format!(
            "{NORMAL_UNMOUNT_FAILURE_SUMMARY}\n{NORMAL_UNMOUNT_RECOVERY_GUIDANCE}\n{LAZY_UNMOUNT_WARNING}\n{LAZY_UNMOUNT_SUCCESS}\n{REMOUNT_GUIDANCE}"
        );

        assert!(wording.contains("not responding correctly"));
        assert!(wording.contains("still has files open"));
        assert!(wording.contains("Normal Unmount repeatedly fails"));
        for avoided in ["wedged", "force kill", "nuke"] {
            assert!(!wording.to_lowercase().contains(avoided));
        }
    }

    #[test]
    fn lazy_unmount_advances_to_a_separate_final_confirmation() {
        let archive = PathBuf::from("/roms/Game.zip");
        let mut warning_confirmation = Some(archive.clone());
        let mut final_confirmation = None;
        let mut focus_final_cancel = false;

        advance_to_final_lazy_confirmation(
            &mut warning_confirmation,
            &mut final_confirmation,
            &mut focus_final_cancel,
            &archive,
        );

        assert!(warning_confirmation.is_none());
        assert_eq!(final_confirmation.as_deref(), Some(archive.as_path()));
        assert!(focus_final_cancel);
    }

    #[test]
    fn recovery_activity_records_cancel_retry_and_confirmation() {
        let archive = Path::new("/roms/Game.zip");
        let mut history = OperationHistory::default();
        record_recovery_activity(
            &mut history,
            ActivityAction::LazyUnmount,
            archive,
            ActivityOutcome::Cancelled,
            "User cancelled lazy unmount.",
        );
        record_recovery_activity(
            &mut history,
            ActivityAction::Unmount,
            archive,
            ActivityOutcome::Retried,
            "Normal unmount retried.",
        );
        record_recovery_activity(
            &mut history,
            ActivityAction::LazyUnmount,
            archive,
            ActivityOutcome::Confirmed,
            "Lazy unmount confirmed.",
        );

        let entries = history.entries().collect::<Vec<_>>();
        assert_eq!(entries[0].outcome, ActivityOutcome::Confirmed);
        assert_eq!(entries[1].outcome, ActivityOutcome::Retried);
        assert_eq!(entries[2].outcome, ActivityOutcome::Cancelled);
        assert!(entries.iter().all(|entry| {
            entry.archive_path.as_deref() == Some(archive) && !entry.message.trim().is_empty()
        }));
    }

    #[test]
    fn unmount_all_selects_only_mounted_archives() {
        let records = vec![
            record("/roms/Mounted.zip", MountState::Mounted),
            record("/roms/Pending.zip", MountState::Pending),
            record("/roms/Existing.zip", MountState::MountPathExists),
        ];

        let selected = pending_unmount_items(&records);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].archive_path, PathBuf::from("/roms/Mounted.zip"));
    }

    #[test]
    fn unmount_all_is_sequential_continues_and_keeps_cleanup_failure_separate() {
        let items = vec![
            unmount_all_item("One"),
            unmount_all_item("Two"),
            unmount_all_item("Three"),
        ];
        let stop = AtomicBool::new(false);
        let mut order = Vec::new();
        let mut events = Vec::new();

        let result = run_unmount_all_coordinator(
            items,
            &stop,
            |item| {
                order.push(item.display_name.clone());
                match item.display_name.as_str() {
                    "One" => Ok(BatchUnmountAttempt::Unmounted),
                    "Two" => Err(BatchUnmountError {
                        message: "mount is busy".to_string(),
                        offer_lazy_unmount: true,
                    }),
                    _ => Ok(BatchUnmountAttempt::NotMounted),
                }
            },
            |item, publish| {
                (item.display_name == "One").then(|| {
                    publish(UnmountAllEvent::CleanupStarted(item.mount_path.clone()));
                    Err("directory remained".to_string())
                })
            },
            |event| events.push(event),
        );

        assert_eq!(order, ["One", "Two", "Three"]);
        assert_eq!(result.attempted(), 2);
        assert_eq!(result.successful, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.cleanup_successes, 0);
        assert_eq!(result.cleanup_failures.len(), 1);
        assert!(result.completion_message().contains("1 failure"));
        let completed_index = events
            .iter()
            .position(|event| matches!(event, UnmountAllEvent::ArchiveCompleted(_)))
            .unwrap();
        let cleanup_index = events
            .iter()
            .position(|event| matches!(event, UnmountAllEvent::CleanupStarted(_)))
            .unwrap();
        assert!(completed_index < cleanup_index);
    }

    #[test]
    fn unmount_all_stop_after_current_leaves_later_items_unattempted() {
        let items = vec![
            unmount_all_item("One"),
            unmount_all_item("Two"),
            unmount_all_item("Three"),
        ];
        let stop = AtomicBool::new(false);
        let result = run_unmount_all_coordinator(
            items,
            &stop,
            |_| {
                stop.store(true, Ordering::Release);
                Ok(BatchUnmountAttempt::Unmounted)
            },
            |_, _| None,
            |_| {},
        );

        assert!(result.stopped);
        assert_eq!(result.successful, 1);
        assert_eq!(result.unattempted, 2);
    }

    #[test]
    fn unmount_all_setup_failure_is_terminal_and_truthful() {
        let result = UnmountAllResult::setup_failed(7, "mountinfo unavailable");

        assert_eq!(result.completion_message(), "Unmount All could not start.");
        assert_eq!(result.attempted(), 0);
        assert_eq!(result.successful, 0);
        assert!(result.failures.is_empty());
        assert!(result.skipped.is_empty());
        assert_eq!(result.unattempted, 7);

        let cleanup_only_failure = UnmountAllResult {
            total: 1,
            successful: 1,
            cleanup_failures: vec![UnmountAllCleanupFailure {
                mount_path: PathBuf::from("/mount/Game"),
                message: "directory remained".to_string(),
            }],
            ..Default::default()
        };
        assert_eq!(
            cleanup_only_failure.completion_message(),
            "Unmount All completed, but cleanup failed for 1 mount."
        );
    }

    #[test]
    fn unmount_all_marks_the_app_busy_and_blocks_individual_actions() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: UnmountAllProgress::default(),
        });

        assert!(app.is_busy());
        assert!(!individual_actions_available(app.is_busy()));
    }

    #[test]
    fn unmount_all_activity_records_batch_archive_cleanup_and_recovery_lifecycle() {
        let mut app = app_for_operation_tests();
        let item = unmount_all_item("Game");
        let failed = unmount_all_item("Busy");
        let (sender, receiver) = mpsc::channel();
        app.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: UnmountAllProgress {
                total: 2,
                ..Default::default()
            },
        });
        sender
            .send(UnmountAllEvent::ArchiveStarted {
                index: 1,
                total: 2,
                item: item.clone(),
            })
            .unwrap();
        sender
            .send(UnmountAllEvent::ArchiveCompleted(item.clone()))
            .unwrap();
        sender
            .send(UnmountAllEvent::CleanupStarted(item.mount_path.clone()))
            .unwrap();
        sender
            .send(UnmountAllEvent::CleanupCompleted(item.mount_path.clone()))
            .unwrap();
        sender
            .send(UnmountAllEvent::ArchiveFailed {
                item: failed.clone(),
                message: "mount is busy".to_string(),
                offer_lazy_unmount: true,
            })
            .unwrap();
        sender
            .send(UnmountAllEvent::Finished(UnmountAllResult {
                total: 2,
                successful: 1,
                failures: vec![UnmountAllFailure {
                    archive_path: failed.archive_path.clone(),
                    message: "mount is busy".to_string(),
                    offer_lazy_unmount: true,
                }],
                cleanup_successes: 1,
                ..Default::default()
            }))
            .unwrap();

        app.poll_unmount_all(&egui::Context::default());

        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Unmount && entry.outcome == ActivityOutcome::Started
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Cleanup && entry.outcome == ActivityOutcome::Completed
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::Unmount
                && entry.outcome == ActivityOutcome::Failed
                && entry.message.contains("busy")
        }));
        assert!(app.history.entries().any(|entry| {
            entry.action == ActivityAction::UnmountAll
                && entry.outcome == ActivityOutcome::Completed
        }));
        assert!(app.lazy_unmount_offers.contains(&failed.archive_path));
    }
}
