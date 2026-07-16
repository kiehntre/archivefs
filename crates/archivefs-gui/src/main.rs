use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::{
    ArchiveFsError, ArchiveKind, ArchiveMountSession, ArchiveRecord, ArchiveSnapshot, ArchiveStats,
    ArchiveStatus, ArchiveUnmountSession, BulkPlatformAssignmentResult, CUSTOM_FOLDER_ALIAS_SOURCE,
    CatalogueDuplicateArchive, CatalogueDuplicateGroup, CatalogueDuplicateReport, CatalogueStats,
    CompletedScanSummary, Config, ConfigIdentity, Database, DatabaseHealth, DoctorReport,
    DoctorStatus, LazyUnmountCleanupResult, MANUAL_PLATFORM_SOURCE, MissingArchiveRemovalResult,
    MountOneOutcome, MountState, PersistedArchive, PlatformAlias, PlatformAssignmentChange,
    PlatformProvenanceDetails, ScanPersistSummary, SetupDiagnosticStatus, SetupDiagnostics,
    UnmountOneOutcome, canonical_platform_names, catalogue_filename_duplicates,
    check_database_health, cleanup_selected_mount_tree, create_configured_mount_root_default,
    create_starter_config_default, default_config_path, default_database_path,
    format_unix_timestamp_utc, latest_schema_version, lazy_unmount_one_archive_path_with_progress,
    load_read_only_snapshot_default, mount_one_archive_path,
    persisted_archive_has_unknown_platform, remount_one_archive_path,
    run_setup_diagnostics_default, scan_and_persist, unmount_one_archive_path,
};
use eframe::egui;

const COLUMN_WIDTHS: [f32; 4] = [120.0, 120.0, 440.0, 520.0];
const COLUMN_HEADERS: [&str; 4] = ["Platform", "State", "Archive path", "Mount path"];
/// A fixed, explicit `egui::Id` for the library search box - not for any
/// production behaviour, but so tests can give it real keyboard focus
/// via `Memory::request_focus` deterministically, without depending on
/// egui's position-derived auto-id (see the milestone requirement that
/// keyboard shortcuts must be ignored while this field has focus).
const SEARCH_FILTER_TEXT_EDIT_ID: &str = "archivefs_library_search_filter";
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
    Diagnostics,
    Setup,
    LibraryDatabase,
    PlatformAssignment,
    BulkPlatformAssignment,
    PlatformAliasManagement,
    CatalogueCleanup,
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
            Self::Diagnostics => "Diagnostics",
            Self::Setup => "Setup",
            Self::LibraryDatabase => "Library database",
            Self::PlatformAssignment => "Platform assignment",
            Self::BulkPlatformAssignment => "Bulk platform assignment",
            Self::PlatformAliasManagement => "Platform alias management",
            Self::CatalogueCleanup => "Catalogue cleanup",
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
    config_identity: ConfigIdentity,
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
            config_identity: snapshot.config_identity,
        }
    }
}

/// Where a displayed row's data came from - see requirement 4. Only `Live`
/// rows carry a path that `selected_record`/`selected_record_index` can
/// ever match against `LoadedData.records`, since those come from the
/// cache's `PersistedArchive.absolute_path`, never from a live
/// `ArchiveRecord` - this is what guarantees a cache-only selection can
/// never resolve to a live record and so can never expose an action
/// button (see `show_selected_archive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowOrigin {
    /// Backed by the latest coherent live snapshot. Actions are available,
    /// subject to `latest_generation_actions_safe`.
    Live,
    /// Known to the persisted catalogue, not (yet) confirmed by the live
    /// snapshot, and not marked missing by the last scan.
    CachedAwaitingValidation,
    /// Known to the persisted catalogue and marked missing
    /// (`last_verified_missing_at` set) as of the last completed scan.
    CachedMissing,
    /// Known to the persisted catalogue, not marked missing by the last
    /// scan, but its path is not reachable right now (a cheap existence
    /// check at merge time, for display only - never used to authorize
    /// mount/unmount).
    CachedUnavailable,
}

impl RowOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::CachedAwaitingValidation => "Cached: awaiting validation",
            Self::CachedMissing => "Cached: missing",
            Self::CachedUnavailable => "Cached: source unavailable",
        }
    }
}

#[derive(Clone)]
struct ArchiveRow {
    /// Exact-byte identity used for selection and reconciliation - never
    /// rendered directly, and never compared via `.display()` (see
    /// requirement 5). For a live row this is
    /// `ArchiveRecord.mount_plan.archive.path`; for a cache-only row it is
    /// `PersistedArchive.absolute_path` - the same pairing the database's
    /// own `(source_folder_id, relative_path)` uniqueness constraint
    /// already encodes.
    path: PathBuf,
    archive_path: String,
    mount_path: String,
    platform: String,
    state: String,
    search_text: String,
    origin: RowOrigin,
    unknown_platform: bool,
}

impl ArchiveRow {
    fn new(record: &ArchiveRecord, status: &ArchiveStatus) -> Self {
        let archive_path = status.archive_path.display().to_string();
        let mount_path = status.mount_path.display().to_string();
        let raw_platform = record
            .metadata
            .platform
            .as_deref()
            .or(record.identity.platform.as_deref());
        let unknown_platform = raw_platform.is_none();
        let platform = raw_platform.unwrap_or("Unknown").to_string();
        let state = status.state.to_string();
        let search_text =
            format!("{archive_path}\n{mount_path}\n{platform}\n{state}").to_lowercase();

        Self {
            path: record.mount_plan.archive.path.clone(),
            archive_path,
            mount_path,
            platform,
            state,
            search_text,
            origin: RowOrigin::Live,
            unknown_platform,
        }
    }

    /// Synthesizes a display-only row for a cache-only archive: one the
    /// persisted catalogue knows about but the latest live snapshot does
    /// not confirm. `path_exists` is a cheap, display-only existence
    /// check (never a substitute for live validation) that distinguishes
    /// "unreachable right now" from "awaiting the next live refresh".
    fn from_cached(persisted: &PersistedArchive, path_exists: bool) -> Self {
        let archive_path = persisted.absolute_path.display().to_string();
        let unknown_platform = persisted_archive_has_unknown_platform(persisted);
        let platform = persisted
            .platform
            .as_deref()
            .unwrap_or("Unknown")
            .to_string();
        let origin = if persisted.last_verified_missing_at.is_some() {
            RowOrigin::CachedMissing
        } else if !path_exists {
            RowOrigin::CachedUnavailable
        } else {
            RowOrigin::CachedAwaitingValidation
        };
        let state = origin.label().to_string();
        let mount_path = String::new();
        let search_text =
            format!("{archive_path}\n{mount_path}\n{platform}\n{state}").to_lowercase();

        Self {
            path: persisted.absolute_path.clone(),
            archive_path,
            mount_path,
            platform,
            state,
            search_text,
            origin,
            unknown_platform,
        }
    }

    /// Overrides this row's platform-derived fields (`platform`,
    /// `unknown_platform`, and the platform portion of `search_text`)
    /// with the library database's effective (manual-aware) platform for
    /// this archive.
    ///
    /// A live row built from `ArchiveRecord` alone only ever sees the
    /// live scan's own automatic detection
    /// (`record.metadata.platform`/`record.identity.platform`), which
    /// disagrees with the persisted effective platform exactly when a
    /// manual assignment is active and automatic detection found
    /// nothing. Without this override, such a row would be wrongly
    /// classified (and counted/filtered) as unknown. Only ever applied
    /// when the database already has a persisted row for this exact path
    /// (see `build_display_rows`); a live row with no persisted
    /// counterpart yet keeps its live-only classification, the only
    /// signal available for it.
    fn with_persisted_platform(mut self, persisted: &PersistedArchive) -> Self {
        self.unknown_platform = persisted_archive_has_unknown_platform(persisted);
        self.platform = persisted
            .platform
            .as_deref()
            .unwrap_or("Unknown")
            .to_string();
        self.search_text = format!(
            "{}\n{}\n{}\n{}",
            self.archive_path, self.mount_path, self.platform, self.state
        )
        .to_lowercase();
        self
    }

    fn matches(&self, normalized_filter: &str) -> bool {
        self.search_text.contains(normalized_filter)
    }

    fn row_text_color(&self, visuals: &egui::Visuals) -> Option<egui::Color32> {
        match self.origin {
            RowOrigin::Live => None,
            RowOrigin::CachedAwaitingValidation => Some(egui::Color32::from_rgb(150, 150, 150)),
            RowOrigin::CachedMissing => Some(visuals.error_fg_color),
            RowOrigin::CachedUnavailable => Some(egui::Color32::from_rgb(210, 140, 40)),
        }
    }
}

/// Merges live rows with cache-only rows for display - see requirement 4
/// and 5. Live rows always win: a cached archive whose exact path already
/// appears among `records` is represented only by its live row, never
/// duplicated - but with its platform/unknown-platform classification
/// overridden from the persisted effective value when the database
/// already has an entry for it (see `ArchiveRow::with_persisted_platform`
/// and requirement 6). Recomputed fresh whenever the underlying live or
/// cached data changes (see `ArchiveFsApp::recompute_filtered_rows`), not
/// on every frame, so it stays cheap without risking a stale merge.
fn build_display_rows(
    records: &[ArchiveRecord],
    live_rows: &[ArchiveRow],
    cached: Option<&CachedLibrarySnapshot>,
) -> Vec<ArchiveRow> {
    let persisted_by_path: HashMap<&Path, &PersistedArchive> = cached
        .map(|cached| {
            cached
                .archives
                .iter()
                .map(|persisted| (persisted.absolute_path.as_path(), persisted))
                .collect()
        })
        .unwrap_or_default();

    let mut merged: Vec<ArchiveRow> = live_rows
        .iter()
        .cloned()
        .map(|row| match persisted_by_path.get(row.path.as_path()) {
            Some(persisted) => row.with_persisted_platform(persisted),
            None => row,
        })
        .collect();

    if let Some(cached) = cached {
        let live_paths: HashSet<&Path> = records
            .iter()
            .map(|record| record.mount_plan.archive.path.as_path())
            .collect();
        for persisted in &cached.archives {
            if live_paths.contains(persisted.absolute_path.as_path()) {
                continue;
            }
            let path_exists = persisted.absolute_path.exists();
            merged.push(ArchiveRow::from_cached(persisted, path_exists));
        }
    }

    merged
}

/// Optional search filters over the merged row list (requirement 6). Two
/// independent groups - state and platform - each AND'd together; within
/// a group, an unchecked filter set imposes no restriction (defaults to
/// "show everything") and multiple checked filters within the same group
/// are OR'd, so checking both `present` and `missing` shows both rather
/// than nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LibraryRowFilters {
    present: bool,
    missing: bool,
    awaiting_validation: bool,
    known_platform: bool,
    unknown_platform: bool,
}

impl LibraryRowFilters {
    fn is_active(&self) -> bool {
        self.present
            || self.missing
            || self.awaiting_validation
            || self.known_platform
            || self.unknown_platform
    }

    fn matches(&self, row: &ArchiveRow) -> bool {
        let state_group_active = self.present || self.missing || self.awaiting_validation;
        let state_match = !state_group_active || {
            let is_present = matches!(row.origin, RowOrigin::Live);
            let is_missing = matches!(row.origin, RowOrigin::CachedMissing);
            let is_awaiting = matches!(
                row.origin,
                RowOrigin::CachedAwaitingValidation | RowOrigin::CachedUnavailable
            );
            (self.present && is_present)
                || (self.missing && is_missing)
                || (self.awaiting_validation && is_awaiting)
        };

        let platform_group_active = self.known_platform || self.unknown_platform;
        let platform_match = !platform_group_active
            || (self.known_platform && !row.unknown_platform)
            || (self.unknown_platform && row.unknown_platform);

        state_match && platform_match
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

fn set_lazy_unmount_offer(
    offers: &mut HashSet<PathBuf>,
    archive_path: &Path,
    recovery_needed: bool,
) {
    if recovery_needed {
        offers.insert(archive_path.to_path_buf());
    } else {
        offers.remove(archive_path);
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RefreshGeneration(u64);

impl RefreshGeneration {
    const INITIAL: Self = Self(0);

    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

type LoadResult = Result<LoadedData, String>;
type LoadMessage = (RefreshGeneration, LoadResult);
type DiagnosticsMessage = (RefreshGeneration, SetupDiagnostics);

enum LoadState {
    Loading {
        generation: RefreshGeneration,
        receiver: Receiver<LoadMessage>,
        previous: Option<Box<LoadedData>>,
    },
    Ready(Box<LoadedData>),
    Error(String),
}

enum DiagnosticsState {
    Loading {
        generation: RefreshGeneration,
        receiver: Receiver<DiagnosticsMessage>,
    },
    Ready {
        generation: RefreshGeneration,
        report: SetupDiagnostics,
    },
    Error {
        generation: RefreshGeneration,
        message: String,
    },
}

impl DiagnosticsState {
    fn generation(&self) -> RefreshGeneration {
        match self {
            Self::Loading { generation, .. }
            | Self::Ready { generation, .. }
            | Self::Error { generation, .. } => *generation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupAction {
    CreateStarterConfig,
    CreateMountRoot,
    OpenConfigFolder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiagnosticsUiAction {
    Refresh,
    Continue,
    ViewLastSnapshot,
    CreateStarterConfig,
    CreateMountRoot,
    OpenConfigFolder,
    CopyConfigPath,
}

fn open_diagnostics_view(show_diagnostics: &mut bool) {
    *show_diagnostics = true;
}

fn diagnostics_can_continue(report: &SetupDiagnostics) -> bool {
    report.ready_for_scanning
}

fn starter_config_available(report: &SetupDiagnostics) -> bool {
    report.config_path.is_some() && report.config_missing && report.config_path_error.is_none()
}

fn diagnostics_state_can_continue(state: &DiagnosticsState) -> bool {
    matches!(state, DiagnosticsState::Ready { report, .. } if diagnostics_can_continue(report))
}

/// Archive actions are only safe when the snapshot and diagnostics both
/// belong to the current refresh generation *and* were derived from the
/// exact same configuration contents. Matching generations alone is not
/// enough: the config file can change between the snapshot read and the
/// diagnostics read of the same generation, so identities are compared too.
fn latest_generation_actions_safe(
    current: RefreshGeneration,
    snapshot_generation: Option<RefreshGeneration>,
    snapshot_stale: bool,
    snapshot_identity: Option<&ConfigIdentity>,
    diagnostics: &DiagnosticsState,
) -> bool {
    if snapshot_generation != Some(current) || snapshot_stale || diagnostics.generation() != current
    {
        return false;
    }
    let DiagnosticsState::Ready { report, .. } = diagnostics else {
        return false;
    };
    report.ready_for_actions && snapshot_identity == Some(&report.config_identity)
}

fn snapshot_identity(state: &LoadState) -> Option<&ConfigIdentity> {
    match state {
        LoadState::Ready(data) => Some(&data.config_identity),
        LoadState::Loading { .. } | LoadState::Error(_) => None,
    }
}

// ---------------------------------------------------------------------
// Persistent library database (stage 4): a read-only, background-loaded
// cache of archivefs_core::Database that speeds up startup and browsing.
// It is deliberately a *separate* state machine from LoadState/
// DiagnosticsState above, polled the same way (its own generation
// counter, its own channel, the same stale-message double-check) - see
// docs/DATABASE_DESIGN.md section 5 and
// docs/adr/0001-persistent-library-database.md: this cache is never
// consulted to authorize a mount or unmount. Only `latest_generation_actions_safe`
// (backed by a live snapshot and live diagnostics, both unchanged by this
// stage) gates archive actions - see `build_display_rows` and
// `show_selected_archive` below for how a cache-only row is guaranteed to
// never carry a live ArchiveRecord into the action-granting code path.
// ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DatabaseGeneration(u64);

impl DatabaseGeneration {
    const INITIAL: Self = Self(0);

    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// A read-only snapshot of the persisted library catalogue: every row
/// `Database::load_archives` returned, plus aggregate stats and the most
/// recent completed scan, all read from one opened `Database` handle in
/// one background pass.
#[derive(Debug, Clone)]
struct CachedLibrarySnapshot {
    database_path: PathBuf,
    schema_version: i64,
    archives: Vec<PersistedArchive>,
    platform_details: HashMap<i64, PlatformProvenanceDetails>,
    stats: CatalogueStats,
    last_completed_scan: Option<CompletedScanSummary>,
    platform_aliases: Vec<PlatformAlias>,
    /// Computed once on the database worker whenever this snapshot is
    /// loaded. Rendering only filters/sorts these cached groups; it never
    /// reruns duplicate detection per frame.
    duplicate_report: CatalogueDuplicateReport,
}

enum DatabaseOutcome {
    Loaded(CachedLibrarySnapshot),
    Scanned {
        snapshot: CachedLibrarySnapshot,
        scan_summary: ScanPersistSummary,
    },
}

enum DatabaseLoadError {
    NotCreated { database_path: PathBuf },
    Outdated { health: DatabaseHealth },
    Failed { message: String },
}

type DatabaseLoadResult = Result<DatabaseOutcome, DatabaseLoadError>;
type DatabaseMessage = (DatabaseGeneration, DatabaseLoadResult);

/// The Library Database status area's state - see requirement 3's exact
/// vocabulary ("Not created / Loading / Ready / Outdated / Error").
enum DatabaseState {
    NotCreated {
        database_path: PathBuf,
    },
    Loading {
        generation: DatabaseGeneration,
        receiver: Receiver<DatabaseMessage>,
        previous: Option<Box<CachedLibrarySnapshot>>,
        scanning: bool,
    },
    Ready {
        snapshot: Box<CachedLibrarySnapshot>,
        last_scan_summary: Option<ScanPersistSummary>,
    },
    Outdated {
        health: DatabaseHealth,
        previous: Option<Box<CachedLibrarySnapshot>>,
    },
    Error {
        message: String,
        previous: Option<Box<CachedLibrarySnapshot>>,
    },
}

impl DatabaseState {
    /// The most recent known-good cached snapshot regardless of the
    /// current state, so a failed reload never discards useful data
    /// already on screen (requirement 7: retain the last useful database
    /// catalogue where safe).
    fn snapshot(&self) -> Option<&CachedLibrarySnapshot> {
        match self {
            Self::Ready { snapshot, .. } => Some(snapshot),
            Self::Loading { previous, .. }
            | Self::Outdated { previous, .. }
            | Self::Error { previous, .. } => previous.as_deref(),
            Self::NotCreated { .. } => None,
        }
    }

    fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    fn is_scanning(&self) -> bool {
        matches!(self, Self::Loading { scanning: true, .. })
    }

    fn status_label(&self) -> &'static str {
        match self {
            Self::NotCreated { .. } => "Not created",
            Self::Loading { .. } => "Loading",
            Self::Ready { .. } => "Ready",
            Self::Outdated { .. } => "Outdated",
            Self::Error { .. } => "Error",
        }
    }
}

fn start_database_load(
    context: egui::Context,
    generation: DatabaseGeneration,
    previous: Option<Box<CachedLibrarySnapshot>>,
    run_scan_first: bool,
) -> DatabaseState {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = load_database_snapshot(run_scan_first);
        let _ = sender.send((generation, result));
        context.request_repaint();
    });
    DatabaseState::Loading {
        generation,
        receiver,
        previous,
        scanning: run_scan_first,
    }
}

fn load_database_snapshot(run_scan_first: bool) -> DatabaseLoadResult {
    let database_path = default_database_path().map_err(|error| DatabaseLoadError::Failed {
        message: error.to_string(),
    })?;

    let scan_config = if run_scan_first {
        Some(
            Config::load_default().map_err(|error| DatabaseLoadError::Failed {
                message: error.to_string(),
            })?,
        )
    } else {
        None
    };

    load_database_snapshot_at(&database_path, scan_config.as_ref())
}

/// The logic behind [`load_database_snapshot`], taking the already-resolved
/// database path (and, for a scan, the already-loaded config) as
/// parameters instead of reading `HOME`/the default config path itself -
/// the same split `resolve_database_path` uses in
/// `archivefs-core/src/database.rs`, so tests can exercise every branch
/// against a temporary database path without touching the real home
/// directory.
fn load_database_snapshot_at(
    database_path: &Path,
    scan_config: Option<&Config>,
) -> DatabaseLoadResult {
    if let Some(config) = scan_config {
        let mut database =
            Database::open_or_create(database_path).map_err(|error| DatabaseLoadError::Failed {
                message: error.to_string(),
            })?;
        let scan_summary =
            scan_and_persist(&mut database, config, "gui-scan-library").map_err(|error| {
                DatabaseLoadError::Failed {
                    message: error.to_string(),
                }
            })?;
        let snapshot = load_snapshot_from(&database, database_path)?;
        return Ok(DatabaseOutcome::Scanned {
            snapshot,
            scan_summary,
        });
    }

    let health = check_database_health(database_path);
    if !health.database_exists {
        return Err(DatabaseLoadError::NotCreated {
            database_path: database_path.to_path_buf(),
        });
    }
    if !health.migrations_current {
        return Err(classify_unhealthy_database(health));
    }

    let database =
        Database::open_or_create(database_path).map_err(|error| DatabaseLoadError::Failed {
            message: error.to_string(),
        })?;
    let snapshot = load_snapshot_from(&database, database_path)?;
    Ok(DatabaseOutcome::Loaded(snapshot))
}

/// Turns a `DatabaseHealth` that is not `migrations_current` into the
/// right `DatabaseLoadError` (requirement 7): a database that will not
/// even open is a hard `Failed`, one whose schema is *newer* than this
/// build understands is also `Failed` (with an explicit upgrade message,
/// not a silent "just run a scan"), and everything else - a database that
/// merely has pending migrations - is `Outdated`, which the caller can
/// offer to fix with a scan. `check_database_health` guarantees
/// `database_opens = false` implies `migrations_current = false`, so this
/// is only ever called when at least one of these three applies.
fn classify_unhealthy_database(health: DatabaseHealth) -> DatabaseLoadError {
    // `database_opens` alone is not enough to rule out a corrupt file:
    // Connection::open is lazy, so a garbage file still "opens" and only
    // fails once something actually reads page 1 - `health.error` carries
    // that failure through (see check_database_health) even when
    // `database_opens` is true.
    if !health.database_opens || health.error.is_some() {
        return DatabaseLoadError::Failed {
            message: health
                .error
                .clone()
                .unwrap_or_else(|| "the database could not be opened".to_string()),
        };
    }
    if let Some(version) = health.schema_version
        && version > latest_schema_version()
    {
        return DatabaseLoadError::Failed {
            message: format!(
                "This database's schema (version {version}) is newer than this build of \
                 ArchiveFS supports (version {}). Upgrade ArchiveFS, or remove the database \
                 file to rebuild it.",
                latest_schema_version()
            ),
        };
    }
    DatabaseLoadError::Outdated { health }
}

fn load_snapshot_from(
    database: &Database,
    database_path: &Path,
) -> Result<CachedLibrarySnapshot, DatabaseLoadError> {
    let to_failed = |error: ArchiveFsError| DatabaseLoadError::Failed {
        message: error.to_string(),
    };
    let schema_version = database.schema_version().map_err(to_failed)?;
    let archives = database.load_archives().map_err(to_failed)?;
    let platform_details = database
        .load_platform_provenance_details(&archives)
        .map_err(to_failed)?;
    let stats = database.catalogue_stats().map_err(to_failed)?;
    let last_completed_scan = database.latest_completed_scan().map_err(to_failed)?;
    let platform_aliases = database.list_platform_aliases().map_err(to_failed)?;
    let duplicate_report = catalogue_filename_duplicates(&archives);
    Ok(CachedLibrarySnapshot {
        database_path: database_path.to_path_buf(),
        schema_version,
        archives,
        platform_details,
        stats,
        last_completed_scan,
        platform_aliases,
        duplicate_report,
    })
}

struct RunningSetupAction {
    action: SetupAction,
    receiver: Receiver<Result<String, String>>,
}

/// One manual platform assignment change requested from the selected
/// archive's details panel - see `show_selected_archive`. Metadata-only:
/// unlike mount/unmount, this never depends on `latest_generation_actions_safe`
/// and is available for a cache-only/missing row exactly as for a live
/// one, since it only ever touches the library database, never the
/// filesystem or a mount.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PlatformAction {
    Set(String),
    Clear,
}

struct RunningPlatformAction {
    archive_path: PathBuf,
    receiver: Receiver<Result<PlatformAssignmentChange, String>>,
}

/// One bulk manual platform assignment change requested from the compact
/// "N archives selected" action bar - see `show_bulk_platform_action_bar`.
/// Metadata-only, exactly like `PlatformAction`: never depends on
/// `latest_generation_actions_safe`, never touches the filesystem or a
/// mount. Deliberately narrower than `PlatformAction`: no free-form
/// custom-text escape hatch (only `canonical_platform_names()`), matching
/// the bulk feature's "simple by default" scope.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BulkPlatformActionKind {
    Set(String),
    Clear,
}

/// The outcome of one bulk platform action applied at the GUI layer:
/// [`BulkPlatformAssignmentResult`] (archive-id-keyed, from the database
/// bulk API) plus how many of the *selected paths* never resolved to any
/// database archive id at all (a live-only/not-yet-scanned row, for
/// example) - a GUI-specific concern the database bulk API cannot see,
/// since it only ever receives ids. Kept as a separate, GUI-local
/// wrapper rather than adding a field to the shared core type, which the
/// CLI also uses and has no such "started from an exact PathBuf
/// selection" concept.
struct BulkPlatformActionOutcome {
    result: BulkPlatformAssignmentResult,
    unresolved_paths: usize,
}

struct RunningBulkPlatformAction {
    kind: BulkPlatformActionKind,
    requested_paths: usize,
    receiver: Receiver<Result<BulkPlatformActionOutcome, String>>,
}

/// Sentinel `platform_choice` value meaning "let the user type a custom
/// platform" - the GUI's escape hatch, mirroring the CLI's `--custom`
/// flag. Never itself sent as a platform value; `resolved_platform_choice`
/// substitutes the free-text field's contents instead.
const CUSTOM_PLATFORM_CHOICE: &str = "Custom...";

/// One custom-platform-alias database write requested from the "Custom
/// Platform Aliases" panel - see `show_platform_aliases_panel`.
/// Metadata-only, exactly like `PlatformAction`: never touches the
/// filesystem or a mount, and never triggers a rescan.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AliasAction {
    Add { alias: String, platform: String },
    Remove { alias: String },
}

struct RunningAliasAction {
    action: AliasAction,
    receiver: Receiver<Result<(), String>>,
}

struct RunningMissingRemoval {
    requested_paths: usize,
    receiver: Receiver<Result<MissingArchiveRemovalResult, String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DuplicateReviewFilters {
    search: String,
    platform: Option<String>,
    include_missing: bool,
    more_than_two: bool,
}

impl DuplicateReviewFilters {
    fn initial() -> Self {
        Self {
            include_missing: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DuplicateSortField {
    #[default]
    Title,
    Platform,
    Entries,
    KnownSize,
}

impl std::fmt::Display for DuplicateSortField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Title => "Title",
            Self::Platform => "Platform",
            Self::Entries => "Number of entries",
            Self::KnownSize => "Total known size",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DuplicateGroupIdentity {
    normalized_title: String,
    platform: String,
}

impl From<&CatalogueDuplicateGroup> for DuplicateGroupIdentity {
    fn from(group: &CatalogueDuplicateGroup) -> Self {
        Self {
            normalized_title: group.normalized_title.clone(),
            platform: group.platform.clone(),
        }
    }
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
    diagnostics: DiagnosticsState,
    show_diagnostics: bool,
    setup_action: Option<RunningSetupAction>,
    refresh_error: Option<String>,
    snapshot_stale: bool,
    refresh_generation: RefreshGeneration,
    snapshot_generation: Option<RefreshGeneration>,
    database_state: DatabaseState,
    database_generation: DatabaseGeneration,
    library_filters: LibraryRowFilters,
    platform_action: Option<RunningPlatformAction>,
    platform_choice: Option<String>,
    platform_custom_text: String,
    alias_action: Option<RunningAliasAction>,
    missing_removal: Option<RunningMissingRemoval>,
    confirm_remove_missing: Option<Vec<PathBuf>>,
    new_alias_text: String,
    new_alias_platform_choice: Option<String>,
    /// The exact-identity multi-selection (requirement 1): every
    /// currently multi-selected row's `ArchiveRow::path`. Never row
    /// indices - see `prune_selection` for how this survives a
    /// filter/reload without pointing at the wrong archive.
    /// `selected_archive` (above) remains, unchanged, the single
    /// "focused" row driving the details panel; this is a separate,
    /// additive overlay used only for row highlighting and the bulk
    /// action bar.
    selected_archives: HashSet<PathBuf>,
    bulk_platform_action: Option<RunningBulkPlatformAction>,
    bulk_platform_choice: Option<String>,
    /// The library table's current column sort - milestone requirement
    /// 2. `None` means unsorted / natural (merge) order.
    sort_field: Option<SortField>,
    sort_ascending: bool,
    /// The library table's vertical `ScrollArea` offset as of the end of
    /// the last frame - tracked here (rather than trusted to egui's own
    /// persisted-by-`Id` scroll state) so keyboard focus movement can read
    /// last frame's position *before* deciding whether this frame needs to
    /// override it to bring the newly-focused row into view. See
    /// `compute_scroll_offset_for_focus`.
    library_scroll_offset: f32,
    show_duplicate_review: bool,
    duplicate_filters: DuplicateReviewFilters,
    duplicate_sort_field: DuplicateSortField,
    duplicate_sort_ascending: bool,
    selected_duplicate_group: Option<DuplicateGroupIdentity>,
    selected_duplicate_archive: Option<PathBuf>,
}

impl ArchiveFsApp {
    fn is_busy(&self) -> bool {
        self.operation.is_some()
            || self.mount_all.is_some()
            || self.unmount_all.is_some()
            || self.setup_action.is_some()
    }

    fn new(context: egui::Context) -> Self {
        let generation = RefreshGeneration::INITIAL;
        let database_generation = DatabaseGeneration::INITIAL;
        let mut history = OperationHistory::default();
        history.record(HistoryEntry::new(
            ActivityAction::Refresh,
            None,
            ActivityOutcome::Started,
            "Loading archive snapshot.",
        ));
        Self {
            state: start_load(context.clone(), generation, None),
            database_state: start_database_load(context.clone(), database_generation, None, false),
            database_generation,
            library_filters: LibraryRowFilters::default(),
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
            diagnostics: start_diagnostics(context.clone(), generation),
            show_diagnostics: false,
            setup_action: None,
            refresh_error: None,
            snapshot_stale: false,
            refresh_generation: generation,
            snapshot_generation: None,
            platform_action: None,
            platform_choice: None,
            platform_custom_text: String::new(),
            alias_action: None,
            missing_removal: None,
            confirm_remove_missing: None,
            new_alias_text: String::new(),
            new_alias_platform_choice: None,
            selected_archives: HashSet::new(),
            bulk_platform_action: None,
            bulk_platform_choice: None,
            sort_field: None,
            sort_ascending: true,
            library_scroll_offset: 0.0,
            show_duplicate_review: false,
            duplicate_filters: DuplicateReviewFilters::initial(),
            duplicate_sort_field: DuplicateSortField::Title,
            duplicate_sort_ascending: true,
            selected_duplicate_group: None,
            selected_duplicate_archive: None,
        }
    }

    fn refresh(&mut self, context: &egui::Context) {
        self.refresh_generation = self.refresh_generation.next();
        let generation = self.refresh_generation;
        self.history.record(HistoryEntry::new(
            ActivityAction::Refresh,
            None,
            ActivityOutcome::Started,
            "Refreshing archive snapshot.",
        ));
        let previous = match std::mem::replace(
            &mut self.state,
            LoadState::Error("refresh starting".to_string()),
        ) {
            LoadState::Ready(data) => Some(data),
            LoadState::Loading { previous, .. } => previous,
            LoadState::Error(_) => None,
        };
        self.refresh_diagnostics(context);
        self.state = start_load(context.clone(), generation, previous);
    }

    fn poll_load(&mut self, _context: &egui::Context) {
        let result = match &self.state {
            LoadState::Loading {
                generation,
                receiver,
                ..
            } => match receiver.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((
                    *generation,
                    Err("background data loader stopped unexpectedly".to_string()),
                )),
            },
            LoadState::Ready(_) | LoadState::Error(_) => None,
        };

        if let Some((generation, result)) = result {
            if generation != self.refresh_generation {
                return;
            }
            let (state_generation, previous) = match std::mem::replace(
                &mut self.state,
                LoadState::Error("load result pending".to_string()),
            ) {
                LoadState::Loading {
                    generation,
                    previous,
                    ..
                } => (Some(generation), previous),
                LoadState::Ready(_) | LoadState::Error(_) => (None, None),
            };
            if state_generation != Some(generation) {
                return;
            }
            self.state = match result {
                Ok(data) => {
                    let merged = build_display_rows(
                        &data.records,
                        &data.rows,
                        self.database_state.snapshot(),
                    );
                    self.filtered_rows = matching_row_indices(&merged, &self.filter);
                    self.prune_selection(&merged);
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Refresh,
                        None,
                        ActivityOutcome::Completed,
                        "Archive snapshot refreshed.",
                    ));
                    self.refresh_error = None;
                    self.snapshot_stale = false;
                    self.snapshot_generation = Some(generation);
                    LoadState::Ready(Box::new(data))
                }
                Err(error) => {
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Refresh,
                        None,
                        ActivityOutcome::Failed,
                        error.clone(),
                    ));
                    self.refresh_error = Some(error.clone());
                    self.snapshot_stale = previous.is_some();
                    self.show_diagnostics = true;
                    previous.map_or_else(|| LoadState::Error(error), LoadState::Ready)
                }
            };
        }
    }

    /// Starts (or restarts) a background database load. `run_scan_first =
    /// true` is "Scan Library" (runs `scan_and_persist` before reloading);
    /// `false` is "Refresh Database Status" / "Retry Database Load" (a
    /// read-only reload). Never blocks the UI thread - mirrors
    /// `refresh`/`start_load` exactly.
    fn start_database_action(&mut self, context: egui::Context, run_scan_first: bool) {
        if self.missing_removal.is_some() {
            return;
        }
        self.database_generation = self.database_generation.next();
        let generation = self.database_generation;
        let previous = match std::mem::replace(
            &mut self.database_state,
            DatabaseState::Error {
                message: "database action starting".to_string(),
                previous: None,
            },
        ) {
            DatabaseState::Ready { snapshot, .. } => Some(snapshot),
            DatabaseState::Loading { previous, .. }
            | DatabaseState::Outdated { previous, .. }
            | DatabaseState::Error { previous, .. } => previous,
            DatabaseState::NotCreated { .. } => None,
        };
        if run_scan_first {
            self.history.record(HistoryEntry::new(
                ActivityAction::LibraryDatabase,
                None,
                ActivityOutcome::Started,
                "Scanning configured source folders into the library database.",
            ));
        }
        self.database_state = start_database_load(context, generation, previous, run_scan_first);
    }

    fn poll_database_load(&mut self, _context: &egui::Context) {
        let message = match &self.database_state {
            DatabaseState::Loading {
                generation,
                receiver,
                ..
            } => match receiver.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((
                    *generation,
                    Err(DatabaseLoadError::Failed {
                        message: "background database loader stopped unexpectedly".to_string(),
                    }),
                )),
            },
            DatabaseState::NotCreated { .. }
            | DatabaseState::Ready { .. }
            | DatabaseState::Outdated { .. }
            | DatabaseState::Error { .. } => None,
        };

        let Some((generation, result)) = message else {
            return;
        };
        // Two independent staleness checks, mirroring poll_load exactly:
        // (1) is this even the current database generation, and (2) does
        // the state we are about to replace still agree it is Loading at
        // that same generation (it could have been replaced by a newer
        // start_database_action call between the channel send and this
        // poll). Either mismatch means this message is from a previous
        // generation and must be ignored, never merged into current state.
        if generation != self.database_generation {
            return;
        }
        let previous = match std::mem::replace(
            &mut self.database_state,
            DatabaseState::Error {
                message: "database load result pending".to_string(),
                previous: None,
            },
        ) {
            DatabaseState::Loading {
                generation: state_generation,
                previous,
                ..
            } if state_generation == generation => previous,
            other => {
                self.database_state = other;
                return;
            }
        };

        self.database_state = match result {
            Ok(DatabaseOutcome::Loaded(snapshot)) => DatabaseState::Ready {
                snapshot: Box::new(snapshot),
                last_scan_summary: None,
            },
            Ok(DatabaseOutcome::Scanned {
                snapshot,
                scan_summary,
            }) => {
                self.history.record(HistoryEntry::new(
                    ActivityAction::LibraryDatabase,
                    None,
                    ActivityOutcome::Completed,
                    format_scan_activity(&scan_summary),
                ));
                DatabaseState::Ready {
                    snapshot: Box::new(snapshot),
                    last_scan_summary: Some(scan_summary),
                }
            }
            Err(DatabaseLoadError::NotCreated { database_path }) => {
                DatabaseState::NotCreated { database_path }
            }
            Err(DatabaseLoadError::Outdated { health }) => {
                DatabaseState::Outdated { health, previous }
            }
            Err(DatabaseLoadError::Failed { message }) => {
                self.history.record(HistoryEntry::new(
                    ActivityAction::LibraryDatabase,
                    None,
                    ActivityOutcome::Failed,
                    message.clone(),
                ));
                DatabaseState::Error { message, previous }
            }
        };

        let duplicate_report = self
            .database_state
            .snapshot()
            .map(|snapshot| snapshot.duplicate_report.clone());
        prune_duplicate_review_selection(
            &mut self.selected_duplicate_group,
            &mut self.selected_duplicate_archive,
            duplicate_report.as_ref(),
        );

        // The merged row set may have just changed (a cache reload/scan
        // just settled) - recompute the cached filtered-index list against
        // it now rather than leaving it stale until the next live refresh
        // or filter-text edit. Only meaningful once a live snapshot
        // exists; the cache-only preview shown before that filters itself
        // fresh each frame instead (see `show_loaded_data`'s Loading
        // branch).
        if let LoadState::Ready(data) = &self.state {
            let merged =
                build_display_rows(&data.records, &data.rows, self.database_state.snapshot());
            self.filtered_rows = matching_row_indices(&merged, &self.filter);
            self.prune_selection(&merged);
        }
    }

    /// Removes any exact-identity entry from `selected_archives` - and
    /// clears `selected_archive` if it was the one that vanished - that
    /// no longer names any row in `merged_rows`, the just-recomputed
    /// live+cache catalogue (requirement 7: "remove selections that no
    /// longer exist in the loaded catalogue"). Called from both
    /// `poll_load` and `poll_database_load`, right where each already
    /// recomputes `filtered_rows` against the same merged list, so
    /// selection state is never one step behind what is actually on
    /// screen. Compares exact `PathBuf` identity only, never a lossy
    /// display string, and never touches row indices - there are none to
    /// go stale here in the first place.
    fn prune_selection(&mut self, merged_rows: &[ArchiveRow]) {
        self.selected_archives
            .retain(|path| merged_rows.iter().any(|row| &row.path == path));
        if let Some(selected) = &self.selected_archive
            && !merged_rows.iter().any(|row| &row.path == selected)
        {
            self.selected_archive = None;
        }
    }

    fn refresh_diagnostics(&mut self, context: &egui::Context) {
        self.history.record(HistoryEntry::new(
            ActivityAction::Diagnostics,
            None,
            ActivityOutcome::Started,
            "Refreshing setup diagnostics.",
        ));
        self.diagnostics = start_diagnostics(context.clone(), self.refresh_generation);
    }

    fn poll_diagnostics(&mut self) {
        enum PollResult {
            Completed(DiagnosticsMessage),
            Disconnected(RefreshGeneration),
        }

        let result = match &self.diagnostics {
            DiagnosticsState::Loading {
                generation,
                receiver,
            } => match receiver.try_recv() {
                Ok(message) => Some(PollResult::Completed(message)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(PollResult::Disconnected(*generation)),
            },
            DiagnosticsState::Ready { .. } | DiagnosticsState::Error { .. } => None,
        };
        match result {
            Some(PollResult::Completed((generation, report)))
                if generation == self.refresh_generation =>
            {
                self.history.record(HistoryEntry::new(
                    ActivityAction::Diagnostics,
                    None,
                    ActivityOutcome::Completed,
                    if report.ready_for_actions {
                        "Diagnostics completed: ArchiveFS is ready."
                    } else {
                        "Diagnostics completed: setup needs attention."
                    },
                ));
                self.diagnostics = DiagnosticsState::Ready { generation, report };
            }
            Some(PollResult::Disconnected(generation)) if generation == self.refresh_generation => {
                let message = "The diagnostics worker stopped unexpectedly. Run diagnostics again."
                    .to_string();
                self.history.record(HistoryEntry::new(
                    ActivityAction::Diagnostics,
                    None,
                    ActivityOutcome::Failed,
                    message.clone(),
                ));
                self.diagnostics = DiagnosticsState::Error {
                    generation,
                    message,
                };
                self.show_diagnostics = true;
            }
            Some(PollResult::Completed(_)) | Some(PollResult::Disconnected(_)) | None => {}
        }
    }

    fn start_setup_action(&mut self, context: egui::Context, action: SetupAction) {
        if self.is_busy() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.history.record(HistoryEntry::new(
            ActivityAction::Setup,
            None,
            ActivityOutcome::Started,
            match action {
                SetupAction::CreateStarterConfig => "Creating starter config.",
                SetupAction::CreateMountRoot => "Creating configured mount root.",
                SetupAction::OpenConfigFolder => "Opening config folder.",
            },
        ));
        self.setup_action = Some(RunningSetupAction { action, receiver });
        thread::spawn(move || {
            let result = match action {
                SetupAction::CreateStarterConfig => create_starter_config_default()
                    .map(|path| format!("Created starter config at {}.", path.display())),
                SetupAction::CreateMountRoot => create_configured_mount_root_default()
                    .map(|path| format!("Created mount root at {}.", path.display())),
                SetupAction::OpenConfigFolder => open_default_config_folder(),
            }
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn poll_setup_action(&mut self, context: &egui::Context) {
        let result = self.setup_action.as_ref().and_then(|running| {
            running
                .receiver
                .try_recv()
                .ok()
                .map(|result| (running.action, result))
        });
        if let Some((action, result)) = result {
            self.setup_action = None;
            match result {
                Ok(message) => {
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Setup,
                        None,
                        ActivityOutcome::Completed,
                        message.clone(),
                    ));
                    self.feedback = Some(ActionFeedback {
                        succeeded: true,
                        message,
                        cleanup: None,
                        warning: None,
                        more_information: None,
                    });
                    if action != SetupAction::OpenConfigFolder {
                        self.refresh_diagnostics(context);
                    }
                }
                Err(message) => {
                    self.history.record(HistoryEntry::new(
                        ActivityAction::Setup,
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
                }
            }
        }
    }

    /// Whether a new platform-assignment action may start: not already
    /// running one (single-row *or* bulk - only one platform-metadata
    /// writer at a time, since both ultimately write the same
    /// `platform_assignments` table), and not in the middle of a database
    /// load/scan (the same "one database writer at a time" convention
    /// `start_database_action`'s own UI already enforces by disabling its
    /// buttons while loading - see `show_database_panel`). This never
    /// touches `is_busy()`/mount safety - platform assignment is
    /// metadata-only and deliberately independent of it.
    fn platform_action_available(&self) -> bool {
        self.platform_action.is_none()
            && self.bulk_platform_action.is_none()
            && self.alias_action.is_none()
            && self.missing_removal.is_none()
            && !self.database_state.is_loading()
    }

    /// The bulk counterpart to `platform_action_available` - see its doc
    /// comment for why single-row and bulk platform actions share one
    /// "no concurrent writer" gate.
    fn bulk_platform_action_available(&self) -> bool {
        self.platform_action_available()
    }

    fn start_platform_action(
        &mut self,
        context: egui::Context,
        archive_path: PathBuf,
        action: PlatformAction,
    ) {
        if !self.platform_action_available() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.history.record(HistoryEntry::new(
            ActivityAction::PlatformAssignment,
            Some(archive_path.clone()),
            ActivityOutcome::Started,
            match &action {
                PlatformAction::Set(platform) => format!("Setting platform to {platform}."),
                PlatformAction::Clear => "Clearing manual platform.".to_string(),
            },
        ));
        self.platform_action = Some(RunningPlatformAction {
            archive_path: archive_path.clone(),
            receiver,
        });
        thread::spawn(move || {
            let result =
                apply_platform_action(&archive_path, &action).map_err(|error| error.to_string());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn poll_platform_action(&mut self, context: &egui::Context) {
        let result = self.platform_action.as_ref().and_then(|running| {
            running
                .receiver
                .try_recv()
                .ok()
                .map(|result| (running.archive_path.clone(), result))
        });
        let Some((archive_path, result)) = result else {
            return;
        };
        self.platform_action = None;
        match result {
            Ok(change) => {
                let message = format!(
                    "Platform changed from {} to {}.",
                    describe_platform_assignment(
                        change.old_platform.as_deref(),
                        change.old_source.as_deref()
                    ),
                    describe_platform_assignment(
                        change.new_platform.as_deref(),
                        change.new_source.as_deref()
                    )
                );
                self.history.record(HistoryEntry::new(
                    ActivityAction::PlatformAssignment,
                    Some(archive_path),
                    ActivityOutcome::Completed,
                    message.clone(),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: true,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
                // Refresh only the cached database row - never the live
                // snapshot (self.state), which this action never touches.
                self.start_database_action(context.clone(), false);
            }
            Err(message) => {
                self.history.record(HistoryEntry::new(
                    ActivityAction::PlatformAssignment,
                    Some(archive_path),
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
            }
        }
    }

    /// Starts a bulk platform action for every archive in
    /// `archive_paths` (the current multi-selection - see
    /// `show_bulk_platform_action_bar`) on a background thread, exactly
    /// like `start_platform_action` for a single archive. A no-op if
    /// `archive_paths` is empty or a platform-metadata write is already
    /// in progress (`bulk_platform_action_available`).
    fn start_bulk_platform_action(
        &mut self,
        context: egui::Context,
        archive_paths: Vec<PathBuf>,
        kind: BulkPlatformActionKind,
    ) {
        if !self.bulk_platform_action_available() || archive_paths.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let requested_paths = archive_paths.len();
        self.history.record(HistoryEntry::new(
            ActivityAction::BulkPlatformAssignment,
            None,
            ActivityOutcome::Started,
            match &kind {
                BulkPlatformActionKind::Set(platform) => format!(
                    "Setting platform to {platform} for {requested_paths} selected archives."
                ),
                BulkPlatformActionKind::Clear => {
                    format!("Clearing manual platform for {requested_paths} selected archives.")
                }
            },
        ));
        self.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: kind.clone(),
            requested_paths,
            receiver,
        });
        thread::spawn(move || {
            let result = apply_bulk_platform_action(&archive_paths, &kind)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    /// Mirrors `poll_platform_action`: on success, refreshes only the
    /// cached database snapshot (never the live archive snapshot, never a
    /// scan - see `start_database_action(.., false)`). The actual
    /// selection pruning (requirement 7's "remove selections that no
    /// longer exist in the loaded catalogue") happens once that reload
    /// settles, in `poll_load`/`poll_database_load` via `prune_selection`,
    /// not here, since the reload is itself asynchronous and has not
    /// necessarily completed yet when this returns.
    fn poll_bulk_platform_action(&mut self, context: &egui::Context) {
        let result = self.bulk_platform_action.as_ref().and_then(|running| {
            running
                .receiver
                .try_recv()
                .ok()
                .map(|result| (running.kind.clone(), running.requested_paths, result))
        });
        let Some((kind, requested_paths, result)) = result else {
            return;
        };
        self.bulk_platform_action = None;
        match result {
            Ok(outcome) => {
                let action_word = match &kind {
                    BulkPlatformActionKind::Set(platform) => format!("set to {platform}"),
                    BulkPlatformActionKind::Clear => "cleared".to_string(),
                };
                let mut message = format!(
                    "Platform {action_word} for {} of {requested_paths} selected archive(s) ({} unchanged, {} missing from the database",
                    outcome.result.changed,
                    outcome.result.unchanged,
                    outcome.result.missing.len(),
                );
                if outcome.unresolved_paths > 0 {
                    message.push_str(&format!(
                        ", {} not yet scanned into the database",
                        outcome.unresolved_paths
                    ));
                }
                message.push(')');
                self.history.record(HistoryEntry::new(
                    ActivityAction::BulkPlatformAssignment,
                    None,
                    ActivityOutcome::Completed,
                    message.clone(),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: true,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
                // Refresh only the cached database row - never the live
                // snapshot (self.state), which this action never touches.
                self.start_database_action(context.clone(), false);
            }
            Err(message) => {
                self.history.record(HistoryEntry::new(
                    ActivityAction::BulkPlatformAssignment,
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
                // Deliberately does not touch database_state or
                // selected_archives - requirement 8: a failed bulk action
                // must preserve both the prior cached rows and the
                // selection exactly as they were.
            }
        }
    }

    /// Whether a new custom-platform-alias action may start: not already
    /// running one, and not in the middle of a database load/scan - the
    /// same "one database writer at a time" convention
    /// `platform_action_available` already enforces for individual
    /// archive platform assignment. This never touches `is_busy()`/mount
    /// safety - alias management is metadata-only and deliberately
    /// independent of it, exactly like platform assignment.
    fn alias_action_available(&self) -> bool {
        self.alias_action.is_none()
            && self.platform_action.is_none()
            && self.bulk_platform_action.is_none()
            && self.missing_removal.is_none()
            && !self.database_state.is_loading()
    }

    fn start_alias_action(&mut self, context: egui::Context, action: AliasAction) {
        if !self.alias_action_available() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.history.record(HistoryEntry::new(
            ActivityAction::PlatformAliasManagement,
            None,
            ActivityOutcome::Started,
            match &action {
                AliasAction::Add { alias, platform } => {
                    format!("Adding platform alias '{alias}' -> {platform}.")
                }
                AliasAction::Remove { alias } => format!("Removing platform alias '{alias}'."),
            },
        ));
        self.alias_action = Some(RunningAliasAction {
            action: action.clone(),
            receiver,
        });
        thread::spawn(move || {
            let result = apply_alias_action(&action).map_err(|error| error.to_string());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    /// Mirrors `poll_platform_action`: on success, refreshes only the
    /// cached database snapshot (`platform_aliases` is now part of it;
    /// see `load_snapshot_from`), never the live archive snapshot and
    /// never a scan. On a successful add, clears the input fields so the
    /// panel is ready for the next alias; a successful remove leaves
    /// them untouched (there is nothing to clear).
    fn poll_alias_action(&mut self, context: &egui::Context) {
        let result = self.alias_action.as_ref().and_then(|running| {
            running
                .receiver
                .try_recv()
                .ok()
                .map(|result| (running.action.clone(), result))
        });
        let Some((action, result)) = result else {
            return;
        };
        self.alias_action = None;
        match result {
            Ok(()) => {
                let message = match &action {
                    AliasAction::Add { alias, platform } => {
                        format!(
                            "Alias added: '{alias}' -> {platform}. Run a library scan to apply it."
                        )
                    }
                    AliasAction::Remove { alias } => {
                        format!(
                            "Alias removed: '{alias}'. Run a library scan to apply this change."
                        )
                    }
                };
                self.history.record(HistoryEntry::new(
                    ActivityAction::PlatformAliasManagement,
                    None,
                    ActivityOutcome::Completed,
                    message.clone(),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: true,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
                if matches!(action, AliasAction::Add { .. }) {
                    self.new_alias_text.clear();
                    self.new_alias_platform_choice = None;
                }
                self.start_database_action(context.clone(), false);
            }
            Err(message) => {
                let action_label = match &action {
                    AliasAction::Add { alias, .. } => format!("Add alias '{alias}'"),
                    AliasAction::Remove { alias } => format!("Remove alias '{alias}'"),
                };
                self.history.record(HistoryEntry::new(
                    ActivityAction::PlatformAliasManagement,
                    None,
                    ActivityOutcome::Failed,
                    format!("{action_label}: {message}"),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: false,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
            }
        }
    }

    fn missing_removal_action_available(&self) -> bool {
        self.missing_removal.is_none()
            && self.platform_action.is_none()
            && self.bulk_platform_action.is_none()
            && self.alias_action.is_none()
            && matches!(self.database_state, DatabaseState::Ready { .. })
    }

    fn start_missing_removal(&mut self, context: egui::Context, archive_paths: Vec<PathBuf>) {
        if !self.missing_removal_action_available() || archive_paths.is_empty() {
            return;
        }
        let requested_paths = archive_paths.len();
        let (sender, receiver) = mpsc::channel();
        self.missing_removal = Some(RunningMissingRemoval {
            requested_paths,
            receiver,
        });
        thread::spawn(move || {
            let result = apply_missing_removal(&archive_paths).map_err(|error| error.to_string());
            let _ = sender.send(result);
            context.request_repaint();
        });
    }

    fn poll_missing_removal(&mut self, context: &egui::Context) {
        let result = self.missing_removal.as_ref().and_then(|running| {
            running
                .receiver
                .try_recv()
                .ok()
                .map(|result| (running.requested_paths, result))
        });
        let Some((requested_paths, result)) = result else {
            return;
        };
        self.missing_removal = None;
        match result {
            Ok(result) => {
                let message = format!(
                    "Removed {} missing catalogue entr{}. No archive files were deleted.",
                    result.removed,
                    if result.removed == 1 { "y" } else { "ies" }
                );
                self.history.record(HistoryEntry::new(
                    ActivityAction::CatalogueCleanup,
                    None,
                    ActivityOutcome::Completed,
                    message.clone(),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: true,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
                self.start_database_action(context.clone(), false);
            }
            Err(message) => {
                self.history.record(HistoryEntry::new(
                    ActivityAction::CatalogueCleanup,
                    None,
                    ActivityOutcome::Failed,
                    format!(
                        "Could not remove {requested_paths} selected missing catalogue entr{}: {message}",
                        if requested_paths == 1 { "y" } else { "ies" }
                    ),
                ));
                self.feedback = Some(ActionFeedback {
                    succeeded: false,
                    message,
                    cleanup: None,
                    warning: None,
                    more_information: None,
                });
            }
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
                    set_lazy_unmount_offer(
                        &mut self.lazy_unmount_offers,
                        &item.archive_path,
                        false,
                    );
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
                        set_lazy_unmount_offer(
                            &mut self.lazy_unmount_offers,
                            &item.archive_path,
                            true,
                        );
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
                    set_lazy_unmount_offer(
                        &mut self.lazy_unmount_offers,
                        &item.archive_path,
                        false,
                    );
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
    PlatformAssignment {
        archive_path: PathBuf,
        action: PlatformAction,
    },
    BulkPlatformAssignment {
        archive_paths: Vec<PathBuf>,
        kind: BulkPlatformActionKind,
    },
    RemoveMissing(Vec<PathBuf>),
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
        self.poll_load(context);
        self.poll_database_load(context);
        self.poll_diagnostics();
        self.poll_setup_action(context);
        self.poll_platform_action(context);
        self.poll_bulk_platform_action(context);
        self.poll_alias_action(context);
        self.poll_missing_removal(context);
        self.poll_operation(context);
        self.poll_mount_all(context);
        self.poll_unmount_all(context);
        let loading = matches!(self.state, LoadState::Loading { .. });
        let diagnostics_loading = matches!(self.diagnostics, DiagnosticsState::Loading { .. });
        let busy = self.is_busy();
        let actions_safe = latest_generation_actions_safe(
            self.refresh_generation,
            self.snapshot_generation,
            self.snapshot_stale,
            snapshot_identity(&self.state),
            &self.diagnostics,
        );
        let archive_actions_blocked = busy || !actions_safe;
        let missing_removal_available = self.missing_removal_action_available();
        if loading || diagnostics_loading || busy {
            context.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("ArchiveFS");
                ui.separator();
                ui.label("Library overview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Diagnostics"))
                        .clicked()
                    {
                        open_diagnostics_view(&mut self.show_diagnostics);
                        self.refresh_diagnostics(context);
                    }
                    if ui
                        .add_enabled(!loading && !busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh(context);
                    }
                    let duplicate_label = if self.show_duplicate_review {
                        "Back to Library"
                    } else {
                        "Duplicate Review"
                    };
                    if ui
                        .add_enabled(
                            self.database_state.snapshot().is_some(),
                            egui::Button::new(duplicate_label),
                        )
                        .clicked()
                    {
                        self.show_duplicate_review = !self.show_duplicate_review;
                        self.show_diagnostics = false;
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
        let mut diagnostics_action = None;
        let mut stop_mount_all = false;
        let mut stop_unmount_all = false;
        egui::CentralPanel::default().show(context, |ui| {
            if self.show_diagnostics {
                diagnostics_action = show_setup_diagnostics(
                    ui,
                    &self.diagnostics,
                    self.setup_action.is_some(),
                    self.feedback.as_ref(),
                    self.refresh_error.as_deref(),
                    self.snapshot_stale && matches!(self.state, LoadState::Ready(_)),
                );
                return;
            }
            if let Some(error) = &self.refresh_error {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Refresh failed; showing the last known snapshot: {error}"),
                );
                ui.separator();
            }
            if let Some(batch) = self.mount_all.as_ref() {
                stop_mount_all = show_mount_all_progress(ui, &batch.progress);
                ui.separator();
            }
            if let Some(batch) = self.unmount_all.as_ref() {
                stop_unmount_all = show_unmount_all_progress(ui, &batch.progress);
                ui.separator();
            }
            if let Some(action) = show_database_panel(ui, &self.database_state) {
                match action {
                    DatabasePanelAction::ScanLibrary => {
                        self.start_database_action(context.clone(), true);
                    }
                    DatabasePanelAction::RefreshStatus | DatabasePanelAction::RetryLoad => {
                        self.start_database_action(context.clone(), false);
                    }
                }
            }
            let cached_aliases = self
                .database_state
                .snapshot()
                .map(|snapshot| snapshot.platform_aliases.as_slice())
                .unwrap_or(&[]);
            if let Some(action) = show_platform_aliases_panel(
                ui,
                cached_aliases,
                &mut self.new_alias_text,
                &mut self.new_alias_platform_choice,
                self.alias_action.is_some(),
            ) {
                self.start_alias_action(context.clone(), action);
            }
            ui.separator();

            if self.show_duplicate_review
                && let Some(snapshot) = self.database_state.snapshot()
            {
                if show_duplicate_review_panel(
                    ui,
                    &snapshot.duplicate_report,
                    &mut self.duplicate_filters,
                    &mut self.duplicate_sort_field,
                    &mut self.duplicate_sort_ascending,
                    &mut self.selected_duplicate_group,
                    &mut self.selected_duplicate_archive,
                ) {
                    self.show_duplicate_review = false;
                }
                return;
            }

            match &self.state {
                LoadState::Loading { .. } => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.spinner();
                        ui.heading("Loading ArchiveFS data...");
                        ui.label("Scanning runs in the background.");
                    });
                    // Requirement 1: show cached library rows before the
                    // live snapshot finishes loading, clearly labelled as
                    // last-known state. This is a read-only preview -
                    // built with the same ArchiveRow/show_archive_rows
                    // machinery as the live table, but with no selection
                    // or action wiring at all, so it cannot expose a mount
                    // or unmount button even in principle.
                    if let Some(snapshot) = self.database_state.snapshot() {
                        ui.separator();
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Showing last-known catalogue state while the live snapshot loads.",
                        );
                        let preview_rows: Vec<ArchiveRow> = snapshot
                            .archives
                            .iter()
                            .map(|persisted| {
                                let path_exists = persisted.absolute_path.exists();
                                ArchiveRow::from_cached(persisted, path_exists)
                            })
                            .collect();
                        let row_height = fixed_row_height(
                            ui.text_style_height(&egui::TextStyle::Body),
                            ui.spacing().interact_size.y,
                        );
                        let horizontal_spacing = ui.spacing().item_spacing.x;
                        egui::ScrollArea::horizontal()
                            .id_salt("cache_preview_horizontal")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(table_width(horizontal_spacing));
                                // This cache-only loading preview has no sort
                                // state of its own to wire up (it disappears
                                // the moment the live snapshot loads) - the
                                // headers render inertly, unsorted.
                                let _ = show_header_row(
                                    ui,
                                    &COLUMN_HEADERS,
                                    &COLUMN_SORT_FIELDS,
                                    row_height,
                                    None,
                                    true,
                                );
                                ui.separator();
                                let body_height = ui.available_height().max(row_height);
                                egui::ScrollArea::vertical()
                                    .id_salt("cache_preview_vertical")
                                    .max_height(body_height)
                                    .auto_shrink([false, false])
                                    .show_rows(
                                        ui,
                                        row_height,
                                        preview_rows.len(),
                                        |ui, row_range| {
                                            // Discard the clicked index: this preview never
                                            // sets selected_archive, so it can never drive
                                            // show_selected_archive's action buttons.
                                            let _ = show_archive_rows(
                                                ui,
                                                &preview_rows,
                                                None,
                                                row_range,
                                                row_height,
                                                None,
                                                &HashSet::new(),
                                            );
                                        },
                                    );
                            });
                    }
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
                            busy: archive_actions_blocked,
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
                            cached: self.database_state.snapshot(),
                            library_filters: &mut self.library_filters,
                            platform_choice: &mut self.platform_choice,
                            platform_custom_text: &mut self.platform_custom_text,
                            platform_busy: self.platform_action.is_some(),
                            selected_archives: &mut self.selected_archives,
                            bulk_platform_choice: &mut self.bulk_platform_choice,
                            bulk_platform_busy: self.bulk_platform_action.is_some(),
                            missing_removal_available,
                            missing_removal_busy: self.missing_removal.is_some(),
                            confirm_remove_missing: &mut self.confirm_remove_missing,
                            sort_field: &mut self.sort_field,
                            sort_ascending: &mut self.sort_ascending,
                            library_scroll_offset: &mut self.library_scroll_offset,
                        },
                    );
                }
            }
        });
        if stop_mount_all {
            self.request_mount_all_stop();
        }
        if let Some(action) = diagnostics_action {
            match action {
                DiagnosticsUiAction::Refresh => self.refresh_diagnostics(context),
                DiagnosticsUiAction::Continue => {
                    self.show_diagnostics = false;
                    self.refresh(context);
                }
                DiagnosticsUiAction::ViewLastSnapshot => {
                    self.show_diagnostics = false;
                }
                DiagnosticsUiAction::CreateStarterConfig => {
                    self.start_setup_action(context.clone(), SetupAction::CreateStarterConfig)
                }
                DiagnosticsUiAction::CreateMountRoot => {
                    self.start_setup_action(context.clone(), SetupAction::CreateMountRoot)
                }
                DiagnosticsUiAction::OpenConfigFolder => {
                    self.start_setup_action(context.clone(), SetupAction::OpenConfigFolder)
                }
                DiagnosticsUiAction::CopyConfigPath => {
                    if let DiagnosticsState::Ready { report, .. } = &self.diagnostics
                        && let Some(path) = &report.config_path
                    {
                        let path = path.display().to_string();
                        context.copy_text(path.clone());
                        self.history.record(HistoryEntry::new(
                            ActivityAction::Setup,
                            None,
                            ActivityOutcome::Completed,
                            format!("Copied config path: {path}"),
                        ));
                    }
                }
            }
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
                AppOperationRequest::PlatformAssignment {
                    archive_path,
                    action,
                } => {
                    self.start_platform_action(context.clone(), archive_path, action);
                }
                AppOperationRequest::BulkPlatformAssignment {
                    archive_paths,
                    kind,
                } => {
                    self.start_bulk_platform_action(context.clone(), archive_paths, kind);
                }
                AppOperationRequest::RemoveMissing(archive_paths) => {
                    self.start_missing_removal(context.clone(), archive_paths);
                }
            }
        }
    }
}

fn start_load(
    context: egui::Context,
    generation: RefreshGeneration,
    previous: Option<Box<LoadedData>>,
) -> LoadState {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = load_data();
        let _ = sender.send((generation, result));
        context.request_repaint();
    });
    LoadState::Loading {
        generation,
        receiver,
        previous,
    }
}

fn start_diagnostics(context: egui::Context, generation: RefreshGeneration) -> DiagnosticsState {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send((generation, run_setup_diagnostics_default()));
        context.request_repaint();
    });
    DiagnosticsState::Loading {
        generation,
        receiver,
    }
}

fn open_default_config_folder() -> archivefs_core::Result<String> {
    let config_path = default_config_path()?;
    let folder = config_path.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "config path has no parent folder: {}",
            config_path.display()
        ))
    })?;
    let (program, argument) = if cfg!(target_os = "windows") {
        ("explorer", folder.as_os_str())
    } else if cfg!(target_os = "macos") {
        ("open", folder.as_os_str())
    } else {
        ("xdg-open", folder.as_os_str())
    };
    let status = Command::new(program)
        .arg(argument)
        .status()
        .map_err(ArchiveFsError::from)?;
    if !status.success() {
        return Err(ArchiveFsError::ExternalCommand {
            program: program.to_string(),
            status: status.code(),
            stderr: format!("could not open {}", folder.display()),
        });
    }
    Ok(format!("Opened config folder {}.", folder.display()))
}

fn load_data() -> LoadResult {
    load_read_only_snapshot_default()
        .map(LoadedData::from_snapshot)
        .map_err(|error| error.to_string())
}

/// Opens the default library database and applies one `PlatformAction`
/// to the archive at `archive_path` - the production entry point run on
/// the background thread `ArchiveFsApp::start_platform_action` spawns.
/// See [`apply_platform_action_at`] (the testable core, taking an
/// explicit database path - mirrors `load_database_snapshot`/
/// `load_database_snapshot_at`) for the actual logic.
fn apply_platform_action(
    archive_path: &Path,
    action: &PlatformAction,
) -> archivefs_core::Result<PlatformAssignmentChange> {
    let database_path = default_database_path()?;
    apply_platform_action_at(&database_path, archive_path, action)
}

/// Resolves `archive_path` to a stable persisted archive id by exact
/// path bytes first (never a lossy display string - see
/// `Database::find_archive_id_by_absolute_path`), then applies `action`.
/// Errors clearly if the archive is not yet in the library database
/// (nothing to assign a platform to) rather than silently doing nothing.
fn apply_platform_action_at(
    database_path: &Path,
    archive_path: &Path,
    action: &PlatformAction,
) -> archivefs_core::Result<PlatformAssignmentChange> {
    let mut database = Database::open_or_create(database_path)?;
    let archive_id = database
        .find_archive_id_by_absolute_path(archive_path)?
        .ok_or_else(|| {
            ArchiveFsError::Database(format!(
                "{} is not yet in the library database - run a library scan first",
                archive_path.display()
            ))
        })?;
    match action {
        PlatformAction::Set(platform) => database.set_manual_platform(archive_id, platform),
        PlatformAction::Clear => database.clear_manual_platform(archive_id),
    }
}

/// Opens the default library database and applies one
/// `BulkPlatformActionKind` to `archive_paths` - the production entry
/// point run on the background thread `ArchiveFsApp::start_bulk_platform_action`
/// spawns. See [`apply_bulk_platform_action_at`] (the testable core,
/// mirrors `apply_platform_action`/`apply_platform_action_at`) for the
/// actual logic.
fn apply_bulk_platform_action(
    archive_paths: &[PathBuf],
    kind: &BulkPlatformActionKind,
) -> archivefs_core::Result<BulkPlatformActionOutcome> {
    let database_path = default_database_path()?;
    apply_bulk_platform_action_at(&database_path, archive_paths, kind)
}

/// Resolves every path in `archive_paths` to a stable persisted archive
/// id by exact path bytes (never a lossy display string - see
/// `Database::find_archive_id_by_absolute_path`), then applies `kind` to
/// every id that resolved in one database transaction (see
/// `Database::set_manual_platform_for_archives`/
/// `clear_manual_platform_for_archives`). Unlike the single-row
/// `apply_platform_action_at`, a path that does not resolve to any
/// database archive id (a live-only/not-yet-scanned row, for example) is
/// not a hard error here - it is counted in the returned
/// `BulkPlatformActionOutcome::unresolved_paths` instead, so one
/// unresolvable row in a large selection never blocks every other,
/// resolvable row in the same selection from being updated. This mirrors
/// the database bulk API's own "skip and report, don't abort" policy for
/// an archive id that turns out not to exist.
fn apply_bulk_platform_action_at(
    database_path: &Path,
    archive_paths: &[PathBuf],
    kind: &BulkPlatformActionKind,
) -> archivefs_core::Result<BulkPlatformActionOutcome> {
    let mut database = Database::open_or_create(database_path)?;
    let mut ids = Vec::with_capacity(archive_paths.len());
    let mut unresolved_paths = 0usize;
    for path in archive_paths {
        match database.find_archive_id_by_absolute_path(path)? {
            Some(id) => ids.push(id),
            None => unresolved_paths += 1,
        }
    }
    let result = match kind {
        BulkPlatformActionKind::Set(platform) => {
            database.set_manual_platform_for_archives(&ids, platform)?
        }
        BulkPlatformActionKind::Clear => database.clear_manual_platform_for_archives(&ids)?,
    };
    Ok(BulkPlatformActionOutcome {
        result,
        unresolved_paths,
    })
}

/// Opens the default library database and applies one `AliasAction` -
/// the production entry point run on the background thread
/// `ArchiveFsApp::start_alias_action` spawns. See
/// [`apply_alias_action_at`] (the testable core, taking an explicit
/// database path - mirrors `apply_platform_action`/
/// `apply_platform_action_at`) for the actual logic. Uses
/// `Database::open_or_create` (creating the database if it does not
/// exist yet) rather than requiring a pre-existing one: unlike manual
/// platform assignment, an alias is not attached to any specific
/// already-scanned archive, so there is nothing that requires the
/// database - or a scan - to already exist first. This matches
/// `library-scan`'s existing "open or create" write-command convention
/// on the CLI side.
fn apply_alias_action(action: &AliasAction) -> archivefs_core::Result<()> {
    let database_path = default_database_path()?;
    apply_alias_action_at(&database_path, action)
}

fn apply_alias_action_at(database_path: &Path, action: &AliasAction) -> archivefs_core::Result<()> {
    let mut database = Database::open_or_create(database_path)?;
    match action {
        AliasAction::Add { alias, platform } => {
            database.add_platform_alias(alias, platform)?;
        }
        AliasAction::Remove { alias } => {
            if !database.remove_platform_alias(alias)? {
                return Err(ArchiveFsError::Database(format!(
                    "no platform alias matches '{alias}'"
                )));
            }
        }
    }
    Ok(())
}

fn apply_missing_removal(
    archive_paths: &[PathBuf],
) -> archivefs_core::Result<MissingArchiveRemovalResult> {
    let database_path = default_database_path()?;
    apply_missing_removal_at(&database_path, archive_paths)
}

fn apply_missing_removal_at(
    database_path: &Path,
    archive_paths: &[PathBuf],
) -> archivefs_core::Result<MissingArchiveRemovalResult> {
    if !database_path.exists() {
        return Err(ArchiveFsError::Database(format!(
            "library database does not exist at {}",
            database_path.display()
        )));
    }
    let mut database = Database::open_or_create(database_path)?;
    let mut ids = Vec::with_capacity(archive_paths.len());
    for path in archive_paths {
        let archive_id = database
            .find_archive_id_by_absolute_path(path)?
            .ok_or_else(|| {
                ArchiveFsError::Database(format!(
                    "no archive found with exact stored path {}; nothing was removed",
                    path.display()
                ))
            })?;
        ids.push(archive_id);
    }
    database.remove_missing_archives(&ids)
}

/// Formats a platform assignment for display as `"<platform>
/// (<provenance>)"`, or `"Unknown"` when there is none - the same shape
/// as the CLI's `format_platform_and_source`, kept as a small separate
/// copy here rather than a shared crate dependency between the two
/// binaries for two lines of formatting.
fn describe_platform_assignment(platform: Option<&str>, source: Option<&str>) -> String {
    match (platform, source) {
        (Some(platform), Some(source)) => format!("{platform} ({source})"),
        _ => "Unknown".to_string(),
    }
}

fn platform_source_label(source: Option<&str>) -> &'static str {
    match source {
        Some(MANUAL_PLATFORM_SOURCE) => "Manual assignment",
        Some(CUSTOM_FOLDER_ALIAS_SOURCE) => "Custom folder alias",
        Some("folder_alias") => "Built-in folder alias",
        Some("heuristic-path-detector") => "Filename/path heuristic",
        Some(_) => "Automatic detection",
        None => "Unknown",
    }
}

fn platform_provenance_lines(details: &PlatformProvenanceDetails) -> Vec<(&'static str, String)> {
    let mut lines = vec![
        (
            "Platform",
            details.platform.as_deref().unwrap_or("Unknown").to_string(),
        ),
        (
            "Source",
            platform_source_label(details.source.as_deref()).to_string(),
        ),
    ];

    match (
        details.source.as_deref(),
        details.matched_component.as_ref(),
    ) {
        (Some(CUSTOM_FOLDER_ALIAS_SOURCE), Some(matched)) => {
            lines.push(("Matched alias", matched.clone()));
        }
        (Some("folder_alias"), Some(matched)) => {
            lines.push(("Matched folder", matched.clone()));
        }
        _ => {}
    }

    if details.source.as_deref() == Some(MANUAL_PLATFORM_SOURCE) {
        let fallback = details.automatic_fallback.as_ref();
        lines.push((
            "Automatic fallback",
            fallback
                .map(|fallback| fallback.platform.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
        ));
        if let Some(fallback) = fallback {
            lines.push((
                "Fallback source",
                platform_source_label(Some(&fallback.source)).to_string(),
            ));
            match (
                fallback.source.as_str(),
                fallback.matched_component.as_ref(),
            ) {
                (CUSTOM_FOLDER_ALIAS_SOURCE, Some(matched)) => {
                    lines.push(("Fallback matched alias", matched.clone()));
                }
                ("folder_alias", Some(matched)) => {
                    lines.push(("Fallback matched folder", matched.clone()));
                }
                _ => {}
            }
        }
    }

    lines
}

fn format_scan_completion(summary: &ScanPersistSummary) -> String {
    format!(
        "Scan completed\nSeen: {}\nAdded: {}\nUpdated: {}\nRestored: {}\nNewly missing: {}\nUnchanged: {}\nErrors: {}",
        summary.counts.archives_seen,
        summary.counts.archives_added,
        summary.counts.archives_changed,
        summary.counts.archives_restored,
        summary.counts.archives_missing,
        summary.counts.archives_unchanged,
        summary.folder_errors.len(),
    )
}

fn format_scan_activity(summary: &ScanPersistSummary) -> String {
    format!(
        "Scan completed: seen {}, added {}, updated {}, restored {}, newly missing {}, unchanged {}, errors {}.",
        summary.counts.archives_seen,
        summary.counts.archives_added,
        summary.counts.archives_changed,
        summary.counts.archives_restored,
        summary.counts.archives_missing,
        summary.counts.archives_unchanged,
        summary.folder_errors.len(),
    )
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

fn show_setup_diagnostics(
    ui: &mut egui::Ui,
    state: &DiagnosticsState,
    action_running: bool,
    feedback: Option<&ActionFeedback>,
    refresh_error: Option<&str>,
    has_last_snapshot: bool,
) -> Option<DiagnosticsUiAction> {
    let mut action = None;
    ui.heading("Setup / Diagnostics");
    ui.label("Check configuration, folders, and required system tools before using ArchiveFS.");
    ui.add_space(8.0);
    if let Some(error) = refresh_error {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Archive refresh failed: {error}"),
        );
        ui.label("Diagnostics are being refreshed from the current configuration.");
        if has_last_snapshot
            && ui
                .add_enabled(
                    !action_running,
                    egui::Button::new("View Last Known Snapshot"),
                )
                .clicked()
        {
            return Some(DiagnosticsUiAction::ViewLastSnapshot);
        }
        ui.add_space(8.0);
    }
    if let DiagnosticsState::Error { message, .. } = state {
        ui.colored_label(ui.visuals().error_fg_color, message);
        ui.label("Select Refresh Diagnostics to try again.");
        if ui
            .add_enabled(!action_running, egui::Button::new("Refresh Diagnostics"))
            .clicked()
        {
            return Some(DiagnosticsUiAction::Refresh);
        }
        return None;
    }
    let DiagnosticsState::Ready { report, .. } = state else {
        ui.spinner();
        ui.label("Running diagnostics in the background...");
        ui.add_enabled(false, egui::Button::new("Continue to ArchiveFS"));
        return None;
    };
    ui.horizontal_wrapped(|ui| {
        ui.strong("Config path:");
        match &report.config_path {
            Some(path) => {
                ui.monospace(path.display().to_string());
                if ui
                    .add_enabled(!action_running, egui::Button::new("Copy Config Path"))
                    .clicked()
                {
                    action = Some(DiagnosticsUiAction::CopyConfigPath);
                }
                if ui
                    .add_enabled(!action_running, egui::Button::new("Open Config Folder"))
                    .clicked()
                {
                    action = Some(DiagnosticsUiAction::OpenConfigFolder);
                }
            }
            None => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    report
                        .config_path_error
                        .as_deref()
                        .unwrap_or("Config path could not be resolved."),
                );
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        if starter_config_available(report)
            && ui
                .add_enabled(!action_running, egui::Button::new("Create Starter Config"))
                .clicked()
        {
            action = Some(DiagnosticsUiAction::CreateStarterConfig);
        }
        if report.can_create_mount_root
            && ui
                .add_enabled(!action_running, egui::Button::new("Create Mount Root"))
                .clicked()
        {
            action = Some(DiagnosticsUiAction::CreateMountRoot);
        }
        if ui
            .add_enabled(!action_running, egui::Button::new("Refresh Diagnostics"))
            .clicked()
        {
            action = Some(DiagnosticsUiAction::Refresh);
        }
        if ui
            .add_enabled(
                !action_running && diagnostics_state_can_continue(state),
                egui::Button::new("Continue to ArchiveFS"),
            )
            .clicked()
        {
            action = Some(DiagnosticsUiAction::Continue);
        }
        if action_running {
            ui.spinner();
            ui.label("Setup action running...");
        }
    });
    if let Some(feedback) = feedback {
        ui.colored_label(
            if feedback.succeeded {
                egui::Color32::from_rgb(70, 170, 90)
            } else {
                ui.visuals().error_fg_color
            },
            &feedback.message,
        );
    }
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("setup_diagnostics_checks")
        .show(ui, |ui| {
            for check in &report.checks {
                let (state_label, color) = match check.status {
                    SetupDiagnosticStatus::Ready => ("Ready", egui::Color32::from_rgb(70, 170, 90)),
                    SetupDiagnosticStatus::Warning => {
                        ("Warning", egui::Color32::from_rgb(220, 170, 40))
                    }
                    SetupDiagnosticStatus::Error => ("Error", ui.visuals().error_fg_color),
                };
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, state_label);
                        ui.strong(&check.name);
                    });
                    ui.label(&check.detail);
                    if check.status != SetupDiagnosticStatus::Ready {
                        ui.label(format!("Why it matters: {}", check.why_it_matters));
                        ui.label(format!("Next step: {}", check.next_step));
                    }
                });
                ui.add_space(4.0);
            }
        });
    action
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

/// Which "Library Database" panel button (requirement 3) was clicked.
/// `RefreshStatus` and `RetryLoad` both trigger the same underlying
/// read-only reload (`start_database_action(.., false)`) - they are kept
/// as separate variants only because they are offered from different
/// states and read better as separate buttons to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabasePanelAction {
    ScanLibrary,
    RefreshStatus,
    RetryLoad,
}

/// Renders the compact "Library Database" status area (requirement 3).
/// Purely informational plus three buttons - never itself authorizes an
/// action, and never blocks the caller (all of its data comes from
/// `state`, already computed off the UI thread).
fn show_database_panel(ui: &mut egui::Ui, state: &DatabaseState) -> Option<DatabasePanelAction> {
    let mut action = None;
    egui::CollapsingHeader::new("Library Database")
        .id_salt("library_database_panel")
        .default_open(!matches!(state, DatabaseState::Ready { .. }))
        .show(ui, |ui| {
            let database_path = match state {
                DatabaseState::NotCreated { database_path } => Some(database_path.clone()),
                DatabaseState::Loading { previous, .. }
                | DatabaseState::Outdated { previous, .. }
                | DatabaseState::Error { previous, .. } => previous
                    .as_ref()
                    .map(|snapshot| snapshot.database_path.clone()),
                DatabaseState::Ready { snapshot, .. } => Some(snapshot.database_path.clone()),
            };

            egui::Grid::new("database_status_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.strong("Database path");
                    ui.label(
                        database_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "unresolved".to_string()),
                    );
                    ui.end_row();

                    ui.strong("Status");
                    ui.label(state.status_label());
                    ui.end_row();

                    if let Some(snapshot) = state.snapshot() {
                        ui.strong("Schema version");
                        ui.label(snapshot.schema_version.to_string());
                        ui.end_row();

                        ui.strong("Last completed scan");
                        ui.label(
                            snapshot
                                .last_completed_scan
                                .as_ref()
                                .map(|scan| {
                                    scan.finished_at.clone().unwrap_or_else(|| {
                                        format!("{} (in progress)", scan.started_at)
                                    })
                                })
                                .unwrap_or_else(|| "never".to_string()),
                        );
                        ui.end_row();

                        ui.strong("Cached archives");
                        ui.label(snapshot.stats.total_archives.to_string());
                        ui.end_row();

                        ui.strong("Present / missing");
                        ui.label(format!(
                            "{} / {}",
                            snapshot.stats.present_archives, snapshot.stats.missing_archives
                        ));
                        ui.end_row();
                    }

                    if let DatabaseState::Ready {
                        last_scan_summary: Some(summary),
                        ..
                    } = state
                    {
                        ui.strong("Last scan (this session)");
                        ui.label(format_scan_completion(summary));
                        ui.end_row();
                    }

                    ui.strong("Action safety");
                    ui.label(
                        "Cached rows never authorize mount or unmount - only a validated live \
                         snapshot can.",
                    );
                    ui.end_row();
                });

            match state {
                DatabaseState::Outdated { health, .. } => {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!(
                            "Database schema is outdated (found version {}); run a library scan \
                             to upgrade it.",
                            health
                                .schema_version
                                .map(|version| version.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ),
                    );
                }
                DatabaseState::Error { message, .. } => {
                    ui.colored_label(ui.visuals().error_fg_color, message.as_str());
                }
                DatabaseState::NotCreated { .. } => {
                    ui.label("No library database yet. Run a library scan to create one.");
                }
                DatabaseState::Loading { .. } | DatabaseState::Ready { .. } => {}
            }

            ui.horizontal(|ui| {
                let loading = state.is_loading();
                if ui
                    .add_enabled(!loading, egui::Button::new("Scan Library"))
                    .clicked()
                {
                    action = Some(DatabasePanelAction::ScanLibrary);
                }
                match state {
                    DatabaseState::Ready { .. } => {
                        if ui
                            .add_enabled(!loading, egui::Button::new("Refresh Database Status"))
                            .clicked()
                        {
                            action = Some(DatabasePanelAction::RefreshStatus);
                        }
                    }
                    DatabaseState::NotCreated { .. }
                    | DatabaseState::Outdated { .. }
                    | DatabaseState::Error { .. } => {
                        if ui
                            .add_enabled(!loading, egui::Button::new("Retry Database Load"))
                            .clicked()
                        {
                            action = Some(DatabasePanelAction::RetryLoad);
                        }
                    }
                    DatabaseState::Loading { .. } => {}
                }
                if loading {
                    ui.spinner();
                    ui.label(if state.is_scanning() {
                        "Scanning..."
                    } else {
                        "Loading..."
                    });
                }
            });
        });
    action
}

/// Renders the compact "Custom Platform Aliases" section: a collapsible
/// list of persisted aliases (each with a Remove button), plus an
/// alias-text field and a canonical-platform dropdown with an Add Alias
/// button. Purely a view over `aliases` (already loaded off the UI
/// thread as part of the cached database snapshot - see
/// `CachedLibrarySnapshot::platform_aliases`) plus the two caller-owned
/// input fields; never itself opens a database or blocks. `busy` (from
/// `ArchiveFsApp::alias_action`) disables every control here while one
/// alias action is already running, so two cannot overlap - it is
/// deliberately independent of `is_busy()`/mount safety, exactly like
/// the existing per-archive platform assignment controls.
fn show_platform_aliases_panel(
    ui: &mut egui::Ui,
    aliases: &[PlatformAlias],
    new_alias_text: &mut String,
    new_alias_platform_choice: &mut Option<String>,
    busy: bool,
) -> Option<AliasAction> {
    let mut action = None;
    egui::CollapsingHeader::new("Custom Platform Aliases")
        .id_salt("platform_aliases_panel")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                "Map a folder name to a platform (for example \"gc\" -> GameCube). Custom \
                 aliases outrank built-in detection but never a manual archive assignment. \
                 Changes take effect on the next library scan.",
            );
            ui.separator();

            if aliases.is_empty() {
                ui.label("No custom platform aliases defined.");
            } else {
                egui::Grid::new("platform_aliases_grid")
                    .num_columns(3)
                    .show(ui, |ui| {
                        for alias in aliases {
                            ui.label(&alias.alias);
                            ui.label(&alias.platform);
                            if ui.add_enabled(!busy, egui::Button::new("Remove")).clicked() {
                                action = Some(AliasAction::Remove {
                                    alias: alias.alias.clone(),
                                });
                            }
                            ui.end_row();
                        }
                    });
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Alias:");
                ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(new_alias_text)
                        .desired_width(120.0)
                        .hint_text("gc"),
                );
                ui.label("Platform:");
                egui::ComboBox::from_id_salt("platform_alias_choice_combo")
                    .selected_text(
                        new_alias_platform_choice
                            .as_deref()
                            .unwrap_or("Select platform..."),
                    )
                    .show_ui(ui, |ui| {
                        for name in canonical_platform_names() {
                            ui.selectable_value(
                                new_alias_platform_choice,
                                Some(name.to_string()),
                                name,
                            );
                        }
                    });

                let resolved_action =
                    resolved_new_alias_action(new_alias_text, new_alias_platform_choice.as_deref());
                if ui
                    .add_enabled(
                        !busy && resolved_action.is_some(),
                        egui::Button::new("Add Alias"),
                    )
                    .clicked()
                {
                    action = resolved_action;
                }
                if busy {
                    ui.spinner();
                }
            });
        });
    action
}

/// The `AliasAction::Add` the Add Alias button constructs, factored out
/// so it is directly testable (mirrors `resolved_platform_choice`'s
/// existing convention for the per-archive platform editor). `None`
/// exactly when the button itself would be disabled: `alias` trims to
/// empty, or no platform has been chosen from the canonical-platform
/// picker yet.
fn resolved_new_alias_action(alias: &str, platform_choice: Option<&str>) -> Option<AliasAction> {
    let trimmed_alias = alias.trim();
    if trimmed_alias.is_empty() {
        return None;
    }
    let platform = platform_choice?;
    Some(AliasAction::Add {
        alias: trimmed_alias.to_string(),
        platform: platform.to_string(),
    })
}

fn duplicate_visible_entries(
    group: &CatalogueDuplicateGroup,
    include_missing: bool,
) -> Vec<&CatalogueDuplicateArchive> {
    group
        .entries
        .iter()
        .filter(|entry| include_missing || entry.present)
        .collect()
}

fn duplicate_group_matches(
    group: &CatalogueDuplicateGroup,
    filters: &DuplicateReviewFilters,
) -> bool {
    if filters
        .platform
        .as_deref()
        .is_some_and(|platform| platform != group.platform)
    {
        return false;
    }
    let entries = duplicate_visible_entries(group, filters.include_missing);
    if entries.len() < 2 || (filters.more_than_two && entries.len() <= 2) {
        return false;
    }
    let search = filters.search.trim().to_lowercase();
    search.is_empty()
        || group.title.to_lowercase().contains(&search)
        || group.normalized_title.contains(&search)
        || entries.iter().any(|entry| {
            entry
                .path
                .to_string_lossy()
                .to_lowercase()
                .contains(&search)
        })
}

fn visible_duplicate_group_indices(
    report: &CatalogueDuplicateReport,
    filters: &DuplicateReviewFilters,
    sort_field: DuplicateSortField,
    ascending: bool,
) -> Vec<usize> {
    let mut indices = report
        .groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| duplicate_group_matches(group, filters).then_some(index))
        .collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        let left_group = &report.groups[*left];
        let right_group = &report.groups[*right];
        let left_entries = duplicate_visible_entries(left_group, filters.include_missing);
        let right_entries = duplicate_visible_entries(right_group, filters.include_missing);
        let ordering = match sort_field {
            DuplicateSortField::Title => left_group
                .normalized_title
                .cmp(&right_group.normalized_title),
            DuplicateSortField::Platform => left_group.platform.cmp(&right_group.platform),
            DuplicateSortField::Entries => left_entries.len().cmp(&right_entries.len()),
            DuplicateSortField::KnownSize => {
                visible_known_size(&left_entries).cmp(&visible_known_size(&right_entries))
            }
        }
        .then_with(|| {
            left_group
                .normalized_title
                .cmp(&right_group.normalized_title)
        })
        .then_with(|| left_group.platform.cmp(&right_group.platform));
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
    indices
}

fn visible_known_size(entries: &[&CatalogueDuplicateArchive]) -> u128 {
    entries
        .iter()
        .filter_map(|entry| entry.size_bytes)
        .map(u128::from)
        .sum()
}

fn prune_duplicate_review_selection(
    selected_group: &mut Option<DuplicateGroupIdentity>,
    selected_archive: &mut Option<PathBuf>,
    report: Option<&CatalogueDuplicateReport>,
) {
    let selected_group_still_exists = selected_group.as_ref().is_some_and(|selected| {
        report.is_some_and(|report| {
            report
                .groups
                .iter()
                .any(|group| DuplicateGroupIdentity::from(group) == *selected)
        })
    });
    if !selected_group_still_exists {
        *selected_group = None;
        *selected_archive = None;
        return;
    }
    let selected_archive_still_exists = selected_archive.as_ref().is_none_or(|path| {
        report.is_some_and(|report| {
            report.groups.iter().any(|group| {
                selected_group.as_ref() == Some(&DuplicateGroupIdentity::from(group))
                    && group.entries.iter().any(|entry| entry.path == *path)
            })
        })
    });
    if !selected_archive_still_exists {
        *selected_archive = None;
    }
}

fn show_duplicate_review_panel(
    ui: &mut egui::Ui,
    report: &CatalogueDuplicateReport,
    filters: &mut DuplicateReviewFilters,
    sort_field: &mut DuplicateSortField,
    sort_ascending: &mut bool,
    selected_group: &mut Option<DuplicateGroupIdentity>,
    selected_archive: &mut Option<PathBuf>,
) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.heading("Duplicate Review");
        ui.label("Review only — ArchiveFS will not change archive files here.");
        if ui.button("Back to Library").clicked() {
            close = true;
        }
    });
    ui.label("Groups are likely duplicates, not claims that files are byte-identical.");
    ui.add_space(6.0);

    ui.horizontal_wrapped(|ui| {
        ui.label("Search title or exact path:");
        ui.add(
            egui::TextEdit::singleline(&mut filters.search)
                .id_salt("archivefs_duplicate_search")
                .desired_width(260.0),
        );
        ui.checkbox(&mut filters.include_missing, "Include missing entries");
        ui.checkbox(&mut filters.more_than_two, "More than two entries");
    });

    let mut platforms = report
        .groups
        .iter()
        .map(|group| group.platform.as_str())
        .collect::<Vec<_>>();
    platforms.sort_unstable();
    platforms.dedup();
    ui.horizontal(|ui| {
        ui.label("Platform:");
        egui::ComboBox::from_id_salt("duplicate_platform_filter")
            .selected_text(filters.platform.as_deref().unwrap_or("All platforms"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut filters.platform, None, "All platforms");
                for platform in platforms {
                    ui.selectable_value(
                        &mut filters.platform,
                        Some(platform.to_string()),
                        platform,
                    );
                }
            });
        ui.label("Sort by:");
        egui::ComboBox::from_id_salt("duplicate_sort")
            .selected_text(sort_field.to_string())
            .show_ui(ui, |ui| {
                for field in [
                    DuplicateSortField::Title,
                    DuplicateSortField::Platform,
                    DuplicateSortField::Entries,
                    DuplicateSortField::KnownSize,
                ] {
                    ui.selectable_value(sort_field, field, field.to_string());
                }
            });
        ui.checkbox(sort_ascending, "Ascending");
    });

    let visible = visible_duplicate_group_indices(report, filters, *sort_field, *sort_ascending);
    let visible_entry_count = visible
        .iter()
        .map(|index| {
            duplicate_visible_entries(&report.groups[*index], filters.include_missing).len()
        })
        .sum::<usize>();
    ui.horizontal_wrapped(|ui| {
        summary_value(ui, "Duplicate groups", visible.len());
        summary_value(ui, "Archive entries involved", visible_entry_count);
        if !filters.include_missing {
            ui.label("Present entries only");
        }
    });
    ui.separator();

    if visible.is_empty() {
        ui.label("No likely duplicate groups match the current review filters.");
        return close;
    }

    ui.strong("Likely duplicate groups");
    egui::ScrollArea::vertical()
        .id_salt("duplicate_group_list")
        .max_height(180.0)
        .show(ui, |ui| {
            for index in &visible {
                let group = &report.groups[*index];
                let identity = DuplicateGroupIdentity::from(group);
                let entry_count = duplicate_visible_entries(group, filters.include_missing).len();
                let selected = selected_group.as_ref() == Some(&identity);
                if ui
                    .selectable_label(
                        selected,
                        format!(
                            "{} — {} — {} entries",
                            group.title, group.platform, entry_count
                        ),
                    )
                    .clicked()
                {
                    *selected_group = Some(identity);
                    *selected_archive = None;
                }
            }
        });

    let Some(group) = selected_group.as_ref().and_then(|selected| {
        visible
            .iter()
            .map(|index| &report.groups[*index])
            .find(|group| DuplicateGroupIdentity::from(*group) == *selected)
    }) else {
        ui.label("Select a likely duplicate group to inspect every archive in it.");
        return close;
    };
    let entries = duplicate_visible_entries(group, filters.include_missing);
    if entries.len() < 2 {
        ui.label("The selected group is hidden by the current entry filters.");
        return close;
    }

    ui.separator();
    ui.strong("Likely duplicate group");
    egui::Grid::new("duplicate_group_details")
        .num_columns(2)
        .show(ui, |ui| {
            detail_row(ui, "Title", &group.title);
            detail_row(ui, "Platform", &group.platform);
            detail_row(ui, "Entries", &entries.len().to_string());
            detail_row(ui, "Method", "Filename and platform");
            detail_row(ui, "Reason", &group.reason);
            let known_count = entries
                .iter()
                .filter(|entry| entry.size_bytes.is_some())
                .count();
            detail_row(
                ui,
                "Total known size",
                &format!(
                    "{} ({} of {} entries known)",
                    format_known_size(visible_known_size(&entries)),
                    known_count,
                    entries.len()
                ),
            );
        });

    ui.add_space(4.0);
    for entry in entries {
        let is_selected = selected_archive.as_ref() == Some(&entry.path);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if ui
                .selectable_label(is_selected, entry.path.display().to_string())
                .on_hover_text(entry.path.display().to_string())
                .clicked()
            {
                *selected_archive = Some(entry.path.clone());
            }
            let state = if entry.present { "Present" } else { "Missing" };
            let color = if entry.present {
                ui.visuals().text_color()
            } else {
                ui.visuals().warn_fg_color
            };
            ui.colored_label(color, state);
            ui.label(format!("Size: {}", format_duplicate_size(entry.size_bytes)));
            ui.label(format!(
                "Modified time: {}",
                format_modified_time(entry.modified_time_unix_seconds)
            ));
        });
    }

    if let Some(path) = selected_archive.as_ref()
        && let Some(entry) = group.entries.iter().find(|entry| entry.path == *path)
    {
        ui.separator();
        ui.strong("Selected duplicate archive");
        egui::Grid::new("selected_duplicate_archive_details")
            .num_columns(2)
            .show(ui, |ui| {
                detail_row(ui, "Exact archive path", &entry.path.display().to_string());
                detail_row(ui, "Platform", &group.platform);
                detail_row(
                    ui,
                    "State",
                    if entry.present { "Present" } else { "Missing" },
                );
                detail_row(ui, "Size", &format_duplicate_size(entry.size_bytes));
                detail_row(
                    ui,
                    "Modified time",
                    &format_modified_time(entry.modified_time_unix_seconds),
                );
            });
    }
    close
}

fn format_known_size(size_bytes: u128) -> String {
    format_byte_count(size_bytes)
}

fn format_duplicate_size(size_bytes: Option<u64>) -> String {
    size_bytes
        .map(|size| format_byte_count(u128::from(size)))
        .unwrap_or_else(|| "Unknown".to_string())
}

fn format_byte_count(size_bytes: u128) -> String {
    const KIB: u128 = 1024;
    const MIB: u128 = KIB * 1024;
    const GIB: u128 = MIB * 1024;
    const TIB: u128 = GIB * 1024;
    let (unit_size, unit_name) = if size_bytes >= TIB {
        (TIB, "TiB")
    } else if size_bytes >= GIB {
        (GIB, "GiB")
    } else if size_bytes >= MIB {
        (MIB, "MiB")
    } else if size_bytes >= KIB {
        (KIB, "KiB")
    } else {
        return format!("{size_bytes} bytes");
    };
    let whole = size_bytes / unit_size;
    let tenth = (size_bytes % unit_size) * 10 / unit_size;
    format!("{whole}.{tenth} {unit_name} ({size_bytes} bytes)")
}

fn format_modified_time(seconds: Option<i64>) -> String {
    seconds
        .map(format_unix_timestamp_utc)
        .unwrap_or_else(|| "Unknown".to_string())
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
    cached: Option<&'a CachedLibrarySnapshot>,
    library_filters: &'a mut LibraryRowFilters,
    platform_choice: &'a mut Option<String>,
    platform_custom_text: &'a mut String,
    platform_busy: bool,
    selected_archives: &'a mut HashSet<PathBuf>,
    bulk_platform_choice: &'a mut Option<String>,
    bulk_platform_busy: bool,
    missing_removal_available: bool,
    missing_removal_busy: bool,
    confirm_remove_missing: &'a mut Option<Vec<PathBuf>>,
    sort_field: &'a mut Option<SortField>,
    sort_ascending: &'a mut bool,
    library_scroll_offset: &'a mut f32,
}

const REMOVE_MISSING_CANCEL_LABEL: &str = "Cancel";
const REMOVE_MISSING_CONFIRM_LABEL: &str = "Remove Missing Entries";

fn set_missing_review_mode(filters: &mut LibraryRowFilters, enabled: bool) {
    filters.missing = enabled;
    if enabled {
        filters.present = false;
        filters.awaiting_validation = false;
    }
}

fn selected_missing_paths(
    cached: Option<&CachedLibrarySnapshot>,
    selected_archives: &HashSet<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    if selected_archives.is_empty() {
        return Err("Select one or more missing catalogue entries first.".to_string());
    }
    let cached = cached.ok_or_else(|| "The library database is unavailable.".to_string())?;
    let mut paths: Vec<PathBuf> = selected_archives.iter().cloned().collect();
    paths.sort();
    for path in &paths {
        let archive = cached
            .archives
            .iter()
            .find(|archive| archive.absolute_path == *path)
            .ok_or_else(|| {
                format!(
                    "{} is not an exact stored catalogue path. Nothing was removed.",
                    path.display()
                )
            })?;
        if archive.last_verified_missing_at.is_none() {
            return Err(format!(
                "{} is currently present. Only missing catalogue entries can be removed; nothing was removed.",
                path.display()
            ));
        }
    }
    Ok(paths)
}

fn missing_removal_confirmation_text(count: usize) -> String {
    format!(
        "Remove {count} missing entr{} from the ArchiveFS catalogue?\n\n\
         This removes only ArchiveFS database records.\n\
         It will not delete archive files or mounted contents.\n\
         Entries will return if the archives are found in a later scan.",
        if count == 1 { "y" } else { "ies" }
    )
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
        cached,
        library_filters,
        history,
        platform_choice,
        platform_custom_text,
        platform_busy,
        selected_archives,
        bulk_platform_choice,
        bulk_platform_busy,
        missing_removal_available,
        missing_removal_busy,
        confirm_remove_missing,
        sort_field,
        sort_ascending,
        library_scroll_offset,
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

    let selected_persisted = selected_persisted_archive(cached, selected_archive.as_deref());
    let (operation_request, platform_request) = show_selected_archive(
        ui,
        selected_record(&data.records, selected_archive.as_deref()),
        selected_persisted,
        selected_platform_details(cached, selected_persisted),
        SelectedArchiveViewState {
            operation,
            busy,
            confirm_unmount,
            confirm_lazy_unmount,
            focus_lazy_cancel,
            lazy_unmount_offers,
            remount_offers,
            cleanup_after_unmount,
            platform_choice,
            platform_custom_text,
            platform_busy,
        },
    );
    if let Some(request) = operation_request {
        requested_action = Some(AppOperationRequest::Archive(request));
    }
    if let Some((archive_path, action)) = platform_request {
        requested_action = Some(AppOperationRequest::PlatformAssignment {
            archive_path,
            action,
        });
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

    // Requirement: the bulk platform action bar must render immediately
    // above the Search/filter controls, in the CentralPanel's ordinary
    // top-to-bottom flow - never after the table's ScrollAreas. Both
    // ScrollAreas below use `auto_shrink([false, false])`, which makes
    // them greedily claim *all* remaining vertical space in `ui`; a
    // widget placed after them in the same vertical layout would be
    // squeezed into whatever sliver of height (often zero) is left over,
    // which is why the bar previously never appeared despite a correct
    // selection count. Uses the exact same `selected_archives` `HashSet`
    // that `show_archive_rows` highlights rows from - never a second,
    // possibly-stale copy.
    if let Some(action) = show_bulk_platform_action_bar(
        ui,
        selected_archives,
        bulk_platform_choice,
        bulk_platform_busy,
    ) {
        requested_action = Some(AppOperationRequest::BulkPlatformAssignment {
            archive_paths: selected_archives.iter().cloned().collect(),
            kind: action,
        });
    }

    // Merged rows are rebuilt fresh every frame (cheap for realistic
    // library sizes, and always exactly consistent with the current
    // self.state/self.database_state - see build_display_rows). Only the
    // *cached* filtered_rows index list is invalidated on the discrete
    // events that actually change this merge (poll_load, poll_database_load),
    // not every frame - see ArchiveFsApp::poll_load/poll_database_load.
    let merged_rows = build_display_rows(&data.records, &data.rows, cached);

    let missing_count = merged_rows
        .iter()
        .filter(|row| row.origin == RowOrigin::CachedMissing)
        .count();
    let mut missing_only =
        library_filters.missing && !library_filters.present && !library_filters.awaiting_validation;
    let selected_missing = selected_missing_paths(cached, selected_archives);
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Missing catalogue entries: {missing_count}"));
        if ui
            .checkbox(&mut missing_only, "Show missing only")
            .changed()
        {
            set_missing_review_mode(library_filters, missing_only);
        }
        let enabled = missing_removal_available && selected_missing.is_ok();
        let response = ui.add_enabled(enabled, egui::Button::new(REMOVE_MISSING_CONFIRM_LABEL));
        if !enabled && let Err(reason) = &selected_missing {
            response.clone().on_hover_text(reason);
        }
        if response.clicked()
            && let Ok(paths) = &selected_missing
        {
            *confirm_remove_missing = Some(paths.clone());
        }
        if missing_removal_busy {
            ui.spinner();
            ui.label("Removing catalogue entries...");
        }
    });

    if let Some(paths) = confirm_remove_missing.clone() {
        let confirmation_selection: HashSet<PathBuf> = paths.iter().cloned().collect();
        let still_valid = selected_missing_paths(cached, &confirmation_selection).is_ok();
        egui::Window::new(format!(
            "Remove {} missing catalogue entr{}?",
            paths.len(),
            if paths.len() == 1 { "y" } else { "ies" }
        ))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(missing_removal_confirmation_text(paths.len()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(REMOVE_MISSING_CANCEL_LABEL).clicked() {
                    *confirm_remove_missing = None;
                }
                if ui
                    .add_enabled(
                        missing_removal_available && still_valid,
                        egui::Button::new(REMOVE_MISSING_CONFIRM_LABEL),
                    )
                    .clicked()
                {
                    requested_action = Some(AppOperationRequest::RemoveMissing(paths.clone()));
                    *confirm_remove_missing = None;
                }
            });
        });
    }

    let mut filter_changed = false;
    ui.horizontal(|ui| {
        ui.label("Search:");
        filter_changed |= ui
            .add(
                egui::TextEdit::singleline(filter)
                    .id(egui::Id::new(SEARCH_FILTER_TEXT_EDIT_ID))
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
        *filtered_rows = matching_row_indices(&merged_rows, filter);
    }

    // Unknown-platform review workflow: a compact count plus a single
    // "show unknown only" toggle, reusing the same `unknown_platform`
    // filter field the "Filters:" row's platform group already reads
    // (see `LibraryRowFilters`) - no separate filter state, no separate
    // index generation, no rescan. The count covers every merged row
    // (live and cache-only alike), independent of which filters are
    // currently active - see requirement 7.
    let unknown_count = merged_rows
        .iter()
        .filter(|row| row.unknown_platform)
        .count();
    let mut filters_changed = false;
    ui.horizontal(|ui| {
        ui.label(format!("Unknown platforms: {unknown_count}"));
        filters_changed |= ui
            .checkbox(&mut library_filters.unknown_platform, "Show unknown only")
            .changed();
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("Filters:");
        filters_changed |= ui
            .checkbox(&mut library_filters.present, "Present")
            .changed();
        filters_changed |= ui
            .checkbox(&mut library_filters.missing, "Missing")
            .changed();
        filters_changed |= ui
            .checkbox(
                &mut library_filters.awaiting_validation,
                "Awaiting validation",
            )
            .changed();
        filters_changed |= ui
            .checkbox(&mut library_filters.known_platform, "Known platform")
            .changed();
        if library_filters.is_active() && ui.small_button("Clear filters").clicked() {
            *library_filters = LibraryRowFilters::default();
            filters_changed = true;
        }
    });
    let _ = filters_changed;

    let base_indices: Vec<usize> = filtered_rows
        .clone()
        .unwrap_or_else(|| (0..merged_rows.len()).collect());
    let mut visible_indices: Vec<usize> = if library_filters.is_active() {
        base_indices
            .into_iter()
            .filter(|&index| library_filters.matches(&merged_rows[index]))
            .collect()
    } else {
        base_indices
    };
    // Milestone requirement 2: sorting reorders only this filtered index
    // list, never `merged_rows`/`data.records`/`data.rows` themselves, so
    // it can never mutate database order or archive identity.
    if let Some(field) = *sort_field {
        sort_visible_indices(&merged_rows, &mut visible_indices, field, *sort_ascending);
    }
    let visible_count = visible_indices.len();

    show_selection_controls_row(ui, &merged_rows, &visible_indices, selected_archives);
    ui.add_space(4.0);

    // Milestone requirement 1: Escape / Ctrl+A / arrow-key navigation.
    // Gated on `keyboard_shortcuts_blocked_by_focus` so typing Ctrl+A to
    // select all text in the Search box (or navigating an open platform
    // ComboBox with the arrow keys) is never hijacked into a table
    // selection change.
    // Set whenever this frame's arrow-key handling below actually moves
    // focus - names the newly-focused row's position in `visible_indices`
    // so the vertical `ScrollArea` built further down can scroll it into
    // view (Ctrl+Up/Down's "does not visibly move" fix: focus moving among
    // an existing multi-selection paints no different fill/border at all
    // unless it also happens to leave the visible viewport, so this alone
    // is not the fix - see `show_data_row`'s `focused` stroke for that
    // half - but a focus change that scrolls off-screen must still scroll
    // back into view).
    let mut requested_scroll_pos: Option<usize> = None;
    if !keyboard_shortcuts_blocked_by_focus(ui.ctx()) {
        let (escape_pressed, select_all_pressed, arrow_down_pressed, arrow_up_pressed, ctrl_held) =
            ui.input(|input| {
                (
                    input.key_pressed(egui::Key::Escape),
                    input.modifiers.ctrl && input.key_pressed(egui::Key::A),
                    input.key_pressed(egui::Key::ArrowDown),
                    input.key_pressed(egui::Key::ArrowUp),
                    input.modifiers.ctrl,
                )
            });

        if escape_pressed {
            selected_archives.clear();
        }
        if select_all_pressed {
            *selected_archives = select_all_visible(&merged_rows, &visible_indices);
        }
        if arrow_down_pressed || arrow_up_pressed {
            let direction = if arrow_down_pressed {
                ArrowDirection::Down
            } else {
                ArrowDirection::Up
            };
            if let Some(new_focus) = next_focus_in_visible_order(
                &merged_rows,
                &visible_indices,
                selected_archive.as_deref(),
                direction,
            ) {
                apply_arrow_focus_change(
                    selected_archives,
                    selected_archive,
                    new_focus.clone(),
                    ctrl_held,
                );
                requested_scroll_pos = visible_indices
                    .iter()
                    .position(|&index| merged_rows[index].path == new_focus);
            }
        }
    }

    let row_height = fixed_row_height(
        ui.text_style_height(&egui::TextStyle::Body),
        ui.spacing().interact_size.y,
    );
    let horizontal_spacing = ui.spacing().item_spacing.x;
    let selected_index = selected_row_index(&merged_rows, selected_archive.as_deref());

    // Milestone requirement 4: never show an empty table with no
    // explanation - distinguish "the library itself is empty" from "the
    // library has archives, but the current search/filters hide all of
    // them".
    match library_table_message(merged_rows.is_empty(), visible_count) {
        Some(LibraryTableMessage::EmptyLibrary) => {
            ui.label(EMPTY_LIBRARY_MESSAGE);
        }
        Some(LibraryTableMessage::NoFilterResults) => {
            ui.label(ZERO_FILTER_RESULTS_MESSAGE);
        }
        None => {
            let mut clicked = None;
            egui::ScrollArea::horizontal()
                .id_salt("archive_status_horizontal")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(table_width(horizontal_spacing));
                    if let Some(clicked_field) = show_header_row(
                        ui,
                        &COLUMN_HEADERS,
                        &COLUMN_SORT_FIELDS,
                        row_height,
                        *sort_field,
                        *sort_ascending,
                    ) {
                        apply_header_click(sort_field, sort_ascending, clicked_field);
                    }
                    ui.separator();

                    let body_height = ui.available_height().max(row_height);
                    let mut vertical_scroll_area = egui::ScrollArea::vertical()
                        .id_salt("archive_status_vertical")
                        .max_height(body_height)
                        .auto_shrink([false, false]);
                    if let Some(pos) = requested_scroll_pos {
                        let row_stride = row_height + ui.spacing().item_spacing.y;
                        vertical_scroll_area = vertical_scroll_area.vertical_scroll_offset(
                            compute_scroll_offset_for_focus(
                                pos,
                                row_stride,
                                *library_scroll_offset,
                                body_height,
                            ),
                        );
                    }
                    let scroll_output = vertical_scroll_area.show_rows(
                        ui,
                        row_height,
                        visible_count,
                        |ui, row_range| {
                            clicked = show_archive_rows(
                                ui,
                                &merged_rows,
                                Some(&visible_indices),
                                row_range,
                                row_height,
                                selected_index,
                                selected_archives,
                            );
                        },
                    );
                    *library_scroll_offset = scroll_output.state.offset.y;
                });
            // Requirement 2: an ordinary click replaces the whole selection
            // with just this row; a Ctrl-click toggles only this row,
            // leaving every other currently-selected row untouched. Either
            // way the details panel's "focused" row (selected_archive)
            // becomes whatever was just clicked, and its platform picker
            // resets - it must never keep showing a choice made for a
            // different, previously-focused archive.
            if let Some((index, ctrl_held)) = clicked {
                let path = merged_rows[index].path.clone();
                apply_row_click(selected_archives, selected_archive, path, ctrl_held);
                *platform_choice = None;
                platform_custom_text.clear();
            }
        }
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

/// Renders the column header row - milestone requirement 2: each cell is
/// its own small, borderless `Button`, clickable to select/toggle that
/// column's sort. This is a separate function from `show_data_row` and
/// renders outside any data row's own click-sensing `Rect`, so it does
/// not touch the Painter-based single-clickable-region row fix at all;
/// unlike a data row, there is no archive identity here that a second
/// child widget could ever steal a click from.
///
/// `sort_field`/`sort_ascending` describe the *current* sort (`None`
/// means unsorted / natural order); the active column's label gets a
/// small arrow suffix showing its direction. Returns the column whose
/// header was clicked this frame, if any - the caller decides whether
/// that selects a new sort field or toggles the existing one.
fn show_header_row(
    ui: &mut egui::Ui,
    cells: &[&str; 4],
    fields: &[SortField; 4],
    row_height: f32,
    sort_field: Option<SortField>,
    sort_ascending: bool,
) -> Option<SortField> {
    let mut clicked_field = None;
    ui.horizontal(|ui| {
        for ((text, width), field) in cells.iter().zip(COLUMN_WIDTHS).zip(fields.iter().copied()) {
            let label = if sort_field == Some(field) {
                format!(
                    "{text} {}",
                    if sort_ascending {
                        "\u{25B2}"
                    } else {
                        "\u{25BC}"
                    }
                )
            } else {
                (*text).to_string()
            };
            let response = ui
                .add_sized(
                    [width, row_height],
                    egui::Button::new(egui::RichText::new(label).strong()).frame(false),
                )
                .on_hover_text(*text);
            if response.clicked() {
                clicked_field = Some(field);
            }
        }
    });
    clicked_field
}

/// Renders one selectable archive table row as a *single* clickable
/// region (`Sense::click()` on one allocated `Rect`, identified by
/// `id_source` - the archive's exact path, never a lossy display string)
/// with the four cells' text painted passively inside it.
///
/// This replaced an earlier version that rendered each of the four cells
/// as its own separate `egui::Button`, with the row's overall
/// clicked-ness computed by OR-ing all four `Response::clicked()` values
/// together. That meant a row had no single, authoritative `Response` of
/// its own: four independent interactive widgets shared the row's
/// hover/press state, with real gaps between them (the `horizontal`
/// layout's item spacing) that belonged to no widget's sense area at
/// all, and Ctrl-click reliability regressed as a direct result - see the
/// fix for the real-world Nobara bug report this was rewritten for.
///
/// Cell text is painted directly with `Painter::text` rather than as
/// separate child `Label` widgets. This was not just a style choice:
/// registering more than one child widget inside the row's own interact
/// `Rect` (even purely non-interactive `Label`s, `Sense::hover()`-only)
/// was empirically confirmed, while fixing the Ctrl-click bug, to make
/// egui's hit-testing stop recognizing the row's *own* `Response` as
/// hovered/clicked at all in some cases - see the headless
/// `simulate_row_click`-based tests below, which reproduce this exact
/// failure mode against the old approach. Direct painting registers no
/// widgets at all, so there is nothing left inside the row that could
/// ever compete with its own click/hover sensing.
fn show_data_row(
    ui: &mut egui::Ui,
    cells: &[&str; 4],
    row_height: f32,
    id_source: &Path,
    multi_selected: bool,
    focused: bool,
    text_color: Option<egui::Color32>,
) -> egui::Response {
    let width = table_width(ui.spacing().item_spacing.x);
    // Reserve the row's layout space first (advancing the cursor exactly
    // as any other widget would), then sense clicks/hover for that exact
    // `Rect` under a stable `Id` derived from `id_source` - not egui's
    // auto-generated one. `show_rows` virtualizes this list, so the
    // *same* screen position can render a *different* archive across
    // scroll frames; a stable, identity-derived `Id` (rather than one
    // implied only by rendering order/position) means a press-then-scroll
    // gesture can never have its release misattributed to whatever
    // archive now happens to occupy that same position.
    let (_, rect) = ui.allocate_space(egui::vec2(width, row_height));
    let row_id = egui::Id::new("archive_table_row").with(id_source);
    let response = ui.interact(rect, row_id, egui::Sense::click());

    // Paint the background *before* the text, so a selected/hovered row
    // gets one clean, contiguous highlight across all four columns
    // (requirement: "a clearly visible selected background", not four
    // separately-tinted buttons with unhighlighted gaps between them).
    // `selection.bg_fill` and `hovered.weak_bg_fill` are egui's own
    // default palette entries - the same colors any ordinary selected or
    // hovered widget would use.
    let visuals = ui.visuals();
    if multi_selected {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
    }
    // The Ctrl+Up/Down "focus doesn't visibly move" fix: a *border*, not
    // another fill, so it stays visible whether or not this row is also
    // `multi_selected` - moving focus with Ctrl held between two rows that
    // are both already multi-selected must still show something change.
    // `warn_fg_color` is deliberately a different hue from
    // `selection.bg_fill`/`.stroke` so the two states never look like the
    // same highlight at a glance.
    if focused {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            0.0,
            egui::Stroke::new(2.0_f32, visuals.warn_fg_color),
            egui::StrokeKind::Inside,
        );
    }

    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = text_color.unwrap_or_else(|| ui.visuals().text_color());
    let spacing = ui.spacing().item_spacing.x;
    let mut x = rect.left();
    for (text, column_width) in cells.iter().zip(COLUMN_WIDTHS) {
        let cell_rect = egui::Rect::from_min_size(
            egui::pos2(x, rect.top()),
            egui::vec2(column_width, row_height),
        );
        ui.painter().with_clip_rect(cell_rect).text(
            egui::pos2(x + 2.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            font_id.clone(),
            color,
        );
        x += column_width + spacing;
    }

    response
}

/// Renders one page of table rows. A row can be `multi_selected` (a member
/// of the exact `ArchiveRow::path` identity set), the single "focused" row
/// (`selected_index`), both, or neither - these are rendered as visually
/// distinct states (see `show_data_row`), not collapsed into one "is
/// selected" flag, so that Ctrl+Up/Down moving focus among an existing
/// multi-selection is still visible. Returns `Some((row_index, ctrl_held))`
/// for the row clicked this frame, if any - `ctrl_held` is read once, from
/// the same frame's input state every row in this call shares, so the
/// caller can distinguish an ordinary click (replace the selection) from a
/// Ctrl-click (toggle just this row) without this function needing to know
/// anything about selection semantics itself.
fn show_archive_rows(
    ui: &mut egui::Ui,
    rows: &[ArchiveRow],
    filtered_rows: Option<&[usize]>,
    row_range: Range<usize>,
    row_height: f32,
    selected_index: Option<usize>,
    multi_selected: &HashSet<PathBuf>,
) -> Option<(usize, bool)> {
    let mut clicked = None;
    let visuals = ui.visuals().clone();
    let ctrl_held = ui.input(|input| input.modifiers.ctrl);
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
        let is_multi_selected = multi_selected.contains(&row.path);
        let is_focused = selected_index == Some(row_index);
        let response = show_data_row(
            ui,
            &cells,
            row_height,
            &row.path,
            is_multi_selected,
            is_focused,
            row.row_text_color(&visuals),
        );
        if response.clicked() {
            clicked = Some((row_index, ctrl_held));
        }
    }
    clicked
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

/// Like `selected_record_index`, but over the merged live+cache row list
/// via each row's exact-byte `path` identity - never a lossy display
/// string (requirement 5). Used to drive table-row highlighting for both
/// live and cache-only rows; selecting a cache-only row still leaves
/// `selected_record` (which only searches live records) returning `None`,
/// so no action button is ever offered for it.
fn selected_row_index(rows: &[ArchiveRow], selected_archive: Option<&Path>) -> Option<usize> {
    let selected_archive = selected_archive?;
    rows.iter().position(|row| row.path == selected_archive)
}

/// Applies one row click to the selection state - requirement 2's exact
/// semantics. An ordinary click (`ctrl_held = false`) replaces the whole
/// multi-selection with just `path`; a Ctrl-click toggles only `path`,
/// leaving every other currently-selected row untouched. Either way,
/// `selected_archive` (the details panel's "focused" row) becomes
/// `path`. Factored out from `show_loaded_data`'s row-click handling so
/// it is directly testable without an `egui::Ui`.
fn apply_row_click(
    selected_archives: &mut HashSet<PathBuf>,
    selected_archive: &mut Option<PathBuf>,
    path: PathBuf,
    ctrl_held: bool,
) {
    if ctrl_held {
        if !selected_archives.remove(&path) {
            selected_archives.insert(path.clone());
        }
    } else {
        selected_archives.clear();
        selected_archives.insert(path.clone());
    }
    *selected_archive = Some(path);
}

/// Whether the compact bulk platform action bar should be shown -
/// requirement 3: only when more than one row is selected. Factored out
/// as its own pure predicate (mirroring `mount_all_available`) so the
/// condition is directly testable without an `egui::Ui`.
fn bulk_action_bar_visible(selected_archives: &HashSet<PathBuf>) -> bool {
    selected_archives.len() > 1
}

/// Whether "Select all visible" should be enabled - requirement 6:
/// disabled whenever the current search/filters leave zero library rows
/// visible. Factored out as its own pure predicate (mirroring
/// `mount_all_available`/`bulk_action_bar_visible`) so it is directly
/// testable without an `egui::Ui`.
fn select_all_visible_button_enabled(visible_count: usize) -> bool {
    visible_count > 0
}

/// A compact, always-visible summary of the multi-selection's size -
/// milestone requirement 3. Shown unconditionally, unlike
/// `show_bulk_platform_action_bar` (2+ rows only), so a single selected
/// row is still visibly confirmed somewhere other than the row's
/// background colour.
fn selection_status_text(selected_count: usize) -> String {
    match selected_count {
        0 => "No archives selected".to_string(),
        1 => "1 archive selected".to_string(),
        n => format!("{n} archives selected"),
    }
}

/// Renders the "Showing X of Y archives" / selection-status / "Select all
/// visible" row - the ordinary-library selection controls that sit above
/// the table, next to the always-visible `selection_status_text` label
/// (see that function's doc comment) and near the bulk action bar's
/// "Clear selected"/"Clear selection" (`show_bulk_platform_action_bar`).
/// Factored out from `show_loaded_data` (mirroring
/// `show_bulk_platform_action_bar`) so it can be rendered and click-tested
/// standalone.
///
/// v0.4.2-alpha follow-up requirement: "Select all visible" is a
/// mouse-only equivalent of Ctrl+A. It calls `select_all_visible` with
/// this same frame's own `merged_rows`/`visible_indices` - the exact same
/// helper and inputs the Ctrl+A handler in `show_loaded_data` dispatches
/// to - so there is no second selection implementation to drift out of
/// sync with search/filters/sort. Disabled whenever zero rows are
/// currently visible; clicking it while every visible row is already
/// selected is a no-op rebuild of the identical `HashSet`.
fn show_selection_controls_row(
    ui: &mut egui::Ui,
    merged_rows: &[ArchiveRow],
    visible_indices: &[usize],
    selected_archives: &mut HashSet<PathBuf>,
) {
    let visible_count = visible_indices.len();
    ui.horizontal(|ui| {
        ui.label(format!(
            "Showing {} of {} archives",
            visible_count,
            merged_rows.len()
        ));
        ui.separator();
        // Milestone requirement 3: always visible, unlike the bulk action
        // bar (2+ selections only) - a single selection is otherwise only
        // shown via the selected row's background colour.
        ui.label(selection_status_text(selected_archives.len()));
        ui.separator();
        if ui
            .add_enabled(
                select_all_visible_button_enabled(visible_count),
                egui::Button::new("Select all visible"),
            )
            .clicked()
        {
            *selected_archives = select_all_visible(merged_rows, visible_indices);
        }
    });
}

const EMPTY_LIBRARY_MESSAGE: &str =
    "No archives in the library yet. Scan a source folder to add archives.";
const ZERO_FILTER_RESULTS_MESSAGE: &str = "No archives match the current search and filters.";

/// Requirement 4: distinguishes "the library itself has no archives at
/// all" from "the library has archives, but the current search/filters
/// hide every one of them" - factored out as its own pure predicate
/// (mirroring `bulk_action_bar_visible`) so the choice of message is
/// directly testable without an `egui::Ui`, and so the table is never
/// left rendering as an unexplained blank area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryTableMessage {
    EmptyLibrary,
    NoFilterResults,
}

fn library_table_message(
    merged_rows_is_empty: bool,
    visible_count: usize,
) -> Option<LibraryTableMessage> {
    if merged_rows_is_empty {
        Some(LibraryTableMessage::EmptyLibrary)
    } else if visible_count == 0 {
        Some(LibraryTableMessage::NoFilterResults)
    } else {
        None
    }
}

/// The four sortable table columns - milestone requirement 2. Order
/// matches `COLUMN_HEADERS`/`COLUMN_WIDTHS` exactly (see
/// `COLUMN_SORT_FIELDS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortField {
    Platform,
    State,
    ArchivePath,
    MountPath,
}

const COLUMN_SORT_FIELDS: [SortField; 4] = [
    SortField::Platform,
    SortField::State,
    SortField::ArchivePath,
    SortField::MountPath,
];

fn sort_field_key(row: &ArchiveRow, field: SortField) -> &str {
    match field {
        SortField::Platform => &row.platform,
        SortField::State => &row.state,
        SortField::ArchivePath => &row.archive_path,
        SortField::MountPath => &row.mount_path,
    }
}

/// Sorts `indices` (a filtered view into `merged_rows`, never the rows
/// themselves - requirement 2: "must not mutate database order or
/// archive identity") by the chosen column. `Vec::sort_by` is a stable
/// sort, and the exact `ArchiveRow::path` is always the final
/// tie-breaker (in fixed ascending order, independent of `ascending`, so
/// ties resolve identically either direction) - together these make the
/// result fully deterministic regardless of `merged_rows`'s incoming
/// order.
fn sort_visible_indices(
    merged_rows: &[ArchiveRow],
    indices: &mut [usize],
    field: SortField,
    ascending: bool,
) {
    indices.sort_by(|&left, &right| {
        let left_row = &merged_rows[left];
        let right_row = &merged_rows[right];
        let primary = sort_field_key(left_row, field).cmp(sort_field_key(right_row, field));
        let primary = if ascending {
            primary
        } else {
            primary.reverse()
        };
        primary.then_with(|| left_row.path.cmp(&right_row.path))
    });
}

/// Applies one header click to the current sort state - requirement 2:
/// clicking a new column selects it (starting ascending); clicking the
/// already-active column toggles its direction. Factored out from
/// `show_loaded_data` so this decision is directly testable without an
/// `egui::Ui`.
fn apply_header_click(
    sort_field: &mut Option<SortField>,
    sort_ascending: &mut bool,
    clicked_field: SortField,
) {
    if *sort_field == Some(clicked_field) {
        *sort_ascending = !*sort_ascending;
    } else {
        *sort_field = Some(clicked_field);
        *sort_ascending = true;
    }
}

/// Requirement 1: Ctrl+A must select exactly the archives currently
/// visible after filters are applied - never a hidden/filtered-out row.
/// Paths are cloned directly out of `merged_rows` at the positions
/// `visible_indices` names, computed fresh this same frame, so this can
/// never select against a stale filter/sort state from an earlier frame.
fn select_all_visible(merged_rows: &[ArchiveRow], visible_indices: &[usize]) -> HashSet<PathBuf> {
    visible_indices
        .iter()
        .map(|&index| merged_rows[index].path.clone())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArrowDirection {
    Up,
    Down,
}

/// Requirement 1: computes the next focused archive for Up/Down,
/// stepping strictly through `visible_indices` - the exact filtered *and
/// sorted* order currently on screen - by searching for the current
/// focus's exact path rather than trusting any previously-computed row
/// index. A raw index saved from an earlier frame can be invalidated by
/// a filter or sort change between frames; re-deriving the position from
/// `current_focus`'s `PathBuf` identity every call means there is no
/// stale index to go stale. Clamps at either end rather than wrapping.
fn next_focus_in_visible_order(
    merged_rows: &[ArchiveRow],
    visible_indices: &[usize],
    current_focus: Option<&Path>,
    direction: ArrowDirection,
) -> Option<PathBuf> {
    if visible_indices.is_empty() {
        return None;
    }
    let current_pos = current_focus.and_then(|path| {
        visible_indices
            .iter()
            .position(|&index| merged_rows[index].path == path)
    });
    let next_pos = match (current_pos, direction) {
        (Some(pos), ArrowDirection::Down) => (pos + 1).min(visible_indices.len() - 1),
        (Some(pos), ArrowDirection::Up) => pos.saturating_sub(1),
        (None, _) => 0,
    };
    Some(merged_rows[visible_indices[next_pos]].path.clone())
}

/// Applies one Up/Down focus change - requirement 1's exact semantics.
/// Moving focus without Ctrl replaces the whole multi-selection with
/// just the newly-focused row; with Ctrl held, only the focus itself
/// moves and the multi-selection is left untouched (Shift-range
/// selection is explicitly out of scope for this milestone).
fn apply_arrow_focus_change(
    selected_archives: &mut HashSet<PathBuf>,
    selected_archive: &mut Option<PathBuf>,
    new_focus: PathBuf,
    ctrl_held: bool,
) {
    if !ctrl_held {
        selected_archives.clear();
        selected_archives.insert(new_focus.clone());
    }
    *selected_archive = Some(new_focus);
}

/// Auto-scroll fix for Ctrl+Up/Down: computes the vertical `ScrollArea`
/// offset needed to bring the row at visible position `focus_pos` (rows
/// `row_stride` pixels apart) into view, given the scroll area's
/// `current_offset` (its own offset as of the end of the previous frame)
/// and `viewport_height`. Performs the smallest scroll that satisfies
/// this: if the row is already fully within
/// `current_offset..current_offset + viewport_height`, `current_offset` is
/// returned unchanged (no jump on every keypress); otherwise the offset is
/// clamped to align the row to whichever edge it just crossed.
fn compute_scroll_offset_for_focus(
    focus_pos: usize,
    row_stride: f32,
    current_offset: f32,
    viewport_height: f32,
) -> f32 {
    let row_top = focus_pos as f32 * row_stride;
    let row_bottom = row_top + row_stride;
    if row_top < current_offset {
        row_top
    } else if row_bottom > current_offset + viewport_height {
        (row_bottom - viewport_height).max(0.0)
    } else {
        current_offset
    }
}

/// Requirement 1's last bullet: keyboard shortcuts (Escape, Ctrl+A,
/// arrow navigation) must not fire while a text field or `ComboBox` is
/// actively receiving keyboard input. `mem.focused()` is `Some` exactly
/// when a widget (a `TextEdit`'s cursor, for example) currently holds
/// keyboard focus; `Popup::is_any_open` additionally covers an open
/// `ComboBox` dropdown, which does not itself hold "focus" in that sense
/// but should equally suppress these shortcuts while its own keyboard
/// navigation is active.
fn keyboard_shortcuts_blocked_by_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.focused().is_some()) || egui::Popup::is_any_open(ctx)
}

/// The persisted database row backing the selected archive, if the
/// library database knows about it - live or cache-only alike, unlike
/// `selected_record` (live only). This is what makes manual platform
/// assignment available for a cache-only/missing row: it is metadata
/// only, never a mount action, so it does not need `selected_record`'s
/// live-only restriction. Matches by exact path bytes (`PersistedArchive::absolute_path`),
/// never a lossy display string.
fn selected_persisted_archive<'a>(
    cached: Option<&'a CachedLibrarySnapshot>,
    selected_archive: Option<&Path>,
) -> Option<&'a PersistedArchive> {
    let selected_archive = selected_archive?;
    cached?
        .archives
        .iter()
        .find(|persisted| persisted.absolute_path == selected_archive)
}

fn selected_platform_details<'a>(
    cached: Option<&'a CachedLibrarySnapshot>,
    persisted: Option<&PersistedArchive>,
) -> Option<&'a PlatformProvenanceDetails> {
    cached?.platform_details.get(&persisted?.id)
}

/// Resolves the platform text a "Set Platform" click should apply:
/// `platform_custom_text` (trimmed, rejecting empty) when
/// `CUSTOM_PLATFORM_CHOICE` is selected, otherwise the selected
/// canonical name directly. `None` means nothing valid to apply yet
/// (no selection, or an empty custom field) - the caller uses this to
/// keep "Set Platform" disabled.
fn resolved_platform_choice<'a>(choice: Option<&'a str>, custom_text: &'a str) -> Option<&'a str> {
    match choice {
        Some(CUSTOM_PLATFORM_CHOICE) => {
            let trimmed = custom_text.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        Some(name) => Some(name),
        None => None,
    }
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
    platform_choice: &'a mut Option<String>,
    platform_custom_text: &'a mut String,
    platform_busy: bool,
}

fn show_selected_archive(
    ui: &mut egui::Ui,
    record: Option<&ArchiveRecord>,
    persisted: Option<&PersistedArchive>,
    platform_details: Option<&PlatformProvenanceDetails>,
    view_state: SelectedArchiveViewState<'_>,
) -> (Option<OperationRequest>, Option<(PathBuf, PlatformAction)>) {
    let SelectedArchiveViewState {
        operation,
        busy,
        confirm_unmount,
        confirm_lazy_unmount,
        focus_lazy_cancel,
        lazy_unmount_offers,
        remount_offers,
        cleanup_after_unmount,
        platform_choice,
        platform_custom_text,
        platform_busy,
    } = view_state;
    let mut request = None;
    let mut platform_request = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong("Selected archive");
        if record.is_none() && persisted.is_none() {
            ui.label("Select an archive row to view details.");
            return;
        }

        let Some(record) = record else {
            if let Some(persisted) = persisted {
                ui.label(format!(
                    "Archive path: {}",
                    persisted.absolute_path.display()
                ));
                if persisted.last_verified_missing_at.is_some() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "Status: Missing from the latest successful source-folder scan",
                    );
                    ui.label(format!("Last seen: {}", persisted.last_seen_at));
                }
                ui.label(
                    "Known to the library database, not confirmed by the latest live snapshot. \
                     Mount/unmount actions are unavailable until it is - platform assignment \
                     below is metadata only and unaffected.",
                );
            }
            let action = show_platform_section(
                ui,
                persisted,
                platform_details,
                platform_choice,
                platform_custom_text,
                platform_busy,
            );
            if let (Some(persisted), Some(action)) = (persisted, action) {
                platform_request = Some((persisted.absolute_path.clone(), action));
            }
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

        let action = show_platform_section(
            ui,
            persisted,
            platform_details,
            platform_choice,
            platform_custom_text,
            platform_busy,
        );
        if let Some(action) = action {
            platform_request = Some((record.mount_plan.archive.path.clone(), action));
        }
    });
    (request, platform_request)
}

/// Renders the "Set platform" / "Clear manual platform" controls tucked
/// into the selected-archive details - available whenever `persisted` is
/// `Some` (the library database knows this archive), live or cache-only
/// row alike, since this is metadata only, never a mount action (see
/// `show_selected_archive`'s two call sites above). Uses
/// `canonical_platform_names` (the same central list the CLI's
/// `library-set-platform` validates against - never a second,
/// independently-drifting list here), with `CUSTOM_PLATFORM_CHOICE` as
/// the escape hatch for a platform not in that list, mirroring the CLI's
/// `--custom` flag.
fn show_platform_section(
    ui: &mut egui::Ui,
    persisted: Option<&PersistedArchive>,
    platform_details: Option<&PlatformProvenanceDetails>,
    platform_choice: &mut Option<String>,
    platform_custom_text: &mut String,
    platform_busy: bool,
) -> Option<PlatformAction> {
    ui.add_space(6.0);
    ui.separator();
    ui.strong("Platform");
    let Some(persisted) = persisted else {
        ui.label(
            "Not yet in the library database. Run a library scan to enable platform assignment.",
        );
        return None;
    };

    let fallback_details;
    let details = if let Some(details) = platform_details {
        details
    } else {
        fallback_details = PlatformProvenanceDetails {
            platform: persisted.platform.clone(),
            source: persisted.platform_source.clone(),
            matched_component: None,
            automatic_fallback: None,
        };
        &fallback_details
    };
    for (label, value) in platform_provenance_lines(details) {
        ui.label(format!("{label}: {value}"));
    }
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("platform_choice_combo")
            .selected_text(platform_choice.as_deref().unwrap_or("Select platform..."))
            .show_ui(ui, |ui| {
                for name in canonical_platform_names() {
                    ui.selectable_value(platform_choice, Some(name.to_string()), name);
                }
                ui.selectable_value(
                    platform_choice,
                    Some(CUSTOM_PLATFORM_CHOICE.to_string()),
                    CUSTOM_PLATFORM_CHOICE,
                );
            });
        if platform_choice.as_deref() == Some(CUSTOM_PLATFORM_CHOICE) {
            ui.text_edit_singleline(platform_custom_text);
        }
    });
    let resolved = resolved_platform_choice(platform_choice.as_deref(), platform_custom_text)
        .map(str::to_string);
    let mut platform_request = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !platform_busy && resolved.is_some(),
                egui::Button::new("Set Platform"),
            )
            .clicked()
            && let Some(platform) = resolved
        {
            platform_request = Some(PlatformAction::Set(platform));
        }
        if persisted.platform_source.as_deref() == Some(MANUAL_PLATFORM_SOURCE)
            && ui
                .add_enabled(!platform_busy, egui::Button::new("Clear Manual Platform"))
                .clicked()
        {
            platform_request = Some(PlatformAction::Clear);
        }
        if platform_busy {
            ui.spinner();
            ui.label("Updating platform...");
        }
    });
    platform_request
}

/// Renders the compact bulk platform action bar - shown only when more
/// than one row is selected (requirement 3): a single selected row
/// already has its own platform picker in the details panel
/// (`show_platform_section`), and showing both for one row would be
/// redundant and ambiguous about which one actually applies. Uses
/// `canonical_platform_names()` (the same central list
/// `show_platform_section`/the CLI validate against) with no free-form
/// custom-text escape hatch - deliberately narrower than the single-row
/// picker, matching the bulk feature's "simple by default" scope
/// (requirement 4).
/// Renders the compact bulk platform action bar - shown only when more
/// than one row is selected (requirement 3): a single selected row
/// already has its own platform picker in the details panel
/// (`show_platform_section`), and showing both for one row would be
/// redundant and ambiguous about which one actually applies. Uses
/// `canonical_platform_names()` (the same central list
/// `show_platform_section`/the CLI validate against) with no free-form
/// custom-text escape hatch - deliberately narrower than the single-row
/// picker, matching the bulk feature's "simple by default" scope
/// (requirement 4).
///
/// Takes `selected_archives` by `&mut` - not because this function
/// starts any database write itself (it only ever returns the requested
/// `BulkPlatformActionKind` for the caller to dispatch asynchronously,
/// exactly as before), but because "Clear selection" is a purely local,
/// synchronous UI action with nothing to dispatch: it just empties the
/// *same* `HashSet` `show_archive_rows` highlights rows from, directly.
fn show_bulk_platform_action_bar(
    ui: &mut egui::Ui,
    selected_archives: &mut HashSet<PathBuf>,
    bulk_platform_choice: &mut Option<String>,
    bulk_platform_busy: bool,
) -> Option<BulkPlatformActionKind> {
    if !bulk_action_bar_visible(selected_archives) {
        return None;
    }

    let mut action = None;
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .show(ui, |ui| {
            ui.strong(format!("{} archives selected", selected_archives.len()));
            ui.horizontal(|ui| {
                ui.label("Platform:");
                egui::ComboBox::from_id_salt("bulk_platform_choice_combo")
                    .selected_text(
                        bulk_platform_choice
                            .as_deref()
                            .unwrap_or("Select platform..."),
                    )
                    .show_ui(ui, |ui| {
                        for name in canonical_platform_names() {
                            ui.selectable_value(bulk_platform_choice, Some(name.to_string()), name);
                        }
                    });
                if ui
                    .add_enabled(
                        !bulk_platform_busy && bulk_platform_choice.is_some(),
                        egui::Button::new("Set selected"),
                    )
                    .clicked()
                    && let Some(platform) = bulk_platform_choice.clone()
                {
                    action = Some(BulkPlatformActionKind::Set(platform));
                }
                if ui
                    .add_enabled(!bulk_platform_busy, egui::Button::new("Clear selected"))
                    .clicked()
                {
                    action = Some(BulkPlatformActionKind::Clear);
                }
                if ui.button("Clear selection").clicked() {
                    selected_archives.clear();
                }
                if bulk_platform_busy {
                    ui.spinner();
                    ui.label("Updating...");
                }
            });
        });
    ui.add_space(4.0);
    action
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
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    fn row(search_text: &str) -> ArchiveRow {
        ArchiveRow {
            path: PathBuf::new(),
            archive_path: String::new(),
            mount_path: String::new(),
            platform: String::new(),
            state: String::new(),
            search_text: search_text.to_lowercase(),
            origin: RowOrigin::Live,
            unknown_platform: false,
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

    fn default_config_identity() -> ConfigIdentity {
        ConfigIdentity {
            config_path: Some(PathBuf::from("/config/archivefs.toml")),
            content_digest: Some([1; 32]),
        }
    }

    fn app_for_operation_tests() -> ArchiveFsApp {
        ArchiveFsApp {
            state: LoadState::Ready(Box::new(empty_loaded_data("/mount"))),
            database_state: DatabaseState::NotCreated {
                database_path: PathBuf::from("/config/library.sqlite3"),
            },
            database_generation: DatabaseGeneration::INITIAL,
            library_filters: LibraryRowFilters::default(),
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
            diagnostics: DiagnosticsState::Ready {
                generation: RefreshGeneration::INITIAL,
                report: setup_report(true, true),
            },
            show_diagnostics: false,
            setup_action: None,
            refresh_error: None,
            snapshot_stale: false,
            refresh_generation: RefreshGeneration::INITIAL,
            snapshot_generation: Some(RefreshGeneration::INITIAL),
            platform_action: None,
            platform_choice: None,
            platform_custom_text: String::new(),
            alias_action: None,
            missing_removal: None,
            confirm_remove_missing: None,
            new_alias_text: String::new(),
            new_alias_platform_choice: None,
            selected_archives: HashSet::new(),
            bulk_platform_action: None,
            bulk_platform_choice: None,
            sort_field: None,
            sort_ascending: true,
            library_scroll_offset: 0.0,
            show_duplicate_review: false,
            duplicate_filters: DuplicateReviewFilters::initial(),
            duplicate_sort_field: DuplicateSortField::Title,
            duplicate_sort_ascending: true,
            selected_duplicate_group: None,
            selected_duplicate_archive: None,
        }
    }

    fn setup_report(ready_for_scanning: bool, ready_for_actions: bool) -> SetupDiagnostics {
        SetupDiagnostics {
            config_path: Some(PathBuf::from("/config/archivefs.toml")),
            config_path_error: None,
            config_missing: false,
            mount_root: Some(PathBuf::from("/mount")),
            can_create_mount_root: false,
            ready_for_scanning,
            ready_for_actions,
            config_identity: default_config_identity(),
            checks: Vec::new(),
        }
    }

    fn empty_loaded_data(mount_root: &str) -> LoadedData {
        LoadedData {
            mount_root: PathBuf::from(mount_root),
            records: Vec::new(),
            rows: Vec::new(),
            stats: ArchiveStats {
                total_archives: 0,
                mounted_count: 0,
                pending_count: 0,
                platform_counts: Vec::new(),
                extension_counts: Vec::new(),
                largest_archive: None,
                smallest_archive: None,
                total_size_bytes: 0,
            },
            doctor: DoctorReport {
                config_path: PathBuf::from("/config/archivefs.toml"),
                checks: Vec::new(),
                archives_found: 0,
                archives_with_platform: 0,
                archives_unknown_platform: 0,
                unknown_platform_examples: Vec::new(),
                platform_counts: Vec::new(),
                pending_archives: 0,
                mounted_archives: 0,
            },
            config_identity: default_config_identity(),
        }
    }

    /// A fully-populated `ArchiveRow` with every displayable field set
    /// explicitly - unlike `row()` above (search-text only, everything
    /// else blank), this is what the milestone's sort/keyboard-nav tests
    /// need: distinct `path`/`platform`/`state`/`archive_path`/
    /// `mount_path` values to actually exercise ordering.
    fn row_with_fields(
        path: &str,
        platform: &str,
        state: &str,
        archive_path: &str,
        mount_path: &str,
    ) -> ArchiveRow {
        ArchiveRow {
            path: PathBuf::from(path),
            archive_path: archive_path.to_string(),
            mount_path: mount_path.to_string(),
            platform: platform.to_string(),
            state: state.to_string(),
            search_text: format!("{archive_path}\n{mount_path}\n{platform}\n{state}")
                .to_lowercase(),
            origin: RowOrigin::Live,
            unknown_platform: false,
        }
    }

    /// Like `empty_loaded_data`, but with `rows` populated directly -
    /// `records` stays empty and `cached` stays `None` at every call
    /// site that uses this, so `build_display_rows` always passes these
    /// rows straight through unchanged (see its `cached.is_none()`
    /// short-circuit), making `data.rows` and `merged_rows` identical for
    /// these tests.
    fn loaded_data_with_rows(mount_root: &str, rows: Vec<ArchiveRow>) -> LoadedData {
        LoadedData {
            rows,
            ..empty_loaded_data(mount_root)
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

    // -----------------------------------------------------------------
    // Stage 4: persistent library database GUI integration - helpers.
    // -----------------------------------------------------------------

    /// A unique per-test temporary directory, following the exact
    /// pattern `archivefs-core/src/database.rs`'s own test module uses
    /// (no `tempfile` dependency in this workspace) - see requirement 8:
    /// every stage 4 test that touches real paths uses one of these, and
    /// none of them ever touch the real `HOME`/config/database path.
    fn database_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "archivefs-gui-database-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_archive_file(dir: &Path, relative_path: &str, content: &[u8]) -> PathBuf {
        let full_path = dir.join(relative_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full_path, content).unwrap();
        full_path
    }

    fn config_for(source_dir: &Path, mount_dir: &Path) -> Config {
        Config {
            source_folders: vec![source_dir.to_path_buf()],
            mount_root: mount_dir.to_path_buf(),
            ratarmount_bin: "ratarmount".to_string(),
        }
    }

    fn record_at(path: PathBuf, mount_state: MountState) -> ArchiveRecord {
        let archive = Archive::from_path(&path).unwrap();
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

    fn row_for(record: &ArchiveRecord) -> ArchiveRow {
        let status = ArchiveStatus {
            archive_path: record.mount_plan.archive.path.clone(),
            mount_path: record.mount_plan.mount_path.clone(),
            state: record.mount_state,
        };
        ArchiveRow::new(record, &status)
    }

    fn persisted_archive(path: PathBuf, missing: bool) -> PersistedArchive {
        PersistedArchive {
            id: 1,
            source_folder_id: 1,
            relative_path: PathBuf::from(path.file_name().unwrap()),
            absolute_path: path,
            archive_kind: "zip".to_string(),
            display_name: "Test Archive".to_string(),
            normalized_name: "test archive".to_string(),
            size_bytes: Some(1024),
            modified_time_unix_seconds: Some(0),
            platform: None,
            platform_source: None,
            last_known_health: "Pending".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_verified_missing_at: missing.then(|| "2026-01-01T00:00:00Z".to_string()),
        }
    }

    fn persisted_archive_with_platform(
        path: PathBuf,
        id: i64,
        platform: &str,
        source: &str,
    ) -> PersistedArchive {
        PersistedArchive {
            platform: Some(platform.to_string()),
            platform_source: Some(source.to_string()),
            id,
            ..persisted_archive(path, false)
        }
    }

    fn empty_catalogue_stats() -> CatalogueStats {
        CatalogueStats {
            total_archives: 0,
            present_archives: 0,
            missing_archives: 0,
            archives_with_platform: 0,
            archives_unknown_platform: 0,
        }
    }

    fn cached_snapshot(archives: Vec<PersistedArchive>) -> CachedLibrarySnapshot {
        let platform_details = archives
            .iter()
            .map(|archive| {
                (
                    archive.id,
                    PlatformProvenanceDetails {
                        platform: archive.platform.clone(),
                        source: archive.platform_source.clone(),
                        matched_component: None,
                        automatic_fallback: None,
                    },
                )
            })
            .collect();
        let duplicate_report = catalogue_filename_duplicates(&archives);
        CachedLibrarySnapshot {
            database_path: PathBuf::from("/config/library.sqlite3"),
            schema_version: latest_schema_version(),
            archives,
            platform_details,
            stats: empty_catalogue_stats(),
            last_completed_scan: None,
            platform_aliases: Vec::new(),
            duplicate_report,
        }
    }

    fn duplicate_catalogue_for_gui() -> Vec<PersistedArchive> {
        let mut first = persisted_archive_with_platform(
            PathBuf::from("/roms/a/Sonic the Hedgehog.zip"),
            1,
            "Mega Drive",
            "heuristic-path-detector",
        );
        first.display_name = "Sonic the Hedgehog".to_string();
        let mut second = persisted_archive_with_platform(
            PathBuf::from("/backup/Sonic the Hedgehog.7z"),
            2,
            "Mega Drive",
            "heuristic-path-detector",
        );
        second.display_name = "Sonic the Hedgehog".to_string();
        second.size_bytes = Some(2048);
        second.last_verified_missing_at = Some("2026-02-01T00:00:00Z".to_string());
        let mut third = persisted_archive_with_platform(
            PathBuf::from("/roms/a/Another Game.zip"),
            3,
            "SNES",
            "heuristic-path-detector",
        );
        third.display_name = "Another Game".to_string();
        let mut fourth = persisted_archive_with_platform(
            PathBuf::from("/backup/Another_Game.7z"),
            4,
            "SNES",
            "heuristic-path-detector",
        );
        fourth.display_name = "Another Game".to_string();
        vec![first, second, third, fourth]
    }

    #[test]
    fn duplicate_review_filters_count_groups_entries_and_exact_paths() {
        let report = catalogue_filename_duplicates(&duplicate_catalogue_for_gui());
        let mut filters = DuplicateReviewFilters::initial();

        let all =
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Title, true);
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter()
                .map(|index| report.groups[*index].entries.len())
                .sum::<usize>(),
            4
        );
        assert!(report.groups.iter().any(|group| {
            group
                .entries
                .iter()
                .any(|entry| entry.path == Path::new("/backup/Sonic the Hedgehog.7z"))
        }));

        filters.search = "/backup/sonic".to_string();
        let searched =
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Title, true);
        assert_eq!(searched.len(), 1);
        assert_eq!(report.groups[searched[0]].platform, "Mega Drive");

        filters.search.clear();
        filters.platform = Some("SNES".to_string());
        assert_eq!(
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Title, true)
                .len(),
            1
        );
    }

    #[test]
    fn duplicate_review_include_missing_and_more_than_two_filters_are_truthful() {
        let mut archives = duplicate_catalogue_for_gui();
        let mut third_sonic = persisted_archive_with_platform(
            PathBuf::from("/old/Sonic the Hedgehog.rar"),
            5,
            "Mega Drive",
            "heuristic-path-detector",
        );
        third_sonic.last_verified_missing_at = Some("2026-02-02T00:00:00Z".to_string());
        archives.push(third_sonic);
        let report = catalogue_filename_duplicates(&archives);
        let mut filters = DuplicateReviewFilters::initial();
        filters.more_than_two = true;
        assert_eq!(
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Entries, true)
                .len(),
            1
        );

        filters.include_missing = false;
        assert!(
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Entries, true)
                .is_empty()
        );
    }

    #[test]
    fn duplicate_review_sorting_is_deterministic_with_stable_tiebreakers() {
        let report = catalogue_filename_duplicates(&duplicate_catalogue_for_gui());
        let filters = DuplicateReviewFilters::initial();
        let first =
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Entries, true);
        let second =
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Entries, true);
        assert_eq!(first, second);
        assert_eq!(report.groups[first[0]].normalized_title, "another_game");
        let descending =
            visible_duplicate_group_indices(&report, &filters, DuplicateSortField::Title, false);
        assert_eq!(
            report.groups[descending[0]].normalized_title,
            "sonic_the_hedgehog"
        );
    }

    #[test]
    fn duplicate_review_state_is_separate_from_library_state_and_activity() {
        let mut app = app_for_operation_tests();
        app.filter = "ordinary search".to_string();
        app.library_filters.missing = true;
        app.sort_field = Some(SortField::State);
        app.selected_archive = Some(PathBuf::from("/roms/library.zip"));
        let history_len = app.history.entries.len();

        app.show_duplicate_review = true;
        app.duplicate_filters.search = "sonic".to_string();
        app.selected_duplicate_archive = Some(PathBuf::from("/backup/Sonic.7z"));
        app.show_duplicate_review = false;

        assert_eq!(app.filter, "ordinary search");
        assert!(app.library_filters.missing);
        assert_eq!(app.sort_field, Some(SortField::State));
        assert_eq!(
            app.selected_archive,
            Some(PathBuf::from("/roms/library.zip"))
        );
        assert_eq!(app.history.entries.len(), history_len);
    }

    #[test]
    fn duplicate_cache_rebuilds_after_platform_change_and_catalogue_cleanup() {
        let archives = duplicate_catalogue_for_gui();
        let original = cached_snapshot(archives.clone());
        assert_eq!(original.duplicate_report.groups.len(), 2);

        let mut platform_changed = archives.clone();
        platform_changed[1].platform = Some("Master System".to_string());
        let regrouped = cached_snapshot(platform_changed);
        assert_eq!(regrouped.duplicate_report.groups.len(), 1);

        let cleaned = cached_snapshot(archives.into_iter().skip(1).collect());
        assert_eq!(cleaned.duplicate_report.groups.len(), 1);
        assert!(
            cleaned
                .duplicate_report
                .groups
                .iter()
                .all(|group| group.normalized_title != "sonic_the_hedgehog")
        );
    }

    #[test]
    fn duplicate_refresh_prunes_only_vanished_duplicate_selections() {
        let report = catalogue_filename_duplicates(&duplicate_catalogue_for_gui());
        let sonic = report
            .groups
            .iter()
            .find(|group| group.normalized_title == "sonic_the_hedgehog")
            .unwrap();
        let mut selected_group = Some(DuplicateGroupIdentity::from(sonic));
        let mut selected_archive = Some(sonic.entries[0].path.clone());
        let remaining = CatalogueDuplicateReport {
            groups: report
                .groups
                .into_iter()
                .filter(|group| group.normalized_title != "sonic_the_hedgehog")
                .collect(),
            archives_in_groups: 2,
        };

        prune_duplicate_review_selection(
            &mut selected_group,
            &mut selected_archive,
            Some(&remaining),
        );

        assert!(selected_group.is_none());
        assert!(selected_archive.is_none());
    }

    #[test]
    fn duplicate_display_wording_is_review_only_and_metadata_is_explicit() {
        let report = catalogue_filename_duplicates(&duplicate_catalogue_for_gui());
        let group = report
            .groups
            .iter()
            .find(|group| group.platform == "Mega Drive")
            .unwrap();
        assert_eq!(group.reason, "Matching normalized filename and platform");
        assert!(
            group
                .entries
                .iter()
                .map(|entry| format_duplicate_size(entry.size_bytes))
                .any(|size| size == "1.0 KiB (1024 bytes)")
        );
        assert_eq!(format_duplicate_size(None), "Unknown");
        assert_eq!(format_modified_time(None), "Unknown");
        assert_eq!(format_modified_time(Some(0)), "1970-01-01T00:00:00Z");
        assert!(group.entries.iter().any(|entry| entry.present));
        assert!(group.entries.iter().any(|entry| !entry.present));
    }

    #[test]
    fn real_duplicate_review_renders_paths_states_details_and_no_deletion_controls() {
        fn collect_text(shape: &egui::Shape, output: &mut String) {
            match shape {
                egui::Shape::Text(text) => {
                    output.push_str(&text.galley.job.text);
                    output.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, output);
                    }
                }
                _ => {}
            }
        }

        let report = catalogue_filename_duplicates(&duplicate_catalogue_for_gui());
        let group = report
            .groups
            .iter()
            .find(|group| group.platform == "Mega Drive")
            .unwrap();
        let mut filters = DuplicateReviewFilters::initial();
        filters.platform = Some("Mega Drive".to_string());
        let mut sort_field = DuplicateSortField::Title;
        let mut ascending = true;
        let mut selected_group = Some(DuplicateGroupIdentity::from(group));
        let mut selected_archive = Some(group.entries[0].path.clone());
        let context = egui::Context::default();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1400.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let _ = show_duplicate_review_panel(
                        ui,
                        &report,
                        &mut filters,
                        &mut sort_field,
                        &mut ascending,
                        &mut selected_group,
                        &mut selected_archive,
                    );
                });
            },
        );
        let mut painted_text = String::new();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut painted_text);
        }

        for expected in [
            "Duplicate Review",
            "Likely duplicate group",
            "Filename and platform",
            "Matching normalized filename and platform",
            "/backup/Sonic the Hedgehog.7z",
            "/roms/a/Sonic the Hedgehog.zip",
            "Present",
            "Missing",
            "Mega Drive",
            "Selected duplicate archive",
            "Exact archive path",
        ] {
            assert!(
                painted_text.contains(expected),
                "expected rendered duplicate-review text {expected:?}, got:\n{painted_text}"
            );
        }
        assert!(!painted_text.contains("Remove Missing Entries"));
        assert!(!painted_text.contains("Delete"));
    }

    fn provenance_line_map(details: &PlatformProvenanceDetails) -> HashMap<&'static str, String> {
        platform_provenance_lines(details).into_iter().collect()
    }

    #[test]
    fn missing_removal_selection_requires_missing_only_and_nonempty_selection() {
        let missing_path = PathBuf::from("/roms/missing.zip");
        let present_path = PathBuf::from("/roms/present.zip");
        let mut missing = persisted_archive(missing_path.clone(), true);
        missing.id = 1;
        let mut present = persisted_archive(present_path.clone(), false);
        present.id = 2;
        let snapshot = cached_snapshot(vec![missing, present]);

        assert!(selected_missing_paths(Some(&snapshot), &HashSet::new()).is_err());
        assert_eq!(
            selected_missing_paths(
                Some(&snapshot),
                &[missing_path.clone()].into_iter().collect()
            )
            .unwrap(),
            vec![missing_path.clone()]
        );
        let mixed = selected_missing_paths(
            Some(&snapshot),
            &[missing_path, present_path].into_iter().collect(),
        )
        .unwrap_err();
        assert!(mixed.contains("currently present"));
        assert!(mixed.contains("nothing was removed"));
    }

    #[test]
    fn missing_review_mode_reuses_filters_without_resetting_platform_filters() {
        let mut filters = LibraryRowFilters {
            present: true,
            awaiting_validation: true,
            known_platform: true,
            ..LibraryRowFilters::default()
        };

        set_missing_review_mode(&mut filters, true);

        assert!(filters.missing);
        assert!(!filters.present);
        assert!(!filters.awaiting_validation);
        assert!(filters.known_platform);
        set_missing_review_mode(&mut filters, false);
        assert!(!filters.missing);
        assert!(filters.known_platform);
    }

    #[test]
    fn missing_removal_confirmation_is_explicit_about_catalogue_only_safety() {
        let wording = missing_removal_confirmation_text(3);

        assert!(wording.contains("Remove 3 missing entries from the ArchiveFS catalogue?"));
        assert!(wording.contains("only ArchiveFS database records"));
        assert!(wording.contains("will not delete archive files or mounted contents"));
        assert!(wording.contains("return if the archives are found in a later scan"));
        assert_eq!(REMOVE_MISSING_CANCEL_LABEL, "Cancel");
        assert_eq!(REMOVE_MISSING_CONFIRM_LABEL, "Remove Missing Entries");
    }

    #[test]
    fn apply_missing_removal_uses_exact_paths_and_rejects_a_mixed_selection() {
        let root = database_test_dir("remove-missing-exact-paths");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let gone = write_archive_file(&source, "gone.zip", b"gone");
        let present = write_archive_file(&source, "present.zip", b"present");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(&database_path).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        std::fs::remove_file(&gone).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();
        database.close().unwrap();

        let error =
            apply_missing_removal_at(&database_path, &[gone.clone(), present.clone()]).unwrap_err();

        assert!(error.to_string().contains("currently present"));
        let database = Database::open_or_create(&database_path).unwrap();
        assert_eq!(database.load_archives().unwrap().len(), 2);
        database.close().unwrap();
        assert_eq!(
            apply_missing_removal_at(&database_path, &[gone])
                .unwrap()
                .removed,
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_removal_availability_requires_a_healthy_idle_database() {
        let mut app = app_for_operation_tests();
        assert!(!app.missing_removal_action_available());
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(cached_snapshot(Vec::new())),
            last_scan_summary: None,
        };
        assert!(app.missing_removal_action_available());
        let (_sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Remove {
                alias: "busy".to_string(),
            },
            receiver,
        });
        assert!(!app.missing_removal_action_available());
    }

    #[test]
    fn successful_missing_removal_records_one_activity_and_refreshes_without_resetting_view() {
        let path = PathBuf::from("/roms/missing.zip");
        let mut app = app_for_operation_tests();
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(cached_snapshot(vec![persisted_archive(path.clone(), true)])),
            last_scan_summary: None,
        };
        app.selected_archives.insert(path.clone());
        app.selected_archive = Some(path);
        app.library_filters.missing = true;
        app.sort_field = Some(SortField::ArchivePath);
        app.sort_ascending = false;
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(MissingArchiveRemovalResult {
                requested: 1,
                removed: 1,
                archive_ids: vec![1],
            }))
            .unwrap();
        app.missing_removal = Some(RunningMissingRemoval {
            requested_paths: 1,
            receiver,
        });

        app.poll_missing_removal(&egui::Context::default());

        assert!(matches!(app.database_state, DatabaseState::Loading { .. }));
        let entries: Vec<_> = app.history.entries().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, ActivityAction::CatalogueCleanup);
        assert!(entries[0].message.contains("No archive files were deleted"));
        assert!(app.library_filters.missing);
        assert_eq!(app.sort_field, Some(SortField::ArchivePath));
        assert!(!app.sort_ascending);
    }

    #[test]
    fn failed_missing_removal_preserves_selection_cached_rows_filters_and_sort() {
        let path = PathBuf::from("/roms/missing.zip");
        let mut app = app_for_operation_tests();
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(cached_snapshot(vec![persisted_archive(path.clone(), true)])),
            last_scan_summary: None,
        };
        app.selected_archives.insert(path.clone());
        app.selected_archive = Some(path.clone());
        app.library_filters.missing = true;
        app.sort_field = Some(SortField::State);
        let (sender, receiver) = mpsc::channel();
        sender.send(Err("simulated failure".to_string())).unwrap();
        app.missing_removal = Some(RunningMissingRemoval {
            requested_paths: 1,
            receiver,
        });

        app.poll_missing_removal(&egui::Context::default());

        assert!(matches!(app.database_state, DatabaseState::Ready { .. }));
        assert_eq!(app.selected_archives, [path.clone()].into_iter().collect());
        assert_eq!(app.selected_archive, Some(path));
        assert!(app.library_filters.missing);
        assert_eq!(app.sort_field, Some(SortField::State));
        assert_eq!(app.database_state.snapshot().unwrap().archives.len(), 1);
    }

    #[test]
    fn vanished_missing_selections_are_pruned_after_cache_refresh() {
        let path = PathBuf::from("/roms/removed.zip");
        let mut app = app_for_operation_tests();
        app.selected_archives.insert(path.clone());
        app.selected_archive = Some(path);

        app.prune_selection(&[]);

        assert!(app.selected_archives.is_empty());
        assert!(app.selected_archive.is_none());
    }

    #[test]
    fn manual_platform_provenance_uses_human_wording_and_unknown_fallback() {
        let details = PlatformProvenanceDetails {
            platform: Some("GameCube".to_string()),
            source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
            matched_component: None,
            automatic_fallback: None,
        };

        let lines = provenance_line_map(&details);
        assert_eq!(lines["Platform"], "GameCube");
        assert_eq!(lines["Source"], "Manual assignment");
        assert_eq!(lines["Automatic fallback"], "Unknown");
    }

    #[test]
    fn manual_platform_provenance_shows_the_correct_detailed_automatic_fallback() {
        let details = PlatformProvenanceDetails {
            platform: Some("GameCube".to_string()),
            source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
            matched_component: None,
            automatic_fallback: Some(archivefs_core::AutomaticPlatformDetails {
                platform: "Amiga CD32".to_string(),
                source: CUSTOM_FOLDER_ALIAS_SOURCE.to_string(),
                matched_component: Some("am".to_string()),
            }),
        };

        let lines = provenance_line_map(&details);
        assert_eq!(lines["Source"], "Manual assignment");
        assert_eq!(lines["Automatic fallback"], "Amiga CD32");
        assert_eq!(lines["Fallback source"], "Custom folder alias");
        assert_eq!(lines["Fallback matched alias"], "am");
    }

    #[test]
    fn custom_and_built_in_alias_provenance_show_their_matches() {
        let custom = PlatformProvenanceDetails {
            platform: Some("Amiga CD32".to_string()),
            source: Some(CUSTOM_FOLDER_ALIAS_SOURCE.to_string()),
            matched_component: Some("am".to_string()),
            automatic_fallback: None,
        };
        let built_in = PlatformProvenanceDetails {
            platform: Some("Intellivision".to_string()),
            source: Some("folder_alias".to_string()),
            matched_component: Some("intellivision".to_string()),
            automatic_fallback: None,
        };

        let custom_lines = provenance_line_map(&custom);
        assert_eq!(custom_lines["Source"], "Custom folder alias");
        assert_eq!(custom_lines["Matched alias"], "am");
        let built_in_lines = provenance_line_map(&built_in);
        assert_eq!(built_in_lines["Source"], "Built-in folder alias");
        assert_eq!(built_in_lines["Matched folder"], "intellivision");
    }

    #[test]
    fn heuristic_and_unknown_provenance_are_clear_and_never_show_raw_sources() {
        let heuristic = PlatformProvenanceDetails {
            platform: Some("MSX".to_string()),
            source: Some("heuristic-path-detector".to_string()),
            matched_component: None,
            automatic_fallback: None,
        };
        let unknown = PlatformProvenanceDetails {
            platform: None,
            source: None,
            matched_component: None,
            automatic_fallback: None,
        };

        let heuristic_lines = provenance_line_map(&heuristic);
        assert_eq!(heuristic_lines["Source"], "Filename/path heuristic");
        assert!(
            !platform_provenance_lines(&heuristic)
                .iter()
                .any(|(_, value)| value.contains("heuristic-path-detector"))
        );
        let unknown_lines = provenance_line_map(&unknown);
        assert_eq!(unknown_lines["Platform"], "Unknown");
        assert_eq!(unknown_lines["Source"], "Unknown");
    }

    #[test]
    fn scan_completion_and_activity_use_every_truthful_non_overlapping_count() {
        let summary = ScanPersistSummary {
            scan_run_id: 42,
            counts: archivefs_core::ScanRunCounts {
                archives_seen: 1_236,
                archives_added: 3,
                archives_changed: 2,
                archives_restored: 1,
                archives_unchanged: 1_230,
                archives_updated: 3,
                archives_missing: 4,
                errors_count: 1,
                source_folders_scanned: 2,
            },
            folder_errors: vec![(PathBuf::from("/offline"), "unavailable".to_string())],
        };

        assert_eq!(
            format_scan_completion(&summary),
            "Scan completed\nSeen: 1236\nAdded: 3\nUpdated: 2\nRestored: 1\nNewly missing: 4\nUnchanged: 1230\nErrors: 1"
        );
        let activity = format_scan_activity(&summary);
        assert_eq!(
            activity,
            "Scan completed: seen 1236, added 3, updated 2, restored 1, newly missing 4, unchanged 1230, errors 1."
        );
        assert_eq!(
            activity.lines().count(),
            1,
            "activity must stay one concise entry"
        );
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

    #[test]
    fn successful_batch_unmount_clears_only_its_previous_lazy_offer() {
        let mut app = app_for_operation_tests();
        let item = unmount_all_item("Game");
        let other = PathBuf::from("/roms/Other.zip");
        app.lazy_unmount_offers = HashSet::from([item.archive_path.clone(), other.clone()]);
        let (sender, receiver) = mpsc::channel();
        app.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: UnmountAllProgress::default(),
        });
        sender
            .send(UnmountAllEvent::ArchiveCompleted(item.clone()))
            .unwrap();

        app.poll_unmount_all(&egui::Context::default());

        assert!(!app.lazy_unmount_offers.contains(&item.archive_path));
        assert!(app.lazy_unmount_offers.contains(&other));
        let mounted_again = record("/roms/Game.zip", MountState::Mounted);
        assert!(!lazy_unmount_available(
            &mounted_again,
            &app.lazy_unmount_offers,
            false,
        ));
    }

    #[test]
    fn no_longer_mounted_batch_skip_clears_only_its_previous_lazy_offer() {
        let mut app = app_for_operation_tests();
        let item = unmount_all_item("Game");
        let other = PathBuf::from("/roms/Other.zip");
        app.lazy_unmount_offers = HashSet::from([item.archive_path.clone(), other.clone()]);
        let (sender, receiver) = mpsc::channel();
        app.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: UnmountAllProgress::default(),
        });
        sender
            .send(UnmountAllEvent::ArchiveSkipped {
                item: item.clone(),
                reason: "archive is no longer mounted".to_string(),
            })
            .unwrap();

        app.poll_unmount_all(&egui::Context::default());

        assert!(!app.lazy_unmount_offers.contains(&item.archive_path));
        assert!(app.lazy_unmount_offers.contains(&other));
    }

    #[test]
    fn failed_normal_batch_unmount_retains_its_exact_lazy_offer() {
        let mut app = app_for_operation_tests();
        let item = unmount_all_item("Busy");
        let (sender, receiver) = mpsc::channel();
        app.unmount_all = Some(RunningUnmountAll {
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            progress: UnmountAllProgress::default(),
        });
        sender
            .send(UnmountAllEvent::ArchiveFailed {
                item: item.clone(),
                message: "mount is busy".to_string(),
                offer_lazy_unmount: true,
            })
            .unwrap();

        app.poll_unmount_all(&egui::Context::default());

        assert!(app.lazy_unmount_offers.contains(&item.archive_path));
    }

    #[test]
    fn missing_config_load_opens_setup_instead_of_leaving_a_fatal_view() {
        let mut app = app_for_operation_tests();
        let (_diagnostics_sender, diagnostics_receiver) = mpsc::channel();
        app.diagnostics = DiagnosticsState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver: diagnostics_receiver,
        };
        let (sender, receiver) = mpsc::channel();
        app.state = LoadState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver,
            previous: None,
        };
        sender
            .send((
                RefreshGeneration::INITIAL,
                Err("missing /config/archivefs.toml".to_string()),
            ))
            .unwrap();

        app.poll_load(&egui::Context::default());

        assert!(app.show_diagnostics);
        assert!(matches!(app.state, LoadState::Error(_)));
        assert!(matches!(app.diagnostics, DiagnosticsState::Loading { .. }));
        assert!(!diagnostics_state_can_continue(&app.diagnostics));
        assert!(app.refresh_error.is_some());
    }

    #[test]
    fn failed_refresh_retains_snapshot_and_invalidates_stale_diagnostics() {
        let mut app = app_for_operation_tests();
        let (_diagnostics_sender, diagnostics_receiver) = mpsc::channel();
        app.diagnostics = DiagnosticsState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver: diagnostics_receiver,
        };
        let (sender, receiver) = mpsc::channel();
        app.state = LoadState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver,
            previous: Some(Box::new(empty_loaded_data("/old-mount"))),
        };
        sender
            .send((
                RefreshGeneration::INITIAL,
                Err("config became invalid".to_string()),
            ))
            .unwrap();

        app.poll_load(&egui::Context::default());

        assert!(matches!(
            &app.state,
            LoadState::Ready(data) if data.mount_root == Path::new("/old-mount")
        ));
        assert!(app.snapshot_stale);
        assert!(matches!(app.diagnostics, DiagnosticsState::Loading { .. }));
        assert!(!diagnostics_state_can_continue(&app.diagnostics));
        assert_eq!(app.refresh_error.as_deref(), Some("config became invalid"));
    }

    #[test]
    fn retry_success_replaces_the_old_snapshot_and_clears_error() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.state = LoadState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver,
            previous: Some(Box::new(empty_loaded_data("/old-mount"))),
        };
        app.refresh_error = Some("old failure".to_string());
        app.snapshot_stale = true;
        sender
            .send((
                RefreshGeneration::INITIAL,
                Ok(empty_loaded_data("/new-mount")),
            ))
            .unwrap();

        app.poll_load(&egui::Context::default());

        assert!(matches!(
            &app.state,
            LoadState::Ready(data) if data.mount_root == Path::new("/new-mount")
        ));
        assert!(!app.snapshot_stale);
        assert!(app.refresh_error.is_none());
    }

    #[test]
    fn fresh_invalid_diagnostics_keep_setup_open() {
        let mut app = app_for_operation_tests();
        app.show_diagnostics = true;
        let (sender, receiver) = mpsc::channel();
        app.diagnostics = DiagnosticsState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver,
        };
        sender
            .send((RefreshGeneration::INITIAL, setup_report(false, false)))
            .unwrap();

        app.poll_diagnostics();

        assert!(app.show_diagnostics);
        assert!(!diagnostics_state_can_continue(&app.diagnostics));
    }

    #[test]
    fn successful_refresh_invalidates_stale_action_readiness() {
        let mut app = app_for_operation_tests();
        assert!(latest_generation_actions_safe(
            app.refresh_generation,
            app.snapshot_generation,
            app.snapshot_stale,
            snapshot_identity(&app.state),
            &app.diagnostics,
        ));

        app.refresh(&egui::Context::default());
        let current = app.refresh_generation;
        app.state = LoadState::Ready(Box::new(empty_loaded_data("/new-mount")));
        app.snapshot_generation = Some(current);

        assert!(matches!(
            app.diagnostics,
            DiagnosticsState::Loading {
                generation,
                ..
            } if generation == current
        ));
        assert!(!latest_generation_actions_safe(
            current,
            app.snapshot_generation,
            app.snapshot_stale,
            snapshot_identity(&app.state),
            &app.diagnostics,
        ));
    }

    #[test]
    fn newer_unsafe_config_cannot_inherit_old_action_readiness() {
        let current = RefreshGeneration(2);
        let old_ready = DiagnosticsState::Ready {
            generation: RefreshGeneration(1),
            report: setup_report(true, true),
        };
        assert!(!latest_generation_actions_safe(
            current,
            Some(current),
            false,
            Some(&default_config_identity()),
            &old_ready,
        ));

        let current_unsafe = DiagnosticsState::Ready {
            generation: current,
            report: setup_report(true, false),
        };
        assert!(!latest_generation_actions_safe(
            current,
            Some(current),
            false,
            Some(&default_config_identity()),
            &current_unsafe,
        ));
    }

    #[test]
    fn late_diagnostics_from_an_older_generation_are_ignored() {
        let mut app = app_for_operation_tests();
        let current = RefreshGeneration(2);
        app.refresh_generation = current;
        let (sender, receiver) = mpsc::channel();
        app.diagnostics = DiagnosticsState::Loading {
            generation: current,
            receiver,
        };
        sender
            .send((RefreshGeneration(1), setup_report(true, true)))
            .unwrap();

        app.poll_diagnostics();

        assert!(matches!(
            app.diagnostics,
            DiagnosticsState::Loading {
                generation,
                ..
            } if generation == current
        ));
        assert!(!latest_generation_actions_safe(
            current,
            Some(current),
            false,
            snapshot_identity(&app.state),
            &app.diagnostics,
        ));
    }

    #[test]
    fn actions_require_current_valid_snapshot_and_diagnostics() {
        let current = RefreshGeneration(4);
        let ready = DiagnosticsState::Ready {
            generation: current,
            report: setup_report(true, true),
        };
        assert!(latest_generation_actions_safe(
            current,
            Some(current),
            false,
            Some(&default_config_identity()),
            &ready,
        ));
        assert!(!latest_generation_actions_safe(
            current,
            Some(RefreshGeneration(3)),
            false,
            Some(&default_config_identity()),
            &ready,
        ));
    }

    #[test]
    fn disconnected_diagnostics_stop_loading_and_allow_retry() {
        let mut app = app_for_operation_tests();
        let snapshot_root = PathBuf::from("/last-good");
        app.state = LoadState::Ready(Box::new(empty_loaded_data("/last-good")));
        let (sender, receiver) = mpsc::channel::<DiagnosticsMessage>();
        drop(sender);
        app.diagnostics = DiagnosticsState::Loading {
            generation: app.refresh_generation,
            receiver,
        };

        app.poll_diagnostics();

        assert!(matches!(
            &app.diagnostics,
            DiagnosticsState::Error { message, .. }
                if message.contains("Run diagnostics again")
        ));
        assert!(matches!(
            &app.state,
            LoadState::Ready(data) if data.mount_root == snapshot_root
        ));
        assert!(!diagnostics_state_can_continue(&app.diagnostics));
        assert!(app.show_diagnostics);
        assert!(!latest_generation_actions_safe(
            app.refresh_generation,
            app.snapshot_generation,
            app.snapshot_stale,
            snapshot_identity(&app.state),
            &app.diagnostics,
        ));
    }

    #[test]
    fn ready_diagnostics_allow_continue_and_can_be_reopened() {
        let report = setup_report(true, false);
        assert!(diagnostics_can_continue(&report));

        let mut visible = false;
        open_diagnostics_view(&mut visible);
        assert!(visible);
    }

    #[test]
    fn starter_config_requires_a_resolved_confirmed_missing_path() {
        let mut report = setup_report(false, false);
        report.config_missing = false;
        report.config_path_error = Some("HOME is unavailable".to_string());
        assert!(!starter_config_available(&report));

        report.config_path_error = None;
        report.config_missing = true;
        assert!(starter_config_available(&report));
    }

    #[test]
    fn unresolved_config_path_disables_starter_config_and_path_actions() {
        let mut report = setup_report(false, false);
        report.config_path = None;
        report.config_path_error = Some("HOME and USERPROFILE are unavailable".to_string());
        report.config_missing = true;

        assert!(report.config_path.is_none());
        assert!(!starter_config_available(&report));
    }

    #[test]
    fn resolved_config_path_allows_path_actions() {
        let report = setup_report(true, true);
        assert!(report.config_path.is_some());
    }

    #[test]
    fn mismatched_config_identity_blocks_actions_despite_matching_generation() {
        let current = RefreshGeneration(7);
        let ready = DiagnosticsState::Ready {
            generation: current,
            report: setup_report(true, true),
        };
        let different_identity = ConfigIdentity {
            config_path: Some(PathBuf::from("/config/archivefs.toml")),
            content_digest: Some([2; 32]),
        };

        assert!(!latest_generation_actions_safe(
            current,
            Some(current),
            false,
            Some(&different_identity),
            &ready,
        ));
        assert!(latest_generation_actions_safe(
            current,
            Some(current),
            false,
            Some(&default_config_identity()),
            &ready,
        ));
    }

    #[test]
    fn config_changed_between_worker_starts_cannot_produce_trusted_combined_state() {
        let mut app = app_for_operation_tests();
        let current = app.refresh_generation;
        app.state = LoadState::Ready(Box::new(empty_loaded_data("/mount")));
        let changed_identity = ConfigIdentity {
            config_path: Some(PathBuf::from("/config/archivefs.toml")),
            content_digest: Some([9; 32]),
        };
        app.diagnostics = DiagnosticsState::Ready {
            generation: current,
            report: SetupDiagnostics {
                config_identity: changed_identity,
                ..setup_report(true, true)
            },
        };

        assert!(!latest_generation_actions_safe(
            app.refresh_generation,
            app.snapshot_generation,
            app.snapshot_stale,
            snapshot_identity(&app.state),
            &app.diagnostics,
        ));
    }

    #[test]
    fn setup_failure_preserves_the_last_valid_snapshot() {
        let mut app = app_for_operation_tests();
        app.state = LoadState::Ready(Box::new(empty_loaded_data("/mount")));
        let (sender, receiver) = mpsc::channel();
        app.setup_action = Some(RunningSetupAction {
            action: SetupAction::OpenConfigFolder,
            receiver,
        });
        sender
            .send(Err("could not open folder".to_string()))
            .unwrap();

        app.poll_setup_action(&egui::Context::default());

        assert!(matches!(app.state, LoadState::Ready(_)));
        assert!(!app.feedback.as_ref().unwrap().succeeded);
    }

    // -----------------------------------------------------------------
    // Stage 4: persistent library database GUI integration - tests.
    // -----------------------------------------------------------------

    #[test]
    fn startup_with_no_database_is_reported_as_not_created_not_as_an_error() {
        let dir = database_test_dir("no-database");
        let database_path = dir.join("library.sqlite3");

        let result = load_database_snapshot_at(&database_path, None);

        assert!(matches!(
            result,
            Err(DatabaseLoadError::NotCreated { database_path: reported }) if reported == database_path
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_rows_appear_before_live_refresh_completes() {
        let snapshot = cached_snapshot(vec![
            persisted_archive(PathBuf::from("/roms/present.zip"), false),
            persisted_archive(PathBuf::from("/roms/missing.zip"), true),
        ]);

        let merged = build_display_rows(&[], &[], Some(&snapshot));

        assert_eq!(merged.len(), 2);
        assert!(
            merged
                .iter()
                .any(|row| row.origin == RowOrigin::CachedAwaitingValidation
                    || row.origin == RowOrigin::CachedUnavailable)
        );
        assert!(
            merged
                .iter()
                .any(|row| row.origin == RowOrigin::CachedMissing)
        );
    }

    #[test]
    fn cache_only_rows_cannot_resolve_to_a_live_record() {
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/cache-only.zip"),
            false,
        )]);
        let merged = build_display_rows(&[], &[], Some(&snapshot));
        let cache_row = &merged[0];

        // No live records at all, so selecting the cache-only row's exact
        // path can never resolve to an ArchiveRecord - this is the same
        // fallback show_selected_archive already relies on to render zero
        // action buttons for `None`.
        assert_eq!(selected_record(&[], Some(&cache_row.path)), None);
    }

    #[test]
    fn live_validation_enables_actions_for_a_confirmed_row() {
        let record = record_at(PathBuf::from("/roms/confirmed.zip"), MountState::Pending);
        let live_row = row_for(&record);
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/confirmed.zip"),
            false,
        )]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        // The live row wins - the cache row for the same exact path is not
        // duplicated - and selecting it resolves to the live record, which
        // is what makes action buttons available.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::Live);
        assert_eq!(
            selected_record(std::slice::from_ref(&record), Some(&merged[0].path)),
            Some(&record)
        );
    }

    #[test]
    fn missing_cached_rows_remain_visible_in_the_merge() {
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/gone.zip"),
            true,
        )]);

        let merged = build_display_rows(&[], &[], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::CachedMissing);
    }

    #[test]
    fn newly_discovered_live_archive_not_yet_in_cache_appears_as_a_live_row() {
        let record = record_at(PathBuf::from("/roms/brand-new.zip"), MountState::Pending);
        let live_row = row_for(&record);
        let snapshot = cached_snapshot(vec![]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::Live);
    }

    // -----------------------------------------------------------------
    // Unknown-platform review workflow.
    // -----------------------------------------------------------------

    #[test]
    fn live_row_with_a_persisted_manual_platform_is_not_classified_unknown() {
        // The crux of requirement 6: automatic detection found nothing
        // for this live record (no metadata/identity platform), but the
        // database already has a manual assignment for the same exact
        // path - the merged row must reflect the effective (manual)
        // platform, not the live-only "nothing detected" signal.
        let path = PathBuf::from("/roms/mystery.zip");
        let record = record_at(path.clone(), MountState::Pending);
        let live_row = row_for(&record);
        assert!(
            live_row.unknown_platform,
            "sanity check: live-only detection found nothing"
        );
        let snapshot = cached_snapshot(vec![persisted_archive_with_platform(
            path,
            1,
            "GameCube",
            MANUAL_PLATFORM_SOURCE,
        )]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::Live);
        assert!(!merged[0].unknown_platform);
        assert_eq!(merged[0].platform, "GameCube");
    }

    #[test]
    fn live_row_without_a_persisted_entry_keeps_its_live_only_classification() {
        // No database row exists for this archive yet (never scanned
        // into the library database) - there is no persisted effective
        // value to defer to, so the live-only signal is the only one
        // available and must be used as-is.
        let record = record_at(PathBuf::from("/roms/brand-new.zip"), MountState::Pending);
        let live_row = row_for(&record);
        let snapshot = cached_snapshot(vec![]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert!(merged[0].unknown_platform);
    }

    #[test]
    fn cache_only_missing_row_with_no_platform_is_classified_unknown() {
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/gone.zip"),
            true,
        )]);

        let merged = build_display_rows(&[], &[], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::CachedMissing);
        assert!(merged[0].unknown_platform);
    }

    #[test]
    fn cache_only_missing_row_with_a_manual_platform_is_not_unknown() {
        let snapshot = cached_snapshot(vec![PersistedArchive {
            platform: Some("GameCube".to_string()),
            platform_source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
            ..persisted_archive(PathBuf::from("/roms/gone.zip"), true)
        }]);

        let merged = build_display_rows(&[], &[], Some(&snapshot));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::CachedMissing);
        assert!(!merged[0].unknown_platform);
    }

    #[test]
    fn unknown_count_covers_every_merged_row_live_and_cache_only_alike() {
        let known_live_record =
            record_at(PathBuf::from("/roms/known-live.zip"), MountState::Pending);
        let known_live_row = row_for(&known_live_record);
        let unknown_live_record =
            record_at(PathBuf::from("/roms/unknown-live.zip"), MountState::Pending);
        let unknown_live_row = row_for(&unknown_live_record);
        let records = vec![known_live_record, unknown_live_record];
        let live_rows = vec![known_live_row, unknown_live_row];
        let snapshot = cached_snapshot(vec![
            persisted_archive_with_platform(
                PathBuf::from("/roms/known-live.zip"),
                1,
                "GameCube",
                MANUAL_PLATFORM_SOURCE,
            ),
            persisted_archive(PathBuf::from("/roms/unknown-cached.zip"), false),
            persisted_archive_with_platform(
                PathBuf::from("/roms/known-cached.zip"),
                2,
                "SNES",
                "folder_alias",
            ),
        ]);

        let merged = build_display_rows(&records, &live_rows, Some(&snapshot));

        assert_eq!(
            merged.len(),
            4,
            "sanity check: two live + two cache-only rows"
        );
        let unknown_count = merged.iter().filter(|row| row.unknown_platform).count();
        assert_eq!(
            unknown_count, 2,
            "unknown-live.zip and unknown-cached.zip - the manual and known-automatic rows must not count"
        );
    }

    #[test]
    fn corrupt_database_is_non_fatal() {
        let dir = database_test_dir("corrupt");
        let database_path = dir.join("library.sqlite3");
        std::fs::write(&database_path, b"not a sqlite database").unwrap();

        let result = load_database_snapshot_at(&database_path, None);

        assert!(matches!(result, Err(DatabaseLoadError::Failed { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_unhealthy_database_reports_future_schema_as_a_clear_upgrade_message() {
        let health = DatabaseHealth {
            resolved_path: PathBuf::from("/config/library.sqlite3"),
            database_exists: true,
            database_opens: true,
            schema_version: Some(latest_schema_version() + 1),
            migrations_current: false,
            foreign_keys_enabled: true,
            error: None,
        };

        let error = classify_unhealthy_database(health);

        match error {
            DatabaseLoadError::Failed { message } => {
                assert!(message.contains("newer than this build"));
            }
            DatabaseLoadError::NotCreated { .. } => panic!("expected Failed, got NotCreated"),
            DatabaseLoadError::Outdated { .. } => panic!("expected Failed, got Outdated"),
        }
    }

    #[test]
    fn classify_unhealthy_database_reports_a_merely_old_schema_as_outdated_not_an_error() {
        let health = DatabaseHealth {
            resolved_path: PathBuf::from("/config/library.sqlite3"),
            database_exists: true,
            database_opens: true,
            schema_version: Some(0),
            migrations_current: false,
            foreign_keys_enabled: true,
            error: None,
        };

        let error = classify_unhealthy_database(health);

        assert!(matches!(error, DatabaseLoadError::Outdated { .. }));
    }

    #[test]
    fn classify_unhealthy_database_reports_unopenable_database_as_failed_with_its_error() {
        let health = DatabaseHealth {
            resolved_path: PathBuf::from("/config/library.sqlite3"),
            database_exists: true,
            database_opens: false,
            schema_version: None,
            migrations_current: false,
            foreign_keys_enabled: false,
            error: Some("disk I/O error".to_string()),
        };

        let error = classify_unhealthy_database(health);

        match error {
            DatabaseLoadError::Failed { message } => assert_eq!(message, "disk I/O error"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn scan_partial_success_reports_folder_errors_without_failing_the_scan() {
        let dir = database_test_dir("scan-partial");
        let source_a = dir.join("source-a");
        let source_b = dir.join("source-b");
        let mount = dir.join("mount");
        write_archive_file(&source_a, "a.zip", b"a");
        write_archive_file(&source_b, "b.zip", b"b");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }
        std::fs::remove_dir_all(&source_a).unwrap();

        let result = load_database_snapshot_at(&database_path, Some(&config));

        match result {
            Ok(DatabaseOutcome::Scanned {
                snapshot,
                scan_summary,
            }) => {
                assert_eq!(scan_summary.folder_errors.len(), 1);
                assert_eq!(scan_summary.folder_errors[0].0, source_a);
                // Archives under the still-reachable folder remain in the
                // catalogue - a partial failure does not crash or discard
                // the rest of the scan.
                assert!(
                    snapshot
                        .archives
                        .iter()
                        .any(|archive| archive.relative_path == Path::new("b.zip"))
                );
            }
            _ => panic!("expected a partially-successful Scanned outcome"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn successful_scan_refreshes_cached_counts() {
        let dir = database_test_dir("scan-success");
        let source = dir.join("source");
        let mount = dir.join("mount");
        write_archive_file(&source, "game.zip", b"game data");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");

        let result = load_database_snapshot_at(&database_path, Some(&config));

        match result {
            Ok(DatabaseOutcome::Scanned {
                snapshot,
                scan_summary,
            }) => {
                assert_eq!(scan_summary.counts.archives_added, 1);
                assert_eq!(snapshot.stats.total_archives, 1);
                assert_eq!(snapshot.archives.len(), 1);
            }
            _ => panic!("expected a successful Scanned outcome"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn database_worker_disconnect_is_surfaced_as_an_error() {
        let mut app = app_for_operation_tests();
        let generation = DatabaseGeneration::INITIAL.next();
        app.database_generation = generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        drop(sender);
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: None,
            scanning: false,
        };

        app.poll_database_load(&egui::Context::default());

        match &app.database_state {
            DatabaseState::Error { message, .. } => {
                assert!(message.contains("stopped unexpectedly"));
            }
            _ => panic!("expected a disconnected worker to surface as DatabaseState::Error"),
        }
    }

    #[test]
    fn late_database_results_from_an_older_generation_are_ignored() {
        let mut app = app_for_operation_tests();
        let stale_generation = DatabaseGeneration::INITIAL;
        let current_generation = stale_generation.next();
        app.database_generation = current_generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation: stale_generation,
            receiver,
            previous: None,
            scanning: false,
        };
        let stale_snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/stale.zip"),
            false,
        )]);
        sender
            .send((
                stale_generation,
                Ok(DatabaseOutcome::Loaded(stale_snapshot)),
            ))
            .unwrap();

        app.poll_database_load(&egui::Context::default());

        // The message's generation does not match the current generation,
        // so it must be dropped entirely - the state must still be the
        // same stale Loading value, never overwritten with the stale
        // snapshot's data.
        assert!(matches!(
            &app.database_state,
            DatabaseState::Loading { generation, .. } if *generation == stale_generation
        ));
    }

    #[test]
    #[cfg(unix)]
    fn reconciliation_uses_exact_path_bytes_not_lossy_display_strings() {
        // Two distinct invalid-UTF-8 byte sequences that both decode to
        // the same lossy "fo<REPLACEMENT>o.zip" under Path::display() -
        // see database.rs's own non_utf8_path_round_trips_exactly_through_a_blob_column
        // test for why 0x80/0x81 alone are never valid UTF-8 continuation
        // bytes here.
        let bytes_a: Vec<u8> = vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p'];
        let bytes_b: Vec<u8> = vec![0x66, 0x6f, 0x81, 0x6f, b'.', b'z', b'i', b'p'];
        let path_a = PathBuf::from(OsString::from_vec(bytes_a));
        let path_b = PathBuf::from(OsString::from_vec(bytes_b));
        assert_ne!(
            path_a, path_b,
            "the two test paths must differ in exact bytes"
        );
        assert_eq!(
            path_a.display().to_string(),
            path_b.display().to_string(),
            "the two test paths must collide under lossy display - that is the point"
        );

        let record = record_at(path_a, MountState::Pending);
        let live_row = row_for(&record);
        let snapshot = cached_snapshot(vec![persisted_archive(path_b, false)]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        // If reconciliation had compared lossy display strings instead of
        // exact bytes, these two different archives would have been
        // wrongly treated as the same one and collapsed into a single row.
        assert_eq!(merged.len(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_paths_reconcile_correctly_on_unix() {
        let bytes: Vec<u8> = vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p'];
        let path = PathBuf::from(OsString::from_vec(bytes));
        assert!(
            path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );

        let record = record_at(path.clone(), MountState::Pending);
        let live_row = row_for(&record);
        let snapshot = cached_snapshot(vec![persisted_archive(path, false)]);

        let merged =
            build_display_rows(std::slice::from_ref(&record), &[live_row], Some(&snapshot));

        // Identical non-UTF-8 bytes on both sides must be recognized as
        // the same archive - the cache-only entry must be suppressed, not
        // duplicated.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].origin, RowOrigin::Live);
    }

    #[test]
    fn scan_library_action_does_not_block_on_a_slow_background_worker() {
        // Deliberately does not call the real start_database_action/
        // start_database_load (those spawn a real thread that touches the
        // real default database/config paths - see load_database_snapshot -
        // which every other stage 4 test in this file avoids for exactly
        // that reason, matching how the existing live-snapshot tests never
        // call the real start_load/refresh either). Instead this drives
        // the same Loading state and channel those functions would have
        // produced by hand, with nothing sent on it yet, and proves
        // poll_database_load's use of try_recv (not recv) means polling an
        // in-progress scan never blocks the UI thread waiting for a result.
        let mut app = app_for_operation_tests();
        let generation = DatabaseGeneration::INITIAL;
        app.database_generation = generation;
        let (_sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: None,
            scanning: true,
        };

        app.poll_database_load(&egui::Context::default());

        assert!(matches!(
            app.database_state,
            DatabaseState::Loading { scanning: true, .. }
        ));
    }

    #[test]
    fn database_scan_completing_while_a_live_refresh_is_active_does_not_panic() {
        let mut app = app_for_operation_tests();
        app.state = LoadState::Loading {
            generation: RefreshGeneration::INITIAL,
            receiver: mpsc::channel().1,
            previous: None,
        };
        let generation = DatabaseGeneration::INITIAL.next();
        app.database_generation = generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: None,
            scanning: false,
        };
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/during-refresh.zip"),
            false,
        )]);
        sender
            .send((generation, Ok(DatabaseOutcome::Loaded(snapshot))))
            .unwrap();

        app.poll_database_load(&egui::Context::default());

        // No live snapshot exists yet, so the cached filtered-index
        // recompute is a no-op - the important thing is that resolving
        // the database mid-live-refresh does not panic and leaves the
        // database state correctly Ready.
        assert!(matches!(app.database_state, DatabaseState::Ready { .. }));
        assert!(matches!(app.state, LoadState::Loading { .. }));
    }

    fn row_with_origin(origin: RowOrigin, unknown_platform: bool) -> ArchiveRow {
        let mut row = row("");
        row.origin = origin;
        row.unknown_platform = unknown_platform;
        row
    }

    #[test]
    fn library_row_filters_default_hides_nothing() {
        let filters = LibraryRowFilters::default();
        assert!(!filters.is_active());
        assert!(filters.matches(&row_with_origin(RowOrigin::Live, false)));
        assert!(filters.matches(&row_with_origin(RowOrigin::CachedMissing, true)));
    }

    #[test]
    fn library_row_filters_present_only_shows_only_live_rows() {
        let filters = LibraryRowFilters {
            present: true,
            ..LibraryRowFilters::default()
        };

        assert!(filters.matches(&row_with_origin(RowOrigin::Live, false)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::CachedMissing, false)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::CachedAwaitingValidation, false)));
    }

    #[test]
    fn library_row_filters_platform_groups_are_independent_of_state_groups() {
        let filters = LibraryRowFilters {
            missing: true,
            known_platform: true,
            ..LibraryRowFilters::default()
        };

        // A missing row with an unknown platform must fail the platform
        // group even though it passes the state group - both active
        // groups must match (AND across groups, OR within a group).
        assert!(!filters.matches(&row_with_origin(RowOrigin::CachedMissing, true)));
        assert!(filters.matches(&row_with_origin(RowOrigin::CachedMissing, false)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::Live, false)));
    }

    #[test]
    fn show_unknown_only_combines_with_the_present_filter() {
        // Requirement 3's exact example: present + unknown-only means
        // present unknown rows only - missing rows stay excluded even
        // though they are also unknown.
        let filters = LibraryRowFilters {
            present: true,
            unknown_platform: true,
            ..LibraryRowFilters::default()
        };

        assert!(filters.matches(&row_with_origin(RowOrigin::Live, true)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::Live, false)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::CachedMissing, true)));
    }

    #[test]
    fn show_unknown_only_combines_with_the_missing_filter() {
        let filters = LibraryRowFilters {
            missing: true,
            unknown_platform: true,
            ..LibraryRowFilters::default()
        };

        assert!(filters.matches(&row_with_origin(RowOrigin::CachedMissing, true)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::CachedMissing, false)));
        assert!(!filters.matches(&row_with_origin(RowOrigin::Live, true)));
    }

    #[test]
    fn show_unknown_only_combines_with_text_search() {
        // Mirrors exactly how `show_loaded_data` composes the two:
        // `matching_row_indices` (text search) intersected with
        // `library_filters.matches` (checkbox filters).
        let known_record = record_at(
            PathBuf::from("/roms/mystery-known.zip"),
            MountState::Pending,
        );
        let mut known_row = row_for(&known_record);
        known_row.archive_path = "/roms/mystery-known.zip".to_string();
        known_row.unknown_platform = false;
        known_row.platform = "GameCube".to_string();
        known_row.search_text = "mystery-known.zip\n\ngamecube\npending".to_string();
        let unknown_record = record_at(
            PathBuf::from("/roms/mystery-unknown.zip"),
            MountState::Pending,
        );
        let unknown_row = row_for(&unknown_record);
        let rows = vec![known_row, unknown_row];

        let text_matches = matching_row_indices(&rows, "mystery").unwrap();
        assert_eq!(
            text_matches.len(),
            2,
            "sanity check: both rows match the text search"
        );

        let filters = LibraryRowFilters {
            unknown_platform: true,
            ..LibraryRowFilters::default()
        };
        let combined: Vec<usize> = text_matches
            .into_iter()
            .filter(|&index| filters.matches(&rows[index]))
            .collect();

        assert_eq!(combined.len(), 1);
        assert_eq!(rows[combined[0]].archive_path, "/roms/mystery-unknown.zip");
    }

    // -------------------------------------------------------------
    // Manual platform assignment.
    // -------------------------------------------------------------

    #[test]
    fn resolved_platform_choice_uses_canonical_selection_or_trimmed_custom_text() {
        assert_eq!(
            resolved_platform_choice(Some("GameCube"), ""),
            Some("GameCube")
        );
        assert_eq!(resolved_platform_choice(None, "anything"), None);
        assert_eq!(
            resolved_platform_choice(Some(CUSTOM_PLATFORM_CHOICE), "  NeoGeo64  "),
            Some("NeoGeo64")
        );
        assert_eq!(
            resolved_platform_choice(Some(CUSTOM_PLATFORM_CHOICE), "   "),
            None,
            "blank custom text must not resolve to an empty platform"
        );
    }

    #[test]
    fn selected_persisted_archive_finds_a_cache_only_missing_row() {
        let path = PathBuf::from("/roms/mystery.zip");
        let snapshot = cached_snapshot(vec![persisted_archive(path.clone(), true)]);

        assert_eq!(
            selected_persisted_archive(Some(&snapshot), Some(&path)),
            Some(&snapshot.archives[0]),
            "a cache-only/missing row must still be classifiable - it is metadata only, not a mount action"
        );
        assert_eq!(selected_persisted_archive(Some(&snapshot), None), None);
        assert_eq!(
            selected_persisted_archive(Some(&snapshot), Some(Path::new("/roms/other.zip"))),
            None
        );
        assert_eq!(selected_persisted_archive(None, Some(&path)), None);
    }

    #[test]
    fn platform_action_available_requires_no_running_action_or_database_load() {
        let mut app = app_for_operation_tests();
        assert!(app.platform_action_available());

        let (_sender, receiver) = mpsc::channel();
        app.platform_action = Some(RunningPlatformAction {
            archive_path: PathBuf::from("/roms/game.zip"),
            receiver,
        });
        assert!(!app.platform_action_available());
        app.platform_action = None;

        let (_sender, receiver) = mpsc::channel();
        app.database_state = DatabaseState::Loading {
            generation: DatabaseGeneration::INITIAL,
            receiver,
            previous: None,
            scanning: false,
        };
        assert!(!app.platform_action_available());
    }

    #[test]
    fn poll_platform_action_success_refreshes_the_database_cache_asynchronously() {
        let mut app = app_for_operation_tests();
        let archive_path = PathBuf::from("/roms/n64/Luigis_Mansion.zip");
        let (sender, receiver) = mpsc::channel();
        app.platform_action = Some(RunningPlatformAction {
            archive_path: archive_path.clone(),
            receiver,
        });
        sender
            .send(Ok(PlatformAssignmentChange {
                old_platform: Some("N64".to_string()),
                old_source: Some("folder_alias".to_string()),
                new_platform: Some("GameCube".to_string()),
                new_source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
            }))
            .unwrap();

        app.poll_platform_action(&egui::Context::default());

        assert!(app.platform_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(feedback.succeeded);
        assert!(feedback.message.contains("N64 (folder_alias)"));
        assert!(feedback.message.contains("GameCube (manual)"));
        assert!(
            app.history
                .entries()
                .any(
                    |entry| entry.archive_path.as_deref() == Some(archive_path.as_path())
                        && entry.outcome == ActivityOutcome::Completed
                ),
        );
        // Refreshing the cache is asynchronous - poll_platform_action only
        // starts a new background database load, it does not block
        // waiting for it, and the live snapshot is untouched.
        assert!(app.database_state.is_loading());
        assert!(matches!(app.state, LoadState::Ready(_)));
    }

    #[test]
    fn poll_platform_action_failure_preserves_the_cached_row_and_shows_the_error() {
        let mut app = app_for_operation_tests();
        let stale_snapshot = cached_snapshot(vec![persisted_archive_with_platform(
            PathBuf::from("/roms/mystery.zip"),
            1,
            "N64",
            "folder_alias",
        )]);
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(stale_snapshot.clone()),
            last_scan_summary: None,
        };
        let archive_path = PathBuf::from("/roms/mystery.zip");
        let (sender, receiver) = mpsc::channel();
        app.platform_action = Some(RunningPlatformAction {
            archive_path: archive_path.clone(),
            receiver,
        });
        sender
            .send(Err(
                "mystery.zip is not yet in the library database".to_string()
            ))
            .unwrap();

        app.poll_platform_action(&egui::Context::default());

        assert!(app.platform_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("not yet in the library database"));
        assert!(
            app.history
                .entries()
                .any(
                    |entry| entry.archive_path.as_deref() == Some(archive_path.as_path())
                        && entry.outcome == ActivityOutcome::Failed
                ),
        );
        // A failure must never trigger a database reload - the existing
        // cached row is left exactly as it was.
        match &app.database_state {
            DatabaseState::Ready { snapshot, .. } => {
                assert_eq!(snapshot.archives, stale_snapshot.archives);
                assert_eq!(snapshot.database_path, stale_snapshot.database_path);
            }
            other => panic!(
                "expected the stale Ready snapshot to survive untouched, got status {}",
                other.status_label()
            ),
        }
    }

    #[test]
    fn database_reload_removes_a_newly_known_row_from_the_unknown_only_filtered_list() {
        // The second half of "assigning a platform removes the row after
        // the asynchronous database refresh" - the first half
        // (poll_platform_action starting the reload) is covered by
        // poll_platform_action_success_refreshes_the_database_cache_asynchronously;
        // this covers what happens once that reload actually settles,
        // exactly as poll_database_load's own other tests do (a
        // synthetic channel/message, not a real database path).
        let mut app = app_for_operation_tests();
        app.library_filters.unknown_platform = true;
        let archive_path = PathBuf::from("/roms/mystery.zip");
        app.selected_archive = Some(archive_path.clone());
        let generation = app.database_generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: Some(Box::new(cached_snapshot(vec![persisted_archive(
                archive_path.clone(),
                false,
            )]))),
            scanning: false,
        };
        let after_snapshot = cached_snapshot(vec![persisted_archive_with_platform(
            archive_path.clone(),
            1,
            "GameCube",
            MANUAL_PLATFORM_SOURCE,
        )]);
        sender
            .send((generation, Ok(DatabaseOutcome::Loaded(after_snapshot))))
            .unwrap();

        app.poll_database_load(&egui::Context::default());

        let merged = build_display_rows(&[], &[], app.database_state.snapshot());
        assert_eq!(merged.len(), 1);
        assert!(
            !merged[0].unknown_platform,
            "the row must now reflect the manual assignment"
        );
        assert!(
            !app.library_filters.matches(&merged[0]),
            "it must no longer match Show unknown only"
        );

        // Selection safety (requirement 4): the path-based selection is
        // untouched and still resolves the archive's up-to-date details,
        // even though it is no longer visible in the unknown-only
        // filtered list - the existing, intentional
        // selection-independent-of-filter-visibility behavior (see
        // `RowOrigin`'s doc comment), not a new special case.
        assert_eq!(
            app.selected_archive.as_deref(),
            Some(archive_path.as_path())
        );
        assert_eq!(
            selected_persisted_archive(
                app.database_state.snapshot(),
                app.selected_archive.as_deref()
            )
            .and_then(|persisted| persisted.platform.as_deref()),
            Some("GameCube")
        );
    }

    #[test]
    fn database_reload_adds_a_newly_unknown_row_when_a_manual_platform_is_cleared() {
        let mut app = app_for_operation_tests();
        app.library_filters.unknown_platform = true;
        let archive_path = PathBuf::from("/roms/mystery.zip");
        let generation = app.database_generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: Some(Box::new(cached_snapshot(vec![
                persisted_archive_with_platform(
                    archive_path.clone(),
                    1,
                    "GameCube",
                    MANUAL_PLATFORM_SOURCE,
                ),
            ]))),
            scanning: false,
        };
        let after_snapshot = cached_snapshot(vec![persisted_archive(archive_path, false)]);
        sender
            .send((generation, Ok(DatabaseOutcome::Loaded(after_snapshot))))
            .unwrap();

        app.poll_database_load(&egui::Context::default());

        let merged = build_display_rows(&[], &[], app.database_state.snapshot());
        assert_eq!(merged.len(), 1);
        assert!(merged[0].unknown_platform);
        assert!(
            app.library_filters.matches(&merged[0]),
            "clearing manual back to unknown must make the row match Show unknown only again"
        );
    }

    #[test]
    fn filtered_rows_index_cache_is_recomputed_not_left_stale_after_a_database_reload() {
        let mut app = app_for_operation_tests();
        app.filter = "mystery".to_string();
        // A deliberately stale/out-of-bounds cached index list, as if
        // left over from a previous, now-invalid merged row shape -
        // poll_database_load must never trust or reuse this without
        // recomputing it fresh against the new merge.
        app.filtered_rows = Some(vec![0, 1, 2, 99]);
        let generation = app.database_generation;
        let (sender, receiver) = mpsc::channel::<DatabaseMessage>();
        app.database_state = DatabaseState::Loading {
            generation,
            receiver,
            previous: None,
            scanning: false,
        };
        let snapshot = cached_snapshot(vec![persisted_archive(
            PathBuf::from("/roms/mystery.zip"),
            false,
        )]);
        sender
            .send((generation, Ok(DatabaseOutcome::Loaded(snapshot))))
            .unwrap();

        app.poll_database_load(&egui::Context::default());

        let recomputed = app
            .filtered_rows
            .expect("filtered_rows must be recomputed, not left stale");
        assert_eq!(
            recomputed,
            vec![0],
            "must be freshly computed against the new merged row set, not the stale placeholder"
        );
    }

    #[test]
    fn toggling_show_unknown_only_performs_no_database_write_or_scan() {
        let mut app = app_for_operation_tests();
        let generation_before = app.database_generation;
        let refresh_generation_before = app.refresh_generation;

        app.library_filters.unknown_platform = true;
        app.library_filters.unknown_platform = false;

        assert_eq!(
            app.database_generation, generation_before,
            "toggling the filter must never start a database load"
        );
        assert_eq!(
            app.refresh_generation, refresh_generation_before,
            "toggling the filter must never trigger a live rescan either"
        );
        assert!(matches!(
            app.database_state,
            DatabaseState::NotCreated { .. }
        ));
    }

    #[test]
    fn mount_action_availability_is_unaffected_by_library_filters() {
        let mut app = app_for_operation_tests();
        let busy_before = app.is_busy();

        app.library_filters.unknown_platform = true;
        app.library_filters.present = true;
        app.library_filters.missing = true;

        assert_eq!(
            app.is_busy(),
            busy_before,
            "library_filters must never influence mount/unmount action-safety gating"
        );
    }

    #[test]
    fn apply_platform_action_sets_and_clears_a_manual_platform() {
        let dir = database_test_dir("apply-platform-set-clear");
        let source = dir.join("source");
        let mount = dir.join("mount");
        let archive_path = write_archive_file(&source, "n64/Luigis_Mansion.zip", b"contents");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }

        let change = apply_platform_action_at(
            &database_path,
            &archive_path,
            &PlatformAction::Set("GameCube".to_string()),
        )
        .unwrap();
        assert_eq!(change.old_platform.as_deref(), Some("N64"));
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));

        let clear_change =
            apply_platform_action_at(&database_path, &archive_path, &PlatformAction::Clear)
                .unwrap();
        // Immediate exposure of the automatic result, no rescan involved.
        assert_eq!(clear_change.new_platform.as_deref(), Some("N64"));
        assert_eq!(clear_change.new_source.as_deref(), Some("folder_alias"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_platform_action_errors_clearly_when_not_yet_scanned() {
        let dir = database_test_dir("apply-platform-not-scanned");
        let database_path = dir.join("library.sqlite3");
        Database::open_or_create(&database_path).unwrap();

        let error = apply_platform_action_at(
            &database_path,
            Path::new("/roms/never-scanned.zip"),
            &PlatformAction::Set("GameCube".to_string()),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not yet in the library database")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn apply_platform_action_assigns_a_non_utf8_archive_path_on_unix() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = database_test_dir("apply-platform-non-utf8");
        let source = dir.join("source");
        let mount = dir.join("mount");
        std::fs::create_dir_all(&source).unwrap();
        let mut invalid_name = b"fo".to_vec();
        invalid_name.push(0x80);
        invalid_name.extend_from_slice(b"o.zip");
        let archive_path = source.join(OsString::from_vec(invalid_name));
        assert!(
            archive_path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );
        std::fs::write(&archive_path, b"contents").unwrap();
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }

        let change = apply_platform_action_at(
            &database_path,
            &archive_path,
            &PlatformAction::Set("GameCube".to_string()),
        )
        .unwrap();

        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // -------------------------------------------------------------
    // Bulk manual platform assignment
    // -------------------------------------------------------------

    #[test]
    fn single_click_replaces_the_whole_selection_with_one_row() {
        let mut selected_archives: HashSet<PathBuf> =
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect();
        let mut selected_archive = Some(PathBuf::from("/roms/a.zip"));
        let clicked = PathBuf::from("/roms/c.zip");

        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            clicked.clone(),
            false,
        );

        assert_eq!(selected_archives, [clicked.clone()].into_iter().collect());
        assert_eq!(selected_archive, Some(clicked));
    }

    #[test]
    fn ctrl_click_toggles_individual_rows_without_touching_others() {
        let path_a = PathBuf::from("/roms/a.zip");
        let path_b = PathBuf::from("/roms/b.zip");
        let mut selected_archives: HashSet<PathBuf> = [path_a.clone()].into_iter().collect();
        let mut selected_archive = Some(path_a.clone());

        // Ctrl-click an unselected row: added, path_a untouched.
        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path_b.clone(),
            true,
        );
        assert_eq!(
            selected_archives,
            [path_a.clone(), path_b.clone()].into_iter().collect()
        );
        assert_eq!(selected_archive, Some(path_b.clone()));

        // Ctrl-click an already-selected row: removed, the other stays.
        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path_a.clone(),
            true,
        );
        assert_eq!(selected_archives, [path_b].into_iter().collect());
        assert_eq!(selected_archive, Some(path_a));
    }

    #[test]
    fn ctrl_click_can_deselect_the_last_remaining_row() {
        let path = PathBuf::from("/roms/a.zip");
        let mut selected_archives: HashSet<PathBuf> = [path.clone()].into_iter().collect();
        let mut selected_archive = Some(path.clone());

        apply_row_click(&mut selected_archives, &mut selected_archive, path, true);

        assert!(selected_archives.is_empty());
    }

    /// Simulates a real two-frame click gesture on the row `render_row`
    /// paints (press in frame 1, release in frame 2 - egui requires a
    /// widget to already be known/hovered from a prior frame before the
    /// frame that releases on it can register `Response::clicked()`;
    /// `render_row` must paint the *same* row - same `id_source` - in
    /// both frames for egui to track this correctly, exactly as
    /// `show_loaded_data` does across real consecutive UI frames).
    /// Returns frame 2's `Response` plus whatever `ui.input(|i|
    /// i.modifiers.ctrl)` reads during that same frame - proving the
    /// *real* egui event path (not just the pure `apply_row_click`
    /// helper in isolation) delivers a working click with an accurate
    /// modifier reading, which is the actual bug this test guards
    /// against regressing.
    fn run_frame(
        ctx: &egui::Context,
        raw_input: egui::RawInput,
        render_row: &impl Fn(&mut egui::Ui) -> egui::Response,
    ) -> (egui::Response, bool) {
        let mut response = None;
        let mut ctrl_held = false;
        let _ = ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                response = Some(render_row(ui));
                ctrl_held = ui.input(|i| i.modifiers.ctrl);
            });
        });
        (response.unwrap(), ctrl_held)
    }

    /// Simulates a real click gesture on the row `render_row` paints, at
    /// `pos`, with `modifiers` held throughout. `render_row` must paint
    /// the *same* row - same `id_source` - every time it is called, so
    /// egui recognizes it as the same persistent widget across frames.
    ///
    /// egui's hit-testing for a given frame's pointer events is computed
    /// from the widget rects *registered in the previous frame* (this
    /// frame's widgets have not been laid out yet when input is
    /// processed) - see `egui::interaction::interact`. So registering a
    /// click on a widget that has never been rendered before takes three
    /// frames, not one: frame 1 merely registers the row's rect; frame 2
    /// (now hit-testable) carries the press event, setting egui's
    /// internal "potential click" on this row; frame 3 (hit-testable
    /// again) carries the release event, which is where
    /// `Response::clicked()` actually becomes true. This mirrors real
    /// user input closely enough to exercise the genuine event path this
    /// test suite is guarding (see the three-separate-`ctx.run` structure
    /// below), rather than only calling `apply_row_click` directly with a
    /// hand-built `bool`.
    fn simulate_row_click(
        ctx: &egui::Context,
        pos: egui::Pos2,
        modifiers: egui::Modifiers,
        render_row: impl Fn(&mut egui::Ui) -> egui::Response,
    ) -> (egui::Response, bool) {
        let moved_only = egui::RawInput {
            modifiers,
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        };
        run_frame(ctx, moved_only, &render_row);

        let press = egui::RawInput {
            modifiers,
            events: vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            }],
            ..Default::default()
        };
        run_frame(ctx, press, &render_row);

        let release = egui::RawInput {
            modifiers,
            events: vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
            ..Default::default()
        };
        run_frame(ctx, release, &render_row)
    }

    fn test_row_cells() -> [&'static str; 4] {
        ["Xbox", "Pending", "/roms/a.zip", "/mnt/Xbox/a"]
    }

    #[test]
    fn real_egui_click_on_the_row_registers_and_reports_no_modifier() {
        let ctx = egui::Context::default();
        let path = PathBuf::from("/roms/a.zip");

        let (response, ctrl_held) = simulate_row_click(
            &ctx,
            egui::pos2(50.0, 12.0),
            egui::Modifiers::default(),
            |ui| show_data_row(ui, &test_row_cells(), 24.0, &path, false, false, None),
        );

        assert!(
            response.clicked(),
            "the real row Response must register the click"
        );
        assert!(!ctrl_held, "no modifier key was simulated as held");
    }

    #[test]
    fn real_egui_ctrl_click_on_the_row_reaches_the_selection_helper() {
        // This is the actual bug report: verify Ctrl reaches the row's
        // click handling through the real egui event path, not just
        // through apply_row_click called directly with a hand-built bool.
        let ctx = egui::Context::default();
        let path = PathBuf::from("/roms/a.zip");

        let (response, ctrl_held) =
            simulate_row_click(&ctx, egui::pos2(50.0, 12.0), egui::Modifiers::CTRL, |ui| {
                show_data_row(ui, &test_row_cells(), 24.0, &path, false, false, None)
            });
        assert!(response.clicked(), "the click itself must still register");
        assert!(
            ctrl_held,
            "Ctrl must read as held during the real click's frame"
        );

        let mut selected_archives: HashSet<PathBuf> = HashSet::new();
        let mut selected_archive = None;
        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path.clone(),
            ctrl_held,
        );
        assert_eq!(
            selected_archives,
            [path].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn real_ordinary_click_replaces_the_selection() {
        let ctx = egui::Context::default();
        let path_a = PathBuf::from("/roms/a.zip");
        let path_b = PathBuf::from("/roms/b.zip");
        let mut selected_archives: HashSet<PathBuf> = [path_a.clone()].into_iter().collect();
        let mut selected_archive = Some(path_a);

        let (response, ctrl_held) = simulate_row_click(
            &ctx,
            egui::pos2(50.0, 12.0),
            egui::Modifiers::default(),
            |ui| show_data_row(ui, &test_row_cells(), 24.0, &path_b, false, false, None),
        );
        assert!(response.clicked());
        assert!(!ctrl_held);

        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path_b.clone(),
            ctrl_held,
        );

        assert_eq!(
            selected_archives,
            [path_b].into_iter().collect::<HashSet<_>>(),
            "an ordinary click through the real event path must replace the selection"
        );
    }

    #[test]
    fn real_ctrl_click_adds_a_second_exact_path() {
        let ctx = egui::Context::default();
        let path_a = PathBuf::from("/roms/a.zip");
        let path_b = PathBuf::from("/roms/b.zip");
        let mut selected_archives: HashSet<PathBuf> = [path_a.clone()].into_iter().collect();
        let mut selected_archive = Some(path_a.clone());

        let (response, ctrl_held) =
            simulate_row_click(&ctx, egui::pos2(50.0, 12.0), egui::Modifiers::CTRL, |ui| {
                show_data_row(ui, &test_row_cells(), 24.0, &path_b, false, false, None)
            });
        assert!(response.clicked());
        assert!(ctrl_held);

        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path_b.clone(),
            ctrl_held,
        );

        assert_eq!(
            selected_archives,
            [path_a, path_b].into_iter().collect::<HashSet<_>>(),
            "a real Ctrl-click must add to, not replace, the selection"
        );
    }

    #[test]
    fn real_ctrl_click_removes_an_already_selected_path() {
        let ctx = egui::Context::default();
        let path_a = PathBuf::from("/roms/a.zip");
        let path_b = PathBuf::from("/roms/b.zip");
        let mut selected_archives: HashSet<PathBuf> =
            [path_a.clone(), path_b.clone()].into_iter().collect();
        let mut selected_archive = Some(path_b.clone());

        // is_selected = true, since path_a is already in the set - the
        // row's own highlighted/pressed styling must not prevent the
        // click (or its modifiers) from registering.
        let (response, ctrl_held) =
            simulate_row_click(&ctx, egui::pos2(50.0, 12.0), egui::Modifiers::CTRL, |ui| {
                show_data_row(ui, &test_row_cells(), 24.0, &path_a, true, false, None)
            });
        assert!(response.clicked());
        assert!(ctrl_held);

        apply_row_click(
            &mut selected_archives,
            &mut selected_archive,
            path_a,
            ctrl_held,
        );

        assert_eq!(
            selected_archives,
            [path_b].into_iter().collect::<HashSet<_>>(),
            "a real Ctrl-click on an already-selected row must remove it"
        );
    }

    #[test]
    fn clicking_text_inside_the_row_behaves_the_same_as_blank_row_space() {
        let ctx = egui::Context::default();
        let path = PathBuf::from("/roms/a.zip");

        // COLUMN_WIDTHS = [120.0, 120.0, 440.0, 520.0]; a position early
        // in the first column lands squarely on rendered text, while a
        // position just past the first column (in the item-spacing gap
        // the old four-separate-Buttons layout never sensed clicks in)
        // must click exactly as reliably - proving there is now one
        // consistent Sense::click response for the whole row, not one
        // per cell with unsensed gaps between them.
        let (on_text, _) = simulate_row_click(
            &ctx,
            egui::pos2(10.0, 12.0),
            egui::Modifiers::default(),
            |ui| show_data_row(ui, &test_row_cells(), 24.0, &path, false, false, None),
        );
        assert!(on_text.clicked(), "a click on rendered text must register");

        let (on_gap, _) = simulate_row_click(
            &ctx,
            egui::pos2(121.0, 12.0),
            egui::Modifiers::default(),
            |ui| show_data_row(ui, &test_row_cells(), 24.0, &path, false, false, None),
        );
        assert!(
            on_gap.clicked(),
            "a click in the inter-column gap must register exactly the same as on text"
        );
    }

    /// The Ctrl+Up/Down "focus doesn't visibly move" bug, reproduced and
    /// fixed at the paint level: before this fix, `multi_selected` and
    /// `focused` collapsed into one `is_selected` flag that painted the
    /// exact same fill either way, so moving focus (via Ctrl+arrow)
    /// between two rows that were both already multi-selected painted
    /// nothing different at all. Inspects `FullOutput::shapes` - the real
    /// painted output, not a re-implementation of the paint logic - to
    /// prove multi-selection (a fill) and focus (a stroke) are genuinely
    /// distinct paint calls, independently present or absent.
    #[test]
    fn focused_row_paints_a_distinct_stroke_from_the_multi_selected_fill() {
        let ctx = egui::Context::default();
        let path = PathBuf::from("/roms/a.zip");

        let capture = |multi_selected: bool, focused: bool| -> egui::FullOutput {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_data_row(
                        ui,
                        &test_row_cells(),
                        24.0,
                        &path,
                        multi_selected,
                        focused,
                        None,
                    );
                });
            })
        };
        let visuals = ctx.style().visuals.clone();

        fn has_stroke(output: &egui::FullOutput, color: egui::Color32) -> bool {
            output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(rect) if rect.stroke.width > 0.0 && rect.stroke.color == color)
            })
        }
        fn has_fill(output: &egui::FullOutput, color: egui::Color32) -> bool {
            output.shapes.iter().any(
                |clipped| matches!(&clipped.shape, egui::Shape::Rect(rect) if rect.fill == color),
            )
        }

        let neither = capture(false, false);
        assert!(!has_fill(&neither, visuals.selection.bg_fill));
        assert!(!has_stroke(&neither, visuals.warn_fg_color));

        let multi_selected_only = capture(true, false);
        assert!(has_fill(&multi_selected_only, visuals.selection.bg_fill));
        assert!(
            !has_stroke(&multi_selected_only, visuals.warn_fg_color),
            "a multi-selected but unfocused row must not show the focus ring"
        );

        let focused_only = capture(false, true);
        assert!(has_stroke(&focused_only, visuals.warn_fg_color));
        assert!(
            !has_fill(&focused_only, visuals.selection.bg_fill),
            "a focused but not multi-selected row must not show the multi-select fill"
        );

        let both = capture(true, true);
        assert!(
            has_fill(&both, visuals.selection.bg_fill) && has_stroke(&both, visuals.warn_fg_color),
            "a row that is both focused and multi-selected must show both the fill and the ring - \
             this is the exact case Ctrl+Up/Down moving focus within a multi-selection hits"
        );
    }

    #[test]
    fn bulk_action_bar_renders_only_when_more_than_one_row_is_selected() {
        // Proves the *rendering function itself* stays empty/grows the
        // layout appropriately, not just its extracted visibility
        // predicate (bulk_action_bar_requires_more_than_one_selected_row
        // below already covers that in isolation) - `ui.cursor()`
        // advancing down the panel is a real, observable side effect of
        // `show_bulk_platform_action_bar` actually painting the
        // separator/frame/combo box, not a stand-in for it.
        let ctx = egui::Context::default();
        let mut bulk_platform_choice: Option<String> = None;

        let mut one_selected_extra_height = -1.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = ui.cursor().top();
                let mut selected: HashSet<PathBuf> =
                    [PathBuf::from("/roms/a.zip")].into_iter().collect();
                let _ = show_bulk_platform_action_bar(
                    ui,
                    &mut selected,
                    &mut bulk_platform_choice,
                    false,
                );
                one_selected_extra_height = ui.cursor().top() - before;
            });
        });
        assert_eq!(
            one_selected_extra_height, 0.0,
            "one selected row must render nothing"
        );

        let mut two_selected_extra_height = 0.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let before = ui.cursor().top();
                let mut selected: HashSet<PathBuf> =
                    [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                        .into_iter()
                        .collect();
                let _ = show_bulk_platform_action_bar(
                    ui,
                    &mut selected,
                    &mut bulk_platform_choice,
                    false,
                );
                two_selected_extra_height = ui.cursor().top() - before;
            });
        });
        assert!(
            two_selected_extra_height > 0.0,
            "two selected rows must actually render the bulk action bar"
        );
    }

    /// Renders the *real* `show_loaded_data` - the exact parent GUI
    /// section `update()` calls in production, not any of its inner
    /// helpers in isolation - into a real `egui::Context` frame, driving
    /// it through the exact same fields `ArchiveFsApp` itself owns across
    /// frames. This is what closes the exact gap the original "bulk bar
    /// never appears" bug slipped through: a test that only called an
    /// inner helper (or its pure predicate) directly could never have
    /// caught a layout-ordering bug in `show_loaded_data` itself, since
    /// that function was never exercised at all. Used the same way for
    /// the milestone's keyboard-navigation and header-sort-click tests:
    /// each call can supply its own synthetic `RawInput` (key events,
    /// clicks, focus changes) while every other field persists across
    /// calls exactly as it would across real frames.
    struct RealLoadedDataHarness {
        filter: String,
        filtered_rows: Option<Vec<usize>>,
        selected_archive: Option<PathBuf>,
        selected_archives: HashSet<PathBuf>,
        library_filters: LibraryRowFilters,
        sort_field: Option<SortField>,
        sort_ascending: bool,
        library_scroll_offset: f32,
    }

    impl RealLoadedDataHarness {
        fn new() -> Self {
            Self {
                filter: String::new(),
                filtered_rows: None,
                selected_archive: None,
                selected_archives: HashSet::new(),
                library_filters: LibraryRowFilters::default(),
                sort_field: None,
                sort_ascending: true,
                library_scroll_offset: 0.0,
            }
        }

        /// Renders one frame with `input`, returning the whole panel's
        /// rendered content height - a real, observable side effect of
        /// everything `show_loaded_data` actually painted this frame
        /// (mount-all/doctor/search/filters/table/bulk bar/details panel
        /// all included), never a pixel-position assertion.
        fn render(&mut self, ctx: &egui::Context, data: &LoadedData, input: egui::RawInput) -> f32 {
            let mut confirm_unmount = None;
            let mut confirm_lazy_unmount = None;
            let mut confirm_lazy_unmount_final = None;
            let mut confirm_mount_all = None;
            let mut focus_mount_all_cancel = false;
            let mut confirm_unmount_all = None;
            let mut focus_unmount_all_cancel = false;
            let mut focus_lazy_cancel = false;
            let mut focus_final_lazy_cancel = false;
            let lazy_unmount_offers = HashSet::new();
            let remount_offers = HashSet::new();
            let mut cleanup_after_unmount = false;
            let mut history = OperationHistory::default();
            let mut platform_choice = None;
            let mut platform_custom_text = String::new();
            let mut bulk_platform_choice = None;
            let mut confirm_remove_missing = None;

            let mut panel_height = 0.0;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = show_loaded_data(
                        ui,
                        data,
                        LoadedViewState {
                            filter: &mut self.filter,
                            filtered_rows: &mut self.filtered_rows,
                            selected_archive: &mut self.selected_archive,
                            operation: None,
                            busy: false,
                            feedback: None,
                            confirm_unmount: &mut confirm_unmount,
                            confirm_lazy_unmount: &mut confirm_lazy_unmount,
                            confirm_lazy_unmount_final: &mut confirm_lazy_unmount_final,
                            confirm_mount_all: &mut confirm_mount_all,
                            focus_mount_all_cancel: &mut focus_mount_all_cancel,
                            confirm_unmount_all: &mut confirm_unmount_all,
                            focus_unmount_all_cancel: &mut focus_unmount_all_cancel,
                            focus_lazy_cancel: &mut focus_lazy_cancel,
                            focus_final_lazy_cancel: &mut focus_final_lazy_cancel,
                            lazy_unmount_offers: &lazy_unmount_offers,
                            remount_offers: &remount_offers,
                            cleanup_after_unmount: &mut cleanup_after_unmount,
                            mount_all_result: None,
                            unmount_all_result: None,
                            history: &mut history,
                            cached: None,
                            library_filters: &mut self.library_filters,
                            platform_choice: &mut platform_choice,
                            platform_custom_text: &mut platform_custom_text,
                            platform_busy: false,
                            selected_archives: &mut self.selected_archives,
                            bulk_platform_choice: &mut bulk_platform_choice,
                            bulk_platform_busy: false,
                            missing_removal_available: false,
                            missing_removal_busy: false,
                            confirm_remove_missing: &mut confirm_remove_missing,
                            sort_field: &mut self.sort_field,
                            sort_ascending: &mut self.sort_ascending,
                            library_scroll_offset: &mut self.library_scroll_offset,
                        },
                    );
                    panel_height = ui.min_rect().height();
                });
            });
            panel_height
        }
    }

    // A bounded, realistic `screen_rect` is required here, not just for
    // fidelity: with `RawInput::default()` (no `screen_rect`), egui falls
    // back to a very large default canvas. Both `ScrollArea`s in
    // `show_loaded_data` use `auto_shrink([false, false])`, i.e. "always
    // claim all remaining space" - against a near-infinite canvas that
    // remaining space is effectively constant, so the scroll areas
    // silently absorb whatever height content placed above them adds and
    // the panel's total `min_rect().height()` comes out identical either
    // way. Bounding the panel to a realistic window size makes the inner
    // vertical `ScrollArea`'s `.max(row_height)` floor (see
    // `show_loaded_data`) bite, so it can no longer fully compensate -
    // only then does content above it actually show up in the total
    // height, which is what makes a height comparison a meaningful "did
    // the real layout render it" check instead of a tautology.
    fn bounded_test_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1000.0, 250.0),
            )),
            ..Default::default()
        }
    }

    #[test]
    fn real_show_loaded_data_hides_the_bulk_bar_for_one_selected_row() {
        let ctx = egui::Context::default();
        let data = empty_loaded_data("/mount");

        let mut one_selected = RealLoadedDataHarness::new();
        one_selected.selected_archives = [PathBuf::from("/roms/a.zip")].into_iter().collect();
        let one_selected_height = one_selected.render(&ctx, &data, bounded_test_input());

        let mut none_selected = RealLoadedDataHarness::new();
        let none_selected_height = none_selected.render(&ctx, &data, bounded_test_input());

        assert_eq!(
            one_selected_height, none_selected_height,
            "one selected row must render exactly like no selection - no bulk bar"
        );
    }

    #[test]
    fn real_show_loaded_data_shows_the_bulk_bar_for_two_selected_rows() {
        // This is the exact scenario the Nobara bug report described:
        // 3+ rows selected, but the bar never appeared because it was
        // being rendered after a `ScrollArea::auto_shrink([false, false])`
        // that claimed all remaining vertical space. Rendering the real
        // `show_loaded_data` end to end - not a helper condition, not
        // `show_bulk_platform_action_bar` alone - is what proves that
        // regression is actually fixed.
        let ctx = egui::Context::default();
        let data = empty_loaded_data("/mount");

        let mut one_selected = RealLoadedDataHarness::new();
        one_selected.selected_archives = [PathBuf::from("/roms/a.zip")].into_iter().collect();
        let one_selected_height = one_selected.render(&ctx, &data, bounded_test_input());

        let mut three_selected = RealLoadedDataHarness::new();
        three_selected.selected_archives = [
            PathBuf::from("/roms/a.zip"),
            PathBuf::from("/roms/b.zip"),
            PathBuf::from("/roms/c.zip"),
        ]
        .into_iter()
        .collect();
        let three_selected_height = three_selected.render(&ctx, &data, bounded_test_input());

        assert!(
            three_selected_height > one_selected_height,
            "3 selected archives must render additional content (the bulk action bar) that \
             1 selected archive does not - got {one_selected_height} vs {three_selected_height}"
        );
    }

    #[test]
    fn clear_selection_button_click_empties_the_same_selected_archives_set() {
        let ctx = egui::Context::default();
        let selected_archives = std::cell::RefCell::new(
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect::<HashSet<PathBuf>>(),
        );
        let bulk_platform_choice = std::cell::RefCell::new(None::<String>);

        // Renders the real `show_bulk_platform_action_bar` - the same
        // production function `show_loaded_data` calls - through a
        // `RefCell` so this closure can implement `Fn` (required by
        // `simulate_row_click`/`run_frame`, which call it repeatedly
        // across the 3-frame click sequence) while still mutating the
        // *same* selection set on every call.
        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            let mut choice = bulk_platform_choice.borrow_mut();
            ui.scope(|ui| {
                let _ = show_bulk_platform_action_bar(ui, &mut selected, &mut choice, false);
            })
            .response
        };

        // Measurement pass: find the rendered bar's bounding rect using
        // the exact same production function, before attempting to click
        // it - never a hardcoded/guessed pixel position.
        let mut bar_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                bar_rect = Some(render(ui).rect);
            });
        });
        let bar_rect = bar_rect.unwrap();
        assert_eq!(
            selected_archives.borrow().len(),
            2,
            "the measurement pass must not itself change the selection"
        );

        // "Clear selection" is the rightmost control in this row (no
        // spinner, since bulk_platform_busy is false here).
        let click_pos = egui::pos2(bar_rect.right() - 15.0, bar_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert!(
            selected_archives.borrow().is_empty(),
            "clicking Clear selection must empty the exact same HashSet row highlighting reads from"
        );
    }

    #[test]
    fn bulk_action_bar_requires_more_than_one_selected_row() {
        let mut selected: HashSet<PathBuf> = HashSet::new();
        assert!(!bulk_action_bar_visible(&selected));

        selected.insert(PathBuf::from("/roms/a.zip"));
        assert!(!bulk_action_bar_visible(&selected));

        selected.insert(PathBuf::from("/roms/b.zip"));
        assert!(bulk_action_bar_visible(&selected));
    }

    // -----------------------------------------------------------------
    // v0.3.8-alpha: library-table usability milestone - pure-logic tests.
    // -----------------------------------------------------------------

    #[test]
    fn selection_status_text_matches_the_hashset_count() {
        assert_eq!(selection_status_text(0), "No archives selected");
        assert_eq!(selection_status_text(1), "1 archive selected");
        assert_eq!(selection_status_text(2), "2 archives selected");
        assert_eq!(selection_status_text(11), "11 archives selected");
    }

    #[test]
    fn library_table_message_distinguishes_empty_library_from_zero_filter_results() {
        assert_eq!(
            library_table_message(true, 0),
            Some(LibraryTableMessage::EmptyLibrary),
            "an empty library must report EmptyLibrary regardless of visible_count"
        );
        assert_eq!(
            library_table_message(false, 0),
            Some(LibraryTableMessage::NoFilterResults),
            "archives exist but none are visible must report NoFilterResults"
        );
        assert_eq!(
            library_table_message(false, 3),
            None,
            "archives exist and some are visible: no message, render the table"
        );
    }

    #[test]
    fn select_all_visible_returns_only_the_visible_paths_not_the_hidden_ones() {
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
        ];
        // Only rows 0 and 2 pass the current filter - row 1 is hidden.
        let visible_indices = vec![0usize, 2usize];

        let selected = select_all_visible(&merged_rows, &visible_indices);

        assert_eq!(
            selected,
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
                .into_iter()
                .collect::<HashSet<_>>(),
            "Ctrl+A must select exactly the visible rows, never the filtered-out one"
        );
    }

    // -----------------------------------------------------------------
    // v0.4.2-alpha follow-up: explicit "Select all visible" button.
    // -----------------------------------------------------------------

    fn row_with_fields_and_origin(path: &str, origin: RowOrigin) -> ArchiveRow {
        let mut row = row_with_fields(path, "SNES", "state", path, path);
        row.origin = origin;
        row
    }

    #[test]
    fn select_all_visible_button_enabled_requires_at_least_one_visible_row() {
        assert!(!select_all_visible_button_enabled(0));
        assert!(select_all_visible_button_enabled(1));
        assert!(select_all_visible_button_enabled(3));
    }

    #[test]
    fn select_all_visible_button_click_selects_all_currently_visible_rows() {
        let ctx = egui::Context::default();
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
        ];
        let visible_indices = vec![0usize, 1usize, 2usize];
        let selected_archives = std::cell::RefCell::new(HashSet::<PathBuf>::new());

        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        // Measurement pass: locate the rendered row's bounding rect via the
        // real production function, before attempting to click it.
        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();
        assert!(
            selected_archives.borrow().is_empty(),
            "the measurement pass must not itself change the selection"
        );

        // "Select all visible" is the rightmost control in this row.
        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *selected_archives.borrow(),
            [
                PathBuf::from("/roms/a.zip"),
                PathBuf::from("/roms/b.zip"),
                PathBuf::from("/roms/c.zip"),
            ]
            .into_iter()
            .collect::<HashSet<_>>(),
            "clicking Select all visible must select every currently visible row"
        );
    }

    #[test]
    fn select_all_visible_button_click_never_selects_hidden_filtered_rows() {
        let ctx = egui::Context::default();
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
        ];
        // Row 1 (b.zip) is filtered out - only positions 0 and 2 are
        // currently visible.
        let visible_indices = vec![0usize, 2usize];
        // b.zip was selected before the filter hid it (e.g. a leftover
        // selection from before the current search) - the button must
        // drop it, not merely fail to add it.
        let selected_archives = std::cell::RefCell::new(
            [PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect::<HashSet<PathBuf>>(),
        );

        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();

        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *selected_archives.borrow(),
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
                .into_iter()
                .collect::<HashSet<_>>(),
            "hidden b.zip must never end up selected, even though it was selected before \
             the current filter hid it"
        );
    }

    #[test]
    fn select_all_visible_button_click_selects_only_search_filtered_rows() {
        let merged_rows = vec![
            row_with_fields("/roms/alpha.zip", "SNES", "Live", "alpha.zip", "/mnt/a"),
            row_with_fields("/roms/bravo.zip", "GBA", "Live", "bravo.zip", "/mnt/b"),
            row_with_fields("/roms/charlie.zip", "SNES", "Live", "charlie.zip", "/mnt/c"),
        ];
        // Mirrors the exact state right after a real search-filter frame
        // (see `real_ctrl_a_selects_only_the_currently_visible_filtered_rows`):
        // only positions 0 and 2 currently pass the search text, computed
        // the same way `show_loaded_data` derives `visible_indices` from
        // `filtered_rows` when no checkbox filter is active.
        let filtered_rows: Option<Vec<usize>> = Some(vec![0usize, 2usize]);
        let library_filters = LibraryRowFilters::default();
        let base_indices = filtered_rows
            .clone()
            .unwrap_or_else(|| (0..merged_rows.len()).collect());
        let visible_indices: Vec<usize> = if library_filters.is_active() {
            base_indices
                .into_iter()
                .filter(|&index| library_filters.matches(&merged_rows[index]))
                .collect()
        } else {
            base_indices
        };

        let ctx = egui::Context::default();
        let selected_archives = std::cell::RefCell::new(HashSet::<PathBuf>::new());
        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();

        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *selected_archives.borrow(),
            [
                PathBuf::from("/roms/alpha.zip"),
                PathBuf::from("/roms/charlie.zip"),
            ]
            .into_iter()
            .collect::<HashSet<_>>(),
            "only the archives that survive the current search text must be selected"
        );
    }

    #[test]
    fn select_all_visible_button_click_selects_only_missing_only_filtered_rows() {
        let merged_rows = vec![
            row_with_fields_and_origin("/roms/present.zip", RowOrigin::Live),
            row_with_fields_and_origin("/roms/missing-a.zip", RowOrigin::CachedMissing),
            row_with_fields_and_origin("/roms/missing-b.zip", RowOrigin::CachedMissing),
        ];
        // Mirrors `show_loaded_data`'s own `visible_indices` derivation
        // with the "Missing" checkbox filter active and no search text.
        let library_filters = LibraryRowFilters {
            missing: true,
            ..LibraryRowFilters::default()
        };
        let base_indices: Vec<usize> = (0..merged_rows.len()).collect();
        let visible_indices: Vec<usize> = base_indices
            .into_iter()
            .filter(|&index| library_filters.matches(&merged_rows[index]))
            .collect();

        let ctx = egui::Context::default();
        let selected_archives = std::cell::RefCell::new(HashSet::<PathBuf>::new());
        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();

        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *selected_archives.borrow(),
            [
                PathBuf::from("/roms/missing-a.zip"),
                PathBuf::from("/roms/missing-b.zip"),
            ]
            .into_iter()
            .collect::<HashSet<_>>(),
            "with 'Show missing only' active, the present row must never be selected"
        );
    }

    #[test]
    fn select_all_visible_button_click_does_nothing_when_zero_rows_are_visible() {
        let ctx = egui::Context::default();
        let merged_rows = vec![row_with_fields(
            "/roms/a.zip",
            "SNES",
            "Live",
            "a.zip",
            "/mnt/a",
        )];
        // The current search/filters hide every row.
        let visible_indices: Vec<usize> = Vec::new();
        let selected_archives = std::cell::RefCell::new(HashSet::<PathBuf>::new());

        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();

        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert!(
            selected_archives.borrow().is_empty(),
            "the disabled button must ignore the click when zero rows are visible"
        );
    }

    #[test]
    fn select_all_visible_button_click_is_idempotent_when_already_fully_selected() {
        let ctx = egui::Context::default();
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
        ];
        let visible_indices = vec![0usize, 1usize];
        let already_selected: HashSet<PathBuf> =
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect();
        let selected_archives = std::cell::RefCell::new(already_selected.clone());

        let render = |ui: &mut egui::Ui| -> egui::Response {
            let mut selected = selected_archives.borrow_mut();
            ui.scope(|ui| {
                show_selection_controls_row(ui, &merged_rows, &visible_indices, &mut selected);
            })
            .response
        };

        let mut row_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = Some(render(ui).rect);
            });
        });
        let row_rect = row_rect.unwrap();

        let click_pos = egui::pos2(row_rect.right() - 15.0, row_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *selected_archives.borrow(),
            already_selected,
            "clicking Select all visible while every visible row is already selected must \
             leave the selection unchanged"
        );
    }

    #[test]
    fn real_ctrl_a_keyboard_selection_is_unchanged_by_the_selection_controls_refactor() {
        // Guards against the "Select all visible" button's introduction -
        // and the resulting factoring of the selection-controls row into
        // `show_selection_controls_row` - having disturbed Ctrl+A, which
        // dispatches to the exact same `select_all_visible` helper from
        // `show_loaded_data` directly (see `real_ctrl_a_selects_only_the_currently_visible_filtered_rows`
        // for the original coverage this mirrors).
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "alpha.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "GBA", "Live", "bravo.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "SNES", "Live", "charlie.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        harness.filtered_rows = Some(vec![0usize, 2usize]);

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::A, egui::Modifiers::CTRL),
        );

        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
                .into_iter()
                .collect(),
            "Ctrl+A must still select exactly the visible rows after adding the button"
        );
    }

    #[test]
    fn select_all_visible_button_click_never_touches_duplicate_review_selection() {
        let mut app = app_for_operation_tests();
        // Duplicate Review's own independent selection.
        app.selected_duplicate_group = Some(DuplicateGroupIdentity {
            normalized_title: "sonic_the_hedgehog".to_string(),
            platform: "Genesis".to_string(),
        });
        app.selected_duplicate_archive = Some(PathBuf::from("/backup/Sonic.7z"));

        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
        ];
        let visible_indices = vec![0usize, 1usize];

        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show_selection_controls_row(
                    ui,
                    &merged_rows,
                    &visible_indices,
                    &mut app.selected_archives,
                );
            });
        });
        // A direct call proves the button's own code path, not just its
        // rendering, is exercised - `show_selection_controls_row` only
        // ever receives `&mut app.selected_archives`, never the duplicate
        // fields, so it is structurally unable to touch them.
        app.selected_archives = select_all_visible(&merged_rows, &visible_indices);

        assert_eq!(
            app.selected_archives,
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect(),
            "the ordinary-library selection must still update normally"
        );
        assert_eq!(
            app.selected_duplicate_group,
            Some(DuplicateGroupIdentity {
                normalized_title: "sonic_the_hedgehog".to_string(),
                platform: "Genesis".to_string(),
            }),
            "Duplicate Review's selected group must remain untouched"
        );
        assert_eq!(
            app.selected_duplicate_archive,
            Some(PathBuf::from("/backup/Sonic.7z")),
            "Duplicate Review's selected archive must remain untouched"
        );
    }

    #[test]
    fn next_focus_in_visible_order_steps_through_visible_sorted_order() {
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "Z", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "Y", "Live", "b.zip", "/mnt/b"),
            row_with_fields("/roms/c.zip", "X", "Live", "c.zip", "/mnt/c"),
        ];
        // Sorted by platform ascending would put c (X), b (Y), a (Z) in
        // that screen order - verify arrow navigation follows *this*
        // order, not the merged_rows insertion order.
        let visible_indices = vec![2usize, 1usize, 0usize];

        let first =
            next_focus_in_visible_order(&merged_rows, &visible_indices, None, ArrowDirection::Down);
        assert_eq!(first, Some(PathBuf::from("/roms/c.zip")));

        let second = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices,
            first.as_deref(),
            ArrowDirection::Down,
        );
        assert_eq!(second, Some(PathBuf::from("/roms/b.zip")));

        let third = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices,
            second.as_deref(),
            ArrowDirection::Down,
        );
        assert_eq!(third, Some(PathBuf::from("/roms/a.zip")));

        let back = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices,
            third.as_deref(),
            ArrowDirection::Up,
        );
        assert_eq!(back, Some(PathBuf::from("/roms/b.zip")));
    }

    #[test]
    fn next_focus_in_visible_order_clamps_at_both_ends_without_wrapping() {
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
        ];
        let visible_indices = vec![0usize, 1usize];

        let at_last = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices,
            Some(Path::new("/roms/b.zip")),
            ArrowDirection::Down,
        );
        assert_eq!(
            at_last,
            Some(PathBuf::from("/roms/b.zip")),
            "Down at the last visible row must stay put, not wrap to the first"
        );

        let at_first = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices,
            Some(Path::new("/roms/a.zip")),
            ArrowDirection::Up,
        );
        assert_eq!(
            at_first,
            Some(PathBuf::from("/roms/a.zip")),
            "Up at the first visible row must stay put, not wrap to the last"
        );
    }

    #[test]
    fn next_focus_in_visible_order_does_not_use_a_stale_index_after_filtering() {
        // Reproduces the exact stale-index hazard requirement 1 calls
        // out: focus is on a row that a *new* filter has just hidden.
        // `next_focus_in_visible_order` must re-derive the focus's
        // position from its exact path every call, never trust a
        // previously-computed row index into the old (pre-filter)
        // `visible_indices`.
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
        ];
        // Before filtering: focus sits on b.zip at visible position 1.
        let focus = Some(PathBuf::from("/roms/b.zip"));

        // A filter change just ran: b.zip no longer matches, only a.zip
        // and c.zip remain visible (at *new* positions 0 and 1).
        let visible_indices_after_filter = vec![0usize, 2usize];

        let next = next_focus_in_visible_order(
            &merged_rows,
            &visible_indices_after_filter,
            focus.as_deref(),
            ArrowDirection::Down,
        );

        // b.zip can no longer be found in the new visible list, so this
        // must fall back to the first visible row (a.zip) - never panic,
        // never silently keep pointing at the now-hidden b.zip, and never
        // misinterpret a stale numeric index as still meaning position 1
        // (which would incorrectly land on c.zip).
        assert_eq!(next, Some(PathBuf::from("/roms/a.zip")));
    }

    #[test]
    fn apply_arrow_focus_change_replaces_selection_without_ctrl_and_preserves_it_with_ctrl() {
        let mut selected_archives: HashSet<PathBuf> =
            [PathBuf::from("/roms/old.zip")].into_iter().collect();
        let mut selected_archive = Some(PathBuf::from("/roms/old.zip"));

        apply_arrow_focus_change(
            &mut selected_archives,
            &mut selected_archive,
            PathBuf::from("/roms/new.zip"),
            false,
        );
        assert_eq!(selected_archive, Some(PathBuf::from("/roms/new.zip")));
        assert_eq!(
            selected_archives,
            [PathBuf::from("/roms/new.zip")].into_iter().collect(),
            "without Ctrl, moving focus must replace the whole selection"
        );

        apply_arrow_focus_change(
            &mut selected_archives,
            &mut selected_archive,
            PathBuf::from("/roms/newer.zip"),
            true,
        );
        assert_eq!(selected_archive, Some(PathBuf::from("/roms/newer.zip")));
        assert_eq!(
            selected_archives,
            [PathBuf::from("/roms/new.zip")].into_iter().collect(),
            "with Ctrl held, moving focus must not touch the multi-selection"
        );
    }

    #[test]
    fn compute_scroll_offset_for_focus_does_not_move_when_already_visible() {
        // Focus at row 2 (rows 24px apart) sits entirely within a viewport
        // already scrolled to show rows 1-5 (offset 24.0, height 120.0) -
        // no scroll should be requested at all, so repeatedly pressing
        // Ctrl+Down within an already-visible range never jitters the view.
        let offset = compute_scroll_offset_for_focus(2, 24.0, 24.0, 120.0);
        assert_eq!(offset, 24.0);
    }

    #[test]
    fn compute_scroll_offset_for_focus_scrolls_up_when_focus_moves_above_the_viewport() {
        // Focus lands on row 1, but the viewport currently starts at
        // offset 48.0 (row 2) - row 1 is above the visible area, so the
        // offset must move up to align row 1 to the top edge exactly.
        let offset = compute_scroll_offset_for_focus(1, 24.0, 48.0, 120.0);
        assert_eq!(
            offset, 24.0,
            "focus above the viewport must scroll up to the row's own top edge"
        );
    }

    #[test]
    fn compute_scroll_offset_for_focus_scrolls_down_when_focus_moves_below_the_viewport() {
        // Viewport shows rows starting at offset 0.0, 120.0 tall (5 rows of
        // 24px each, rows 0-4). Focus moves to row 5, one past the bottom
        // edge - the offset must move down just enough to bring row 5's
        // bottom edge exactly to the viewport's bottom edge.
        let offset = compute_scroll_offset_for_focus(5, 24.0, 0.0, 120.0);
        assert_eq!(
            offset, 24.0,
            "focus below the viewport must scroll down to the row's own bottom edge"
        );
    }

    #[test]
    fn compute_scroll_offset_for_focus_never_scrolls_above_the_top() {
        let offset = compute_scroll_offset_for_focus(0, 24.0, 0.0, 500.0);
        assert_eq!(
            offset, 0.0,
            "a viewport taller than the content must clamp to 0"
        );
    }

    #[test]
    fn sort_visible_indices_orders_each_column_ascending_and_descending() {
        let merged_rows = vec![
            row_with_fields("/roms/a.zip", "SNES", "Missing", "b_archive.zip", "/mnt/z"),
            row_with_fields("/roms/b.zip", "GBA", "Live", "a_archive.zip", "/mnt/a"),
            row_with_fields("/roms/c.zip", "NES", "Pending", "c_archive.zip", "/mnt/m"),
        ];

        for field in COLUMN_SORT_FIELDS {
            let mut ascending = vec![0usize, 1usize, 2usize];
            sort_visible_indices(&merged_rows, &mut ascending, field, true);
            let ascending_keys: Vec<&str> = ascending
                .iter()
                .map(|&index| sort_field_key(&merged_rows[index], field))
                .collect();
            let mut expected_ascending = ascending_keys.clone();
            expected_ascending.sort();
            assert_eq!(
                ascending_keys, expected_ascending,
                "{field:?} ascending must be in ascending key order"
            );

            let mut descending = vec![0usize, 1usize, 2usize];
            sort_visible_indices(&merged_rows, &mut descending, field, false);
            let descending_keys: Vec<&str> = descending
                .iter()
                .map(|&index| sort_field_key(&merged_rows[index], field))
                .collect();
            let mut expected_descending = descending_keys.clone();
            expected_descending.sort();
            expected_descending.reverse();
            assert_eq!(
                descending_keys, expected_descending,
                "{field:?} descending must be in descending key order"
            );
        }
    }

    #[test]
    fn sort_visible_indices_breaks_ties_deterministically_by_exact_path() {
        // All three rows share the same platform - only the exact path
        // can break the tie, and it must do so the same way every time,
        // regardless of `merged_rows`'s incoming order.
        let merged_rows = vec![
            row_with_fields("/roms/charlie.zip", "SNES", "Live", "c.zip", "/mnt/c"),
            row_with_fields("/roms/alpha.zip", "SNES", "Live", "a.zip", "/mnt/a"),
            row_with_fields("/roms/bravo.zip", "SNES", "Live", "b.zip", "/mnt/b"),
        ];
        let mut indices = vec![0usize, 1usize, 2usize];
        sort_visible_indices(&merged_rows, &mut indices, SortField::Platform, true);

        let ordered_paths: Vec<&PathBuf> = indices.iter().map(|&i| &merged_rows[i].path).collect();
        assert_eq!(
            ordered_paths,
            vec![
                &PathBuf::from("/roms/alpha.zip"),
                &PathBuf::from("/roms/bravo.zip"),
                &PathBuf::from("/roms/charlie.zip"),
            ],
            "rows tied on platform must be ordered by their exact path"
        );

        // Reversing the incoming order must not change the outcome -
        // this is what makes the tie-break actually deterministic rather
        // than merely "stable" (stability alone would just preserve
        // whatever order happened to be handed in).
        let mut reversed_indices = vec![2usize, 1usize, 0usize];
        sort_visible_indices(
            &merged_rows,
            &mut reversed_indices,
            SortField::Platform,
            true,
        );
        let reversed_ordered_paths: Vec<&PathBuf> = reversed_indices
            .iter()
            .map(|&i| &merged_rows[i].path)
            .collect();
        assert_eq!(ordered_paths, reversed_ordered_paths);
    }

    #[test]
    fn sort_visible_indices_never_touches_merged_rows_itself() {
        // Requirement 2: sorting must not mutate database order or
        // archive identity - `merged_rows` (and by extension
        // `data.records`/`data.rows`) must come out byte-for-byte
        // unchanged; only the separate `indices` list may reorder.
        let merged_rows = vec![
            row_with_fields("/roms/z.zip", "Z", "Live", "z.zip", "/mnt/z"),
            row_with_fields("/roms/a.zip", "A", "Live", "a.zip", "/mnt/a"),
        ];
        let original_paths: Vec<PathBuf> = merged_rows.iter().map(|row| row.path.clone()).collect();

        let mut indices = vec![0usize, 1usize];
        sort_visible_indices(&merged_rows, &mut indices, SortField::Platform, true);

        let paths_after: Vec<PathBuf> = merged_rows.iter().map(|row| row.path.clone()).collect();
        assert_eq!(
            original_paths, paths_after,
            "merged_rows's own order must be untouched by sorting"
        );
        assert_eq!(indices, vec![1usize, 0usize]);
    }

    #[test]
    fn apply_header_click_selects_new_field_ascending_then_toggles_same_field() {
        let mut sort_field = None;
        let mut sort_ascending = true;

        apply_header_click(&mut sort_field, &mut sort_ascending, SortField::Platform);
        assert_eq!(sort_field, Some(SortField::Platform));
        assert!(sort_ascending, "a newly selected column starts ascending");

        apply_header_click(&mut sort_field, &mut sort_ascending, SortField::Platform);
        assert_eq!(sort_field, Some(SortField::Platform));
        assert!(
            !sort_ascending,
            "clicking the already-active column again toggles direction"
        );

        apply_header_click(&mut sort_field, &mut sort_ascending, SortField::Platform);
        assert!(sort_ascending, "toggling twice returns to ascending");

        apply_header_click(&mut sort_field, &mut sort_ascending, SortField::State);
        assert_eq!(sort_field, Some(SortField::State));
        assert!(
            sort_ascending,
            "selecting a different column resets to ascending"
        );
    }

    // -----------------------------------------------------------------
    // v0.3.8-alpha: library-table usability milestone - real
    // `egui::Context::run` input-path tests.
    // -----------------------------------------------------------------

    #[test]
    fn real_header_click_reaches_show_header_row_and_reports_the_clicked_column() {
        let ctx = egui::Context::default();
        let clicked_field: std::cell::RefCell<Option<SortField>> = std::cell::RefCell::new(None);

        let render = |ui: &mut egui::Ui| -> egui::Response {
            ui.scope(|ui| {
                if let Some(field) =
                    show_header_row(ui, &COLUMN_HEADERS, &COLUMN_SORT_FIELDS, 20.0, None, true)
                {
                    *clicked_field.borrow_mut() = Some(field);
                }
            })
            .response
        };

        let mut header_rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                header_rect = Some(render(ui).rect);
            });
        });
        let header_rect = header_rect.unwrap();

        // "Platform" is the first column - click well inside its left
        // edge, safely clear of any neighbouring column regardless of
        // exact font metrics.
        let click_pos = egui::pos2(header_rect.left() + 20.0, header_rect.center().y);
        simulate_row_click(&ctx, click_pos, egui::Modifiers::default(), render);

        assert_eq!(
            *clicked_field.borrow(),
            Some(SortField::Platform),
            "a real click on the header must be detected as the Platform column"
        );
    }

    /// A single key-press event, bundled with the same bounded
    /// `screen_rect` `bounded_test_input` uses - see its comment for why
    /// an unbounded default canvas would make a height-based assertion
    /// meaningless. Keyboard shortcuts do not depend on hit-testing
    /// (unlike pointer clicks), so - unlike `simulate_row_click` - a
    /// single frame carrying the event is enough; there is no
    /// previous-frame-rect requirement to work around.
    fn key_press_input(key: egui::Key, modifiers: egui::Modifiers) -> egui::RawInput {
        egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..bounded_test_input()
        }
    }

    #[test]
    fn real_escape_key_clears_the_selected_archives_set() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        harness.selected_archives = [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
            .into_iter()
            .collect();

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::Escape, egui::Modifiers::default()),
        );

        assert!(
            harness.selected_archives.is_empty(),
            "Escape must clear the complete selected_archives set"
        );
    }

    #[test]
    fn real_ctrl_a_selects_only_the_currently_visible_filtered_rows() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "alpha.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "GBA", "Live", "bravo.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "SNES", "Live", "charlie.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        // Simulates the state right after a real filter-changed frame: only
        // rows 0 and 2 (a.zip, c.zip) currently pass the search filter;
        // row 1 (b.zip) is hidden.
        harness.filtered_rows = Some(vec![0usize, 2usize]);

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::A, egui::Modifiers::CTRL),
        );

        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
                .into_iter()
                .collect(),
            "Ctrl+A must select only the archives visible after the current search/filters, \
             never the hidden b.zip"
        );
    }

    #[test]
    fn real_ctrl_a_is_ignored_while_the_search_box_has_keyboard_focus() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();

        // Frame 1: render once so the search TextEdit exists this
        // `Context`, then give it real keyboard focus - exactly the state
        // egui is in immediately after a user clicks into the search box.
        harness.render(&ctx, &data, bounded_test_input());
        ctx.memory_mut(|memory| {
            memory.request_focus(egui::Id::new(SEARCH_FILTER_TEXT_EDIT_ID));
        });

        // Frame 2: Ctrl+A must be left for the focused text field's own
        // "select all text" behaviour, never hijacked into a table
        // selection change.
        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::A, egui::Modifiers::CTRL),
        );

        assert!(
            harness.selected_archives.is_empty(),
            "Ctrl+A must be ignored for table selection while the search box has keyboard focus"
        );
    }

    #[test]
    fn real_ctrl_a_is_ignored_while_the_bulk_platform_combobox_popup_is_open() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        harness.selected_archives = [PathBuf::from("/roms/a.zip")].into_iter().collect();

        // Frame 1: render once (registers everything with this Context).
        harness.render(&ctx, &data, bounded_test_input());

        // Open the exact popup egui's own `ComboBox::from_id_salt(
        // "bulk_platform_choice_combo")` (see `show_bulk_platform_action_bar`)
        // opens when clicked - `ComboBox::widget_to_popup_id` is private,
        // but its formula (the widget id salted with "popup") is exactly
        // reproducible, so this is a faithful simulation of a user having
        // opened the dropdown, not a shortcut around it.
        let popup_id = egui::Id::new("bulk_platform_choice_combo").with("popup");
        egui::Popup::open_id(&ctx, popup_id);

        // If Ctrl+A were not suppressed here, all 3 visible rows would be
        // selected - so an unchanged single-row selection is conclusive.
        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::A, egui::Modifiers::CTRL),
        );

        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/a.zip")].into_iter().collect(),
            "Ctrl+A must be ignored for table selection while a ComboBox popup is open"
        );
    }

    #[test]
    fn real_arrow_navigation_follows_the_visible_sorted_order() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "Z-Platform", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "Y-Platform", "Live", "b.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "X-Platform", "Live", "c.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        // Ascending by platform puts c (X), b (Y), a (Z) in that screen
        // order - arrow navigation must follow *this* order, never the
        // rows' insertion order.
        harness.sort_field = Some(SortField::Platform);
        harness.sort_ascending = true;

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::ArrowDown, egui::Modifiers::default()),
        );
        assert_eq!(harness.selected_archive, Some(PathBuf::from("/roms/c.zip")));
        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/c.zip")].into_iter().collect()
        );

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::ArrowDown, egui::Modifiers::default()),
        );
        assert_eq!(harness.selected_archive, Some(PathBuf::from("/roms/b.zip")));
        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/b.zip")].into_iter().collect(),
            "moving focus without Ctrl must replace the selection with the newly focused row"
        );
    }

    #[test]
    fn real_arrow_navigation_does_not_use_a_stale_index_after_filtering() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        harness.selected_archive = Some(PathBuf::from("/roms/b.zip"));
        // A filter has just excluded b.zip - only a.zip and c.zip (at new
        // positions 0 and 1) remain visible; b.zip's old position no
        // longer means anything.
        harness.filtered_rows = Some(vec![0usize, 2usize]);

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::ArrowDown, egui::Modifiers::default()),
        );

        assert_eq!(
            harness.selected_archive,
            Some(PathBuf::from("/roms/a.zip")),
            "focus must fall back to the first visible row, never use a stale index that \
             would have pointed at whatever now occupies the old visible position 1"
        );
    }

    /// The exact Nobara bug report: multiple rows selected, Ctrl+Up/Down
    /// pressed - the selected count must stay unchanged (correct, and
    /// already worked) while the focused archive (`selected_archive`)
    /// actually moves through the visible order (the part that was
    /// broken - see `focused_row_paints_a_distinct_stroke_from_the_multi_selected_fill`
    /// for why it was invisible even though this underlying state change
    /// itself was already correct before this fix).
    #[test]
    fn real_ctrl_arrow_navigation_preserves_the_multi_selection_and_moves_focus() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "SNES", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "SNES", "Live", "b.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "SNES", "Live", "c.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        let full_selection: HashSet<PathBuf> = [
            PathBuf::from("/roms/a.zip"),
            PathBuf::from("/roms/b.zip"),
            PathBuf::from("/roms/c.zip"),
        ]
        .into_iter()
        .collect();
        harness.selected_archives = full_selection.clone();
        harness.selected_archive = Some(PathBuf::from("/roms/a.zip"));

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::ArrowDown, egui::Modifiers::CTRL),
        );
        assert_eq!(
            harness.selected_archive,
            Some(PathBuf::from("/roms/b.zip")),
            "Ctrl+Down must move the focused archive to the next visible row"
        );
        assert_eq!(
            harness.selected_archives, full_selection,
            "Ctrl+Down must leave every multi-selected row exactly as it was"
        );

        harness.render(
            &ctx,
            &data,
            key_press_input(egui::Key::ArrowUp, egui::Modifiers::CTRL),
        );
        assert_eq!(
            harness.selected_archive,
            Some(PathBuf::from("/roms/a.zip")),
            "Ctrl+Up must move the focused archive to the previous visible row"
        );
        assert_eq!(
            harness.selected_archives, full_selection,
            "Ctrl+Up must also leave every multi-selected row exactly as it was"
        );
    }

    #[test]
    fn real_keyboard_navigation_scrolls_the_newly_focused_row_into_view() {
        let ctx = egui::Context::default();
        // Comfortably more rows than the bounded 250px test viewport
        // (see `bounded_test_input`) can show at once, so moving focus to
        // the last one requires the fix to actually scroll - this cannot
        // pass by coincidence the way a 2-3 row table could.
        let rows: Vec<ArchiveRow> = (0..30)
            .map(|i| {
                row_with_fields(
                    &format!("/roms/{i:02}.zip"),
                    "SNES",
                    "Live",
                    &format!("{i:02}.zip"),
                    &format!("/mnt/{i:02}"),
                )
            })
            .collect();
        let data = loaded_data_with_rows("/mount", rows);
        let mut harness = RealLoadedDataHarness::new();
        harness.selected_archive = Some(PathBuf::from("/roms/00.zip"));

        for _ in 0..29 {
            harness.render(
                &ctx,
                &data,
                key_press_input(egui::Key::ArrowDown, egui::Modifiers::default()),
            );
        }

        assert_eq!(
            harness.selected_archive,
            Some(PathBuf::from("/roms/29.zip")),
            "sanity check: focus must actually have reached the last row"
        );
        assert!(
            harness.library_scroll_offset > 0.0,
            "moving keyboard focus down through 30 rows in a ~250px viewport must have \
             scrolled the table - offset is still {}",
            harness.library_scroll_offset
        );
    }

    #[test]
    fn real_sorting_does_not_change_the_selected_archives_set() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![
                row_with_fields("/roms/a.zip", "Z", "Live", "a.zip", "/mnt/a"),
                row_with_fields("/roms/b.zip", "Y", "Live", "b.zip", "/mnt/b"),
                row_with_fields("/roms/c.zip", "X", "Live", "c.zip", "/mnt/c"),
            ],
        );
        let mut harness = RealLoadedDataHarness::new();
        harness.selected_archives = [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
            .into_iter()
            .collect();

        harness.render(&ctx, &data, bounded_test_input());
        assert_eq!(harness.selected_archives.len(), 2);

        harness.sort_field = Some(SortField::Platform);
        harness.sort_ascending = false;
        harness.render(&ctx, &data, bounded_test_input());

        assert_eq!(
            harness.selected_archives,
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/c.zip")]
                .into_iter()
                .collect(),
            "sorting must never change which exact archives are selected, only the display order"
        );
    }

    #[test]
    fn real_zero_filter_results_message_renders_instead_of_an_empty_table() {
        let ctx = egui::Context::default();
        let data = loaded_data_with_rows(
            "/mount",
            vec![row_with_fields(
                "/roms/a.zip",
                "SNES",
                "Live",
                "a.zip",
                "/mnt/a",
            )],
        );

        let mut visible = RealLoadedDataHarness::new();
        let visible_height = visible.render(&ctx, &data, bounded_test_input());

        let mut hidden = RealLoadedDataHarness::new();
        hidden.filtered_rows = Some(Vec::new());
        let hidden_height = hidden.render(&ctx, &data, bounded_test_input());

        assert!(
            hidden_height < visible_height,
            "when every row is filtered out, the header/table must not render at all (a short \
             message instead) - got hidden={hidden_height} vs visible={visible_height}"
        );
    }

    #[test]
    fn real_empty_library_message_renders_instead_of_an_empty_table() {
        let ctx = egui::Context::default();
        let populated = loaded_data_with_rows(
            "/mount",
            vec![row_with_fields(
                "/roms/a.zip",
                "SNES",
                "Live",
                "a.zip",
                "/mnt/a",
            )],
        );
        let empty = empty_loaded_data("/mount");

        let mut populated_harness = RealLoadedDataHarness::new();
        let populated_height = populated_harness.render(&ctx, &populated, bounded_test_input());

        let mut empty_harness = RealLoadedDataHarness::new();
        let empty_height = empty_harness.render(&ctx, &empty, bounded_test_input());

        assert!(
            empty_height < populated_height,
            "an empty library must never render the (empty) table - got empty={empty_height} \
             vs populated={populated_height}"
        );
    }

    #[test]
    fn prune_selection_uses_the_full_catalogue_not_the_filtered_view() {
        // A selected archive a text filter would currently hide must
        // still count as "in the loaded catalogue" - filtering must never
        // silently deselect a row, only change what is visible.
        let mut app = app_for_operation_tests();
        let path_a = PathBuf::from("/roms/a.zip");
        let path_b = PathBuf::from("/roms/b.zip");
        app.selected_archives = [path_a.clone(), path_b.clone()].into_iter().collect();
        let record_a = record_at(path_a, MountState::Pending);
        let record_b = record_at(path_b, MountState::Pending);
        let rows = vec![row_for(&record_a), row_for(&record_b)];

        app.prune_selection(&rows);

        assert_eq!(
            app.selected_archives.len(),
            2,
            "both selected rows are still in the catalogue"
        );
    }

    #[test]
    fn prune_selection_removes_a_vanished_selection_and_clears_the_focused_row() {
        let mut app = app_for_operation_tests();
        let still_present = PathBuf::from("/roms/a.zip");
        let vanished = PathBuf::from("/roms/b.zip");
        app.selected_archives = [still_present.clone(), vanished.clone()]
            .into_iter()
            .collect();
        app.selected_archive = Some(vanished.clone());
        let record = record_at(still_present.clone(), MountState::Pending);
        let rows = vec![row_for(&record)];

        app.prune_selection(&rows);

        assert_eq!(
            app.selected_archives,
            [still_present].into_iter().collect::<HashSet<_>>()
        );
        assert_eq!(
            app.selected_archive, None,
            "the focused row must be cleared once it no longer exists in the catalogue"
        );
    }

    #[test]
    fn bulk_platform_action_available_requires_no_running_action_or_database_load() {
        let mut app = app_for_operation_tests();
        assert!(app.bulk_platform_action_available());

        let (_sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Clear,
            requested_paths: 2,
            receiver,
        });
        assert!(!app.bulk_platform_action_available());
        app.bulk_platform_action = None;

        let (_sender, receiver) = mpsc::channel();
        app.database_state = DatabaseState::Loading {
            generation: DatabaseGeneration::INITIAL,
            receiver,
            previous: None,
            scanning: false,
        };
        assert!(!app.bulk_platform_action_available());
    }

    #[test]
    fn single_and_bulk_platform_actions_are_mutually_exclusive() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.platform_action = Some(RunningPlatformAction {
            archive_path: PathBuf::from("/roms/a.zip"),
            receiver,
        });

        assert!(
            !app.bulk_platform_action_available(),
            "a running single-row platform action must block a new bulk one"
        );

        app.platform_action = None;
        let (_sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Clear,
            requested_paths: 2,
            receiver,
        });

        assert!(
            !app.platform_action_available(),
            "a running bulk platform action must block a new single-row one"
        );
    }

    #[test]
    fn bulk_platform_action_never_affects_is_busy_or_mount_availability() {
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Set("GameCube".to_string()),
            requested_paths: 3,
            receiver,
        });

        assert!(
            !app.is_busy(),
            "bulk platform assignment is metadata-only and must never enter the mount busy state"
        );
    }

    #[test]
    fn bulk_platform_action_does_not_block_on_a_slow_background_worker() {
        // Mirrors scan_library_action_does_not_block_on_a_slow_background_worker:
        // never calls the real start_bulk_platform_action/apply_bulk_platform_action
        // (which would touch the real default database path) - drives the
        // same Loading-equivalent state by hand and proves poll_bulk_platform_action's
        // use of try_recv (not recv) never blocks the UI thread.
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Clear,
            requested_paths: 5,
            receiver,
        });

        app.poll_bulk_platform_action(&egui::Context::default());

        assert!(app.bulk_platform_action.is_some());
    }

    #[test]
    fn poll_bulk_platform_action_success_refreshes_the_database_cache_asynchronously() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Set("GameCube".to_string()),
            requested_paths: 3,
            receiver,
        });
        sender
            .send(Ok(BulkPlatformActionOutcome {
                result: BulkPlatformAssignmentResult {
                    requested: 3,
                    changed: 2,
                    unchanged: 1,
                    missing: Vec::new(),
                },
                unresolved_paths: 0,
            }))
            .unwrap();

        app.poll_bulk_platform_action(&egui::Context::default());

        assert!(app.bulk_platform_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(feedback.succeeded);
        assert!(feedback.message.contains("GameCube"));
        assert!(
            feedback.message.contains('2'),
            "must mention the changed count"
        );
        assert!(app.history.entries().any(|entry| entry.action
            == ActivityAction::BulkPlatformAssignment
            && entry.outcome == ActivityOutcome::Completed));
        // Refreshing the cache is asynchronous - poll_bulk_platform_action
        // only starts a new background database load, it does not block
        // waiting for it, and the live snapshot is untouched.
        assert!(app.database_state.is_loading());
        assert!(matches!(app.state, LoadState::Ready(_)));
    }

    #[test]
    fn poll_bulk_platform_action_failure_preserves_the_cached_row_and_selection() {
        let mut app = app_for_operation_tests();
        let stale_snapshot = cached_snapshot(vec![persisted_archive_with_platform(
            PathBuf::from("/roms/a.zip"),
            1,
            "N64",
            "folder_alias",
        )]);
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(stale_snapshot.clone()),
            last_scan_summary: None,
        };
        let selected: HashSet<PathBuf> =
            [PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
                .into_iter()
                .collect();
        app.selected_archives = selected.clone();
        let (sender, receiver) = mpsc::channel();
        app.bulk_platform_action = Some(RunningBulkPlatformAction {
            kind: BulkPlatformActionKind::Set("GameCube".to_string()),
            requested_paths: 2,
            receiver,
        });
        sender.send(Err("database is locked".to_string())).unwrap();

        app.poll_bulk_platform_action(&egui::Context::default());

        assert!(app.bulk_platform_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("database is locked"));
        assert!(app.history.entries().any(|entry| entry.action
            == ActivityAction::BulkPlatformAssignment
            && entry.outcome == ActivityOutcome::Failed));
        // Requirement 8: a failed bulk action must preserve both the
        // prior cached rows and the selection exactly as they were.
        match &app.database_state {
            DatabaseState::Ready { snapshot, .. } => {
                assert_eq!(snapshot.archives, stale_snapshot.archives);
                assert_eq!(snapshot.database_path, stale_snapshot.database_path);
            }
            other => panic!(
                "expected the cached snapshot to survive untouched, got status {}",
                other.status_label()
            ),
        }
        assert_eq!(app.selected_archives, selected);
    }

    #[test]
    fn apply_bulk_platform_action_at_sets_platform_for_every_selected_archive() {
        let dir = database_test_dir("apply-bulk-set");
        let source = dir.join("source");
        let mount = dir.join("mount");
        let path_a = write_archive_file(&source, "a.zip", b"a");
        let path_b = write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }

        let outcome = apply_bulk_platform_action_at(
            &database_path,
            &[path_a, path_b],
            &BulkPlatformActionKind::Set("GameCube".to_string()),
        )
        .unwrap();

        assert_eq!(outcome.result.requested, 2);
        assert_eq!(outcome.result.changed, 2);
        assert_eq!(outcome.unresolved_paths, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_bulk_platform_action_at_reports_unresolved_paths_separately_from_missing_ids() {
        let dir = database_test_dir("apply-bulk-unresolved");
        let source = dir.join("source");
        let mount = dir.join("mount");
        let path_a = write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }
        // A live-only row never scanned into the database at all - a
        // fundamentally different situation from a stale archive id, and
        // must be reported separately (unresolved_paths), never silently
        // treated as if it were a "missing" database id.
        let never_scanned = source.join("never-scanned.zip");

        let outcome = apply_bulk_platform_action_at(
            &database_path,
            &[path_a, never_scanned],
            &BulkPlatformActionKind::Set("GameCube".to_string()),
        )
        .unwrap();

        assert_eq!(outcome.result.requested, 1);
        assert_eq!(outcome.result.changed, 1);
        assert!(outcome.result.missing.is_empty());
        assert_eq!(outcome.unresolved_paths, 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_bulk_platform_action_at_clears_and_restores_fallback() {
        let dir = database_test_dir("apply-bulk-clear");
        let source = dir.join("source");
        let mount = dir.join("mount");
        let path_a = write_archive_file(&source, "msx2/game.zip", b"a");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }
        apply_bulk_platform_action_at(
            &database_path,
            std::slice::from_ref(&path_a),
            &BulkPlatformActionKind::Set("GameCube".to_string()),
        )
        .unwrap();

        let outcome = apply_bulk_platform_action_at(
            &database_path,
            &[path_a],
            &BulkPlatformActionKind::Clear,
        )
        .unwrap();

        assert_eq!(outcome.result.changed, 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn apply_bulk_platform_action_at_works_for_non_utf8_archive_paths_on_unix() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = database_test_dir("apply-bulk-non-utf8");
        let source = dir.join("source");
        let mount = dir.join("mount");
        std::fs::create_dir_all(&source).unwrap();
        let mut invalid_name = b"fo".to_vec();
        invalid_name.push(0x80);
        invalid_name.extend_from_slice(b"o.zip");
        let archive_path = source.join(OsString::from_vec(invalid_name));
        assert!(
            archive_path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );
        std::fs::write(&archive_path, b"contents").unwrap();
        let other_path = write_archive_file(&source, "other.zip", b"contents");
        let config = config_for(&source, &mount);
        let database_path = dir.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            scan_and_persist(&mut database, &config, "test").unwrap();
        }

        let outcome = apply_bulk_platform_action_at(
            &database_path,
            &[archive_path, other_path],
            &BulkPlatformActionKind::Set("GameCube".to_string()),
        )
        .unwrap();

        assert_eq!(
            outcome.result.changed, 2,
            "a non-UTF-8 archive path must resolve to its exact archive, not be silently dropped"
        );
        assert_eq!(outcome.unresolved_paths, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // -------------------------------------------------------------
    // Custom Platform Aliases panel
    // -------------------------------------------------------------

    #[test]
    fn alias_action_available_requires_no_running_action_or_database_load() {
        let mut app = app_for_operation_tests();
        assert!(app.alias_action_available());

        let (_sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Remove {
                alias: "gc".to_string(),
            },
            receiver,
        });
        assert!(!app.alias_action_available());
        app.alias_action = None;

        let (_sender, receiver) = mpsc::channel();
        app.database_state = DatabaseState::Loading {
            generation: DatabaseGeneration::INITIAL,
            receiver,
            previous: None,
            scanning: false,
        };
        assert!(!app.alias_action_available());
    }

    #[test]
    fn start_alias_action_does_not_start_a_second_concurrent_action() {
        let mut app = app_for_operation_tests();
        app.start_alias_action(
            egui::Context::default(),
            AliasAction::Add {
                alias: "gc".to_string(),
                platform: "GameCube".to_string(),
            },
        );
        assert!(app.alias_action.is_some());
        let first_action = app.alias_action.as_ref().unwrap().action.clone();

        // A second alias action must not replace the first one's receiver
        // - mirrors start_operation_rejects_a_second_operation_without_replacing_the_receiver's
        // existing convention for the archive-action channel.
        app.start_alias_action(
            egui::Context::default(),
            AliasAction::Remove {
                alias: "wii".to_string(),
            },
        );
        assert_eq!(app.alias_action.as_ref().unwrap().action, first_action);
    }

    #[test]
    fn poll_alias_action_add_success_refreshes_the_cache_and_clears_the_input_fields() {
        let mut app = app_for_operation_tests();
        app.new_alias_text = "gc".to_string();
        app.new_alias_platform_choice = Some("GameCube".to_string());
        let (sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Add {
                alias: "gc".to_string(),
                platform: "GameCube".to_string(),
            },
            receiver,
        });
        sender.send(Ok(())).unwrap();

        app.poll_alias_action(&egui::Context::default());

        assert!(app.alias_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(feedback.succeeded);
        assert!(feedback.message.contains("gc"));
        assert!(feedback.message.contains("GameCube"));
        assert!(feedback.message.contains("Run a library scan"));
        assert!(
            app.history
                .entries()
                .any(|entry| entry.outcome == ActivityOutcome::Completed
                    && entry.action == ActivityAction::PlatformAliasManagement)
        );
        assert!(app.new_alias_text.is_empty());
        assert!(app.new_alias_platform_choice.is_none());
        // Asynchronous: only a new background database load is started,
        // never blocked on, and the live snapshot is untouched.
        assert!(app.database_state.is_loading());
        assert!(matches!(app.state, LoadState::Ready(_)));
    }

    #[test]
    fn poll_alias_action_remove_success_refreshes_the_cache() {
        let mut app = app_for_operation_tests();
        let (sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Remove {
                alias: "gc".to_string(),
            },
            receiver,
        });
        sender.send(Ok(())).unwrap();

        app.poll_alias_action(&egui::Context::default());

        assert!(app.alias_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(feedback.succeeded);
        assert!(feedback.message.contains("gc"));
        assert!(app.database_state.is_loading());
    }

    #[test]
    fn poll_alias_action_failure_preserves_the_cached_aliases_and_shows_the_error() {
        let mut app = app_for_operation_tests();
        let stale_snapshot = cached_snapshot(Vec::new());
        let mut stale_snapshot = stale_snapshot;
        stale_snapshot.platform_aliases = vec![PlatformAlias {
            id: 1,
            alias: "gc".to_string(),
            normalized_alias: "gc".to_string(),
            platform: "GameCube".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];
        app.database_state = DatabaseState::Ready {
            snapshot: Box::new(stale_snapshot.clone()),
            last_scan_summary: None,
        };
        app.new_alias_text = "wii".to_string();
        app.new_alias_platform_choice = Some("Wii".to_string());
        let (sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Add {
                alias: "wii".to_string(),
                platform: "Wii".to_string(),
            },
            receiver,
        });
        sender
            .send(Err("a platform alias for 'wii' already exists".to_string()))
            .unwrap();

        app.poll_alias_action(&egui::Context::default());

        assert!(app.alias_action.is_none());
        let feedback = app.feedback.as_ref().unwrap();
        assert!(!feedback.succeeded);
        assert!(feedback.message.contains("already exists"));
        assert!(
            app.history
                .entries()
                .any(|entry| entry.outcome == ActivityOutcome::Failed
                    && entry.action == ActivityAction::PlatformAliasManagement)
        );
        // A failed add must not clear the input fields (the user should
        // be able to see/correct what they typed) and must not touch the
        // cached snapshot or trigger a database reload.
        assert_eq!(app.new_alias_text, "wii");
        assert_eq!(app.new_alias_platform_choice, Some("Wii".to_string()));
        match &app.database_state {
            DatabaseState::Ready { snapshot, .. } => {
                assert_eq!(snapshot.platform_aliases, stale_snapshot.platform_aliases);
            }
            other => panic!(
                "expected the stale Ready snapshot to survive untouched, got status {}",
                other.status_label()
            ),
        }
    }

    #[test]
    fn alias_action_is_independent_of_is_busy_and_mount_action_availability() {
        // Alias management is metadata-only, exactly like per-archive
        // platform assignment (platform_action): it must never appear in
        // is_busy() (which gates mount/unmount exclusivity), and a
        // running alias action must not disable mount/unmount actions.
        let mut app = app_for_operation_tests();
        let (_sender, receiver) = mpsc::channel();
        app.alias_action = Some(RunningAliasAction {
            action: AliasAction::Remove {
                alias: "gc".to_string(),
            },
            receiver,
        });
        assert!(!app.is_busy());
    }

    #[test]
    fn new_alias_action_uses_the_chosen_canonical_platform() {
        for platform in canonical_platform_names() {
            let action = resolved_new_alias_action("gc", Some(platform)).unwrap();
            assert_eq!(
                action,
                AliasAction::Add {
                    alias: "gc".to_string(),
                    platform: platform.to_string(),
                }
            );
        }
    }

    #[test]
    fn resolved_new_alias_action_requires_a_non_empty_alias_and_a_chosen_platform() {
        assert!(resolved_new_alias_action("gc", None).is_none());
        assert!(resolved_new_alias_action("   ", Some("GameCube")).is_none());
        assert!(resolved_new_alias_action("", Some("GameCube")).is_none());
        assert_eq!(
            resolved_new_alias_action("  gc  ", Some("GameCube")),
            Some(AliasAction::Add {
                alias: "gc".to_string(),
                platform: "GameCube".to_string(),
            })
        );
    }

    #[test]
    fn apply_alias_action_add_list_remove_round_trip_and_duplicate_error() {
        let dir = database_test_dir("apply-alias-round-trip");
        let database_path = dir.join("library.sqlite3");

        apply_alias_action_at(
            &database_path,
            &AliasAction::Add {
                alias: "gc".to_string(),
                platform: "GameCube".to_string(),
            },
        )
        .unwrap();

        let duplicate_error = apply_alias_action_at(
            &database_path,
            &AliasAction::Add {
                alias: "GC".to_string(),
                platform: "Wii".to_string(),
            },
        )
        .unwrap_err();
        assert!(duplicate_error.to_string().contains("already exists"));

        let database = Database::open_or_create(&database_path).unwrap();
        assert_eq!(database.list_platform_aliases().unwrap().len(), 1);
        drop(database);

        apply_alias_action_at(
            &database_path,
            &AliasAction::Remove {
                alias: "gc".to_string(),
            },
        )
        .unwrap();
        let database = Database::open_or_create(&database_path).unwrap();
        assert!(database.list_platform_aliases().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_alias_action_remove_unknown_alias_is_a_clear_error() {
        let dir = database_test_dir("apply-alias-remove-unknown");
        let database_path = dir.join("library.sqlite3");
        Database::open_or_create(&database_path).unwrap();

        let error = apply_alias_action_at(
            &database_path,
            &AliasAction::Remove {
                alias: "does-not-exist".to_string(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("no platform alias matches"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
