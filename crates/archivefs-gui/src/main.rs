use std::collections::{HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::{
    ArchiveKind, ArchiveRecord, ArchiveSnapshot, ArchiveStats, ArchiveStatus, Config, DoctorReport,
    DoctorStatus, LazyUnmountCleanupResult, MountState, cleanup_selected_mount_tree,
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
    feedback: Option<ActionFeedback>,
    confirm_unmount: Option<PathBuf>,
    confirm_lazy_unmount: Option<PathBuf>,
    confirm_lazy_unmount_final: Option<PathBuf>,
    focus_lazy_cancel: bool,
    focus_final_lazy_cancel: bool,
    lazy_unmount_offer: Option<PathBuf>,
    remount_offers: HashSet<PathBuf>,
    history: OperationHistory,
    cleanup_after_unmount: bool,
}

impl ArchiveFsApp {
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
            feedback: None,
            confirm_unmount: None,
            confirm_lazy_unmount: None,
            confirm_lazy_unmount_final: None,
            focus_lazy_cancel: false,
            focus_final_lazy_cancel: false,
            lazy_unmount_offer: None,
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
        if self.operation.is_some() {
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
                            self.lazy_unmount_offer = None;
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
                        self.lazy_unmount_offer = Some(archive_path.clone());
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
        if matches!(self.state, LoadState::Loading(_)) || self.operation.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ArchiveFS");
                ui.separator();
                ui.label("Library overview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let loading = matches!(self.state, LoadState::Loading(_));
                    let busy = loading || self.operation.is_some();
                    if ui
                        .add_enabled(!busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh(context);
                    }
                    if busy {
                        ui.spinner();
                    }
                });
            });
        });
        show_activity_panel(context, &mut self.history);

        let mut retry = false;
        let mut requested_action = None;
        egui::CentralPanel::default().show(context, |ui| match &self.state {
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
                        feedback: self.feedback.as_ref(),
                        confirm_unmount: &mut self.confirm_unmount,
                        confirm_lazy_unmount: &mut self.confirm_lazy_unmount,
                        confirm_lazy_unmount_final: &mut self.confirm_lazy_unmount_final,
                        focus_lazy_cancel: &mut self.focus_lazy_cancel,
                        focus_final_lazy_cancel: &mut self.focus_final_lazy_cancel,
                        lazy_unmount_offer: self.lazy_unmount_offer.as_deref(),
                        remount_offers: &self.remount_offers,
                        cleanup_after_unmount: &mut self.cleanup_after_unmount,
                        history: &mut self.history,
                    },
                )
            }
        });
        if retry {
            self.refresh(context);
        }
        if let Some(request) = requested_action {
            self.start_operation(
                context.clone(),
                request.action,
                request.archive_path,
                request.cleanup_after_unmount,
            );
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
    feedback: Option<&'a ActionFeedback>,
    confirm_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount_final: &'a mut Option<PathBuf>,
    focus_lazy_cancel: &'a mut bool,
    focus_final_lazy_cancel: &'a mut bool,
    lazy_unmount_offer: Option<&'a Path>,
    remount_offers: &'a HashSet<PathBuf>,
    cleanup_after_unmount: &'a mut bool,
    history: &'a mut OperationHistory,
}

fn show_loaded_data(
    ui: &mut egui::Ui,
    data: &LoadedData,
    view_state: LoadedViewState<'_>,
) -> Option<OperationRequest> {
    let LoadedViewState {
        filter,
        filtered_rows,
        selected_archive,
        operation,
        feedback,
        confirm_unmount,
        confirm_lazy_unmount,
        confirm_lazy_unmount_final,
        focus_lazy_cancel,
        focus_final_lazy_cancel,
        lazy_unmount_offer,
        remount_offers,
        cleanup_after_unmount,
        history,
    } = view_state;
    let mut requested_action = None;
    ui.horizontal_wrapped(|ui| {
        summary_value(ui, "Total archives", data.stats.total_archives);
        summary_value(ui, "Mounted", data.stats.mounted_count);
        summary_value(ui, "Pending", data.stats.pending_count);
        ui.separator();
        let (readiness, color) = if data.doctor.is_ready() {
            ("Ready", ui.visuals().selection.bg_fill)
        } else {
            ("Needs attention", ui.visuals().error_fg_color)
        };
        ui.label("Doctor:");
        ui.colored_label(color, readiness);
    });

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
    if let Some(request) = show_selected_archive(
        ui,
        selected_record(&data.records, selected_archive.as_deref()),
        SelectedArchiveViewState {
            operation,
            confirm_unmount,
            confirm_lazy_unmount,
            focus_lazy_cancel,
            lazy_unmount_offer,
            remount_offers,
            cleanup_after_unmount,
        },
    ) {
        requested_action = Some(request);
    }

    if let Some(archive_path) = confirm_lazy_unmount.clone() {
        let actions_available =
            lazy_confirmation_available(&archive_path, lazy_unmount_offer, operation.is_some());
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
                        requested_action = Some(OperationRequest {
                            action: ArchiveAction::Unmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        });
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
            lazy_confirmation_available(&archive_path, lazy_unmount_offer, operation.is_some());
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
                        requested_action = Some(OperationRequest {
                            action: ArchiveAction::LazyUnmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        });
                        *confirm_lazy_unmount_final = None;
                    }
                });
            });
    }

    if let Some(archive_path) = confirm_unmount.clone() {
        let actions_available = confirmation_actions_available(operation);
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
                        requested_action = Some(OperationRequest {
                            action: ArchiveAction::Unmount,
                            archive_path: archive_path.clone(),
                            cleanup_after_unmount: *cleanup_after_unmount,
                        });
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

fn confirmation_actions_available(operation: Option<&RunningOperation>) -> bool {
    operation.is_none()
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
    offered_archive: Option<&Path>,
    busy: bool,
) -> bool {
    !busy && offered_archive == Some(confirmed_archive)
}

fn lazy_unmount_available(
    record: &ArchiveRecord,
    offered_archive: Option<&Path>,
    busy: bool,
) -> bool {
    !busy
        && record.mount_state == MountState::Mounted
        && offered_archive == Some(record.mount_plan.archive.path.as_path())
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
    confirm_unmount: &'a mut Option<PathBuf>,
    confirm_lazy_unmount: &'a mut Option<PathBuf>,
    focus_lazy_cancel: &'a mut bool,
    lazy_unmount_offer: Option<&'a Path>,
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
        confirm_unmount,
        confirm_lazy_unmount,
        focus_lazy_cancel,
        lazy_unmount_offer,
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
        let busy = operation.is_some();
        let can_lazy_unmount = lazy_unmount_available(record, lazy_unmount_offer, busy);
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
            ArchiveAction::Mount | ArchiveAction::Unmount | ArchiveAction::LazyUnmount => !busy,
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
            feedback: None,
            confirm_unmount: None,
            confirm_lazy_unmount: None,
            confirm_lazy_unmount_final: None,
            focus_lazy_cancel: false,
            focus_final_lazy_cancel: false,
            lazy_unmount_offer: None,
            remount_offers: HashSet::new(),
            history: OperationHistory::default(),
            cleanup_after_unmount: false,
        }
    }

    fn history_entry(outcome: ActivityOutcome, message: impl Into<String>) -> HistoryEntry {
        HistoryEntry::new(ActivityAction::Mount, None, outcome, message)
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
        let (_sender, receiver) = mpsc::channel();
        let operation = RunningOperation {
            action: ArchiveAction::Mount,
            archive_path: PathBuf::from("/roms/Alpha.zip"),
            receiver,
            progress_receiver: mpsc::channel().1,
        };

        assert!(confirmation_actions_available(None));
        assert!(!confirmation_actions_available(Some(&operation)));
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

        assert!(!lazy_unmount_available(&mounted, None, false));
        assert!(!lazy_unmount_available(
            &mounted,
            Some(Path::new("/roms/Other.zip")),
            false
        ));
        assert!(lazy_unmount_available(
            &mounted,
            Some(Path::new("/roms/Game.zip")),
            false
        ));
    }

    #[test]
    fn lazy_unmount_requires_matching_confirmation_and_is_blocked_while_busy() {
        let archive = Path::new("/roms/Game.zip");

        assert!(!lazy_confirmation_available(archive, None, false));
        assert!(!lazy_confirmation_available(
            archive,
            Some(Path::new("/roms/Other.zip")),
            false
        ));
        assert!(lazy_confirmation_available(archive, Some(archive), false));
        assert!(!lazy_confirmation_available(archive, Some(archive), true));
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

        assert_eq!(
            app.lazy_unmount_offer.as_deref(),
            Some(archive_path.as_path())
        );
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
        app.lazy_unmount_offer = Some(archive_path.clone());
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
        assert!(app.lazy_unmount_offer.is_none());
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
}
