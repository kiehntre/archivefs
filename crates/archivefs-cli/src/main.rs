use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use archivefs_core::{
    ArchiveIndex, ArchiveIndexEntry, ArchiveIndexFreshness, ArchiveIndexSummary, ArchiveInfo,
    ArchiveScanner, ArchiveStats, ArchiveStatus, CatalogueStats, CompletedScanSummary, Config,
    ConfigCheckReport, ConfigCheckStatus, Database, DatabaseHealth, DoctorReport,
    DuplicateDetector, DuplicateEntry, DuplicateReport, FilenameDuplicateDetector, MountPlan,
    PersistedArchive, ScanPersistSummary, WatchRebuildSummary, build_and_write_archive_index,
    check_archive_index_freshness, check_database_health, clean_mount_root,
    cleanup_selected_mount_dir, current_archive_info, current_archive_stats, current_statuses,
    default_database_path, default_index_path, find_archive_index_entries, latest_schema_version,
    mount_archives, mount_one_archive, read_default_archive_index, run_config_check_default,
    run_doctor_default, scan_and_persist, summarize_archive_index, unmount_archives,
    unmount_one_archive, watch_archive_index,
};
use serde::Serialize;

static LOGGER: StderrLogger = StderrLogger;
static LOGGER_INIT: OnceLock<()> = OnceLock::new();

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    log_level: log::LevelFilter,
    command: String,
    args: Vec<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("archivefs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(level: log::LevelFilter) {
    LOGGER_INIT.get_or_init(|| {
        let _ = log::set_logger(&LOGGER);
    });
    log::set_max_level(level);
}

fn parse_cli_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut log_level = log::LevelFilter::Off;
    let mut rest = args.into_iter().collect::<Vec<_>>();

    while let Some(flag) = rest.first() {
        match flag.as_str() {
            "--debug" => {
                log_level = log::LevelFilter::Debug;
                rest.remove(0);
            }
            "--verbose" | "-v" => {
                if log_level < log::LevelFilter::Info {
                    log_level = log::LevelFilter::Info;
                }
                rest.remove(0);
            }
            _ => break,
        }
    }

    let command = if rest.is_empty() {
        "help".to_string()
    } else {
        rest.remove(0)
    };

    CliArgs {
        log_level,
        command,
        args: rest,
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli_args(env::args().skip(1));
    init_logging(cli.log_level);
    let command = cli.command;
    let mut args = cli.args.into_iter();

    match command.as_str() {
        "scan" => {
            let config = Config::load_default()?;
            let scanner = ArchiveScanner::new(&config);
            for archive in scanner.scan_archives()? {
                println!("{}", archive.path.display());
            }
        }
        "mount" => {
            let config = Config::load_default()?;
            print_statuses(&mount_archives(&config)?);
        }
        "mount-one" => {
            let Some(first) = args.next() else {
                return Err("mount-one requires an archive path or name".into());
            };
            let input = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let config = Config::load_default()?;
            print_mount_one(&mount_one_archive(&config, &input)?);
            warn_if_index_refresh_failed(&config);
        }
        "unmount" => {
            let config = Config::load_default()?;
            print_statuses(&unmount_archives(&config)?);
        }
        "unmount-one" => {
            let Some(first) = args.next() else {
                return Err("unmount-one requires an archive path or name".into());
            };
            let input = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let config = Config::load_default()?;
            let plan = unmount_one_archive(&config, &input)?;
            print_unmount_one(&plan);
            warn_if_mount_dir_cleanup_failed(&config, &plan);
            warn_if_index_refresh_failed(&config);
        }
        "status" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let statuses = current_statuses(&config)?;
            if json {
                print_statuses_json(&statuses)?;
            } else {
                print_statuses(&statuses);
            }
        }
        "stats" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let stats = current_archive_stats(&config)?;
            if json {
                print_archive_stats_json(&stats)?;
            } else {
                print_archive_stats(&stats);
            }
        }
        "duplicates" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let scanner = ArchiveScanner::new(&config);
            let records = scanner.archive_records()?;
            let detector = FilenameDuplicateDetector;
            let report = detector.detect_duplicates(&records)?;
            if json {
                print_duplicate_report_json(&report)?;
            } else {
                print_duplicate_report(&report);
            }
        }
        "info" => {
            let Some(first) = args.next() else {
                return Err("info requires an archive path or name".into());
            };
            let mut input_args = std::iter::once(first).chain(args).collect::<Vec<_>>();
            let json = input_args.last().is_some_and(|arg| arg == "--json");
            if json {
                input_args.pop();
            }
            let input = input_args.join(" ");
            if input.is_empty() {
                return Err("info requires an archive path or name".into());
            }
            let config = Config::load_default()?;
            let info = current_archive_info(&config, &input)?;
            if json {
                print_archive_info_json(&info)?;
            } else {
                print_archive_info(&info);
            }
        }
        "doctor" => {
            let json = args.any(|arg| arg == "--json");
            let report = run_doctor_default();
            if json {
                print_doctor_report_json(&report)?;
            } else {
                print_doctor_report(&report);
            }
        }
        "config-check" => {
            print_config_check_report(&run_config_check_default());
        }
        "index-build" => {
            let config = Config::load_default()?;
            let index = build_and_write_archive_index(&config)?;
            println!(
                "Wrote index: {} ({} archives)",
                default_index_path()?.display(),
                index.archives.len()
            );
        }
        "index-show" => {
            let Some(index) = read_index_or_print_build_hint()? else {
                return Ok(());
            };
            print_index_warnings(&check_archive_index_freshness(&index));
            print_index_summary(&summarize_archive_index(&index));
        }
        "index-find" => {
            let Some(first) = args.next() else {
                return Err("index-find requires a query".into());
            };
            let query = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let Some(index) = read_index_or_print_build_hint()? else {
                return Ok(());
            };
            print_index_warnings(&check_archive_index_freshness(&index));
            print_index_find_results(&query, &find_archive_index_entries(&index, &query));
        }
        "library-status" => {
            let json = args.any(|arg| arg == "--json");
            let view = build_library_status_view(&default_database_path()?);
            if json {
                print_library_status_json(&view)?;
            } else {
                print_library_status(&view);
            }
        }
        "library-scan" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let database_path = default_database_path()?;
            let report = run_library_scan(&config, &database_path, "cli-library-scan")?;
            if json {
                print_library_scan_json(&report)?;
            } else {
                print_library_scan(&report);
            }
        }
        "library-list" => {
            let json = args.any(|arg| arg == "--json");
            let database_path = default_database_path()?;
            let entries = build_library_entries(&database_path)?;
            if json {
                print_library_entries_json(&entries)?;
            } else {
                print_library_entries(&database_path, &entries);
            }
        }
        "library-find" => {
            let Some(first) = args.next() else {
                return Err("library-find requires a query".into());
            };
            let mut input_args = std::iter::once(first).chain(args).collect::<Vec<_>>();
            let json = input_args.last().is_some_and(|arg| arg == "--json");
            if json {
                input_args.pop();
            }
            let query = input_args.join(" ");
            if query.is_empty() {
                return Err("library-find requires a query".into());
            }
            let database_path = default_database_path()?;
            let entries = build_library_entries(&database_path)?;
            let matches = filter_library_entries(&entries, &query);
            if json {
                print_library_entries_json(&matches)?;
            } else {
                print_library_find_results(&query, &matches);
            }
        }
        "clean" => {
            let config = Config::load_default()?;
            print_cleaned_dirs(&clean_mount_root(&config)?);
        }
        "watch" => {
            let config = Config::load_default()?;
            watch_archive_index(
                &config,
                || println!("Watching configured source folders for archive changes."),
                print_watch_rebuild,
            )?;
        }
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
}

fn print_config_check_report(report: &ConfigCheckReport) {
    println!("ArchiveFS Config Check");
    println!("Config: {}", report.config_path.display());
    println!();
    println!("Checks:");
    for check in &report.checks {
        println!(
            "  [{:<5}] {:<28} {}",
            check.status, check.name, check.detail
        );
    }

    let warnings = report
        .checks
        .iter()
        .filter(|check| check.status == ConfigCheckStatus::Warn)
        .collect::<Vec<_>>();
    println!();
    println!("Warnings:");
    if warnings.is_empty() {
        println!("  none");
    } else {
        for warning in warnings {
            println!("  {}: {}", warning.name, warning.detail);
        }
    }

    let errors = report
        .checks
        .iter()
        .filter(|check| check.status == ConfigCheckStatus::Error)
        .collect::<Vec<_>>();
    println!();
    println!("Errors:");
    if errors.is_empty() {
        println!("  none");
    } else {
        for error in errors {
            println!("  {}: {}", error.name, error.detail);
        }
    }

    println!();
    println!("Summary:");
    println!("  Errors: {}", report.error_count());
    println!("  Warnings: {}", report.warning_count());
    println!(
        "  Status: {}",
        if report.is_ok() {
            "OK"
        } else {
            "Needs attention"
        }
    );
}

fn print_watch_rebuild(index: &ArchiveIndex, summary: &WatchRebuildSummary) {
    let event_word = if summary.archive_event_count == 1 {
        "event"
    } else {
        "events"
    };
    println!(
        "Rebuilt index ({} archives) after {} archive {}:",
        index.archives.len(),
        summary.archive_event_count,
        event_word
    );
    for path in &summary.changed_paths {
        println!("  {}", path.display());
    }
}

fn warn_if_mount_dir_cleanup_failed(config: &Config, plan: &MountPlan) {
    if let Err(error) = cleanup_selected_mount_dir(config, &plan.mount_path) {
        eprintln!(
            "Warning: unmounted {}, but mount directory cleanup failed: {error}",
            plan.mount_path.display()
        );
    }
}

fn warn_if_index_refresh_failed(config: &Config) {
    if let Err(error) = build_and_write_archive_index(config) {
        eprintln!("Warning: mounted state changed, but index refresh failed: {error}");
    }
}

fn read_index_or_print_build_hint() -> Result<Option<ArchiveIndex>, Box<dyn std::error::Error>> {
    let index_path = default_index_path()?;
    if !Path::new(&index_path).exists() {
        println!(
            "No archive index found at {}. Run: archivefs index-build",
            index_path.display()
        );
        return Ok(None);
    }
    Ok(Some(read_default_archive_index()?))
}

fn print_index_warnings(freshness: &ArchiveIndexFreshness) {
    if !freshness.missing_archive_paths.is_empty() {
        println!("Warning: index contains missing archive paths. Run archivefs index-build.");
    }
    if !freshness.stale_archive_paths.is_empty() {
        println!("Warning: index may be stale. Run archivefs index-build.");
    }
}

// ---------------------------------------------------------------------
// Library database commands (library-status, library-scan, library-list,
// library-find). These read/write the persistent SQLite catalogue
// (archivefs_core::Database) - a separate store from the JSON index above.
// They never touch mount or unmount behavior, and index-build/index-show/
// index-find are unchanged and unaffected by any of this.
// ---------------------------------------------------------------------

/// Combined status view for `library-status`. Built from
/// [`check_database_health`] plus, only when the schema is already
/// current, [`Database::catalogue_stats`] and
/// [`Database::latest_completed_scan`] - a status check never triggers a
/// migration itself.
#[derive(Debug, Clone, Serialize)]
struct LibraryStatusView {
    #[serde(flatten)]
    health: DatabaseHealth,
    latest_known_schema_version: i64,
    stats: Option<CatalogueStats>,
    last_completed_scan: Option<CompletedScanSummary>,
}

fn build_library_status_view(database_path: &Path) -> LibraryStatusView {
    let health = check_database_health(database_path);
    let (stats, last_completed_scan) = if health.database_opens && health.migrations_current {
        match Database::open_or_create(database_path) {
            Ok(database) => (
                database.catalogue_stats().ok(),
                database.latest_completed_scan().ok().flatten(),
            ),
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    LibraryStatusView {
        health,
        latest_known_schema_version: latest_schema_version(),
        stats,
        last_completed_scan,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_library_status(view: &LibraryStatusView) {
    print!("{}", format_library_status(view));
}

fn print_library_status_json(view: &LibraryStatusView) -> Result<(), serde_json::Error> {
    println!("{}", format_library_status_json(view)?);
    Ok(())
}

fn format_library_status_json(view: &LibraryStatusView) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(view)
}

fn format_library_status(view: &LibraryStatusView) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Library Status\n\n");
    output.push_str(&format!(
        "Database: {}\n",
        view.health.resolved_path.display()
    ));
    output.push_str(&format!(
        "  Exists: {}\n",
        yes_no(view.health.database_exists)
    ));

    if !view.health.database_exists {
        output.push_str("\nNo library database yet. Run: archivefs-cli library-scan\n");
        return output;
    }

    output.push_str(&format!(
        "  Opens: {}\n",
        yes_no(view.health.database_opens)
    ));
    if let Some(error) = &view.health.error {
        output.push_str(&format!("  Error: {error}\n"));
    }

    if !view.health.database_opens {
        output.push_str(
            "\nThe database file exists but could not be opened. It is always safe to \
             delete it and run archivefs-cli library-scan to rebuild it from your \
             configured source folders.\n",
        );
        return output;
    }

    output.push_str(&format!(
        "  Schema version: {}\n",
        view.health
            .schema_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!(
        "  Migrations current: {}\n",
        yes_no(view.health.migrations_current)
    ));
    output.push_str(&format!(
        "  Foreign keys enabled: {}\n",
        yes_no(view.health.foreign_keys_enabled)
    ));

    if !view.health.migrations_current {
        if let Some(schema_version) = view.health.schema_version {
            if schema_version > view.latest_known_schema_version {
                output.push_str(&format!(
                    "\nThis database's schema (version {schema_version}) is newer than this \
                     build of ArchiveFS supports (version {}). Upgrade ArchiveFS, or remove \
                     the database file to rebuild it with this version.\n",
                    view.latest_known_schema_version
                ));
            } else {
                output.push_str(
                    "\nThis database's schema is outdated. Run: archivefs-cli library-scan \
                     to upgrade it.\n",
                );
            }
        }
        return output;
    }

    output.push_str("\nArchive counts:\n");
    match &view.stats {
        Some(stats) => {
            output.push_str(&format!("  Total: {}\n", stats.total_archives));
            output.push_str(&format!("  Present: {}\n", stats.present_archives));
            output.push_str(&format!("  Missing: {}\n", stats.missing_archives));
            output.push_str(&format!(
                "  Detected platform: {}\n",
                stats.archives_with_platform
            ));
            output.push_str(&format!(
                "  Unknown platform: {}\n",
                stats.archives_unknown_platform
            ));
        }
        None => output.push_str("  unavailable\n"),
    }

    output.push_str("\nLast completed scan:\n");
    match &view.last_completed_scan {
        Some(scan) => {
            output.push_str(&format!("  Started: {}\n", scan.started_at));
            output.push_str(&format!(
                "  Finished: {}\n",
                scan.finished_at.as_deref().unwrap_or("unknown")
            ));
            output.push_str(&format!("  Triggered by: {}\n", scan.triggered_by));
            output.push_str(&format!(
                "  Source folders scanned: {}\n",
                scan.source_folders_scanned
            ));
            output.push_str(&format!("  Archives seen: {}\n", scan.archives_seen));
            output.push_str(&format!("  Archives added: {}\n", scan.archives_added));
            output.push_str(&format!("  Archives updated: {}\n", scan.archives_updated));
            output.push_str(&format!("  Archives missing: {}\n", scan.archives_missing));
            output.push_str(&format!("  Errors: {}\n", scan.errors_count));
            if let Some(message) = &scan.error_message {
                output.push_str(&format!("  Error details: {message}\n"));
            }
        }
        None => output.push_str("  none yet - run: archivefs-cli library-scan\n"),
    }

    output
}

/// A `library-scan` result, reshaped from [`ScanPersistSummary`] into
/// names that read clearly on their own (`source_folders_attempted` etc.)
/// rather than requiring the reader to know this crate's internal
/// `ScanRunCounts` field names.
#[derive(Debug, Clone, Serialize)]
struct LibraryScanReport {
    scan_run_id: i64,
    source_folders_attempted: i64,
    source_folders_succeeded: i64,
    source_folders_failed: i64,
    archives_new: i64,
    archives_changed: i64,
    archives_restored: i64,
    archives_unchanged: i64,
    archives_missing: i64,
    folder_errors: Vec<FolderErrorView>,
}

#[derive(Debug, Clone, Serialize)]
struct FolderErrorView {
    path: PathBuf,
    error: String,
}

impl From<&ScanPersistSummary> for LibraryScanReport {
    fn from(summary: &ScanPersistSummary) -> Self {
        let succeeded = summary.counts.source_folders_scanned;
        let failed = summary.folder_errors.len() as i64;
        Self {
            scan_run_id: summary.scan_run_id,
            source_folders_attempted: succeeded + failed,
            source_folders_succeeded: succeeded,
            source_folders_failed: failed,
            archives_new: summary.counts.archives_added,
            archives_changed: summary.counts.archives_changed,
            archives_restored: summary.counts.archives_restored,
            archives_unchanged: summary.counts.archives_unchanged,
            archives_missing: summary.counts.archives_missing,
            folder_errors: summary
                .folder_errors
                .iter()
                .map(|(path, error)| FolderErrorView {
                    path: path.clone(),
                    error: error.clone(),
                })
                .collect(),
        }
    }
}

/// Opens (creating if needed) the database at `database_path`, runs
/// [`scan_and_persist`] against `config`, and reshapes the result. A
/// database or config problem propagates as `Err` (a non-zero exit code
/// from `main`); one or more failed source folders within an otherwise
/// successful run does not - it shows up in the returned report's
/// `folder_errors` instead. See `docs/DATABASE_DESIGN.md` section 5: this
/// never touches mount or unmount state.
fn run_library_scan(
    config: &Config,
    database_path: &Path,
    triggered_by: &str,
) -> Result<LibraryScanReport, Box<dyn std::error::Error>> {
    let mut database = Database::open_or_create(database_path)?;
    let summary = scan_and_persist(&mut database, config, triggered_by)?;
    Ok(LibraryScanReport::from(&summary))
}

fn print_library_scan(report: &LibraryScanReport) {
    print!("{}", format_library_scan(report));
}

fn print_library_scan_json(report: &LibraryScanReport) -> Result<(), serde_json::Error> {
    println!("{}", format_library_scan_json(report)?);
    Ok(())
}

fn format_library_scan_json(report: &LibraryScanReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_library_scan(report: &LibraryScanReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Library Scan\n\n");
    output.push_str("Source folders:\n");
    output.push_str(&format!(
        "  Attempted: {}\n",
        report.source_folders_attempted
    ));
    output.push_str(&format!(
        "  Succeeded: {}\n",
        report.source_folders_succeeded
    ));
    output.push_str(&format!("  Failed: {}\n", report.source_folders_failed));
    output.push_str("\nArchives:\n");
    output.push_str(&format!("  New: {}\n", report.archives_new));
    output.push_str(&format!("  Changed: {}\n", report.archives_changed));
    output.push_str(&format!("  Restored: {}\n", report.archives_restored));
    output.push_str(&format!("  Unchanged: {}\n", report.archives_unchanged));
    output.push_str(&format!("  Missing: {}\n", report.archives_missing));
    output.push_str("\nErrors:\n");
    if report.folder_errors.is_empty() {
        output.push_str("  none\n");
    } else {
        for error in &report.folder_errors {
            output.push_str(&format!("  {}: {}\n", error.path.display(), error.error));
        }
    }
    output
}

/// One archive as shown by `library-list`/`library-find`: a display-ready
/// reshaping of [`PersistedArchive`] with just the fields those commands
/// need (path, platform, present/missing, size, modified time), not the
/// full persisted row (database id, normalized name, cached health, ...).
///
/// `path` serializes via `Path::display` (see `serialize_path_display`)
/// rather than `PathBuf`'s own `Serialize` impl, which requires valid
/// Unicode and would otherwise make `--json` output fail entirely for the
/// whole list just because one archive's path is not valid UTF-8. Exact
/// path bytes remain safely preserved in the database (see
/// `PersistedArchive`/`archives.relative_path`) - this is purely a display
/// concern for a view type, matching the same "display-safe path text"
/// this crate already uses for `library-find`'s search matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LibraryArchiveView {
    #[serde(serialize_with = "serialize_path_display")]
    path: PathBuf,
    platform: Option<String>,
    present: bool,
    size_bytes: Option<u64>,
    modified_time_unix_seconds: Option<i64>,
}

fn serialize_path_display<S: serde::Serializer>(
    path: &std::path::Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.display().to_string())
}

impl From<&PersistedArchive> for LibraryArchiveView {
    fn from(archive: &PersistedArchive) -> Self {
        Self {
            path: archive.absolute_path.clone(),
            platform: archive.platform.clone(),
            present: archive.last_verified_missing_at.is_none(),
            size_bytes: archive.size_bytes,
            modified_time_unix_seconds: archive.modified_time_unix_seconds,
        }
    }
}

/// Loads every persisted archive for `library-list`/`library-find`. If no
/// database file exists yet, this is an empty catalogue (`Ok(vec![])`),
/// not an error - `print_library_entries` distinguishes "no database yet"
/// from "database exists but is empty" for the human-readable message.
fn build_library_entries(
    database_path: &Path,
) -> Result<Vec<LibraryArchiveView>, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Ok(Vec::new());
    }
    let database = Database::open_or_create(database_path)?;
    Ok(database
        .load_archives()?
        .iter()
        .map(LibraryArchiveView::from)
        .collect())
}

/// Case-insensitive match against each entry's display-safe path text
/// (`Path::display`, the same lossy-for-display-only conversion used
/// throughout this CLI - never the entry's identity) and detected
/// platform, mirroring `find_archive_index_entries`'s existing matching
/// style for the JSON index.
fn filter_library_entries(entries: &[LibraryArchiveView], query: &str) -> Vec<LibraryArchiveView> {
    let needle = query.to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            entry
                .path
                .display()
                .to_string()
                .to_lowercase()
                .contains(&needle)
                || entry
                    .platform
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect()
}

fn print_library_entries(database_path: &Path, entries: &[LibraryArchiveView]) {
    if entries.is_empty() {
        if database_path.exists() {
            println!("No archives in the library catalogue yet.");
        } else {
            println!(
                "No library database found at {}. Run: archivefs-cli library-scan",
                database_path.display()
            );
        }
        return;
    }

    println!("ArchiveFS Library List\n");
    print!("{}", format_library_entries(entries));
}

fn print_library_find_results(query: &str, entries: &[LibraryArchiveView]) {
    if entries.is_empty() {
        println!("No library matches found for '{query}'.");
        return;
    }

    println!("ArchiveFS Library Find");
    println!("Query: {query}\n");
    print!("{}", format_library_entries(entries));
}

fn print_library_entries_json(entries: &[LibraryArchiveView]) -> Result<(), serde_json::Error> {
    println!("{}", format_library_entries_json(entries)?);
    Ok(())
}

fn format_library_entries_json(
    entries: &[LibraryArchiveView],
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(entries)
}

fn format_library_entries(entries: &[LibraryArchiveView]) -> String {
    let mut output = String::new();
    for entry in entries {
        output.push_str(&format!("  Path: {}\n", entry.path.display()));
        output.push_str(&format!(
            "  Platform: {}\n",
            entry.platform.as_deref().unwrap_or("Unknown")
        ));
        output.push_str(&format!(
            "  State: {}\n",
            if entry.present { "Present" } else { "Missing" }
        ));
        output.push_str(&format!(
            "  Size: {}\n",
            entry
                .size_bytes
                .map(human_size)
                .unwrap_or_else(|| "unknown".to_string())
        ));
        output.push_str(&format!(
            "  Modified: {}\n",
            entry
                .modified_time_unix_seconds
                .map(|seconds| format_unix_timestamp(seconds.max(0) as u64))
                .unwrap_or_else(|| "unknown".to_string())
        ));
        output.push('\n');
    }
    output
}

fn print_cleaned_dirs(paths: &[std::path::PathBuf]) {
    for path in paths {
        println!("Removed: {}", path.display());
    }
    println!("Removed {} empty directories.", paths.len());
}

fn print_index_find_results(query: &str, entries: &[ArchiveIndexEntry]) {
    if entries.is_empty() {
        println!("No index matches found for '{query}'.");
        return;
    }

    println!("ArchiveFS Index Find");
    println!("Query: {query}");
    println!();
    println!("Matches:");
    for entry in entries {
        println!(
            "  Platform: {}",
            entry.platform.as_deref().unwrap_or("Unknown")
        );
        println!("  Display: {}", entry.display_name);
        println!("  Archive: {}", entry.archive_path.display());
        println!("  Mount: {}", entry.mount_path.display());
        println!("  Health: {}", entry.health);
        println!("  State: {}", entry.mount_state);
        println!();
    }
}

fn print_index_summary(summary: &ArchiveIndexSummary) {
    println!("ArchiveFS Index");
    println!();
    println!("Summary:");
    println!("  Total archives: {}", summary.archives_count);
    println!("  Mounted: {}", summary.mounted_count);
    println!("  Pending: {}", summary.pending_count);
    println!();
    println!("Platforms:");
    if summary.platform_counts.is_empty() {
        println!("  none");
    } else {
        for (platform, count) in &summary.platform_counts {
            println!("  {platform}: {count}");
        }
    }
}

fn print_mount_one(plan: &MountPlan) {
    println!("Mounted:");
    println!("  Archive: {}", plan.archive.path.display());
    println!("  Mount:   {}", plan.mount_path.display());
}

fn print_unmount_one(plan: &MountPlan) {
    println!("Unmounted:");
    println!("  Archive: {}", plan.archive.path.display());
    println!("  Mount:   {}", plan.mount_path.display());
}

fn print_doctor_report(report: &DoctorReport) {
    print!("{}", format_doctor_report(report));
}

fn print_doctor_report_json(report: &DoctorReport) -> Result<(), serde_json::Error> {
    println!("{}", format_doctor_report_json(report)?);
    Ok(())
}

fn format_doctor_report_json(report: &DoctorReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Doctor\n");
    output.push_str(&format!("Config: {}\n", report.config_path.display()));
    output.push_str("\nChecks:\n");
    for check in &report.checks {
        output.push_str(&format!(
            "  [{:<4}] {:<16} {}\n",
            check.status, check.name, check.detail
        ));
    }
    output.push_str("\nSummary:\n");
    output.push_str(&format!("  Archives found: {}\n", report.archives_found));
    output.push_str(&format!(
        "  Archives with detected platform: {}\n",
        report.archives_with_platform
    ));
    output.push_str(&format!(
        "  Archives with unknown platform: {}\n",
        report.archives_unknown_platform
    ));
    output.push_str(&format!(
        "  Pending archives: {}\n",
        report.pending_archives
    ));
    output.push_str(&format!(
        "  Mounted archives: {}\n",
        report.mounted_archives
    ));
    output.push_str(&format!(
        "  Ready: {}\n",
        if report.is_ready() { "yes" } else { "no" }
    ));
    output.push_str("\nPlatforms:\n");
    if report.platform_counts.is_empty() {
        output.push_str("  none\n");
    } else {
        for (platform, count) in &report.platform_counts {
            output.push_str(&format!("  {platform}: {count}\n"));
        }
    }
    output.push_str("\nUnknown platform examples:\n");
    if report.unknown_platform_examples.is_empty() {
        output.push_str("  none\n");
    } else {
        for path in &report.unknown_platform_examples {
            output.push_str(&format!("  {}\n", path.display()));
        }
    }
    output
}

fn print_duplicate_report(report: &DuplicateReport) {
    print!("{}", format_duplicate_report(report));
}

fn print_duplicate_report_json(report: &DuplicateReport) -> Result<(), serde_json::Error> {
    println!("{}", format_duplicate_report_json(report)?);
    Ok(())
}

fn format_duplicate_report_json(report: &DuplicateReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_duplicate_report(report: &DuplicateReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Duplicates\n\n");
    output.push_str("Summary:\n");
    output.push_str(&format!("  Records checked: {}\n", report.archives_checked));
    output.push_str(&format!(
        "  Duplicate groups found: {}\n",
        report.entries.len()
    ));

    if report.entries.is_empty() {
        output.push_str("\nNo duplicate candidates found.\n");
        return output;
    }

    output.push_str("\nDuplicate groups:\n");
    for (index, entry) in report.entries.iter().enumerate() {
        push_duplicate_entry(&mut output, index + 1, entry);
    }
    output
}

fn push_duplicate_entry(output: &mut String, index: usize, entry: &DuplicateEntry) {
    output.push_str(&format!("  Group {index}:\n"));
    output.push_str(&format!("    Platform: {}\n", entry.platform));
    output.push_str(&format!("    Severity: {}\n", entry.severity));
    output.push_str(&format!("    Reason: {}\n", entry.reason));
    output.push_str("    Archives:\n");
    for archive_path in &entry.archive_paths {
        output.push_str(&format!("      {}\n", archive_path.display()));
    }
}

fn print_archive_info(info: &ArchiveInfo) {
    print!("{}", format_archive_info(info));
}

fn print_archive_info_json(info: &ArchiveInfo) -> Result<(), serde_json::Error> {
    println!("{}", format_archive_info_json(info)?);
    Ok(())
}

fn format_archive_info_json(info: &ArchiveInfo) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(info)
}

fn format_archive_info(info: &ArchiveInfo) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Info\n\n");
    output.push_str("Details:\n");
    output.push_str(&format!("  Title: {}\n", info.title));
    output.push_str(&format!(
        "  Platform: {}\n",
        info.platform.as_deref().unwrap_or("Unknown")
    ));
    output.push_str(&format!(
        "  Archive path: {}\n",
        info.archive_path.display()
    ));
    output.push_str(&format!("  Mount path: {}\n", info.mount_path.display()));
    output.push_str(&format!("  Extension: {}\n", info.extension));
    output.push_str(&format!(
        "  Archive size: {}\n",
        info.size_bytes
            .map(human_size)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!(
        "  Last modified: {}\n",
        info.modified_time
            .map(format_system_time)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!("  Health: {}\n", info.health));
    output.push_str(&format!("  Mount state: {}\n", info.mount_state));
    output.push_str(&format!(
        "  Metadata provider: {}\n",
        info.metadata_provider
    ));
    output.push_str(&format!("  Health provider: {}\n", info.health_provider));
    output
}

fn print_archive_stats(stats: &ArchiveStats) {
    print!("{}", format_archive_stats(stats));
}

fn print_archive_stats_json(stats: &ArchiveStats) -> Result<(), serde_json::Error> {
    println!("{}", format_archive_stats_json(stats)?);
    Ok(())
}

fn format_archive_stats_json(stats: &ArchiveStats) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(stats)
}

fn format_archive_stats(stats: &ArchiveStats) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Stats\n\n");
    output.push_str("Summary:\n");
    output.push_str(&format!("  Total archives: {}\n", stats.total_archives));
    output.push_str(&format!("  Mounted: {}\n", stats.mounted_count));
    output.push_str(&format!("  Pending: {}\n", stats.pending_count));
    output.push_str(&format!(
        "  Total archive size: {}\n",
        human_size(stats.total_size_bytes)
    ));
    output.push_str("\nPlatforms:\n");
    push_counts(&mut output, &stats.platform_counts);
    output.push_str("\nArchive extensions:\n");
    push_counts(&mut output, &stats.extension_counts);
    output.push_str("\nLargest archive:\n");
    push_archive_size(&mut output, stats.largest_archive.as_ref());
    output.push_str("\nSmallest archive:\n");
    push_archive_size(&mut output, stats.smallest_archive.as_ref());
    output
}

fn push_counts(output: &mut String, counts: &[(String, usize)]) {
    if counts.is_empty() {
        output.push_str("  none\n");
    } else {
        for (name, count) in counts {
            output.push_str(&format!("  {name}: {count}\n"));
        }
    }
}

fn push_archive_size(output: &mut String, archive: Option<&archivefs_core::ArchiveSizeSummary>) {
    if let Some(archive) = archive {
        output.push_str(&format!(
            "  {} ({})\n",
            archive.archive_path.display(),
            human_size(archive.size_bytes)
        ));
    } else {
        output.push_str("  none\n");
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn format_system_time(time: std::time::SystemTime) -> String {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format_unix_timestamp(duration.as_secs()),
        Err(error) => format!("before UNIX epoch by {}s", error.duration().as_secs()),
    }
}

fn format_unix_timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn print_statuses(statuses: &[ArchiveStatus]) {
    print!("{}", format_statuses(statuses));
}

fn format_statuses(statuses: &[ArchiveStatus]) -> String {
    let mut output = format!("{:<48}  {:<48}  State\n", "Archive", "Mount");
    for status in statuses {
        output.push_str(&format!(
            "{:<48}  {:<48}  {}\n",
            status.archive_path.display(),
            status.mount_path.display(),
            status.state
        ));
    }
    output
}

fn print_statuses_json(statuses: &[ArchiveStatus]) -> Result<(), serde_json::Error> {
    println!("{}", format_statuses_json(statuses)?);
    Ok(())
}

fn format_statuses_json(statuses: &[ArchiveStatus]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(statuses)
}

fn print_help() {
    println!("archivefs [--verbose|-v] [--debug] <command>");
    println!();
    println!("Global flags:");
    println!("  -v, --verbose  Show operational logs");
    println!("  --debug        Show diagnostic logs");
    println!();
    println!("Commands:");
    println!("  scan           List supported archives from configured source folders");
    println!("  doctor         Check whether ArchiveFS is ready to run");
    println!("  config-check   Validate ArchiveFS configuration");
    println!("  status         Show archive paths, mount paths, and mount states");
    println!("  stats          Show archive library counts and sizes");
    println!("  duplicates     Show filename-based duplicate candidates");
    println!("  info           Show details for one archive by path or name");
    println!("  mount          Mount scanned archives with ratarmount");
    println!("  mount-one      Mount one archive by path or name");
    println!("  unmount        Unmount ArchiveFS mountpoints under mount_root");
    println!("  unmount-one    Unmount one archive by path or name");
    println!("  clean          Remove empty directories under mount_root");
    println!("  watch          Watch source folders and refresh the JSON index");
    println!("  index-build    Build the JSON archive index");
    println!("  index-show     Show a summary of the JSON archive index");
    println!("  index-find     Find entries in the JSON archive index");
    println!("  library-status Show the persistent library database's health and counts");
    println!("  library-scan   Scan configured source folders into the library database");
    println!("  library-list   List archives from the library database (no rescan)");
    println!("  library-find   Search the library database by path or platform");
    println!();
    println!("Examples:");
    println!("  archivefs doctor");
    println!("  archivefs config-check");
    println!("  archivefs status --json");
    println!("  archivefs stats");
    println!("  archivefs library-status");
    println!("  archivefs library-scan");
    println!("  archivefs library-list");
    println!("  archivefs library-find \"007 Legends\"");
    println!("  archivefs stats --json");
    println!("  archivefs info \"007 Legends\"");
    println!("  archivefs mount-one \"007 Legends\"");
    println!("  archivefs unmount-one \"007 Legends\"");
    println!("  archivefs watch");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example config:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_statuses() -> Vec<ArchiveStatus> {
        vec![
            ArchiveStatus {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
                state: archivefs_core::MountState::Mounted,
            },
            ArchiveStatus {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                mount_path: std::path::PathBuf::from("/mnt/archivefs/Unknown/Mystery"),
                state: archivefs_core::MountState::Pending,
            },
        ]
    }

    #[test]
    fn format_statuses_preserves_existing_human_output_exactly() {
        let output = format_statuses(&example_statuses());

        assert_eq!(
            output,
            concat!(
                "Archive                                           Mount                                             State\n",
                "/roms/Halo.zip                                    /mnt/archivefs/Xbox/Halo                          Mounted\n",
                "/roms/Mystery.7z                                  /mnt/archivefs/Unknown/Mystery                    Pending\n",
            )
        );
    }

    #[test]
    fn format_statuses_json_outputs_valid_pretty_json_with_expected_fields() {
        let output = format_statuses_json(&example_statuses()).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();
        let statuses = json.as_array().unwrap();

        assert!(output.starts_with("[\n"));
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0]["archive_path"], "/roms/Halo.zip");
        assert_eq!(statuses[0]["mount_path"], "/mnt/archivefs/Xbox/Halo");
        assert_eq!(statuses[0]["state"], "Mounted");
        assert_eq!(statuses[1]["state"], "Pending");
    }

    #[test]
    fn format_statuses_json_contains_no_human_heading() {
        let output = format_statuses_json(&example_statuses()).unwrap();

        assert!(!output.contains("Archive                                           Mount"));
        assert!(!output.contains("State\n"));
    }

    #[test]
    fn format_doctor_report_preserves_human_output_shape() {
        let report = DoctorReport {
            config_path: std::path::PathBuf::from("/home/user/.config/archivefs/config.toml"),
            checks: vec![archivefs_core::DoctorCheck {
                name: "config".to_string(),
                status: archivefs_core::DoctorStatus::Pass,
                detail: "configuration loaded".to_string(),
            }],
            archives_found: 3,
            archives_with_platform: 2,
            archives_unknown_platform: 1,
            unknown_platform_examples: vec![std::path::PathBuf::from("/roms/Unknown.zip")],
            platform_counts: vec![("Xbox360".to_string(), 2)],
            pending_archives: 2,
            mounted_archives: 1,
        };

        let output = format_doctor_report(&report);

        assert!(output.contains("ArchiveFS Doctor"));
        assert!(output.contains("Config: /home/user/.config/archivefs/config.toml"));
        assert!(output.contains("Checks:"));
        assert!(output.contains("[PASS] config"));
        assert!(output.contains("Summary:"));
        assert!(output.contains("Archives found: 3"));
        assert!(output.contains("Ready: yes"));
        assert!(output.contains("Platforms:"));
        assert!(output.contains("Xbox360: 2"));
        assert!(output.contains("Unknown platform examples:"));
        assert!(output.contains("/roms/Unknown.zip"));
    }

    #[test]
    fn format_doctor_report_json_outputs_pretty_json_only() {
        let report = DoctorReport {
            config_path: std::path::PathBuf::from("/home/user/.config/archivefs/config.toml"),
            checks: vec![archivefs_core::DoctorCheck {
                name: "config".to_string(),
                status: archivefs_core::DoctorStatus::Warn,
                detail: "configuration has warnings".to_string(),
            }],
            archives_found: 3,
            archives_with_platform: 2,
            archives_unknown_platform: 1,
            unknown_platform_examples: vec![std::path::PathBuf::from("/roms/Unknown.zip")],
            platform_counts: vec![("Xbox360".to_string(), 2)],
            pending_archives: 2,
            mounted_archives: 1,
        };

        let output = format_doctor_report_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Doctor"));
        assert!(!output.contains("Summary:"));
        assert_eq!(
            json["config_path"],
            "/home/user/.config/archivefs/config.toml"
        );
        assert_eq!(json["checks"][0]["name"], "config");
        assert_eq!(json["checks"][0]["status"], "Warn");
        assert_eq!(json["archives_found"], 3);
        assert_eq!(json["archives_with_platform"], 2);
        assert_eq!(json["archives_unknown_platform"], 1);
        assert_eq!(json["unknown_platform_examples"][0], "/roms/Unknown.zip");
        assert_eq!(json["platform_counts"][0][0], "Xbox360");
        assert_eq!(json["platform_counts"][0][1], 2);
        assert_eq!(json["pending_archives"], 2);
        assert_eq!(json["mounted_archives"], 1);
    }

    #[test]
    fn format_duplicate_report_shows_friendly_empty_message() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: Vec::new(),
        };

        let output = format_duplicate_report(&report);

        assert!(output.contains("ArchiveFS Duplicates"));
        assert!(output.contains("Records checked: 2"));
        assert!(output.contains("Duplicate groups found: 0"));
        assert!(output.contains("No duplicate candidates found."));
    }

    #[test]
    fn format_duplicate_report_json_outputs_pretty_json_only() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name '007_legends' on platform 'Xbox360'"
                    .to_string(),
                archive_paths: vec![
                    std::path::PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    std::path::PathBuf::from("/roms/imports/007 Legends.7z"),
                ],
            }],
        };

        let output = format_duplicate_report_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Duplicates"));
        assert!(!output.contains("Summary:"));
        assert_eq!(json["detector"], "filename");
        assert_eq!(json["archives_checked"], 2);
        assert_eq!(json["entries"].as_array().unwrap().len(), 1);
        assert_eq!(json["entries"][0]["platform"], "Xbox360");
        assert_eq!(json["entries"][0]["severity"], "Warning");
        assert_eq!(
            json["entries"][0]["archive_paths"][0],
            "/roms/xbox360/007 Legends.zip"
        );
        assert_eq!(
            json["entries"][0]["archive_paths"][1],
            "/roms/imports/007 Legends.7z"
        );
    }

    #[test]
    fn format_duplicate_report_shows_group_details() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name '007_legends' on platform 'Xbox360'"
                    .to_string(),
                archive_paths: vec![
                    std::path::PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    std::path::PathBuf::from("/roms/imports/007 Legends.7z"),
                ],
            }],
        };

        let output = format_duplicate_report(&report);

        assert!(output.contains("Records checked: 2"));
        assert!(output.contains("Duplicate groups found: 1"));
        assert!(output.contains("Group 1:"));
        assert!(output.contains("Platform: Xbox360"));
        assert!(output.contains("Severity: Warning"));
        assert!(output.contains("007_legends"));
        assert!(output.contains("/roms/xbox360/007 Legends.zip"));
        assert!(output.contains("/roms/imports/007 Legends.7z"));
    }

    #[test]
    fn format_archive_info_includes_all_display_fields() {
        let info = ArchiveInfo {
            title: "Halo".to_string(),
            platform: Some("Xbox".to_string()),
            archive_path: std::path::PathBuf::from("/roms/xbox/Halo.zip"),
            mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
            extension: "zip".to_string(),
            size_bytes: Some(2048),
            modified_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
            health: archivefs_core::ArchiveHealth::Pending,
            mount_state: archivefs_core::MountState::Mounted,
            metadata_provider: "FilenameMetadataProvider".to_string(),
            health_provider: "FilesystemHealthProvider".to_string(),
        };

        let output = format_archive_info(&info);

        assert!(output.contains("Title: Halo"));
        assert!(output.contains("Platform: Xbox"));
        assert!(output.contains("Archive path: /roms/xbox/Halo.zip"));
        assert!(output.contains("Mount path: /mnt/archivefs/Xbox/Halo"));
        assert!(output.contains("Extension: zip"));
        assert!(output.contains("Archive size: 2.0 KiB"));
        assert!(output.contains("Last modified: 1970-01-02 00:00:00 UTC"));
        assert!(output.contains("Health: Pending"));
        assert!(output.contains("Mount state: Mounted"));
        assert!(output.contains("Metadata provider: FilenameMetadataProvider"));
        assert!(output.contains("Health provider: FilesystemHealthProvider"));
    }

    #[test]
    fn format_archive_info_json_outputs_expected_fields_without_headings() {
        let info = ArchiveInfo {
            title: "Halo".to_string(),
            platform: Some("Xbox".to_string()),
            archive_path: std::path::PathBuf::from("/roms/xbox/Halo.zip"),
            mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
            extension: "zip".to_string(),
            size_bytes: Some(2048),
            modified_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
            health: archivefs_core::ArchiveHealth::Pending,
            mount_state: archivefs_core::MountState::Mounted,
            metadata_provider: "FilenameMetadataProvider".to_string(),
            health_provider: "FilesystemHealthProvider".to_string(),
        };

        let output = format_archive_info_json(&info).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Info"));
        assert!(!output.contains("Details:"));
        assert_eq!(json["title"], "Halo");
        assert_eq!(json["platform"], "Xbox");
        assert_eq!(json["archive_path"], "/roms/xbox/Halo.zip");
        assert_eq!(json["mount_path"], "/mnt/archivefs/Xbox/Halo");
        assert_eq!(json["extension"], "zip");
        assert_eq!(json["size_bytes"], 2048);
        assert_eq!(json["modified_time"], 86_400);
        assert_eq!(json["health"], "Pending");
        assert_eq!(json["mount_state"], "Mounted");
        assert_eq!(json["metadata_provider"], "FilenameMetadataProvider");
        assert_eq!(json["health_provider"], "FilesystemHealthProvider");
    }

    #[test]
    fn format_archive_stats_json_outputs_pretty_json_only() {
        let stats = ArchiveStats {
            total_archives: 2,
            mounted_count: 1,
            pending_count: 1,
            platform_counts: vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 1)],
            extension_counts: vec![("7z".to_string(), 1), ("zip".to_string(), 1)],
            largest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                size_bytes: 2048,
            }),
            smallest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                size_bytes: 512,
            }),
            total_size_bytes: 2560,
        };

        let output = format_archive_stats_json(&stats).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Stats"));
        assert_eq!(json["total_archives"], 2);
        assert_eq!(json["mounted_count"], 1);
        assert_eq!(json["pending_count"], 1);
        assert_eq!(json["total_size_bytes"], 2560);
        assert_eq!(json["platform_counts"]["Unknown"], 1);
        assert_eq!(json["platform_counts"]["Xbox360"], 1);
        assert_eq!(json["extension_counts"]["7z"], 1);
        assert_eq!(json["extension_counts"]["zip"], 1);
        assert_eq!(json["largest_archive"]["archive_path"], "/roms/Halo.zip");
        assert_eq!(json["smallest_archive"]["size_bytes"], 512);
    }

    #[test]
    fn format_archive_stats_includes_counts_and_sizes() {
        let stats = ArchiveStats {
            total_archives: 2,
            mounted_count: 1,
            pending_count: 1,
            platform_counts: vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 1)],
            extension_counts: vec![("7z".to_string(), 1), ("zip".to_string(), 1)],
            largest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                size_bytes: 2048,
            }),
            smallest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                size_bytes: 512,
            }),
            total_size_bytes: 2560,
        };

        let output = format_archive_stats(&stats);

        assert!(output.contains("Total archives: 2"));
        assert!(output.contains("Mounted: 1"));
        assert!(output.contains("Pending: 1"));
        assert!(output.contains("Total archive size: 2.5 KiB"));
        assert!(output.contains("  Xbox360: 1"));
        assert!(output.contains("  zip: 1"));
        assert!(output.contains("/roms/Halo.zip (2.0 KiB)"));
        assert!(output.contains("/roms/Mystery.7z (512 B)"));
    }

    #[test]
    fn parse_cli_args_defaults_to_quiet_help() {
        let args = parse_cli_args(Vec::<String>::new());

        assert_eq!(args.log_level, log::LevelFilter::Off);
        assert_eq!(args.command, "help");
        assert!(args.args.is_empty());
    }

    #[test]
    fn parse_cli_args_accepts_verbose_flag() {
        let args = parse_cli_args(["-v", "scan"].into_iter().map(str::to_string));

        assert_eq!(args.log_level, log::LevelFilter::Info);
        assert_eq!(args.command, "scan");
    }

    #[test]
    fn parse_cli_args_accepts_debug_flag_and_preserves_command_args() {
        let args = parse_cli_args(
            ["--debug", "mount-one", "Test", "Game"]
                .into_iter()
                .map(str::to_string),
        );

        assert_eq!(args.log_level, log::LevelFilter::Debug);
        assert_eq!(args.command, "mount-one");
        assert_eq!(args.args, vec!["Test".to_string(), "Game".to_string()]);
    }

    // -------------------------------------------------------------
    // library-status / library-scan / library-list / library-find
    //
    // All of these call the testable core functions
    // (build_library_status_view / run_library_scan / build_library_entries
    // / filter_library_entries) directly with explicit temp paths, exactly
    // like archivefs_core's own database tests - never Config::load_default
    // or default_database_path, so nothing here touches the real $HOME or
    // races other tests over process-wide environment variables.
    // -------------------------------------------------------------

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("archivefs-cli-test-{name}-{}", std::process::id()));
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

    #[test]
    fn library_status_reports_no_database_before_any_scan() {
        let root = temp_dir("status-no-database");
        let database_path = root.join("library.sqlite3");

        let view = build_library_status_view(&database_path);

        assert!(!view.health.database_exists);
        assert!(!view.health.database_opens);
        assert!(view.stats.is_none());
        assert!(view.last_completed_scan.is_none());
        assert!(
            !database_path.exists(),
            "a status check must never create the database"
        );

        let output = format_library_status(&view);
        assert!(output.contains("Exists: no"));
        assert!(output.contains("No library database yet. Run: archivefs-cli library-scan"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_status_reports_counts_after_a_successful_scan() {
        let root = temp_dir("status-after-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);

        run_library_scan(&config, &database_path, "test").unwrap();
        let view = build_library_status_view(&database_path);

        assert!(view.health.database_exists);
        assert!(view.health.database_opens);
        assert!(view.health.migrations_current);
        assert!(view.health.foreign_keys_enabled);
        let stats = view
            .stats
            .as_ref()
            .expect("stats must be present once migrations are current");
        assert_eq!(stats.total_archives, 1);
        assert_eq!(stats.present_archives, 1);
        assert_eq!(stats.archives_with_platform, 1);
        let scan = view
            .last_completed_scan
            .as_ref()
            .expect("a completed scan must be reported");
        assert_eq!(scan.archives_added, 1);

        let output = format_library_status(&view);
        assert!(output.contains("Total: 1"));
        assert!(output.contains("Present: 1"));
        assert!(output.contains("Detected platform: 1"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_status_json_parses_and_contains_expected_fields() {
        let root = temp_dir("status-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let view = build_library_status_view(&database_path);
        let output = format_library_status_json(&view).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert_eq!(json["database_exists"], true);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["migrations_current"], true);
        assert_eq!(json["stats"]["total_archives"], 1);
        assert_eq!(json["last_completed_scan"]["archives_added"], 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_creates_the_database() {
        let root = temp_dir("scan-creates-database");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        assert!(!database_path.exists());

        let report = run_library_scan(&config, &database_path, "test").unwrap();

        assert!(database_path.exists());
        assert_eq!(report.archives_new, 1);
        assert_eq!(report.source_folders_attempted, 1);
        assert_eq!(report.source_folders_succeeded, 1);
        assert_eq!(report.source_folders_failed, 0);
        assert!(report.folder_errors.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_reports_partial_source_folder_failure() {
        let root = temp_dir("scan-partial-failure");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source_a, "a.zip", b"a");
        write_archive_file(&source_b, "b.zip", b"b");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        run_library_scan(&config, &database_path, "test").unwrap();

        std::fs::remove_dir_all(&source_a).unwrap();
        let report = run_library_scan(&config, &database_path, "test").unwrap();

        assert_eq!(report.source_folders_attempted, 2);
        assert_eq!(report.source_folders_succeeded, 1);
        assert_eq!(report.source_folders_failed, 1);
        assert_eq!(report.folder_errors.len(), 1);
        assert_eq!(report.folder_errors[0].path, source_a);

        let output = format_library_scan(&report);
        assert!(output.contains("Attempted: 2"));
        assert!(output.contains("Succeeded: 1"));
        assert!(output.contains("Failed: 1"));
        assert!(output.contains(&source_a.display().to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_json_parses_and_contains_expected_fields() {
        let root = temp_dir("scan-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);

        let report = run_library_scan(&config, &database_path, "test").unwrap();
        let output = format_library_scan_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert_eq!(json["archives_new"], 1);
        assert_eq!(json["source_folders_succeeded"], 1);
        assert_eq!(json["folder_errors"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_shows_present_and_missing_rows() {
        let root = temp_dir("list-present-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "keep.zip", b"a");
        let doomed = write_archive_file(&source, "gone.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        std::fs::remove_file(&doomed).unwrap();
        run_library_scan(&config, &database_path, "test").unwrap();

        let entries = build_library_entries(&database_path).unwrap();

        assert_eq!(entries.len(), 2);
        let keep = entries
            .iter()
            .find(|entry| entry.path.ends_with("keep.zip"))
            .unwrap();
        let gone = entries
            .iter()
            .find(|entry| entry.path.ends_with("gone.zip"))
            .unwrap();
        assert!(keep.present);
        assert!(!gone.present);

        let output = format_library_entries(&entries);
        assert!(output.contains("State: Present"));
        assert!(output.contains("State: Missing"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_with_no_database_is_an_empty_but_successful_result() {
        let root = temp_dir("list-no-database");
        let database_path = root.join("library.sqlite3");

        let entries = build_library_entries(&database_path).unwrap();

        assert!(entries.is_empty());
        assert!(!database_path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_matches_case_insensitively_on_path() {
        let root = temp_dir("find-path-match");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Halo.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path).unwrap();

        let matches = filter_library_entries(&entries, "HALO");

        assert_eq!(matches.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_matches_on_platform() {
        let root = temp_dir("find-platform-match");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path).unwrap();

        let matches = filter_library_entries(&entries, "xbox360");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].platform.as_deref(), Some("Xbox360"));

        let output = print_library_find_results_for_test("xbox360", &matches);
        assert!(output.contains("Query: xbox360"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_returns_no_results_without_erroring() {
        let entries: Vec<LibraryArchiveView> = Vec::new();
        let matches = filter_library_entries(&entries, "nothing-will-match-this");

        assert!(matches.is_empty());
    }

    #[test]
    fn library_find_json_parses_and_round_trips_expected_fields() {
        let root = temp_dir("find-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path).unwrap();
        let matches = filter_library_entries(&entries, "game");

        let output = format_library_entries_json(&matches).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("[\n"));
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["present"], true);
        assert_eq!(json[0]["platform"], "Xbox360");
        assert!(
            json[0]["path"]
                .as_str()
                .unwrap()
                .ends_with("Xbox360/game.zip")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_platform_is_shown_as_unknown_not_a_stored_sentinel() {
        let root = temp_dir("unknown-platform-cli");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].platform, None,
            "an undetected platform must round-trip as None, not a sentinel string"
        );

        let output = format_library_entries(&entries);
        assert!(output.contains("Platform: Unknown"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_formats_without_panicking() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let non_utf8_name =
            OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p']);
        let entry = LibraryArchiveView {
            path: PathBuf::from("/roms").join(&non_utf8_name),
            platform: Some("Unknown".to_string()),
            present: true,
            size_bytes: Some(10),
            modified_time_unix_seconds: Some(0),
        };

        // Human output uses Path::display, which is lossy-but-safe and
        // must not panic on a non-UTF-8 path.
        let human = format_library_entries(std::slice::from_ref(&entry));
        assert!(human.contains("Path: "));

        // JSON output uses the same display-safe conversion (see
        // serialize_path_display) rather than PathBuf's own Serialize
        // impl (which requires valid Unicode and would otherwise fail the
        // whole list's --json output over one oddly-named archive) - it
        // must succeed and produce valid, parseable JSON, not panic or
        // error out.
        let json = format_library_entries_json(std::slice::from_ref(&entry)).unwrap();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert!(parsed[0]["path"].as_str().unwrap().contains("fo"));
    }

    #[test]
    fn database_failure_does_not_affect_mount_planning_in_the_cli_layer() {
        // Mirrors the equivalent test in archivefs_core::database: force a
        // database failure here, in the CLI's own test suite, then confirm
        // real (unrelated) core mount-planning logic still behaves
        // normally in the same test. mount/mount-one/unmount/unmount-one
        // command handlers in `run()` never call any library-* function.
        let root = temp_dir("cli-database-failure-mount-safety");
        let occupied_by_a_file = root.join("not-a-directory");
        std::fs::write(&occupied_by_a_file, b"not a directory").unwrap();
        let impossible_db_path = occupied_by_a_file.join("library.sqlite3");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);

        let result = run_library_scan(&config, &impossible_db_path, "test");
        assert!(result.is_err());

        let scanner = ArchiveScanner::new(&config);
        let archives = scanner.scan_archives().unwrap();
        let plans = archivefs_core::plan_mounts(&archives, &config.mount_root);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].state, archivefs_core::MountState::Pending);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Small helper so `library_find_matches_on_platform` can check the
    /// heading text without duplicating `print_library_find_results`'s
    /// stdout-writing shape.
    fn print_library_find_results_for_test(query: &str, entries: &[LibraryArchiveView]) -> String {
        let mut output = format!("ArchiveFS Library Find\nQuery: {query}\n\n");
        output.push_str(&format_library_entries(entries));
        output
    }
}
