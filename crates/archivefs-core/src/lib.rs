use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use log::{debug, info};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

mod database;
pub use database::{
    ArchiveChangeKind, ArchiveObservationKind, ArchiveUpsertOutcome, CatalogueStats,
    CompletedScanSummary, Database, DatabaseHealth, MANUAL_PLATFORM_SOURCE, PersistedArchive,
    PlatformAssignmentChange, RegisteredSourceFolder, ScanPersistSummary, ScanRunCounts,
    check_database_health, default_database_path, latest_schema_version, scan_and_persist,
};

#[derive(Debug)]
pub enum ArchiveFsError {
    Config(String),
    Scanner(String),
    Selection(SelectionError),
    Mount(String),
    Unmount(String),
    Index(String),
    Watcher(String),
    Database(String),
    Io {
        path: Option<PathBuf>,
        source: io::Error,
    },
    ExternalCommand {
        program: String,
        status: Option<i32>,
        stderr: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    NoMatch {
        input: String,
    },
    Ambiguous {
        input: String,
        matches: Vec<(PathBuf, PathBuf)>,
    },
}

impl ArchiveFsError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: Some(path.into()),
            source,
        }
    }

    fn selection_no_match(input: impl Into<String>) -> Self {
        Self::Selection(SelectionError::NoMatch {
            input: input.into(),
        })
    }

    fn selection_ambiguous(input: impl Into<String>, matches: Vec<(PathBuf, PathBuf)>) -> Self {
        Self::Selection(SelectionError::Ambiguous {
            input: input.into(),
            matches,
        })
    }

    pub fn allows_lazy_unmount_recovery(&self) -> bool {
        matches!(self, Self::Unmount(_) | Self::ExternalCommand { .. })
    }
}

impl fmt::Display for ArchiveFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "config error: {message}"),
            Self::Scanner(message) => write!(f, "scanner error: {message}"),
            Self::Selection(error) => write!(f, "{error}"),
            Self::Mount(message) => write!(f, "mount error: {message}"),
            Self::Unmount(message) => write!(f, "unmount error: {message}"),
            Self::Index(message) => write!(f, "index error: {message}"),
            Self::Watcher(message) => write!(f, "watcher error: {message}"),
            Self::Database(message) => write!(f, "database error: {message}"),
            Self::Io { path, source } => match path {
                Some(path) => write!(f, "{}: {}", path.display(), source),
                None => write!(f, "{source}"),
            },
            Self::ExternalCommand {
                program,
                status,
                stderr,
            } => {
                write!(f, "{program} failed")?;
                if let Some(code) = status {
                    write!(f, " with exit code {code}")?;
                }
                if !stderr.trim().is_empty() {
                    write!(f, ": {}", stderr.trim())?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatch { input } => write!(f, "no archive matched '{input}'"),
            Self::Ambiguous { input, matches } => {
                writeln!(f, "multiple archives matched '{input}':")?;
                for (archive_path, mount_path) in matches {
                    writeln!(
                        f,
                        "  Archive: {}\n  Mount:   {}",
                        archive_path.display(),
                        mount_path.display()
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ArchiveFsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl std::error::Error for SelectionError {}

impl From<io::Error> for ArchiveFsError {
    fn from(source: io::Error) -> Self {
        Self::Io { path: None, source }
    }
}

pub type Result<T> = std::result::Result<T, ArchiveFsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub source_folders: Vec<PathBuf>,
    pub mount_root: PathBuf,
    pub ratarmount_bin: String,
}

impl Config {
    pub fn load_default() -> Result<Self> {
        let path = default_config_path()?;
        info!("loading config from {}", path.display());
        Self::load_from(path)
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        debug!("reading config file {}", path.display());
        let contents = fs::read_to_string(path)
            .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
        let config = parse_config(&contents)?;
        info!(
            "loaded config: {} source folder(s), mount_root={}, ratarmount_bin={}",
            config.source_folders.len(),
            config.mount_root.display(),
            config.ratarmount_bin
        );
        Ok(config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

impl fmt::Display for DoctorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Warn => write!(f, "WARN"),
            Self::Fail => write!(f, "FAIL"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCheckStatus {
    Pass,
    Warn,
    Error,
}

impl fmt::Display for ConfigCheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Warn => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCheck {
    pub name: String,
    pub status: ConfigCheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCheckReport {
    pub config_path: PathBuf,
    pub checks: Vec<ConfigCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupDiagnosticStatus {
    Ready,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupDiagnostic {
    pub name: String,
    pub status: SetupDiagnosticStatus,
    pub detail: String,
    pub why_it_matters: String,
    pub next_step: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupDiagnostics {
    pub config_path: Option<PathBuf>,
    pub config_path_error: Option<String>,
    pub config_missing: bool,
    pub mount_root: Option<PathBuf>,
    pub can_create_mount_root: bool,
    pub ready_for_scanning: bool,
    pub ready_for_actions: bool,
    pub config_identity: ConfigIdentity,
    pub checks: Vec<SetupDiagnostic>,
}

/// A strong identity for one read of the configuration file: the resolved
/// path plus a SHA-256 digest of the exact bytes read. Two `ConfigIdentity`
/// values are only equal when both the path and content digest match, so
/// results derived from different reads of a changed config can never be
/// mistaken for a single coherent state.
///
/// This digest is a staleness fingerprint, not a security or authentication
/// mechanism: it is only ever compared against another value computed the
/// same way within this process to detect a config file changing between
/// two reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIdentity {
    pub config_path: Option<PathBuf>,
    pub content_digest: Option<[u8; 32]>,
}

fn config_identity(config_path: &Path, contents: Option<&str>) -> ConfigIdentity {
    ConfigIdentity {
        config_path: Some(config_path.to_path_buf()),
        content_digest: contents.map(|contents| Sha256::digest(contents.as_bytes()).into()),
    }
}

impl ConfigCheckReport {
    pub fn error_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == ConfigCheckStatus::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == ConfigCheckStatus::Warn)
            .count()
    }

    pub fn is_ok(&self) -> bool {
        self.error_count() == 0
    }

    fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ConfigCheck {
            name: name.into(),
            status: ConfigCheckStatus::Pass,
            detail: detail.into(),
        });
    }

    fn warn(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ConfigCheck {
            name: name.into(),
            status: ConfigCheckStatus::Warn,
            detail: detail.into(),
        });
    }

    fn error(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ConfigCheck {
            name: name.into(),
            status: ConfigCheckStatus::Error,
            detail: detail.into(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub config_path: PathBuf,
    pub checks: Vec<DoctorCheck>,
    pub archives_found: usize,
    pub archives_with_platform: usize,
    pub archives_unknown_platform: usize,
    pub unknown_platform_examples: Vec<PathBuf>,
    pub platform_counts: Vec<(String, usize)>,
    pub pending_archives: usize,
    pub mounted_archives: usize,
}

impl DoctorReport {
    pub fn is_ready(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|check| check.status == DoctorStatus::Fail)
    }

    fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Pass,
            detail: detail.into(),
        });
    }

    fn warn(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Warn,
            detail: detail.into(),
        });
    }

    fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Fail,
            detail: detail.into(),
        });
    }
}

pub fn run_doctor_default() -> DoctorReport {
    match default_config_path() {
        Ok(path) => run_doctor(path),
        Err(error) => doctor_config_path_error(error),
    }
}

pub fn run_doctor(config_path: impl AsRef<Path>) -> DoctorReport {
    run_doctor_with_mount_root_creation(config_path, true)
}

/// Runs doctor diagnostics without creating a missing mount root.
pub fn run_doctor_read_only_default() -> DoctorReport {
    match default_config_path() {
        Ok(path) => run_doctor_read_only(path),
        Err(error) => doctor_config_path_error(error),
    }
}

/// Runs doctor diagnostics without creating a missing mount root.
pub fn run_doctor_read_only(config_path: impl AsRef<Path>) -> DoctorReport {
    run_doctor_with_mount_root_creation(config_path, false)
}

fn doctor_config_path_error(error: ArchiveFsError) -> DoctorReport {
    let mut report = empty_doctor_report(PathBuf::from("~/.config/archivefs/config.toml"));
    report.fail("config path", error.to_string());
    report
}

fn empty_doctor_report(config_path: PathBuf) -> DoctorReport {
    DoctorReport {
        config_path,
        checks: Vec::new(),
        archives_found: 0,
        archives_with_platform: 0,
        archives_unknown_platform: 0,
        unknown_platform_examples: Vec::new(),
        platform_counts: Vec::new(),
        pending_archives: 0,
        mounted_archives: 0,
    }
}

fn run_doctor_with_mount_root_creation(
    config_path: impl AsRef<Path>,
    create_mount_root: bool,
) -> DoctorReport {
    let config_path = config_path.as_ref().to_path_buf();
    let mut report = empty_doctor_report(config_path.clone());

    if config_path.exists() {
        report.pass("config file", format!("found {}", config_path.display()));
    } else {
        report.fail("config file", format!("missing {}", config_path.display()));
        return report;
    }

    let config = match Config::load_from(&config_path) {
        Ok(config) => {
            report.pass("config parses", "configuration parsed successfully");
            config
        }
        Err(error) => {
            report.fail("config parses", error.to_string());
            return report;
        }
    };

    complete_doctor_report(&mut report, &config, create_mount_root, None);
    report
}

fn complete_doctor_report(
    report: &mut DoctorReport,
    config: &Config,
    create_mount_root: bool,
    snapshot: Option<(&[ArchiveRecord], &[ArchiveStatus])>,
) {
    let mut sources_ok = true;
    for source in &config.source_folders {
        match inspect_path(source) {
            PathInspection::Directory => {
                report.pass("source folder", format!("{} exists", source.display()));
            }
            state => {
                sources_ok = false;
                report.fail(
                    "source folder",
                    state.error_detail().map_or_else(
                        || format!("{} does not exist or is not a directory", source.display()),
                        |error| format!("{} cannot be inspected: {error}", source.display()),
                    ),
                );
            }
        }
    }

    match inspect_path(&config.mount_root) {
        PathInspection::Directory => report.pass(
            "mount root",
            format!("{} exists", config.mount_root.display()),
        ),
        PathInspection::Other => report.fail(
            "mount root",
            format!(
                "{} exists but is not a directory",
                config.mount_root.display()
            ),
        ),
        PathInspection::Missing if create_mount_root => {
            match fs::create_dir_all(&config.mount_root) {
                Ok(()) => report.pass(
                    "mount root",
                    format!("{} was created", config.mount_root.display()),
                ),
                Err(error) => report.fail(
                    "mount root",
                    format!("{} cannot be created: {error}", config.mount_root.display()),
                ),
            }
        }
        PathInspection::Missing => report.fail(
            "mount root",
            format!("{} does not exist", config.mount_root.display()),
        ),
        PathInspection::PermissionDenied(error) | PathInspection::MetadataError(error) => report
            .fail(
                "mount root",
                format!(
                    "{} cannot be inspected: {error}",
                    config.mount_root.display()
                ),
            ),
    }

    if command_available(&config.ratarmount_bin) {
        report.pass(
            "ratarmount",
            format!("{} is available", config.ratarmount_bin),
        );
    } else {
        report.fail(
            "ratarmount",
            format!("{} was not found", config.ratarmount_bin),
        );
    }

    if command_available("fusermount3") || command_available("umount") {
        report.pass("unmount tool", "fusermount3 or umount is available");
    } else {
        report.fail("unmount tool", "neither fusermount3 nor umount was found");
    }

    if !sources_ok {
        report.warn(
            "archive scan",
            "skipped because one or more source folders are unavailable",
        );
        report.warn(
            "mount status",
            "skipped because one or more source folders are unavailable",
        );
        return;
    }

    if let Some((records, statuses)) = snapshot {
        populate_doctor_archive_results(
            report,
            records
                .iter()
                .map(|record| (&record.identity.platform, &record.mount_plan.archive.path)),
        );
        populate_doctor_status_results(report, statuses);
        return;
    }

    let scanner = ArchiveScanner::new(config);
    match scanner.scan_archives() {
        Ok(archives) => {
            populate_doctor_archive_results(
                report,
                archives
                    .iter()
                    .map(|archive| (&archive.identity.platform, &archive.path)),
            );
            match scanner.archive_records_from_archives(archives) {
                Ok(records) => {
                    let statuses = archive_statuses_from_records(&records);
                    populate_doctor_status_results(report, &statuses);
                }
                Err(error) => report.fail("mount status", error.to_string()),
            }
        }
        Err(error) => {
            let detail = error.to_string();
            report.fail("archive scan", detail.clone());
            report.fail("mount status", detail);
        }
    }
}

fn populate_doctor_archive_results<'a>(
    report: &mut DoctorReport,
    mut archives: impl ExactSizeIterator<Item = (&'a Option<String>, &'a PathBuf)>,
) {
    let archives_found = archives.len();
    report.archives_found = archives_found;
    let mut platform_counts = BTreeMap::<String, usize>::new();
    for (platform, archive_path) in &mut archives {
        if let Some(platform) = platform {
            *platform_counts.entry(platform.clone()).or_default() += 1;
        } else {
            report.archives_unknown_platform += 1;
            if report.unknown_platform_examples.len() < 10 {
                report.unknown_platform_examples.push(archive_path.clone());
            }
        }
    }
    report.archives_with_platform = archives_found - report.archives_unknown_platform;
    report.platform_counts = platform_counts.into_iter().collect();
    report.pass("archive scan", format!("{archives_found} archives found"));
}

fn populate_doctor_status_results(report: &mut DoctorReport, statuses: &[ArchiveStatus]) {
    report.pending_archives = statuses
        .iter()
        .filter(|status| status.state == MountState::Pending)
        .count();
    report.mounted_archives = statuses
        .iter()
        .filter(|status| status.state == MountState::Mounted)
        .count();
    report.pass(
        "mount status",
        format!(
            "{} pending, {} mounted",
            report.pending_archives, report.mounted_archives
        ),
    );
}

pub fn run_config_check_default() -> ConfigCheckReport {
    match default_config_path() {
        Ok(path) => run_config_check(path),
        Err(error) => ConfigCheckReport {
            config_path: PathBuf::from("~/.config/archivefs/config.toml"),
            checks: vec![ConfigCheck {
                name: "config path".to_string(),
                status: ConfigCheckStatus::Error,
                detail: error.to_string(),
            }],
        },
    }
}

pub fn run_config_check(config_path: impl AsRef<Path>) -> ConfigCheckReport {
    run_config_check_with_mount_root_creation(config_path, true)
}

pub fn run_setup_diagnostics_default() -> SetupDiagnostics {
    run_setup_diagnostics_default_with_path(default_config_path())
}

fn run_setup_diagnostics_default_with_path(config_path: Result<PathBuf>) -> SetupDiagnostics {
    match config_path {
        Ok(path) => run_setup_diagnostics(path),
        Err(error) => SetupDiagnostics {
            config_path: None,
            config_path_error: Some(error.to_string()),
            config_missing: false,
            mount_root: None,
            can_create_mount_root: false,
            ready_for_scanning: false,
            ready_for_actions: false,
            config_identity: ConfigIdentity {
                config_path: None,
                content_digest: None,
            },
            checks: vec![SetupDiagnostic {
                name: "Config path".to_string(),
                status: SetupDiagnosticStatus::Error,
                detail: format!(
                    "ArchiveFS could not determine the user configuration directory: {error}"
                ),
                why_it_matters: "ArchiveFS needs a known configuration location.".to_string(),
                next_step: "Set HOME and refresh diagnostics.".to_string(),
            }],
        },
    }
}

pub fn run_setup_diagnostics(config_path: impl AsRef<Path>) -> SetupDiagnostics {
    run_setup_diagnostics_with_command_check(config_path, command_available)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathInspection {
    Missing,
    Directory,
    Other,
    PermissionDenied(String),
    MetadataError(String),
}

impl PathInspection {
    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory)
    }

    fn is_confirmed_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    fn error_detail(&self) -> Option<&str> {
        match self {
            Self::PermissionDenied(detail) | Self::MetadataError(detail) => Some(detail),
            Self::Missing | Self::Directory | Self::Other => None,
        }
    }
}

fn inspect_path(path: &Path) -> PathInspection {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => PathInspection::Directory,
        Ok(_) => PathInspection::Other,
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(path) {
            Ok(_) => PathInspection::Other,
            Err(error) if error.kind() == io::ErrorKind::NotFound => PathInspection::Missing,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                PathInspection::PermissionDenied(error.to_string())
            }
            Err(error) => PathInspection::MetadataError(error.to_string()),
        },
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            PathInspection::PermissionDenied(error.to_string())
        }
        Err(error) => PathInspection::MetadataError(error.to_string()),
    }
}

fn run_setup_diagnostics_with_command_check(
    config_path: impl AsRef<Path>,
    command_check: impl Fn(&str) -> bool,
) -> SetupDiagnostics {
    let config_path = config_path.as_ref().to_path_buf();
    run_setup_diagnostics_with_checks(
        config_path,
        |path| fs::read_to_string(path),
        inspect_path,
        command_check,
    )
}

fn run_setup_diagnostics_with_checks(
    config_path: PathBuf,
    read_config: impl Fn(&Path) -> io::Result<String>,
    inspect: impl Fn(&Path) -> PathInspection,
    command_check: impl Fn(&str) -> bool,
) -> SetupDiagnostics {
    let (config_missing, config_read_ok, config_read_detail, contents) =
        match read_config(&config_path) {
            Ok(contents) => (
                false,
                true,
                format!("Configuration path: {}", config_path.display()),
                Some(contents),
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let state = inspect(&config_path);
                let missing = state.is_confirmed_missing();
                let detail = if missing {
                    format!("Configuration file is missing: {}", config_path.display())
                } else {
                    format!(
                        "Configuration path {} could not be inspected safely: {}",
                        config_path.display(),
                        state
                            .error_detail()
                            .unwrap_or("the path is not a readable file")
                    )
                };
                (missing, false, detail, None)
            }
            Err(error) => (
                false,
                false,
                format!("{} cannot be read: {error}", config_path.display()),
                None,
            ),
        };
    let parsed = contents.as_deref().map(parse_config_fields);
    let parse_detail = match &parsed {
        Some(Ok(_)) => "Configuration parsed successfully.".to_string(),
        Some(Err(error)) => error.to_string(),
        None => "Configuration cannot be parsed until it can be read.".to_string(),
    };
    let fields = parsed.and_then(Result::ok);
    let config_valid = contents
        .as_deref()
        .is_some_and(|contents| parse_config(contents).is_ok());
    let source_folders = fields
        .as_ref()
        .and_then(|fields| fields.source_folders.as_ref())
        .map(|sources| sources.iter().map(PathBuf::from).collect::<Vec<_>>());
    let source_states = source_folders
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|source| (source.clone(), inspect(source)))
        .collect::<Vec<_>>();
    let mount_root = fields.as_ref().and_then(|fields| fields.mount_root.clone());
    let mount_root_state = mount_root.as_ref().map(|root| inspect(root));
    let can_create_mount_root = mount_root_state
        .as_ref()
        .is_some_and(PathInspection::is_confirmed_missing)
        && mount_root.as_ref().is_some_and(|root| {
            root.parent()
                .is_some_and(|parent| inspect(parent).is_directory())
        });
    let sources_ready =
        !source_states.is_empty() && source_states.iter().all(|(_, state)| state.is_directory());
    let mount_root_ready = mount_root_state
        .as_ref()
        .is_some_and(PathInspection::is_directory);
    let mount_root_writable = mount_root_ready
        && mount_root
            .as_ref()
            .is_some_and(|root| directory_is_writable(root));
    let ratarmount_name = fields
        .as_ref()
        .and_then(|fields| fields.ratarmount_bin.as_deref())
        .unwrap_or("ratarmount");
    let ratarmount_ready = command_check(ratarmount_name);
    let unmount_ready = command_check("fusermount3") || command_check("umount");
    let ready_for_scanning = config_valid && sources_ready;
    let ready_for_actions = ready_for_scanning
        && mount_root_ready
        && mount_root_writable
        && ratarmount_ready
        && unmount_ready;
    let mut checks = Vec::new();
    setup_check(
        &mut checks,
        "Config file exists",
        config_read_ok,
        config_read_detail,
        "ArchiveFS needs this file to locate archives and mounts.",
        "Create a starter config or create this file manually.",
    );
    setup_check(
        &mut checks,
        "Config parses successfully",
        fields.is_some(),
        parse_detail,
        "Invalid TOML prevents ArchiveFS from reading any settings.",
        "Open the config and correct the reported fields or syntax.",
    );
    setup_check(
        &mut checks,
        "At least one source folder is configured",
        source_folders
            .as_ref()
            .is_some_and(|sources| !sources.is_empty()),
        source_folders.as_ref().map_or_else(
            || "No usable source_folders setting is available.".to_string(),
            |sources| format!("{} source folder(s) configured.", sources.len()),
        ),
        "Source folders contain the archives ArchiveFS scans.",
        "Add at least one existing directory to source_folders.",
    );
    for (source, state) in &source_states {
        setup_check(
            &mut checks,
            "Configured source folder exists",
            state.is_directory(),
            state.error_detail().map_or_else(
                || format!("Source folder: {}", source.display()),
                |error| {
                    format!(
                        "Source folder {} cannot be inspected: {error}",
                        source.display()
                    )
                },
            ),
            "Unavailable source folders make library scans incomplete.",
            "Create the directory or update source_folders to the correct path.",
        );
    }
    setup_check(
        &mut checks,
        "mount_root is configured",
        mount_root.is_some(),
        mount_root.as_ref().map_or_else(
            || "No mount_root setting is available.".to_string(),
            |root| format!("Mount root: {}", root.display()),
        ),
        "ArchiveFS places read-only archive mounts below this directory.",
        "Set mount_root to a dedicated directory.",
    );
    setup_check_with_warning(
        &mut checks,
        "Mount root exists or can be created safely",
        mount_root_ready,
        can_create_mount_root,
        mount_root.as_ref().map_or_else(
            || "No mount root is configured.".to_string(),
            |root| {
                mount_root_state
                    .as_ref()
                    .and_then(PathInspection::error_detail)
                    .map_or_else(
                        || format!("Mount root: {}", root.display()),
                        |error| {
                            format!("Mount root {} cannot be inspected: {error}", root.display())
                        },
                    )
            },
        ),
        "Mount and unmount actions require a safe dedicated root.",
        "Use Create Mount Root when offered, or correct its parent path.",
    );
    setup_check_with_warning(
        &mut checks,
        "Mount root is writable",
        mount_root_writable,
        can_create_mount_root,
        mount_root.as_ref().map_or_else(
            || "No mount root is available to test.".to_string(),
            |root| format!("Writable directory required: {}", root.display()),
        ),
        "ArchiveFS must create mount-point directories below mount_root.",
        "Grant the current user write access or choose another mount_root.",
    );
    setup_check(
        &mut checks,
        "ratarmount is available",
        ratarmount_ready,
        if ratarmount_ready {
            format!("{ratarmount_name} is available.")
        } else {
            format!("{ratarmount_name} was not found.")
        },
        "ArchiveFS uses ratarmount to expose archive contents as read-only folders.",
        "Install ratarmount and ensure it is available on PATH, then refresh diagnostics.",
    );
    setup_check(
        &mut checks,
        "fusermount3 or umount is available",
        unmount_ready,
        if unmount_ready {
            "fusermount3 or umount is available.".to_string()
        } else {
            "Neither fusermount3 nor umount was found.".to_string()
        },
        "Without either tool, ArchiveFS cannot detach mounted archives.",
        "Install fusermount3 or provide umount on PATH, then refresh diagnostics.",
    );
    setup_check(
        &mut checks,
        "ArchiveFS is ready for scanning",
        ready_for_scanning,
        "Scanning requires a valid config and all configured source folders.".to_string(),
        "Archive scanning populates the library shown in the GUI.",
        "Resolve config and source-folder errors above.",
    );
    setup_check(
        &mut checks,
        "ArchiveFS is ready for mount/unmount actions",
        ready_for_actions,
        "Actions additionally require a writable mount root and system tools.".to_string(),
        "Mount and unmount controls are unsafe or unusable until these checks pass.",
        "Resolve mount-root and tool errors above.",
    );
    let identity = config_identity(&config_path, contents.as_deref());
    SetupDiagnostics {
        config_path: Some(config_path),
        config_path_error: None,
        config_missing,
        mount_root,
        can_create_mount_root,
        ready_for_scanning,
        ready_for_actions,
        config_identity: identity,
        checks,
    }
}

fn setup_check(
    checks: &mut Vec<SetupDiagnostic>,
    name: &str,
    ready: bool,
    detail: String,
    why_it_matters: &str,
    next_step: &str,
) {
    checks.push(SetupDiagnostic {
        name: name.to_string(),
        status: if ready {
            SetupDiagnosticStatus::Ready
        } else {
            SetupDiagnosticStatus::Error
        },
        detail,
        why_it_matters: why_it_matters.to_string(),
        next_step: next_step.to_string(),
    });
}

fn setup_check_with_warning(
    checks: &mut Vec<SetupDiagnostic>,
    name: &str,
    ready: bool,
    warning: bool,
    detail: String,
    why_it_matters: &str,
    next_step: &str,
) {
    checks.push(SetupDiagnostic {
        name: name.to_string(),
        status: if ready {
            SetupDiagnosticStatus::Ready
        } else if warning {
            SetupDiagnosticStatus::Warning
        } else {
            SetupDiagnosticStatus::Error
        },
        detail,
        why_it_matters: why_it_matters.to_string(),
        next_step: next_step.to_string(),
    });
}

fn directory_is_writable(path: &Path) -> bool {
    static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let probe = path.join(format!(
        ".archivefs-write-test-{}-{}",
        std::process::id(),
        PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => fs::remove_file(probe).is_ok(),
        Err(_) => false,
    }
}

pub fn create_starter_config_default() -> Result<PathBuf> {
    let path = default_config_path()?;
    create_starter_config(&path)?;
    Ok(path)
}

pub fn create_starter_config(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!("config path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    use std::io::Write;
    file.write_all(
        b"# ArchiveFS starter configuration\n# Replace these example paths with directories on your system.\nsource_folders = [\"/path/to/archives\"]\nmount_root = \"/path/to/archivefs-mounts\"\nratarmount_bin = \"ratarmount\"\n",
    )
    .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))
}

pub fn create_configured_mount_root_default() -> Result<PathBuf> {
    let config_path = default_config_path()?;
    let contents = fs::read_to_string(&config_path)
        .map_err(|source| ArchiveFsError::io(config_path.clone(), source))?;
    let root = parse_config_fields(&contents)?
        .mount_root
        .ok_or_else(|| ArchiveFsError::Config("missing mount_root".to_string()))?;
    create_mount_root(&root)?;
    Ok(root)
}

pub fn create_configured_mount_root(config: &Config) -> Result<()> {
    create_mount_root(&config.mount_root)
}

fn create_mount_root(root: &Path) -> Result<()> {
    match inspect_path(root) {
        PathInspection::Missing => {}
        PathInspection::Directory | PathInspection::Other => {
            return Err(ArchiveFsError::Config(format!(
                "mount root already exists: {}",
                root.display()
            )));
        }
        PathInspection::PermissionDenied(error) | PathInspection::MetadataError(error) => {
            return Err(ArchiveFsError::Config(format!(
                "mount root cannot be inspected safely at {}: {error}",
                root.display()
            )));
        }
    }
    let parent = root.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!("mount root has no parent: {}", root.display()))
    })?;
    if !inspect_path(parent).is_directory() {
        return Err(ArchiveFsError::Config(format!(
            "mount root parent is unavailable: {}",
            parent.display()
        )));
    }
    fs::create_dir(root).map_err(|source| ArchiveFsError::io(root.to_path_buf(), source))
}

/// Validates configuration without creating a missing mount root.
pub fn run_config_check_read_only(config_path: impl AsRef<Path>) -> ConfigCheckReport {
    run_config_check_with_mount_root_creation(config_path, false)
}

fn run_config_check_with_mount_root_creation(
    config_path: impl AsRef<Path>,
    create_mount_root: bool,
) -> ConfigCheckReport {
    let config_path = config_path.as_ref().to_path_buf();
    let mut report = ConfigCheckReport {
        config_path: config_path.clone(),
        checks: Vec::new(),
    };

    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => {
            report.pass(
                "config file exists",
                format!("found {}", config_path.display()),
            );
            contents
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            report.error(
                "config file exists",
                format!("missing {}", config_path.display()),
            );
            return report;
        }
        Err(error) => {
            report.error(
                "config file exists",
                format!("{} cannot be read: {error}", config_path.display()),
            );
            return report;
        }
    };

    let fields = match parse_config_fields(&contents) {
        Ok(fields) => {
            report.pass("config parses", "configuration syntax parsed successfully");
            fields
        }
        Err(error) => {
            report.error("config parses", error.to_string());
            return report;
        }
    };

    match &fields.source_folders {
        Some(source_folders) if source_folders.is_empty() => {
            report.error("source_folders not empty", "source_folders is empty");
        }
        Some(source_folders) => {
            report.pass(
                "source_folders not empty",
                format!("{} source folder(s) configured", source_folders.len()),
            );
            let mut seen = HashSet::<PathBuf>::new();
            for source in source_folders {
                let source = PathBuf::from(source);
                if !seen.insert(source.clone()) {
                    report.warn(
                        "duplicate source folder",
                        format!("{} is listed more than once", source.display()),
                    );
                }
                match inspect_path(&source) {
                    PathInspection::Directory => {
                        report.pass(
                            "source folder exists",
                            format!("{} exists", source.display()),
                        );
                        report.pass(
                            "source folder is directory",
                            format!("{} is a directory", source.display()),
                        );
                    }
                    PathInspection::Other => {
                        report.pass(
                            "source folder exists",
                            format!("{} exists", source.display()),
                        );
                        report.error(
                            "source folder is directory",
                            format!("{} exists but is not a directory", source.display()),
                        );
                    }
                    PathInspection::Missing => {
                        report.error(
                            "source folder exists",
                            format!("{} does not exist", source.display()),
                        );
                        report.error(
                            "source folder is directory",
                            format!("{} is not a directory", source.display()),
                        );
                    }
                    PathInspection::PermissionDenied(error)
                    | PathInspection::MetadataError(error) => {
                        report.error(
                            "source folder exists",
                            format!("{} cannot be inspected: {error}", source.display()),
                        );
                        report.error(
                            "source folder is directory",
                            format!("{} cannot be verified as a directory", source.display()),
                        );
                    }
                }
            }
        }
        None => {
            report.error("source_folders not empty", "missing source_folders");
        }
    }

    match &fields.mount_root {
        Some(mount_root) => {
            report.pass("mount_root set", format!("{}", mount_root.display()));
            match inspect_path(mount_root) {
                PathInspection::Directory => {
                    report.pass(
                        "mount_root exists",
                        format!("{} exists", mount_root.display()),
                    );
                    report.pass(
                        "mount_root is directory",
                        format!("{} is a directory", mount_root.display()),
                    );
                }
                PathInspection::Other => {
                    report.pass(
                        "mount_root exists",
                        format!("{} exists", mount_root.display()),
                    );
                    report.error(
                        "mount_root is directory",
                        format!("{} exists but is not a directory", mount_root.display()),
                    );
                }
                PathInspection::Missing if create_mount_root => {
                    match fs::create_dir_all(mount_root) {
                        Ok(()) => {
                            report.pass(
                                "mount_root exists",
                                format!("{} was created", mount_root.display()),
                            );
                            report.pass(
                                "mount_root is directory",
                                format!("{} is a directory", mount_root.display()),
                            );
                        }
                        Err(error) => {
                            report.error(
                                "mount_root exists",
                                format!("{} cannot be created: {error}", mount_root.display()),
                            );
                            report.error(
                                "mount_root is directory",
                                format!("{} is not a directory", mount_root.display()),
                            );
                        }
                    }
                }
                PathInspection::Missing
                    if mount_root
                        .parent()
                        .is_some_and(|parent| inspect_path(parent).is_directory()) =>
                {
                    report.warn(
                        "mount_root exists",
                        format!("{} does not exist but can be created", mount_root.display()),
                    );
                    report.warn(
                        "mount_root is directory",
                        format!(
                            "{} will be a directory after creation",
                            mount_root.display()
                        ),
                    );
                }
                PathInspection::Missing => {
                    report.error(
                        "mount_root exists",
                        format!(
                            "{} does not exist and its parent is unavailable",
                            mount_root.display()
                        ),
                    );
                    report.error(
                        "mount_root is directory",
                        format!("{} cannot be created safely", mount_root.display()),
                    );
                }
                PathInspection::PermissionDenied(error) | PathInspection::MetadataError(error) => {
                    report.error(
                        "mount_root exists",
                        format!("{} cannot be inspected: {error}", mount_root.display()),
                    );
                    report.error(
                        "mount_root is directory",
                        format!("{} cannot be verified as a directory", mount_root.display()),
                    );
                }
            }
        }
        None => {
            report.error("mount_root set", "missing mount_root");
        }
    }

    let ratarmount_bin = fields.ratarmount_bin.as_deref().unwrap_or("ratarmount");
    if command_available(ratarmount_bin) {
        report.pass(
            "ratarmount binary",
            format!("{ratarmount_bin} is available"),
        );
    } else {
        report.error(
            "ratarmount binary",
            format!("{ratarmount_bin} was not found"),
        );
    }

    if command_available("fusermount3") || command_available("umount") {
        report.pass("unmount tool", "fusermount3 or umount is available");
    } else {
        report.error("unmount tool", "neither fusermount3 nor umount was found");
    }

    report
}

pub fn default_config_path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("config.toml"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigFields {
    source_folders: Option<Vec<String>>,
    mount_root: Option<PathBuf>,
    ratarmount_bin: Option<String>,
}

pub fn parse_config(contents: &str) -> Result<Config> {
    let fields = parse_config_fields(contents)?;
    let source_folders = fields
        .source_folders
        .ok_or_else(|| ArchiveFsError::Config("missing source_folders".to_string()))?;
    if source_folders.is_empty() {
        return Err(ArchiveFsError::Config(
            "source_folders must contain at least one path".to_string(),
        ));
    }

    Ok(Config {
        source_folders: source_folders.into_iter().map(PathBuf::from).collect(),
        mount_root: fields
            .mount_root
            .ok_or_else(|| ArchiveFsError::Config("missing mount_root".to_string()))?,
        ratarmount_bin: fields
            .ratarmount_bin
            .unwrap_or_else(|| "ratarmount".to_string()),
    })
}

fn parse_config_fields(contents: &str) -> Result<ConfigFields> {
    let mut fields = ConfigFields {
        source_folders: None,
        mount_root: None,
        ratarmount_bin: None,
    };

    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line_number = i + 1;
        let line = strip_comment(lines[i]).trim();
        i += 1;
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} is not a key/value pair",
            )));
        };

        match key.trim() {
            "source_folders" | "sources" => {
                // An array value may open with '[' here and only close
                // with ']' on a later line - collect_array_text joins any
                // such continuation lines into one string first, so
                // parse_string_array always sees a complete, single-line
                // array exactly like it always has. Single-line arrays
                // (the common case) are returned unchanged with zero
                // lines consumed, so this is a no-op for existing configs.
                let (array_text, consumed) =
                    collect_array_text(value.trim(), &lines[i..], line_number)?;
                i += consumed;
                fields.source_folders = Some(parse_string_array(&array_text, line_number)?);
            }
            "mount_root" => {
                fields.mount_root = Some(PathBuf::from(parse_string(value.trim(), line_number)?));
            }
            "ratarmount_bin" | "ratarmount" => {
                fields.ratarmount_bin = Some(parse_string(value.trim(), line_number)?);
            }
            _ => {}
        }
    }

    Ok(fields)
}

/// If `first` opens an array with '[' but does not itself close it with a
/// matching ']', pulls further lines from `rest` (each comment-stripped;
/// blank lines skipped) and joins them onto `first` with a single space
/// until the array closes. Returns the joined text and how many lines
/// from `rest` were consumed (0 if `first` was already a complete,
/// single-line array or not an array at all - the common case, and
/// unchanged from parse_config_fields's prior single-line-only
/// behavior). `line_number` is only used for the "never closed" error.
///
/// This performs no comma handling of its own: it only re-joins physical
/// lines into one logical line, so parse_string_array (unchanged) parses
/// exactly the same comma/quote syntax it always has, just assembled
/// from more than one source line.
fn collect_array_text(first: &str, rest: &[&str], line_number: usize) -> Result<(String, usize)> {
    let mut text = first.to_string();
    if !text.starts_with('[') || array_is_balanced(&text) {
        return Ok((text, 0));
    }

    let mut consumed = 0;
    for raw in rest {
        consumed += 1;
        let cont = strip_comment(raw).trim();
        if cont.is_empty() {
            continue;
        }
        text.push(' ');
        text.push_str(cont);
        if array_is_balanced(&text) {
            return Ok((text, consumed));
        }
    }

    Err(ArchiveFsError::Config(format!(
        "line {line_number} starts an array with '[' that is never closed with ']'",
    )))
}

/// True if `text` contains a '[' ... ']' pair that balances back to depth
/// zero, ignoring any '[' or ']' that appear inside quoted strings.
fn array_is_balanced(text: &str) -> bool {
    let mut in_string = false;
    let mut previous_was_escape = false;
    let mut depth: i32 = 0;

    for ch in text.chars() {
        match ch {
            '"' if !previous_was_escape => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
        previous_was_escape = ch == '\\' && !previous_was_escape;
    }

    false
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut previous_was_escape = false;

    for (index, ch) in line.char_indices() {
        match ch {
            '"' if !previous_was_escape => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
        previous_was_escape = ch == '\\' && !previous_was_escape;
        if ch != '\\' {
            previous_was_escape = false;
        }
    }

    line
}

fn parse_string(value: &str, line_number: usize) -> Result<String> {
    let value = value.trim();
    if !value.starts_with('"') || !value.ends_with('"') || value.len() < 2 {
        return Err(ArchiveFsError::Config(format!(
            "line {line_number} expected a quoted string"
        )));
    }

    Ok(value[1..value.len() - 1]
        .replace("\\\"", "\"")
        .replace("\\\\", "\\"))
}

fn parse_string_array(value: &str, line_number: usize) -> Result<Vec<String>> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(ArchiveFsError::Config(format!(
            "line {line_number} expected an array of quoted strings"
        )));
    }

    let mut values = Vec::new();
    let mut rest = value[1..value.len() - 1].trim();
    while !rest.is_empty() {
        if !rest.starts_with('"') {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} expected a quoted string in array"
            )));
        }

        let mut end = None;
        let mut previous_was_escape = false;
        for (index, ch) in rest[1..].char_indices() {
            if ch == '"' && !previous_was_escape {
                end = Some(index + 1);
                break;
            }
            previous_was_escape = ch == '\\' && !previous_was_escape;
            if ch != '\\' {
                previous_was_escape = false;
            }
        }

        let Some(end) = end else {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} has an unterminated string"
            )));
        };

        values.push(parse_string(&rest[..=end], line_number)?);
        rest = rest[end + 1..].trim_start();
        if let Some(after_comma) = rest.strip_prefix(',') {
            rest = after_comma.trim_start();
        } else if !rest.is_empty() {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} expected ',' between array values"
            )));
        }
    }

    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    Zip,
    SevenZip,
    Rar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveHealth {
    Pending,
    Mounted,
    Failed,
    MissingParts,
    Corrupt,
    Unsupported,
    PermissionDenied,
    RetryAvailable,
}

impl ArchiveHealth {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::MissingParts | Self::RetryAvailable
        )
    }

    pub fn is_terminal_without_source_change(self) -> bool {
        matches!(
            self,
            Self::Corrupt | Self::Unsupported | Self::PermissionDenied
        )
    }
}

impl fmt::Display for ArchiveHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Mounted => write!(f, "Mounted"),
            Self::Failed => write!(f, "Failed"),
            Self::MissingParts => write!(f, "MissingParts"),
            Self::Corrupt => write!(f, "Corrupt"),
            Self::Unsupported => write!(f, "Unsupported"),
            Self::PermissionDenied => write!(f, "PermissionDenied"),
            Self::RetryAvailable => write!(f, "RetryAvailable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveIdentity {
    pub display_name: String,
    pub normalized_name: String,
    pub source_root: PathBuf,
    pub size_bytes: Option<u64>,
    pub modified_time: Option<std::time::SystemTime>,
    pub platform: Option<String>,
    /// How `platform` was determined, or `None` iff `platform` is `None`.
    /// Carried alongside `platform` (rather than recomputed later) so a
    /// single detection pass is the source of truth for both the display
    /// value and the provenance persisted with it - see
    /// [`Database::assign_platform`](crate::database::Database::assign_platform)
    /// and `detect_platform_with_provenance`.
    pub platform_provenance: Option<PlatformProvenance>,
    pub region: Option<String>,
    pub content_hash: Option<String>,
    pub archive_hash: Option<String>,
    pub internal_listing_hash: Option<String>,
}

impl ArchiveIdentity {
    pub fn from_path(
        path: &Path,
        source_root: impl Into<PathBuf>,
        metadata: Option<&fs::Metadata>,
    ) -> Self {
        let source_root = source_root.into();
        let detection = detect_platform_with_provenance(path, &source_root);
        let (platform, platform_provenance) = match detection {
            Some(detection) => (Some(detection.platform), Some(detection.provenance)),
            None => (None, None),
        };
        Self {
            display_name: archive_title(path),
            normalized_name: normalized_title(path),
            source_root,
            size_bytes: metadata.map(fs::Metadata::len),
            modified_time: metadata.and_then(|metadata| metadata.modified().ok()),
            platform,
            platform_provenance,
            region: None,
            content_hash: None,
            archive_hash: None,
            internal_listing_hash: None,
        }
    }

    fn path_fingerprint(&self, archive_path: &Path) -> String {
        let mut input = self.source_root.clone();
        input.push(archive_path);
        short_path_hash(&input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveMetadata {
    pub title: Option<String>,
    pub platform: Option<String>,
    pub region: Option<String>,
    pub languages: Option<Vec<String>>,
    pub version: Option<String>,
    pub disc: Option<String>,
    pub publisher: Option<String>,
    pub developer: Option<String>,
    pub release_year: Option<u16>,
    pub genre: Option<String>,
    pub notes: Option<String>,
    pub source: Option<String>,
}

impl ArchiveMetadata {
    fn empty() -> Self {
        Self {
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
        }
    }
}

pub trait MetadataProvider {
    fn metadata_for(&self, archive: &Archive) -> ArchiveMetadata;
}

pub trait HealthProvider {
    fn health_for(&self, archive: &Archive) -> ArchiveHealth;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilesystemHealthProvider;

impl HealthProvider for FilesystemHealthProvider {
    fn health_for(&self, archive: &Archive) -> ArchiveHealth {
        archive.health
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilenameMetadataProvider;

impl MetadataProvider for FilenameMetadataProvider {
    fn metadata_for(&self, archive: &Archive) -> ArchiveMetadata {
        let mut metadata = ArchiveMetadata::empty();
        metadata.title = Some(archive_title(&archive.path));
        metadata.platform = detect_platform(&archive.path, &archive.identity.source_root);
        metadata.region = archive.identity.region.clone();
        metadata
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub path: PathBuf,
    pub kind: ArchiveKind,
    pub identity: ArchiveIdentity,
    pub health: ArchiveHealth,
}

impl Archive {
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        Self::from_path_in_root(path, PathBuf::new())
    }

    pub fn from_path_in_root(
        path: impl AsRef<Path>,
        source_root: impl Into<PathBuf>,
    ) -> Option<Self> {
        let path = path.as_ref();
        let kind = archive_kind(path)?;
        let metadata = fs::metadata(path).ok();
        Some(Self {
            path: path.to_path_buf(),
            kind,
            identity: ArchiveIdentity::from_path(path, source_root, metadata.as_ref()),
            health: ArchiveHealth::Pending,
        })
    }
}

impl AsRef<Path> for Archive {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

pub fn archive_kind(path: impl AsRef<Path>) -> Option<ArchiveKind> {
    let filename = path.as_ref().file_name()?.to_string_lossy().to_lowercase();
    if should_skip_split_archive_part(&filename) {
        return None;
    }

    if filename.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if filename.ends_with(".7z") {
        Some(ArchiveKind::SevenZip)
    } else if filename.ends_with(".rar") {
        Some(ArchiveKind::Rar)
    } else {
        None
    }
}

pub fn is_supported_archive(path: impl AsRef<Path>) -> bool {
    archive_kind(path).is_some()
}

pub fn should_skip_split_archive_part(path: impl AsRef<Path>) -> bool {
    let Some(filename) = path.as_ref().file_name() else {
        return false;
    };
    let filename = filename.to_string_lossy().to_lowercase();

    if let Some(part_number) = rar_part_number(&filename) {
        return part_number != 1;
    }

    let Some(extension) = Path::new(filename.as_str()).extension() else {
        return false;
    };
    let extension = extension.to_string_lossy();
    extension.len() == 3
        && extension.starts_with('r')
        && extension[1..].chars().all(|ch| ch.is_ascii_digit())
}

fn rar_part_number(filename: &str) -> Option<u32> {
    let without_rar = filename.strip_suffix(".rar")?;
    let (_, part) = without_rar.rsplit_once(".part")?;
    if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MountState {
    Pending,
    Mounted,
    MountPathExists,
}

impl fmt::Display for MountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Mounted => write!(f, "Mounted"),
            Self::MountPathExists => write!(f, "MountPathExists"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    pub archive: Archive,
    pub mount_path: PathBuf,
    pub state: MountState,
}

impl MountPlan {
    pub fn new(archive: Archive, mount_path: PathBuf) -> Self {
        Self {
            archive,
            mount_path,
            state: MountState::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRecord {
    pub identity: ArchiveIdentity,
    pub metadata: ArchiveMetadata,
    pub mount_plan: MountPlan,
    pub health: ArchiveHealth,
    pub mount_state: MountState,
}

impl ArchiveRecord {
    pub fn new(
        mut mount_plan: MountPlan,
        mount_state: MountState,
        metadata: ArchiveMetadata,
        health: ArchiveHealth,
    ) -> Self {
        mount_plan.state = mount_state;
        Self {
            identity: mount_plan.archive.identity.clone(),
            metadata,
            health,
            mount_plan,
            mount_state,
        }
    }
}

pub trait DuplicateDetector {
    fn detect_duplicates(&self, records: &[ArchiveRecord]) -> Result<DuplicateReport>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateReport {
    pub detector: String,
    pub archives_checked: usize,
    pub entries: Vec<DuplicateEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateEntry {
    pub platform: String,
    pub severity: DuplicateSeverity,
    pub reason: String,
    pub archive_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DuplicateSeverity {
    Warning,
    Low,
    Medium,
    High,
}

impl fmt::Display for DuplicateSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Warning => write!(f, "Warning"),
            Self::Low => write!(f, "Low"),
            Self::Medium => write!(f, "Medium"),
            Self::High => write!(f, "High"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilenameDuplicateDetector;

impl DuplicateDetector for FilenameDuplicateDetector {
    fn detect_duplicates(&self, records: &[ArchiveRecord]) -> Result<DuplicateReport> {
        let mut groups = BTreeMap::<(String, String), Vec<PathBuf>>::new();

        for record in records {
            let platform = duplicate_record_platform(record);
            let name = safe_mount_name(&record.mount_plan.archive.path).to_lowercase();
            groups
                .entry((platform, name))
                .or_default()
                .push(record.mount_plan.archive.path.clone());
        }

        let entries = groups
            .into_iter()
            .filter_map(|((platform, name), archive_paths)| {
                if archive_paths.len() < 2 {
                    return None;
                }

                Some(DuplicateEntry {
                    platform: platform.clone(),
                    severity: DuplicateSeverity::Warning,
                    reason: format!(
                        "same normalized archive name '{name}' on platform '{platform}'"
                    ),
                    archive_paths,
                })
            })
            .collect();

        Ok(DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: records.len(),
            entries,
        })
    }
}

fn duplicate_record_platform(record: &ArchiveRecord) -> String {
    record
        .metadata
        .platform
        .clone()
        .or_else(|| record.identity.platform.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmptyDuplicateDetector;

impl DuplicateDetector for EmptyDuplicateDetector {
    fn detect_duplicates(&self, records: &[ArchiveRecord]) -> Result<DuplicateReport> {
        Ok(DuplicateReport {
            detector: "empty".to_string(),
            archives_checked: records.len(),
            entries: Vec::new(),
        })
    }
}

pub trait MountBackend {
    fn mount(&self, plan: &MountPlan) -> Result<()>;
    fn unmount(&self, mount_path: &Path) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountOneOutcome {
    Mounted(MountPlan),
    AlreadyMounted(MountPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnmountOneOutcome {
    Unmounted(MountPlan),
    NotMounted(MountPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountBatchTargetSkipReason {
    Selection(String),
    InvalidTarget(String),
    DuplicateResolvedTarget {
        resolved_mount_path: PathBuf,
        first_mount_path: PathBuf,
    },
    AlreadyMountedResolvedTarget {
        resolved_mount_path: PathBuf,
    },
}

impl fmt::Display for MountBatchTargetSkipReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selection(message) | Self::InvalidTarget(message) => formatter.write_str(message),
            Self::DuplicateResolvedTarget {
                resolved_mount_path,
                first_mount_path,
            } => write!(
                formatter,
                "duplicate target {} already reserved by {}",
                resolved_mount_path.display(),
                first_mount_path.display()
            ),
            Self::AlreadyMountedResolvedTarget {
                resolved_mount_path,
            } => write!(
                formatter,
                "resolved target {} is already mounted",
                resolved_mount_path.display()
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountBatchTargetValidation {
    Ready {
        archive_path: PathBuf,
        mount_path: PathBuf,
        resolved_mount_path: PathBuf,
    },
    Skipped {
        archive_path: PathBuf,
        mount_path: Option<PathBuf>,
        reason: MountBatchTargetSkipReason,
    },
}

impl MountBatchTargetValidation {
    pub fn archive_path(&self) -> &Path {
        match self {
            Self::Ready { archive_path, .. } | Self::Skipped { archive_path, .. } => archive_path,
        }
    }

    pub fn skip_reason(&self) -> Option<&MountBatchTargetSkipReason> {
        match self {
            Self::Ready { .. } => None,
            Self::Skipped { reason, .. } => Some(reason),
        }
    }
}

impl MountOneOutcome {
    pub fn plan(&self) -> &MountPlan {
        match self {
            Self::Mounted(plan) | Self::AlreadyMounted(plan) => plan,
        }
    }

    pub fn into_plan(self) -> MountPlan {
        match self {
            Self::Mounted(plan) | Self::AlreadyMounted(plan) => plan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyUnmountTool {
    Fusermount3,
    Umount,
}

impl fmt::Display for LazyUnmountTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fusermount3 => "fusermount3 -uz",
            Self::Umount => "umount -l",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LazyUnmountCleanupResult {
    Completed(Vec<PathBuf>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LazyUnmountResult {
    pub succeeded: bool,
    pub tool: LazyUnmountTool,
    pub mount_path: PathBuf,
    pub warning: Option<String>,
    pub cleanup: Option<LazyUnmountCleanupResult>,
}

trait LazyUnmountBackend {
    fn mounted_paths_under(&self, root: &Path) -> Result<HashSet<PathBuf>>;
    fn lazy_unmount(&self, mount_path: &Path) -> Result<LazyUnmountTool>;
}

struct SystemLazyUnmountBackend;

impl LazyUnmountBackend for SystemLazyUnmountBackend {
    fn mounted_paths_under(&self, root: &Path) -> Result<HashSet<PathBuf>> {
        mounted_paths_under(root)
    }

    fn lazy_unmount(&self, mount_path: &Path) -> Result<LazyUnmountTool> {
        lazy_unmount_path(mount_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatarmountBackend {
    ratarmount_bin: String,
}

impl RatarmountBackend {
    pub fn new(ratarmount_bin: impl Into<String>) -> Self {
        Self {
            ratarmount_bin: ratarmount_bin.into(),
        }
    }
}

pub struct ArchiveMountSession {
    config: Config,
    plans: Vec<MountPlan>,
    backend: RatarmountBackend,
}

pub struct ArchiveUnmountSession {
    config: Config,
    plans: Vec<MountPlan>,
    backend: RatarmountBackend,
}

impl ArchiveUnmountSession {
    /// Builds a reusable exact-path unmount session and validates batch-wide prerequisites.
    pub fn new(config: &Config) -> Result<Self> {
        fs::canonicalize(&config.mount_root)
            .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;
        current_mount_paths()?;
        let scanner = ArchiveScanner::new(config);
        Ok(Self {
            config: config.clone(),
            plans: scanner.mount_plans()?,
            backend: RatarmountBackend::new(config.ratarmount_bin.clone()),
        })
    }

    /// Normally unmounts one exact archive after revalidating its captured mount path.
    pub fn unmount_archive_path(
        &self,
        archive_path: &Path,
        expected_mount_path: &Path,
    ) -> Result<UnmountOneOutcome> {
        let plan = select_mount_plan_by_path(&self.plans, archive_path)?;
        if plan.mount_path != expected_mount_path {
            return Err(ArchiveFsError::Config(format!(
                "mount target changed after batch capture: expected {}, found {}",
                expected_mount_path.display(),
                plan.mount_path.display()
            )));
        }
        let active_mount_paths = current_mount_paths()?;
        let outcome = unmount_one_plan_outcome_with_active_mounts(
            &self.config,
            plan,
            &self.backend,
            &active_mount_paths,
        )?;
        if let UnmountOneOutcome::Unmounted(plan) = &outcome {
            ensure_mount_disappeared(&plan.mount_path, &current_mount_paths()?)?;
        }
        Ok(outcome)
    }
}

impl ArchiveMountSession {
    pub fn new(config: &Config) -> Result<Self> {
        let scanner = ArchiveScanner::new(config);
        Ok(Self {
            config: config.clone(),
            plans: scanner.mount_plans()?,
            backend: RatarmountBackend::new(config.ratarmount_bin.clone()),
        })
    }

    pub fn mount_archive_path(&self, archive_path: &Path) -> Result<MountOneOutcome> {
        let plan = select_mount_plan_by_path(&self.plans, archive_path)?;
        mount_one_plan_outcome(&self.config, plan, &self.backend)
    }

    pub fn validate_batch_targets(
        &self,
        archive_paths: &[PathBuf],
    ) -> Result<Vec<MountBatchTargetValidation>> {
        fs::create_dir_all(&self.config.mount_root)
            .map_err(|source| ArchiveFsError::io(self.config.mount_root.clone(), source))?;
        let active_mount_paths = current_mount_paths()?;
        Ok(validate_mount_batch_targets(
            &self.config,
            &self.plans,
            archive_paths,
            &active_mount_paths,
        ))
    }

    pub fn mount_validated_batch_target(
        &self,
        validation: &MountBatchTargetValidation,
    ) -> Result<MountOneOutcome> {
        let (archive_path, mount_path, expected_resolved_path) = match validation {
            MountBatchTargetValidation::Ready {
                archive_path,
                mount_path,
                resolved_mount_path,
            } => (archive_path, mount_path, resolved_mount_path),
            MountBatchTargetValidation::Skipped { reason, .. } => {
                return Err(ArchiveFsError::Config(format!(
                    "refusing to mount a skipped batch target: {reason}"
                )));
            }
        };
        let plan = select_mount_plan_by_path(&self.plans, archive_path)?;
        if plan.mount_path != *mount_path {
            return Err(ArchiveFsError::Config(format!(
                "mount target changed after batch validation: {}",
                plan.mount_path.display()
            )));
        }
        let resolved_path = resolved_mount_target(&self.config, &plan.mount_path)?;
        if resolved_path != *expected_resolved_path {
            return Err(ArchiveFsError::Config(format!(
                "resolved mount target changed after batch validation: {}",
                plan.mount_path.display()
            )));
        }
        mount_one_plan_outcome(&self.config, plan, &self.backend)
    }
}

impl MountBackend for RatarmountBackend {
    fn mount(&self, plan: &MountPlan) -> Result<()> {
        run_command(
            &self.ratarmount_bin,
            &[plan.archive.path.as_path(), plan.mount_path.as_path()],
        )
    }

    fn unmount(&self, mount_path: &Path) -> Result<()> {
        unmount_path(mount_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveStatus {
    pub archive_path: PathBuf,
    pub mount_path: PathBuf,
    pub state: MountState,
}

pub struct ArchiveScanner<'a> {
    config: &'a Config,
}

impl<'a> ArchiveScanner<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    pub fn scan_archives(&self) -> Result<Vec<Archive>> {
        info!(
            "starting archive scan across {} source folder(s)",
            self.config.source_folders.len()
        );
        let mut archives = Vec::new();
        for source in &self.config.source_folders {
            debug!("scanning source folder {}", source.display());
            self.scan_source(source, source, &mut archives)?;
        }
        archives.sort_by(|left, right| left.path.cmp(&right.path));
        archives.dedup_by(|left, right| left.path == right.path);
        info!("archive scan complete: {} archive(s) found", archives.len());
        Ok(archives)
    }

    pub fn mount_plans(&self) -> Result<Vec<MountPlan>> {
        let archives = self.scan_archives()?;
        Ok(plan_mounts(&archives, &self.config.mount_root))
    }

    pub fn archive_records(&self) -> Result<Vec<ArchiveRecord>> {
        let archives = self.scan_archives()?;
        self.archive_records_from_archives(archives)
    }

    fn archive_records_from_archives(&self, archives: Vec<Archive>) -> Result<Vec<ArchiveRecord>> {
        let metadata_provider = FilenameMetadataProvider;
        let health_provider = FilesystemHealthProvider;
        self.archive_records_from_archives_with_providers(
            archives,
            &metadata_provider,
            &health_provider,
        )
    }

    pub fn archive_records_with_providers(
        &self,
        metadata_provider: &impl MetadataProvider,
        health_provider: &impl HealthProvider,
    ) -> Result<Vec<ArchiveRecord>> {
        let archives = self.scan_archives()?;
        self.archive_records_from_archives_with_providers(
            archives,
            metadata_provider,
            health_provider,
        )
    }

    fn archive_records_from_archives_with_providers(
        &self,
        archives: Vec<Archive>,
        metadata_provider: &impl MetadataProvider,
        health_provider: &impl HealthProvider,
    ) -> Result<Vec<ArchiveRecord>> {
        let plans = plan_mounts(&archives, &self.config.mount_root);
        let mounted_paths = mounted_paths_under(&self.config.mount_root)?;
        Ok(records_from_plans(
            plans,
            &mounted_paths,
            metadata_provider,
            health_provider,
        ))
    }

    fn scan_source(
        &self,
        source_root: &Path,
        source: &Path,
        archives: &mut Vec<Archive>,
    ) -> Result<()> {
        let entries = fs::read_dir(source)
            .map_err(|source_error| ArchiveFsError::io(source.to_path_buf(), source_error))?;

        for entry in entries {
            let entry = entry
                .map_err(|source_error| ArchiveFsError::io(source.to_path_buf(), source_error))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source_error| ArchiveFsError::io(path.clone(), source_error))?;

            if file_type.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|name| is_container_directory(&name.to_string_lossy()))
                {
                    debug!(
                        "not descending into container directory {} - its contents are internal \
                         payload, not separate library archives",
                        path.display()
                    );
                    continue;
                }
                self.scan_source(source_root, &path, archives)?;
            } else if file_type.is_file()
                && let Some(archive) = Archive::from_path_in_root(&path, source_root)
            {
                debug!("discovered archive {}", archive.path.display());
                archives.push(archive);
            }
        }

        Ok(())
    }
}

/// Directory-name suffixes (case-insensitive) that mark a recognised
/// "container" or game-directory format: the directory as a whole
/// represents one release (the container directory itself is the game),
/// and everything nested inside it - at any depth - is internal payload
/// resources, not separate library content. Scanning does not descend
/// into a directory whose name matches one of these suffixes at all, so
/// nested archives below it (for example N-Gage's internal
/// `System/Apps/.../data.zip` resource files) are never emitted as
/// independent `Archive` records.
///
/// Deliberately a narrow, explicit allow-list rather than a rule that
/// skips every archive found inside any directory: ordinary nested
/// folders elsewhere are scanned exactly as before. Add a new suffix here
/// only for a genuinely recognised container/game-directory format, not
/// as a general-purpose exclusion mechanism.
const CONTAINER_DIRECTORY_SUFFIXES: &[&str] = &[".ngage"];

/// True if `name` (a directory's own file name, already lossily
/// stringified so a non-UTF-8 name is a safe non-match rather than a
/// panic) matches a recognised container/game-directory suffix,
/// case-insensitively.
fn is_container_directory(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CONTAINER_DIRECTORY_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

pub fn scan_archives(config: &Config) -> Result<Vec<Archive>> {
    ArchiveScanner::new(config).scan_archives()
}

pub fn plan_mounts(archives: &[Archive], mount_root: impl AsRef<Path>) -> Vec<MountPlan> {
    let mount_root = mount_root.as_ref();
    let mut base_counts = HashMap::<(String, String), usize>::new();
    for archive in archives {
        let platform_folder = platform_mount_folder(archive);
        *base_counts
            .entry((platform_folder, safe_mount_name(&archive.path)))
            .or_default() += 1;
    }

    let mut used = HashSet::<(String, String)>::new();
    archives
        .iter()
        .map(|archive| {
            let platform_folder = platform_mount_folder(archive);
            let base = safe_mount_name(&archive.path);
            let key = (platform_folder.clone(), base.clone());
            let mut name = if base_counts.get(&key).copied().unwrap_or(0) > 1 {
                format!(
                    "{base}--{}",
                    archive.identity.path_fingerprint(&archive.path)
                )
            } else {
                base
            };

            if used.contains(&(platform_folder.clone(), name.clone())) {
                name = format!(
                    "{name}--{}",
                    archive.identity.path_fingerprint(&archive.path)
                );
            }
            let mut suffix = 2;
            while used.contains(&(platform_folder.clone(), name.clone())) {
                name = format!("{}-{suffix}", safe_mount_name(&archive.path));
                suffix += 1;
            }
            used.insert((platform_folder.clone(), name.clone()));

            MountPlan::new(archive.clone(), mount_root.join(platform_folder).join(name))
        })
        .collect()
}

fn platform_mount_folder(archive: &Archive) -> String {
    archive
        .identity
        .platform
        .clone()
        .unwrap_or_else(|| "Unknown".to_string())
}

pub fn select_mount_plan(plans: &[MountPlan], input: &str) -> Result<MountPlan> {
    let exact_matches = plans
        .iter()
        .filter(|plan| Path::new(input) == plan.archive.path)
        .collect::<Vec<_>>();
    if !exact_matches.is_empty() {
        return single_mount_match(input, exact_matches);
    }

    let needle = input.to_lowercase();
    let display_name_matches = plans
        .iter()
        .filter(|plan| {
            plan.archive
                .identity
                .display_name
                .to_lowercase()
                .contains(&needle)
        })
        .collect::<Vec<_>>();
    if !display_name_matches.is_empty() {
        return single_mount_match(input, display_name_matches);
    }

    let safe_name_matches = plans
        .iter()
        .filter(|plan| {
            safe_mount_name(&plan.archive.path)
                .to_lowercase()
                .contains(&needle)
        })
        .collect::<Vec<_>>();
    if !safe_name_matches.is_empty() {
        return single_mount_match(input, safe_name_matches);
    }

    Err(ArchiveFsError::selection_no_match(input))
}

fn select_mount_plan_by_path(plans: &[MountPlan], archive_path: &Path) -> Result<MountPlan> {
    let input = archive_path.display().to_string();
    let matches = plans
        .iter()
        .filter(|plan| plan.archive.path == archive_path)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(ArchiveFsError::selection_no_match(input));
    }
    single_mount_match(&input, matches)
}

fn single_mount_match(input: &str, matches: Vec<&MountPlan>) -> Result<MountPlan> {
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }

    Err(ArchiveFsError::selection_ambiguous(
        input,
        matches
            .into_iter()
            .map(|plan| (plan.archive.path.clone(), plan.mount_path.clone()))
            .collect(),
    ))
}

pub fn safe_mount_name(path: impl AsRef<Path>) -> String {
    let base = archive_title(path.as_ref());
    let mut safe = String::new();
    let mut previous_was_separator = false;

    for ch in base.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            ch
        } else {
            '_'
        };

        if mapped == '_' {
            if !previous_was_separator {
                safe.push(mapped);
            }
            previous_was_separator = true;
        } else {
            safe.push(mapped);
            previous_was_separator = false;
        }
    }

    let safe = safe.trim_matches(['.', '-', '_']).to_string();
    if safe.is_empty() {
        "archive".to_string()
    } else {
        safe
    }
}

fn normalized_title(path: &Path) -> String {
    safe_mount_name(path).to_lowercase()
}

/// How a [`detect_platform_with_provenance`] result was determined. This is
/// what `platform_assignments.source` (see `database.rs`) is ultimately
/// derived from, and what decides whether a later, weaker guess is allowed
/// to overwrite an existing assignment - see
/// [`Database::assign_platform`](crate::database::Database::assign_platform).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformProvenance {
    /// The existing filename/title/known-path-segment heuristic below
    /// (`detect_platform_from_known_heuristics`) - unchanged from before
    /// this enum existed, and always tried first.
    Heuristic,
    /// The generic folder alias map (`FOLDER_PLATFORM_ALIASES`), used only
    /// as a fallback when the heuristic above finds nothing.
    FolderAlias,
}

impl PlatformProvenance {
    /// The exact string persisted as `platform_assignments.source`.
    /// `"heuristic-path-detector"` is unchanged from every row written
    /// before this enum existed; `"folder_alias"` is new.
    pub fn as_source_str(self) -> &'static str {
        match self {
            Self::Heuristic => "heuristic-path-detector",
            Self::FolderAlias => "folder_alias",
        }
    }
}

/// The result of [`detect_platform_with_provenance`]: a canonical platform
/// name plus how confidently/how it was determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformDetection {
    pub platform: String,
    pub provenance: PlatformProvenance,
}

/// Detects a platform for `path` (an archive discovered under
/// `source_root`), discarding provenance - the stable public entry point
/// every existing caller (`ArchiveIdentity::from_path`,
/// `FilenameMetadataProvider`) already used before folder-alias detection
/// existed. Prefer [`detect_platform_with_provenance`] for any new caller
/// that needs to know *how* confidently the platform was determined (for
/// example, database persistence deciding whether it is safe to overwrite
/// an existing assignment).
pub fn detect_platform(path: impl AsRef<Path>, source_root: impl AsRef<Path>) -> Option<String> {
    detect_platform_with_provenance(path, source_root).map(|detection| detection.platform)
}

/// Detects a platform for `path` (an archive discovered under
/// `source_root`), in priority order:
///
/// 1. The existing filename/title/known-path-segment heuristic
///    (`detect_platform_from_known_heuristics`) - unchanged, and always
///    tried first, since it is generally more specific than a bare folder
///    name.
/// 2. The generic folder alias map (`FOLDER_PLATFORM_ALIASES`), walking
///    from the archive's nearest containing directory up to (never beyond)
///    `source_root` - see `detect_platform_from_folder_alias`.
/// 3. `None` if neither found a confident match.
pub fn detect_platform_with_provenance(
    path: impl AsRef<Path>,
    source_root: impl AsRef<Path>,
) -> Option<PlatformDetection> {
    let path = path.as_ref();
    let source_root = source_root.as_ref();

    if let Some(platform) = detect_platform_from_known_heuristics(path, source_root) {
        return Some(PlatformDetection {
            platform,
            provenance: PlatformProvenance::Heuristic,
        });
    }

    detect_platform_from_folder_alias(path, source_root).map(|platform| PlatformDetection {
        platform: platform.to_string(),
        provenance: PlatformProvenance::FolderAlias,
    })
}

/// The original `detect_platform` heuristic, unchanged: a small set of
/// known path segments (Xbox/Xbox360/AtariST/Atari2600, matched with a
/// `starts_with` on the Xbox family to tolerate region/part suffixes like
/// `_f_part1`) and a short list of specific, hardcoded known-title
/// substring matches. Deliberately left as-is - see
/// `detect_platform_from_folder_alias` for the new, generic fallback.
fn detect_platform_from_known_heuristics(path: &Path, source_root: &Path) -> Option<String> {
    for segment in source_root.iter().chain(path.iter()) {
        let normalized = normalize_path_segment(&segment.to_string_lossy());
        if normalized.starts_with("microsoftxbox360") || normalized.starts_with("xbox360") {
            return Some("Xbox360".to_string());
        }
        if normalized.starts_with("microsoftxbox") || normalized.starts_with("xbox") {
            return Some("Xbox".to_string());
        }
        match normalized.as_str() {
            "atarist" => return Some("AtariST".to_string()),
            "a2600" | "atari2600" => return Some("Atari2600".to_string()),
            _ => {}
        }
    }

    let normalized_path = normalize_path_segment(&path.to_string_lossy());
    let normalized_root = normalize_path_segment(&source_root.to_string_lossy());
    let searchable = format!("{normalized_root}{normalized_path}");

    if searchable.contains("007legends") || searchable.contains("mortalkombatkompleteedition") {
        return Some("Xbox360".to_string());
    }
    if searchable.contains("fableusaeurope") {
        return Some("Xbox".to_string());
    }
    if searchable.contains("gameboyadvancecias") {
        return Some("Nintendo3DS".to_string());
    }
    if searchable.contains("iamjesuschrist") || searchable.contains("steamrip") {
        return Some("PC".to_string());
    }
    if searchable.contains("metalgearsolidpeacewalker") {
        return Some("PSP".to_string());
    }
    if searchable.contains("atari2600vcsromcollection") {
        return Some("Atari2600".to_string());
    }

    None
}

/// Canonical platform name for every folder alias this build recognizes,
/// keyed by the alias already run through `normalize_path_segment` (ASCII
/// alphanumeric only, lowercased - so separators and casing like
/// `"MSX 2"`/`"msx_2"`/`"msx2"` all key to the same `"msx2"` entry without
/// needing a separate row per spelling variant here). Exact match only,
/// deliberately: a substring or prefix match would risk false positives
/// like `"genesis".contains("nes")`. Keep entries specific and
/// unambiguous, avoiding single common English words that are not also
/// an explicitly requested platform alias.
const FOLDER_PLATFORM_ALIASES: &[(&str, &str)] = &[
    ("msx", "MSX"),
    ("msx1", "MSX"),
    ("msx2", "MSX2"),
    ("neogeo", "NeoGeo"),
    ("neogeoaes", "NeoGeo"),
    ("neogeomvs", "NeoGeo"),
    ("neogeo64", "NeoGeo64"),
    ("ngage", "NGage"),
    ("nokiangage", "NGage"),
    ("intellivision", "Intellivision"),
    ("amiga", "Amiga"),
    ("commodoreamiga", "Amiga"),
    ("amigacd32", "AmigaCD32"),
    ("cd32", "AmigaCD32"),
    ("atarist", "AtariST"),
    ("atari2600", "Atari2600"),
    ("a2600", "Atari2600"),
    ("atarivcs", "Atari2600"),
    ("atari5200", "Atari5200"),
    ("a5200", "Atari5200"),
    ("atari7800", "Atari7800"),
    ("a7800", "Atari7800"),
    ("nes", "NES"),
    ("nintendoentertainmentsystem", "NES"),
    ("famicom", "NES"),
    ("nintendofamicom", "NES"),
    ("snes", "SNES"),
    ("supernintendo", "SNES"),
    ("supernintendoentertainmentsystem", "SNES"),
    ("superfamicom", "SNES"),
    ("n64", "N64"),
    ("nintendo64", "N64"),
    ("gamecube", "GameCube"),
    ("nintendogamecube", "GameCube"),
    ("gcn", "GameCube"),
    ("ngc", "GameCube"),
    ("wii", "Wii"),
    ("nintendowii", "Wii"),
    ("wiiu", "WiiU"),
    ("nintendowiiu", "WiiU"),
    ("switch", "Switch"),
    ("nintendoswitch", "Switch"),
    ("megadrive", "MegaDrive"),
    ("genesis", "MegaDrive"),
    ("segamegadrive", "MegaDrive"),
    ("segagenesis", "MegaDrive"),
    ("smd", "MegaDrive"),
    ("mastersystem", "MasterSystem"),
    ("segamastersystem", "MasterSystem"),
    ("sms", "MasterSystem"),
    ("gamegear", "GameGear"),
    ("segagamegear", "GameGear"),
    ("saturn", "Saturn"),
    ("segasaturn", "Saturn"),
    ("dreamcast", "Dreamcast"),
    ("segadreamcast", "Dreamcast"),
    ("psx", "PSX"),
    ("ps1", "PSX"),
    ("playstation", "PSX"),
    ("playstation1", "PSX"),
    ("sonyplaystation", "PSX"),
    ("sonyplaystation1", "PSX"),
    ("ps2", "PS2"),
    ("playstation2", "PS2"),
    ("sonyplaystation2", "PS2"),
    ("ps3", "PS3"),
    ("playstation3", "PS3"),
    ("sonyplaystation3", "PS3"),
    ("psp", "PSP"),
    ("playstationportable", "PSP"),
    ("sonypsp", "PSP"),
    ("xbox", "Xbox"),
    ("microsoftxbox", "Xbox"),
    ("xbox360", "Xbox360"),
    ("microsoftxbox360", "Xbox360"),
    ("arcade", "Arcade"),
    ("mame", "Arcade"),
    ("dos", "DOS"),
    ("msdos", "DOS"),
    ("scummvm", "ScummVM"),
];

/// Every canonical platform name this build recognises via the
/// folder-alias system (`FOLDER_PLATFORM_ALIASES`), deduplicated and
/// sorted. This is the single source of truth for "known platform" used
/// by manual platform assignment (`Database::set_manual_platform`) and
/// its CLI/GUI callers - neither the CLI nor the GUI maintains a second,
/// independently-drifting platform list. Does not include the small set
/// of platform names only ever produced by the filename/title heuristic
/// in `detect_platform_from_known_heuristics` (for example `"PC"` and
/// `"Nintendo3DS"`) - those are ad hoc title matches, not part of the
/// structured alias table this function draws from.
pub fn canonical_platform_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = FOLDER_PLATFORM_ALIASES
        .iter()
        .map(|(_, canonical)| *canonical)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Canonical platform name for one already-lossy-stringified path
/// component, if it exactly matches a known folder alias after
/// normalization, or `None` if it does not.
fn folder_platform_alias(segment: &str) -> Option<&'static str> {
    let normalized = normalize_path_segment(segment);
    FOLDER_PLATFORM_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, canonical)| *canonical)
}

/// Infers a platform from `path`'s folder structure alone, walking
/// directory components from the archive's nearest containing folder
/// upward to (but never beyond) `source_root` - the nearest matching
/// folder wins. Only components strictly inside `source_root` ever
/// participate: `source_root`'s own components (`/home/davedap/Archives`
/// in the example from the platform-detection task) never do, and neither
/// does anything outside `source_root` altogether. The archive's own
/// filename is excluded too - this only ever looks at directory names.
///
/// Uses `to_string_lossy` on each component (matching
/// `detect_platform_from_known_heuristics`'s existing convention) - this
/// is a best-effort display guess, not an identity or reconciliation key,
/// so a lossy conversion on a non-UTF-8 path component is safe and simply
/// yields no match rather than panicking.
fn detect_platform_from_folder_alias(path: &Path, source_root: &Path) -> Option<&'static str> {
    let relative = path.strip_prefix(source_root).ok()?;
    let mut components: Vec<_> = relative.components().collect();
    components.pop(); // the archive's own filename never counts as a folder.

    components
        .iter()
        .rev()
        .find_map(|component| folder_platform_alias(&component.as_os_str().to_string_lossy()))
}

fn normalize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn archive_title(path: &Path) -> String {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive".to_string());
    let lower = filename.to_lowercase();

    if let Some(part_number) = rar_part_number(&lower)
        && part_number == 1
    {
        let suffix_len = ".part1.rar".len();
        let part_digits = lower
            .strip_suffix(".rar")
            .and_then(|name| name.rsplit_once(".part"))
            .map(|(_, digits)| digits.len())
            .unwrap_or(1);
        return filename[..filename.len() - suffix_len + 1 - part_digits].to_string();
    }

    for extension in [".zip", ".7z", ".rar"] {
        if lower.ends_with(extension) {
            return filename[..filename.len() - extension.len()].to_string();
        }
    }

    filename
}

fn short_path_hash(path: &Path) -> String {
    let mut hasher = FnvHasher::default();
    path.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

#[derive(Default)]
struct FnvHasher(u64);

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIndexEntry {
    pub archive_path: PathBuf,
    pub platform: Option<String>,
    pub display_name: String,
    pub mount_path: PathBuf,
    pub modified_time_seconds: Option<u64>,
    pub health: ArchiveHealth,
    pub mount_state: MountState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIndex {
    pub archives: Vec<ArchiveIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIndexSummary {
    pub archives_count: usize,
    pub platform_counts: Vec<(String, usize)>,
    pub mounted_count: usize,
    pub pending_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveSizeSummary {
    pub archive_path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveStats {
    pub total_archives: usize,
    pub mounted_count: usize,
    pub pending_count: usize,
    pub platform_counts: Vec<(String, usize)>,
    pub extension_counts: Vec<(String, usize)>,
    pub largest_archive: Option<ArchiveSizeSummary>,
    pub smallest_archive: Option<ArchiveSizeSummary>,
    pub total_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSnapshot {
    pub mount_root: PathBuf,
    pub records: Vec<ArchiveRecord>,
    pub stats: ArchiveStats,
    pub statuses: Vec<ArchiveStatus>,
    pub doctor: DoctorReport,
    pub config_identity: ConfigIdentity,
}

impl Serialize for ArchiveStats {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ArchiveStats", 8)?;
        state.serialize_field("total_archives", &self.total_archives)?;
        state.serialize_field("mounted_count", &self.mounted_count)?;
        state.serialize_field("pending_count", &self.pending_count)?;
        state.serialize_field("platform_counts", &CountMap(&self.platform_counts))?;
        state.serialize_field("extension_counts", &CountMap(&self.extension_counts))?;
        state.serialize_field("largest_archive", &self.largest_archive)?;
        state.serialize_field("smallest_archive", &self.smallest_archive)?;
        state.serialize_field("total_size_bytes", &self.total_size_bytes)?;
        state.end()
    }
}

struct CountMap<'a>(&'a [(String, usize)]);

impl Serialize for CountMap<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveInfo {
    pub title: String,
    pub platform: Option<String>,
    pub archive_path: PathBuf,
    pub mount_path: PathBuf,
    pub extension: String,
    pub size_bytes: Option<u64>,
    pub modified_time: Option<std::time::SystemTime>,
    pub health: ArchiveHealth,
    pub mount_state: MountState,
    pub metadata_provider: String,
    pub health_provider: String,
}

impl Serialize for ArchiveInfo {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let modified_time = self
            .modified_time
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        let mut state = serializer.serialize_struct("ArchiveInfo", 11)?;
        state.serialize_field("title", &self.title)?;
        state.serialize_field("platform", &self.platform)?;
        state.serialize_field("archive_path", &self.archive_path)?;
        state.serialize_field("mount_path", &self.mount_path)?;
        state.serialize_field("extension", &self.extension)?;
        state.serialize_field("size_bytes", &self.size_bytes)?;
        state.serialize_field("modified_time", &modified_time)?;
        state.serialize_field("health", &self.health.to_string())?;
        state.serialize_field("mount_state", &self.mount_state.to_string())?;
        state.serialize_field("metadata_provider", &self.metadata_provider)?;
        state.serialize_field("health_provider", &self.health_provider)?;
        state.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIndexFreshness {
    pub missing_archive_paths: Vec<PathBuf>,
    pub stale_archive_paths: Vec<PathBuf>,
}

impl ArchiveIndexFreshness {
    pub fn has_warnings(&self) -> bool {
        !self.missing_archive_paths.is_empty() || !self.stale_archive_paths.is_empty()
    }
}

pub fn default_index_path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Index("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("archivefs")
        .join("index.json"))
}

pub fn build_archive_index(config: &Config) -> Result<ArchiveIndex> {
    debug!("building archive index records");
    Ok(ArchiveIndex {
        archives: current_archive_records(config)?
            .into_iter()
            .map(archive_index_entry_from_record)
            .collect(),
    })
}

pub fn write_archive_index(index: &ArchiveIndex, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
    }
    fs::write(path, archive_index_to_json(index))
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))
}

pub fn build_and_write_archive_index(config: &Config) -> Result<ArchiveIndex> {
    let index_path = default_index_path()?;
    info!("starting index rebuild");
    debug!("index path {}", index_path.display());
    let index = build_archive_index(config)?;
    write_archive_index(&index, &index_path)?;
    info!(
        "index rebuild complete: {} archive(s) written to {}",
        index.archives.len(),
        index_path.display()
    );
    Ok(index)
}

pub const WATCH_DEBOUNCE_DURATION: Duration = Duration::from_secs(5);
const WATCH_STATS_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRebuildSummary {
    pub archive_event_count: usize,
    pub changed_paths: Vec<PathBuf>,
}

pub fn watch_archive_index(
    config: &Config,
    mut on_started: impl FnMut(),
    mut on_rebuilt: impl FnMut(&ArchiveIndex, &WatchRebuildSummary),
) -> Result<()> {
    use notify::{RecursiveMode, Watcher};

    info!(
        "starting watcher for {} source folder(s)",
        config.source_folders.len()
    );
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| ArchiveFsError::Watcher(error.to_string()))?;

    for source in &config.source_folders {
        debug!("watching source folder {}", source.display());
        watcher
            .watch(source, RecursiveMode::Recursive)
            .map_err(|error| ArchiveFsError::Watcher(error.to_string()))?;
    }

    on_started();

    let mut debouncer = WatchDebouncer::new(WATCH_DEBOUNCE_DURATION);
    let mut stats = WatchStats::new(WATCH_STATS_INTERVAL);
    loop {
        match debouncer.recv_timeout() {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(event) => {
                    handle_watch_event(event, &mut debouncer, &mut stats)?;
                    stats.log_if_due(Instant::now(), &debouncer);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if debouncer.should_fire(now) {
                        let summary = debouncer.take_summary();
                        let index = build_and_write_archive_index(config)?;
                        on_rebuilt(&index, &summary);
                        stats.record_rebuild();
                        stats.log_if_due(now, &debouncer);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ArchiveFsError::Watcher(
                        "filesystem watcher channel disconnected".to_string(),
                    ));
                }
            },
            None => match receiver.recv_timeout(WATCH_STATS_INTERVAL) {
                Ok(event) => {
                    handle_watch_event(event, &mut debouncer, &mut stats)?;
                    stats.log_if_due(Instant::now(), &debouncer);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    stats.log_if_due(Instant::now(), &debouncer);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ArchiveFsError::Watcher(
                        "filesystem watcher channel disconnected".to_string(),
                    ));
                }
            },
        }
    }
}

fn handle_watch_event(
    event: std::result::Result<notify::Event, notify::Error>,
    debouncer: &mut WatchDebouncer,
    stats: &mut WatchStats,
) -> Result<()> {
    let event = event.map_err(|error| ArchiveFsError::Watcher(error.to_string()))?;
    stats.record_received();
    let archive_paths = watch_event_archive_paths(&event);
    if !archive_paths.is_empty() {
        info!(
            "watcher accepted archive event: {:?} for {} path(s)",
            event.kind,
            archive_paths.len()
        );
        for path in &archive_paths {
            debug!("watcher accepted path {}", path.display());
        }
        debouncer.record_change(Instant::now(), archive_paths);
        stats.record_accepted();
    } else {
        stats.record_ignored();
    }
    Ok(())
}

#[cfg(test)]
fn watch_event_should_rebuild(event: &notify::Event) -> bool {
    !watch_event_archive_paths(event).is_empty()
}

fn watch_event_archive_paths(event: &notify::Event) -> Vec<PathBuf> {
    if !watch_event_kind_can_change_archive(&event.kind) {
        return Vec::new();
    }

    event
        .paths
        .iter()
        .filter(|path| watch_path_is_supported_archive(path))
        .cloned()
        .collect()
}

fn watch_event_kind_can_change_archive(kind: &notify::EventKind) -> bool {
    use notify::event::{AccessKind, AccessMode, CreateKind, EventKind, RemoveKind};

    matches!(
        kind,
        EventKind::Create(CreateKind::Any | CreateKind::File | CreateKind::Other)
            | EventKind::Modify(_)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Other)
            | EventKind::Access(AccessKind::Close(AccessMode::Write | AccessMode::Any))
    )
}

fn watch_path_is_supported_archive(path: &Path) -> bool {
    if path.is_dir() {
        return false;
    }

    if is_temporary_or_incomplete_path(path) {
        return false;
    }

    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    matches!(
        extension.to_ascii_lowercase().as_str(),
        "zip" | "rar" | "7z" | "iso"
    )
}

pub fn is_temporary_or_incomplete_path(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = filename.to_ascii_lowercase();

    if lower.ends_with('~') {
        return true;
    }

    let temporary_suffixes = [
        ".part",
        ".partial",
        ".crdownload",
        ".download",
        ".tmp",
        ".temp",
        ".!qb",
        ".aria2",
    ];
    temporary_suffixes
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

#[derive(Debug, Clone)]
struct WatchStats {
    interval: Duration,
    last_logged: Instant,
    events_received: usize,
    ignored: usize,
    accepted: usize,
    rebuilds: usize,
}

impl WatchStats {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_logged: Instant::now(),
            events_received: 0,
            ignored: 0,
            accepted: 0,
            rebuilds: 0,
        }
    }

    fn record_received(&mut self) {
        self.events_received += 1;
    }

    fn record_ignored(&mut self) {
        self.ignored += 1;
    }

    fn record_accepted(&mut self) {
        self.accepted += 1;
    }

    fn record_rebuild(&mut self) {
        self.rebuilds += 1;
    }

    fn log_if_due(&mut self, now: Instant, debouncer: &WatchDebouncer) {
        if now.duration_since(self.last_logged) < self.interval {
            return;
        }

        debug!(
            "Watcher statistics\n------------------\nEvents received: {}\nIgnored: {}\nAccepted: {}\nRebuilds: {}\nCurrent debounce queue: {}",
            self.events_received,
            self.ignored,
            self.accepted,
            self.rebuilds,
            debouncer.queue_len()
        );
        self.last_logged = now;
    }
}

#[derive(Debug, Clone)]
struct WatchDebouncer {
    debounce: Duration,
    last_change: Option<Instant>,
    archive_event_count: usize,
    changed_paths: Vec<PathBuf>,
}

impl WatchDebouncer {
    fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            last_change: None,
            archive_event_count: 0,
            changed_paths: Vec::new(),
        }
    }

    fn record_change(&mut self, now: Instant, changed_paths: Vec<PathBuf>) {
        self.last_change = Some(now);
        self.archive_event_count += 1;
        for path in changed_paths {
            if self.changed_paths.len() >= 5 {
                break;
            }
            if !self.changed_paths.contains(&path) {
                self.changed_paths.push(path);
            }
        }
    }

    fn queue_len(&self) -> usize {
        self.archive_event_count
    }

    fn should_fire(&self, now: Instant) -> bool {
        self.last_change
            .is_some_and(|last_change| now.duration_since(last_change) >= self.debounce)
    }

    fn take_summary(&mut self) -> WatchRebuildSummary {
        let summary = WatchRebuildSummary {
            archive_event_count: self.archive_event_count,
            changed_paths: std::mem::take(&mut self.changed_paths),
        };
        self.last_change = None;
        self.archive_event_count = 0;
        summary
    }

    #[cfg(test)]
    fn mark_fired(&mut self) {
        let _ = self.take_summary();
    }

    fn recv_timeout(&self) -> Option<Duration> {
        let last_change = self.last_change?;
        let elapsed = Instant::now().duration_since(last_change);
        Some(self.debounce.saturating_sub(elapsed))
    }
}

pub fn read_archive_index(path: impl AsRef<Path>) -> Result<ArchiveIndex> {
    let path = path.as_ref();
    let json = fs::read_to_string(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    Ok(parse_archive_index_json(&json))
}

pub fn read_default_archive_index() -> Result<ArchiveIndex> {
    read_archive_index(default_index_path()?)
}

pub fn find_archive_index_entries(index: &ArchiveIndex, query: &str) -> Vec<ArchiveIndexEntry> {
    let needle = query.to_lowercase();
    index
        .archives
        .iter()
        .filter(|entry| {
            entry
                .archive_path
                .display()
                .to_string()
                .to_lowercase()
                .contains(&needle)
                || entry.display_name.to_lowercase().contains(&needle)
                || entry
                    .platform
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
                || entry
                    .mount_path
                    .display()
                    .to_string()
                    .to_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect()
}

pub fn find_default_archive_index_entries(query: &str) -> Result<Vec<ArchiveIndexEntry>> {
    let index = read_default_archive_index()?;
    Ok(find_archive_index_entries(&index, query))
}

pub fn summarize_archive_records(records: &[ArchiveRecord]) -> ArchiveStats {
    let mut platform_counts = BTreeMap::<String, usize>::new();
    let mut extension_counts = BTreeMap::<String, usize>::new();
    let mut mounted_count = 0;
    let mut pending_count = 0;
    let mut largest_archive = None::<ArchiveSizeSummary>;
    let mut smallest_archive = None::<ArchiveSizeSummary>;
    let mut total_size_bytes = 0;

    for record in records {
        *(platform_counts
            .entry(
                record
                    .metadata
                    .platform
                    .clone()
                    .or_else(|| record.identity.platform.clone())
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .or_default()) += 1;

        let extension = record
            .mount_plan
            .archive
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string());
        *extension_counts.entry(extension).or_default() += 1;

        match record.mount_state {
            MountState::Mounted => mounted_count += 1,
            MountState::Pending => pending_count += 1,
            MountState::MountPathExists => {}
        }

        if let Some(size_bytes) = record.identity.size_bytes {
            total_size_bytes += size_bytes;
            let size_summary = ArchiveSizeSummary {
                archive_path: record.mount_plan.archive.path.clone(),
                size_bytes,
            };
            if largest_archive
                .as_ref()
                .is_none_or(|largest| size_bytes > largest.size_bytes)
            {
                largest_archive = Some(size_summary.clone());
            }
            if smallest_archive
                .as_ref()
                .is_none_or(|smallest| size_bytes < smallest.size_bytes)
            {
                smallest_archive = Some(size_summary);
            }
        }
    }

    ArchiveStats {
        total_archives: records.len(),
        mounted_count,
        pending_count,
        platform_counts: platform_counts.into_iter().collect(),
        extension_counts: extension_counts.into_iter().collect(),
        largest_archive,
        smallest_archive,
        total_size_bytes,
    }
}

pub fn current_archive_stats(config: &Config) -> Result<ArchiveStats> {
    let scanner = ArchiveScanner::new(config);
    let records = scanner.archive_records()?;
    Ok(summarize_archive_records(&records))
}

/// Loads one read-only view of the configured library for desktop frontends.
pub fn load_read_only_snapshot_default() -> Result<ArchiveSnapshot> {
    load_read_only_snapshot(default_config_path()?)
}

/// Loads one read-only view of a library without creating mount directories.
pub fn load_read_only_snapshot(config_path: impl AsRef<Path>) -> Result<ArchiveSnapshot> {
    let config_path = config_path.as_ref().to_path_buf();
    let contents = fs::read_to_string(&config_path)
        .map_err(|source| ArchiveFsError::io(config_path.clone(), source))?;
    let config = parse_config(&contents)?;
    let identity = config_identity(&config_path, Some(&contents));
    let records = current_archive_records(&config)?;
    let stats = summarize_archive_records(&records);
    let statuses = archive_statuses_from_records(&records);
    let mut doctor = empty_doctor_report(config_path.clone());
    doctor.pass("config file", format!("found {}", config_path.display()));
    doctor.pass("config parses", "configuration parsed successfully");
    complete_doctor_report(&mut doctor, &config, false, Some((&records, &statuses)));

    Ok(ArchiveSnapshot {
        mount_root: config.mount_root.clone(),
        records,
        stats,
        statuses,
        doctor,
        config_identity: identity,
    })
}

pub fn select_archive_record(records: &[ArchiveRecord], input: &str) -> Result<ArchiveRecord> {
    let plans = records
        .iter()
        .map(|record| record.mount_plan.clone())
        .collect::<Vec<_>>();
    let selected = select_mount_plan(&plans, input)?;
    records
        .iter()
        .find(|record| {
            record.mount_plan.archive.path == selected.archive.path
                && record.mount_plan.mount_path == selected.mount_path
        })
        .cloned()
        .ok_or_else(|| ArchiveFsError::selection_no_match(input))
}

pub fn archive_info_from_record(record: ArchiveRecord) -> ArchiveInfo {
    let extension = record
        .mount_plan
        .archive
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string());

    ArchiveInfo {
        title: record
            .metadata
            .title
            .unwrap_or(record.identity.display_name),
        platform: record.metadata.platform.or(record.identity.platform),
        archive_path: record.mount_plan.archive.path,
        mount_path: record.mount_plan.mount_path,
        extension,
        size_bytes: record.identity.size_bytes,
        modified_time: record.identity.modified_time,
        health: record.health,
        mount_state: record.mount_state,
        metadata_provider: "FilenameMetadataProvider".to_string(),
        health_provider: "FilesystemHealthProvider".to_string(),
    }
}

pub fn current_archive_info(config: &Config, input: &str) -> Result<ArchiveInfo> {
    let scanner = ArchiveScanner::new(config);
    let records = scanner.archive_records()?;
    let record = select_archive_record(&records, input)?;
    Ok(archive_info_from_record(record))
}

pub fn summarize_archive_index(index: &ArchiveIndex) -> ArchiveIndexSummary {
    let mut mounted_count = 0;
    let mut pending_count = 0;
    let mut platform_counts = BTreeMap::<String, usize>::new();

    for entry in &index.archives {
        *platform_counts
            .entry(
                entry
                    .platform
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
            )
            .or_default() += 1;
        match entry.mount_state {
            MountState::Mounted => mounted_count += 1,
            MountState::Pending => pending_count += 1,
            MountState::MountPathExists => {}
        }
    }

    ArchiveIndexSummary {
        archives_count: index.archives.len(),
        platform_counts: platform_counts.into_iter().collect(),
        mounted_count,
        pending_count,
    }
}

pub fn check_archive_index_freshness(index: &ArchiveIndex) -> ArchiveIndexFreshness {
    let mut missing_archive_paths = Vec::new();
    let mut stale_archive_paths = Vec::new();

    for entry in &index.archives {
        let Ok(metadata) = fs::metadata(&entry.archive_path) else {
            missing_archive_paths.push(entry.archive_path.clone());
            continue;
        };
        let current_modified_time = metadata.modified().ok().and_then(system_time_seconds);
        if entry.modified_time_seconds != current_modified_time {
            stale_archive_paths.push(entry.archive_path.clone());
        }
    }

    ArchiveIndexFreshness {
        missing_archive_paths,
        stale_archive_paths,
    }
}

fn system_time_seconds(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

pub fn read_archive_index_summary(path: impl AsRef<Path>) -> Result<ArchiveIndexSummary> {
    let index = read_archive_index(path)?;
    Ok(summarize_archive_index(&index))
}

pub fn read_default_archive_index_summary() -> Result<ArchiveIndexSummary> {
    read_archive_index_summary(default_index_path()?)
}

fn parse_archive_index_json(json: &str) -> ArchiveIndex {
    ArchiveIndex {
        archives: json
            .split("\n    {")
            .skip(1)
            .filter_map(parse_archive_index_entry)
            .collect(),
    }
}

fn parse_archive_index_entry(object: &str) -> Option<ArchiveIndexEntry> {
    Some(ArchiveIndexEntry {
        archive_path: PathBuf::from(extract_json_string_field(object, "archive_path")?),
        platform: extract_json_string_field(object, "platform"),
        display_name: extract_json_string_field(object, "display_name")?,
        mount_path: PathBuf::from(extract_json_string_field(object, "mount_path")?),
        modified_time_seconds: extract_json_number_field(object, "modified_time"),
        health: parse_archive_health(&extract_json_string_field(object, "health")?),
        mount_state: parse_mount_state(&extract_json_string_field(object, "mount_state")?),
    })
}

fn parse_archive_health(value: &str) -> ArchiveHealth {
    match value {
        "Mounted" => ArchiveHealth::Mounted,
        "Failed" => ArchiveHealth::Failed,
        "MissingParts" => ArchiveHealth::MissingParts,
        "Corrupt" => ArchiveHealth::Corrupt,
        "Unsupported" => ArchiveHealth::Unsupported,
        "PermissionDenied" => ArchiveHealth::PermissionDenied,
        "RetryAvailable" => ArchiveHealth::RetryAvailable,
        _ => ArchiveHealth::Pending,
    }
}

fn parse_mount_state(value: &str) -> MountState {
    match value {
        "Mounted" => MountState::Mounted,
        "MountPathExists" => MountState::MountPathExists,
        _ => MountState::Pending,
    }
}

fn archive_index_to_json(index: &ArchiveIndex) -> String {
    let mut json = String::from("{\n  \"archives\": [\n");
    for (idx, entry) in index.archives.iter().enumerate() {
        if idx > 0 {
            json.push_str(",\n");
        }
        json.push_str("    {\n");
        json.push_str(&format!(
            "      \"archive_path\": \"{}\",\n",
            escape_json(&entry.archive_path.display().to_string())
        ));
        match &entry.platform {
            Some(platform) => json.push_str(&format!(
                "      \"platform\": \"{}\",\n",
                escape_json(platform)
            )),
            None => json.push_str("      \"platform\": null,\n"),
        }
        json.push_str(&format!(
            "      \"display_name\": \"{}\",\n",
            escape_json(&entry.display_name)
        ));
        json.push_str(&format!(
            "      \"mount_path\": \"{}\",\n",
            escape_json(&entry.mount_path.display().to_string())
        ));
        match entry.modified_time_seconds {
            Some(modified_time) => {
                json.push_str(&format!("      \"modified_time\": {modified_time},\n"))
            }
            None => json.push_str("      \"modified_time\": null,\n"),
        }
        json.push_str(&format!("      \"health\": \"{}\",\n", entry.health));
        json.push_str(&format!(
            "      \"mount_state\": \"{}\"\n",
            entry.mount_state
        ));
        json.push_str("    }");
    }
    json.push_str("\n  ]\n}\n");
    json
}

fn extract_json_number_field(object: &str, field: &str) -> Option<u64> {
    let needle = format!("\"{field}\":");
    let start = object.find(&needle)? + needle.len();
    let rest = object[start..].trim_start();
    let digits = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn extract_json_string_field(object: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = object.find(&needle)? + needle.len();
    let rest = object[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut value = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

pub fn current_archive_records(config: &Config) -> Result<Vec<ArchiveRecord>> {
    ArchiveScanner::new(config).archive_records()
}

pub fn current_archive_records_with_metadata_provider(
    config: &Config,
    metadata_provider: &impl MetadataProvider,
) -> Result<Vec<ArchiveRecord>> {
    let health_provider = FilesystemHealthProvider;
    ArchiveScanner::new(config).archive_records_with_providers(metadata_provider, &health_provider)
}

pub fn current_archive_records_with_providers(
    config: &Config,
    metadata_provider: &impl MetadataProvider,
    health_provider: &impl HealthProvider,
) -> Result<Vec<ArchiveRecord>> {
    ArchiveScanner::new(config).archive_records_with_providers(metadata_provider, health_provider)
}

pub fn current_statuses(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let records = current_archive_records(config)?;
    Ok(archive_statuses_from_records(&records))
}

fn archive_statuses_from_records(records: &[ArchiveRecord]) -> Vec<ArchiveStatus> {
    records
        .iter()
        .map(|record| ArchiveStatus {
            archive_path: record.mount_plan.archive.path.clone(),
            mount_path: record.mount_plan.mount_path.clone(),
            state: record.mount_state,
        })
        .collect()
}

pub fn mount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    mount_archives_with_backend(config, &backend)
}

pub fn mount_archives_with_backend(
    config: &Config,
    backend: &impl MountBackend,
) -> Result<Vec<ArchiveStatus>> {
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    fs::create_dir_all(&config.mount_root)
        .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;

    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    for plan in &plans {
        if mounted_paths.contains(&plan.mount_path) {
            continue;
        }
        fs::create_dir_all(&plan.mount_path)
            .map_err(|source| ArchiveFsError::io(plan.mount_path.clone(), source))?;
        backend.mount(plan)?;
    }

    current_statuses(config)
}

pub fn mount_one_archive(config: &Config, input: &str) -> Result<MountPlan> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    mount_one_archive_with_backend(config, input, &backend)
}

/// Mounts the archive whose filesystem path exactly matches `archive_path`.
pub fn mount_one_archive_path(config: &Config, archive_path: &Path) -> Result<MountPlan> {
    mount_one_archive_path_outcome(config, archive_path).map(MountOneOutcome::into_plan)
}

/// Mounts one exact archive and reports whether it was already active.
pub fn mount_one_archive_path_outcome(
    config: &Config,
    archive_path: &Path,
) -> Result<MountOneOutcome> {
    ArchiveMountSession::new(config)?.mount_archive_path(archive_path)
}

/// Mounts one exact archive only when its planned mount path is not already active.
pub fn remount_one_archive_path(config: &Config, archive_path: &Path) -> Result<MountPlan> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan_by_path(&plans, archive_path)?;
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    remount_one_plan(config, plan, &mounted_paths, &backend)
}

fn remount_one_plan(
    config: &Config,
    plan: MountPlan,
    mounted_paths: &HashSet<PathBuf>,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    if !plan.archive.path.is_file() {
        return Err(ArchiveFsError::Mount(format!(
            "archive no longer exists: {}",
            plan.archive.path.display()
        )));
    }
    if mounted_paths.contains(&plan.mount_path) {
        return Err(ArchiveFsError::Mount(format!(
            "refusing to remount {} because {} is still mounted",
            plan.archive.path.display(),
            plan.mount_path.display()
        )));
    }
    mount_unmounted_plan(config, plan, backend)
}

pub fn mount_one_archive_with_backend(
    config: &Config,
    input: &str,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    info!("mount-one requested for {input}");
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan(&plans, input)?;
    mount_one_plan(config, plan, backend)
}

#[cfg(test)]
fn mount_one_archive_path_with_backend(
    config: &Config,
    archive_path: &Path,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    info!("mount-one requested for {}", archive_path.display());
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan_by_path(&plans, archive_path)?;
    mount_one_plan(config, plan, backend)
}

fn mount_one_plan(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    mount_one_plan_outcome(config, plan, backend).map(MountOneOutcome::into_plan)
}

fn mount_one_plan_outcome(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
) -> Result<MountOneOutcome> {
    debug!(
        "mount-one selected archive={} mount={}",
        plan.archive.path.display(),
        plan.mount_path.display()
    );

    fs::create_dir_all(&config.mount_root)
        .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;
    let active_mount_paths = current_mount_paths()?;
    mount_one_plan_outcome_with_active_mounts(config, plan, backend, &active_mount_paths)
}

fn mount_one_plan_outcome_with_active_mounts(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
    active_mount_paths: &HashSet<PathBuf>,
) -> Result<MountOneOutcome> {
    let resolved_mount_path = resolved_mount_target(config, &plan.mount_path)?;
    if active_mount_paths.contains(&plan.mount_path)
        || active_mount_paths.contains(&resolved_mount_path)
    {
        info!("{} is already mounted", plan.mount_path.display());
        return Ok(MountOneOutcome::AlreadyMounted(plan));
    }
    mount_unmounted_plan(config, plan, backend).map(MountOneOutcome::Mounted)
}

fn mount_unmounted_plan(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    fs::create_dir_all(&config.mount_root)
        .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;
    validate_mount_target_parent(config, &plan.mount_path)?;
    info!("mounting {}", plan.archive.path.display());
    fs::create_dir_all(&plan.mount_path)
        .map_err(|source| ArchiveFsError::io(plan.mount_path.clone(), source))?;
    if !path_resolves_below(&plan.mount_path, &config.mount_root)? {
        return Err(ArchiveFsError::Config(format!(
            "refusing to mount outside mount root: {}",
            plan.mount_path.display()
        )));
    }
    backend.mount(&plan)?;
    info!(
        "mounted {} at {}",
        plan.archive.path.display(),
        plan.mount_path.display()
    );
    Ok(plan)
}

fn validate_mount_batch_targets(
    config: &Config,
    plans: &[MountPlan],
    archive_paths: &[PathBuf],
    active_mount_paths: &HashSet<PathBuf>,
) -> Vec<MountBatchTargetValidation> {
    let mut seen_targets: std::collections::HashMap<PathBuf, PathBuf> =
        std::collections::HashMap::new();
    archive_paths
        .iter()
        .map(|archive_path| {
            let plan = match select_mount_plan_by_path(plans, archive_path) {
                Ok(plan) => plan,
                Err(error) => {
                    return MountBatchTargetValidation::Skipped {
                        archive_path: archive_path.clone(),
                        mount_path: None,
                        reason: MountBatchTargetSkipReason::Selection(error.to_string()),
                    };
                }
            };
            let resolved_mount_path = match resolved_mount_target(config, &plan.mount_path) {
                Ok(path) => path,
                Err(error) => {
                    return MountBatchTargetValidation::Skipped {
                        archive_path: archive_path.clone(),
                        mount_path: Some(plan.mount_path.clone()),
                        reason: MountBatchTargetSkipReason::InvalidTarget(error.to_string()),
                    };
                }
            };
            if active_mount_paths.contains(&resolved_mount_path) {
                return MountBatchTargetValidation::Skipped {
                    archive_path: archive_path.clone(),
                    mount_path: Some(plan.mount_path.clone()),
                    reason: MountBatchTargetSkipReason::AlreadyMountedResolvedTarget {
                        resolved_mount_path,
                    },
                };
            }
            if let Some(first_mount_path) = seen_targets.get(&resolved_mount_path) {
                return MountBatchTargetValidation::Skipped {
                    archive_path: archive_path.clone(),
                    mount_path: Some(plan.mount_path.clone()),
                    reason: MountBatchTargetSkipReason::DuplicateResolvedTarget {
                        resolved_mount_path,
                        first_mount_path: first_mount_path.clone(),
                    },
                };
            }
            seen_targets.insert(resolved_mount_path.clone(), plan.mount_path.clone());
            MountBatchTargetValidation::Ready {
                archive_path: archive_path.clone(),
                mount_path: plan.mount_path.clone(),
                resolved_mount_path,
            }
        })
        .collect()
}

fn validate_mount_target_parent(config: &Config, mount_path: &Path) -> Result<()> {
    resolved_mount_target(config, mount_path).map(|_| ())
}

fn resolved_mount_target(config: &Config, mount_path: &Path) -> Result<PathBuf> {
    if mount_path == config.mount_root || !path_is_under(mount_path, &config.mount_root) {
        return Err(ArchiveFsError::Config(format!(
            "refusing to mount {} outside mount root {}",
            mount_path.display(),
            config.mount_root.display()
        )));
    }

    let resolved_root = fs::canonicalize(&config.mount_root)
        .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;
    let mut existing_parent = mount_path.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "mount path has no parent: {}",
            mount_path.display()
        ))
    })?;
    while !existing_parent.exists() {
        existing_parent = existing_parent.parent().ok_or_else(|| {
            ArchiveFsError::Config(format!(
                "cannot resolve a safe parent for {}",
                mount_path.display()
            ))
        })?;
    }
    let resolved_parent = fs::canonicalize(existing_parent)
        .map_err(|source| ArchiveFsError::io(existing_parent.to_path_buf(), source))?;
    if resolved_parent != resolved_root && !path_is_under(&resolved_parent, &resolved_root) {
        return Err(ArchiveFsError::Config(format!(
            "refusing to mount {} through a parent outside mount root {}",
            mount_path.display(),
            config.mount_root.display()
        )));
    }
    let suffix = mount_path.strip_prefix(existing_parent).map_err(|_| {
        ArchiveFsError::Config(format!(
            "cannot resolve mount target {} from parent {}",
            mount_path.display(),
            existing_parent.display()
        ))
    })?;
    Ok(resolved_parent.join(suffix))
}

pub fn unmount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    unmount_archives_with_backend(config, &backend)
}

pub fn unmount_archives_with_backend(
    config: &Config,
    backend: &impl MountBackend,
) -> Result<Vec<ArchiveStatus>> {
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    let mut mounted_paths = mounted_paths.into_iter().collect::<Vec<_>>();
    mounted_paths.sort();
    mounted_paths.reverse();

    for mount_path in mounted_paths {
        if path_is_under(&mount_path, &config.mount_root) {
            backend.unmount(&mount_path)?;
        }
    }

    current_statuses(config)
}

pub fn unmount_one_archive(config: &Config, input: &str) -> Result<MountPlan> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    unmount_one_archive_with_backend(config, input, &backend)
}

/// Unmounts the archive whose filesystem path exactly matches `archive_path`.
pub fn unmount_one_archive_path(config: &Config, archive_path: &Path) -> Result<MountPlan> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    unmount_one_archive_path_with_backend(config, archive_path, &backend)
}

/// Lazily unmounts one exact archive after verifying its mount is active and safely rooted.
pub fn lazy_unmount_one_archive_path(
    config: &Config,
    archive_path: &Path,
    cleanup_after_unmount: bool,
) -> Result<LazyUnmountResult> {
    lazy_unmount_one_archive_path_with_progress(config, archive_path, cleanup_after_unmount, |_| {})
}

/// Lazily unmounts one exact archive and reports when optional cleanup begins.
pub fn lazy_unmount_one_archive_path_with_progress(
    config: &Config,
    archive_path: &Path,
    cleanup_after_unmount: bool,
    cleanup_started: impl FnOnce(&Path),
) -> Result<LazyUnmountResult> {
    let backend = SystemLazyUnmountBackend;
    lazy_unmount_one_archive_path_with_backend(
        config,
        archive_path,
        cleanup_after_unmount,
        &backend,
        cleanup_started,
    )
}

fn lazy_unmount_one_archive_path_with_backend(
    config: &Config,
    archive_path: &Path,
    cleanup_after_unmount: bool,
    backend: &impl LazyUnmountBackend,
    cleanup_started: impl FnOnce(&Path),
) -> Result<LazyUnmountResult> {
    info!("lazy-unmount requested for {}", archive_path.display());
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan_by_path(&plans, archive_path)?;
    let mount_path = plan.mount_path;

    let mounted_paths = backend.mounted_paths_under(&config.mount_root)?;
    validate_lazy_unmount_path(config, &mount_path, &mounted_paths)?;

    let tool = backend.lazy_unmount(&mount_path)?;
    if backend
        .mounted_paths_under(&config.mount_root)?
        .contains(&mount_path)
    {
        return Err(ArchiveFsError::Unmount(format!(
            "{} is still mounted after lazy unmount",
            mount_path.display()
        )));
    }

    let cleanup = if cleanup_after_unmount {
        cleanup_started(&mount_path);
        Some(match cleanup_selected_mount_tree(config, &mount_path) {
            Ok(removed) => LazyUnmountCleanupResult::Completed(removed),
            Err(error) => LazyUnmountCleanupResult::Failed(error.to_string()),
        })
    } else {
        None
    };

    Ok(LazyUnmountResult {
        succeeded: true,
        tool,
        mount_path,
        warning: Some(
            "Lazy unmount detached the mount; programs may still be using files from it."
                .to_string(),
        ),
        cleanup,
    })
}

fn validate_lazy_unmount_path(
    config: &Config,
    mount_path: &Path,
    mounted_paths: &HashSet<PathBuf>,
) -> Result<()> {
    validate_active_unmount_path(config, mount_path, mounted_paths, "lazy-unmount")
}

pub fn unmount_one_archive_with_backend(
    config: &Config,
    input: &str,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    info!("unmount-one requested for {input}");
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan(&plans, input)?;
    unmount_one_plan(config, plan, backend)
}

fn unmount_one_archive_path_with_backend(
    config: &Config,
    archive_path: &Path,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    info!("unmount-one requested for {}", archive_path.display());
    let scanner = ArchiveScanner::new(config);
    let plans = scanner.mount_plans()?;
    let plan = select_mount_plan_by_path(&plans, archive_path)?;
    unmount_one_plan(config, plan, backend)
}

fn unmount_one_plan(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
) -> Result<MountPlan> {
    debug!(
        "unmount-one selected archive={} mount={}",
        plan.archive.path.display(),
        plan.mount_path.display()
    );

    if !path_is_under(&plan.mount_path, &config.mount_root) {
        return Err(ArchiveFsError::Config(format!(
            "refusing to unmount {} outside mount root {}",
            plan.mount_path.display(),
            config.mount_root.display()
        )));
    }

    info!("unmounting {}", plan.mount_path.display());
    backend.unmount(&plan.mount_path)?;
    info!("unmounted {}", plan.mount_path.display());
    Ok(plan)
}

fn unmount_one_plan_outcome_with_active_mounts(
    config: &Config,
    plan: MountPlan,
    backend: &impl MountBackend,
    active_mount_paths: &HashSet<PathBuf>,
) -> Result<UnmountOneOutcome> {
    if !active_mount_paths.contains(&plan.mount_path) {
        return Ok(UnmountOneOutcome::NotMounted(plan));
    }
    validate_active_unmount_path(config, &plan.mount_path, active_mount_paths, "unmount")?;
    unmount_one_plan(config, plan, backend).map(UnmountOneOutcome::Unmounted)
}

fn ensure_mount_disappeared(
    mount_path: &Path,
    active_mount_paths: &HashSet<PathBuf>,
) -> Result<()> {
    if active_mount_paths.contains(mount_path) {
        return Err(ArchiveFsError::Unmount(format!(
            "{} is still mounted after normal unmount",
            mount_path.display()
        )));
    }
    Ok(())
}

fn validate_active_unmount_path(
    config: &Config,
    mount_path: &Path,
    mounted_paths: &HashSet<PathBuf>,
    operation: &str,
) -> Result<()> {
    if mount_path == config.mount_root || !path_is_under(mount_path, &config.mount_root) {
        return Err(ArchiveFsError::Config(format!(
            "refusing to {operation} {} outside mount root {}",
            mount_path.display(),
            config.mount_root.display()
        )));
    }
    if !mounted_paths.contains(mount_path) {
        return Err(ArchiveFsError::Unmount(format!(
            "{} is not currently mounted",
            mount_path.display()
        )));
    }
    let Some(parent) = mount_path.parent() else {
        return Err(ArchiveFsError::Config(format!(
            "refusing to {operation} {} without a parent below mount root {}",
            mount_path.display(),
            config.mount_root.display()
        )));
    };
    let resolved_root = fs::canonicalize(&config.mount_root)
        .map_err(|source| ArchiveFsError::io(config.mount_root.clone(), source))?;
    let resolved_parent = fs::canonicalize(parent)
        .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
    if resolved_parent != resolved_root && !path_is_under(&resolved_parent, &resolved_root) {
        return Err(ArchiveFsError::Config(format!(
            "refusing to {operation} {} through a parent outside mount root {}",
            mount_path.display(),
            config.mount_root.display()
        )));
    }
    Ok(())
}

pub fn cleanup_selected_mount_dir(config: &Config, mount_path: &Path) -> Result<bool> {
    if mount_path == config.mount_root
        || !path_is_under(mount_path, &config.mount_root)
        || !mount_path.is_dir()
        || !path_resolves_below(mount_path, &config.mount_root)?
    {
        return Ok(false);
    }
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    remove_empty_unmounted_dir(mount_path, &mounted_paths)
}

/// Removes an empty selected mount directory and its empty ancestors below the mount root.
pub fn cleanup_selected_mount_tree(config: &Config, mount_path: &Path) -> Result<Vec<PathBuf>> {
    if mount_path == config.mount_root
        || !path_is_under(mount_path, &config.mount_root)
        || !mount_path.is_dir()
        || !path_resolves_below(mount_path, &config.mount_root)?
    {
        return Ok(Vec::new());
    }

    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    let mut removed = Vec::new();
    let mut current = mount_path.to_path_buf();
    while current != config.mount_root && path_is_under(&current, &config.mount_root) {
        if !remove_empty_unmounted_dir(&current, &mounted_paths)? {
            break;
        }
        removed.push(current.clone());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    Ok(removed)
}

fn remove_empty_unmounted_dir(path: &Path, mounted_paths: &HashSet<PathBuf>) -> Result<bool> {
    if !path.is_dir() || mounted_paths.contains(path) || !directory_is_empty(path)? {
        return Ok(false);
    }
    fs::remove_dir(path).map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    Ok(true)
}

fn path_resolves_below(path: &Path, root: &Path) -> Result<bool> {
    let resolved_root =
        fs::canonicalize(root).map_err(|source| ArchiveFsError::io(root.to_path_buf(), source))?;
    let resolved_path =
        fs::canonicalize(path).map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    Ok(resolved_path != resolved_root && path_is_under(&resolved_path, &resolved_root))
}

pub fn clean_mount_root(config: &Config) -> Result<Vec<PathBuf>> {
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    clean_empty_dirs_under(&config.mount_root, &mounted_paths)
}

fn clean_empty_dirs_under(root: &Path, mounted_paths: &HashSet<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if !root.exists() {
        return Ok(removed);
    }
    clean_empty_dirs_recursive(root, root, mounted_paths, &mut removed)?;
    Ok(removed)
}

fn clean_empty_dirs_recursive(
    root: &Path,
    dir: &Path,
    mounted_paths: &HashSet<PathBuf>,
    removed: &mut Vec<PathBuf>,
) -> Result<()> {
    if mounted_paths.contains(dir) {
        return Ok(());
    }

    let entries =
        fs::read_dir(dir).map_err(|source| ArchiveFsError::io(dir.to_path_buf(), source))?;

    for entry in entries {
        let entry = entry.map_err(|source| ArchiveFsError::io(dir.to_path_buf(), source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| ArchiveFsError::io(path.clone(), source))?;
        if file_type.is_dir() {
            clean_empty_dirs_recursive(root, &path, mounted_paths, removed)?;
        }
    }

    if dir != root && !mounted_paths.contains(dir) && directory_is_empty(dir)? {
        fs::remove_dir(dir).map_err(|source| ArchiveFsError::io(dir.to_path_buf(), source))?;
        removed.push(dir.to_path_buf());
    }

    Ok(())
}

fn directory_is_empty(path: &Path) -> Result<bool> {
    let mut entries =
        fs::read_dir(path).map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    Ok(entries.next().is_none())
}

fn records_from_plans(
    plans: Vec<MountPlan>,
    mounted_paths: &HashSet<PathBuf>,
    metadata_provider: &impl MetadataProvider,
    health_provider: &impl HealthProvider,
) -> Vec<ArchiveRecord> {
    plans
        .into_iter()
        .map(|plan| {
            let mount_state = mount_state_for_plan(&plan, mounted_paths);
            let metadata = metadata_provider.metadata_for(&plan.archive);
            let health = health_provider.health_for(&plan.archive);
            ArchiveRecord::new(plan, mount_state, metadata, health)
        })
        .collect()
}

fn mount_state_for_plan(plan: &MountPlan, mounted_paths: &HashSet<PathBuf>) -> MountState {
    if mounted_paths.contains(&plan.mount_path) {
        MountState::Mounted
    } else if plan.mount_path.exists() {
        MountState::MountPathExists
    } else {
        MountState::Pending
    }
}

fn archive_index_entry_from_record(record: ArchiveRecord) -> ArchiveIndexEntry {
    ArchiveIndexEntry {
        archive_path: record.mount_plan.archive.path,
        platform: record.metadata.platform,
        display_name: record
            .metadata
            .title
            .unwrap_or(record.identity.display_name),
        mount_path: record.mount_plan.mount_path,
        modified_time_seconds: record.identity.modified_time.and_then(system_time_seconds),
        health: record.health,
        mount_state: record.mount_state,
    }
}

fn current_mount_paths() -> Result<HashSet<PathBuf>> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|source| ArchiveFsError::io(PathBuf::from("/proc/self/mountinfo"), source))?;
    Ok(mountinfo
        .lines()
        .filter_map(mount_path_from_mountinfo_line)
        .collect())
}

fn mounted_paths_under(root: &Path) -> Result<HashSet<PathBuf>> {
    Ok(current_mount_paths()?
        .into_iter()
        .filter(|path| path_is_under(path, root))
        .collect())
}

fn mount_path_from_mountinfo_line(line: &str) -> Option<PathBuf> {
    let mut fields = line.split_whitespace();
    fields
        .nth(4)
        .map(unescape_mountinfo_path)
        .map(PathBuf::from)
}

fn unescape_mountinfo_path(path: &str) -> String {
    path.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn unmount_path(path: &Path) -> Result<()> {
    for program in ["fusermount3", "fusermount", "umount"] {
        match run_command(program, &[path]) {
            Ok(()) => return Ok(()),
            Err(ArchiveFsError::ExternalCommand { .. }) => continue,
            Err(ArchiveFsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error),
        }
    }

    Err(ArchiveFsError::ExternalCommand {
        program: "fusermount3/fusermount/umount".to_string(),
        status: None,
        stderr: format!("failed to unmount {}", path.display()),
    })
}

fn lazy_unmount_path(path: &Path) -> Result<LazyUnmountTool> {
    let mut last_error = None;
    if command_available("fusermount3") {
        match run_command_os("fusermount3", &["-uz".as_ref(), path.as_os_str()]) {
            Ok(()) => return Ok(LazyUnmountTool::Fusermount3),
            Err(error) => last_error = Some(error),
        }
    }
    if command_available("umount") {
        match run_command_os("umount", &["-l".as_ref(), path.as_os_str()]) {
            Ok(()) => return Ok(LazyUnmountTool::Umount),
            Err(error) => last_error = Some(error),
        }
    }

    Err(
        last_error.unwrap_or_else(|| ArchiveFsError::ExternalCommand {
            program: "fusermount3 -uz/umount -l".to_string(),
            status: None,
            stderr: "no supported lazy-unmount command is available".to_string(),
        }),
    )
}

pub fn command_available(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file();
    }

    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

fn run_command(program: &str, args: &[&Path]) -> Result<()> {
    let args = args.iter().map(|path| path.as_os_str()).collect::<Vec<_>>();
    run_command_os(program, &args)
}

fn run_command_os(program: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| ArchiveFsError::io(PathBuf::from(program), source))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(ArchiveFsError::ExternalCommand {
            program: program.to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn central_error_display_preserves_selection_messages() {
        let error = ArchiveFsError::selection_no_match("missing");
        assert_eq!(error.to_string(), "no archive matched 'missing'");

        let error = ArchiveFsError::selection_ambiguous(
            "007",
            vec![(
                PathBuf::from("/roms/007 Legends.zip"),
                PathBuf::from("/mnt/archivefs/Xbox360/007_Legends"),
            )],
        );
        let message = error.to_string();
        assert!(message.contains("multiple archives matched '007':"));
        assert!(message.contains("Archive: /roms/007 Legends.zip"));
        assert!(message.contains("Mount:   /mnt/archivefs/Xbox360/007_Legends"));
    }

    #[test]
    fn central_error_from_io_error_keeps_source() {
        let error = ArchiveFsError::from(io::Error::other("boom"));

        assert_eq!(error.to_string(), "boom");
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn index_error_has_index_category() {
        let error = ArchiveFsError::Index("HOME is not set".to_string());

        assert_eq!(error.to_string(), "index error: HOME is not set");
    }

    #[test]
    fn detects_supported_archive_extensions_case_insensitively() {
        assert_eq!(archive_kind("game.zip"), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind("game.7Z"), Some(ArchiveKind::SevenZip));
        assert_eq!(archive_kind("game.RAR"), Some(ArchiveKind::Rar));
        assert_eq!(archive_kind("game.iso"), None);
        assert_eq!(archive_kind("game.zip.tmp"), None);
    }

    #[test]
    fn skips_split_rar_parts_except_main_parts() {
        assert!(!should_skip_split_archive_part("game.rar"));
        assert!(!should_skip_split_archive_part("game.part1.rar"));
        assert!(!should_skip_split_archive_part("game.part01.rar"));
        assert!(should_skip_split_archive_part("game.part2.rar"));
        assert!(should_skip_split_archive_part("game.part10.rar"));
        assert!(should_skip_split_archive_part("game.r00"));
        assert!(should_skip_split_archive_part("game.r99"));
        assert_eq!(archive_kind("game.part2.rar"), None);
        assert_eq!(archive_kind("game.part1.rar"), Some(ArchiveKind::Rar));
    }

    #[test]
    fn generates_safe_mount_names() {
        assert_eq!(
            safe_mount_name("/tmp/Resident Evil 2.zip"),
            "Resident_Evil_2"
        );
        assert_eq!(safe_mount_name("/tmp/../../!!!.7z"), "archive");
        assert_eq!(
            safe_mount_name("/tmp/Metal: Gear? Solid.rar"),
            "Metal_Gear_Solid"
        );
        assert_eq!(safe_mount_name("/tmp/Game.part1.rar"), "Game");
    }

    #[test]
    fn duplicate_filenames_get_distinct_mount_paths() {
        let archives = vec![
            archive("/roms/collection-a/game.zip"),
            archive("/roms/collection-b/game.zip"),
        ];
        let mounts = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(mounts.len(), 2);
        assert_ne!(mounts[0].mount_path, mounts[1].mount_path);
        assert!(
            mounts[0]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
        assert!(
            mounts[1]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
    }

    #[test]
    fn platform_aware_mount_paths_use_platform_folder() {
        let archives = vec![archive_with_platform(
            "/roms/xbox360/Halo 3.zip",
            Some("Xbox360"),
        )];
        let mounts = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(
            mounts[0].mount_path,
            PathBuf::from("/mnt/archivefs/Xbox360/Halo_3")
        );
    }

    #[test]
    fn platform_aware_mount_paths_use_unknown_folder_without_platform() {
        let archives = vec![archive_with_platform("/roms/misc/Mystery.zip", None)];
        let mounts = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(
            mounts[0].mount_path,
            PathBuf::from("/mnt/archivefs/Unknown/Mystery")
        );
    }

    #[test]
    fn duplicate_mount_suffixes_are_scoped_to_platform_folder() {
        let archives = vec![
            archive_with_platform("/roms/xbox/game.zip", Some("Xbox")),
            archive_with_platform("/roms/xbox-alt/game.zip", Some("Xbox")),
            archive_with_platform("/roms/xbox360/game.zip", Some("Xbox360")),
        ];
        let mounts = plan_mounts(&archives, "/mnt/archivefs");

        assert_ne!(mounts[0].mount_path, mounts[1].mount_path);
        assert!(
            mounts[0]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
        assert!(
            mounts[1]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
        assert_eq!(
            mounts[2].mount_path,
            PathBuf::from("/mnt/archivefs/Xbox360/game")
        );
    }

    #[test]
    fn select_mount_plan_matches_exact_archive_path() {
        let archives = vec![archive_with_platform(
            "/roms/xbox360/007 Legends.zip",
            Some("Xbox360"),
        )];
        let plans = plan_mounts(&archives, "/mnt/archivefs");
        let selected = select_mount_plan(&plans, "/roms/xbox360/007 Legends.zip").unwrap();

        assert_eq!(
            selected.archive.path,
            PathBuf::from("/roms/xbox360/007 Legends.zip")
        );
        assert_eq!(
            selected.mount_path,
            PathBuf::from("/mnt/archivefs/Xbox360/007_Legends")
        );
    }

    #[test]
    fn select_mount_plan_matches_display_name_partial_case_insensitively() {
        let archives = vec![archive_with_platform(
            "/roms/xbox360/007 Legends.zip",
            Some("Xbox360"),
        )];
        let plans = plan_mounts(&archives, "/mnt/archivefs");
        let selected = select_mount_plan(&plans, "legends").unwrap();

        assert_eq!(
            selected.archive.path,
            PathBuf::from("/roms/xbox360/007 Legends.zip")
        );
    }

    #[test]
    fn select_mount_plan_matches_safe_mount_name() {
        let archives = vec![archive_with_platform(
            "/roms/ps1/Metal: Gear? Solid.zip",
            None,
        )];
        let plans = plan_mounts(&archives, "/mnt/archivefs");
        let selected = select_mount_plan(&plans, "metal_gear").unwrap();

        assert_eq!(
            selected.mount_path,
            PathBuf::from("/mnt/archivefs/Unknown/Metal_Gear_Solid")
        );
    }

    #[test]
    fn select_mount_plan_errors_for_zero_matches() {
        let archives = vec![archive_with_platform(
            "/roms/xbox360/007 Legends.zip",
            Some("Xbox360"),
        )];
        let plans = plan_mounts(&archives, "/mnt/archivefs");
        let error = select_mount_plan(&plans, "not here").unwrap_err();

        assert!(matches!(
            error,
            ArchiveFsError::Selection(SelectionError::NoMatch { input }) if input == "not here"
        ));
    }

    #[test]
    fn select_mount_plan_errors_for_multiple_matches() {
        let archives = vec![
            archive_with_platform("/roms/xbox360/007 Legends.zip", Some("Xbox360")),
            archive_with_platform("/roms/xbox360/007 Racing.zip", Some("Xbox360")),
        ];
        let plans = plan_mounts(&archives, "/mnt/archivefs");
        let error = select_mount_plan(&plans, "007").unwrap_err();

        match error {
            ArchiveFsError::Selection(SelectionError::Ambiguous { input, matches }) => {
                assert_eq!(input, "007");
                assert_eq!(matches.len(), 2);
                assert!(matches.iter().any(|(archive_path, mount_path)| {
                    archive_path == &PathBuf::from("/roms/xbox360/007 Legends.zip")
                        && mount_path == &PathBuf::from("/mnt/archivefs/Xbox360/007_Legends")
                }));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn mount_target_validation_rejects_root_and_outside_paths() {
        let root = test_root("mount_target_safety");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&mount_root).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: mount_root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(validate_mount_target_parent(&config, &mount_root).is_err());
        assert!(validate_mount_target_parent(&config, &root.join("outside").join("Game")).is_err());
        assert!(validate_mount_target_parent(&config, &mount_root.join("Platform/Game")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn mount_target_validation_rejects_symlinked_parent_escape() {
        use std::os::unix::fs::symlink;

        let root = test_root("mount_target_symlink_escape");
        let mount_root = root.join("mounts");
        let outside = root.join("outside");
        fs::create_dir_all(&mount_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, mount_root.join("escape")).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: mount_root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(validate_mount_target_parent(&config, &mount_root.join("escape/Game")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn batch_validation_deduplicates_symlinked_parent_aliases_before_mounting() {
        use std::os::unix::fs::symlink;

        let root = test_root("batch_target_alias");
        let mount_root = root.join("mounts");
        let real_parent = mount_root.join("Real");
        fs::create_dir_all(&real_parent).unwrap();
        symlink(&real_parent, mount_root.join("Alias")).unwrap();
        let first_archive = root.join("First.zip");
        let second_archive = root.join("Second.zip");
        let plans = vec![
            MountPlan::new(
                Archive::from_path(&first_archive).unwrap(),
                real_parent.join("Game"),
            ),
            MountPlan::new(
                Archive::from_path(&second_archive).unwrap(),
                mount_root.join("Alias/Game"),
            ),
        ];
        let config = Config {
            source_folders: vec![root.clone()],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        let validations = validate_mount_batch_targets(
            &config,
            &plans,
            &[first_archive.clone(), second_archive.clone()],
            &HashSet::new(),
        );

        assert!(matches!(
            &validations[0],
            MountBatchTargetValidation::Ready { resolved_mount_path, .. }
                if resolved_mount_path == &real_parent.join("Game")
        ));
        assert!(matches!(
            &validations[1],
            MountBatchTargetValidation::Skipped { reason, .. }
                if reason.to_string().contains("duplicate target")
        ));

        let backend = RecordingBackend::default();
        for validation in validations {
            if let MountBatchTargetValidation::Ready { archive_path, .. } = validation {
                let plan = select_mount_plan_by_path(&plans, &archive_path).unwrap();
                mount_one_plan_outcome(&config, plan, &backend).unwrap();
            }
        }
        assert_eq!(backend.mounted(), vec![first_archive]);
    }

    #[cfg(unix)]
    #[test]
    fn batch_validation_rejects_active_mount_alias_without_backend_call() {
        use std::os::unix::fs::symlink;

        let root = test_root("batch_active_alias");
        let mount_root = root.join("mounts");
        let real_parent = mount_root.join("Real");
        fs::create_dir_all(&real_parent).unwrap();
        symlink(&real_parent, mount_root.join("Alias")).unwrap();
        let archive_path = root.join("Game.zip");
        let plans = vec![MountPlan::new(
            Archive::from_path(&archive_path).unwrap(),
            mount_root.join("Alias/Game"),
        )];
        let config = Config {
            source_folders: vec![root],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let active_mounts = HashSet::from([real_parent.join("Game")]);

        let validations = validate_mount_batch_targets(
            &config,
            &plans,
            std::slice::from_ref(&archive_path),
            &active_mounts,
        );

        assert!(matches!(
            &validations[0],
            MountBatchTargetValidation::Skipped {
                reason: MountBatchTargetSkipReason::AlreadyMountedResolvedTarget { .. },
                ..
            }
        ));
        let backend = RecordingBackend::default();
        for validation in validations {
            if let MountBatchTargetValidation::Ready { archive_path, .. } = validation {
                let plan = select_mount_plan_by_path(&plans, &archive_path).unwrap();
                mount_one_plan_outcome_with_active_mounts(&config, plan, &backend, &active_mounts)
                    .unwrap();
            }
        }
        assert!(backend.mounted().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn exact_active_mount_is_rejected_without_touching_target() {
        let root = test_root("exact_active_no_touch");
        let mount_root = root.join("mounts");
        let parent = mount_root.join("Platform");
        fs::create_dir_all(&parent).unwrap();
        let mount_path = parent.join("Game");
        std::os::unix::fs::symlink(root.join("missing-target"), &mount_path).unwrap();
        let plan = MountPlan::new(
            Archive::from_path(root.join("Game.zip")).unwrap(),
            mount_path.clone(),
        );
        let config = Config {
            source_folders: vec![root],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let backend = RecordingBackend::default();
        let active_mounts = HashSet::from([mount_path]);

        let outcome =
            mount_one_plan_outcome_with_active_mounts(&config, plan, &backend, &active_mounts)
                .unwrap();

        assert!(matches!(outcome, MountOneOutcome::AlreadyMounted(_)));
        assert!(backend.mounted().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn normal_batch_unmount_requires_exact_membership_without_touching_target() {
        let root = test_root("unmount_batch_no_touch");
        let mount_root = root.join("mounts");
        let parent = mount_root.join("Platform");
        fs::create_dir_all(&parent).unwrap();
        let mount_path = parent.join("Game");
        std::os::unix::fs::symlink(root.join("missing-target"), &mount_path).unwrap();
        let plan = MountPlan::new(
            Archive::from_path(root.join("Game.zip")).unwrap(),
            mount_path.clone(),
        );
        let config = Config {
            source_folders: vec![root],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let backend = RecordingBackend::default();

        let not_mounted = unmount_one_plan_outcome_with_active_mounts(
            &config,
            plan.clone(),
            &backend,
            &HashSet::new(),
        )
        .unwrap();
        assert!(matches!(not_mounted, UnmountOneOutcome::NotMounted(_)));
        assert!(backend.unmounted().is_empty());

        let outcome = unmount_one_plan_outcome_with_active_mounts(
            &config,
            plan,
            &backend,
            &HashSet::from([mount_path.clone()]),
        )
        .unwrap();
        assert!(matches!(outcome, UnmountOneOutcome::Unmounted(_)));
        assert_eq!(backend.unmounted(), vec![mount_path]);
    }

    #[test]
    fn normal_batch_unmount_requires_mount_disappearance_before_cleanup() {
        let mount_path = PathBuf::from("/mounts/Game");
        assert!(ensure_mount_disappeared(&mount_path, &HashSet::new()).is_ok());
        assert!(
            ensure_mount_disappeared(&mount_path, &HashSet::from([mount_path.clone()]),).is_err()
        );
    }

    #[test]
    fn normal_batch_unmount_rejects_root_and_outside_paths() {
        let root = test_root("unmount_batch_roots");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&mount_root).unwrap();
        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: mount_root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };
        let backend = RecordingBackend::default();
        for mount_path in [mount_root.clone(), root.join("outside/Game")] {
            let plan = MountPlan::new(
                Archive::from_path(root.join("Game.zip")).unwrap(),
                mount_path.clone(),
            );
            assert!(
                unmount_one_plan_outcome_with_active_mounts(
                    &config,
                    plan,
                    &backend,
                    &HashSet::from([mount_path]),
                )
                .is_err()
            );
        }
        assert!(backend.unmounted().is_empty());
    }

    #[test]
    fn batch_validation_allows_distinct_targets_and_rejects_outside_root() {
        let root = test_root("batch_target_distinct");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&mount_root).unwrap();
        let archives = [
            root.join("First.zip"),
            root.join("Second.zip"),
            root.join("Outside.zip"),
        ];
        let plans = vec![
            MountPlan::new(
                Archive::from_path(&archives[0]).unwrap(),
                mount_root.join("One/Game"),
            ),
            MountPlan::new(
                Archive::from_path(&archives[1]).unwrap(),
                mount_root.join("Two/Game"),
            ),
            MountPlan::new(
                Archive::from_path(&archives[2]).unwrap(),
                root.join("outside/Game"),
            ),
        ];
        let config = Config {
            source_folders: vec![root],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        let validations = validate_mount_batch_targets(&config, &plans, &archives, &HashSet::new());

        assert!(matches!(
            validations[0],
            MountBatchTargetValidation::Ready { .. }
        ));
        assert!(matches!(
            validations[1],
            MountBatchTargetValidation::Ready { .. }
        ));
        assert!(matches!(
            &validations[2],
            MountBatchTargetValidation::Skipped { reason, .. }
                if reason.to_string().contains("outside mount root")
        ));
    }

    #[cfg(unix)]
    fn non_utf8_archive_fixture(name: &str) -> (Config, PathBuf, PathBuf) {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = test_root(name);
        let source_root = root.join("roms");
        fs::create_dir_all(&source_root).unwrap();
        let first = source_root.join(OsString::from_vec(b"Game\x80.zip".to_vec()));
        let second = source_root.join(OsString::from_vec(b"Game\x81.zip".to_vec()));
        fs::write(&first, b"").unwrap();
        fs::write(&second, b"").unwrap();
        let config = Config {
            source_folders: vec![source_root],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
        };
        (config, first, second)
    }

    #[cfg(unix)]
    #[test]
    fn path_selector_matches_non_utf8_archive_exactly() {
        let (config, first, second) = non_utf8_archive_fixture("non_utf8_select");
        let plans = ArchiveScanner::new(&config).mount_plans().unwrap();

        let selected = select_mount_plan_by_path(&plans, &second).unwrap();

        assert_eq!(selected.archive.path, second);
        assert_ne!(selected.archive.path, first);
        assert!(select_mount_plan(&plans, &selected.archive.path.to_string_lossy()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mount_one_path_targets_non_utf8_archive_without_fuzzy_fallback() {
        let (config, first, second) = non_utf8_archive_fixture("non_utf8_mount");
        let backend = RecordingBackend::default();

        let plan = mount_one_archive_path_with_backend(&config, &second, &backend).unwrap();

        assert_eq!(plan.archive.path, second);
        assert_ne!(plan.archive.path, first);
        assert_eq!(backend.mounted(), vec![plan.archive.path]);
    }

    #[cfg(unix)]
    #[test]
    fn unmount_one_path_targets_non_utf8_archive_exactly() {
        let (config, first, second) = non_utf8_archive_fixture("non_utf8_unmount");
        let backend = RecordingBackend::default();

        let plan = unmount_one_archive_path_with_backend(&config, &second, &backend).unwrap();

        assert_eq!(plan.archive.path, second);
        assert_ne!(plan.archive.path, first);
        assert_eq!(backend.unmounted(), vec![plan.mount_path]);
    }

    #[cfg(unix)]
    #[test]
    fn lazy_unmount_path_targets_non_utf8_archive_exactly() {
        let (config, first, second) = non_utf8_archive_fixture("non_utf8_lazy_unmount");
        let plan = select_mount_plan_by_path(
            &ArchiveScanner::new(&config).mount_plans().unwrap(),
            &second,
        )
        .unwrap();
        fs::create_dir_all(&plan.mount_path).unwrap();
        let backend = RecordingLazyUnmountBackend::new(vec![
            HashSet::from([plan.mount_path.clone()]),
            HashSet::new(),
        ]);

        let result =
            lazy_unmount_one_archive_path_with_backend(&config, &second, false, &backend, |_| {})
                .unwrap();

        assert_eq!(result.mount_path, plan.mount_path);
        assert_ne!(plan.archive.path, first);
        assert_eq!(plan.archive.path, second);
        assert_eq!(backend.unmounted(), vec![result.mount_path]);
    }

    #[test]
    fn unmount_one_unmounts_only_selected_mount_path() {
        let root = test_root("unmount_one_selected");
        let source_root = root.join("roms");
        let xbox360 = source_root.join("xbox360");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&xbox360).unwrap();
        fs::write(xbox360.join("007 Legends.zip"), b"").unwrap();
        fs::write(xbox360.join("Halo 3.zip"), b"").unwrap();
        let config = Config {
            source_folders: vec![source_root],
            mount_root: mount_root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };
        let backend = RecordingBackend::default();

        let plan = unmount_one_archive_with_backend(&config, "007 Legends", &backend).unwrap();

        assert_eq!(
            plan.mount_path,
            mount_root.join("Xbox360").join("007_Legends")
        );
        assert_eq!(backend.unmounted(), vec![plan.mount_path]);
    }

    #[test]
    fn unmount_one_reuses_selection_errors() {
        let root = test_root("unmount_one_zero");
        let source_root = root.join("roms");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("Halo.zip"), b"").unwrap();
        let config = Config {
            source_folders: vec![source_root],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
        };
        let backend = RecordingBackend::default();
        let error = unmount_one_archive_with_backend(&config, "missing", &backend).unwrap_err();

        assert!(matches!(
            error,
            ArchiveFsError::Selection(SelectionError::NoMatch { input }) if input == "missing"
        ));
        assert!(backend.unmounted().is_empty());
    }

    fn lazy_unmount_fixture(name: &str) -> (Config, PathBuf, PathBuf) {
        let root = test_root(name);
        let source_root = root.join("roms").join("xbox360");
        let archive_path = source_root.join("Game.zip");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(&archive_path, b"").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
        };
        let mount_path = ArchiveScanner::new(&config)
            .mount_plans()
            .unwrap()
            .remove(0)
            .mount_path;
        fs::create_dir_all(&mount_path).unwrap();
        (config, archive_path, mount_path)
    }

    #[test]
    fn lazy_unmount_rejects_mount_root_and_outside_paths() {
        let root = test_root("lazy_unmount_boundaries");
        let outside = test_root("lazy_unmount_outside");
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(
            validate_lazy_unmount_path(&config, &root, &HashSet::from([root.clone()])).is_err()
        );
        assert!(
            validate_lazy_unmount_path(&config, &outside, &HashSet::from([outside.clone()]))
                .is_err()
        );
    }

    #[test]
    fn lazy_unmount_validation_does_not_touch_the_mounted_target() {
        let root = test_root("lazy_unmount_no_target_access");
        let mount_root = root.join("mounts");
        let parent = mount_root.join("Xbox360");
        let mount_path = parent.join("Broken_Game");
        fs::create_dir_all(&parent).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(!mount_path.exists());
        validate_lazy_unmount_path(&config, &mount_path, &HashSet::from([mount_path.clone()]))
            .unwrap();
        assert!(!mount_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn lazy_unmount_validation_rejects_a_symlinked_parent_escape() {
        let root = test_root("lazy_unmount_parent_escape");
        let mount_root = root.join("mounts");
        let outside = test_root("lazy_unmount_parent_escape_outside");
        fs::create_dir_all(&mount_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, mount_root.join("escape")).unwrap();
        let mount_path = mount_root.join("escape").join("Game");
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        let error =
            validate_lazy_unmount_path(&config, &mount_path, &HashSet::from([mount_path.clone()]))
                .unwrap_err();

        assert!(error.to_string().contains("parent outside mount root"));
        assert!(!mount_path.exists());
    }

    #[test]
    fn lazy_unmount_recovery_accepts_a_broken_mount_without_target_access() {
        let (config, archive_path, mount_path) = lazy_unmount_fixture("lazy_unmount_broken_target");
        fs::remove_dir(&mount_path).unwrap();
        let backend = RecordingLazyUnmountBackend::new(vec![
            HashSet::from([mount_path.clone()]),
            HashSet::new(),
        ]);

        let result = lazy_unmount_one_archive_path_with_backend(
            &config,
            &archive_path,
            false,
            &backend,
            |_| {},
        )
        .unwrap();

        assert_eq!(result.mount_path, mount_path);
        assert_eq!(backend.unmounted(), vec![result.mount_path]);
    }

    #[test]
    fn lazy_unmount_rejects_a_path_that_is_not_mounted() {
        let (config, archive_path, _) = lazy_unmount_fixture("lazy_unmount_not_mounted");
        let backend = RecordingLazyUnmountBackend::new(vec![HashSet::new()]);

        let error = lazy_unmount_one_archive_path_with_backend(
            &config,
            &archive_path,
            false,
            &backend,
            |_| {},
        )
        .unwrap_err();

        assert!(error.to_string().contains("not currently mounted"));
        assert!(backend.unmounted().is_empty());
    }

    #[test]
    fn lazy_unmount_requires_mount_disappearance_before_cleanup() {
        let (config, archive_path, mount_path) = lazy_unmount_fixture("lazy_unmount_still_active");
        let mounted = HashSet::from([mount_path.clone()]);
        let backend = RecordingLazyUnmountBackend::new(vec![mounted.clone(), mounted]);
        let cleanup_started = std::cell::Cell::new(false);

        let error = lazy_unmount_one_archive_path_with_backend(
            &config,
            &archive_path,
            true,
            &backend,
            |_| cleanup_started.set(true),
        )
        .unwrap_err();

        assert!(error.to_string().contains("still mounted"));
        assert!(!cleanup_started.get());
        assert!(mount_path.exists());
    }

    #[test]
    fn lazy_unmount_cleans_only_after_mount_disappears() {
        let (config, archive_path, mount_path) = lazy_unmount_fixture("lazy_unmount_cleanup");
        let backend = RecordingLazyUnmountBackend::new(vec![
            HashSet::from([mount_path.clone()]),
            HashSet::new(),
        ]);

        let cleanup_started = std::cell::Cell::new(false);
        let result = lazy_unmount_one_archive_path_with_backend(
            &config,
            &archive_path,
            true,
            &backend,
            |path| {
                assert_eq!(path, mount_path);
                cleanup_started.set(true);
            },
        )
        .unwrap();

        assert!(result.succeeded);
        assert!(cleanup_started.get());
        assert_eq!(result.tool, LazyUnmountTool::Fusermount3);
        assert!(matches!(
            result.cleanup,
            Some(LazyUnmountCleanupResult::Completed(ref removed))
                if removed.contains(&mount_path)
        ));
        assert!(!mount_path.exists());
    }

    #[test]
    fn remount_rejects_a_still_active_mount_path() {
        let (config, _, mount_path) = lazy_unmount_fixture("remount_still_active");
        let plan = ArchiveScanner::new(&config)
            .mount_plans()
            .unwrap()
            .remove(0);
        let backend = RecordingBackend::default();

        let error =
            remount_one_plan(&config, plan, &HashSet::from([mount_path]), &backend).unwrap_err();

        assert!(error.to_string().contains("still mounted"));
        assert!(backend.mounted().is_empty());
    }

    #[test]
    fn remount_rejects_an_archive_that_no_longer_exists() {
        let (config, archive_path, _) = lazy_unmount_fixture("remount_missing_archive");
        let plan = ArchiveScanner::new(&config)
            .mount_plans()
            .unwrap()
            .remove(0);
        fs::remove_file(&archive_path).unwrap();
        let backend = RecordingBackend::default();

        let error = remount_one_plan(&config, plan, &HashSet::new(), &backend).unwrap_err();

        assert!(error.to_string().contains("no longer exists"));
        assert!(backend.mounted().is_empty());
    }

    #[test]
    fn archive_index_json_contains_required_fields() {
        let index = ArchiveIndex {
            archives: vec![ArchiveIndexEntry {
                archive_path: PathBuf::from("/roms/xbox360/007 Legends.zip"),
                platform: Some("Xbox360".to_string()),
                display_name: "007 Legends".to_string(),
                mount_path: PathBuf::from("/mnt/archivefs/Xbox360/007_Legends"),
                modified_time_seconds: None,
                health: ArchiveHealth::Pending,
                mount_state: MountState::Pending,
            }],
        };
        let json = archive_index_to_json(&index);

        assert!(json.contains("\"archive_path\": \"/roms/xbox360/007 Legends.zip\""));
        assert!(json.contains("\"platform\": \"Xbox360\""));
        assert!(json.contains("\"display_name\": \"007 Legends\""));
        assert!(json.contains("\"mount_path\": \"/mnt/archivefs/Xbox360/007_Legends\""));
        assert!(json.contains("\"health\": \"Pending\""));
        assert!(json.contains("\"mount_state\": \"Pending\""));
    }

    #[test]
    fn filename_duplicate_detector_returns_empty_report_for_empty_input() {
        let detector = FilenameDuplicateDetector;

        let report = detector.detect_duplicates(&[]).unwrap();

        assert_eq!(report.detector, "filename");
        assert_eq!(report.archives_checked, 0);
        assert!(report.entries.is_empty());
    }

    #[test]
    fn filename_duplicate_detector_matches_same_platform_and_name_ignoring_extension() {
        let detector = FilenameDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/xbox360/007 Legends.zip",
                Some("Xbox360"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/imports/007 Legends.7z",
                Some("Xbox360"),
                MountState::Pending,
                2048,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert_eq!(report.detector, "filename");
        assert_eq!(report.archives_checked, 2);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].platform, "Xbox360");
        assert_eq!(report.entries[0].severity, DuplicateSeverity::Warning);
        assert_eq!(
            report.entries[0].archive_paths,
            vec![
                PathBuf::from("/roms/xbox360/007 Legends.zip"),
                PathBuf::from("/roms/imports/007 Legends.7z"),
            ]
        );
        assert!(report.entries[0].reason.contains("007_legends"));
        assert!(report.entries[0].reason.contains("Xbox360"));
    }

    #[test]
    fn filename_duplicate_detector_normalizes_names_with_safe_mount_logic() {
        let detector = FilenameDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/xbox/Halo 3.zip",
                Some("Xbox"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/imports/Halo_3.rar",
                Some("Xbox"),
                MountState::Pending,
                2048,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert_eq!(report.entries.len(), 1);
        assert!(report.entries[0].reason.contains("halo_3"));
    }

    #[test]
    fn filename_duplicate_detector_keeps_platforms_separate() {
        let detector = FilenameDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/xbox/Halo.zip",
                Some("Xbox"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/xbox360/Halo.7z",
                Some("Xbox360"),
                MountState::Pending,
                2048,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert!(report.entries.is_empty());
    }

    #[test]
    fn filename_duplicate_detector_ignores_different_names_on_same_platform() {
        let detector = FilenameDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/xbox/Halo.zip",
                Some("Xbox"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/xbox/Fable.zip",
                Some("Xbox"),
                MountState::Pending,
                2048,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert!(report.entries.is_empty());
    }

    #[test]
    fn filename_duplicate_detector_uses_metadata_platform_before_identity_platform() {
        let detector = FilenameDuplicateDetector;
        let mut xbox_record =
            archive_record_with_size("/roms/a/Game.zip", Some("Xbox"), MountState::Pending, 1024);
        xbox_record.identity.platform = Some("PC".to_string());
        xbox_record.metadata.platform = Some("Xbox".to_string());
        let mut pc_record =
            archive_record_with_size("/roms/b/Game.7z", Some("PC"), MountState::Pending, 2048);
        pc_record.identity.platform = Some("Xbox".to_string());
        pc_record.metadata.platform = Some("PC".to_string());

        let report = detector
            .detect_duplicates(&[xbox_record, pc_record])
            .unwrap();

        assert!(report.entries.is_empty());
    }

    #[test]
    fn filename_duplicate_detector_groups_more_than_two_records() {
        let detector = FilenameDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/a/Game.zip",
                Some("Unknown"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/b/Game.7z",
                Some("Unknown"),
                MountState::Pending,
                2048,
            ),
            archive_record_with_size(
                "/roms/c/Game.rar",
                Some("Unknown"),
                MountState::Pending,
                4096,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].archive_paths.len(), 3);
    }

    #[test]
    fn empty_duplicate_detector_returns_empty_report_for_empty_input() {
        let detector = EmptyDuplicateDetector;

        let report = detector.detect_duplicates(&[]).unwrap();

        assert_eq!(report.detector, "empty");
        assert_eq!(report.archives_checked, 0);
        assert!(report.entries.is_empty());
    }

    #[test]
    fn empty_duplicate_detector_counts_inputs_without_entries() {
        let detector = EmptyDuplicateDetector;
        let records = vec![
            archive_record_with_size(
                "/roms/xbox360/007 Legends.zip",
                Some("Xbox360"),
                MountState::Pending,
                1024,
            ),
            archive_record_with_size(
                "/roms/xbox360/007 Racing.zip",
                Some("Xbox360"),
                MountState::Pending,
                2048,
            ),
        ];

        let report = detector.detect_duplicates(&records).unwrap();

        assert_eq!(report.detector, "empty");
        assert_eq!(report.archives_checked, 2);
        assert_eq!(report.entries, Vec::<DuplicateEntry>::new());
    }

    #[test]
    fn archive_info_from_record_includes_archive_record_details() {
        let mut record = archive_record_with_size(
            "/roms/xbox360/Halo.zip",
            Some("Xbox360"),
            MountState::Mounted,
            2048,
        );
        record.metadata.title = Some("Halo Custom".to_string());
        record.identity.modified_time = Some(std::time::UNIX_EPOCH + Duration::from_secs(5));
        record.health = ArchiveHealth::Mounted;

        let info = archive_info_from_record(record);

        assert_eq!(info.title, "Halo Custom");
        assert_eq!(info.platform, Some("Xbox360".to_string()));
        assert_eq!(info.archive_path, PathBuf::from("/roms/xbox360/Halo.zip"));
        assert_eq!(info.mount_path, PathBuf::from("/mnt/archivefs/Test"));
        assert_eq!(info.extension, "zip");
        assert_eq!(info.size_bytes, Some(2048));
        assert_eq!(
            info.modified_time,
            Some(std::time::UNIX_EPOCH + Duration::from_secs(5))
        );
        assert_eq!(info.health, ArchiveHealth::Mounted);
        assert_eq!(info.mount_state, MountState::Mounted);
        assert_eq!(info.metadata_provider, "FilenameMetadataProvider");
        assert_eq!(info.health_provider, "FilesystemHealthProvider");
    }

    #[test]
    fn select_archive_record_reuses_selection_errors() {
        let records = vec![
            archive_record_with_size(
                "/roms/xbox360/007 Legends.zip",
                Some("Xbox360"),
                MountState::Pending,
                1,
            ),
            archive_record_with_size(
                "/roms/xbox360/007 Racing.zip",
                Some("Xbox360"),
                MountState::Pending,
                1,
            ),
        ];

        let missing = select_archive_record(&records, "missing").unwrap_err();
        assert!(matches!(
            missing,
            ArchiveFsError::Selection(SelectionError::NoMatch { input }) if input == "missing"
        ));

        let ambiguous = select_archive_record(&records, "007").unwrap_err();
        assert!(matches!(
            ambiguous,
            ArchiveFsError::Selection(SelectionError::Ambiguous { input, matches })
                if input == "007" && matches.len() == 2
        ));
    }

    #[test]
    fn select_archive_record_returns_matching_record() {
        let records = vec![archive_record_with_size(
            "/roms/xbox360/007 Legends.zip",
            Some("Xbox360"),
            MountState::Pending,
            1,
        )];

        let selected = select_archive_record(&records, "legends").unwrap();

        assert_eq!(
            selected.mount_plan.archive.path,
            PathBuf::from("/roms/xbox360/007 Legends.zip")
        );
    }

    #[test]
    fn archive_stats_summarizes_records_counts_and_sizes() {
        let records = vec![
            archive_record_with_size(
                "/roms/xbox360/Big Game.ZIP",
                Some("Xbox360"),
                MountState::Mounted,
                2048,
            ),
            archive_record_with_size(
                "/roms/xbox360/Small Game.7z",
                Some("Xbox360"),
                MountState::Pending,
                512,
            ),
            archive_record_with_size(
                "/roms/misc/Mystery.rar",
                None,
                MountState::MountPathExists,
                1024,
            ),
        ];

        let stats = summarize_archive_records(&records);

        assert_eq!(stats.total_archives, 3);
        assert_eq!(stats.mounted_count, 1);
        assert_eq!(stats.pending_count, 1);
        assert_eq!(
            stats.platform_counts,
            vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 2)]
        );
        assert_eq!(
            stats.extension_counts,
            vec![
                ("7z".to_string(), 1),
                ("rar".to_string(), 1),
                ("zip".to_string(), 1),
            ]
        );
        assert_eq!(stats.total_size_bytes, 3584);
        assert_eq!(
            stats.largest_archive,
            Some(ArchiveSizeSummary {
                archive_path: PathBuf::from("/roms/xbox360/Big Game.ZIP"),
                size_bytes: 2048,
            })
        );
        assert_eq!(
            stats.smallest_archive,
            Some(ArchiveSizeSummary {
                archive_path: PathBuf::from("/roms/xbox360/Small Game.7z"),
                size_bytes: 512,
            })
        );
    }

    #[test]
    fn archive_index_summary_counts_platforms_and_mount_states() {
        let index = ArchiveIndex {
            archives: vec![
                ArchiveIndexEntry {
                    archive_path: PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    platform: Some("Xbox360".to_string()),
                    display_name: "007 Legends".to_string(),
                    mount_path: PathBuf::from("/mnt/archivefs/Xbox360/007_Legends"),
                    modified_time_seconds: None,
                    health: ArchiveHealth::Pending,
                    mount_state: MountState::Mounted,
                },
                ArchiveIndexEntry {
                    archive_path: PathBuf::from("/roms/unknown/Mystery.zip"),
                    platform: None,
                    display_name: "Mystery".to_string(),
                    mount_path: PathBuf::from("/mnt/archivefs/Unknown/Mystery"),
                    modified_time_seconds: None,
                    health: ArchiveHealth::Pending,
                    mount_state: MountState::Pending,
                },
            ],
        };
        let summary =
            summarize_archive_index(&parse_archive_index_json(&archive_index_to_json(&index)));

        assert_eq!(summary.archives_count, 2);
        assert_eq!(summary.mounted_count, 1);
        assert_eq!(summary.pending_count, 1);
        assert_eq!(
            summary.platform_counts,
            vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 1),]
        );
    }

    #[test]
    fn archive_index_find_searches_all_requested_fields_case_insensitively() {
        let index = sample_index_for_find();

        assert_eq!(find_archive_index_entries(&index, "007").len(), 1);
        assert_eq!(find_archive_index_entries(&index, "legends").len(), 1);
        assert_eq!(find_archive_index_entries(&index, "xbox360").len(), 1);
        assert_eq!(
            find_archive_index_entries(&index, "unknown/mystery").len(),
            1
        );
        assert_eq!(find_archive_index_entries(&index, "MYSTERY").len(), 1);
    }

    #[test]
    fn archive_index_find_returns_empty_for_no_matches() {
        let index = sample_index_for_find();

        assert!(find_archive_index_entries(&index, "not present").is_empty());
    }

    #[test]
    fn archive_index_json_round_trips_for_find() {
        let index = sample_index_for_find();
        let parsed = parse_archive_index_json(&archive_index_to_json(&index));

        assert_eq!(find_archive_index_entries(&parsed, "xbox360").len(), 1);
        assert_eq!(find_archive_index_entries(&parsed, "mystery").len(), 1);
    }

    #[test]
    fn clean_empty_dirs_removes_empty_dirs_but_not_root_or_nonempty_dirs() {
        let root = test_root("clean_empty_dirs");
        let empty_child = root.join("Unknown").join("Empty");
        let nonempty_child = root.join("Xbox360").join("Keep");
        fs::create_dir_all(&empty_child).unwrap();
        fs::create_dir_all(&nonempty_child).unwrap();
        fs::write(nonempty_child.join("file.txt"), b"keep").unwrap();

        let removed = clean_empty_dirs_under(&root, &HashSet::new()).unwrap();

        assert!(root.exists());
        assert!(!empty_child.exists());
        assert!(!root.join("Unknown").exists());
        assert!(nonempty_child.exists());
        assert!(removed.contains(&empty_child));
        assert!(removed.contains(&root.join("Unknown")));
        assert!(!removed.contains(&root));
    }

    #[test]
    fn clean_empty_dirs_skips_mounted_dirs_and_their_children() {
        let root = test_root("clean_mounted_dirs");
        let mounted = root.join("Xbox360").join("Mounted");
        let virtual_empty = mounted.join("Empty");
        fs::create_dir_all(&virtual_empty).unwrap();
        let mounted_paths = HashSet::from([mounted.clone()]);

        let removed = clean_empty_dirs_under(&root, &mounted_paths).unwrap();

        assert!(mounted.exists());
        assert!(virtual_empty.exists());
        assert!(!removed.contains(&mounted));
        assert!(!removed.contains(&virtual_empty));
    }

    #[test]
    fn archive_index_freshness_detects_missing_archive_paths() {
        let index = ArchiveIndex {
            archives: vec![ArchiveIndexEntry {
                archive_path: PathBuf::from("/definitely/missing/archive.zip"),
                platform: Some("Xbox360".to_string()),
                display_name: "Missing".to_string(),
                mount_path: PathBuf::from("/mnt/archivefs/Xbox360/Missing"),
                modified_time_seconds: Some(1),
                health: ArchiveHealth::Pending,
                mount_state: MountState::Pending,
            }],
        };

        let freshness = check_archive_index_freshness(&index);

        assert_eq!(
            freshness.missing_archive_paths,
            vec![PathBuf::from("/definitely/missing/archive.zip")]
        );
        assert!(freshness.stale_archive_paths.is_empty());
        assert!(freshness.has_warnings());
    }

    #[test]
    fn archive_index_freshness_detects_stale_modified_time() {
        let root = test_root("index_stale_modified_time");
        let archive_path = root.join("game.zip");
        fs::write(&archive_path, b"game").unwrap();
        let index = ArchiveIndex {
            archives: vec![ArchiveIndexEntry {
                archive_path: archive_path.clone(),
                platform: None,
                display_name: "game".to_string(),
                mount_path: root.join("Unknown").join("game"),
                modified_time_seconds: Some(0),
                health: ArchiveHealth::Pending,
                mount_state: MountState::Pending,
            }],
        };

        let freshness = check_archive_index_freshness(&index);

        assert!(freshness.missing_archive_paths.is_empty());
        assert_eq!(freshness.stale_archive_paths, vec![archive_path]);
        assert!(freshness.has_warnings());
    }

    #[test]
    fn write_archive_index_creates_parent_dirs_and_writes_readable_index() {
        let root = test_root("write_index_parent_dirs");
        let index_path = root.join("nested").join("index.json");
        let index = sample_index_for_find();

        write_archive_index(&index, &index_path).unwrap();
        let parsed = read_archive_index(&index_path).unwrap();

        assert!(index_path.exists());
        assert_eq!(parsed.archives.len(), 2);
        assert_eq!(find_archive_index_entries(&parsed, "007").len(), 1);
    }

    #[test]
    fn cleanup_selected_mount_dir_removes_empty_selected_dir() {
        let root = test_root("cleanup_selected_empty");
        let mount_path = root.join("Xbox360").join("Game");
        fs::create_dir_all(&mount_path).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(cleanup_selected_mount_dir(&config, &mount_path).unwrap());
        assert!(!mount_path.exists());
        assert!(root.exists());
    }

    #[test]
    fn cleanup_selected_mount_dir_never_removes_mount_root() {
        let root = test_root("cleanup_selected_root");
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(!cleanup_selected_mount_dir(&config, &root).unwrap());
        assert!(root.exists());
    }

    #[test]
    fn cleanup_selected_mount_dir_keeps_non_empty_dir() {
        let root = test_root("cleanup_selected_nonempty");
        let mount_path = root.join("Xbox360").join("Game");
        fs::create_dir_all(&mount_path).unwrap();
        fs::write(mount_path.join("file.txt"), b"keep").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(!cleanup_selected_mount_dir(&config, &mount_path).unwrap());
        assert!(mount_path.exists());
    }

    #[test]
    fn cleanup_selected_mount_dir_ignores_paths_outside_mount_root() {
        let root = test_root("cleanup_selected_outside_root");
        let outside = test_root("cleanup_selected_outside_target").join("Game");
        fs::create_dir_all(&outside).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(!cleanup_selected_mount_dir(&config, &outside).unwrap());
        assert!(outside.exists());
    }

    #[test]
    fn cleanup_selected_mount_tree_removes_empty_ancestors_but_not_mount_root() {
        let root = test_root("cleanup_selected_tree");
        let platform_path = root.join("Xbox360");
        let mount_path = platform_path.join("Game");
        fs::create_dir_all(&mount_path).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        let removed = cleanup_selected_mount_tree(&config, &mount_path).unwrap();

        assert_eq!(removed, vec![mount_path.clone(), platform_path.clone()]);
        assert!(!mount_path.exists());
        assert!(!platform_path.exists());
        assert!(root.exists());
    }

    #[test]
    fn cleanup_selected_mount_tree_rejects_mount_root_itself() {
        let root = test_root("cleanup_selected_tree_root");
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(
            cleanup_selected_mount_tree(&config, &root)
                .unwrap()
                .is_empty()
        );
        assert!(root.exists());
    }

    #[test]
    fn cleanup_selected_mount_tree_rejects_paths_outside_mount_root() {
        let root = test_root("cleanup_selected_tree_outside_root");
        let outside = test_root("cleanup_selected_tree_outside_target").join("Game");
        fs::create_dir_all(&outside).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(
            cleanup_selected_mount_tree(&config, &outside)
                .unwrap()
                .is_empty()
        );
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_selected_mount_tree_rejects_symlink_resolving_outside_mount_root() {
        use std::os::unix::fs::symlink;

        let root = test_root("cleanup_selected_tree_symlink_root");
        let outside = test_root("cleanup_selected_tree_symlink_target");
        let outside_mount = outside.join("Game");
        fs::create_dir_all(&outside_mount).unwrap();
        symlink(&outside, root.join("Platform")).unwrap();
        let linked_mount = root.join("Platform").join("Game");
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root,
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(
            cleanup_selected_mount_tree(&config, &linked_mount)
                .unwrap()
                .is_empty()
        );
        assert!(outside_mount.exists());
    }

    #[test]
    fn cleanup_selected_mount_tree_preserves_non_empty_directories() {
        let root = test_root("cleanup_selected_tree_nonempty");
        let mount_path = root.join("Xbox360").join("Game");
        fs::create_dir_all(&mount_path).unwrap();
        fs::write(mount_path.join("file.txt"), b"keep").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        assert!(
            cleanup_selected_mount_tree(&config, &mount_path)
                .unwrap()
                .is_empty()
        );
        assert!(mount_path.exists());
        assert!(root.exists());
    }

    #[test]
    fn cleanup_selected_mount_tree_preserves_parent_with_another_archive_directory() {
        let root = test_root("cleanup_selected_tree_sibling");
        let platform_path = root.join("Xbox360");
        let mount_path = platform_path.join("RemovedGame");
        let other_mount_path = platform_path.join("OtherGame");
        fs::create_dir_all(&mount_path).unwrap();
        fs::create_dir_all(&other_mount_path).unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.clone(),
            ratarmount_bin: "ratarmount".to_string(),
        };

        let removed = cleanup_selected_mount_tree(&config, &mount_path).unwrap();

        assert_eq!(removed, vec![mount_path.clone()]);
        assert!(!mount_path.exists());
        assert!(other_mount_path.exists());
        assert!(platform_path.exists());
        assert!(root.exists());
    }

    #[test]
    fn archive_health_marks_retryable_states() {
        assert!(ArchiveHealth::Failed.is_retryable());
        assert!(ArchiveHealth::MissingParts.is_retryable());
        assert!(ArchiveHealth::RetryAvailable.is_retryable());
        assert!(!ArchiveHealth::Pending.is_retryable());
        assert!(!ArchiveHealth::Mounted.is_retryable());
    }

    #[test]
    fn archive_health_marks_terminal_states() {
        assert!(ArchiveHealth::Corrupt.is_terminal_without_source_change());
        assert!(ArchiveHealth::Unsupported.is_terminal_without_source_change());
        assert!(ArchiveHealth::PermissionDenied.is_terminal_without_source_change());
        assert!(!ArchiveHealth::Failed.is_terminal_without_source_change());
    }

    #[test]
    fn detects_platform_from_known_source_path_segments() {
        assert_eq!(
            detect_platform("/roms/microsoft_xbox/Halo.zip", "/roms"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/roms/xbox360/Halo 3.zip", "/roms"),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/collections/Atari ST/Gem.zip", "/collections"),
            Some("AtariST".to_string())
        );
        assert_eq!(
            detect_platform("/collections/Atari-2600/Pitfall.zip", "/collections"),
            Some("Atari2600".to_string())
        );
        assert_eq!(detect_platform("/roms/unknown/game.zip", "/roms"), None);
    }

    #[test]
    fn detects_platform_from_collection_style_xbox_segments() {
        assert_eq!(
            detect_platform(
                "/collections/microsoft_xbox360_f_part1/Game.zip",
                "/collections"
            ),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/collections/microsoft_xbox_f/Game.zip", "/collections"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/collections/microsoft_xbox_j/Game.zip", "/collections"),
            Some("Xbox".to_string())
        );
    }

    #[test]
    fn detects_platform_from_title_and_release_heuristics() {
        assert_eq!(
            detect_platform("/incoming/007 Legends.zip", "/incoming"),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform(
                "/incoming/Mortal Kombat - Komplete Edition.rar",
                "/incoming",
            ),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Fable (USA, Europe).7z", "/incoming"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Gameboy Advance CIAs/Metroid.zip", "/incoming"),
            Some("Nintendo3DS".to_string())
        );
        assert_eq!(
            detect_platform("/downloads/I.Am.Jesus.Christ.zip", "/downloads"),
            Some("PC".to_string())
        );
        assert_eq!(
            detect_platform("/downloads/SteamRIP/Example.zip", "/downloads"),
            Some("PC".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Metal Gear Solid - Peace Walker.zip", "/incoming",),
            Some("PSP".to_string())
        );
        assert_eq!(
            detect_platform("/sets/Atari-2600-VCS-ROM-Collection/archive.zip", "/sets",),
            Some("Atari2600".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/random-game.zip", "/incoming"),
            None
        );
    }

    #[test]
    fn archive_identity_stores_detected_platform() {
        let archive =
            Archive::from_path_in_root("/roms/microsoft_xbox360/Halo 3.zip", "/roms").unwrap();

        assert_eq!(archive.identity.platform, Some("Xbox360".to_string()));
        assert_eq!(
            archive.identity.platform_provenance,
            Some(PlatformProvenance::Heuristic)
        );
    }

    // -----------------------------------------------------------------
    // Folder-based platform detection (alias-map fallback).
    // -----------------------------------------------------------------

    #[test]
    fn msx2_folder_detects_msx2() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/msx2/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("MSX2".to_string())
        );
    }

    #[test]
    fn neogeo_folder_detects_neo_geo() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/neogeo/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("NeoGeo".to_string())
        );
    }

    #[test]
    fn neogeo64_folder_detects_neo_geo_64() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/neogeo64/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("NeoGeo64".to_string())
        );
    }

    #[test]
    fn ngage_folder_detects_ngage_for_genuine_top_level_content() {
        // A loose N-Gage archive sitting directly under the "ngage"
        // category folder (not packaged as a *.ngage container directory)
        // must still resolve via the folder alias.
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/ngage/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("NGage".to_string())
        );
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/Nokia N-Gage/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("NGage".to_string())
        );
    }

    #[test]
    fn luigis_mansion_with_no_platform_hint_stays_unknown() {
        // Reproduces the Nobara report: a loose archive sitting directly
        // in the source root, no platform subfolder and no strong
        // filename hint - must not be swept up by any alias or heuristic.
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/Luigis_Mansion_[hexrom.com].zip",
                "/home/davedap/Archives"
            ),
            None
        );
    }

    #[test]
    fn intellivision_folder_detects_intellivision() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/intellivision/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("Intellivision".to_string())
        );
    }

    #[test]
    fn folder_alias_matching_is_case_and_separator_insensitive() {
        let root = "/home/davedap/Archives";
        for folder in ["msx2", "MSX2", "MSX 2", "msx_2", "Msx-2"] {
            assert_eq!(
                detect_platform(format!("{root}/{folder}/Game.zip"), root),
                Some("MSX2".to_string()),
                "folder spelling {folder:?} should detect MSX2"
            );
        }

        assert_eq!(
            detect_platform(format!("{root}/sony-playstation-2/Game.zip"), root),
            Some("PS2".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Nintendo GameCube/Game.zip"), root),
            Some("GameCube".to_string())
        );
    }

    #[test]
    fn nearest_matching_parent_folder_wins_over_a_higher_one() {
        // "ps2" (nearer) must win over "gamecube" (further from the file),
        // even though both are valid aliases - see requirement 2's
        // detection order.
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/gamecube/extras/ps2/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("PS2".to_string())
        );
    }

    #[test]
    fn a_higher_parent_folder_is_used_when_the_nearest_one_does_not_match() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/msx2/subfolder/extras/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("MSX2".to_string())
        );
    }

    #[test]
    fn absolute_path_components_outside_source_root_are_ignored() {
        // The source root itself is literally named "Archives", nested
        // under "davedap" and "home" - none of those may ever influence
        // detection, only "msx2" (a descendant of the configured root).
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/msx2/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("MSX2".to_string())
        );

        // A source root that is not itself under any alias-looking
        // ancestor must not spuriously detect one either - sanity check
        // that stripping the root is doing real work, not accidentally
        // matching on the full absolute path.
        assert_eq!(
            detect_platform("/home/davedap/msx2/game.zip", "/home/davedap/msx2"),
            None,
            "the source root's own name must never itself count as a folder hint"
        );
    }

    #[test]
    fn filename_detection_remains_stronger_than_the_folder_fallback() {
        // "/incoming/Fable (USA, Europe).7z" is inside an "incoming"
        // folder with no platform hint, but the existing title heuristic
        // still (correctly) detects Xbox - the folder fallback must never
        // run at all when the heuristic already found something,
        // regardless of what the folder path would have suggested.
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/psp/007 Legends.zip",
                "/home/davedap/Archives"
            ),
            Some("Xbox360".to_string()),
            "the known-title heuristic for \"007 Legends\" must win over the \
             \"psp\" folder alias"
        );
    }

    #[test]
    fn unknown_folder_remains_unknown() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/misc/Game.zip",
                "/home/davedap/Archives"
            ),
            None
        );
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/roms/backup/Game.zip",
                "/home/davedap/Archives"
            ),
            None
        );
    }

    #[test]
    fn ambiguous_vague_folder_names_do_not_false_positive() {
        // "genesis" is a real, requested alias (MegaDrive) - but a folder
        // literally containing the substring is not a match unless the
        // *whole* normalized component equals a known alias. "genesis"
        // itself is deliberately excluded here: this checks compound
        // names that merely *contain* an alias substring are not matched.
        for folder in ["genesis-of-a-nightmare", "my-nes-notes", "megadrivetools"] {
            assert_eq!(
                detect_platform(
                    format!("/home/davedap/Archives/{folder}/Game.zip"),
                    "/home/davedap/Archives"
                ),
                None,
                "{folder:?} merely contains an alias substring and must not match"
            );
        }
    }

    #[test]
    fn provenance_is_folder_alias_for_the_fallback_and_heuristic_for_the_existing_path() {
        let heuristic = detect_platform_with_provenance("/roms/xbox360/Halo.zip", "/roms")
            .expect("known heuristic should detect Xbox360");
        assert_eq!(heuristic.provenance, PlatformProvenance::Heuristic);
        assert_eq!(
            heuristic.provenance.as_source_str(),
            "heuristic-path-detector"
        );

        let folder_alias = detect_platform_with_provenance(
            "/home/davedap/Archives/msx2/Game.zip",
            "/home/davedap/Archives",
        )
        .expect("folder alias should detect MSX2");
        assert_eq!(folder_alias.provenance, PlatformProvenance::FolderAlias);
        assert_eq!(folder_alias.provenance.as_source_str(), "folder_alias");
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_components_do_not_panic_on_unix() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // "unknownfolder" plus one invalid byte in the middle - the
        // invalid byte is dropped entirely by normalize_path_segment's
        // ASCII-alphanumeric filter (lossy replacement characters are not
        // alphanumeric), leaving "unknownfolder", which does not match
        // any alias either way. The point of this test is that a
        // non-UTF-8 component never panics, regardless of how it
        // normalizes.
        let mut folder_bytes = b"unknown".to_vec();
        folder_bytes.push(0x80); // never valid UTF-8 on its own.
        folder_bytes.extend_from_slice(b"folder");
        let folder = OsString::from_vec(folder_bytes);

        let root = PathBuf::from("/home/davedap/Archives");
        let mut path = root.clone();
        path.push(&folder);
        path.push("Game.zip");
        assert!(
            path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );

        assert_eq!(detect_platform(&path, &root), None);
    }

    #[test]
    fn real_nested_console_paths_behave_predictably() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/console/ps2/game.zip",
                "/home/davedap/Archives"
            ),
            Some("PS2".to_string())
        );
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/console/dreamcast/game.zip",
                "/home/davedap/Archives"
            ),
            Some("Dreamcast".to_string())
        );
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/console/unknown-thing/game.zip",
                "/home/davedap/Archives"
            ),
            None
        );
    }

    #[test]
    fn folder_alias_table_detects_every_listed_canonical_platform() {
        let cases: &[(&str, &str)] = &[
            ("msx", "MSX"),
            ("msx2", "MSX2"),
            ("neogeo", "NeoGeo"),
            ("neogeo64", "NeoGeo64"),
            ("ngage", "NGage"),
            ("intellivision", "Intellivision"),
            ("amiga", "Amiga"),
            ("amigacd32", "AmigaCD32"),
            ("atarist", "AtariST"),
            ("atari2600", "Atari2600"),
            ("atari5200", "Atari5200"),
            ("atari7800", "Atari7800"),
            ("nes", "NES"),
            ("snes", "SNES"),
            ("n64", "N64"),
            ("gamecube", "GameCube"),
            ("wii", "Wii"),
            ("wiiu", "WiiU"),
            ("switch", "Switch"),
            ("megadrive", "MegaDrive"),
            ("genesis", "MegaDrive"),
            ("mastersystem", "MasterSystem"),
            ("gamegear", "GameGear"),
            ("saturn", "Saturn"),
            ("dreamcast", "Dreamcast"),
            ("psx", "PSX"),
            ("ps1", "PSX"),
            ("ps2", "PS2"),
            ("ps3", "PS3"),
            ("psp", "PSP"),
            ("xbox", "Xbox"),
            ("xbox360", "Xbox360"),
            ("arcade", "Arcade"),
            ("mame", "Arcade"),
            ("dos", "DOS"),
            ("scummvm", "ScummVM"),
        ];

        let root = "/home/davedap/Archives";
        for (folder, expected_platform) in cases {
            assert_eq!(
                detect_platform(format!("{root}/{folder}/Game.zip"), root),
                Some((*expected_platform).to_string()),
                "folder {folder:?} should detect {expected_platform:?}"
            );
        }
    }

    #[test]
    fn canonical_platform_names_are_sorted_deduplicated_and_include_new_aliases() {
        let names = canonical_platform_names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "canonical_platform_names must already be sorted"
        );

        let mut deduplicated = names.clone();
        deduplicated.dedup();
        assert_eq!(
            names.len(),
            deduplicated.len(),
            "canonical_platform_names must not contain duplicates"
        );

        for expected in ["NeoGeo64", "NGage", "GameCube", "MSX2", "Xbox360"] {
            assert!(
                names.contains(&expected),
                "{expected:?} should be a canonical platform name"
            );
        }
    }

    // -----------------------------------------------------------------
    // Container/game-directory scanner boundary (e.g. *.ngage).
    // -----------------------------------------------------------------

    #[test]
    fn is_container_directory_matches_ngage_case_insensitively() {
        for name in [
            "Glimmerati.ngage",
            "SSX Out Of Bounds.ngage",
            "game.NGAGE",
            "Game.NgAge",
        ] {
            assert!(
                is_container_directory(name),
                "{name:?} should be recognised as a container directory"
            );
        }
        for name in ["Glimmerati", "ngage", "notngage", "System", "data.zip"] {
            assert!(
                !is_container_directory(name),
                "{name:?} should not be recognised as a container directory"
            );
        }
    }

    fn scanner_config(root: &Path) -> Config {
        Config {
            source_folders: vec![root.to_path_buf()],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
        }
    }

    #[test]
    fn nested_zips_beneath_dot_ngage_are_skipped() {
        let root = test_root("ngage_container_boundary");
        let game_dir = root
            .join("ngage")
            .join("Glimmerati.ngage")
            .join("System")
            .join("Apps")
            .join("Glimmerati");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("data.zip"), b"").unwrap();

        let archives = ArchiveScanner::new(&scanner_config(&root))
            .scan_archives()
            .unwrap();

        assert!(
            archives.is_empty(),
            "the only archive present is internal N-Gage payload and must not be scanned: {archives:?}"
        );
    }

    #[test]
    fn ordinary_nested_archives_outside_container_directories_are_still_scanned() {
        let root = test_root("ordinary_nested_scan_unaffected");
        let deep = root.join("psp").join("collection").join("extras");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Game.zip"), b"").unwrap();

        let archives = ArchiveScanner::new(&scanner_config(&root))
            .scan_archives()
            .unwrap();

        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].path, deep.join("Game.zip"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_sibling_paths_do_not_panic_during_container_boundary_scan() {
        use std::os::unix::ffi::OsStringExt;

        let root = test_root("ngage_non_utf8_sibling");
        let ngage_dir = root.join("ngage");
        fs::create_dir_all(&ngage_dir).unwrap();

        let container = ngage_dir.join("Glimmerati.ngage");
        fs::create_dir_all(container.join("System")).unwrap();
        fs::write(container.join("System").join("data.zip"), b"").unwrap();

        // A sibling directory whose name is not valid UTF-8, sitting right
        // next to the container directory - must not cause a panic, and
        // must be scanned normally (it is not itself a container).
        let mut invalid_name = b"broken-".to_vec();
        invalid_name.push(0x80);
        let invalid_dir = ngage_dir.join(std::ffi::OsString::from_vec(invalid_name));
        fs::create_dir_all(&invalid_dir).unwrap();
        fs::write(invalid_dir.join("Game.zip"), b"").unwrap();

        let archives = ArchiveScanner::new(&scanner_config(&root))
            .scan_archives()
            .unwrap();

        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].path, invalid_dir.join("Game.zip"));
    }

    #[test]
    fn nobara_reproduction_matches_the_reported_directory_shape() {
        let root = test_root("nobara_reproduction");

        // 1. Luigi's Mansion: no platform folder, no strong filename hint.
        fs::write(root.join("Luigis_Mansion_[hexrom.com].zip"), b"").unwrap();

        // 2. neogeo64/: a genuine top-level archive under the folder alias.
        let neogeo64_dir = root.join("neogeo64");
        fs::create_dir_all(&neogeo64_dir).unwrap();
        fs::write(neogeo64_dir.join("Game.zip"), b"").unwrap();

        // 3. ngage/: two *.ngage game-container directories, each with a
        //    nested internal resource zip that must not be scanned.
        let glimmerati_data = root
            .join("ngage")
            .join("Glimmerati.ngage")
            .join("System")
            .join("Apps")
            .join("Glimmerati");
        fs::create_dir_all(&glimmerati_data).unwrap();
        fs::write(glimmerati_data.join("data.zip"), b"").unwrap();

        let ssx_data = root
            .join("ngage")
            .join("SSX Out Of Bounds.ngage")
            .join("System")
            .join("Apps")
            .join("SSXOutOfBounds");
        fs::create_dir_all(&ssx_data).unwrap();
        fs::write(ssx_data.join("data.zip"), b"").unwrap();
        fs::write(ssx_data.join("resource.zip"), b"").unwrap();

        let config = scanner_config(&root);
        let scanner = ArchiveScanner::new(&config);
        let archives = scanner.scan_archives().unwrap();

        let paths: Vec<_> = archives
            .iter()
            .map(|archive| archive.path.clone())
            .collect();
        assert_eq!(
            paths,
            vec![
                root.join("Luigis_Mansion_[hexrom.com].zip"),
                neogeo64_dir.join("Game.zip"),
            ],
            "only the loose Luigi's Mansion archive and the neogeo64 archive should be scanned; \
             all three internal N-Gage payload zips must be excluded"
        );

        let records = scanner.archive_records().unwrap();
        let luigi = records
            .iter()
            .find(|record| {
                record
                    .mount_plan
                    .archive
                    .path
                    .ends_with("Luigis_Mansion_[hexrom.com].zip")
            })
            .expect("Luigi's Mansion should be scanned");
        assert_eq!(
            luigi.identity.platform, None,
            "Luigi's Mansion must stay Unknown for now"
        );

        let neogeo64 = records
            .iter()
            .find(|record| record.mount_plan.archive.path == neogeo64_dir.join("Game.zip"))
            .expect("neogeo64 archive should be scanned");
        assert_eq!(neogeo64.identity.platform, Some("NeoGeo64".to_string()));
    }

    #[test]
    fn container_boundary_reduces_scan_count_by_exactly_the_nested_archives() {
        // Build the identical tree twice, differing only in whether the
        // per-game directories are named as recognised *.ngage containers
        // or as an ordinary (non-container) directory name. The scanned
        // count must drop by exactly the two nested payload archives -
        // nothing else in the tree is affected by the boundary.
        fn build_tree(root: &Path, game_dir_name: &str) {
            for game in ["Glimmerati", "SSX Out Of Bounds"] {
                let data_dir = root
                    .join("ngage")
                    .join(format!("{game}{game_dir_name}"))
                    .join("System")
                    .join("Apps")
                    .join(game.replace(' ', ""));
                fs::create_dir_all(&data_dir).unwrap();
                fs::write(data_dir.join("data.zip"), b"").unwrap();
            }
            fs::write(root.join("Loose.zip"), b"").unwrap();
        }

        let without_boundary_root = test_root("ngage_count_without_boundary");
        build_tree(&without_boundary_root, ".notacontainer");
        let without_boundary_count = ArchiveScanner::new(&scanner_config(&without_boundary_root))
            .scan_archives()
            .unwrap()
            .len();

        let with_boundary_root = test_root("ngage_count_with_boundary");
        build_tree(&with_boundary_root, ".ngage");
        let with_boundary_count = ArchiveScanner::new(&scanner_config(&with_boundary_root))
            .scan_archives()
            .unwrap()
            .len();

        assert_eq!(
            without_boundary_count, 3,
            "sanity check: 2 nested + 1 loose archive"
        );
        assert_eq!(
            with_boundary_count,
            without_boundary_count - 2,
            "the count must drop by exactly the two nested N-Gage payload archives"
        );
    }

    #[test]
    fn mount_plan_generation_carries_archive_identity_and_pending_state() {
        let archives = vec![archive("/roms/collection/Resident Evil 2.zip")];
        let plans = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].archive.path,
            PathBuf::from("/roms/collection/Resident Evil 2.zip")
        );
        assert_eq!(plans[0].archive.kind, ArchiveKind::Zip);
        assert_eq!(plans[0].archive.identity.normalized_name, "resident_evil_2");
        assert_eq!(
            plans[0].mount_path,
            PathBuf::from("/mnt/archivefs/Unknown/Resident_Evil_2")
        );
        assert_eq!(plans[0].state, MountState::Pending);
    }

    #[test]
    fn doctor_reports_missing_config() {
        let root = test_root("doctor_missing_config");
        let config_path = root.join("missing.toml");
        let report = run_doctor(&config_path);

        assert!(!report.is_ready());
        assert_eq!(report.archives_found, 0);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "config file" && check.status == DoctorStatus::Fail)
        );
    }

    #[test]
    fn doctor_counts_archives_platforms_and_pending_mounts() {
        let root = test_root("doctor_counts");
        let source_root = root.join("roms");
        let xbox = source_root.join("microsoft_xbox");
        let unknown = source_root.join("unknown");
        let mount_root = root.join("mounts");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&xbox).unwrap();
        fs::create_dir_all(&unknown).unwrap();
        fs::write(xbox.join("Halo.zip"), b"").unwrap();
        fs::write(unknown.join("Mystery.7z"), b"").unwrap();
        fs::write(&ratarmount, b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                source_root.display(),
                mount_root.display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let report = run_doctor(&config_path);

        assert_eq!(report.archives_found, 2);
        assert_eq!(report.archives_with_platform, 1);
        assert_eq!(report.archives_unknown_platform, 1);
        assert_eq!(
            report.unknown_platform_examples,
            vec![unknown.join("Mystery.7z")]
        );
        assert_eq!(report.platform_counts, vec![("Xbox".to_string(), 1)]);
        assert_eq!(report.pending_archives, 2);
        assert_eq!(report.mounted_archives, 0);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "config parses" && check.status == DoctorStatus::Pass)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "archive scan" && check.status == DoctorStatus::Pass)
        );
    }

    #[test]
    fn read_only_snapshot_matches_existing_stats_statuses_and_doctor_counts() {
        let root = test_root("read_only_snapshot");
        let source_root = root.join("roms");
        let xbox = source_root.join("microsoft_xbox");
        let unknown = source_root.join("unknown");
        let mount_root = root.join("mounts");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&xbox).unwrap();
        fs::create_dir_all(&unknown).unwrap();
        fs::create_dir_all(&mount_root).unwrap();
        fs::write(xbox.join("Halo.zip"), b"halo").unwrap();
        fs::write(unknown.join("Mystery.7z"), b"mystery").unwrap();
        fs::write(&ratarmount, b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                source_root.display(),
                mount_root.display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let snapshot = load_read_only_snapshot(&config_path).unwrap();
        let config = Config::load_from(&config_path).unwrap();
        let expected_stats = current_archive_stats(&config).unwrap();
        let expected_statuses = current_statuses(&config).unwrap();
        let expected_doctor = run_doctor_read_only(&config_path);

        assert_eq!(snapshot.mount_root, mount_root);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.stats, expected_stats);
        assert_eq!(snapshot.statuses, expected_statuses);
        assert_eq!(
            snapshot.doctor.archives_found,
            expected_doctor.archives_found
        );
        assert_eq!(
            snapshot.doctor.archives_with_platform,
            expected_doctor.archives_with_platform
        );
        assert_eq!(
            snapshot.doctor.archives_unknown_platform,
            expected_doctor.archives_unknown_platform
        );
        assert_eq!(
            snapshot.doctor.platform_counts,
            expected_doctor.platform_counts
        );
        assert_eq!(
            snapshot.doctor.pending_archives,
            expected_doctor.pending_archives
        );
        assert_eq!(
            snapshot.doctor.mounted_archives,
            expected_doctor.mounted_archives
        );
    }

    #[test]
    fn read_only_doctor_does_not_create_missing_mount_root() {
        let root = test_root("doctor_read_only");
        let source_root = root.join("roms");
        let mount_root = root.join("missing-mounts");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(&ratarmount, b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                source_root.display(),
                mount_root.display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let report = run_doctor_read_only(&config_path);

        assert!(!mount_root.exists());
        assert!(report.checks.iter().any(|check| {
            check.name == "mount root"
                && check.status == DoctorStatus::Fail
                && check.detail.ends_with("does not exist")
        }));
    }

    #[test]
    fn multiline_source_folders_with_single_entry_parses() {
        let contents = concat!(
            "source_folders = [\n",
            "  \"/home/user/Archives\"\n",
            "]\n",
            "mount_root = \"/mnt/archivefs\"\n",
        );

        let config = parse_config(contents).unwrap();

        assert_eq!(
            config.source_folders,
            vec![PathBuf::from("/home/user/Archives")]
        );
    }

    #[test]
    fn multiline_source_folders_with_multiple_entries_and_trailing_comma_parses() {
        let contents = concat!(
            "source_folders = [\n",
            "  \"/data/archives\",\n",
            "  \"/mnt/other\",\n",
            "]\n",
            "mount_root = \"/mnt/archivefs\"\n",
        );

        let config = parse_config(contents).unwrap();

        assert_eq!(
            config.source_folders,
            vec![PathBuf::from("/data/archives"), PathBuf::from("/mnt/other")]
        );
    }

    #[test]
    fn multiline_source_folders_with_comment_on_continuation_line_parses() {
        let contents = concat!(
            "source_folders = [\n",
            "  \"/data/archives\", # primary library\n",
            "  \"/mnt/other\"\n",
            "]\n",
            "mount_root = \"/mnt/archivefs\"\n",
        );

        let config = parse_config(contents).unwrap();

        assert_eq!(
            config.source_folders,
            vec![PathBuf::from("/data/archives"), PathBuf::from("/mnt/other")]
        );
    }

    #[test]
    fn unclosed_multiline_source_folders_array_is_a_config_error() {
        let contents = concat!(
            "source_folders = [\n",
            "  \"/data/archives\"\n",
            "mount_root = \"/mnt/archivefs\"\n",
        );

        let error = parse_config(contents).unwrap_err();

        assert!(error.to_string().contains("never closed"));
    }

    #[test]
    fn single_line_source_folders_array_is_unaffected_by_multiline_support() {
        let contents = "source_folders = [\"/data/archives\", \"/mnt/other\"]\nmount_root = \"/mnt/archivefs\"\n";

        let config = parse_config(contents).unwrap();

        assert_eq!(
            config.source_folders,
            vec![PathBuf::from("/data/archives"), PathBuf::from("/mnt/other")]
        );
    }

    #[test]
    fn shipped_config_toml_example_parses_with_the_real_parser() {
        // Reads the repository's actual config.toml.example (not a copy
        // of its contents) so this test fails if the shipped file and
        // the parser ever drift apart. No real $HOME or filesystem paths
        // are touched - this only reads a file already in the repo.
        let example_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.toml.example");
        let contents = fs::read_to_string(&example_path).unwrap_or_else(|error| {
            panic!(
                "failed to read shipped example config at {}: {error}",
                example_path.display()
            )
        });

        let config = parse_config(&contents).expect(
            "config.toml.example must parse with the real ArchiveFS config parser - \
             if you changed the parser or the example, keep both in sync",
        );

        assert_eq!(config.source_folders, vec![PathBuf::from("/data/archives")]);
        assert_eq!(config.mount_root, PathBuf::from("/mnt/archivefs"));
        assert_eq!(config.ratarmount_bin, "ratarmount");
    }

    #[test]
    fn config_check_reports_empty_sources_as_validation_error() {
        let root = test_root("config_check_empty_sources");
        let config_path = root.join("config.toml");
        let ratarmount = root.join("ratarmount");
        fs::write(&ratarmount, b"").unwrap();
        fs::write(
            &config_path,
            format!(
                "source_folders = []\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                root.join("mounts").display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let report = run_config_check(&config_path);

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "config parses"
                    && check.status == ConfigCheckStatus::Pass)
        );
        assert!(report.checks.iter().any(|check| {
            check.name == "source_folders not empty" && check.status == ConfigCheckStatus::Error
        }));
        assert!(!report.is_ok());
    }

    #[test]
    fn config_check_warns_for_duplicate_source_folders() {
        let root = test_root("config_check_duplicate_sources");
        let source = root.join("roms");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&source).unwrap();
        fs::write(&ratarmount, b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\", \"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                source.display(),
                source.display(),
                root.join("mounts").display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let report = run_config_check(&config_path);

        assert_eq!(report.warning_count(), 1);
        assert!(report.checks.iter().any(|check| {
            check.name == "duplicate source folder" && check.status == ConfigCheckStatus::Warn
        }));
        assert!(
            !report
                .checks
                .iter()
                .any(|check| check.name == "source folder exists"
                    && check.status == ConfigCheckStatus::Error)
        );
    }

    #[test]
    fn starter_config_creates_parents_and_never_overwrites() {
        let root = test_root("starter_config");
        let path = root.join("nested/archivefs/config.toml");

        create_starter_config(&path).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("source_folders"));
        assert!(contents.contains("mount_root"));
        assert!(contents.contains("ratarmount_bin"));
        assert!(create_starter_config(&path).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    #[test]
    fn setup_diagnostics_report_invalid_sources_and_missing_tools() {
        let root = test_root("setup_diagnostics_missing");
        let mount_root = root.join("mounts");
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                root.join("missing-roms").display(),
                mount_root.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| false);

        assert!(!report.ready_for_scanning);
        assert!(!report.ready_for_actions);
        assert!(report.checks.iter().any(|check| {
            check.name == "Configured source folder exists"
                && check.status == SetupDiagnosticStatus::Error
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "ratarmount is available" && check.status == SetupDiagnosticStatus::Error
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "fusermount3 or umount is available"
                && check.status == SetupDiagnosticStatus::Error
        }));
    }

    #[test]
    fn setup_diagnostics_offer_mount_root_creation_only_with_valid_parent() {
        let root = test_root("setup_mount_root_offer");
        let source = root.join("roms");
        fs::create_dir(&source).unwrap();
        let config_path = root.join("config.toml");
        let mount_root = root.join("mounts");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                source.display(),
                mount_root.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert!(report.can_create_mount_root);
        let config = Config::load_from(&config_path).unwrap();
        create_configured_mount_root(&config).unwrap();
        assert!(mount_root.is_dir());
        assert!(create_configured_mount_root(&config).is_err());

        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                source.display(),
                root.join("missing-parent/mounts").display()
            ),
        )
        .unwrap();
        let unsafe_report = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert!(!unsafe_report.can_create_mount_root);
    }

    #[test]
    fn setup_diagnostics_only_mark_confirmed_missing_config_as_missing() {
        let actual_missing_path = test_root("setup_actual_missing").join("config.toml");
        let actual_missing =
            run_setup_diagnostics_with_command_check(&actual_missing_path, |_| false);
        assert!(actual_missing.config_missing);
        assert!(actual_missing.config_path_error.is_none());

        let config_path = PathBuf::from("/virtual/config.toml");
        let missing = run_setup_diagnostics_with_checks(
            config_path.clone(),
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
            |_| PathInspection::Missing,
            |_| false,
        );
        assert!(missing.config_missing);

        let ambiguous = run_setup_diagnostics_with_checks(
            config_path,
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
            |_| PathInspection::PermissionDenied("permission denied".to_string()),
            |_| false,
        );
        assert!(!ambiguous.config_missing);
        assert!(ambiguous.checks.iter().any(|check| {
            check.name == "Config file exists"
                && check.status == SetupDiagnosticStatus::Error
                && check.detail.contains("permission denied")
        }));
    }

    #[test]
    fn unresolved_default_config_path_is_not_reported_as_missing() {
        let report = run_setup_diagnostics_default_with_path(Err(ArchiveFsError::Config(
            "HOME and USERPROFILE are unavailable".to_string(),
        )));

        assert!(!report.config_missing);
        assert!(report.config_path_error.is_some());
        assert!(!report.ready_for_scanning);
        assert!(report.config_path.is_none());
        assert!(report.config_identity.config_path.is_none());
        assert!(report.config_identity.content_digest.is_none());
        assert!(report.checks.iter().any(|check| {
            check.name == "Config path"
                && check.status == SetupDiagnosticStatus::Error
                && check
                    .detail
                    .contains("could not determine the user configuration directory")
        }));
    }

    #[test]
    fn config_identity_changes_when_config_bytes_change() {
        let root = test_root("config_identity_changes");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            "source_folders = [\"/a\"]\nmount_root = \"/mnt/a\"\n",
        )
        .unwrap();
        let first = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        fs::write(
            &config_path,
            "source_folders = [\"/b\"]\nmount_root = \"/mnt/b\"\n",
        )
        .unwrap();
        let second = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert_eq!(
            first.config_identity.config_path,
            second.config_identity.config_path
        );
        assert_ne!(first.config_identity, second.config_identity);
    }

    #[test]
    fn snapshot_and_diagnostics_share_identity_for_the_same_config_read() {
        let root = test_root("shared_identity");
        let source = root.join("roms");
        let mount_root = root.join("mounts");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                source.display(),
                mount_root.display()
            ),
        )
        .unwrap();

        let snapshot = load_read_only_snapshot(&config_path).unwrap();
        let diagnostics = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert_eq!(snapshot.config_identity, diagnostics.config_identity);

        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n# changed\n",
                source.display(),
                mount_root.display()
            ),
        )
        .unwrap();
        let changed_diagnostics = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert_ne!(
            snapshot.config_identity,
            changed_diagnostics.config_identity
        );
    }

    #[test]
    fn mount_root_creation_offer_requires_confirmed_absence_and_valid_parent() {
        let config_path = PathBuf::from("/virtual/config.toml");
        let source = PathBuf::from("/virtual/source");
        let mount_root = PathBuf::from("/virtual/mounts");
        let contents = format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            source.display(),
            mount_root.display()
        );
        let missing_mount = run_setup_diagnostics_with_checks(
            config_path.clone(),
            |_| Ok(contents.clone()),
            |path| {
                if path == mount_root {
                    PathInspection::Missing
                } else {
                    PathInspection::Directory
                }
            },
            |_| true,
        );
        assert!(missing_mount.can_create_mount_root);

        let ambiguous_mount = run_setup_diagnostics_with_checks(
            config_path,
            |_| Ok(contents.clone()),
            |path| {
                if path == mount_root {
                    PathInspection::MetadataError("input/output error".to_string())
                } else {
                    PathInspection::Directory
                }
            },
            |_| true,
        );
        assert!(!ambiguous_mount.can_create_mount_root);
        assert!(ambiguous_mount.checks.iter().any(|check| {
            check.name == "Mount root exists or can be created safely"
                && check.detail.contains("input/output error")
        }));
    }

    #[test]
    fn setup_diagnostics_are_derived_from_one_config_read() {
        use std::cell::Cell;

        let root = test_root("setup_single_read");
        let config_path = root.join("config.toml");
        let source = root.join("source");
        let mount_root = root.join("mounts");
        let contents = format!(
            "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
            source.display(),
            mount_root.display()
        );
        fs::write(&config_path, &contents).unwrap();
        let reads = Cell::new(0);
        let report = run_setup_diagnostics_with_checks(
            config_path.clone(),
            |path| {
                reads.set(reads.get() + 1);
                let snapshot = fs::read_to_string(path)?;
                fs::write(path, "not valid config").unwrap();
                Ok(snapshot)
            },
            |_| PathInspection::Directory,
            |_| true,
        );

        assert_eq!(reads.get(), 1);
        assert_eq!(fs::read_to_string(config_path).unwrap(), "not valid config");
        assert!(report.ready_for_scanning);
        assert_eq!(report.mount_root.as_deref(), Some(mount_root.as_path()));
        assert!(report.checks.iter().any(|check| {
            check.name == "Configured source folder exists"
                && check.status == SetupDiagnosticStatus::Ready
        }));
    }

    #[test]
    fn ready_setup_diagnostics_distinguish_scanning_and_actions() {
        let root = test_root("setup_ready");
        let source = root.join("roms");
        let mount_root = root.join("mounts");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                source.display(),
                mount_root.display()
            ),
        )
        .unwrap();

        let ready = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert!(ready.ready_for_scanning);
        assert!(ready.ready_for_actions);
        let tools_missing = run_setup_diagnostics_with_command_check(&config_path, |_| false);
        assert!(tools_missing.ready_for_scanning);
        assert!(!tools_missing.ready_for_actions);
    }

    #[test]
    fn watcher_ignores_obvious_temporary_and_incomplete_paths() {
        for path in [
            "Game.zip.part",
            "Game.zip.partial",
            "Game.zip.crdownload",
            "Game.zip.download",
            "Game.zip.tmp",
            "Game.zip.temp",
            "Game.zip.!qB",
            "Game.zip.aria2",
            "Game.zip~",
        ] {
            assert!(is_temporary_or_incomplete_path(path), "{path}");
        }

        assert!(!is_temporary_or_incomplete_path("Game.zip"));
        assert!(!is_temporary_or_incomplete_path("Game.7z"));
        assert!(!is_temporary_or_incomplete_path("Game.rar"));
    }

    #[test]
    fn watcher_event_filter_accepts_only_archive_related_mutations() {
        use notify::event::{AccessKind, AccessMode, CreateKind, EventKind, ModifyKind};

        let archive_create = notify::Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/roms/Game.zip"));
        assert!(watch_event_should_rebuild(&archive_create));

        let archive_iso_modify = notify::Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(PathBuf::from("/roms/Game.iso"));
        assert!(watch_event_should_rebuild(&archive_iso_modify));

        let close_write =
            notify::Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
                .add_path(PathBuf::from("/roms/Game.7z"));
        assert!(watch_event_should_rebuild(&close_write));

        let unrelated_file = notify::Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(PathBuf::from("/roms/Game.nfo"));
        assert!(!watch_event_should_rebuild(&unrelated_file));

        let temp_archive = notify::Event::new(EventKind::Create(CreateKind::File))
            .add_path(PathBuf::from("/roms/Game.zip.part"));
        assert!(!watch_event_should_rebuild(&temp_archive));

        let directory_only = notify::Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(PathBuf::from("/roms/New Folder"));
        assert!(!watch_event_should_rebuild(&directory_only));

        let read_access = notify::Event::new(EventKind::Access(AccessKind::Read))
            .add_path(PathBuf::from("/roms/Game.zip"));
        assert!(!watch_event_should_rebuild(&read_access));

        let archive_named_folder = notify::Event::new(EventKind::Create(CreateKind::Folder))
            .add_path(PathBuf::from("/roms/Folder.zip"));
        assert!(!watch_event_should_rebuild(&archive_named_folder));
    }

    #[test]
    fn watcher_debouncer_fires_after_quiet_period() {
        let debounce = Duration::from_secs(5);
        let start = Instant::now();
        let mut debouncer = WatchDebouncer::new(debounce);

        assert!(!debouncer.should_fire(start + debounce));

        debouncer.record_change(start, vec![PathBuf::from("/roms/Game.zip")]);
        assert!(!debouncer.should_fire(start + Duration::from_secs(4)));
        assert!(debouncer.should_fire(start + debounce));

        debouncer.record_change(
            start + Duration::from_secs(3),
            vec![PathBuf::from("/roms/Game.7z")],
        );
        assert!(!debouncer.should_fire(start + Duration::from_secs(7)));
        assert!(debouncer.should_fire(start + Duration::from_secs(8)));

        debouncer.mark_fired();
        assert!(!debouncer.should_fire(start + Duration::from_secs(20)));
    }

    #[test]
    fn watcher_debouncer_tracks_event_count_and_first_five_changed_paths() {
        let start = Instant::now();
        let mut debouncer = WatchDebouncer::new(Duration::from_secs(5));

        debouncer.record_change(
            start,
            vec![
                PathBuf::from("/roms/one.zip"),
                PathBuf::from("/roms/two.7z"),
            ],
        );
        debouncer.record_change(
            start + Duration::from_secs(1),
            vec![
                PathBuf::from("/roms/two.7z"),
                PathBuf::from("/roms/three.rar"),
                PathBuf::from("/roms/four.iso"),
                PathBuf::from("/roms/five.zip"),
                PathBuf::from("/roms/six.zip"),
            ],
        );

        let summary = debouncer.take_summary();

        assert_eq!(summary.archive_event_count, 2);
        assert_eq!(
            summary.changed_paths,
            vec![
                PathBuf::from("/roms/one.zip"),
                PathBuf::from("/roms/two.7z"),
                PathBuf::from("/roms/three.rar"),
                PathBuf::from("/roms/four.iso"),
                PathBuf::from("/roms/five.zip"),
            ]
        );
        assert!(!debouncer.should_fire(start + Duration::from_secs(20)));
    }

    fn test_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("archivefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[derive(Default)]
    struct RecordingBackend {
        mounted: std::cell::RefCell<Vec<PathBuf>>,
        unmounted: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl RecordingBackend {
        fn mounted(&self) -> Vec<PathBuf> {
            self.mounted.borrow().clone()
        }

        fn unmounted(&self) -> Vec<PathBuf> {
            self.unmounted.borrow().clone()
        }
    }

    impl MountBackend for RecordingBackend {
        fn mount(&self, plan: &MountPlan) -> Result<()> {
            self.mounted.borrow_mut().push(plan.archive.path.clone());
            Ok(())
        }

        fn unmount(&self, mount_path: &Path) -> Result<()> {
            self.unmounted.borrow_mut().push(mount_path.to_path_buf());
            Ok(())
        }
    }

    struct RecordingLazyUnmountBackend {
        mounted_snapshots: std::cell::RefCell<std::collections::VecDeque<HashSet<PathBuf>>>,
        unmounted: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl RecordingLazyUnmountBackend {
        fn new(mounted_snapshots: Vec<HashSet<PathBuf>>) -> Self {
            Self {
                mounted_snapshots: std::cell::RefCell::new(mounted_snapshots.into()),
                unmounted: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn unmounted(&self) -> Vec<PathBuf> {
            self.unmounted.borrow().clone()
        }
    }

    impl LazyUnmountBackend for RecordingLazyUnmountBackend {
        fn mounted_paths_under(&self, _root: &Path) -> Result<HashSet<PathBuf>> {
            Ok(self
                .mounted_snapshots
                .borrow_mut()
                .pop_front()
                .unwrap_or_default())
        }

        fn lazy_unmount(&self, mount_path: &Path) -> Result<LazyUnmountTool> {
            self.unmounted.borrow_mut().push(mount_path.to_path_buf());
            Ok(LazyUnmountTool::Fusermount3)
        }
    }

    fn sample_index_for_find() -> ArchiveIndex {
        ArchiveIndex {
            archives: vec![
                ArchiveIndexEntry {
                    archive_path: PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    platform: Some("Xbox360".to_string()),
                    display_name: "007 Legends".to_string(),
                    mount_path: PathBuf::from("/mnt/archivefs/Xbox360/007_Legends"),
                    modified_time_seconds: None,
                    health: ArchiveHealth::Pending,
                    mount_state: MountState::Mounted,
                },
                ArchiveIndexEntry {
                    archive_path: PathBuf::from("/roms/misc/Mystery.zip"),
                    platform: None,
                    display_name: "Mystery".to_string(),
                    mount_path: PathBuf::from("/mnt/archivefs/Unknown/Mystery"),
                    modified_time_seconds: None,
                    health: ArchiveHealth::Pending,
                    mount_state: MountState::Pending,
                },
            ],
        }
    }

    fn archive_record_with_size(
        path: &str,
        platform: Option<&str>,
        mount_state: MountState,
        size_bytes: u64,
    ) -> ArchiveRecord {
        let mut archive = archive_with_platform(path, platform);
        archive.identity.size_bytes = Some(size_bytes);
        let mut metadata = ArchiveMetadata::empty();
        metadata.platform = platform.map(str::to_string);
        ArchiveRecord::new(
            MountPlan::new(archive, PathBuf::from("/mnt/archivefs/Test")),
            mount_state,
            metadata,
            ArchiveHealth::Pending,
        )
    }

    fn archive_with_platform(path: &str, platform: Option<&str>) -> Archive {
        let mut archive = archive(path);
        archive.identity.platform = platform.map(str::to_string);
        archive
    }

    fn archive(path: &str) -> Archive {
        let path = PathBuf::from(path);
        Archive {
            kind: archive_kind(&path).unwrap(),
            identity: ArchiveIdentity::from_path(&path, PathBuf::new(), None),
            path,
            health: ArchiveHealth::Pending,
        }
    }
}
