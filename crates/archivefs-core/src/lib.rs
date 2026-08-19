use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, info};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

/// App-directory resolution with legacy ArchiveFS compatibility. See the
/// module for the reuse-not-migrate strategy that keeps existing user data
/// reachable during the EmuWiz rename.
pub mod app_dirs;
mod database;
/// Read-only Doctor diagnostics: one shared finding model plus adapters
/// over the existing per-subsystem reports. See the module documentation
/// for the read-only contract.
pub mod diagnostics;
use database::scan_and_persist_folders;
pub use database::{
    ArchiveChangeKind, ArchiveObservationKind, ArchiveUpsertOutcome, AutomaticPlatformDetails,
    BulkPlatformAssignmentResult, CUSTOM_FOLDER_ALIAS_SOURCE, CatalogueStats, CompletedScanSummary,
    DAT_ROMM_AGREEMENT_SOURCE, Database, DatabaseCheckOutcome, DatabaseCheckStatus,
    DatabaseDiagnostic, DatabaseDiagnosticCode, DatabaseDiagnosticSeverity, DatabaseFileFinding,
    DatabaseHealth, DatabaseHealthReport, DatabaseOpenOutcome, DatabaseSidecarFinding,
    DatabaseSidecarKind, MANUAL_PLATFORM_SOURCE, MissingArchiveRemovalResult, PersistedArchive,
    PlatformAlias, PlatformAssignmentChange, PlatformIdentityApplyOutcome,
    PlatformIdentityConflictDetail, PlatformIdentityEnrichmentSummary, PlatformProvenanceDetails,
    ROMM_PLATFORM_SOURCE, RecentScanAdditions, RegisteredSourceFolder,
    SOURCE_PLATFORM_ASSIGNMENT_SOURCE, ScanPersistSummary, ScanRunCounts, SourceFolderRecord,
    SourceScanStatus, VERIFIED_DAT_PLATFORM_SOURCE, check_database_health, default_database_path,
    diagnose_database, format_unix_timestamp_utc, latest_schema_version,
    persisted_archive_has_unknown_platform, scan_and_persist,
};

mod inspector;
pub use inspector::{
    INSPECTOR_ENTRY_LIMIT, InspectorEntry, InspectorEntryClassification, InspectorEntryKind,
    InspectorError, InspectorReport, classify_entry, inspect_archive, inspect_archive_with_limit,
    is_inspectable, is_known_disc_companion,
};

pub mod game_identity;

mod library_views;
pub use library_views::{
    FrontendPlatformMapping, FrontendProfile, FrontendProfileKind, FrontendProfilePolicy,
    LibraryViewApplyEntryResult, LibraryViewApplyOutcome, LibraryViewApplyReport,
    LibraryViewConfig, LibraryViewGeneratedFilePlan, LibraryViewLayoutTemplate,
    LibraryViewManifest, LibraryViewManifestEntry, LibraryViewObjectClassification,
    LibraryViewObjectKind, LibraryViewPlan, LibraryViewPlanAction, LibraryViewPlanCounts,
    LibraryViewPlanEntry, MultidiscHandlingPolicy, SymlinkTargetStrategy, TitleSelectionPolicy,
    VariantHandlingPolicy, add_library_view_default, apply_library_view,
    apply_library_view_default, classify_library_view_object, compute_view_profile_fingerprint,
    default_library_views_config_path, default_library_views_data_dir, derive_view_filename,
    edit_library_view_default, generate_library_view_id, generate_relative_link_path,
    library_view_manifest_path, load_library_view_configs_default, load_library_view_configs_from,
    load_library_view_manifest_at, load_library_view_manifest_default, plan_generated_file,
    plan_library_view, preview_library_view_default, remove_library_view_default,
    remove_library_view_symlinks, repair_library_view, repair_library_view_default,
    resolve_library_view_identifier, resolve_romm_platform_slug, save_library_view_configs_default,
    save_library_view_configs_to, set_library_view_enabled_default,
    validate_library_view_destination,
};

/// The single authoritative platform registry and the evidence-based
/// detector built on it. Every platform alias, extension and signature this
/// build knows lives there - nothing keeps a second copy.
/// The one bounded, read-only file-open policy in this build, shared by
/// platform signature detection and disc identity so the two cannot drift
/// apart on symlink handling.
/// The one read-only disk-image format evidence layer, shared by platform
/// detection and disc identity so a structural format is recognised the same way
/// wherever it is asked about.
/// Read-only external identity providers - RomM in Stage 1, with room for local
/// DAT catalogues and Hasheous later.
pub mod identity_source;

/// Provider-neutral DAT catalogue parsing and read-only audit.
///
/// Parses Logiqx XML and ClrMamePro text DAT files (No-Intro, Redump, TOSEC)
/// into a common model, indexes their entries by hash for fast lookup, and
/// provides a read-only audit that compares known hashes against the catalogue.
/// Stage 1A: core model, parsers, indexes, and audit - no persistence or GUI.
pub mod dat;

pub mod disk_format;

/// Pure content/container/media evidence, deliberately separate from
/// canonical platform identity. See the module documentation for why.
pub mod content_evidence;

/// The generic, dependency-free contract future content/media detectors
/// implement. No detector implementation lives here.
pub mod content_detector;

/// Pure, read-only Nintendo 64 byte-order (z64/v64/n64) detection and
/// normalization prototype. The first real [`content_detector`] example.
pub mod n64_byte_order;

/// Pure, read-only recognition and removal of known, reversible fixed-length
/// copier/container headers (iNES, FDS, Lynx, Atari 7800, SNES copier).
pub mod header_normalization;

/// Pure, read-only Super Magic Drive (SMD) de-interleave normalization for
/// Sega Mega Drive / Genesis ROM images.
pub mod smd_normalization;

/// Pure, read-only CHD identity observation - separates a CHD's physical
/// file identity from the raw/combined logical identity its own v5 header
/// already records. Reuses [`dat::archive::chd`] rather than duplicating it.
pub mod chd_identity;

/// The smallest possible read-only "bytes addressable by offset"
/// abstraction, so a logical-filesystem reader (see [`iso9660`]) is not
/// permanently coupled to any one container.
pub mod logical_media;

/// Pure, read-only ISO9660 logical-filesystem observation: root layout,
/// path lookup, and well-known boot-path existence facts. A filesystem is
/// evidence, never a platform - see the module documentation.
pub mod iso9660;

/// Pure, read-only CHD logical-data reading: adapts the `chd` crate
/// (chd-rs) to [`logical_media::LogicalMedia`], connecting a CHD's selected
/// CD/GD-ROM data track to [`iso9660`].
pub mod chd_logical_media;

/// Optional specialist optical-disc backend (Dreamcast GD-ROM high-density
/// data, and other complex optical layouts [`chd_logical_media`]
/// deliberately does not handle), behind the `chd-optical-specialist`
/// feature. See the module documentation for the fallback policy and the
/// native-dependency tradeoffs of enabling it.
#[cfg(feature = "chd-optical-specialist")]
pub mod chd_optical_specialist;

pub mod repair;

pub mod safe_read;

pub mod media_registry;
pub mod platform;

/// Explicit, offline management of user-owned platform artwork overrides.
pub mod platform_artwork;

pub mod patch_manager;

pub mod emulator_environment;

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
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
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

/// A single configured archive source folder, as the multi-source
/// milestone persists it - richer than the plain `PathBuf` entries in
/// `Config::source_folders`. `Config::source_folders` is deliberately kept
/// as "every *enabled* source's path" (see `parse_config`), so every
/// existing consumer of `Config` (the scanner, doctor checks, diagnostics,
/// mount-root creation, and dozens of existing tests) is automatically and
/// correctly disabled-source-aware without being touched at all - only the
/// new Sources-page/CLI source-management code needs this richer type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFolderConfig {
    pub path: PathBuf,
    pub enabled: bool,
    /// RFC 3339 timestamp string, if known. `None` for sources migrated
    /// from a legacy `source_folders = [...]` config that never recorded
    /// one - never fabricated, per the milestone's "created timestamp if
    /// consistent with existing configuration style" (i.e. best-effort).
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub source_folders: Vec<PathBuf>,
    pub mount_root: PathBuf,
    pub ratarmount_bin: String,
    /// The optional canonical ROM root: games organised into canonical
    /// platform directories beneath it. `None` when the user has not
    /// configured one; configuring it never moves anything by itself.
    pub master_rom_root: Option<PathBuf>,
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
    /// The check did not run, so its answer is unknown. Never a pass and
    /// never a problem - a gap. Used by
    /// [`run_setup_diagnostics_read_only`] for mount-root writability,
    /// which can only be established by writing.
    NotChecked,
    /// This check cannot pass yet only because no configuration file
    /// exists at all - a brand-new install, not a broken one. Distinct
    /// from [`Self::Error`], which means something that *should* work is
    /// actually broken (malformed config, a path that should exist but
    /// does not, a missing tool). A config that is merely absent is never
    /// downgraded to this status until the read has confirmed it is
    /// genuinely missing (see `config_missing` in
    /// `run_setup_diagnostics_with_checks`) rather than merely unreadable,
    /// so a real permission problem still reports [`Self::Error`].
    NotConfigured,
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
        // The exact wording here matters beyond this report: Doctor's
        // Finding adapter (`diagnostics::mod::doctor_check_severity`)
        // recognises literal "missing {path}" text on this check as a
        // confirmed-absent config and reports it as Info rather than
        // Error. Reusing `inspect_path`/`is_confirmed_missing` - the same
        // primitives `run_setup_diagnostics_with_checks` uses for its own
        // `config_missing` - keeps that wording reserved for genuine
        // absence, so a broken symlink or other ambiguous path state
        // (which `.exists()` alone cannot tell apart from "not there at
        // all") is never softened to Info here while `SetupDiagnostics`
        // correctly still reports it as Error.
        let state = inspect_path(&config_path);
        let detail = if state.is_confirmed_missing() {
            format!("missing {}", config_path.display())
        } else {
            format!(
                "{} cannot be inspected safely: {}",
                config_path.display(),
                state
                    .error_detail()
                    .unwrap_or("the path is not a readable file")
            )
        };
        report.fail("config file", detail);
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

    // `mount_root_exists_as_directory` tracks whether the checks below
    // should also probe writability - deliberately a *second*, separate
    // check (mirroring `run_setup_diagnostics_with_checks`'s
    // `mount_root_ready`/`mount_root_writable` split) rather than folding
    // writability into the "mount root" pass/fail above. This closes a
    // real gap where a mount root that exists but is not writable by the
    // current user (owned by another user, wrong permissions - the exact
    // live-Nobara symptom this was added for) used to report "mount root:
    // Pass" here even though `SetupDiagnostics.ready_for_actions` (the
    // actual Mount/Unmount gate) already correctly failed on it, making
    // the Library page's "Doctor: Ready" summary silently contradict the
    // real, stricter gate with no visible explanation.
    let mount_root_exists_as_directory = match inspect_path(&config.mount_root) {
        PathInspection::Directory => {
            report.pass(
                "mount root",
                format!("{} exists", config.mount_root.display()),
            );
            true
        }
        PathInspection::Other => {
            report.fail(
                "mount root",
                format!(
                    "{} exists but is not a directory",
                    config.mount_root.display()
                ),
            );
            false
        }
        PathInspection::Missing if create_mount_root => {
            match fs::create_dir_all(&config.mount_root) {
                Ok(()) => {
                    report.pass(
                        "mount root",
                        format!("{} was created", config.mount_root.display()),
                    );
                    true
                }
                Err(error) => {
                    report.fail(
                        "mount root",
                        format!("{} cannot be created: {error}", config.mount_root.display()),
                    );
                    false
                }
            }
        }
        PathInspection::Missing => {
            report.fail(
                "mount root",
                format!("{} does not exist", config.mount_root.display()),
            );
            false
        }
        PathInspection::PermissionDenied(error) | PathInspection::MetadataError(error) => {
            report.fail(
                "mount root",
                format!(
                    "{} cannot be inspected: {error}",
                    config.mount_root.display()
                ),
            );
            false
        }
    };
    if mount_root_exists_as_directory {
        if directory_is_writable(&config.mount_root) {
            report.pass(
                "mount root writable",
                format!("{} is writable", config.mount_root.display()),
            );
        } else {
            report.fail(
                "mount root writable",
                format!(
                    "{} exists but is not writable by the current user",
                    config.mount_root.display()
                ),
            );
        }
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
                    "EmuWiz could not determine the user configuration directory: {error}"
                ),
                why_it_matters: "EmuWiz needs a known configuration location.".to_string(),
                next_step: "Set HOME and refresh diagnostics.".to_string(),
            }],
        },
    }
}

/// Setup diagnostics that never write.
///
/// Identical to [`run_setup_diagnostics`] except that mount-root
/// writability is reported as [`SetupDiagnosticStatus::NotChecked`] instead
/// of being probed. The probe used by the normal path
/// ([`directory_is_writable`]) creates and removes a file inside the mount
/// root, which changes that directory's modification time - unacceptable
/// for a strictly read-only diagnostic.
///
/// `ready_for_actions` is therefore always `false` here: EmuWiz cannot
/// honestly assert that mount and unmount actions are usable without having
/// established writability.
pub fn run_setup_diagnostics_read_only(config_path: impl AsRef<Path>) -> SetupDiagnostics {
    run_setup_diagnostics_with_command_check_and_probe(config_path, command_available, false)
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
    run_setup_diagnostics_with_command_check_and_probe(config_path, command_check, true)
}

fn run_setup_diagnostics_with_command_check_and_probe(
    config_path: impl AsRef<Path>,
    command_check: impl Fn(&str) -> bool,
    probe_writability: bool,
) -> SetupDiagnostics {
    let config_path = config_path.as_ref().to_path_buf();
    run_setup_diagnostics_with_checks(
        config_path,
        |path| fs::read_to_string(path),
        inspect_path,
        command_check,
        probe_writability,
    )
}

fn run_setup_diagnostics_with_checks(
    config_path: PathBuf,
    read_config: impl Fn(&Path) -> io::Result<String>,
    inspect: impl Fn(&Path) -> PathInspection,
    command_check: impl Fn(&str) -> bool,
    probe_writability: bool,
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
    // Must agree with the Sources page, Scan All, and CLI source
    // management - all three ultimately read `parse_source_folder_configs`
    // (via `load_source_folder_configs_from`), never `ConfigFields.
    // source_folders` directly. Reading that raw legacy field here used to
    // mean a config using only the newer `[[source]]` block format
    // reported zero source folders to diagnostics while the rest of the
    // app correctly saw every structured, enabled source - "EmuWiz is
    // ready for scanning"/"...for mount/unmount actions" could then be
    // permanently false even with a fully valid, populated config. Enabled
    // filtering here mirrors `parse_config`'s own `Config.source_folders`
    // semantics, so a disabled-only config is correctly still "no usable
    // source folder is configured".
    let source_folders = contents.as_deref().and_then(|contents| {
        parse_source_folder_configs(contents).ok().map(|sources| {
            sources
                .into_iter()
                .filter(|source| source.enabled)
                .map(|source| source.path)
                .collect::<Vec<_>>()
        })
    });
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
    // `None` when writability was deliberately not established. Never
    // `Some(true)` unless a real probe ran and succeeded.
    let mount_root_writable: Option<bool> = if probe_writability {
        Some(
            mount_root_ready
                && mount_root
                    .as_ref()
                    .is_some_and(|root| directory_is_writable(root)),
        )
    } else {
        None
    };
    let ratarmount_name = fields
        .as_ref()
        .and_then(|fields| fields.ratarmount_bin.as_deref())
        .unwrap_or("ratarmount");
    let ratarmount_ready = command_check(ratarmount_name);
    let unmount_ready = command_check("fusermount3") || command_check("umount");
    let ready_for_scanning = config_valid && sources_ready;
    // An unprobed mount root can never make actions "ready": EmuWiz has
    // not established that it can create mount points there.
    let ready_for_actions = ready_for_scanning
        && mount_root_ready
        && mount_root_writable == Some(true)
        && ratarmount_ready
        && unmount_ready;
    let mut checks = Vec::new();
    setup_check(
        &mut checks,
        "Config file exists",
        config_read_ok,
        config_read_detail,
        "EmuWiz needs this file to locate archives and mounts.",
        "Create a starter config or create this file manually.",
    );
    setup_check(
        &mut checks,
        "Config parses successfully",
        fields.is_some(),
        parse_detail,
        "Invalid TOML prevents EmuWiz from reading any settings.",
        "Open the config and correct the reported fields or syntax.",
    );
    setup_check(
        &mut checks,
        "At least one source folder is configured",
        source_folders
            .as_ref()
            .is_some_and(|sources| !sources.is_empty()),
        source_folders.as_ref().map_or_else(
            || "No usable source folder is configured.".to_string(),
            |sources| format!("{} source folder(s) configured.", sources.len()),
        ),
        "Source folders contain the archives EmuWiz scans.",
        "Add at least one existing, enabled source folder (Sources page or source_folders).",
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
            "Create the directory or update this source's path (Sources page or source_folders).",
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
        "EmuWiz places read-only archive mounts below this directory.",
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
    match mount_root_writable {
        Some(writable) => setup_check_with_warning(
            &mut checks,
            "Mount root is writable",
            writable,
            can_create_mount_root,
            mount_root.as_ref().map_or_else(
                || "No mount root is available to test.".to_string(),
                |root| format!("Writable directory required: {}", root.display()),
            ),
            "EmuWiz must create mount-point directories below mount_root.",
            "Grant the current user write access or choose another mount_root.",
        ),
        // Deliberately not probed - see `run_setup_diagnostics_read_only`.
        // Reported as unknown rather than as a pass or a failure.
        None => checks.push(SetupDiagnostic {
            name: "Mount root is writable".to_string(),
            status: SetupDiagnosticStatus::NotChecked,
            detail: mount_root.as_ref().map_or_else(
                || "Not probed: no mount root is configured.".to_string(),
                |root| {
                    format!(
                        "Not probed: establishing write access means creating and removing a file in {}, which a read-only check never does.",
                        root.display()
                    )
                },
            ),
            why_it_matters:
                "EmuWiz must create mount-point directories below mount_root before it can mount anything."
                    .to_string(),
            next_step:
                "Run `archivefs config-check`, or use Settings -> Validate configuration, to test write access."
                    .to_string(),
        }),
    }
    setup_check(
        &mut checks,
        "ratarmount is available",
        ratarmount_ready,
        if ratarmount_ready {
            format!("{ratarmount_name} is available.")
        } else {
            format!("{ratarmount_name} was not found.")
        },
        "EmuWiz uses ratarmount to expose archive contents as read-only folders.",
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
        "Without either tool, EmuWiz cannot detach mounted archives.",
        "Install fusermount3 or provide umount on PATH, then refresh diagnostics.",
    );
    setup_check(
        &mut checks,
        "EmuWiz is ready for scanning",
        ready_for_scanning,
        "Scanning requires a valid config and all configured source folders.".to_string(),
        "Archive scanning populates the library shown in the GUI.",
        "Resolve config and source-folder errors above.",
    );
    setup_check(
        &mut checks,
        "EmuWiz is ready for mount/unmount actions",
        ready_for_actions,
        "Actions additionally require a writable mount root and system tools.".to_string(),
        "Mount and unmount controls are unsafe or unusable until these checks pass.",
        "Resolve mount-root and tool errors above.",
    );
    // A genuinely missing config file (first run - never one that exists
    // but is malformed, unreadable, or references paths that do not exist)
    // makes every downstream check fail for the same single reason: there
    // is nothing configured yet. Presenting each of those as its own red
    // "Error" would greet a brand-new install with a wall of failures for
    // a state that is not a failure at all. System-tool checks are left
    // alone: whether ratarmount or an unmount tool is installed is a fact
    // about the machine, independent of whether a config file exists.
    if config_missing {
        for check in &mut checks {
            if check.status == SetupDiagnosticStatus::Error
                && check.name != "ratarmount is available"
                && check.name != "fusermount3 or umount is available"
            {
                check.status = SetupDiagnosticStatus::NotConfigured;
            }
        }
    }
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
        b"# EmuWiz starter configuration\n\
          # No source folders are configured yet - that is fine, a fresh\n\
          # install with zero sources loads normally. Add your first one\n\
          # from the Sources page in the GUI, or from the command line:\n\
          #   archivefs-cli source add /path/to/archives\n\
          # More can be added the same way at any time; nothing here needs\n\
          # to be edited by hand unless you prefer to.\n\
          source_folders = []\n\
          mount_root = \"/path/to/archivefs-mounts\"\n\
          ratarmount_bin = \"ratarmount\"\n",
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

    let mut fields = match parse_config_fields(&contents) {
        Ok(fields) => {
            report.pass("config parses", "configuration syntax parsed successfully");
            fields
        }
        Err(error) => {
            report.error("config parses", error.to_string());
            return report;
        }
    };
    // The checks below were written against the legacy flat
    // `source_folders` list; a config using the newer `[[source]]` block
    // format never populates that field directly, so synthesize an
    // equivalent view (every configured path, enabled or not - this is a
    // truthfulness diagnostic about what's on disk, not a scan) rather
    // than duplicating the whole per-folder existence/readability loop.
    if fields.source_folders.is_none() && !fields.structured_sources.is_empty() {
        fields.source_folders = Some(
            fields
                .structured_sources
                .iter()
                .map(|source| source.path.display().to_string())
                .collect(),
        );
    }

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
    app_dirs::config_path("config.toml")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ConfigFields {
    source_folders: Option<Vec<String>>,
    mount_root: Option<PathBuf>,
    ratarmount_bin: Option<String>,
    master_rom_root: Option<PathBuf>,
    /// Populated only when the config uses the newer `[[source]]` block
    /// format (see `parse_config_fields`) - empty for every legacy
    /// `source_folders = [...]`/`sources = [...]` config, which is the
    /// signal `parse_config` and `run_config_check_with_mount_root_creation`
    /// both use to decide which representation is authoritative.
    structured_sources: Vec<SourceFolderConfig>,
}

/// Accumulates one `[[source]]` block's `path`/`enabled`/`created_at`
/// fields across however many lines it spans, in any order, before being
/// finalized into a `SourceFolderConfig` once the block ends (the next
/// `[[source]]`, any other `[section]` header, or end of file).
#[derive(Debug, Clone, Default)]
struct PendingSourceEntry {
    path: Option<PathBuf>,
    enabled: Option<bool>,
    created_at: Option<String>,
}

impl PendingSourceEntry {
    fn finish(self, block_line: usize) -> Result<SourceFolderConfig> {
        let path = self.path.ok_or_else(|| {
            ArchiveFsError::Config(format!(
                "the [[source]] block starting at line {block_line} has no path"
            ))
        })?;
        Ok(SourceFolderConfig {
            path,
            enabled: self.enabled.unwrap_or(true),
            created_at: self.created_at,
        })
    }
}

/// Parses `config.toml` into a `Config` whose `source_folders` is always
/// "every currently *enabled* source's path" - disabled sources are
/// filtered out here, at the single narrowest point, so every existing
/// consumer of `Config` (scanner, doctor checks, mount-root creation, ...)
/// is correctly disabled-source-aware without any of that code changing.
/// Reading the richer per-source list (including disabled entries and
/// their metadata) is `load_source_folder_configs_default`/`_from`'s job,
/// not this function's.
///
/// A config with zero enabled sources (via an empty/absent legacy
/// `source_folders`, or zero/all-disabled `[[source]]` blocks) is valid,
/// not an error - the multi-source milestone's first-run flow explicitly
/// allows skipping source setup and adding folders later from inside the
/// app, so `Config::load_default` must be able to succeed with nothing
/// configured yet.
pub fn parse_config(contents: &str) -> Result<Config> {
    let fields = parse_config_fields(contents)?;

    let source_folders = if !fields.structured_sources.is_empty() {
        fields
            .structured_sources
            .iter()
            .filter(|source| source.enabled)
            .map(|source| source.path.clone())
            .collect::<Vec<_>>()
    } else {
        fields
            .source_folders
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect()
    };

    Ok(Config {
        source_folders,
        mount_root: fields
            .mount_root
            .ok_or_else(|| ArchiveFsError::Config("missing mount_root".to_string()))?,
        ratarmount_bin: fields
            .ratarmount_bin
            .unwrap_or_else(|| "ratarmount".to_string()),
        master_rom_root: fields.master_rom_root,
    })
}

fn parse_config_fields(contents: &str) -> Result<ConfigFields> {
    let mut fields = ConfigFields::default();
    let mut pending_source: Option<PendingSourceEntry> = None;
    let mut pending_source_line: usize = 0;

    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line_number = i + 1;
        let line = strip_comment(lines[i]).trim();
        i += 1;
        if line.is_empty() {
            continue;
        }

        if line == "[[source]]" {
            if let Some(pending) = pending_source.take() {
                fields
                    .structured_sources
                    .push(pending.finish(pending_source_line)?);
            }
            pending_source = Some(PendingSourceEntry::default());
            pending_source_line = line_number;
            continue;
        }
        if line.starts_with('[') {
            if let Some(pending) = pending_source.take() {
                fields
                    .structured_sources
                    .push(pending.finish(pending_source_line)?);
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} is not a key/value pair",
            )));
        };
        let key = key.trim();

        if let Some(pending) = pending_source.as_mut() {
            match key {
                "path" => {
                    pending.path = Some(PathBuf::from(parse_string(value.trim(), line_number)?))
                }
                "enabled" => pending.enabled = Some(parse_bool(value.trim(), line_number)?),
                "created_at" => pending.created_at = Some(parse_string(value.trim(), line_number)?),
                _ => {}
            }
            continue;
        }

        match key {
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
            "master_rom_root" => {
                fields.master_rom_root =
                    Some(PathBuf::from(parse_string(value.trim(), line_number)?));
            }
            _ => {}
        }
    }

    if let Some(pending) = pending_source.take() {
        fields
            .structured_sources
            .push(pending.finish(pending_source_line)?);
    }

    Ok(fields)
}

fn parse_bool(value: &str, line_number: usize) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ArchiveFsError::Config(format!(
            "line {line_number} expected true or false, found '{other}'"
        ))),
    }
}

/// Reads every configured source folder, including disabled ones and
/// their `enabled`/`created_at` metadata - the multi-source Sources
/// page/CLI's data source. `Config::source_folders` deliberately can't
/// answer this: it's enabled-only by design (see `parse_config`'s doc
/// comment), so every pre-existing consumer of `Config` stays correct
/// without change while this is the one new entry point that needs the
/// full picture.
pub fn load_source_folder_configs_default() -> Result<Vec<SourceFolderConfig>> {
    load_source_folder_configs_from(default_config_path()?)
}

pub fn load_source_folder_configs_from(path: impl AsRef<Path>) -> Result<Vec<SourceFolderConfig>> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    parse_source_folder_configs(&contents)
}

/// Shares `parse_config_fields` with `parse_config` (never a second,
/// divergent parser): if the file uses the newer `[[source]]` block
/// format, those entries are authoritative and returned directly, complete
/// with disabled entries; otherwise every legacy `source_folders`/
/// `sources` path is treated as one enabled source with an unknown
/// (`None`) creation time - the automatic migration this milestone
/// requires, applied in memory on every read, with no file rewrite unless
/// the caller explicitly saves (see `save_source_folder_configs_default`).
pub fn parse_source_folder_configs(contents: &str) -> Result<Vec<SourceFolderConfig>> {
    let fields = parse_config_fields(contents)?;
    if !fields.structured_sources.is_empty() {
        return Ok(fields.structured_sources);
    }
    Ok(fields
        .source_folders
        .unwrap_or_default()
        .into_iter()
        .map(|path| SourceFolderConfig {
            path: PathBuf::from(path),
            enabled: true,
            created_at: None,
        })
        .collect())
}

/// Atomically rewrites the config file with `sources`, always in the
/// newer `[[source]]` block format regardless of which format the file
/// was previously in - the first source-management mutation (add/enable/
/// disable/remove) is what actually upgrades a legacy config on disk;
/// merely loading it never does (see `parse_source_folder_configs`'s doc
/// comment). `mount_root`/`ratarmount_bin` are preserved exactly as
/// given, never invented or defaulted.
pub fn save_source_folder_configs_default(
    sources: &[SourceFolderConfig],
    mount_root: &Path,
    ratarmount_bin: &str,
) -> Result<()> {
    let master_rom_root = Config::load_default()
        .ok()
        .and_then(|config| config.master_rom_root);
    save_source_folder_configs_to(
        default_config_path()?,
        sources,
        mount_root,
        ratarmount_bin,
        master_rom_root.as_deref(),
    )
}

pub fn save_source_folder_configs_to(
    path: impl AsRef<Path>,
    sources: &[SourceFolderConfig],
    mount_root: &Path,
    ratarmount_bin: &str,
    master_rom_root: Option<&Path>,
) -> Result<()> {
    if let Some(source) = sources.iter().find(|source| source.path.to_str().is_none()) {
        return Err(ArchiveFsError::Config(format!(
            "source path cannot be stored losslessly in the UTF-8 configuration file: {}",
            source.path.display()
        )));
    }
    let contents =
        render_source_folder_configs(sources, mount_root, ratarmount_bin, master_rom_root);
    atomic_write_text(path.as_ref(), &contents)
}

/// Sets the canonical master ROM root that organised games are moved into.
///
/// This only configures the root - no action occurs merely because it is
/// set; the user must plan and explicitly approve an organisation first.
/// The path must be absolute and must not be a filesystem root. Returns the
/// previously configured root, if any.
pub fn set_master_rom_root_default(path: &Path) -> Result<Option<PathBuf>> {
    set_master_rom_root_to(&default_config_path()?, path)
}

/// Sets the master ROM root at an explicit config path (tests).
pub fn set_master_rom_root_to(config_path: &Path, path: &Path) -> Result<Option<PathBuf>> {
    validate_master_rom_root(path)?;
    let sources = load_source_folder_configs_from(config_path)?;
    let config = Config::load_from(config_path).ok();
    let mount_root = config
        .as_ref()
        .map(|config| config.mount_root.clone())
        .unwrap_or_default();
    let ratarmount_bin = config
        .as_ref()
        .map(|config| config.ratarmount_bin.clone())
        .unwrap_or_else(|| "ratarmount".to_string());
    let previous = config.and_then(|config| config.master_rom_root);
    save_source_folder_configs_to(
        config_path,
        &sources,
        &mount_root,
        &ratarmount_bin,
        Some(path),
    )?;
    Ok(previous)
}

/// Clears the configured master ROM root (returns the removed value, if any).
pub fn clear_master_rom_root_default() -> Result<Option<PathBuf>> {
    clear_master_rom_root_to(&default_config_path()?)
}

/// Clears the master ROM root at an explicit config path (tests).
pub fn clear_master_rom_root_to(config_path: &Path) -> Result<Option<PathBuf>> {
    let sources = load_source_folder_configs_from(config_path)?;
    let config = Config::load_from(config_path).ok();
    let mount_root = config
        .as_ref()
        .map(|config| config.mount_root.clone())
        .unwrap_or_default();
    let ratarmount_bin = config
        .as_ref()
        .map(|config| config.ratarmount_bin.clone())
        .unwrap_or_else(|| "ratarmount".to_string());
    let previous = config.and_then(|config| config.master_rom_root);
    save_source_folder_configs_to(config_path, &sources, &mount_root, &ratarmount_bin, None)?;
    Ok(previous)
}

fn validate_master_rom_root(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(ArchiveFsError::Config(
            "the master ROM root must be an absolute path".to_string(),
        ));
    }
    if path.parent().is_none() {
        return Err(ArchiveFsError::Config(
            "the master ROM root cannot be a filesystem root".to_string(),
        ));
    }
    if path.iter().any(|component| component == "..") {
        return Err(ArchiveFsError::Config(
            "the master ROM root must not contain '..'".to_string(),
        ));
    }
    Ok(())
}

/// Renders the config in the current, `[[source]]`-block format - see
/// `SourceFolderConfig`'s doc comment. Once this has run once (any
/// add/enable/disable/remove), the config no longer contains a plain
/// `source_folders = [...]` line at all.
///
/// Downgrade note: a pre-multi-source EmuWiz build's parser only
/// understands `key = value` / `key = [...]` lines (see
/// `config.toml.example`'s own note on this), not `[[source]]` tables. If
/// you downgrade to such a build after using any source-management
/// feature (Sources page, or `archivefs-cli source`/`sources` commands)
/// even once, that older build will see zero configured sources - it has
/// no `source_folders` key left to fall back on, and it does not
/// recognize `[[source]]`. Nothing is corrupted or lost: upgrading again
/// reads the same `[[source]]` blocks back correctly, and the sources
/// themselves are never deleted by this - only re-add them (or restore a
/// backup of `config.toml`) if you need them visible on an older build in
/// the meantime.
fn render_source_folder_configs(
    sources: &[SourceFolderConfig],
    mount_root: &Path,
    ratarmount_bin: &str,
    master_rom_root: Option<&Path>,
) -> String {
    let mut out = String::from("# EmuWiz configuration\n\n");
    out.push_str(&format!(
        "mount_root = {}\n",
        quote_config_string(&mount_root.display().to_string())
    ));
    out.push_str(&format!(
        "ratarmount_bin = {}\n",
        quote_config_string(ratarmount_bin)
    ));
    if let Some(master_rom_root) = master_rom_root {
        out.push_str(&format!(
            "master_rom_root = {}\n",
            quote_config_string(&master_rom_root.display().to_string())
        ));
    }
    for source in sources {
        out.push('\n');
        out.push_str("[[source]]\n");
        out.push_str(&format!(
            "path = {}\n",
            quote_config_string(&source.path.display().to_string())
        ));
        out.push_str(&format!("enabled = {}\n", source.enabled));
        if let Some(created_at) = &source.created_at {
            out.push_str(&format!(
                "created_at = {}\n",
                quote_config_string(created_at)
            ));
        }
    }
    out
}

fn quote_config_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Writes `contents` to `path` durably: a same-directory temp file, whose
/// contents are flushed to stable storage before it is published, then an
/// atomic `fs::rename`, then a best-effort sync of the parent directory.
///
/// # Why the syncs, and not just the rename
///
/// The rename alone is atomic on any POSIX filesystem, so a reader always
/// sees either the old complete file or the new one - never a splice of
/// both. That is not the same as durable. Without
/// [`File::sync_all`] on the temp file, a crash can order the rename ahead
/// of the data, publishing a file that is *present, named correctly and
/// empty or truncated*. For a preferences file that is worse than losing
/// the edit: an empty `cheat_sources.toml` still parses, as "no overrides",
/// so every stored preference is silently discarded rather than visibly
/// lost. Syncing the directory afterwards makes the rename itself survive
/// the same crash.
///
/// Directory sync is best-effort by platform: filesystems that do not give
/// directory handles `fsync` semantics report an error here, and that is
/// not a failure of the write - the data is already durable and published,
/// so the error is discarded rather than propagated.
///
/// # Permissions
///
/// When `path` already exists its permissions are copied onto the
/// replacement before the rename, so writing a config that a user had
/// tightened to `0600` does not silently widen it to the process umask's
/// default.
///
/// These writes happen only on an explicit user action (saving settings or
/// preferences), never in a hot loop, so the cost of two syncs is not a
/// throughput concern.
pub(crate) fn atomic_write_text(path: &Path, contents: &str) -> Result<()> {
    static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!("config path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;

    let temp_path = parent.join(format!(
        ".archivefs-config-write-{}-{}.tmp",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    // Scoped so the handle is closed before the rename: on Windows a rename
    // over an open file fails, and there is nothing to gain from holding it.
    let write_result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(source) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(ArchiveFsError::io(temp_path, source));
    }

    // Carry the existing file's mode across. Failure to read it is not fatal:
    // the write still has to happen, just with default permissions.
    if let Ok(existing) = fs::metadata(path) {
        let _ = fs::set_permissions(&temp_path, existing.permissions());
    }

    fs::rename(&temp_path, path).map_err(|source| {
        let _ = fs::remove_file(&temp_path);
        ArchiveFsError::io(path.to_path_buf(), source)
    })?;

    sync_directory_best_effort(parent);
    Ok(())
}

/// Flushes a directory entry change (a rename) to stable storage.
///
/// Best-effort on purpose - see [`atomic_write_text`]. Opening a directory
/// for reading and syncing it is the POSIX idiom; platforms and filesystems
/// that do not support it fail here, and the caller's write is still
/// complete and durable, so the error is deliberately dropped.
fn sync_directory_best_effort(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilesystemIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn filesystem_identity(metadata: &fs::Metadata) -> FilesystemIdentity {
    use std::os::unix::fs::MetadataExt;
    FilesystemIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn filesystem_identity(_metadata: &fs::Metadata) -> FilesystemIdentity {
    FilesystemIdentity {
        device: 0,
        inode: 0,
    }
}

fn validate_source_root_shape(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(ArchiveFsError::Config(format!(
            "source root must be an absolute non-root path: {}",
            path.display()
        )));
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(ArchiveFsError::Config(format!(
            "source root contains traversal components: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_source_root(path: &Path) -> Result<FilesystemIdentity> {
    validate_source_root_shape(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|source| ArchiveFsError::io(current.clone(), source))?;
        if metadata.file_type().is_symlink() {
            return Err(ArchiveFsError::Scanner(format!(
                "refusing symlinked source component {}",
                current.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    if !metadata.is_dir() {
        return Err(ArchiveFsError::Scanner(format!(
            "source root is not a directory: {}",
            path.display()
        )));
    }
    fs::read_dir(path).map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    Ok(filesystem_identity(&metadata))
}

pub(crate) fn validate_configured_source_roots(paths: &[PathBuf]) -> Result<()> {
    const MAX_CONFIGURED_SOURCE_ROOTS: usize = 4_096;
    if paths.len() > MAX_CONFIGURED_SOURCE_ROOTS {
        return Err(ArchiveFsError::Config(format!(
            "configured source count exceeds the {MAX_CONFIGURED_SOURCE_ROOTS} source limit"
        )));
    }
    let mut normalized: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        validate_source_root_shape(path)?;
        let path: PathBuf = path.components().collect();
        if let Some(existing) = normalized.iter().find(|existing| {
            path == **existing || path.starts_with(existing) || existing.starts_with(&path)
        }) {
            return Err(ArchiveFsError::Config(format!(
                "duplicate or overlapping source roots are not supported: {} and {}",
                existing.display(),
                path.display()
            )));
        }
        normalized.push(path);
    }
    Ok(())
}

/// Validates a user-supplied path as a candidate new source folder,
/// against the currently configured sources - the multi-source
/// milestone's shared validation, used identically by the GUI's Add
/// Folder dialog and the CLI's `source add` (never two implementations).
/// Confirms the path exists, is a directory, and is readable (via
/// `fs::read_dir`, which only lists directory entries - this never opens
/// or inspects any archive file inside it), then rejects it as a
/// duplicate or a parent/child overlap of any `existing` source, using
/// canonicalized paths for that comparison so a trailing separator or a
/// symlink cannot slip an equivalent path past the check. Overlaps are
/// rejected outright rather than accepted-with-confirmation, per the
/// milestone's stated preference, since nothing in this schema
/// deduplicates a file discovered under two different source folders.
///
/// Returns the exact absolute path to store with a harmless trailing
/// separator removed. Symlink components, traversal, filesystem-root paths,
/// and paths that cannot be represented losslessly in the UTF-8 config are
/// rejected rather than stored as aliases or lossy text.
pub fn validate_new_source_folder(candidate: &Path, existing: &[PathBuf]) -> Result<PathBuf> {
    let normalized: PathBuf = candidate.components().collect();

    validate_source_root_shape(candidate)?;
    if normalized.to_str().is_none() {
        return Err(ArchiveFsError::Config(format!(
            "source path cannot be stored losslessly in the UTF-8 configuration file: {}",
            normalized.display()
        )));
    }

    match inspect_path(&normalized) {
        PathInspection::Missing => {
            return Err(ArchiveFsError::Config(format!(
                "{} does not exist",
                normalized.display()
            )));
        }
        PathInspection::Other => {
            return Err(ArchiveFsError::Config(format!(
                "{} is not a directory",
                normalized.display()
            )));
        }
        PathInspection::PermissionDenied(detail) => {
            return Err(ArchiveFsError::Config(format!(
                "{} cannot be inspected: permission denied ({detail})",
                normalized.display()
            )));
        }
        PathInspection::MetadataError(detail) => {
            return Err(ArchiveFsError::Config(format!(
                "{} cannot be inspected: {detail}",
                normalized.display()
            )));
        }
        PathInspection::Directory => {}
    }

    validate_source_root(&normalized)?;

    if let Err(error) = fs::read_dir(&normalized) {
        return Err(ArchiveFsError::Config(format!(
            "{} cannot be read: {error}",
            normalized.display()
        )));
    }

    let candidate_canonical = fs::canonicalize(&normalized)
        .map_err(|source| ArchiveFsError::io(normalized.clone(), source))?;

    for existing_path in existing {
        validate_source_root_shape(existing_path)?;
        let existing_normalized: PathBuf = existing_path.components().collect();
        let existing_canonical = match inspect_path(&existing_normalized) {
            PathInspection::Missing => existing_normalized,
            PathInspection::Directory => {
                validate_source_root(&existing_normalized)?;
                fs::canonicalize(&existing_normalized)
                    .map_err(|source| ArchiveFsError::io(existing_normalized.clone(), source))?
            }
            PathInspection::Other => {
                return Err(ArchiveFsError::Config(format!(
                    "configured source root is not a directory: {}",
                    existing_normalized.display()
                )));
            }
            PathInspection::PermissionDenied(detail) | PathInspection::MetadataError(detail) => {
                return Err(ArchiveFsError::Config(format!(
                    "configured source root cannot be safely compared: {} ({detail})",
                    existing_normalized.display()
                )));
            }
        };
        if candidate_canonical == existing_canonical {
            return Err(ArchiveFsError::Config(format!(
                "{} is already a configured source folder",
                normalized.display()
            )));
        }
        if candidate_canonical.starts_with(&existing_canonical) {
            return Err(ArchiveFsError::Config(format!(
                "{} is inside the already-configured source folder {} - overlapping \
                 sources are not supported",
                normalized.display(),
                existing_path.display()
            )));
        }
        if existing_canonical.starts_with(&candidate_canonical) {
            return Err(ArchiveFsError::Config(format!(
                "{} would contain the already-configured source folder {} - overlapping \
                 sources are not supported",
                normalized.display(),
                existing_path.display()
            )));
        }
    }

    Ok(normalized)
}

fn now_utc_timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    format_unix_timestamp_utc(seconds)
}

// -----------------------------------------------------------------------
// Multi-source management: shared CLI/GUI operations. Every mutating
// function here follows the same three-step shape - load the full
// per-source config list, mutate it in memory, save it back atomically -
// then, where relevant, brings the database's `source_folders` table in
// sync via `Database::register_source_folders` (which already knows how
// to add/refresh/mark-removed a path, see its own doc comment) rather
// than inventing a second synchronization mechanism.
// -----------------------------------------------------------------------

/// The five states the multi-source milestone requires the Sources page
/// to distinguish. `Disabled` always wins regardless of scan history (a
/// user who disabled a source doesn't need to be told it also failed to
/// scan). For an enabled source, `None`/`Success` scan history reads as
/// `Available` (optimistic for a never-yet-scanned source - it was
/// validated as an existing, readable directory at add time); a failed
/// scan is further classified from its error text into `PermissionDenied`
/// / `Unavailable` (path missing) / `ScanFailed` (anything else) by
/// [`classify_scan_failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SourceAvailability {
    Available,
    Unavailable,
    PermissionDenied,
    Disabled,
    ScanFailed,
}

/// Pure and fully deterministic from already-known state (never touches
/// the filesystem itself) - see [`SourceAvailability`]'s doc comment for
/// the precedence rules.
pub fn classify_source_availability(
    enabled: bool,
    last_scan_status: Option<SourceScanStatus>,
    last_scan_error: Option<&str>,
) -> SourceAvailability {
    if !enabled {
        return SourceAvailability::Disabled;
    }
    match last_scan_status {
        None | Some(SourceScanStatus::Success) => SourceAvailability::Available,
        Some(SourceScanStatus::Failed) => {
            classify_scan_failure(last_scan_error.unwrap_or_default())
        }
    }
}

/// Classifies a scan failure's error text (always produced by this
/// process's own `ArchiveFsError`/`io::Error` formatting, never
/// user-supplied) into the milestone's three failure categories. Matches
/// the standard Linux `io::Error` `Display` text for the two specific,
/// stable cases the milestone calls out; anything else is `ScanFailed`
/// rather than mis-filed as one of the more specific categories.
fn classify_scan_failure(error: &str) -> SourceAvailability {
    let lower = error.to_lowercase();
    if lower.contains("permission denied") {
        SourceAvailability::PermissionDenied
    } else if lower.contains("no such file") || lower.contains("not found") {
        SourceAvailability::Unavailable
    } else {
        SourceAvailability::ScanFailed
    }
}

/// One source folder's complete display state for the Sources page/CLI -
/// config-owned facts (`enabled`, `created_at`) merged with
/// database-owned facts (`id`, scan history) by exact path, via
/// [`build_source_folder_views`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFolderView {
    pub path: PathBuf,
    pub enabled: bool,
    pub created_at: Option<String>,
    /// `None` only if this source has never been registered in the
    /// database at all - in practice this should not happen once
    /// `add_source_folder_at` always registers on add, but is kept
    /// `Option` rather than assumed for robustness against a config
    /// edited by hand outside the app.
    pub id: Option<i64>,
    pub availability: SourceAvailability,
    pub last_scan_status: Option<SourceScanStatus>,
    pub last_scan_error: Option<String>,
    pub last_scan_at: Option<String>,
    pub last_successful_scan_at: Option<String>,
    pub last_archive_count: Option<i64>,
    pub assigned_platform: Option<String>,
    pub unknown_archive_count: i64,
}

/// Joins the config's per-source list against the database's per-source
/// scan history by exact path - a pure, DB-free function so it is
/// directly testable with hand-built fixtures (see the tests module).
pub fn build_source_folder_views(
    sources: &[SourceFolderConfig],
    records: &[SourceFolderRecord],
) -> Vec<SourceFolderView> {
    sources
        .iter()
        .map(|source| {
            let record = records.iter().find(|record| record.path == source.path);
            let last_scan_status = record.and_then(|record| record.last_scan_status);
            let last_scan_error = record.and_then(|record| record.last_scan_error.clone());
            SourceFolderView {
                path: source.path.clone(),
                enabled: source.enabled,
                created_at: source.created_at.clone(),
                id: record.map(|record| record.id),
                availability: classify_source_availability(
                    source.enabled,
                    last_scan_status,
                    last_scan_error.as_deref(),
                ),
                last_scan_status,
                last_scan_error,
                last_scan_at: record.and_then(|record| record.last_scan_at.clone()),
                last_successful_scan_at: record
                    .and_then(|record| record.last_successful_scan_at.clone()),
                last_archive_count: record.and_then(|record| record.last_archive_count),
                assigned_platform: record.and_then(|record| record.assigned_platform.clone()),
                unknown_archive_count: record.map_or(0, |record| record.unknown_archive_count),
            }
        })
        .collect()
}

pub fn list_source_folder_views_default() -> Result<Vec<SourceFolderView>> {
    list_source_folder_views_at(&default_config_path()?, &default_database_path()?)
}

pub fn list_source_folder_views_at(
    config_path: &Path,
    database_path: &Path,
) -> Result<Vec<SourceFolderView>> {
    let sources = load_source_folder_configs_from(config_path)?;
    let database = Database::open_read_only(database_path)?;
    let records = database.list_source_folders()?;
    Ok(build_source_folder_views(&sources, &records))
}

/// Saves an explicit platform default for a configured source. The returned
/// count is the number of currently visible Unknown rows that a subsequent
/// rescan may safely recover; callers must show it before invoking this
/// mutating action.
pub fn preview_source_platform_assignment_at(database_path: &Path, target: &Path) -> Result<i64> {
    let database = Database::open_or_create(database_path)?;
    let record = database
        .list_source_folders()?
        .into_iter()
        .find(|record| record.path == target)
        .ok_or_else(|| {
            ArchiveFsError::Database(format!(
                "source folder {} is not registered",
                target.display()
            ))
        })?;
    Ok(record.unknown_archive_count)
}

pub fn assign_source_platform_at(
    config_path: &Path,
    database_path: &Path,
    target: &Path,
    platform: &str,
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    let canonical = canonical_platform_for_alias(platform).ok_or_else(|| {
        ArchiveFsError::Config(format!("unknown platform assignment '{platform}'"))
    })?;
    let sources = load_source_folder_configs_from(config_path)?;
    if !sources.iter().any(|source| source.path == target) {
        return Err(ArchiveFsError::Config(format!(
            "source folder {} is not configured",
            target.display()
        )));
    }
    let all_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    let mut database = Database::open_or_create(database_path)?;
    database.register_source_folders(&all_paths)?;
    database.set_source_platform_assignment(target, Some(canonical))?;
    drop(database);
    scan_source_folder_at(config_path, database_path, target, triggered_by)
}

pub fn assign_source_platform_default(target: &Path, platform: &str) -> Result<ScanPersistSummary> {
    assign_source_platform_at(
        &default_config_path()?,
        &default_database_path()?,
        target,
        platform,
        "gui-assign-source-platform",
    )
}

/// Adds `candidate` as a new, enabled source folder: validates it against
/// every currently configured source (see [`validate_new_source_folder`]),
/// atomically saves the updated config, then immediately registers it in
/// the database so it has a stable id and shows up on the Sources page
/// right away - deliberately *never* scans it (the milestone requires an
/// explicit, separate Scan action; adding a source must never itself walk
/// the filesystem beyond the one-time validation check).
pub fn add_source_folder_default(candidate: &Path) -> Result<SourceFolderConfig> {
    add_source_folder_at(
        &default_config_path()?,
        &default_database_path()?,
        candidate,
    )
}

pub fn add_source_folder_at(
    config_path: &Path,
    database_path: &Path,
    candidate: &Path,
) -> Result<SourceFolderConfig> {
    let mut sources = load_source_folder_configs_from(config_path)?;
    let config = Config::load_from(config_path)?;

    let existing_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    let validated_path = validate_new_source_folder(candidate, &existing_paths)?;

    let new_source = SourceFolderConfig {
        path: validated_path,
        enabled: true,
        created_at: Some(now_utc_timestamp()),
    };
    sources.push(new_source.clone());

    save_source_folder_configs_to(
        config_path,
        &sources,
        &config.mount_root,
        &config.ratarmount_bin,
        config.master_rom_root.as_deref(),
    )?;

    let all_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    let mut database = Database::open_or_create(database_path)?;
    database.register_source_folders(&all_paths)?;

    Ok(new_source)
}

/// The result of enabling or disabling a source folder. `scan` is only
/// `Some` when *enabling* - re-enabling a source must not silently trust
/// old filesystem state, so this always scans that one source as part of
/// the same operation before returning (never a separate step the caller
/// could forget); disabling never scans anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetSourceFolderEnabledOutcome {
    pub source: SourceFolderConfig,
    pub scan: Option<ScanPersistSummary>,
}

pub fn set_source_folder_enabled_default(
    target: &Path,
    enabled: bool,
) -> Result<SetSourceFolderEnabledOutcome> {
    set_source_folder_enabled_at(
        &default_config_path()?,
        &default_database_path()?,
        target,
        enabled,
    )
}

pub fn set_source_folder_enabled_at(
    config_path: &Path,
    database_path: &Path,
    target: &Path,
    enabled: bool,
) -> Result<SetSourceFolderEnabledOutcome> {
    let mut sources = load_source_folder_configs_from(config_path)?;
    let config = Config::load_from(config_path)?;

    let index = sources
        .iter()
        .position(|source| source.path == target)
        .ok_or_else(|| {
            ArchiveFsError::Config(format!(
                "{} is not a configured source folder",
                target.display()
            ))
        })?;
    sources[index].enabled = enabled;
    let updated = sources[index].clone();

    save_source_folder_configs_to(
        config_path,
        &sources,
        &config.mount_root,
        &config.ratarmount_bin,
        config.master_rom_root.as_deref(),
    )?;

    let all_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    let mut database = Database::open_or_create(database_path)?;
    let registered = database.register_source_folders(&all_paths)?;

    let scan = if enabled {
        let folder = registered
            .into_iter()
            .find(|folder| folder.path == target)
            .ok_or_else(|| {
                ArchiveFsError::Database(format!(
                    "source folder {} could not be resolved to a database id",
                    target.display()
                ))
            })?;
        Some(scan_and_persist_folders(
            &mut database,
            std::slice::from_ref(&folder),
            "source-enable",
        )?)
    } else {
        None
    };

    Ok(SetSourceFolderEnabledOutcome {
        source: updated,
        scan,
    })
}

/// The result of removing a source folder from configuration.
/// `catalogue_rows_removed` is `None` when the default, safe "Keep
/// catalogue entries" choice was used - the only difference between the
/// two removal modes is whether this is `None` or `Some(count)`; the
/// config-file and `source_folders`-table changes are identical either
/// way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveSourceFolderOutcome {
    pub removed_source: SourceFolderConfig,
    pub catalogue_rows_removed: Option<usize>,
}

/// Removes `target` from EmuWiz configuration only - never the
/// directory or any file inside it; this function never touches the
/// filesystem the source folder points to at all. Defaults
/// (`keep_catalogue = true`) to preserving every archive row the source
/// ever contributed, marking its `source_folders` row
/// `removed_from_config_at` but leaving `archives` untouched, so
/// re-adding the same path later reunites with its history. Only when the
/// caller explicitly passes `keep_catalogue = false` are that source's
/// catalogue rows atomically deleted (see
/// [`Database::remove_source_folder_catalogue`]) - never another
/// source's.
pub fn remove_source_folder_default(
    target: &Path,
    keep_catalogue: bool,
) -> Result<RemoveSourceFolderOutcome> {
    remove_source_folder_at(
        &default_config_path()?,
        &default_database_path()?,
        target,
        keep_catalogue,
    )
}

pub fn remove_source_folder_at(
    config_path: &Path,
    database_path: &Path,
    target: &Path,
    keep_catalogue: bool,
) -> Result<RemoveSourceFolderOutcome> {
    let mut sources = load_source_folder_configs_from(config_path)?;
    let config = Config::load_from(config_path)?;

    let index = sources
        .iter()
        .position(|source| source.path == target)
        .ok_or_else(|| {
            ArchiveFsError::Config(format!(
                "{} is not a configured source folder",
                target.display()
            ))
        })?;
    let removed_source = sources.remove(index);

    save_source_folder_configs_to(
        config_path,
        &sources,
        &config.mount_root,
        &config.ratarmount_bin,
        config.master_rom_root.as_deref(),
    )?;

    let mut database = Database::open_or_create(database_path)?;

    // Resolve the removed source's database id *before* re-registering
    // only the remaining paths (which is what actually marks its row
    // `removed_from_config_at` - see `register_source_folders`'s doc
    // comment). Registering the full original list first guarantees an
    // id exists even for a source that was somehow never registered
    // before (for example a config edited by hand outside the app).
    let all_paths_including_removed: Vec<PathBuf> = std::iter::once(removed_source.path.clone())
        .chain(sources.iter().map(|source| source.path.clone()))
        .collect();
    let registered = database.register_source_folders(&all_paths_including_removed)?;
    let removed_folder = registered
        .into_iter()
        .find(|folder| folder.path == removed_source.path)
        .ok_or_else(|| {
            ArchiveFsError::Database(format!(
                "source folder {} could not be resolved to a database id",
                removed_source.path.display()
            ))
        })?;

    let remaining_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    database.register_source_folders(&remaining_paths)?;

    let catalogue_rows_removed = if keep_catalogue {
        None
    } else {
        Some(database.remove_source_folder_catalogue(removed_folder.id)?)
    };

    Ok(RemoveSourceFolderOutcome {
        removed_source,
        catalogue_rows_removed,
    })
}

/// Scans exactly one configured source folder, enabled or disabled - an
/// explicit, targeted action, so unlike [`scan_all_enabled_sources_at`]
/// this does not check `enabled` at all (a user directly clicking "Scan"
/// on one row is unambiguous explicit intent). Shares
/// `scan_and_persist_folders` with every other scan entry point; nothing
/// here duplicates archive-discovery logic.
pub fn scan_source_folder_default(target: &Path) -> Result<ScanPersistSummary> {
    scan_source_folder_at(
        &default_config_path()?,
        &default_database_path()?,
        target,
        "cli-source-scan",
    )
}

pub fn scan_source_folder_at(
    config_path: &Path,
    database_path: &Path,
    target: &Path,
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    let sources = load_source_folder_configs_from(config_path)?;
    let configured_paths = sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    validate_configured_source_roots(&configured_paths)?;
    if !sources.iter().any(|source| source.path == target) {
        return Err(ArchiveFsError::Config(format!(
            "{} is not a configured source folder",
            target.display()
        )));
    }

    let all_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    let mut database = Database::open_or_create(database_path)?;
    let registered = database.register_source_folders(&all_paths)?;
    let folder = registered
        .into_iter()
        .find(|folder| folder.path == target)
        .ok_or_else(|| {
            ArchiveFsError::Database(format!(
                "source folder {} could not be resolved to a database id",
                target.display()
            ))
        })?;

    scan_and_persist_folders(&mut database, std::slice::from_ref(&folder), triggered_by)
}

/// Scans every *enabled* configured source folder independently - a
/// disabled source is registered (so it stays "configured", never marked
/// removed) but never walked, exactly matching "excluded from Scan All"
/// and "not treated as missing". One folder's failure never affects
/// another's, via the same per-folder isolation `scan_and_persist_folders`
/// always provides.
pub fn scan_all_enabled_sources_default() -> Result<ScanPersistSummary> {
    scan_all_enabled_sources_at(
        &default_config_path()?,
        &default_database_path()?,
        "cli-scan-all-sources",
    )
}

pub fn scan_all_enabled_sources_at(
    config_path: &Path,
    database_path: &Path,
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    let sources = load_source_folder_configs_from(config_path)?;
    let all_paths: Vec<PathBuf> = sources.iter().map(|source| source.path.clone()).collect();
    validate_configured_source_roots(&all_paths)?;
    let mut database = Database::open_or_create(database_path)?;
    let registered = database.register_source_folders(&all_paths)?;

    let enabled_paths: HashSet<&PathBuf> = sources
        .iter()
        .filter(|source| source.enabled)
        .map(|source| &source.path)
        .collect();
    let enabled_folders: Vec<RegisteredSourceFolder> = registered
        .into_iter()
        .filter(|folder| enabled_paths.contains(&folder.path))
        .collect();

    scan_and_persist_folders(&mut database, &enabled_folders, triggered_by)
}

/// Resolves a CLI-style `<id-or-path>` argument to a configured source
/// folder's exact path - shared by every CLI source subcommand so the
/// numeric-id-vs-path parsing logic exists exactly once. A pure numeric
/// string is looked up against database ids; anything else is compared
/// directly against configured paths.
pub fn resolve_source_folder_identifier(
    identifier: &str,
    sources: &[SourceFolderConfig],
    records: &[SourceFolderRecord],
) -> Result<PathBuf> {
    if let Ok(id) = identifier.parse::<i64>() {
        let record = records
            .iter()
            .find(|record| record.id == id)
            .ok_or_else(|| ArchiveFsError::Config(format!("no source folder with id {id}")))?;
        return Ok(record.path.clone());
    }
    let candidate = PathBuf::from(identifier);
    if sources.iter().any(|source| source.path == candidate) {
        return Ok(candidate);
    }
    Err(ArchiveFsError::Config(format!(
        "no configured source folder matches '{identifier}'"
    )))
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
    /// A loose Mega Drive/Genesis ROM. It is catalogued but deliberately
    /// marked unsupported for EmuWiz's archive-mount backend.
    MegaDriveRom,
    /// A supported game image that is catalogued directly rather than
    /// requiring an archive wrapper. Scanning never mounts or modifies it.
    DirectGameImage,
}

impl ArchiveKind {
    /// Whether this library entry is an EmuWiz archive-mount input.
    /// Loose cartridge ROMs remain selectable library content but never
    /// become queue or mount candidates.
    pub fn is_mount_input(self) -> bool {
        !matches!(self, Self::MegaDriveRom | Self::DirectGameImage)
    }
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
    pub filesystem_device: Option<u64>,
    pub filesystem_inode: Option<u64>,
    pub source_filesystem_device: Option<u64>,
    pub source_filesystem_inode: Option<u64>,
}

impl ArchiveIdentity {
    pub fn from_path(
        path: &Path,
        source_root: impl Into<PathBuf>,
        metadata: Option<&fs::Metadata>,
    ) -> Self {
        let source_root = source_root.into();
        let source_metadata = fs::symlink_metadata(&source_root).ok();
        let file_identity = metadata.map(filesystem_identity);
        let source_identity = source_metadata.as_ref().map(filesystem_identity);
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
            filesystem_device: file_identity.map(|identity| identity.device),
            filesystem_inode: file_identity.map(|identity| identity.inode),
            source_filesystem_device: source_identity.map(|identity| identity.device),
            source_filesystem_inode: source_identity.map(|identity| identity.inode),
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
        metadata.platform = archive
            .identity
            .platform
            .clone()
            .or_else(|| detect_platform(&archive.path, &archive.identity.source_root));
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
        let source_root = source_root.into();
        let kind = archive_kind_in_root(path, &source_root)?;
        let metadata = fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        Some(Self {
            path: path.to_path_buf(),
            kind,
            identity: {
                let mut identity = ArchiveIdentity::from_path(path, source_root, metadata.as_ref());
                if kind == ArchiveKind::DirectGameImage
                    && let Some(platform) = detect_direct_image_header_platform(path)
                {
                    identity.platform = Some(platform.to_string());
                    identity.platform_provenance = Some(PlatformProvenance::HeaderIdentity);
                }
                // A Mega Drive ROM's *kind* is weak evidence: it comes from an
                // extension. It may fill in a platform nothing else identified,
                // but it must never replace a platform that stronger evidence
                // already established - which is how `scummvm/laurabow2/
                // RESOURCE.GEN` came to be labelled MegaDrive while its
                // provenance still read `FolderAlias` for a folder named
                // `scummvm`.
                if kind == ArchiveKind::MegaDriveRom && identity.platform.is_none() {
                    identity.platform = Some("MegaDrive".to_string());
                    identity
                        .platform_provenance
                        .get_or_insert(PlatformProvenance::Heuristic);
                }
                identity
            },
            health: if kind == ArchiveKind::MegaDriveRom {
                ArchiveHealth::Unsupported
            } else {
                ArchiveHealth::Pending
            },
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

    // `.gen` deliberately never resolves here, even though it would collide
    // with Mega Drive dumps: it also collides with Sierra SCI
    // `RESOURCE.GEN` files, which every ScummVM game directory contains,
    // and classifying one of those as a Mega Drive ROM is exactly the
    // misdetection an earlier milestone fixed. A `.gen` (or `.md`/`.bin`)
    // file is recognised as a Mega Drive ROM only once folder, source-root,
    // or cartridge-header evidence corroborates it - see
    // `archive_kind_in_root`. Every extension below is, by contrast,
    // self-evidencing: the media registry is the single source of truth
    // for it, shared with the filesystem watcher (`media_registry::
    // is_watch_relevant_extension`) so the two can never drift apart again.
    let extension = Path::new(&filename).extension()?.to_str()?;
    media_registry::kind_for_extension(extension)
}

pub(crate) fn archive_kind_in_root(path: &Path, source_root: &Path) -> Option<ArchiveKind> {
    if let Some(kind) = archive_kind(path) {
        return Some(kind);
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !media_registry::is_corroboration_candidate(&extension) {
        return None;
    }
    // Corroboration, in the same priority order detection uses everywhere:
    // the containing folder, the configured source root, or the cartridge
    // header itself. A shared extension alone is never enough.
    let nested_match = detect_platform_from_folder_alias_with_match(path, source_root)
        .is_some_and(|(platform, _)| platform == "MegaDrive");
    let source_root_match = source_root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(folder_platform_alias)
        == Some("MegaDrive");
    // A folder that names a *different* platform is a positive refusal, not
    // merely an absence of support: a `.gen` inside a ScummVM game directory
    // is a resource file, whatever its extension suggests.
    let contradicted = detect_platform_from_folder_alias_with_match(path, source_root)
        .is_some_and(|(platform, _)| platform != "MegaDrive");
    if contradicted {
        return None;
    }
    let header_match = !nested_match && !source_root_match && has_mega_drive_cartridge_header(path);
    (nested_match || source_root_match || header_match).then_some(ArchiveKind::MegaDriveRom)
}

pub fn is_supported_archive(path: impl AsRef<Path>) -> bool {
    archive_kind(path).is_some()
}

/// Identifies only formats whose uncompressed disc header is directly
/// available. Compressed RVZ/GCZ images deliberately remain visible without
/// pretending that their exact identity was extracted.
///
/// The GameCube and Wii magic words are no longer written out here: they are
/// two [`platform::MagicRule`] entries in the registry, so the disc check and
/// everything else that reasons about signatures read the same table. The
/// behaviour is unchanged - one bounded read, `.iso`/`.gcm` only, and `None`
/// rather than a guess when neither word matches.
fn detect_direct_image_header_platform(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "iso" | "gcm") {
        return None;
    }
    let request = platform::DetectionRequest::new(path, Path::new("")).inspecting_content();
    let report = platform::detect_platform_report(&request);
    report
        .evidence
        .iter()
        .find(|item| {
            item.source == platform::DetectionSource::Signature
                && matches!(item.platform, "GameCube" | "Wii")
        })
        .map(|item| item.platform)
}

/// Whether `path` carries a real Mega Drive cartridge header, using the
/// registry's own signature rule rather than a second copy of the offset.
///
/// Bounded: one short read at a known offset. No trusted roots are supplied, so
/// a symlink is refused here exactly as it always was - `archive_kind_in_root`
/// has no access to the configuration, and inventing a trusted set from a
/// single `source_root` argument would be guessing at what the user configured.
/// Deliberately left fail-closed; see `crate::safe_read`.
fn has_mega_drive_cartridge_header(path: &Path) -> bool {
    let request = platform::DetectionRequest::new(path, Path::new("")).inspecting_content();
    platform::detect_platform_report(&request)
        .evidence
        .iter()
        .any(|item| {
            item.source == platform::DetectionSource::Signature && item.platform == "MegaDrive"
        })
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
    NotMountable,
}

impl fmt::Display for MountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Mounted => write!(f, "Mounted"),
            Self::MountPathExists => write!(f, "MountPathExists"),
            Self::NotMountable => write!(f, "NotMountable"),
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

    pub fn is_mount_input(&self) -> bool {
        self.mount_plan.archive.kind.is_mount_input()
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

/// Read-only duplicate information for catalogue-backed callers such as the
/// GUI. This deliberately remains separate from the serialized CLI
/// [`DuplicateReport`] so that enriching the GUI cannot change the existing
/// human or JSON compatibility surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueDuplicateReport {
    pub groups: Vec<CatalogueDuplicateGroup>,
    pub archives_in_groups: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueDuplicateGroup {
    pub normalized_title: String,
    pub title: String,
    pub platform: String,
    pub reason: String,
    pub entries: Vec<CatalogueDuplicateArchive>,
    /// Sum of only the entry sizes which are actually known.
    pub total_known_size_bytes: u128,
    /// Makes a partial known-size sum explicit rather than implying it is a
    /// complete byte total for the group.
    pub entries_with_known_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueDuplicateArchive {
    pub archive_id: i64,
    pub path: PathBuf,
    pub present: bool,
    pub size_bytes: Option<u64>,
    pub modified_time_unix_seconds: Option<i64>,
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
            let name = duplicate_normalized_name(&record.mount_plan.archive.path);
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

fn duplicate_normalized_name(path: &Path) -> String {
    safe_mount_name(path).to_lowercase()
}

/// Builds deterministic likely-duplicate groups from already-loaded catalogue
/// rows without reading or changing the filesystem. The grouping key is the
/// same normalized filename plus effective-platform key used by
/// [`FilenameDuplicateDetector`]. Missing state and metadata are display
/// attributes, never grouping inputs.
pub fn catalogue_filename_duplicates(archives: &[PersistedArchive]) -> CatalogueDuplicateReport {
    let mut grouped = BTreeMap::<(String, String), Vec<&PersistedArchive>>::new();
    for archive in archives {
        let platform = archive
            .platform
            .clone()
            .unwrap_or_else(|| "Unknown".to_string());
        let normalized_title = duplicate_normalized_name(&archive.absolute_path);
        grouped
            .entry((platform, normalized_title))
            .or_default()
            .push(archive);
    }

    let groups = grouped
        .into_iter()
        .filter_map(|((platform, normalized_title), mut archives)| {
            if archives.len() < 2 {
                return None;
            }
            archives.sort_by(|left, right| left.absolute_path.cmp(&right.absolute_path));
            let title = archives
                .first()
                .map(|archive| archive.display_name.clone())
                .unwrap_or_else(|| normalized_title.clone());
            let entries_with_known_size = archives
                .iter()
                .filter(|archive| archive.size_bytes.is_some())
                .count();
            let total_known_size_bytes = archives
                .iter()
                .filter_map(|archive| archive.size_bytes)
                .map(u128::from)
                .sum();
            let entries = archives
                .into_iter()
                .map(|archive| CatalogueDuplicateArchive {
                    archive_id: archive.id,
                    path: archive.absolute_path.clone(),
                    present: archive.last_verified_missing_at.is_none(),
                    size_bytes: archive.size_bytes,
                    modified_time_unix_seconds: archive.modified_time_unix_seconds,
                })
                .collect();
            Some(CatalogueDuplicateGroup {
                normalized_title,
                title,
                platform,
                reason: "Matching normalized filename and platform".to_string(),
                entries,
                total_known_size_bytes,
                entries_with_known_size,
            })
        })
        .collect::<Vec<_>>();
    let archives_in_groups = groups.iter().map(|group| group.entries.len()).sum();
    CatalogueDuplicateReport {
        groups,
        archives_in_groups,
    }
}

/// A single archive's overall health category - see `classify_archive_health`
/// for the truthful, non-invented rules deriving this (v0.4.3-alpha, Health
/// and Recovery Dashboard). Every variant here is backed by state EmuWiz
/// can already observe reliably; nothing here is inferred or guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HealthCategory {
    /// A live mount attempt failed in a way
    /// [`ArchiveHealth::is_terminal_without_source_change`] says will not
    /// succeed by retrying alone (corrupt archive, unsupported format,
    /// permission denied).
    TerminalFailure,
    /// A live mount attempt failed in a way [`ArchiveHealth::is_retryable`]
    /// says may succeed if retried.
    RetryableFailure,
    /// A failure value retained from an earlier catalogue observation. It is
    /// evidence that something failed before, not evidence of a failure in
    /// the current process/session.
    HistoricalMountFailure,
    /// The item is deliberately catalogued as a loose game/resource and the
    /// archive mount backend is not applicable to it.
    MountNotRequired,
    /// A failure value exists, but neither current mount state nor a retained
    /// observation timestamp establishes when it happened.
    MountFailureEvidenceInsufficient,
    /// A remount or lazy-unmount recovery offer is currently active for
    /// this exact archive in this session.
    RecoveryAvailable,
    /// The persisted catalogue's last successful scan explicitly marked
    /// this archive missing (`last_verified_missing_at` is set).
    Missing,
    /// Known to the persisted catalogue and not marked missing, but not
    /// yet confirmed present by the current live snapshot.
    AwaitingValidation,
    /// Known to the persisted catalogue, not confirmed present by the
    /// current live snapshot, and not currently reachable on disk either.
    CachedOnly,
    /// No current platform assignment - manual, alias, or automatic.
    UnknownPlatform,
}

impl HealthCategory {
    /// The milestone's documented default severity order: lower is more
    /// severe. Used only to sort the issue list; never to hide a category.
    pub fn severity_rank(self) -> u8 {
        match self {
            Self::TerminalFailure => 1,
            Self::RetryableFailure => 2,
            Self::RecoveryAvailable => 3,
            Self::HistoricalMountFailure => 4,
            Self::MountFailureEvidenceInsufficient => 5,
            Self::MountNotRequired => 6,
            Self::Missing => 7,
            Self::AwaitingValidation => 8,
            Self::CachedOnly => 9,
            Self::UnknownPlatform => 10,
        }
    }

    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RetryableFailure)
    }

    /// A short, human-readable classification label - never a raw enum or
    /// database source string (see the milestone's UI wording rule).
    pub fn label(self) -> &'static str {
        match self {
            Self::TerminalFailure => "Terminal failure",
            Self::RetryableFailure => "Retryable failure",
            Self::HistoricalMountFailure => "Historical mount failure",
            Self::MountNotRequired => "No mount required",
            Self::MountFailureEvidenceInsufficient => "Earlier mount result needs context",
            Self::RecoveryAvailable => "Recovery available",
            Self::Missing => "Missing",
            Self::AwaitingValidation => "Awaiting validation",
            Self::CachedOnly => "Cached only",
            Self::UnknownPlatform => "Unknown platform",
        }
    }
}

/// Whether an archive is confirmed by the caller's live session, or only
/// known some other way - see `classify_archive_health`. A catalogue-only
/// caller with no live session (e.g. the CLI) can only ever truthfully
/// assert `Confirmed` (not known to be missing) or `Missing`;
/// `AwaitingValidation` and `Unreachable` both require a live session to
/// be a meaningful claim, so `catalogue_health_report` never produces
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePresence {
    /// Live-confirmed this session, or (for a catalogue-only caller)
    /// simply not known to be missing.
    Confirmed,
    /// The persisted catalogue's last successful scan explicitly marked
    /// this archive missing.
    Missing,
    /// Known to the catalogue, not marked missing, not yet confirmed by
    /// the current live snapshot, but still reachable on disk right now.
    AwaitingValidation,
    /// Known to the catalogue, not confirmed by the current live
    /// snapshot, and not currently reachable on disk either.
    Unreachable,
}

/// Which existing recovery offer (if any) is currently active for an
/// archive - see the GUI's `lazy_unmount_offers`/`remount_offers` session
/// state, the only place this ever varies. Always `None` outside a GUI
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecoveryOffer {
    Remount,
    LazyUnmount,
}

/// Which existing, already-implemented action (if any) can safely resolve
/// a [`HealthIssue`] - see the milestone's "reuse existing action
/// availability checks... do not implement a second mount, unmount,
/// remount, or cleanup path" requirement. Always one of the actions
/// already wired up elsewhere; never a new action kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecoveryAction {
    RetryMount,
    Remount,
    LazyUnmount,
}

/// Every raw signal [`classify_archive_health`] needs for one archive.
/// Borrowed, not owned - the caller (GUI or CLI) already owns all of this
/// data; classification never clones anything until it decides an issue
/// actually exists.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveHealthInput<'a> {
    pub path: &'a Path,
    pub platform: Option<&'a str>,
    pub presence: ArchivePresence,
    /// `Some` establishes current-session context. `None` keeps a retained
    /// health value historical/uncertain rather than silently promoting it.
    pub mount_state: Option<MountState>,
    /// A current value when `mount_state` is present, or a retained value
    /// when the caller supplies it without current mount state.
    pub archive_health: Option<ArchiveHealth>,
    pub recovery_offer: Option<RecoveryOffer>,
    pub last_seen_at: Option<&'a str>,
    pub size_bytes: Option<u64>,
    pub modified_time_unix_seconds: Option<i64>,
}

/// One archive that needs attention, plus everything the milestone's
/// health issue list requires to display it without a raw enum or
/// database source string - see `classify_archive_health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthIssue {
    pub path: PathBuf,
    pub platform: Option<String>,
    pub present: bool,
    pub mount_state: Option<MountState>,
    pub category: HealthCategory,
    pub reason: String,
    pub retryable: bool,
    pub recovery_action: Option<RecoveryAction>,
    pub last_seen_at: Option<String>,
    pub size_bytes: Option<u64>,
    pub modified_time_unix_seconds: Option<i64>,
}

impl HealthIssue {
    pub fn recovery_available(&self) -> bool {
        self.recovery_action.is_some()
    }
}

fn health_issue_reason(category: HealthCategory, recovery_offer: Option<RecoveryOffer>) -> String {
    match category {
        HealthCategory::TerminalFailure => {
            "A current mount input remains in a terminal failure state".to_string()
        }
        HealthCategory::RetryableFailure => "Mount failed and may be retried".to_string(),
        HealthCategory::HistoricalMountFailure => {
            "An earlier stored observation recorded a mount failure; this is not a current failure"
                .to_string()
        }
        HealthCategory::MountNotRequired => {
            "This loose game or resource is catalogued directly and does not use the archive mount workflow"
                .to_string()
        }
        HealthCategory::MountFailureEvidenceInsufficient => {
            "A failure value was recorded without enough evidence to call it current".to_string()
        }
        HealthCategory::RecoveryAvailable => match recovery_offer {
            Some(RecoveryOffer::Remount) => "Remount is available".to_string(),
            Some(RecoveryOffer::LazyUnmount) => "Lazy-unmount recovery is available".to_string(),
            None => "Recovery is available".to_string(),
        },
        HealthCategory::Missing => "Missing from latest successful scan".to_string(),
        HealthCategory::AwaitingValidation => "Awaiting validation".to_string(),
        HealthCategory::CachedOnly => "Archive exists only in the cached catalogue".to_string(),
        HealthCategory::UnknownPlatform => "Platform could not be determined".to_string(),
    }
}

/// The single, truthful rule set deciding whether one archive needs
/// attention, and if so, exactly which category - see the milestone's
/// documented severity order ([`HealthCategory::severity_rank`]) and "do
/// not invent health states" requirement. Every branch here is backed by
/// a signal `input` already carries; nothing is guessed. Returns `None`
/// for a healthy archive - callers must never synthesize an issue for
/// one.
///
/// Priority (highest first, matching [`HealthCategory::severity_rank`]): a
/// live mount failure always outranks a merely-offered recovery, which
/// outranks catalogue presence, which outranks an unknown platform - an
/// archive gets exactly one category, its single most severe applicable
/// one, never more than one.
pub fn classify_archive_health(input: &ArchiveHealthInput<'_>) -> Option<HealthIssue> {
    let recorded_failure = input
        .archive_health
        .filter(|health| health.is_terminal_without_source_change() || health.is_retryable());
    let category = recorded_failure.map(|health| {
        if input.mount_state == Some(MountState::NotMountable) {
            HealthCategory::MountNotRequired
        } else if input.mount_state.is_some() {
            if health.is_terminal_without_source_change() {
                HealthCategory::TerminalFailure
            } else {
                HealthCategory::RetryableFailure
            }
        } else if input.last_seen_at.is_some() {
            HealthCategory::HistoricalMountFailure
        } else {
            HealthCategory::MountFailureEvidenceInsufficient
        }
    });

    let category = category.or_else(|| {
        input
            .recovery_offer
            .map(|_| HealthCategory::RecoveryAvailable)
    });

    let category = category.or(match input.presence {
        ArchivePresence::Missing => Some(HealthCategory::Missing),
        ArchivePresence::AwaitingValidation => Some(HealthCategory::AwaitingValidation),
        ArchivePresence::Unreachable => Some(HealthCategory::CachedOnly),
        ArchivePresence::Confirmed => None,
    });

    let category = category.or_else(|| {
        input
            .platform
            .is_none()
            .then_some(HealthCategory::UnknownPlatform)
    });

    let category = category?;
    let retryable = category.is_retryable();
    let recovery_action = match category {
        HealthCategory::RetryableFailure => Some(RecoveryAction::RetryMount),
        HealthCategory::RecoveryAvailable => match input.recovery_offer {
            Some(RecoveryOffer::Remount) => Some(RecoveryAction::Remount),
            Some(RecoveryOffer::LazyUnmount) => Some(RecoveryAction::LazyUnmount),
            None => None,
        },
        HealthCategory::HistoricalMountFailure
        | HealthCategory::MountNotRequired
        | HealthCategory::MountFailureEvidenceInsufficient
        | HealthCategory::TerminalFailure
        | HealthCategory::Missing
        | HealthCategory::AwaitingValidation
        | HealthCategory::CachedOnly
        | HealthCategory::UnknownPlatform => None,
    };
    let reason = health_issue_reason(category, input.recovery_offer);

    Some(HealthIssue {
        path: input.path.to_path_buf(),
        platform: input.platform.map(str::to_string),
        present: !matches!(input.presence, ArchivePresence::Missing),
        mount_state: input.mount_state,
        category,
        reason,
        retryable,
        recovery_action,
        last_seen_at: input.last_seen_at.map(str::to_string),
        size_bytes: input.size_bytes,
        modified_time_unix_seconds: input.modified_time_unix_seconds,
    })
}

/// A catalogue-only health report - see `catalogue_health_report`. Built
/// without any live scan, so a caller with no live session (the CLI) can
/// still truthfully report `Missing` and `UnknownPlatform` (both facts the
/// persisted catalogue alone already knows); `AwaitingValidation`/
/// `CachedOnly` never appear here since asserting either requires a live
/// session this report deliberately never has (see `ArchivePresence`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogueHealthReport {
    pub archives_checked: usize,
    pub missing_count: usize,
    pub unknown_platform_count: usize,
    pub issues: Vec<HealthIssue>,
}

/// Builds a [`CatalogueHealthReport`] purely from already-loaded catalogue
/// rows - no filesystem or live-scan access, mirroring
/// `catalogue_filename_duplicates`'s existing read-only contract. Safe for
/// a CLI command that must never trigger a scan, mount, unmount, or
/// write. Deterministically sorted by severity, then exact path.
pub fn catalogue_health_report(archives: &[PersistedArchive]) -> CatalogueHealthReport {
    let mut issues: Vec<HealthIssue> = archives
        .iter()
        .filter_map(|archive| {
            let presence = if archive.last_verified_missing_at.is_some() {
                ArchivePresence::Missing
            } else {
                ArchivePresence::Confirmed
            };
            let input = ArchiveHealthInput {
                path: &archive.absolute_path,
                platform: archive.platform.as_deref(),
                presence,
                mount_state: None,
                archive_health: None,
                recovery_offer: None,
                last_seen_at: Some(&archive.last_seen_at),
                size_bytes: archive.size_bytes,
                modified_time_unix_seconds: archive.modified_time_unix_seconds,
            };
            classify_archive_health(&input)
        })
        .collect();
    issues.sort_by(|left, right| {
        left.category
            .severity_rank()
            .cmp(&right.category.severity_rank())
            .then_with(|| left.path.cmp(&right.path))
    });
    let missing_count = issues
        .iter()
        .filter(|issue| issue.category == HealthCategory::Missing)
        .count();
    let unknown_platform_count = issues
        .iter()
        .filter(|issue| issue.category == HealthCategory::UnknownPlatform)
        .count();
    CatalogueHealthReport {
        archives_checked: archives.len(),
        missing_count,
        unknown_platform_count,
        issues,
    }
}

/// One source folder's own health problem - kept entirely separate from
/// per-archive [`HealthIssue`]s so an offline source produces exactly one
/// entry here, never one `HealthIssue` per archive it owns (the
/// multi-source milestone's explicit "do not flood Health with one issue
/// per archive for an offline source" safety requirement).
/// `classify_source_availability` already applies `Disabled` precedence
/// over any scan-derived state, and this only ever looks at the other four
/// variants, so a merely-disabled source (ordinary, user-directed
/// configuration state, not a problem) never appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceHealthIssue {
    pub path: PathBuf,
    pub availability: SourceAvailability,
    pub reason: String,
    pub last_scan_error: Option<String>,
    /// The catalogue row count this source last successfully scanned -
    /// never recomputed here and never affected by the source currently
    /// being offline (see `scan_and_persist_folders`'s per-folder
    /// isolation, which never touches another folder's archives). Purely
    /// informational: "these rows were preserved, not lost."
    pub archives_preserved: i64,
}

fn source_health_reason(availability: SourceAvailability) -> &'static str {
    match availability {
        SourceAvailability::Unavailable => {
            "Source unavailable. Existing catalogue entries were preserved."
        }
        SourceAvailability::PermissionDenied => {
            "Permission denied reading this source. Existing catalogue entries were preserved."
        }
        SourceAvailability::ScanFailed => {
            "The last scan of this source failed. Existing catalogue entries were preserved."
        }
        SourceAvailability::Available | SourceAvailability::Disabled => "",
    }
}

/// Builds one health issue per *enabled* source folder that is not
/// currently `Available` - never one per archive that source owns. Pure
/// and database-free, exactly like [`catalogue_health_report`]: takes the
/// already-built [`SourceFolderView`]s (see [`build_source_folder_views`])
/// and only reads already-known fields, so the CLI and the GUI both ever
/// compute this identically.
pub fn source_health_issues(views: &[SourceFolderView]) -> Vec<SourceHealthIssue> {
    views
        .iter()
        .filter(|view| {
            matches!(
                view.availability,
                SourceAvailability::Unavailable
                    | SourceAvailability::PermissionDenied
                    | SourceAvailability::ScanFailed
            )
        })
        .map(|view| SourceHealthIssue {
            path: view.path.clone(),
            availability: view.availability,
            reason: source_health_reason(view.availability).to_string(),
            last_scan_error: view.last_scan_error.clone(),
            archives_preserved: view.last_archive_count.unwrap_or(0),
        })
        .collect()
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

    fn active_mount_paths(&self, root: &Path) -> Result<HashSet<PathBuf>> {
        mounted_paths_under(root)
    }

    fn verify_mounted(&self, _mount_path: &Path) -> Result<()> {
        Ok(())
    }

    fn verify_unmounted(&self, _mount_path: &Path) -> Result<()> {
        Ok(())
    }
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

    fn verify_mounted(&self, mount_path: &Path) -> Result<()> {
        if current_mount_paths()?.contains(mount_path) {
            Ok(())
        } else {
            Err(ArchiveFsError::Mount(format!(
                "{} is not mounted after the mount command reported success",
                mount_path.display()
            )))
        }
    }

    fn verify_unmounted(&self, mount_path: &Path) -> Result<()> {
        ensure_mount_disappeared(mount_path, &current_mount_paths()?)
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

/// The maximum number of individual [`SkippedFile`] entries
/// [`ArchiveScanDiscovery::skipped_files`] retains, regardless of how many
/// files a scan actually skips. The aggregate counters
/// (`skipped_unsupported_extension`/`skipped_ambiguous_platform`) always
/// reflect the *entire* scan; `skipped_files` is a bounded, best-effort
/// sample of that total for GUI drill-down, never a second source of truth
/// for the count - see [`ArchiveScanDiscovery::skipped_files_truncated`].
pub const MAX_RETAINED_SKIPPED_FILES: usize = 1000;

/// Why one file was skipped during a scan - see
/// [`ArchiveScanner::scan_source`]'s two skip branches, which are the only
/// two ways a file is ever skipped today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// No extension the media registry recognises at all -
    /// `media_registry::is_recognized_extension` returned `false` and the
    /// extension is not one of the Mega Drive corroboration candidates
    /// either.
    UnsupportedExtension,
    /// The extension is shared with `MegaDriveRom` (`.md`/`.bin`/`.gen`) but
    /// no folder, source-root, or cartridge-header evidence corroborated it,
    /// see `archive_kind_in_root`. A genuinely different, unrelated file
    /// that merely shares one of these three extensions (for example a
    /// ScummVM `RESOURCE.GEN`) is reported this way, not as unsupported.
    AmbiguousPlatform,
}

impl SkipReason {
    /// A short, human-readable label - what a GUI shows for this reason.
    pub fn label(self) -> &'static str {
        match self {
            Self::UnsupportedExtension => "Unsupported extension",
            Self::AmbiguousPlatform => "Ambiguous platform",
        }
    }
}

/// One skipped file's identity and reason - part of the bounded,
/// best-effort sample [`ArchiveScanDiscovery::skipped_files`] retains for
/// GUI drill-down. Never itself a classification decision: recording (or
/// failing to record, once the bound is reached) a file here changes
/// nothing about whether it was skipped or why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveScanDiscovery {
    pub archives: Vec<Archive>,
    pub skipped_unsupported_extension: usize,
    pub skipped_ambiguous_platform: usize,
    /// A bounded (see [`MAX_RETAINED_SKIPPED_FILES`]) sample of individual
    /// skipped files, for GUI drill-down. The exact totals always live in
    /// `skipped_unsupported_extension`/`skipped_ambiguous_platform` - this
    /// list is explanatory detail on top of those counts, never a
    /// replacement for them; call [`Self::skipped_files_truncated`] before
    /// presenting it as complete.
    pub skipped_files: Vec<SkippedFile>,
}

impl ArchiveScanDiscovery {
    /// The exact total skipped count across both reasons - always accurate,
    /// regardless of how much detail `skipped_files` retains.
    pub fn total_skipped(&self) -> usize {
        self.skipped_unsupported_extension + self.skipped_ambiguous_platform
    }

    /// Whether `skipped_files` is a truncated sample of the full skip
    /// count - true whenever more files were skipped in total than
    /// [`MAX_RETAINED_SKIPPED_FILES`] retains detail for.
    pub fn skipped_files_truncated(&self) -> bool {
        self.total_skipped() > self.skipped_files.len()
    }
}

impl<'a> ArchiveScanner<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    pub fn scan_archives(&self) -> Result<Vec<Archive>> {
        Ok(self.scan_archives_with_summary()?.archives)
    }

    pub fn scan_archives_with_summary(&self) -> Result<ArchiveScanDiscovery> {
        info!(
            "starting archive scan across {} source folder(s)",
            self.config.source_folders.len()
        );
        validate_configured_source_roots(&self.config.source_folders)?;
        let mut discovery = ArchiveScanDiscovery::default();
        for source in &self.config.source_folders {
            debug!("scanning source folder {}", source.display());
            self.scan_source(source, source, &mut discovery)?;
        }
        discovery
            .archives
            .sort_by(|left, right| left.path.cmp(&right.path));
        discovery
            .archives
            .dedup_by(|left, right| left.path == right.path);
        info!(
            "archive scan complete: {} archive(s) found",
            discovery.archives.len()
        );
        Ok(discovery)
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
        discovery: &mut ArchiveScanDiscovery,
    ) -> Result<()> {
        const MAX_SCAN_ENTRIES: usize = 250_000;
        const MAX_SCAN_DEPTH: usize = 128;

        let source_identity = validate_source_root(source_root)?;
        let mut directories = vec![(source.to_path_buf(), 0_usize)];
        let mut entries_seen = 0_usize;
        while let Some((directory, depth)) = directories.pop() {
            let before = fs::symlink_metadata(&directory)
                .map_err(|error| ArchiveFsError::io(directory.clone(), error))?;
            if before.file_type().is_symlink() || !before.is_dir() {
                return Err(ArchiveFsError::Scanner(format!(
                    "source directory changed or became unsafe during scan: {}",
                    directory.display()
                )));
            }
            let read_dir = fs::read_dir(&directory)
                .map_err(|error| ArchiveFsError::io(directory.clone(), error))?;
            let mut entries = Vec::new();
            for entry in read_dir {
                entries_seen = entries_seen.checked_add(1).ok_or_else(|| {
                    ArchiveFsError::Scanner("source entry count overflow".to_string())
                })?;
                if entries_seen > MAX_SCAN_ENTRIES {
                    return Err(ArchiveFsError::Scanner(format!(
                        "source scan exceeded the {MAX_SCAN_ENTRIES} entry limit at {}",
                        directory.display()
                    )));
                }
                entries.push(entry.map_err(|error| ArchiveFsError::io(directory.clone(), error))?);
            }
            entries.sort_by_key(|entry| entry.path());

            let mut child_directories = Vec::new();
            for entry in entries {
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|error| ArchiveFsError::io(path.clone(), error))?;
                if file_type.is_symlink() {
                    continue;
                }
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
                    if depth >= MAX_SCAN_DEPTH {
                        return Err(ArchiveFsError::Scanner(format!(
                            "source scan exceeded the {MAX_SCAN_DEPTH} directory depth limit at {}",
                            path.display()
                        )));
                    }
                    child_directories.push((path, depth + 1));
                } else if file_type.is_file()
                    && let Some(archive) = Archive::from_path_in_root(&path, source_root)
                {
                    if archive.identity.size_bytes.is_none() {
                        return Err(ArchiveFsError::Scanner(format!(
                            "archive changed or became unreadable during scan: {}",
                            path.display()
                        )));
                    }
                    debug!("discovered archive {}", archive.path.display());
                    discovery.archives.push(archive);
                } else if file_type.is_file() {
                    let extension = path
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(str::to_ascii_lowercase);
                    // `.gen` joins `.md` and `.bin` here: all three are shared
                    // extensions that only become a Mega Drive ROM once a folder
                    // or a cartridge header corroborates them, so one that was
                    // skipped was skipped for an ambiguous platform - not
                    // because the extension is unsupported.
                    let reason = if extension
                        .as_deref()
                        .is_some_and(|value| matches!(value, "md" | "bin" | "gen"))
                    {
                        discovery.skipped_ambiguous_platform += 1;
                        SkipReason::AmbiguousPlatform
                    } else {
                        discovery.skipped_unsupported_extension += 1;
                        SkipReason::UnsupportedExtension
                    };
                    // Bounded: the counters above always stay exact for the
                    // whole scan, but the retained detail list never grows
                    // past `MAX_RETAINED_SKIPPED_FILES` - see
                    // `ArchiveScanDiscovery::skipped_files_truncated`.
                    if discovery.skipped_files.len() < MAX_RETAINED_SKIPPED_FILES {
                        discovery.skipped_files.push(SkippedFile {
                            path: path.clone(),
                            reason,
                        });
                    }
                }
            }
            let after = fs::symlink_metadata(&directory)
                .map_err(|error| ArchiveFsError::io(directory.clone(), error))?;
            if after.file_type().is_symlink()
                || !after.is_dir()
                || filesystem_identity(&before) != filesystem_identity(&after)
            {
                return Err(ArchiveFsError::Scanner(format!(
                    "source directory changed during scan: {}",
                    directory.display()
                )));
            }
            child_directories.reverse();
            directories.extend(child_directories);
        }
        if validate_source_root(source_root)? != source_identity {
            return Err(ArchiveFsError::Scanner(format!(
                "source root changed during scan: {}",
                source_root.display()
            )));
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

pub(crate) fn revalidate_archive_for_catalogue(archive: &Archive) -> Result<()> {
    let source_identity = validate_source_root(&archive.identity.source_root)?;
    if archive.identity.source_filesystem_device != Some(source_identity.device)
        || archive.identity.source_filesystem_inode != Some(source_identity.inode)
    {
        return Err(ArchiveFsError::Scanner(format!(
            "source root changed after scan: {}",
            archive.identity.source_root.display()
        )));
    }
    if !archive.path.starts_with(&archive.identity.source_root) {
        return Err(ArchiveFsError::Scanner(format!(
            "archive escaped source root: {}",
            archive.path.display()
        )));
    }
    let relative = archive
        .path
        .strip_prefix(&archive.identity.source_root)
        .map_err(|_| ArchiveFsError::Scanner("archive source binding changed".to_string()))?;
    let mut current = archive.identity.source_root.clone();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| ArchiveFsError::io(current.clone(), error))?;
        if metadata.file_type().is_symlink() {
            return Err(ArchiveFsError::Scanner(format!(
                "archive path contains a symlink after scan: {}",
                current.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(&archive.path)
        .map_err(|error| ArchiveFsError::io(archive.path.clone(), error))?;
    let identity = filesystem_identity(&metadata);
    if !metadata.is_file()
        || archive.identity.filesystem_device != Some(identity.device)
        || archive.identity.filesystem_inode != Some(identity.inode)
        || archive.identity.size_bytes != Some(metadata.len())
        || archive.identity.modified_time != metadata.modified().ok()
    {
        return Err(ArchiveFsError::Scanner(format!(
            "archive changed after scan: {}",
            archive.path.display()
        )));
    }
    Ok(())
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
    /// A bounded, read-only format/header check identified the platform.
    HeaderIdentity,
    /// Filename/title/known-path-segment evidence, used only after exact
    /// folder-alias matching finds nothing.
    Heuristic,
    /// An exact folder-alias match in the `platform` registry.
    FolderAlias,
}

impl PlatformProvenance {
    /// The exact string persisted as `platform_assignments.source`.
    /// `"heuristic-path-detector"` is unchanged from every row written
    /// before this enum existed; `"folder_alias"` is new.
    pub fn as_source_str(self) -> &'static str {
        match self {
            Self::HeaderIdentity => "header_identity",
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

/// A platform detection plus the human-meaningful folder component that
/// supplied a built-in alias match. Heuristic detections intentionally have
/// no matched folder: their evidence may come from a filename, title, or path
/// segment and should not be presented as a folder-alias match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailedPlatformDetection {
    pub platform: String,
    pub provenance: PlatformProvenance,
    pub matched_folder: Option<String>,
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
/// 1. The folder-alias table in the `platform` registry, walking
///    from the archive's nearest containing directory up to (never beyond)
///    `source_root` - see `detect_platform_from_folder_alias`.
/// 2. The filename/title/known-path-segment heuristic.
/// 3. `None` if neither found a confident match.
pub fn detect_platform_with_provenance(
    path: impl AsRef<Path>,
    source_root: impl AsRef<Path>,
) -> Option<PlatformDetection> {
    detect_platform_with_details(path, source_root).map(|detection| PlatformDetection {
        platform: detection.platform,
        provenance: detection.provenance,
    })
}

/// Detects a platform with enough detail for a human-readable provenance
/// explanation while retaining the matched folder text.
pub fn detect_platform_with_details(
    path: impl AsRef<Path>,
    source_root: impl AsRef<Path>,
) -> Option<DetailedPlatformDetection> {
    let path = path.as_ref();
    let source_root = source_root.as_ref();

    if let Some((platform, matched_folder)) =
        detect_platform_from_folder_alias_with_match(path, source_root)
    {
        return Some(DetailedPlatformDetection {
            platform: platform.to_string(),
            provenance: PlatformProvenance::FolderAlias,
            matched_folder: Some(matched_folder),
        });
    }

    detect_platform_from_known_heuristics(path, source_root).map(|platform| {
        DetailedPlatformDetection {
            platform,
            provenance: PlatformProvenance::Heuristic,
            matched_folder: None,
        }
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

// The folder-alias table that used to live here now lives in
// `crate::platform::PLATFORMS`, which is the single authoritative registry for
// every platform, alias, extension and signature this build knows. The
// functions below are thin adapters onto it, kept so that every existing
// caller's behaviour and signature are unchanged.

/// Non-hardware "which emulator core" evidence for platforms whose folder
/// name commonly identifies a specific emulator/core rather than the hardware
/// itself (Arcade via FBNeo/MAME/FBA being the primary case). Kept separate
/// from the platform registry deliberately: the registry's job is exactly one
/// canonical *hardware* platform per folder name, never an emulator choice, so
/// FBNeo is never promoted to its own canonical platform - the hardware label
/// "Arcade" is always what such a folder alias resolves to.
const FOLDER_PREFERRED_EMULATOR_ALIASES: &[(&str, &str)] = &[
    ("fbneo", "FBNeo"),
    ("finalburnneo", "FBNeo"),
    ("fba", "FBNeo"),
    ("mame", "MAME"),
];

/// The emulator/core a normalized folder name suggests, when the folder name
/// itself names a specific emulator rather than only hardware. Returns `None`
/// when the folder name carries no such evidence - most platforms have no
/// preferred-emulator folder convention at all, which is not an error.
#[must_use]
pub fn platform_preferred_emulator_for_alias(folder_hint: &str) -> Option<&'static str> {
    let normalized = normalize_path_segment(folder_hint);
    FOLDER_PREFERRED_EMULATOR_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, emulator)| *emulator)
}

/// Every canonical platform name this build recognises, deduplicated and
/// sorted. Delegates to [`crate::platform::canonical_ids`], so manual platform
/// assignment (`Database::set_manual_platform`) and its CLI/GUI callers all
/// draw from the one registry and cannot drift apart.
pub fn canonical_platform_names() -> Vec<&'static str> {
    platform::canonical_ids()
}

/// Resolves one external platform hint through the registry's normalized
/// folder-alias matching. The original hint is never changed; callers use the
/// returned canonical name only for comparison. An alias claimed by more than
/// one platform is treated as ambiguous and returns `None` rather than picking
/// a winner - see [`crate::platform::platform_for_alias`].
pub fn canonical_platform_for_alias(platform_hint: &str) -> Option<&'static str> {
    // An exact canonical identifier resolves to itself, so a stored value round
    // trips even when it is not also spelled as one of its own aliases.
    platform::platform_by_id(platform_hint)
        .or_else(|| platform::platform_for_alias(platform_hint))
        .map(|platform| platform.id)
}

/// Canonical platform name for one already-lossy-stringified path component,
/// if it exactly matches a known folder alias after normalization, tolerating
/// the parenthesized-suffix and trailing-date conventions real collection
/// folders use. Matching remains exact - never a substring.
fn folder_platform_alias(segment: &str) -> Option<&'static str> {
    platform::detect::platform_for_folder_name(segment).map(|platform| platform.id)
}

/// Infers a platform from `path`'s folder structure alone, walking directory
/// components from the archive's nearest containing folder upward to and
/// including `source_root` - the nearest matching folder wins. Nothing outside
/// `source_root` participates, and the archive's own filename is excluded.
fn detect_platform_from_folder_alias_with_match(
    path: &Path,
    source_root: &Path,
) -> Option<(&'static str, String)> {
    let relative = path.strip_prefix(source_root).ok()?;
    let mut components: Vec<_> = relative.components().collect();
    components.pop(); // the archive's own filename never counts as a folder.

    components
        .iter()
        .rev()
        .find_map(|component| {
            let matched_folder = component.as_os_str().to_string_lossy();
            folder_platform_alias(&matched_folder)
                .map(|platform| (platform, matched_folder.into_owned()))
        })
        .or_else(|| {
            let matched_folder = source_root.file_name()?.to_string_lossy();
            folder_platform_alias(&matched_folder)
                .map(|platform| (platform, matched_folder.into_owned()))
        })
}

/// Normalizes one path segment for alias matching. Delegates to the registry
/// so there is exactly one normalization rule in the build.
fn normalize_path_segment(segment: &str) -> String {
    platform::normalize_alias(segment)
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

    // Registry-backed, so a newly recognised media format never needs a
    // second hardcoded extension list to strip its own suffix - see
    // `media_registry`.
    for format in media_registry::MEDIA_FORMATS {
        let dotted_extension = format!(".{}", format.extension);
        if lower.ends_with(&dotted_extension) {
            return filename[..filename.len() - dotted_extension.len()].to_string();
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
    app_dirs::data_path("index.json")
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

/// Publishes the index atomically: the JSON is written to a temporary file
/// beside the destination, fsynced, and only then renamed into place.
///
/// This matters because the index is a derived cache that readers parse
/// wholesale. A direct `fs::write` truncates first, so an interrupted write
/// would leave a half-written index that parses as an empty or partial
/// library. With a rename, a reader ever only sees the complete previous
/// index or the complete new one, and a failed write leaves the previous
/// index exactly as it was.
pub fn write_archive_index(index: &ArchiveIndex, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
    }
    let temporary = path.with_extension(format!("partial-{}", std::process::id()));
    let json = archive_index_to_json(index);
    let write = (|| -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if let Err(source) = write {
        let _ = fs::remove_file(&temporary);
        return Err(ArchiveFsError::io(path.to_path_buf(), source));
    }
    Ok(())
}

pub fn build_and_write_archive_index(config: &Config) -> Result<ArchiveIndex> {
    build_and_write_archive_index_to(config, &default_index_path()?)
}

/// Same as [`build_and_write_archive_index`], with the destination supplied
/// rather than derived from `$HOME`. Exists so callers - including tests -
/// can rebuild a specific index without depending on the invoking user's
/// home directory.
///
/// The index is built completely *before* anything is written, so a scan
/// failure leaves the existing index untouched, and publication itself is
/// atomic (see [`write_archive_index`]).
pub fn build_and_write_archive_index_to(
    config: &Config,
    index_path: &Path,
) -> Result<ArchiveIndex> {
    info!("starting index rebuild");
    debug!("index path {}", index_path.display());
    let index = build_archive_index(config)?;
    write_archive_index(&index, index_path)?;
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

    // Delegates to the same media registry `archive_kind` consults, so
    // scanning and watching can never independently drift out of sync
    // again - see `media_registry`.
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    media_registry::is_watch_relevant_extension(&extension.to_ascii_lowercase())
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
            MountState::MountPathExists | MountState::NotMountable => {}
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
            MountState::MountPathExists | MountState::NotMountable => {}
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
    prepare_mount_root(&config.mount_root)?;

    let archive_paths = plans
        .iter()
        .map(|plan| plan.archive.path.clone())
        .collect::<Vec<_>>();
    let active_mount_paths = backend.active_mount_paths(&config.mount_root)?;
    let validations =
        validate_mount_batch_targets(config, &plans, &archive_paths, &active_mount_paths);
    for validation in validations {
        match validation {
            MountBatchTargetValidation::Ready { archive_path, .. } => {
                let plan = select_mount_plan_by_path(&plans, &archive_path)?;
                let active_mount_paths = backend.active_mount_paths(&config.mount_root)?;
                mount_one_plan_outcome_with_active_mounts(
                    config,
                    plan,
                    backend,
                    &active_mount_paths,
                )?;
            }
            MountBatchTargetValidation::Skipped {
                reason: MountBatchTargetSkipReason::AlreadyMountedResolvedTarget { .. },
                ..
            } => {}
            MountBatchTargetValidation::Skipped { reason, .. } => {
                return Err(ArchiveFsError::Mount(format!(
                    "refusing unsafe batch target: {reason}"
                )));
            }
        }
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

    prepare_mount_root(&config.mount_root)?;
    let active_mount_paths = backend.active_mount_paths(&config.mount_root)?;
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
    prepare_mount_root(&config.mount_root)?;
    validate_archive_for_mount(&plan.archive)?;
    validate_mount_target_parent(config, &plan.mount_path)?;
    refuse_unsafe_existing_mount_target(&plan.mount_path)?;
    info!("mounting {}", plan.archive.path.display());
    fs::create_dir_all(&plan.mount_path)
        .map_err(|source| ArchiveFsError::io(plan.mount_path.clone(), source))?;
    ensure_no_symlink_components(&plan.mount_path)?;
    if !path_resolves_below(&plan.mount_path, &config.mount_root)? {
        return Err(ArchiveFsError::Config(format!(
            "refusing to mount outside mount root: {}",
            plan.mount_path.display()
        )));
    }
    let target_identity = fs::symlink_metadata(&plan.mount_path)
        .map_err(|source| ArchiveFsError::io(plan.mount_path.clone(), source))?;
    validate_archive_for_mount(&plan.archive)?;
    refuse_unsafe_existing_mount_target(&plan.mount_path)?;
    let revalidated_target = fs::symlink_metadata(&plan.mount_path)
        .map_err(|source| ArchiveFsError::io(plan.mount_path.clone(), source))?;
    if !same_file_identity(&target_identity, &revalidated_target) {
        return Err(ArchiveFsError::Mount(format!(
            "mount target changed during validation: {}",
            plan.mount_path.display()
        )));
    }
    backend.mount(&plan)?;
    backend.verify_mounted(&plan.mount_path)?;
    info!(
        "mounted {} at {}",
        plan.archive.path.display(),
        plan.mount_path.display()
    );
    Ok(plan)
}

fn prepare_mount_root(mount_root: &Path) -> Result<()> {
    if !mount_root.is_absolute() || mount_root.parent().is_none() {
        return Err(ArchiveFsError::Config(format!(
            "mount root must be an absolute non-root path: {}",
            mount_root.display()
        )));
    }
    ensure_no_symlink_components(mount_root)?;
    fs::create_dir_all(mount_root)
        .map_err(|source| ArchiveFsError::io(mount_root.to_path_buf(), source))?;
    ensure_no_symlink_components(mount_root)
}

fn ensure_no_symlink_components(path: &Path) -> Result<()> {
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(ArchiveFsError::Config(format!(
            "refusing path traversal component in {}",
            path.display()
        )));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ArchiveFsError::Config(format!(
                    "refusing unsafe symlink component {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => break,
            Err(source) => return Err(ArchiveFsError::io(current, source)),
        }
    }
    Ok(())
}

fn validate_archive_for_mount(archive: &Archive) -> Result<()> {
    if archive.kind == ArchiveKind::MegaDriveRom {
        return Err(ArchiveFsError::Mount(format!(
            "loose Mega Drive ROMs are catalogued for library discovery but are not archive mount inputs: {}",
            archive.path.display()
        )));
    }
    ensure_no_symlink_components(&archive.path)?;
    let metadata = fs::symlink_metadata(&archive.path)
        .map_err(|source| ArchiveFsError::io(archive.path.clone(), source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ArchiveFsError::Mount(format!(
            "refusing archive that is not a regular file: {}",
            archive.path.display()
        )));
    }
    if archive
        .identity
        .size_bytes
        .is_some_and(|expected| expected != metadata.len())
        || archive.identity.modified_time.is_some_and(|expected| {
            metadata
                .modified()
                .map_or(true, |observed| observed != expected)
        })
    {
        return Err(ArchiveFsError::Mount(format!(
            "archive changed after planning: {}",
            archive.path.display()
        )));
    }
    Ok(())
}

fn refuse_unsafe_existing_mount_target(mount_path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(mount_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(ArchiveFsError::io(mount_path.to_path_buf(), source)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArchiveFsError::Mount(format!(
            "refusing unsafe existing mount target: {}",
            mount_path.display()
        )));
    }
    if !directory_is_empty(mount_path)? {
        return Err(ArchiveFsError::Mount(format!(
            "refusing to mount over nonempty directory: {}",
            mount_path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type() == right.file_type()
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
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
    let mut plans = ArchiveScanner::new(config).mount_plans()?;
    plans.sort_by(|left, right| right.mount_path.cmp(&left.mount_path));
    for plan in plans {
        let active_mount_paths = backend.active_mount_paths(&config.mount_root)?;
        if active_mount_paths.contains(&plan.mount_path) {
            refuse_nested_active_mounts(&plan.mount_path, &active_mount_paths)?;
            unmount_one_plan_outcome_with_active_mounts(
                config,
                plan,
                backend,
                &active_mount_paths,
            )?;
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
    let parent_identity = validated_unmount_parent_identity(config, &mount_path, "lazy-unmount")?;
    refuse_nested_active_mounts(&mount_path, &mounted_paths)?;
    let revalidated_parent =
        validated_unmount_parent_identity(config, &mount_path, "lazy-unmount")?;
    if !same_file_identity(&parent_identity, &revalidated_parent) {
        return Err(ArchiveFsError::Unmount(format!(
            "refusing lazy-unmount because the mount parent changed: {}",
            mount_path.display()
        )));
    }

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

    let active_mount_paths = backend.active_mount_paths(&config.mount_root)?;
    match unmount_one_plan_outcome_with_active_mounts(config, plan, backend, &active_mount_paths)? {
        UnmountOneOutcome::Unmounted(plan) => Ok(plan),
        UnmountOneOutcome::NotMounted(plan) => Err(ArchiveFsError::Unmount(format!(
            "{} is not currently mounted",
            plan.mount_path.display()
        ))),
    }
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
    let parent_identity = validated_unmount_parent_identity(config, &plan.mount_path, "unmount")?;
    refuse_nested_active_mounts(&plan.mount_path, active_mount_paths)?;
    let revalidated_parent =
        validated_unmount_parent_identity(config, &plan.mount_path, "unmount")?;
    if !same_file_identity(&parent_identity, &revalidated_parent) {
        return Err(ArchiveFsError::Unmount(format!(
            "refusing unmount because the mount parent changed: {}",
            plan.mount_path.display()
        )));
    }
    info!("unmounting {}", plan.mount_path.display());
    backend.unmount(&plan.mount_path)?;
    backend.verify_unmounted(&plan.mount_path)?;
    info!("unmounted {}", plan.mount_path.display());
    Ok(UnmountOneOutcome::Unmounted(plan))
}

fn refuse_nested_active_mounts(
    mount_path: &Path,
    active_mount_paths: &HashSet<PathBuf>,
) -> Result<()> {
    if let Some(child) = active_mount_paths
        .iter()
        .find(|candidate| candidate.as_path() != mount_path && path_is_under(candidate, mount_path))
    {
        return Err(ArchiveFsError::Unmount(format!(
            "refusing to unmount {} while nested mount {} is active",
            mount_path.display(),
            child.display()
        )));
    }
    Ok(())
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
    validated_unmount_parent_identity(config, mount_path, operation).map(|_| ())
}

fn validated_unmount_parent_identity(
    config: &Config,
    mount_path: &Path,
    operation: &str,
) -> Result<fs::Metadata> {
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
    let metadata = fs::symlink_metadata(parent)
        .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArchiveFsError::Config(format!(
            "refusing to {operation} through unsafe parent {}",
            parent.display()
        )));
    }
    Ok(metadata)
}

pub fn cleanup_selected_mount_dir(config: &Config, mount_path: &Path) -> Result<bool> {
    ensure_no_symlink_components(&config.mount_root)?;
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
    ensure_no_symlink_components(&config.mount_root)?;
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

/// The exact conditions that must hold before an empty mount directory may
/// be removed: it exists, it is not a symlink, it is a directory, it is not
/// an active mount point, and it is empty.
///
/// Extracted so a read-only *plan* ([`plan_stale_mount_directories`]) and
/// the actual removal ([`remove_empty_unmounted_dir`]) can never disagree
/// about what is removable. Returns the observed metadata on success so the
/// remover can re-stat and compare identity before acting.
fn empty_unmounted_dir_is_removable(
    path: &Path,
    mounted_paths: &HashSet<PathBuf>,
) -> Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ArchiveFsError::io(path.to_path_buf(), source)),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || mounted_paths.contains(path)
        || !directory_is_empty(path)?
    {
        return Ok(None);
    }
    Ok(Some(metadata))
}

/// Lists the empty, unmounted directories beneath the configured mount root
/// that [`cleanup_selected_mount_tree`] would remove - **without removing
/// anything**.
///
/// Read-only by construction: it walks only EmuWiz's own mount root (not
/// the library, and never an archive), applies exactly the same predicate the
/// remover applies via [`empty_unmounted_dir_is_removable`], refuses to
/// follow symlinks, and never descends into an active mount point. The walk
/// is bounded by `MAX_MOUNT_ROOT_PLAN_DEPTH` and
/// `MAX_MOUNT_ROOT_PLAN_ENTRIES` so a pathological tree cannot make a
/// diagnostic hang.
///
/// Deterministically sorted, deepest path first, matching the order
/// `clean_mount_root` removes in.
pub fn plan_stale_mount_directories(config: &Config) -> Result<Vec<PathBuf>> {
    const MAX_MOUNT_ROOT_PLAN_DEPTH: usize = 6;
    const MAX_MOUNT_ROOT_PLAN_ENTRIES: usize = 4096;

    if !config.mount_root.is_dir() {
        return Ok(Vec::new());
    }
    ensure_no_symlink_components(&config.mount_root)?;
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    let mut stale = Vec::new();
    let mut inspected = 0usize;
    let mut queue = vec![(config.mount_root.clone(), 0usize)];
    while let Some((directory, depth)) = queue.pop() {
        if depth >= MAX_MOUNT_ROOT_PLAN_DEPTH || inspected >= MAX_MOUNT_ROOT_PLAN_ENTRIES {
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(ArchiveFsError::io(directory.clone(), source)),
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            inspected += 1;
            if inspected > MAX_MOUNT_ROOT_PLAN_ENTRIES {
                break;
            }
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            // Never follow a symlink and never enter an active mount.
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || mounted_paths.contains(&path)
            {
                continue;
            }
            if empty_unmounted_dir_is_removable(&path, &mounted_paths)?.is_some() {
                stale.push(path);
            } else {
                queue.push((path, depth + 1));
            }
        }
    }
    stale.sort_by(|left, right| right.cmp(left));
    stale.dedup();
    Ok(stale)
}

fn remove_empty_unmounted_dir(path: &Path, mounted_paths: &HashSet<PathBuf>) -> Result<bool> {
    let Some(metadata) = empty_unmounted_dir_is_removable(path, mounted_paths)? else {
        return Ok(false);
    };
    let revalidated = fs::symlink_metadata(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    if revalidated.file_type().is_symlink()
        || !revalidated.is_dir()
        || !same_file_identity(&metadata, &revalidated)
    {
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
    if !config.mount_root.exists() {
        return Ok(Vec::new());
    }
    ensure_no_symlink_components(&config.mount_root)?;
    let mut plans = ArchiveScanner::new(config).mount_plans()?;
    plans.sort_by(|left, right| right.mount_path.cmp(&left.mount_path));
    let mut removed = Vec::new();
    for plan in plans {
        for path in cleanup_selected_mount_tree(config, &plan.mount_path)? {
            if !removed.contains(&path) {
                removed.push(path);
            }
        }
    }
    Ok(removed)
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
    if !plan.archive.kind.is_mount_input() {
        MountState::NotMountable
    } else if mounted_paths.contains(&plan.mount_path) {
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
    let mountinfo = fs::read("/proc/self/mountinfo")
        .map_err(|source| ArchiveFsError::io(PathBuf::from("/proc/self/mountinfo"), source))?;
    Ok(mountinfo
        .split(|byte| *byte == b'\n')
        .filter_map(mount_path_from_mountinfo_line)
        .collect())
}

fn mounted_paths_under(root: &Path) -> Result<HashSet<PathBuf>> {
    Ok(current_mount_paths()?
        .into_iter()
        .filter(|path| path_is_under(path, root))
        .collect())
}

fn mount_path_from_mountinfo_line(line: &[u8]) -> Option<PathBuf> {
    let field = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .nth(4)?;
    mountinfo_path_from_bytes(&unescape_mountinfo_path(field))
}

/// Decodes one `/proc/self/mountinfo` path field, applying the same octal
/// unescaping the mount code itself uses.
///
/// Exposed to `crate::diagnostics` so the read-only mount-state check cannot
/// drift from how mounts are actually matched - see
/// `diagnostics::environment::parse_mount_table`.
pub(crate) fn mountinfo_path_for_diagnostics(field: &[u8]) -> Option<PathBuf> {
    mountinfo_path_from_bytes(&unescape_mountinfo_path(field))
}

fn unescape_mountinfo_path(path: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut index = 0;
    while index < path.len() {
        if path[index] == b'\\'
            && index + 3 < path.len()
            && path[index + 1..=index + 3]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = (path[index + 1] - b'0') * 64
                + (path[index + 2] - b'0') * 8
                + (path[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(path[index]);
            index += 1;
        }
    }
    decoded
}

#[cfg(unix)]
fn mountinfo_path_from_bytes(path: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
}

#[cfg(not(unix))]
fn mountinfo_path_from_bytes(path: &[u8]) -> Option<PathBuf> {
    String::from_utf8(path.to_vec()).ok().map(PathBuf::from)
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
    run_command_os_with_timeout(program, args, Duration::from_secs(30))
}

const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;

fn run_command_os_with_timeout(
    program: &str,
    args: &[&std::ffi::OsStr],
    timeout: Duration,
) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ArchiveFsError::io(PathBuf::from(program), source))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ArchiveFsError::Mount(format!("failed to capture stdout from {program}")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ArchiveFsError::Mount(format!("failed to capture stderr from {program}")))?;
    let stdout_reader = thread::spawn(move || read_bounded_output(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_output(stderr));
    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status, false),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let status = child
                    .wait()
                    .map_err(|source| ArchiveFsError::io(PathBuf::from(program), source))?;
                break (status, true);
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ArchiveFsError::io(PathBuf::from(program), source));
            }
        }
    };
    let _stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    if timed_out {
        Err(ArchiveFsError::ExternalCommand {
            program: program.to_string(),
            status: status.code(),
            stderr: format!("command timed out after {} ms", timeout.as_millis()),
        })
    } else if status.success() {
        Ok(())
    } else {
        Err(ArchiveFsError::ExternalCommand {
            program: program.to_string(),
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        })
    }
}

fn read_bounded_output(mut reader: impl Read) -> Vec<u8> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let remaining = COMMAND_OUTPUT_LIMIT.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    retained
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
    fn mega_drive_rom_extensions_use_conservative_folder_context() {
        let root = test_root("megadrive-rom-context");
        for folder in [
            "megadrive",
            "Mega-Drive",
            "mega_drive",
            "genesis",
            "sega-megadrive",
            "sega-genesis",
        ] {
            let directory = root.join(folder);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("Alien 3.MD"), b"rom").unwrap();
            fs::write(directory.join("Game.BIN"), b"rom").unwrap();
        }
        fs::write(root.join("README.md"), b"markdown").unwrap();
        let misleading = root.join("genesis-project");
        fs::create_dir_all(&misleading).unwrap();
        fs::write(misleading.join("notes.md"), b"markdown").unwrap();
        fs::write(root.join("specific.GEN"), b"rom").unwrap();
        fs::write(root.join("specific.sMd"), b"rom").unwrap();

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();
        // 13, not 14: `specific.GEN` sits at the source root, where no folder
        // says Mega Drive, and its contents carry no cartridge header. Before
        // this milestone a bare `.gen` was accepted on its extension alone,
        // which is what labelled ScummVM `RESOURCE.GEN` resource files as Mega
        // Drive ROMs. `specific.sMd` is still accepted, because `.smd` really
        // is Mega Drive specific and needs no corroboration.
        assert_eq!(discovery.archives.len(), 13);
        assert!(
            !discovery
                .archives
                .iter()
                .any(|archive| archive.path.ends_with("specific.GEN")),
            "an uncorroborated `.gen` must not be taken for a Mega Drive ROM"
        );
        assert!(
            discovery
                .archives
                .iter()
                .any(|archive| archive.path.ends_with("specific.sMd")),
            "`.smd` is Mega Drive specific and must still be recognised"
        );
        assert!(discovery.archives.iter().all(|archive| {
            archive.kind == ArchiveKind::MegaDriveRom
                && archive.identity.platform.as_deref() == Some("MegaDrive")
        }));
        assert!(!discovery.archives.iter().any(|archive| {
            archive.path.ends_with("README.md") || archive.path.ends_with("notes.md")
        }));
        // 3: the two `.md` markdown files, plus the uncorroborated `.GEN`.
        // All three are shared extensions whose platform could not be
        // established, which is different from an unsupported extension.
        assert_eq!(discovery.skipped_ambiguous_platform, 3);
        assert_eq!(discovery.skipped_unsupported_extension, 0);
        let mount_error = validate_archive_for_mount(&discovery.archives[0]).unwrap_err();
        assert!(mount_error.to_string().contains("not archive mount inputs"));
        let loose_plan = MountPlan::new(
            discovery.archives[0].clone(),
            root.join("mount").join("Alien_3"),
        );
        assert_eq!(
            mount_state_for_plan(&loose_plan, &HashSet::new()),
            MountState::NotMountable,
            "loose ROMs must be rejected by shared action eligibility before mounting"
        );
        assert_eq!(
            archive_kind_in_root(
                &root.join("megadrive").join("Alien 3.MD"),
                &root.join("megadrive")
            ),
            Some(ArchiveKind::MegaDriveRom),
            "an exact platform alias may itself be the configured source root"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn direct_rvz_iso_and_zip_are_discovered_without_wrappers() {
        let root = test_root("direct-game-images").join("gcn");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("ZooCube (USA).rvz"), b"rvz fixture").unwrap();
        fs::write(root.join("Disc.iso"), vec![0_u8; 64]).unwrap();
        fs::write(root.join("Animal Crossing (USA).zip"), b"zip fixture").unwrap();
        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };

        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();
        assert_eq!(discovery.archives.len(), 3);
        assert_eq!(
            discovery
                .archives
                .iter()
                .filter(|archive| archive.kind == ArchiveKind::DirectGameImage)
                .count(),
            2
        );
        assert!(
            discovery
                .archives
                .iter()
                .all(|archive| { archive.identity.platform.as_deref() == Some("GameCube") })
        );
        assert!(discovery.archives.iter().any(|archive| {
            archive.path.ends_with("ZooCube (USA).rvz")
                && archive.identity.platform_provenance == Some(PlatformProvenance::FolderAlias)
        }));
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// Test 1/2/3 (loose Commodore media milestone): `.d64` and `.g64`
    /// under a `c128`-aliased folder are discovered as
    /// `ArchiveKind::DirectGameImage`, assigned `Commodore 128` by the same
    /// folder-alias heuristic every other platform already uses, while
    /// `images/`, `videos/`, and `gamelist.xml` - none of them a
    /// recognised extension - are never discovered as candidates at all.
    #[test]
    fn loose_c128_d64_and_g64_are_discovered_but_companion_assets_are_not() {
        let root = test_root("loose-c128-media").join("c128");
        fs::create_dir_all(root.join("images")).unwrap();
        fs::create_dir_all(root.join("videos")).unwrap();
        fs::write(root.join("BurgerWhop!.d64"), vec![0_u8; 174_848]).unwrap();
        fs::write(
            root.join("Ultima V - Warriors of Destiny(Disk 1 of 4 Side A)(Main).g64"),
            vec![0_u8; 278_234],
        )
        .unwrap();
        fs::write(root.join("gamelist.xml"), b"<gameList/>").unwrap();
        fs::write(
            root.join("images").join("BurgerWhop-boxart.png"),
            b"png fixture",
        )
        .unwrap();
        fs::write(
            root.join("videos").join("BurgerWhop-video.mp4"),
            b"mp4 fixture",
        )
        .unwrap();

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(
            discovery.archives.len(),
            2,
            "only the two recognised loose disk images may be discovered - got {:?}",
            discovery
                .archives
                .iter()
                .map(|archive| archive.path.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            discovery
                .archives
                .iter()
                .all(|archive| archive.kind == ArchiveKind::DirectGameImage),
            "loose Commodore media is catalogued directly, never wrapped or copied"
        );
        assert!(
            discovery
                .archives
                .iter()
                .all(|archive| archive.identity.platform.as_deref() == Some("Commodore 128")),
            "the `c128` folder alias must resolve both `.d64` and `.g64` to Commodore 128, \
             exactly like every other weak/shared extension already does"
        );
        assert!(
            discovery
                .archives
                .iter()
                .any(|archive| archive.path.ends_with("BurgerWhop!.d64"))
        );
        assert!(discovery.archives.iter().any(|archive| {
            archive
                .path
                .ends_with("Ultima V - Warriors of Destiny(Disk 1 of 4 Side A)(Main).g64")
        }));
        assert!(
            discovery.archives.iter().all(|archive| !archive
                .path
                .to_string_lossy()
                .contains("images")
                && !archive.path.to_string_lossy().contains("videos")
                && !archive.path.ends_with("gamelist.xml")),
            "images/, videos/, and gamelist.xml must never become candidates"
        );
        assert_eq!(
            discovery.skipped_unsupported_extension, 3,
            "gamelist.xml, the .png, and the .mp4 are each an unsupported extension - counted, \
             never silently dropped"
        );
        assert_eq!(discovery.skipped_ambiguous_platform, 0);
        // The same three files must also appear as individual skipped-file
        // detail, each with the unsupported-extension reason and its own
        // path - see the "Skipped files / why-skipped UI" milestone.
        assert_eq!(discovery.skipped_files.len(), 3);
        assert!(
            discovery
                .skipped_files
                .iter()
                .all(|item| item.reason == SkipReason::UnsupportedExtension)
        );
        assert!(
            discovery
                .skipped_files
                .iter()
                .any(|item| item.path.ends_with("gamelist.xml"))
        );
        assert!(!discovery.skipped_files_truncated());

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    // -------------------------------------------------------------------
    // Skipped-files detail (`SkippedFile`/`SkipReason`) - the "Skipped
    // files / why-skipped UI" milestone.
    // -------------------------------------------------------------------

    /// Test: an unsupported-extension file produces a `SkippedFile` detail
    /// entry with the exact right reason and path.
    #[test]
    fn unsupported_extension_produces_a_skipped_file_detail_entry() {
        let root = test_root("skipped-detail-unsupported");
        fs::create_dir_all(&root).unwrap();
        let boxart = root.join("boxart.png");
        fs::write(&boxart, b"not a rom").unwrap();

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(discovery.skipped_unsupported_extension, 1);
        assert_eq!(discovery.skipped_files.len(), 1);
        assert_eq!(discovery.skipped_files[0].path, boxart);
        assert_eq!(
            discovery.skipped_files[0].reason,
            SkipReason::UnsupportedExtension
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Test: an uncorroborated `.gen` (shared with Mega Drive, but with no
    /// folder/source/header evidence here) produces a `SkippedFile` detail
    /// entry with the ambiguous-platform reason, not unsupported-extension.
    #[test]
    fn ambiguous_platform_produces_a_skipped_file_detail_entry() {
        let root = test_root("skipped-detail-ambiguous");
        fs::create_dir_all(&root).unwrap();
        let resource = root.join("RESOURCE.GEN");
        fs::write(&resource, b"scummvm resource, not a rom").unwrap();

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(discovery.skipped_ambiguous_platform, 1);
        assert_eq!(discovery.skipped_unsupported_extension, 0);
        assert_eq!(discovery.skipped_files.len(), 1);
        assert_eq!(discovery.skipped_files[0].path, resource);
        assert_eq!(
            discovery.skipped_files[0].reason,
            SkipReason::AmbiguousPlatform
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Test: a recognised archive produces no skipped-file detail at all -
    /// explaining skips must never mention something that was not, in
    /// fact, skipped.
    #[test]
    fn recognized_file_produces_no_skipped_detail() {
        let root = test_root("skipped-detail-recognized");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Sonic.zip"), b"zip bytes").unwrap();

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(discovery.archives.len(), 1);
        assert!(discovery.skipped_files.is_empty());
        assert_eq!(discovery.total_skipped(), 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// Test: the aggregate counters stay exact for the *entire* scan even
    /// when the retained detail list is capped at
    /// `MAX_RETAINED_SKIPPED_FILES` - the count is never derived from
    /// `skipped_files.len()`, and truncation is reported honestly rather
    /// than the list silently pretending to be complete.
    #[test]
    fn aggregate_counters_remain_exact_when_the_detail_list_is_capped() {
        let root = test_root("skipped-detail-capped");
        fs::create_dir_all(&root).unwrap();
        let extra = 5;
        let total_unsupported = MAX_RETAINED_SKIPPED_FILES + extra;
        for index in 0..total_unsupported {
            fs::write(root.join(format!("junk-{index:05}.xyz")), b"").unwrap();
        }

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(
            discovery.skipped_unsupported_extension, total_unsupported,
            "the exact aggregate count must reflect every skipped file, capped detail or not"
        );
        assert_eq!(discovery.total_skipped(), total_unsupported);
        assert_eq!(
            discovery.skipped_files.len(),
            MAX_RETAINED_SKIPPED_FILES,
            "the retained detail list must never grow past the bound"
        );
        assert!(discovery.skipped_files_truncated());

        let _ = fs::remove_dir_all(&root);
    }

    /// Test: no skipped file is ever recorded twice in `skipped_files`,
    /// whether or not it shares its skip reason with others in the same
    /// scan.
    #[test]
    fn no_duplicate_skipped_entries() {
        let root = test_root("skipped-detail-no-duplicates");
        fs::create_dir_all(&root).unwrap();
        for name in ["a.png", "b.jpg", "c.xyz"] {
            fs::write(root.join(name), b"").unwrap();
        }
        for name in ["d.gen", "e.bin"] {
            fs::write(root.join(name), b"").unwrap();
        }

        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let discovery = ArchiveScanner::new(&config)
            .scan_archives_with_summary()
            .unwrap();

        assert_eq!(discovery.skipped_files.len(), 5);
        let unique: std::collections::HashSet<&Path> = discovery
            .skipped_files
            .iter()
            .map(|item| item.path.as_path())
            .collect();
        assert_eq!(unique.len(), discovery.skipped_files.len());

        let _ = fs::remove_dir_all(&root);
    }

    /// Media/platform-responsibility review: `.g64` is strong extension
    /// evidence for Commodore 64 and only weak evidence for Commodore 128
    /// (see `platform::PLATFORMS`) - a real, standing conflict between the
    /// two platforms' own extension tables. Folder-alias evidence must
    /// still win over that conflicting strong extension, exactly as it
    /// already does for every other weak/shared extension: the media
    /// registry only ever decides "this is recognised media"; which
    /// platform it belongs to is entirely `detect_platform_with_provenance`'s
    /// folder-alias-first precedence, unchanged by this work.
    #[test]
    fn folder_alias_wins_over_conflicting_strong_extension_for_g64_in_c128_folder() {
        let root = test_root("g64-folder-alias-precedence").join("c128");
        fs::create_dir_all(&root).unwrap();
        let g64 = root.join("Ultima V.g64");
        fs::write(&g64, vec![0_u8; 1024]).unwrap();

        let archive = Archive::from_path_in_root(&g64, &root).unwrap();
        assert_eq!(archive.kind, ArchiveKind::DirectGameImage);
        assert_eq!(
            archive.identity.platform.as_deref(),
            Some("Commodore 128"),
            "a `c128`-aliased folder must resolve `.g64` to Commodore 128, not Commodore 64, \
             even though `.g64` is C64's own strong extension"
        );
        assert_eq!(
            archive.identity.platform_provenance,
            Some(PlatformProvenance::FolderAlias)
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// Platform-safety review: `.chd` (like `.d64`/`.g64`) is deliberately
    /// never strong evidence for any single platform on its own - it is
    /// shared across Neo Geo CD, Sega CD, arcade sets, and more. A `.chd`
    /// with no corroborating folder/source/header evidence must resolve no
    /// platform at all, never default to any one of the platforms that
    /// merely lists it as weak evidence.
    #[test]
    fn a_bare_chd_with_no_corroborating_evidence_resolves_no_platform() {
        let root = test_root("chd-no-corroboration").join("random");
        fs::create_dir_all(&root).unwrap();
        let chd = root.join("Game.chd");
        fs::write(&chd, vec![0_u8; 1024]).unwrap();

        let archive = Archive::from_path_in_root(&chd, &root).unwrap();
        assert_eq!(archive.kind, ArchiveKind::DirectGameImage);
        assert_eq!(
            archive.identity.platform, None,
            "a `.chd` outside any recognised folder must never be assumed to be Neo Geo CD \
             (or any other platform) by extension alone"
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// The mirror case: a `neogeocd`-aliased folder still resolves `.chd`
    /// to Neo Geo CD, exactly like every other weak/shared extension.
    #[test]
    fn chd_resolves_neo_geo_cd_when_folder_evidence_corroborates_it() {
        let root = test_root("chd-neogeocd-folder-alias").join("neogeocd");
        fs::create_dir_all(&root).unwrap();
        let chd = root.join("Metal Slug.chd");
        fs::write(&chd, vec![0_u8; 1024]).unwrap();

        let archive = Archive::from_path_in_root(&chd, &root).unwrap();
        assert_eq!(archive.identity.platform.as_deref(), Some("Neo Geo CD"));
        assert_eq!(
            archive.identity.platform_provenance,
            Some(PlatformProvenance::FolderAlias)
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// Real-world-surfaced regression: `neocdz` (the actual MAME driver
    /// name, and what several real Neo Geo CD collections/frontends use as
    /// a folder name) must resolve `.chd` to Neo Geo CD via folder-alias
    /// evidence, exactly like `neogeocd`/`snkneogeocd`/`ngcd` already do.
    /// The companion case in the same test proves this is still genuine
    /// folder-alias corroboration, not a relaxation of CHD's ambiguity: a
    /// `.chd` in an unrelated folder must remain unresolved.
    #[test]
    fn neocdz_folder_alias_resolves_chd_to_neo_geo_cd_while_an_unknown_folder_stays_unresolved() {
        let root = test_root("chd-neocdz-folder-alias").join("neocdz");
        fs::create_dir_all(&root).unwrap();
        let chd = root.join("2020 Super Baseball (World).chd");
        fs::write(&chd, vec![0_u8; 1024]).unwrap();

        let archive = Archive::from_path_in_root(&chd, &root).unwrap();
        assert_eq!(archive.kind, ArchiveKind::DirectGameImage);
        assert_eq!(archive.identity.platform.as_deref(), Some("Neo Geo CD"));
        assert_eq!(
            archive.identity.platform_provenance,
            Some(PlatformProvenance::FolderAlias)
        );

        let unknown_root = test_root("chd-neocdz-folder-alias").join("some-other-folder");
        fs::create_dir_all(&unknown_root).unwrap();
        let unknown_chd = unknown_root.join("Game.chd");
        fs::write(&unknown_chd, vec![0_u8; 1024]).unwrap();
        let unknown_archive = Archive::from_path_in_root(&unknown_chd, &unknown_root).unwrap();
        assert_eq!(
            unknown_archive.identity.platform, None,
            "`.chd` must stay unresolved outside a recognised folder - the fix only adds a \
             folder alias, it never makes `.chd` sufficient platform identity on its own"
        );

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn archive_kind_recognizes_loose_commodore_disk_images() {
        assert_eq!(
            archive_kind(Path::new("BurgerWhop!.d64")),
            Some(ArchiveKind::DirectGameImage)
        );
        assert_eq!(
            archive_kind(Path::new("Ultima V.g64")),
            Some(ArchiveKind::DirectGameImage)
        );
        assert_eq!(
            archive_kind(Path::new("BurgerWhop!.D64")),
            Some(ArchiveKind::DirectGameImage)
        );
        // Existing archive-format recognition is unaffected by this addition.
        assert_eq!(archive_kind(Path::new("Sonic.zip")), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind(Path::new("gamelist.xml")), None);
        assert_eq!(archive_kind(Path::new("boxart.png")), None);
    }

    #[test]
    fn archive_title_strips_loose_commodore_disk_image_extensions() {
        assert_eq!(archive_title(Path::new("BurgerWhop!.d64")), "BurgerWhop!");
        assert_eq!(
            archive_title(Path::new(
                "Ultima V - Warriors of Destiny(Disk 1 of 4 Side A)(Main).g64"
            )),
            "Ultima V - Warriors of Destiny(Disk 1 of 4 Side A)(Main)"
        );
        // Existing archive-format title stripping is unaffected.
        assert_eq!(archive_title(Path::new("Sonic.zip")), "Sonic");
    }

    // -------------------------------------------------------------------
    // Media-registry review follow-up: CHD recognition/title handling and
    // scan/watch parity.
    // -------------------------------------------------------------------

    #[test]
    fn archive_kind_recognizes_chd() {
        assert_eq!(
            archive_kind(Path::new("Metal Slug.chd")),
            Some(ArchiveKind::DirectGameImage)
        );
        assert_eq!(
            archive_kind(Path::new("Metal Slug.CHD")),
            Some(ArchiveKind::DirectGameImage)
        );
    }

    #[test]
    fn archive_title_strips_chd_extension() {
        // Registry-backed title stripping (see `media_registry`): a newly
        // registered format must never need its own second hardcoded
        // extension list here to have its suffix stripped.
        assert_eq!(archive_title(Path::new("Metal Slug.chd")), "Metal Slug");
        assert_ne!(archive_title(Path::new("Metal Slug.chd")), "Metal Slug.chd");
    }

    #[test]
    fn scan_and_watch_recognize_exactly_the_same_media_registry_extensions() {
        // Every extension `archive_kind` (scanning) recognises on its own
        // must also be watch-relevant, and vice versa for anything the
        // watcher does not additionally treat as a Mega Drive corroboration
        // candidate - both must derive from `media_registry`, never two
        // independently maintained lists.
        for format in media_registry::MEDIA_FORMATS {
            let dotted = format!("Game.{}", format.extension);
            assert_eq!(
                archive_kind(Path::new(&dotted)),
                Some(format.kind),
                "archive_kind must recognise `.{}`",
                format.extension
            );
            assert!(
                watch_path_is_supported_archive(Path::new(&format!("/roms/{dotted}"))),
                "the watcher must recognise `.{}` - previously gcz/rvz/wbfs/ciso/d64/g64/chd \
                 were missing from its own separate, stale extension list",
                format.extension
            );
        }
    }

    #[test]
    fn watch_closes_the_previously_missing_direct_image_extension_gaps() {
        // These four extensions were already recognised by `archive_kind`
        // (scanning) before this change, but were absent from the
        // watcher's own separate hardcoded list - a real scan/watch
        // disagreement this change closes.
        for extension in ["gcz", "rvz", "wbfs", "ciso"] {
            assert!(
                watch_path_is_supported_archive(Path::new(&format!("/roms/Game.{extension}"))),
                "the watcher must recognise `.{extension}`"
            );
        }
    }

    #[test]
    fn dated_region_collection_root_normalizes_to_gamecube() {
        let root = test_root("dated-gamecube-alias")
            .join("Nintendo - GameCube (2018-08-25 20-44-48) (America)");
        fs::create_dir_all(&root).unwrap();
        let image = root.join("ZooCube (USA).rvz");
        fs::write(&image, b"rvz fixture").unwrap();
        let detection = detect_platform_with_details(&image, &root).unwrap();
        assert_eq!(detection.platform, "GameCube");
        assert_eq!(detection.provenance, PlatformProvenance::FolderAlias);
        for alias in [
            "Nintendo_GameCube 2018-08-25",
            "NINTENDO - GAMECUBE (Europe)",
            "g.c.n",
        ] {
            let alias_root = root.parent().unwrap().join(alias);
            let candidate = alias_root.join("game.rvz");
            assert_eq!(
                detect_platform_with_details(&candidate, &alias_root).map(|value| value.platform),
                Some("GameCube".to_string())
            );
        }
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn direct_iso_header_identity_beats_gamecube_folder_alias() {
        let root = test_root("direct-header-precedence").join("gcn");
        fs::create_dir_all(&root).unwrap();
        let image = root.join("disc.iso");
        let mut bytes = vec![0_u8; 64];
        bytes[0x18..0x1c].copy_from_slice(&[0x5d, 0x1c, 0x9e, 0xa3]);
        fs::write(&image, bytes).unwrap();
        let archive = Archive::from_path_in_root(&image, &root).unwrap();
        assert_eq!(archive.identity.platform.as_deref(), Some("Wii"));
        assert_eq!(
            archive.identity.platform_provenance,
            Some(PlatformProvenance::HeaderIdentity)
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn mega_drive_scan_preserves_non_utf8_path_identity() {
        use std::os::unix::ffi::OsStringExt;

        let root = test_root("megadrive-non-utf8");
        let directory = root.join("megadrive");
        fs::create_dir_all(&directory).unwrap();
        let name = std::ffi::OsString::from_vec(b"game-\xff.md".to_vec());
        let path = directory.join(name);
        fs::write(&path, b"rom").unwrap();
        let config = Config {
            source_folders: vec![root.clone()],
            mount_root: root.join("mount"),
            ratarmount_bin: "ratarmount".into(),
            master_rom_root: None,
        };
        let archives = ArchiveScanner::new(&config).scan_archives().unwrap();
        assert_eq!(archives[0].path, path);
        let _ = fs::remove_dir_all(root);
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
        for image in [
            "game.iso",
            "game.GCM",
            "game.gcz",
            "game.RVZ",
            "game.wbfs",
            "game.CISO",
        ] {
            assert_eq!(archive_kind(image), Some(ArchiveKind::DirectGameImage));
        }
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
            master_rom_root: None,
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
            master_rom_root: None,
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
        fs::write(&first_archive, b"archive").unwrap();
        fs::write(&second_archive, b"archive").unwrap();
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
    fn bulk_unmount_ignores_unrelated_and_similarly_named_active_mounts() {
        let root = test_root("bulk_unmount_exact_plans");
        let source_root = root.join("roms").join("xbox360");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("Game.zip"), b"archive").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let plan = ArchiveScanner::new(&config)
            .mount_plans()
            .unwrap()
            .remove(0);
        fs::create_dir_all(plan.mount_path.parent().unwrap()).unwrap();
        let unrelated = config.mount_root.join("Xbox360/Game-old");
        let backend = RecordingBackend::with_active(HashSet::from([
            plan.mount_path.clone(),
            unrelated.clone(),
        ]));

        unmount_archives_with_backend(&config, &backend).unwrap();

        assert_eq!(backend.unmounted(), vec![plan.mount_path]);
        assert!(backend.active.borrow().contains(&unrelated));
    }

    #[test]
    fn normal_and_lazy_unmount_refuse_nested_active_mounts() {
        let root = test_root("unmount_nested_active");
        let mount_root = root.join("mounts");
        let mount_path = mount_root.join("Platform/Game");
        let nested = mount_path.join("child");
        fs::create_dir_all(mount_path.parent().unwrap()).unwrap();
        let config = Config {
            source_folders: vec![root.clone()],
            mount_root,
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let plan = MountPlan::new(
            Archive::from_path(root.join("Game.zip")).unwrap(),
            mount_path.clone(),
        );
        let active = HashSet::from([mount_path.clone(), nested]);
        let backend = RecordingBackend::with_active(active.clone());

        let error = unmount_one_plan_outcome_with_active_mounts(&config, plan, &backend, &active)
            .unwrap_err();

        assert!(error.to_string().contains("nested mount"));
        assert!(backend.unmounted().is_empty());
        assert!(validate_lazy_unmount_path(&config, &mount_path, &active).is_ok());
        assert!(refuse_nested_active_mounts(&mount_path, &active).is_err());
    }

    #[test]
    fn mount_refuses_nonempty_existing_target_and_missing_archive() {
        let root = test_root("mount_revalidate_inputs");
        let archive_path = root.join("roms/Game.zip");
        let mount_path = root.join("mounts/Unknown/Game");
        fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        fs::write(&archive_path, b"archive").unwrap();
        fs::create_dir_all(&mount_path).unwrap();
        fs::write(mount_path.join("owner-file"), b"owner data").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let backend = RecordingBackend::default();
        let plan = MountPlan::new(
            Archive::from_path(&archive_path).unwrap(),
            mount_path.clone(),
        );

        let error = mount_unmounted_plan(&config, plan.clone(), &backend).unwrap_err();
        assert!(error.to_string().contains("nonempty"));
        assert!(backend.mounted().is_empty());

        fs::remove_file(mount_path.join("owner-file")).unwrap();
        fs::write(&archive_path, b"archive changed size").unwrap();
        let error = mount_unmounted_plan(&config, plan.clone(), &backend).unwrap_err();
        assert!(error.to_string().contains("changed after planning"));
        assert!(backend.mounted().is_empty());

        fs::remove_file(&archive_path).unwrap();
        let error = mount_unmounted_plan(&config, plan, &backend).unwrap_err();
        assert!(error.to_string().contains("No such file"));
        assert!(backend.mounted().is_empty());
    }

    #[test]
    fn backend_success_is_not_reported_before_mount_postcondition_verification() {
        struct FalseSuccessBackend;
        impl MountBackend for FalseSuccessBackend {
            fn mount(&self, _plan: &MountPlan) -> Result<()> {
                Ok(())
            }

            fn unmount(&self, _mount_path: &Path) -> Result<()> {
                Ok(())
            }

            fn verify_mounted(&self, mount_path: &Path) -> Result<()> {
                Err(ArchiveFsError::Mount(format!(
                    "{} did not become mounted",
                    mount_path.display()
                )))
            }
        }

        let root = test_root("mount_false_success");
        let archive_path = root.join("roms/Game.zip");
        fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        fs::write(&archive_path, b"archive").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let plan = MountPlan::new(
            Archive::from_path(archive_path).unwrap(),
            config.mount_root.join("Unknown/Game"),
        );

        let error = mount_unmounted_plan(&config, plan, &FalseSuccessBackend).unwrap_err();
        assert!(error.to_string().contains("did not become mounted"));
    }

    #[test]
    fn relative_and_filesystem_root_mount_roots_are_rejected() {
        assert!(prepare_mount_root(Path::new("relative/mounts")).is_err());
        assert!(prepare_mount_root(Path::new("/")).is_err());
        let root = test_root("mount_root_traversal");
        assert!(prepare_mount_root(&root.join("parent/../mounts")).is_err());
        assert!(!root.join("mounts").exists());
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
            master_rom_root: None,
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
            master_rom_root: None,
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
        let expected = select_mount_plan_by_path(
            &ArchiveScanner::new(&config).mount_plans().unwrap(),
            &second,
        )
        .unwrap();
        fs::create_dir_all(expected.mount_path.parent().unwrap()).unwrap();
        let backend = RecordingBackend::with_active(HashSet::from([expected.mount_path]));

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
            master_rom_root: None,
        };
        let expected_mount_path = mount_root.join("Xbox360").join("007_Legends");
        fs::create_dir_all(expected_mount_path.parent().unwrap()).unwrap();
        let backend = RecordingBackend::with_active(HashSet::from([expected_mount_path.clone()]));

        let plan = unmount_one_archive_with_backend(&config, "007 Legends", &backend).unwrap();

        assert_eq!(plan.mount_path, expected_mount_path);
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
            master_rom_root: None,
        };
        let backend = RecordingBackend::default();
        let error = unmount_one_archive_with_backend(&config, "missing", &backend).unwrap_err();

        assert!(matches!(
            error,
            ArchiveFsError::Selection(SelectionError::NoMatch { input }) if input == "missing"
        ));
        assert!(backend.unmounted().is_empty());
    }

    #[test]
    fn unmount_one_does_not_report_success_when_target_is_not_mounted() {
        let root = test_root("unmount_one_not_mounted");
        let source_root = root.join("roms");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("Game.zip"), b"archive").unwrap();
        let config = Config {
            source_folders: vec![source_root],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let backend = RecordingBackend::default();

        let error = unmount_one_archive_with_backend(&config, "Game", &backend).unwrap_err();

        assert!(error.to_string().contains("not currently mounted"));
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
    fn catalogue_duplicates_share_filename_detector_grouping_rules() {
        let archives = vec![
            persisted_duplicate_archive(1, "/roms/a/Halo 3.zip", Some("Xbox"), true, Some(10)),
            persisted_duplicate_archive(2, "/roms/b/Halo_3.7z", Some("Xbox"), true, Some(20)),
            persisted_duplicate_archive(3, "/roms/c/Halo 3.rar", Some("Xbox360"), true, Some(30)),
            persisted_duplicate_archive(4, "/roms/d/Fable.zip", Some("Xbox"), true, Some(40)),
        ];

        let report = catalogue_filename_duplicates(&archives);

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.archives_in_groups, 2);
        assert_eq!(report.groups[0].normalized_title, "halo_3");
        assert_eq!(report.groups[0].platform, "Xbox");
        assert_eq!(
            report.groups[0].reason,
            "Matching normalized filename and platform"
        );
        assert_eq!(
            report.groups[0]
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("/roms/a/Halo 3.zip"),
                PathBuf::from("/roms/b/Halo_3.7z")
            ]
        );
    }

    #[test]
    fn catalogue_duplicates_keep_present_missing_and_partial_size_truthful() {
        let archives = vec![
            persisted_duplicate_archive(1, "/roms/a/Game.zip", Some("Amiga"), true, Some(10)),
            persisted_duplicate_archive(2, "/roms/b/Game.7z", Some("Amiga"), false, None),
            persisted_duplicate_archive(3, "/roms/c/Game.rar", Some("Amiga"), true, Some(30)),
        ];

        let group = &catalogue_filename_duplicates(&archives).groups[0];

        assert_eq!(group.entries.len(), 3);
        assert_eq!(
            group.entries.iter().filter(|entry| entry.present).count(),
            2
        );
        assert_eq!(group.entries_with_known_size, 2);
        assert_eq!(group.total_known_size_bytes, 40);
    }

    #[test]
    fn catalogue_duplicate_order_is_deterministic_across_reloads() {
        let archives = vec![
            persisted_duplicate_archive(4, "/z/Zelda.7z", Some("NES"), true, Some(1)),
            persisted_duplicate_archive(3, "/a/Zelda.zip", Some("NES"), true, Some(2)),
            persisted_duplicate_archive(2, "/z/Alpha.7z", Some("SNES"), true, Some(3)),
            persisted_duplicate_archive(1, "/a/Alpha.zip", Some("SNES"), true, Some(4)),
        ];

        let first = catalogue_filename_duplicates(&archives);
        let second = catalogue_filename_duplicates(&archives.clone());

        assert_eq!(first, second);
        assert_eq!(first.groups[0].normalized_title, "zelda");
        assert_eq!(first.groups[1].normalized_title, "alpha");
        assert_eq!(
            first.groups[0].entries[0].path,
            PathBuf::from("/a/Zelda.zip")
        );
    }

    #[cfg(unix)]
    #[test]
    fn catalogue_duplicates_preserve_non_utf8_exact_paths_without_panicking() {
        use std::os::unix::ffi::OsStringExt;

        let mut first = PathBuf::from("/roms/a");
        first.push(std::ffi::OsString::from_vec(b"Game\x80.zip".to_vec()));
        let mut second = PathBuf::from("/roms/b");
        second.push(std::ffi::OsString::from_vec(b"Game\x80.7z".to_vec()));
        let archives = vec![
            persisted_duplicate_archive_path(1, first.clone(), Some("PC"), true, None),
            persisted_duplicate_archive_path(2, second.clone(), Some("PC"), true, None),
        ];

        let report = catalogue_filename_duplicates(&archives);

        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].entries[0].path, first);
        assert_eq!(report.groups[0].entries[1].path, second);
    }

    // -----------------------------------------------------------------
    // v0.4.3-alpha: Health and Recovery Dashboard - classification tests.
    // -----------------------------------------------------------------

    /// A baseline "nothing wrong" input: `classify_archive_health` must
    /// return `None` for this exact input (see
    /// `healthy_archive_produces_no_issue`) - every other test below
    /// overrides exactly one field away from this baseline to isolate
    /// what actually triggered the resulting category.
    fn healthy_health_input(path: &Path) -> ArchiveHealthInput<'_> {
        ArchiveHealthInput {
            path,
            platform: Some("SNES"),
            presence: ArchivePresence::Confirmed,
            mount_state: Some(MountState::Mounted),
            archive_health: Some(ArchiveHealth::Mounted),
            recovery_offer: None,
            last_seen_at: Some("2026-01-01T00:00:00Z"),
            size_bytes: Some(1024),
            modified_time_unix_seconds: Some(1_700_000_000),
        }
    }

    #[test]
    fn healthy_archive_produces_no_issue() {
        let path = PathBuf::from("/roms/a/Game.zip");
        assert!(classify_archive_health(&healthy_health_input(&path)).is_none());
    }

    #[test]
    fn pending_live_archive_with_no_failure_produces_no_issue() {
        // Requirement 12: "mounted and pending states classify correctly"
        // - a freshly discovered, not-yet-mounted archive with no mount
        // attempt failure must never be reported as a health issue.
        let path = PathBuf::from("/roms/a/Game.zip");
        let input = ArchiveHealthInput {
            mount_state: Some(MountState::Pending),
            archive_health: Some(ArchiveHealth::Pending),
            ..healthy_health_input(&path)
        };
        assert!(classify_archive_health(&input).is_none());
    }

    #[test]
    fn mounted_archive_produces_no_issue() {
        let path = PathBuf::from("/roms/a/Game.zip");
        let input = ArchiveHealthInput {
            mount_state: Some(MountState::Mounted),
            archive_health: Some(ArchiveHealth::Mounted),
            ..healthy_health_input(&path)
        };
        assert!(classify_archive_health(&input).is_none());
    }

    #[test]
    fn terminal_and_retryable_failures_remain_distinct() {
        let path = PathBuf::from("/roms/a/Game.zip");
        for (health, expected) in [
            (ArchiveHealth::Corrupt, HealthCategory::TerminalFailure),
            (ArchiveHealth::Unsupported, HealthCategory::TerminalFailure),
            (
                ArchiveHealth::PermissionDenied,
                HealthCategory::TerminalFailure,
            ),
            (ArchiveHealth::Failed, HealthCategory::RetryableFailure),
            (
                ArchiveHealth::MissingParts,
                HealthCategory::RetryableFailure,
            ),
            (
                ArchiveHealth::RetryAvailable,
                HealthCategory::RetryableFailure,
            ),
        ] {
            let input = ArchiveHealthInput {
                mount_state: Some(MountState::Pending),
                archive_health: Some(health),
                ..healthy_health_input(&path)
            };
            let issue = classify_archive_health(&input)
                .unwrap_or_else(|| panic!("{health:?} must be reported as an issue"));
            assert_eq!(
                issue.category, expected,
                "{health:?} must classify as {expected:?}"
            );
        }

        let terminal = classify_archive_health(&ArchiveHealthInput {
            archive_health: Some(ArchiveHealth::Corrupt),
            ..healthy_health_input(&path)
        })
        .unwrap();
        let retryable = classify_archive_health(&ArchiveHealthInput {
            archive_health: Some(ArchiveHealth::Failed),
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert!(
            !terminal.retryable,
            "a terminal failure must not be reported retryable"
        );
        assert!(
            retryable.retryable,
            "a retryable failure must be reported retryable"
        );
        assert_eq!(terminal.recovery_action, None);
        assert_eq!(retryable.recovery_action, Some(RecoveryAction::RetryMount));
    }

    #[test]
    fn retained_mount_failures_are_historical_not_current_errors() {
        let path = PathBuf::from("/roms/old/Game.zip");
        let issue = classify_archive_health(&ArchiveHealthInput {
            mount_state: None,
            archive_health: Some(ArchiveHealth::Corrupt),
            last_seen_at: Some("2025-01-01T00:00:00Z"),
            ..healthy_health_input(&path)
        })
        .unwrap();

        assert_eq!(issue.category, HealthCategory::HistoricalMountFailure);
        assert!(issue.reason.contains("not a current failure"));
        assert_eq!(issue.recovery_action, None);
    }

    #[test]
    fn loose_content_that_bypasses_mounting_is_not_a_terminal_failure() {
        for path in [
            "/roms/genesis/Wrong Filename [XXXXXX].md",
            "/roms/scummvm/laurabow2/RESOURCE.GEN",
        ] {
            let path = PathBuf::from(path);
            let issue = classify_archive_health(&ArchiveHealthInput {
                mount_state: Some(MountState::NotMountable),
                archive_health: Some(ArchiveHealth::Unsupported),
                ..healthy_health_input(&path)
            })
            .unwrap();
            assert_eq!(issue.category, HealthCategory::MountNotRequired);
            assert!(
                issue
                    .reason
                    .contains("does not use the archive mount workflow")
            );
            assert_eq!(issue.recovery_action, None);
        }
    }

    #[test]
    fn failure_without_current_or_historical_evidence_stays_uncertain() {
        let path = PathBuf::from("/roms/unknown/Game.zip");
        let issue = classify_archive_health(&ArchiveHealthInput {
            mount_state: None,
            archive_health: Some(ArchiveHealth::Failed),
            last_seen_at: None,
            ..healthy_health_input(&path)
        })
        .unwrap();

        assert_eq!(
            issue.category,
            HealthCategory::MountFailureEvidenceInsufficient
        );
        assert!(!issue.retryable);
        assert_eq!(issue.recovery_action, None);
    }

    #[test]
    fn missing_and_cached_only_states_remain_distinct() {
        let path = PathBuf::from("/roms/a/Game.zip");
        let missing = classify_archive_health(&ArchiveHealthInput {
            presence: ArchivePresence::Missing,
            mount_state: None,
            archive_health: None,
            ..healthy_health_input(&path)
        })
        .unwrap();
        let cached_only = classify_archive_health(&ArchiveHealthInput {
            presence: ArchivePresence::Unreachable,
            mount_state: None,
            archive_health: None,
            ..healthy_health_input(&path)
        })
        .unwrap();
        let awaiting_validation = classify_archive_health(&ArchiveHealthInput {
            presence: ArchivePresence::AwaitingValidation,
            mount_state: None,
            archive_health: None,
            ..healthy_health_input(&path)
        })
        .unwrap();

        assert_eq!(missing.category, HealthCategory::Missing);
        assert_eq!(cached_only.category, HealthCategory::CachedOnly);
        assert_eq!(
            awaiting_validation.category,
            HealthCategory::AwaitingValidation
        );
        assert_ne!(missing.category, cached_only.category);
        assert_ne!(missing.category, awaiting_validation.category);
        assert_ne!(cached_only.category, awaiting_validation.category);
        assert!(!missing.present);
        assert!(
            cached_only.present,
            "cached-only is not the same as missing"
        );
        assert!(awaiting_validation.present);
    }

    #[test]
    fn unknown_platform_is_reported_only_when_nothing_more_severe_applies() {
        let path = PathBuf::from("/roms/a/Game.zip");
        let unknown = classify_archive_health(&ArchiveHealthInput {
            platform: None,
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert_eq!(unknown.category, HealthCategory::UnknownPlatform);
        assert_eq!(unknown.platform, None);

        // A missing archive that also has an unknown platform is reported
        // as Missing, never double-counted or reclassified as
        // UnknownPlatform - single most-severe category only.
        let missing_and_unknown = classify_archive_health(&ArchiveHealthInput {
            platform: None,
            presence: ArchivePresence::Missing,
            mount_state: None,
            archive_health: None,
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert_eq!(missing_and_unknown.category, HealthCategory::Missing);
    }

    #[test]
    fn recovery_availability_is_represented_only_when_a_real_offer_exists() {
        let path = PathBuf::from("/roms/a/Game.zip");
        // No offer at all: a pending, otherwise-healthy archive is not an
        // issue just because it is not currently mounted.
        let no_offer = classify_archive_health(&ArchiveHealthInput {
            mount_state: Some(MountState::Pending),
            archive_health: Some(ArchiveHealth::Pending),
            recovery_offer: None,
            ..healthy_health_input(&path)
        });
        assert!(no_offer.is_none());

        let remount = classify_archive_health(&ArchiveHealthInput {
            mount_state: Some(MountState::Pending),
            archive_health: Some(ArchiveHealth::Pending),
            recovery_offer: Some(RecoveryOffer::Remount),
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert_eq!(remount.category, HealthCategory::RecoveryAvailable);
        assert_eq!(remount.reason, "Remount is available");
        assert_eq!(remount.recovery_action, Some(RecoveryAction::Remount));
        assert!(remount.recovery_available());

        let lazy_unmount = classify_archive_health(&ArchiveHealthInput {
            mount_state: Some(MountState::Mounted),
            archive_health: Some(ArchiveHealth::Mounted),
            recovery_offer: Some(RecoveryOffer::LazyUnmount),
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert_eq!(lazy_unmount.category, HealthCategory::RecoveryAvailable);
        assert_eq!(lazy_unmount.reason, "Lazy-unmount recovery is available");
        assert_eq!(
            lazy_unmount.recovery_action,
            Some(RecoveryAction::LazyUnmount)
        );
    }

    #[test]
    fn a_live_mount_failure_outranks_a_merely_offered_recovery() {
        // Severity order: a terminal/retryable failure must win even if a
        // recovery offer also happens to be active for the same path.
        let path = PathBuf::from("/roms/a/Game.zip");
        let issue = classify_archive_health(&ArchiveHealthInput {
            mount_state: Some(MountState::Pending),
            archive_health: Some(ArchiveHealth::Failed),
            recovery_offer: Some(RecoveryOffer::Remount),
            ..healthy_health_input(&path)
        })
        .unwrap();
        assert_eq!(issue.category, HealthCategory::RetryableFailure);
    }

    #[test]
    fn health_category_severity_order_is_deterministic_and_matches_the_documented_default() {
        let mut categories = [
            HealthCategory::UnknownPlatform,
            HealthCategory::CachedOnly,
            HealthCategory::AwaitingValidation,
            HealthCategory::Missing,
            HealthCategory::MountNotRequired,
            HealthCategory::MountFailureEvidenceInsufficient,
            HealthCategory::HistoricalMountFailure,
            HealthCategory::RecoveryAvailable,
            HealthCategory::RetryableFailure,
            HealthCategory::TerminalFailure,
        ];
        categories.sort_by_key(|category| category.severity_rank());
        assert_eq!(
            categories,
            [
                HealthCategory::TerminalFailure,
                HealthCategory::RetryableFailure,
                HealthCategory::RecoveryAvailable,
                HealthCategory::HistoricalMountFailure,
                HealthCategory::MountFailureEvidenceInsufficient,
                HealthCategory::MountNotRequired,
                HealthCategory::Missing,
                HealthCategory::AwaitingValidation,
                HealthCategory::CachedOnly,
                HealthCategory::UnknownPlatform,
            ]
        );

        // Ranks are also stable across repeated calls (no interior
        // randomness / hashing involved).
        for category in categories {
            assert_eq!(category.severity_rank(), category.severity_rank());
        }
    }

    #[test]
    fn exact_paths_remain_distinct_between_two_otherwise_identical_issues() {
        let first_path = PathBuf::from("/roms/a/Game.zip");
        let second_path = PathBuf::from("/roms/a/Game (1).zip");
        let first = classify_archive_health(&ArchiveHealthInput {
            archive_health: Some(ArchiveHealth::Failed),
            mount_state: Some(MountState::Pending),
            ..healthy_health_input(&first_path)
        })
        .unwrap();
        let second = classify_archive_health(&ArchiveHealthInput {
            archive_health: Some(ArchiveHealth::Failed),
            mount_state: Some(MountState::Pending),
            ..healthy_health_input(&second_path)
        })
        .unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(first.path, first_path);
        assert_eq!(second.path, second_path);
    }

    #[cfg(unix)]
    #[test]
    fn classify_archive_health_never_panics_on_a_non_utf8_path() {
        use std::os::unix::ffi::OsStringExt;

        let mut path = PathBuf::from("/roms/a");
        path.push(std::ffi::OsString::from_vec(b"Game\x80.zip".to_vec()));
        let input = ArchiveHealthInput {
            archive_health: Some(ArchiveHealth::Failed),
            mount_state: Some(MountState::Pending),
            ..healthy_health_input(&path)
        };
        let issue = classify_archive_health(&input).unwrap();
        assert_eq!(issue.path, path);
    }

    #[test]
    fn catalogue_health_report_counts_exactly_match_the_issues_present() {
        let archives = vec![
            persisted_duplicate_archive(1, "/roms/a/Game.zip", Some("SNES"), true, Some(10)),
            persisted_duplicate_archive(2, "/roms/b/Missing.zip", Some("SNES"), false, Some(20)),
            persisted_duplicate_archive(3, "/roms/c/Unknown.zip", None, true, Some(30)),
            persisted_duplicate_archive(4, "/roms/d/Fine.zip", Some("Genesis"), true, Some(40)),
        ];

        let report = catalogue_health_report(&archives);

        assert_eq!(report.archives_checked, 4);
        assert_eq!(report.missing_count, 1);
        assert_eq!(report.unknown_platform_count, 1);
        assert_eq!(
            report.issues.len(),
            report.missing_count + report.unknown_platform_count,
            "the report's counts must exactly match the entries actually present"
        );
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|issue| issue.category == HealthCategory::Missing)
                .count(),
            report.missing_count
        );
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|issue| issue.category == HealthCategory::UnknownPlatform)
                .count(),
            report.unknown_platform_count
        );
        // A catalogue-only report never asserts AwaitingValidation or
        // CachedOnly - those require a live session it deliberately never
        // has.
        assert!(report.issues.iter().all(|issue| !matches!(
            issue.category,
            HealthCategory::AwaitingValidation | HealthCategory::CachedOnly
        )));
    }

    #[test]
    fn catalogue_health_report_sorting_is_deterministic_by_severity_then_path() {
        let archives = vec![
            persisted_duplicate_archive(1, "/roms/z/Unknown.zip", None, true, None),
            persisted_duplicate_archive(2, "/roms/a/Missing.zip", Some("SNES"), false, None),
            persisted_duplicate_archive(3, "/roms/b/Missing.zip", Some("SNES"), false, None),
        ];

        let first = catalogue_health_report(&archives);
        let second = catalogue_health_report(&archives.clone());

        assert_eq!(
            first, second,
            "sorting must be deterministic across reloads"
        );
        assert_eq!(first.issues[0].category, HealthCategory::Missing);
        assert_eq!(first.issues[1].category, HealthCategory::Missing);
        assert_eq!(first.issues[2].category, HealthCategory::UnknownPlatform);
        assert!(
            first.issues[0].path < first.issues[1].path,
            "same-severity issues must be ordered by exact path"
        );
    }

    #[test]
    fn catalogue_health_report_does_not_mutate_its_input() {
        let archives = vec![persisted_duplicate_archive(
            1,
            "/roms/a/Game.zip",
            Some("SNES"),
            false,
            Some(10),
        )];
        let before = archives.clone();

        let _ = catalogue_health_report(&archives);

        assert_eq!(
            archives, before,
            "building the report must not mutate the catalogue slice"
        );
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
    fn public_clean_preserves_unrelated_empty_owner_directories() {
        let root = test_root("clean_only_planned_targets");
        let source_root = root.join("roms/xbox360");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("Game.zip"), b"archive").unwrap();
        let config = Config {
            source_folders: vec![root.join("roms")],
            mount_root: root.join("mounts"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        };
        let plan = ArchiveScanner::new(&config)
            .mount_plans()
            .unwrap()
            .remove(0);
        fs::create_dir_all(&plan.mount_path).unwrap();
        let owner_directory = config.mount_root.join("Owner/Empty");
        fs::create_dir_all(&owner_directory).unwrap();

        let removed = clean_mount_root(&config).unwrap();

        assert!(removed.contains(&plan.mount_path));
        assert!(owner_directory.exists());
    }

    #[test]
    fn mountinfo_parser_decodes_octal_escapes_and_uses_exact_paths() {
        let line = b"36 25 0:32 / /mnt/archive\\040fs rw,nosuid - fuse.test test rw";
        assert_eq!(
            mount_path_from_mountinfo_line(line),
            Some(PathBuf::from("/mnt/archive fs"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn mountinfo_parser_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let line = b"36 25 0:32 / /mnt/game\\200 rw - fuse.test test rw";
        let path = mount_path_from_mountinfo_line(line).unwrap();

        assert_eq!(path.as_os_str().as_bytes(), b"/mnt/game\x80");
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_times_out_and_bounds_captured_error_output() {
        if command_available("sleep") {
            let error =
                run_command_os_with_timeout("sleep", &["5".as_ref()], Duration::from_millis(75))
                    .unwrap_err();
            assert!(error.to_string().contains("timed out"));
        }

        if command_available("sh") && command_available("head") {
            let error = run_command_os_with_timeout(
                "sh",
                &[
                    "-c".as_ref(),
                    "head -c 70000 /dev/zero >&2; exit 7".as_ref(),
                ],
                Duration::from_secs(2),
            )
            .unwrap_err();
            let ArchiveFsError::ExternalCommand { stderr, .. } = error else {
                panic!("expected external-command failure");
            };
            assert_eq!(stderr.len(), COMMAND_OUTPUT_LIMIT);
        }
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            master_rom_root: None,
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
            Some(PlatformProvenance::FolderAlias)
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

        // The configured source root itself is valid evidence.
        assert_eq!(
            detect_platform("/home/davedap/msx2/game.zip", "/home/davedap/msx2"),
            Some("MSX2".to_string())
        );
        // Alias-looking ancestors outside that root remain excluded.
        assert_eq!(
            detect_platform("/home/msx2/collection/game.zip", "/home/msx2/collection"),
            None
        );
    }

    #[test]
    fn exact_folder_alias_remains_stronger_than_filename_similarity() {
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/psp/007 Legends.zip",
                "/home/davedap/Archives"
            ),
            Some("PSP".to_string()),
            "an exact folder alias must outrank weaker title similarity"
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
    fn provenance_distinguishes_folder_alias_from_filename_heuristic() {
        let heuristic = detect_platform_with_provenance("/roms/incoming/007 Legends.zip", "/roms")
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

        let detailed = detect_platform_with_details(
            "/home/davedap/Archives/msx2/Game.zip",
            "/home/davedap/Archives",
        )
        .expect("detailed folder alias detection should succeed");
        assert_eq!(detailed.platform, "MSX2");
        assert_eq!(detailed.matched_folder.as_deref(), Some("msx2"));
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
        assert_eq!(detect_platform_with_details(&path, &root), None);
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
            ("archimedes", "Acorn Archimedes"),
            ("riscos", "Acorn Archimedes"),
            ("pc", "PC"),
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

        for expected in [
            "NeoGeo64",
            "NGage",
            "GameCube",
            "MSX2",
            "Xbox360",
            "Acorn Archimedes",
            "PC",
            "Game Boy",
            "Game Boy Color",
            "Game Boy Advance",
            "Nintendo DS",
            "Commodore 64",
            "ZX Spectrum",
            "Sega 32X",
            "Sega CD",
            "PC Engine",
            "TurboGrafx-16",
            "Atari Lynx",
            "Atari Jaguar",
            "Neo Geo Pocket",
            "Neo Geo Pocket Color",
            "WonderSwan",
            "WonderSwan Color",
            "3DO",
            "PlayStation Vita",
            "ColecoVision",
            "Vectrex",
        ] {
            assert!(
                names.contains(&expected),
                "{expected:?} should be a canonical platform name"
            );
        }
    }

    #[test]
    fn every_new_retro_platform_appears_exactly_once_in_canonical_names() {
        // Several new platforms have multiple aliases mapping to the same
        // canonical string (e.g. "gameboy"/"gb" both -> "Game Boy") -
        // `canonical_platform_names`'s dedup must collapse these to one
        // entry each, which is what the GUI's platform selector iterates
        // directly (see `show_platform_section`).
        let names = canonical_platform_names();
        for expected in [
            "Game Boy",
            "Game Boy Color",
            "Game Boy Advance",
            "Nintendo DS",
            "Commodore 64",
            "ZX Spectrum",
            "Sega 32X",
            "Sega CD",
            "PC Engine",
            "TurboGrafx-16",
            "Atari Lynx",
            "Atari Jaguar",
            "Neo Geo Pocket",
            "Neo Geo Pocket Color",
            "WonderSwan",
            "WonderSwan Color",
            "3DO",
            "PlayStation Vita",
            "ColecoVision",
            "Vectrex",
        ] {
            assert_eq!(
                names.iter().filter(|name| **name == expected).count(),
                1,
                "{expected:?} must appear exactly once in canonical_platform_names"
            );
        }
    }

    #[test]
    fn acorn_archimedes_conservative_aliases_detect_the_canonical_platform() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/Archimedes/Game.zip"), root),
            Some("Acorn Archimedes".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Acorn Archimedes/Game.7z"), root),
            Some("Acorn Archimedes".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/RISC OS/Game.zip"), root),
            Some("Acorn Archimedes".to_string())
        );
        // Explicitly NOT an alias: bare "acorn" is too broad (Acorn made
        // several distinct machines) and must not match anything.
        assert_eq!(
            detect_platform(format!("{root}/Acorn/Game.zip"), root),
            None
        );
    }

    #[test]
    fn platform_recovery_expansion_aliases_detect_their_canonical_platform() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/virtualboy/Game.zip"), root),
            Some("Virtual Boy".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Virtual Boy/Game.zip"), root),
            Some("Virtual Boy".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/sharp-x68000/Sharp X68000/Game.zip"), root),
            Some("Sharp X68000".to_string()),
            "the nearer, more specific folder component must win"
        );
        assert_eq!(
            detect_platform(format!("{root}/Sharp X68000/Game.zip"), root),
            Some("Sharp X68000".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/pc-8801/NEC PC-8801/Game.zip"), root),
            Some("NEC PC-8801".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/NEC PC-8801/Game.zip"), root),
            Some("NEC PC-8801".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/zxs/Game.zip"), root),
            Some("ZX Spectrum".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Sinclair ZX Spectrum/Game.zip"), root),
            Some("ZX Spectrum".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Acorn Archimedes/Game.zip"), root),
            Some("Acorn Archimedes".to_string())
        );
    }

    #[test]
    fn arcade_core_folder_names_classify_as_arcade_with_preferred_emulator_evidence() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/fbneo/Game.zip"), root),
            Some("Arcade".to_string())
        );
        assert_eq!(
            platform_preferred_emulator_for_alias("fbneo"),
            Some("FBNeo")
        );
        assert_eq!(
            detect_platform(format!("{root}/mame/Game.zip"), root),
            Some("Arcade".to_string())
        );
        assert_eq!(platform_preferred_emulator_for_alias("mame"), Some("MAME"));
        // Arcade itself (no specific emulator implied by the folder name)
        // has no preferred-emulator evidence, which is not an error.
        assert_eq!(platform_preferred_emulator_for_alias("arcade"), None);
    }

    #[test]
    fn xbox_folder_never_becomes_xbox_360() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/xbox/Game.iso"), root),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Xbox 360/Game.iso"), root),
            Some("Xbox360".to_string())
        );
    }

    #[test]
    fn game_boy_folder_never_becomes_game_boy_advance() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/Game Boy/Game.gb"), root),
            Some("Game Boy".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Game Boy Advance/Game.gba"), root),
            Some("Game Boy Advance".to_string())
        );
    }

    #[test]
    fn external_platform_hint_uses_the_shared_folder_alias_table() {
        assert_eq!(
            canonical_platform_for_alias("Atari - 2600"),
            Some("Atari2600")
        );
        assert_eq!(canonical_platform_for_alias("unknown console"), None);
    }

    #[test]
    fn conflicting_normalized_platform_aliases_are_rejected_as_ambiguous() {
        // The registry now owns this guarantee: `platform_for_alias` refuses to
        // pick a winner when one normalized alias is claimed by more than one
        // platform, and `no_alias_is_claimed_by_two_platforms` keeps the real
        // table free of that case. Both halves are asserted here.
        assert_eq!(
            platform::platform_for_alias("Atari - 2600").map(|entry| entry.id),
            Some("Atari2600"),
            "an unambiguous alias still resolves"
        );
        let mut seen = std::collections::BTreeMap::new();
        for entry in platform::PLATFORMS {
            for alias in entry.folder_aliases {
                assert!(
                    seen.insert(*alias, entry.id).is_none(),
                    "alias `{alias}` is claimed twice, which would make lookup ambiguous"
                );
            }
        }
    }

    #[test]
    fn pc_conservative_aliases_detect_the_canonical_platform() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/PC Games/Game Name.zip"), root),
            Some("PC".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/Windows Games/Game Name.7z"), root),
            Some("PC".to_string())
        );
    }

    #[test]
    fn generic_games_and_desktop_paths_do_not_automatically_become_pc() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/Games/Game Name.zip"), root),
            None,
            "a bare \"Games\" folder must not imply PC"
        );
        assert_eq!(
            detect_platform(format!("{root}/Desktop/GAMES/Game Name.zip"), root),
            None,
            "\"Desktop\"/\"GAMES\" are too generic to imply PC"
        );
    }

    #[test]
    fn dos_remains_distinct_from_pc_including_dos_games() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/DOS/Game Name.zip"), root),
            Some("DOS".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/DOS Games/Game Name.zip"), root),
            Some("DOS".to_string())
        );
    }

    // -----------------------------------------------------------------
    // Retro-platform expansion: 20 new canonical platforms (Nintendo
    // handhelds + retro computers/consoles), conservative folder aliases.
    // -----------------------------------------------------------------

    #[test]
    fn retro_platform_expansion_folder_aliases_detect_the_canonical_platform() {
        let root = "/home/davedap/Archives";
        let cases: &[(&str, &str)] = &[
            ("Game Boy", "Game Boy"),
            ("GBC", "Game Boy Color"),
            ("Game Boy Color", "Game Boy Color"),
            ("GBA", "Game Boy Advance"),
            ("Game Boy Advance", "Game Boy Advance"),
            ("Nintendo DS", "Nintendo DS"),
            ("DS", "Nintendo DS"),
            ("NDS", "Nintendo DS"),
            ("C64", "Commodore 64"),
            ("Commodore 64", "Commodore 64"),
            ("ZX Spectrum", "ZX Spectrum"),
            ("Sega 32X", "Sega 32X"),
            ("32X", "Sega 32X"),
            ("Mega CD", "Sega CD"),
            ("Sega CD", "Sega CD"),
            ("PC Engine", "PC Engine"),
            ("PCE", "PC Engine"),
            ("TurboGrafx-16", "TurboGrafx-16"),
            ("TG16", "TurboGrafx-16"),
            ("Atari Lynx", "Atari Lynx"),
            ("Atari Jaguar", "Atari Jaguar"),
            ("NGP", "Neo Geo Pocket"),
            ("Neo Geo Pocket", "Neo Geo Pocket"),
            ("NGPC", "Neo Geo Pocket Color"),
            ("Neo Geo Pocket Color", "Neo Geo Pocket Color"),
            ("WonderSwan", "WonderSwan"),
            ("WSC", "WonderSwan Color"),
            ("WonderSwan Color", "WonderSwan Color"),
            ("3DO", "3DO"),
            ("Panasonic 3DO", "3DO"),
            ("PS Vita", "PlayStation Vita"),
            ("PSVita", "PlayStation Vita"),
            ("PlayStation Vita", "PlayStation Vita"),
            ("ColecoVision", "ColecoVision"),
            ("Vectrex", "Vectrex"),
        ];

        for (folder, expected) in cases {
            assert_eq!(
                detect_platform(format!("{root}/{folder}/Game.zip"), root),
                Some((*expected).to_string()),
                "folder {folder:?} should detect {expected:?}"
            );
        }
    }

    #[test]
    fn nintendo_ds_and_nintendo_3ds_remain_distinct() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/Nintendo DS/Game.zip"), root),
            Some("Nintendo DS".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/DS/Game.zip"), root),
            Some("Nintendo DS".to_string())
        );
        // "Nintendo 3DS" must never collide with the "ds"/"nds" aliases -
        // `normalize_path_segment` keeps the "3", so "Nintendo 3DS"
        // normalizes to "nintendo3ds", never "ds"/"nds"/"nintendods". It is
        // now its own first-class folder alias (platform recovery
        // expansion), distinct from Nintendo DS.
        assert_eq!(
            detect_platform(format!("{root}/Nintendo 3DS/Game.zip"), root),
            Some("Nintendo 3DS".to_string()),
            "\"Nintendo 3DS\" must not become Nintendo DS"
        );
        assert_eq!(
            detect_platform(format!("{root}/3DS/Game.zip"), root),
            None,
            "a bare \"3DS\" folder must not become Nintendo DS either"
        );
    }

    #[test]
    fn pc_engine_and_turbografx_16_remain_separate_canonical_platforms() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/PC Engine/Game.zip"), root),
            Some("PC Engine".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/TurboGrafx-16/Game.zip"), root),
            Some("TurboGrafx-16".to_string())
        );
    }

    #[test]
    fn neo_geo_pocket_and_neo_geo_pocket_color_remain_separate() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/NGP/Game.zip"), root),
            Some("Neo Geo Pocket".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/NGPC/Game.zip"), root),
            Some("Neo Geo Pocket Color".to_string())
        );
    }

    #[test]
    fn wonderswan_and_wonderswan_color_remain_separate() {
        let root = "/home/davedap/Archives";
        assert_eq!(
            detect_platform(format!("{root}/WonderSwan/Game.zip"), root),
            Some("WonderSwan".to_string())
        );
        assert_eq!(
            detect_platform(format!("{root}/WSC/Game.zip"), root),
            Some("WonderSwan Color".to_string())
        );
    }

    #[test]
    fn broad_brand_only_and_generic_folders_remain_unknown() {
        let root = "/home/davedap/Archives";
        for folder in ["Games", "Nintendo", "Sega", "Atari", "Sony"] {
            assert_eq!(
                detect_platform(format!("{root}/{folder}/Game.zip"), root),
                None,
                "a bare {folder:?} folder must not imply any platform"
            );
        }
    }

    #[test]
    fn short_alias_substrings_never_trigger_folder_alias_detection() {
        // The new short aliases ("ds", "gb", "lynx", "jaguar", "vita",
        // "pce", "wsc", "spectrum", ...) match one whole, normalized path
        // *component* only - never a substring of a longer folder name,
        // and never the archive's own filename (see
        // `detect_platform_from_folder_alias_with_match`, which excludes
        // the filename entirely before matching).
        let root = "/home/davedap/Archives";
        let filenames = [
            "MyVitaGame.zip",
            "dsgame.zip",
            "gbcollection.zip",
            "lynxmania.zip",
            "jaguarland.zip",
            "spectrumanalysis.zip",
            "pcexpress.zip",
            "wscfile.zip",
        ];
        for filename in filenames {
            assert_eq!(
                detect_platform(format!("{root}/Unsorted/{filename}"), root),
                None,
                "filename {filename:?} must never trigger folder-alias detection"
            );
        }
        // A folder whose normalized name merely *starts with* or contains
        // a short alias must not match either - only an exact, whole-
        // component match counts.
        for folder in [
            "Digital Stuff", // normalizes to "digitalstuff", not "ds"
            "Galaxy Battle", // normalizes to "galaxybattle", not "gb"
        ] {
            assert_eq!(
                detect_platform(format!("{root}/{folder}/Game.zip"), root),
                None,
                "folder {folder:?} must not partially match a short alias"
            );
        }
    }

    // -----------------------------------------------------------------
    // Platform detection must never depend on mount state (requirement:
    // detection happens during scanning/catalogue reconciliation, before
    // - and entirely independent of - any mount action).
    // -----------------------------------------------------------------

    #[test]
    fn unmounted_archive_receives_automatic_platform_detection() {
        // `ArchiveIdentity::from_path` - the sole entry point archive
        // scanning uses to populate `identity.platform` - takes only a
        // path, source root, and filesystem metadata. It has no mount
        // state parameter at all, so an archive the scanner has never
        // even attempted to mount still gets a confident detection here.
        let identity = ArchiveIdentity::from_path(
            Path::new("/home/davedap/Archives/snes/Game.zip"),
            PathBuf::from("/home/davedap/Archives"),
            None,
        );
        assert_eq!(identity.platform.as_deref(), Some("SNES"));
    }

    #[test]
    fn mounting_and_unmounting_do_not_change_the_detected_platform() {
        // Build one `ArchiveRecord` per `MountState` from the exact same
        // path/source-root pair, mirroring how the scanner attaches a
        // separately-computed `MountState` (from `/proc/self/mountinfo`
        // via `mount_state_for_plan`) onto an already-detected identity -
        // the two are structurally independent, computed by different
        // functions with no shared input. Detection must be identical
        // across all three mount states.
        let archive_for = |mount_state: MountState| {
            let path = PathBuf::from("/home/davedap/Archives/snes/Game.zip");
            let identity =
                ArchiveIdentity::from_path(&path, PathBuf::from("/home/davedap/Archives"), None);
            let archive = Archive {
                kind: archive_kind(&path).unwrap(),
                identity,
                path,
                health: ArchiveHealth::Pending,
            };
            ArchiveRecord::new(
                MountPlan::new(archive, PathBuf::from("/mnt/archivefs/Test")),
                mount_state,
                ArchiveMetadata::empty(),
                ArchiveHealth::Pending,
            )
        };

        let pending = archive_for(MountState::Pending);
        let mounted = archive_for(MountState::Mounted);
        let mount_path_exists = archive_for(MountState::MountPathExists);

        assert_eq!(pending.identity.platform.as_deref(), Some("SNES"));
        assert_eq!(
            pending.identity.platform, mounted.identity.platform,
            "mounting must not change the detected platform"
        );
        assert_eq!(
            pending.identity.platform, mount_path_exists.identity.platform,
            "an existing mount-path directory must not change the detected platform"
        );
    }

    #[test]
    fn mounting_and_unmounting_do_not_change_new_retro_platform_detection() {
        // Same guarantee as `mounting_and_unmounting_do_not_change_the_
        // detected_platform`, exercised for two of this milestone's new
        // canonical platforms - one Nintendo handheld, one non-Nintendo
        // console - since `ArchiveIdentity::from_path`/`mount_state_for_
        // plan` are the same structurally-independent functions regardless
        // of which platform's alias matched.
        let archive_for = |path: &str, mount_state: MountState| {
            let path = PathBuf::from(path);
            let identity =
                ArchiveIdentity::from_path(&path, PathBuf::from("/home/davedap/Archives"), None);
            let archive = Archive {
                kind: archive_kind(&path).unwrap(),
                identity,
                path,
                health: ArchiveHealth::Pending,
            };
            ArchiveRecord::new(
                MountPlan::new(archive, PathBuf::from("/mnt/archivefs/Test")),
                mount_state,
                ArchiveMetadata::empty(),
                ArchiveHealth::Pending,
            )
        };

        for (path, expected) in [
            (
                "/home/davedap/Archives/Game Boy Advance/Game.zip",
                "Game Boy Advance",
            ),
            (
                "/home/davedap/Archives/PS Vita/Game.zip",
                "PlayStation Vita",
            ),
        ] {
            let pending = archive_for(path, MountState::Pending);
            let mounted = archive_for(path, MountState::Mounted);
            assert_eq!(pending.identity.platform.as_deref(), Some(expected));
            assert_eq!(
                pending.identity.platform, mounted.identity.platform,
                "mounting must not change the detected platform for {expected:?}"
            );
        }
    }

    #[test]
    fn rescanning_applies_the_current_folder_alias_rules() {
        // A stand-in for "rescan": detection is a pure function of the
        // current alias table and the path, so re-running it (as a
        // rescan would) against a folder alias added in this pass
        // reflects the current rules with no separate "refresh" step
        // needed - there is no stale cache to invalidate.
        assert_eq!(
            detect_platform(
                "/home/davedap/Archives/Acorn Archimedes/Game.zip",
                "/home/davedap/Archives"
            ),
            Some("Acorn Archimedes".to_string())
        );
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
            master_rom_root: None,
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
    fn scanner_refuses_a_symlinked_source_root_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = test_root("scanner_symlink_source");
        let real_source = root.join("real-source");
        let linked_source = root.join("linked-source");
        fs::create_dir_all(&real_source).unwrap();
        fs::write(real_source.join("outside.zip"), b"contents").unwrap();
        symlink(&real_source, &linked_source).unwrap();

        let error = ArchiveScanner::new(&scanner_config(&linked_source))
            .scan_archives()
            .unwrap_err();

        assert!(error.to_string().contains("symlinked source component"));
    }

    #[cfg(unix)]
    #[test]
    fn scanner_skips_a_symlink_escape_beneath_a_valid_source_root() {
        use std::os::unix::fs::symlink;

        let root = test_root("scanner_nested_symlink_escape");
        let source = root.join("source");
        let outside = root.join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(source.join("inside.zip"), b"inside").unwrap();
        fs::write(outside.join("outside.zip"), b"outside").unwrap();
        symlink(&outside, source.join("escape")).unwrap();

        let archives = ArchiveScanner::new(&scanner_config(&source))
            .scan_archives()
            .unwrap();

        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].path, source.join("inside.zip"));
    }

    #[test]
    fn scanner_rejects_duplicate_and_nested_roots_but_not_prefix_siblings() {
        let root = test_root("scanner_source_overlap");
        let source = root.join("cache");
        let child = source.join("nested");
        let prefix_sibling = root.join("cache-old");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&prefix_sibling).unwrap();

        let duplicate = Config {
            source_folders: vec![source.clone(), source.clone()],
            ..scanner_config(&root)
        };
        assert!(
            ArchiveScanner::new(&duplicate)
                .scan_archives()
                .unwrap_err()
                .to_string()
                .contains("overlapping")
        );

        let nested = Config {
            source_folders: vec![source.clone(), child],
            ..scanner_config(&root)
        };
        assert!(
            ArchiveScanner::new(&nested)
                .scan_archives()
                .unwrap_err()
                .to_string()
                .contains("overlapping")
        );

        let prefix_collision = Config {
            source_folders: vec![source, prefix_sibling],
            ..scanner_config(&root)
        };
        assert!(
            ArchiveScanner::new(&prefix_collision)
                .scan_archives()
                .is_ok(),
            "lexical prefix siblings are distinct roots"
        );
    }

    #[test]
    fn scanner_rejects_relative_and_filesystem_root_sources() {
        for source in [PathBuf::from("relative/source"), PathBuf::from("/")] {
            let config = Config {
                source_folders: vec![source],
                mount_root: PathBuf::from("/tmp/archivefs-test-mounts"),
                ratarmount_bin: "ratarmount".to_string(),
                master_rom_root: None,
            };
            let error = ArchiveScanner::new(&config).scan_archives().unwrap_err();
            assert!(error.to_string().contains("absolute non-root"));
        }
    }

    #[test]
    fn scanner_enforces_a_bounded_directory_depth() {
        let root = test_root("scanner_depth_limit");
        let mut directory = root.clone();
        for _ in 0..=128 {
            directory.push("d");
            fs::create_dir(&directory).unwrap();
        }

        let error = ArchiveScanner::new(&scanner_config(&root))
            .scan_archives()
            .unwrap_err();

        assert!(error.to_string().contains("directory depth limit"));
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

    #[cfg(unix)]
    #[test]
    fn scanner_preserves_a_programmatic_non_utf8_source_root_losslessly() {
        use std::os::unix::ffi::OsStringExt;

        let root = test_root("scanner_non_utf8_source");
        let source = root.join(std::ffi::OsString::from_vec(vec![b's', 0x80, b'c']));
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Game.zip"), b"contents").unwrap();

        let archives = ArchiveScanner::new(&scanner_config(&source))
            .scan_archives()
            .unwrap();

        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].identity.source_root, source);
        assert_eq!(archives[0].path, source.join("Game.zip"));
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
    fn doctor_reports_a_broken_symlink_config_path_as_ambiguous_not_confirmed_missing() {
        // A broken symlink at the config path is not "nothing here" - the
        // wording must differ from the confirmed-missing case so Doctor's
        // Finding adapter never mistakes it for an ordinary first run.
        let root = test_root("doctor_broken_symlink_config");
        let config_path = root.join("config.toml");
        std::os::unix::fs::symlink(root.join("nonexistent-target.toml"), &config_path).unwrap();

        let report = run_doctor(&config_path);

        assert!(!report.is_ready());
        let check = report
            .checks
            .iter()
            .find(|check| check.name == "config file")
            .expect("a config file check is reported");
        assert_eq!(check.status, DoctorStatus::Fail);
        assert!(
            !check.detail.starts_with("missing "),
            "a broken symlink must not be worded as confirmed-missing: {}",
            check.detail
        );
        assert!(
            check.detail.contains("cannot be inspected safely"),
            "{}",
            check.detail
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

    /// Reproduces the exact live-Nobara symptom reported: a mount root that
    /// *exists* (so the old "mount root" check alone reported Pass, and
    /// `DoctorReport::is_ready()` - what the Library page's "Doctor: Ready"
    /// summary reads - agreed) but is not writable by the user actually
    /// running EmuWiz, while `run_setup_diagnostics_with_checks` (the
    /// separate check that actually gates Mount/Unmount via
    /// `SetupDiagnostics.ready_for_actions`) already correctly failed on
    /// the same directory. Before the new "mount root writable" check
    /// added alongside this test, Doctor could say "Ready" while Mount
    /// stayed disabled with no visible explanation for why the two
    /// disagreed. This pins `is_ready()` to `false` in that exact state,
    /// closing the gap.
    #[cfg(unix)]
    #[test]
    fn doctor_mount_root_exists_but_unwritable_is_not_reported_ready() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root("doctor_unwritable_mount_root");
        let source_root = root.join("roms");
        let mount_root = root.join("mounts");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&mount_root).unwrap();
        fs::write(&ratarmount, b"").unwrap();
        fs::set_permissions(&mount_root, fs::Permissions::from_mode(0o555)).unwrap();

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

        // Restore write access so `test_root`'s next-run cleanup (and this
        // process's own tempdir) is never left behind unremovable.
        fs::set_permissions(&mount_root, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "mount root" && check.status == DoctorStatus::Pass),
            "the mount root does exist, so the existence check must still pass"
        );
        assert!(
            report.checks.iter().any(|check| {
                check.name == "mount root writable" && check.status == DoctorStatus::Fail
            }),
            "an existing but unwritable mount root must be reported as such"
        );
        assert!(
            !report.is_ready(),
            "Doctor must not report Ready while the mount root is unwritable - this is exactly \
             what let \"Doctor: Ready\" contradict the real Mount/Unmount gate \
             (SetupDiagnostics.ready_for_actions) with no visible explanation"
        );
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
            "config.toml.example must parse with the real EmuWiz config parser - \
             if you changed the parser or the example, keep both in sync",
        );

        // The shipped example intentionally ships with zero sources
        // configured (multi-source milestone: the installer/first-run
        // flow no longer hardcodes one permanent source folder) - a
        // config with no sources loads and runs fine; see
        // `a_config_with_zero_sources_is_valid_not_an_error`.
        assert!(config.source_folders.is_empty());
        assert_eq!(config.mount_root, PathBuf::from("/mnt/archivefs"));
        assert_eq!(config.ratarmount_bin, "ratarmount");
        assert_eq!(config.master_rom_root, None);
    }

    #[test]
    fn master_rom_root_round_trips_and_survives_source_saves() {
        let root = test_root("master_rom_root_round_trip");
        let config_path = root.join("config.toml");
        let mount_root = root.join("mounts");
        fs::write(
            &config_path,
            format!(
                "mount_root = {}\n",
                quote_config_string(&mount_root.display().to_string())
            ),
        )
        .unwrap();

        let roms = root.join("roms");
        assert_eq!(set_master_rom_root_to(&config_path, &roms).unwrap(), None);
        assert_eq!(
            Config::load_from(&config_path).unwrap().master_rom_root,
            Some(roms.clone())
        );

        // A source-management save (the only other writer of config.toml)
        // must preserve the configured master root, not drop it.
        let sources = load_source_folder_configs_from(&config_path).unwrap();
        let config = Config::load_from(&config_path).unwrap();
        save_source_folder_configs_to(
            &config_path,
            &sources,
            &config.mount_root,
            &config.ratarmount_bin,
            config.master_rom_root.as_deref(),
        )
        .unwrap();
        assert_eq!(
            Config::load_from(&config_path).unwrap().master_rom_root,
            Some(roms.clone())
        );

        assert_eq!(
            clear_master_rom_root_to(&config_path).unwrap(),
            Some(roms.clone())
        );
        assert_eq!(
            Config::load_from(&config_path).unwrap().master_rom_root,
            None
        );
    }

    #[test]
    fn a_relative_or_root_master_rom_root_is_rejected() {
        let root = test_root("master_rom_root_reject");
        let config_path = root.join("config.toml");
        let mount_root = root.join("mounts");
        fs::write(
            &config_path,
            format!(
                "mount_root = {}\n",
                quote_config_string(&mount_root.display().to_string())
            ),
        )
        .unwrap();
        assert!(set_master_rom_root_to(&config_path, Path::new("relative/roms")).is_err());
        assert!(set_master_rom_root_to(&config_path, Path::new("/")).is_err());
        assert!(
            Config::load_from(&config_path)
                .unwrap()
                .master_rom_root
                .is_none(),
            "a rejected value must not be persisted"
        );
    }

    #[test]
    fn legacy_source_folders_config_migrates_to_one_enabled_source_in_memory() {
        let contents = "source_folders = [\"/data/archives\"]\nmount_root = \"/mnt/archivefs\"\n";

        let config = parse_config(contents).unwrap();
        let sources = parse_source_folder_configs(contents).unwrap();

        assert_eq!(config.source_folders, vec![PathBuf::from("/data/archives")]);
        assert_eq!(
            sources,
            vec![SourceFolderConfig {
                path: PathBuf::from("/data/archives"),
                enabled: true,
                created_at: None,
            }]
        );
    }

    #[test]
    fn structured_source_blocks_round_trip_through_parse_and_render() {
        let root = test_root("structured_source_round_trip");
        let mount_root = root.join("mounts");
        let contents = format!(
            "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n\
             [[source]]\npath = \"/data/archives\"\nenabled = true\ncreated_at = \"2026-01-01T00:00:00Z\"\n\n\
             [[source]]\npath = \"/mnt/other\"\nenabled = false\n",
            mount_root.display()
        );

        let sources = parse_source_folder_configs(&contents).unwrap();
        assert_eq!(
            sources,
            vec![
                SourceFolderConfig {
                    path: PathBuf::from("/data/archives"),
                    enabled: true,
                    created_at: Some("2026-01-01T00:00:00Z".to_string()),
                },
                SourceFolderConfig {
                    path: PathBuf::from("/mnt/other"),
                    enabled: false,
                    created_at: None,
                },
            ]
        );

        // A disabled source must never appear in the enabled-only Config
        // view every other part of the codebase relies on.
        let config = parse_config(&contents).unwrap();
        assert_eq!(config.source_folders, vec![PathBuf::from("/data/archives")]);

        let rendered = render_source_folder_configs(&sources, &mount_root, "ratarmount", None);
        let round_tripped = parse_source_folder_configs(&rendered).unwrap();
        assert_eq!(
            round_tripped, sources,
            "rendering then re-parsing must be lossless"
        );
    }

    #[test]
    fn a_config_with_zero_sources_is_valid_not_an_error() {
        let contents = "mount_root = \"/mnt/archivefs\"\n";

        let config = parse_config(contents).unwrap();

        assert!(
            config.source_folders.is_empty(),
            "a fresh install with no sources configured yet must still load successfully"
        );
    }

    #[test]
    fn save_source_folder_configs_is_atomic_and_leaves_no_temp_file_behind() {
        let root = test_root("save_source_folder_configs_atomic");
        let config_path = root.join("config.toml");
        let mount_root = root.join("mounts");
        let sources = vec![SourceFolderConfig {
            path: root.join("archives"),
            enabled: true,
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
        }];

        save_source_folder_configs_to(&config_path, &sources, &mount_root, "ratarmount", None)
            .unwrap();

        let loaded = load_source_folder_configs_from(&config_path).unwrap();
        assert_eq!(loaded, sources);
        let loaded_config = Config::load_from(&config_path).unwrap();
        assert_eq!(loaded_config.mount_root, mount_root);

        let leftover_temp_files = fs::read_dir(&root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(
            leftover_temp_files, 0,
            "atomic write must not leave a temp file behind"
        );
    }

    #[test]
    fn save_source_folder_configs_never_loses_an_existing_enabled_source() {
        let root = test_root("save_source_folder_configs_preserves");
        let config_path = root.join("config.toml");
        let archives_dir = root.join("Archives");
        fs::create_dir_all(&archives_dir).unwrap();
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\n",
                archives_dir.display(),
                root.join("mounts").display()
            ),
        )
        .unwrap();

        // Simulate the app's first source-management mutation: load the
        // legacy config, then immediately save it back unchanged.
        let sources = load_source_folder_configs_from(&config_path).unwrap();
        let config = Config::load_from(&config_path).unwrap();
        save_source_folder_configs_to(
            &config_path,
            &sources,
            &config.mount_root,
            &config.ratarmount_bin,
            config.master_rom_root.as_deref(),
        )
        .unwrap();

        let reloaded = Config::load_from(&config_path).unwrap();
        assert_eq!(reloaded.source_folders, vec![archives_dir]);
    }

    #[test]
    fn validate_new_source_folder_rejects_a_missing_directory() {
        let root = test_root("validate_missing");
        let missing = root.join("does-not-exist");

        let error = validate_new_source_folder(&missing, &[]).unwrap_err();

        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn validate_new_source_folder_rejects_a_file_not_a_directory() {
        let root = test_root("validate_not_a_dir");
        let file_path = root.join("not-a-dir");
        fs::write(&file_path, b"x").unwrap();

        let error = validate_new_source_folder(&file_path, &[]).unwrap_err();

        assert!(error.to_string().contains("not a directory"));
    }

    #[test]
    fn validate_new_source_folder_rejects_an_exact_duplicate() {
        let root = test_root("validate_exact_duplicate");
        let source = root.join("archives");
        fs::create_dir_all(&source).unwrap();

        let error = validate_new_source_folder(&source, std::slice::from_ref(&source)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("already a configured source folder")
        );
    }

    #[test]
    fn validate_new_source_folder_rejects_a_trailing_separator_duplicate() {
        let root = test_root("validate_trailing_separator_duplicate");
        let source = root.join("archives");
        fs::create_dir_all(&source).unwrap();
        let with_trailing_slash = PathBuf::from(format!("{}/", source.display()));

        let error = validate_new_source_folder(&with_trailing_slash, std::slice::from_ref(&source))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("already a configured source folder")
        );
    }

    #[test]
    fn validate_new_source_folder_rejects_a_child_of_an_existing_source() {
        let root = test_root("validate_overlap_child");
        let parent = root.join("archives");
        let child = parent.join("nested");
        fs::create_dir_all(&child).unwrap();

        let error = validate_new_source_folder(&child, &[parent]).unwrap_err();

        assert!(error.to_string().contains("overlapping"));
    }

    #[test]
    fn validate_new_source_folder_rejects_a_parent_of_an_existing_source() {
        let root = test_root("validate_overlap_parent");
        let parent = root.join("archives");
        let child = parent.join("nested");
        fs::create_dir_all(&child).unwrap();

        let error = validate_new_source_folder(&parent, &[child]).unwrap_err();

        assert!(error.to_string().contains("overlapping"));
    }

    #[test]
    fn validate_new_source_folder_accepts_a_genuinely_new_sibling_directory() {
        let root = test_root("validate_new_sibling");
        let existing = root.join("archives-a");
        let new_source = root.join("archives-b");
        fs::create_dir_all(&existing).unwrap();
        fs::create_dir_all(&new_source).unwrap();

        let validated = validate_new_source_folder(&new_source, &[existing]).unwrap();

        assert_eq!(validated, new_source);
    }

    #[cfg(unix)]
    #[test]
    fn source_config_save_rejects_non_utf8_without_replacing_existing_config() {
        use std::os::unix::ffi::OsStringExt;

        let root = test_root("source_config_non_utf8");
        let config_path = root.join("config.toml");
        fs::write(&config_path, b"mount_root = \"/safe/existing\"\n").unwrap();
        let before = fs::read(&config_path).unwrap();
        let source = SourceFolderConfig {
            path: root.join(std::ffi::OsString::from_vec(vec![b's', 0x80, b'c'])),
            enabled: true,
            created_at: None,
        };

        let error = save_source_folder_configs_to(
            &config_path,
            &[source],
            &root.join("mounts"),
            "ratarmount",
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("losslessly"));
        assert_eq!(fs::read(&config_path).unwrap(), before);
    }

    fn write_starter_config(config_path: &Path, mount_root: &Path) {
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                mount_root.display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn add_source_folder_persists_immediately_and_is_visible_without_a_scan() {
        let root = test_root("add_source_folder_immediate");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let archives_dir = root.join("archives");
        fs::create_dir_all(&archives_dir).unwrap();

        let added = add_source_folder_at(&config_path, &database_path, &archives_dir).unwrap();
        assert_eq!(added.path, archives_dir);
        assert!(added.enabled);
        assert!(added.created_at.is_some());

        let database_before = fs::read(&database_path).unwrap();
        let modified_before = fs::metadata(&database_path).unwrap().modified().unwrap();

        let views = list_source_folder_views_at(&config_path, &database_path).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].path, archives_dir);
        assert!(
            views[0].id.is_some(),
            "adding a source must assign it a stable id immediately, without scanning"
        );
        assert_eq!(views[0].last_archive_count, None, "adding must never scan");
        assert_eq!(views[0].availability, SourceAvailability::Available);
        assert_eq!(fs::read(&database_path).unwrap(), database_before);
        assert_eq!(
            fs::metadata(&database_path).unwrap().modified().unwrap(),
            modified_before
        );
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut sidecar = database_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            assert!(!PathBuf::from(sidecar).exists());
        }
    }

    #[test]
    fn add_source_folder_rejects_an_overlap_through_the_orchestration_layer() {
        let root = test_root("add_source_folder_overlap");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let parent = root.join("archives");
        let child = parent.join("nested");
        fs::create_dir_all(&child).unwrap();

        add_source_folder_at(&config_path, &database_path, &parent).unwrap();
        let error = add_source_folder_at(&config_path, &database_path, &child).unwrap_err();

        assert!(error.to_string().contains("overlapping"));
        let views = list_source_folder_views_at(&config_path, &database_path).unwrap();
        assert_eq!(views.len(), 1, "a rejected add must not partially persist");
    }

    #[test]
    fn disabling_a_source_excludes_it_from_scan_all_but_preserves_its_catalogue() {
        let root = test_root("disable_source_preserves_catalogue");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        fs::create_dir_all(&source_a).unwrap();
        fs::create_dir_all(&source_b).unwrap();
        fs::write(source_a.join("a.zip"), b"a").unwrap();
        fs::write(source_b.join("b.zip"), b"b").unwrap();
        add_source_folder_at(&config_path, &database_path, &source_a).unwrap();
        add_source_folder_at(&config_path, &database_path, &source_b).unwrap();
        scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();

        let outcome =
            set_source_folder_enabled_at(&config_path, &database_path, &source_a, false).unwrap();
        assert!(!outcome.source.enabled);
        assert!(outcome.scan.is_none(), "disabling must never scan");

        let config = Config::load_from(&config_path).unwrap();
        assert_eq!(
            config.source_folders,
            vec![source_b.clone()],
            "a disabled source must be excluded from the enabled-only Config view"
        );

        let summary = scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();
        assert_eq!(
            summary.counts.source_folders_scanned, 1,
            "Scan All must skip the disabled source entirely"
        );

        let views = list_source_folder_views_at(&config_path, &database_path).unwrap();
        let disabled_view = views.iter().find(|view| view.path == source_a).unwrap();
        assert_eq!(disabled_view.availability, SourceAvailability::Disabled);
        assert_eq!(
            disabled_view.last_archive_count,
            Some(1),
            "disabling must preserve the last known archive count, not reset it"
        );
    }

    #[test]
    fn enabling_a_source_scans_it_automatically() {
        let root = test_root("enable_source_scans_automatically");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let source_a = root.join("source-a");
        fs::create_dir_all(&source_a).unwrap();
        fs::write(source_a.join("a.zip"), b"a").unwrap();
        add_source_folder_at(&config_path, &database_path, &source_a).unwrap();
        set_source_folder_enabled_at(&config_path, &database_path, &source_a, false).unwrap();

        let outcome =
            set_source_folder_enabled_at(&config_path, &database_path, &source_a, true).unwrap();

        assert!(outcome.source.enabled);
        let scan = outcome.scan.expect(
            "re-enabling a source must trigger a scan before live actions become available",
        );
        assert_eq!(scan.counts.source_folders_scanned, 1);
        assert_eq!(scan.counts.archives_added, 1);
    }

    #[test]
    fn removing_a_source_with_keep_catalogue_never_touches_its_archive_rows() {
        let root = test_root("remove_source_keep_catalogue");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let source_a = root.join("source-a");
        fs::create_dir_all(&source_a).unwrap();
        fs::write(source_a.join("a.zip"), b"a").unwrap();
        add_source_folder_at(&config_path, &database_path, &source_a).unwrap();
        scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();

        let outcome =
            remove_source_folder_at(&config_path, &database_path, &source_a, true).unwrap();

        assert_eq!(outcome.catalogue_rows_removed, None);
        let config = Config::load_from(&config_path).unwrap();
        assert!(config.source_folders.is_empty());
        let database = Database::open_or_create(&database_path).unwrap();
        assert_eq!(
            database.load_archives().unwrap().len(),
            1,
            "removing configuration must never delete a catalogue row by default"
        );
        assert!(
            source_a.exists(),
            "removing a source must never touch the filesystem"
        );
        assert!(source_a.join("a.zip").exists());
    }

    #[test]
    fn removing_a_source_with_explicit_catalogue_removal_is_exact_and_scoped() {
        let root = test_root("remove_source_delete_catalogue");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        fs::create_dir_all(&source_a).unwrap();
        fs::create_dir_all(&source_b).unwrap();
        fs::write(source_a.join("a1.zip"), b"1").unwrap();
        fs::write(source_a.join("a2.zip"), b"2").unwrap();
        fs::write(source_b.join("b.zip"), b"b").unwrap();
        add_source_folder_at(&config_path, &database_path, &source_a).unwrap();
        add_source_folder_at(&config_path, &database_path, &source_b).unwrap();
        scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();

        let outcome =
            remove_source_folder_at(&config_path, &database_path, &source_a, false).unwrap();

        assert_eq!(outcome.catalogue_rows_removed, Some(2));
        let database = Database::open_or_create(&database_path).unwrap();
        let remaining = database.load_archives().unwrap();
        assert_eq!(remaining.len(), 1, "only source_a's rows must be removed");
        assert_eq!(remaining[0].relative_path, PathBuf::from("b.zip"));
        assert!(
            source_a.exists(),
            "catalogue removal must never touch the filesystem"
        );
        assert!(source_a.join("a1.zip").exists());
        assert!(source_a.join("a2.zip").exists());
    }

    #[test]
    fn classify_source_availability_covers_all_five_states() {
        assert_eq!(
            classify_source_availability(false, Some(SourceScanStatus::Success), None),
            SourceAvailability::Disabled,
            "disabled always wins regardless of scan history"
        );
        assert_eq!(
            classify_source_availability(true, None, None),
            SourceAvailability::Available
        );
        assert_eq!(
            classify_source_availability(true, Some(SourceScanStatus::Success), None),
            SourceAvailability::Available
        );
        assert_eq!(
            classify_source_availability(
                true,
                Some(SourceScanStatus::Failed),
                Some("/mnt/x: Permission denied (os error 13)")
            ),
            SourceAvailability::PermissionDenied
        );
        assert_eq!(
            classify_source_availability(
                true,
                Some(SourceScanStatus::Failed),
                Some("/mnt/x: No such file or directory (os error 2)")
            ),
            SourceAvailability::Unavailable
        );
        assert_eq!(
            classify_source_availability(
                true,
                Some(SourceScanStatus::Failed),
                Some("/mnt/x: some other unexpected I/O error")
            ),
            SourceAvailability::ScanFailed
        );
    }

    /// A single view-builder helper for the tests below, so each test only
    /// states the one or two fields it actually cares about.
    fn source_view(
        path: &str,
        availability: SourceAvailability,
        archive_count: i64,
    ) -> SourceFolderView {
        SourceFolderView {
            path: PathBuf::from(path),
            enabled: !matches!(availability, SourceAvailability::Disabled),
            created_at: None,
            id: Some(1),
            availability,
            last_scan_status: None,
            last_scan_error: Some("boom".to_string()),
            last_scan_at: None,
            last_successful_scan_at: None,
            last_archive_count: Some(archive_count),
            assigned_platform: None,
            unknown_archive_count: 0,
        }
    }

    #[test]
    fn source_health_issues_reports_exactly_one_issue_per_unavailable_source_never_per_archive() {
        // 1,242 preserved archives under one offline source must still
        // surface as exactly one `SourceHealthIssue`, not 1,242 - this is
        // the milestone's core anti-flood guarantee.
        let views = vec![source_view(
            "/mnt/usbdrive/retro",
            SourceAvailability::Unavailable,
            1242,
        )];
        let issues = source_health_issues(&views);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, PathBuf::from("/mnt/usbdrive/retro"));
        assert_eq!(issues[0].archives_preserved, 1242);
        assert!(issues[0].reason.contains("preserved"));
    }

    #[test]
    fn source_health_issues_covers_permission_denied_and_scan_failed_with_distinct_wording() {
        let views = vec![
            source_view(
                "/mnt/nvme2/collections",
                SourceAvailability::PermissionDenied,
                0,
            ),
            source_view("/mnt/broken", SourceAvailability::ScanFailed, 40),
        ];
        let issues = source_health_issues(&views);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].reason.to_lowercase().contains("permission"));
        assert!(issues[1].reason.to_lowercase().contains("scan"));
    }

    #[test]
    fn source_health_issues_never_reports_available_or_disabled_sources() {
        let views = vec![
            source_view("/home/davedap/Archives", SourceAvailability::Available, 500),
            source_view("/mnt/games/roms", SourceAvailability::Disabled, 10),
        ];
        assert!(source_health_issues(&views).is_empty());
    }

    #[test]
    fn resolve_source_folder_identifier_accepts_both_id_and_path() {
        let sources = vec![SourceFolderConfig {
            path: PathBuf::from("/data/archives"),
            enabled: true,
            created_at: None,
        }];
        let records = vec![SourceFolderRecord {
            id: 7,
            path: PathBuf::from("/data/archives"),
            first_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_scan_status: None,
            last_scan_error: None,
            last_scan_at: None,
            last_successful_scan_at: None,
            last_archive_count: None,
            assigned_platform: None,
            unknown_archive_count: 0,
        }];

        assert_eq!(
            resolve_source_folder_identifier("7", &sources, &records).unwrap(),
            PathBuf::from("/data/archives")
        );
        assert_eq!(
            resolve_source_folder_identifier("/data/archives", &sources, &records).unwrap(),
            PathBuf::from("/data/archives")
        );
        assert!(resolve_source_folder_identifier("99", &sources, &records).is_err());
        assert!(resolve_source_folder_identifier("/no/such/path", &sources, &records).is_err());
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
    fn setup_diagnostics_treats_genuinely_missing_config_as_not_configured_not_error() {
        // A brand-new install with no config file at all: nothing is
        // broken, so nothing downstream of the missing file should read as
        // a red "Error" - only ratarmount/unmount tool availability (a fact
        // about the machine, not the config) may still be genuinely Error.
        let root = test_root("setup_diagnostics_first_run");
        let config_path = root.join("config.toml");

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.config_missing);
        assert!(!report.ready_for_scanning);
        assert!(!report.ready_for_actions);
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status != SetupDiagnosticStatus::Error),
            "expected no Error-status checks for a genuinely missing config, got: {:#?}",
            report.checks
        );
        assert!(report.checks.iter().any(|check| {
            check.name == "Config file exists"
                && check.status == SetupDiagnosticStatus::NotConfigured
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "EmuWiz is ready for scanning"
                && check.status == SetupDiagnosticStatus::NotConfigured
        }));
    }

    #[test]
    fn setup_diagnostics_missing_config_still_flags_genuinely_absent_tools_as_error() {
        // The softening for a missing config must not swallow a real,
        // independent environment problem: if ratarmount or an unmount
        // tool is genuinely absent, that stays Error even on first run.
        let root = test_root("setup_diagnostics_first_run_no_tools");
        let config_path = root.join("config.toml");

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| false);

        assert!(report.config_missing);
        assert!(report.checks.iter().any(|check| {
            check.name == "ratarmount is available" && check.status == SetupDiagnosticStatus::Error
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "fusermount3 or umount is available"
                && check.status == SetupDiagnosticStatus::Error
        }));
    }

    #[test]
    fn setup_diagnostics_malformed_existing_config_remains_an_error() {
        // Distinct from a missing file: an existing-but-malformed config is
        // a real problem and must never be softened to NotConfigured.
        let root = test_root("setup_diagnostics_malformed");
        let config_path = root.join("config.toml");
        fs::write(&config_path, "this is not valid toml [[[").unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(!report.config_missing);
        assert!(report.checks.iter().any(|check| {
            check.name == "Config parses successfully"
                && check.status == SetupDiagnosticStatus::Error
        }));
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
            true,
        );
        assert!(missing.config_missing);

        let ambiguous = run_setup_diagnostics_with_checks(
            config_path,
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
            |_| PathInspection::PermissionDenied("permission denied".to_string()),
            |_| false,
            true,
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
            true,
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
            true,
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
            true,
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

    /// Reproduces the reported live-Nobara bug: `run_setup_diagnostics_
    /// with_checks` used to read `ConfigFields.source_folders` (the raw
    /// legacy key) directly, so a config using only the newer `[[source]]`
    /// block format reported "no usable source folder" and stayed
    /// permanently not-ready-for-scanning/-actions, even though the
    /// Sources page and Scan All (both built on `parse_source_folder_
    /// configs`) correctly saw every structured source.
    #[test]
    fn structured_source_config_with_one_enabled_available_source_is_action_ready() {
        let root = test_root("setup_structured_one_source");
        let source = root.join("Archives");
        let mount_root = root.join("mounts");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n[[source]]\npath = \"{}\"\nenabled = true\n",
                mount_root.display(),
                source.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(
            report.checks.iter().any(|check| check.name
                == "At least one source folder is configured"
                && check.status == SetupDiagnosticStatus::Ready),
            "a single enabled, available [[source]] must count as a configured source folder"
        );
        assert!(report.ready_for_scanning);
        assert!(report.ready_for_actions);
    }

    #[test]
    fn three_structured_sources_are_recognised_by_setup_diagnostics() {
        // Mirrors the exact live-Nobara report: three structured sources,
        // all enabled and available.
        let root = test_root("setup_structured_three_sources");
        let archives = root.join("Archives");
        let games = root.join("Games");
        let desktop_games = root.join("Desktop").join("GAMES");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&archives).unwrap();
        fs::create_dir_all(&games).unwrap();
        fs::create_dir_all(&desktop_games).unwrap();
        fs::create_dir_all(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n",
                mount_root.display(),
                archives.display(),
                games.display(),
                desktop_games.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.status == SetupDiagnosticStatus::Ready
                && check.detail == "3 source folder(s) configured."
        }));
        assert_eq!(
            report
                .checks
                .iter()
                .filter(|check| check.name == "Configured source folder exists")
                .count(),
            3,
            "every structured source must get its own existence check, not just the legacy list"
        );
        assert!(report.ready_for_scanning);
        assert!(report.ready_for_actions);
    }

    #[test]
    fn legacy_source_folders_setup_diagnostics_remain_compatible() {
        // Requirement 3: the fix must not regress the pre-multi-source
        // config format - already exercised end to end by
        // `ready_setup_diagnostics_distinguish_scanning_and_actions`, this
        // pins the specific "at least one source folder is configured"
        // check's Ready status for a legacy config too.
        let root = test_root("setup_legacy_compat");
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

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.status == SetupDiagnosticStatus::Ready
        }));
        assert!(report.ready_for_scanning);
        assert!(report.ready_for_actions);
    }

    #[test]
    fn zero_sources_config_is_valid_but_not_action_ready() {
        // Requirement 4: an empty/absent source list must remain a valid,
        // startable config (no config-read/parse error), just not ready
        // to scan or mount.
        let root = test_root("setup_zero_sources");
        let mount_root = root.join("mounts");
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                mount_root.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.checks.iter().any(|check| {
            check.name == "Config file exists" && check.status == SetupDiagnosticStatus::Ready
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "Config parses successfully"
                && check.status == SetupDiagnosticStatus::Ready
        }));
        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.status != SetupDiagnosticStatus::Ready
        }));
        assert!(!report.ready_for_scanning);
        assert!(!report.ready_for_actions);
    }

    #[test]
    fn only_disabled_structured_sources_is_not_action_ready() {
        // Requirement 5: disabled sources alone must not count as usable,
        // even though they are genuinely present in the config - mirrors
        // `parse_config`'s own enabled-only filtering for `Config.
        // source_folders`.
        let root = test_root("setup_only_disabled_sources");
        let source = root.join("Archives");
        let mount_root = root.join("mounts");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n[[source]]\npath = \"{}\"\nenabled = false\n",
                mount_root.display(),
                source.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.status != SetupDiagnosticStatus::Ready
        }));
        assert!(!report.ready_for_scanning);
        assert!(!report.ready_for_actions);
    }

    #[test]
    fn missing_structured_source_keeps_truthful_diagnostics() {
        // Requirement 6: a source that exists in config but not on disk
        // must still be reported as missing, never silently treated as
        // usable just because it is enabled and present in the file.
        let root = test_root("setup_missing_structured_source");
        let missing_source = root.join("does-not-exist");
        let mount_root = root.join("mounts");
        fs::create_dir(&mount_root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n[[source]]\npath = \"{}\"\nenabled = true\n",
                mount_root.display(),
                missing_source.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.status == SetupDiagnosticStatus::Ready
        }));
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "Configured source folder exists"
                    && check.status != SetupDiagnosticStatus::Ready),
            "an enabled but missing source must still fail its own existence check"
        );
        assert!(
            !report.ready_for_scanning,
            "a missing enabled source must block scanning even though it is configured"
        );
        assert!(!report.ready_for_actions);
    }

    #[test]
    fn setup_diagnostics_sources_match_scan_all_sources() {
        // Requirement 1/9: SetupDiagnostics and Scan All must agree on
        // exactly which sources are "configured and enabled" - both now
        // read `parse_source_folder_configs`/`load_source_folder_configs_
        // from`, never a second, independently-drifting list.
        let root = test_root("setup_matches_scan_all");
        let archives = root.join("Archives");
        let games = root.join("Games");
        let disabled = root.join("Old");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&archives).unwrap();
        fs::create_dir_all(&games).unwrap();
        fs::create_dir_all(&disabled).unwrap();
        fs::create_dir_all(&mount_root).unwrap();
        fs::write(archives.join("Game.zip"), b"").unwrap();
        fs::write(games.join("Other.zip"), b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = false\n",
                mount_root.display(),
                archives.display(),
                games.display(),
                disabled.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);
        assert!(report.ready_for_scanning);
        assert!(report.ready_for_actions);
        assert!(report.checks.iter().any(|check| {
            check.name == "At least one source folder is configured"
                && check.detail == "2 source folder(s) configured."
        }));

        let database_path = root.join("library.sqlite3");
        let summary = scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();
        assert!(
            summary.folder_errors.is_empty(),
            "both enabled sources must scan without error"
        );
        assert_eq!(
            summary.counts.source_folders_scanned, 2,
            "Scan All must attempt exactly the same 2 enabled sources SetupDiagnostics counted"
        );
        assert_eq!(summary.counts.archives_added, 2);
    }

    #[test]
    fn exact_nobara_state_makes_a_present_pending_archive_action_ready() {
        // End-to-end reproduction of the reported live state: three
        // enabled, available structured sources, a valid writable mount
        // root, ratarmount/unmount tools present - `ready_for_actions`
        // must be true, which is exactly what makes
        // `archive_action_block_reason` return `None` (Mount available)
        // for a present, live, Pending archive - see
        // `present_live_selected_archive_enables_mount` in
        // `archivefs-gui` for the GUI-side half of this same guarantee.
        let root = test_root("setup_exact_nobara_state");
        let archives = root.join("Archives");
        let games = root.join("Games");
        let desktop_games = root.join("Desktop").join("GAMES");
        let mount_root = root.join("mounts");
        fs::create_dir_all(&archives).unwrap();
        fs::create_dir_all(&games).unwrap();
        fs::create_dir_all(&desktop_games).unwrap();
        fs::create_dir_all(&mount_root).unwrap();
        fs::write(archives.join("Game.zip"), b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n",
                mount_root.display(),
                archives.display(),
                games.display(),
                desktop_games.display()
            ),
        )
        .unwrap();

        let report = run_setup_diagnostics_with_command_check(&config_path, |_| true);

        for name in [
            "Config file exists",
            "Config parses successfully",
            "At least one source folder is configured",
            "mount_root is configured",
            "Mount root exists or can be created safely",
            "Mount root is writable",
            "ratarmount is available",
            "fusermount3 or umount is available",
            "EmuWiz is ready for scanning",
            "EmuWiz is ready for mount/unmount actions",
        ] {
            assert!(
                report
                    .checks
                    .iter()
                    .any(|check| check.name == name && check.status == SetupDiagnosticStatus::Ready),
                "expected {name:?} to be Ready in the exact reproduced Nobara state"
            );
        }
        assert!(report.ready_for_scanning);
        assert!(report.ready_for_actions);
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
        active: std::cell::RefCell<HashSet<PathBuf>>,
    }

    impl RecordingBackend {
        fn with_active(active: HashSet<PathBuf>) -> Self {
            Self {
                active: std::cell::RefCell::new(active),
                ..Self::default()
            }
        }

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
            self.active.borrow_mut().remove(mount_path);
            Ok(())
        }

        fn active_mount_paths(&self, _root: &Path) -> Result<HashSet<PathBuf>> {
            Ok(self.active.borrow().clone())
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

    fn persisted_duplicate_archive(
        id: i64,
        path: &str,
        platform: Option<&str>,
        present: bool,
        size_bytes: Option<u64>,
    ) -> PersistedArchive {
        persisted_duplicate_archive_path(id, PathBuf::from(path), platform, present, size_bytes)
    }

    fn persisted_duplicate_archive_path(
        id: i64,
        path: PathBuf,
        platform: Option<&str>,
        present: bool,
        size_bytes: Option<u64>,
    ) -> PersistedArchive {
        PersistedArchive {
            id,
            source_folder_id: 1,
            relative_path: path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.clone()),
            absolute_path: path.clone(),
            archive_kind: path
                .extension()
                .map(|extension| extension.to_string_lossy().into_owned())
                .unwrap_or_default(),
            display_name: archive_title(&path),
            normalized_name: duplicate_normalized_name(&path),
            size_bytes,
            modified_time_unix_seconds: Some(1_700_000_000),
            platform: platform.map(str::to_string),
            platform_source: platform.map(|_| "heuristic-path-detector".to_string()),
            last_known_health: "Pending".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_verified_missing_at: (!present).then(|| "2026-01-02T00:00:00Z".to_string()),
            identity_report: None,
        }
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
