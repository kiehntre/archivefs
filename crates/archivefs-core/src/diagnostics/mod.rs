//! One shared, read-only diagnostic finding model for Doctor.
//!
//! ArchiveFS already contains a large diagnostic engine, but every
//! subsystem grew its own finding type, its own severity scale, and its own
//! UI surface. This module adds exactly one thing:
//! a shared [`Finding`] plus thin adapters that translate the *existing*
//! reports into it. It rewrites no check and owns no check of its own.
//!
//! ## Read-only contract
//!
//! Stage 1A is strictly diagnostic. Nothing here repairs, and nothing here
//! mutates. The contract is split deliberately into two halves so the
//! guarantee is provable rather than asserted:
//!
//! - **Gatherers** ([`assess_mount_root_safety`], and the callers'
//!   existing `diagnose_database` / `discover_shared_apply_history` /
//!   `list_source_folder_views_at` calls) perform bounded, read-only I/O. Each
//!   underlying function already documents that it creates nothing - see
//!   `database::diagnose_database`'s "without creating files/directories,
//!   running migrations, changing pragmas, checkpointing WAL, or attempting
//!   recovery", and `patch_manager::destination_safety`'s module doc "never
//!   creates a directory or file".
//!
//!   `run_setup_diagnostics` is deliberately **not** among them: its "Mount
//!   root is writable" check probes by creating and removing a file, which
//!   changes the mount root's modification time. Callers pass a
//!   `SetupDiagnostics` they already computed elsewhere instead, so opening
//!   Doctor never performs that write.
//! - **The runner** ([`runner::run_doctor_scan`]) is a *pure function* over
//!   already-gathered inputs. It performs no I/O at all - no filesystem, no
//!   process, no network. `runner.rs` is guarded by a source-level test
//!   (`runner_source_contains_no_io_or_mutation_calls`) in addition to the
//!   behavioural before/after tests.
//!
//! ## What this module is deliberately not
//!
//! - Not a repair dispatcher. [`KnownRecovery`] is *informational metadata*
//!   describing a repair the user can already perform somewhere else in
//!   ArchiveFS today. It carries no callable, no closure, and no path to
//!   execute against.
//! - Not a replacement for `DoctorReport`, `SetupDiagnostics`,
//!   `HealthIssue`, `DatabaseHealthReport`, or
//!   `emulator_environment::retroarch::Diagnostic`. All of them stay
//!   exactly as they are; these adapters read them.
//! - Not a claim of complete coverage. [`DEFERRED_CHECKS`] names, in the
//!   product itself, every check Stage 1A does not perform, so a clean scan
//!   can never be presented as "everything is fine".

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::emulator_environment::EncodedPath;
use crate::emulator_environment::retroarch::{
    Diagnostic as RetroArchDiagnostic, DiagnosticCategory as RetroArchDiagnosticCategory,
    DiagnosticSeverity as RetroArchDiagnosticSeverity, RetroArchEnvironmentReport,
};
use crate::patch_manager::{
    DestinationRootState, DestinationSafetyError, DestinationSafetyFailureReason,
    SharedApplyStatus, SharedHistoryReport, validate_destination_root,
};
use crate::{
    DoctorCheck, DoctorReport, DoctorStatus, HealthCategory, HealthIssue, RecoveryAction,
    SetupDiagnostic, SetupDiagnosticStatus, SetupDiagnostics, SourceAvailability,
    SourceHealthIssue,
};

pub mod environment;
pub mod managed;
pub mod profiles;
pub mod repair;
pub mod runner;

#[cfg(test)]
mod tests;

pub use runner::{DoctorScan, DoctorScanInputs, FindingLookup, Gathered, run_doctor_scan};

// --- Severity -------------------------------------------------------------

/// The single Doctor-facing severity scale, replacing four parallel ones
/// (`DoctorStatus`, `ConfigCheckStatus`, `SetupDiagnosticStatus`,
/// `DatabaseDiagnosticSeverity`, `retroarch::DiagnosticSeverity`) *for
/// presentation only* - none of those types is changed.
///
/// Mapping is conservative in one direction only: an incoming error is
/// never presented as a warning, and a warning is never presented as info.
/// A small number of database codes are deliberately *escalated* to
/// [`DoctorSeverity::Critical`] because they indicate the user's catalogue
/// may be damaged (see [`database_severity`]).
///
/// [`DoctorSeverity::Healthy`] is reserved for a scan's overall verdict and
/// is never carried by a [`Finding`] - a healthy check produces no finding
/// at all. This is enforced by
/// `no_finding_is_ever_emitted_with_healthy_severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorSeverity {
    Healthy,
    Info,
    Warning,
    Error,
    Critical,
}

impl DoctorSeverity {
    /// Most severe first - the display and sort order.
    pub const ACTIONABLE: [Self; 4] = [Self::Critical, Self::Error, Self::Warning, Self::Info];

    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Info => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::Critical => "Critical",
        }
    }

    /// Lower is more severe - used for deterministic ordering.
    pub fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::Error => 1,
            Self::Warning => 2,
            Self::Info => 3,
            Self::Healthy => 4,
        }
    }

    /// Blocks normal use rather than merely warning about it.
    pub fn is_blocking(self) -> bool {
        matches!(self, Self::Error | Self::Critical)
    }
}

impl fmt::Display for DoctorSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

// --- Category and subsystem ----------------------------------------------

/// The user-facing grouping on the Doctor page. Deliberately coarser than
/// [`DoctorSubsystem`]: a person wants "something is wrong with my mount
/// root", not "the setup-diagnostics module said so".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCategory {
    Configuration,
    /// Free space on the filesystems ArchiveFS depends on.
    Storage,
    /// How those filesystems are mounted.
    Filesystems,
    MountRoot,
    Mounts,
    Sources,
    Library,
    Database,
    Emulators,
    /// Discovered emulator profiles and whether ArchiveFS could write to them.
    EmulatorProfiles,
    /// ArchiveFS-managed cheat and patch entries.
    ManagedEntries,
    Transactions,
    /// Doctor reporting on itself - an adapter that could not run.
    Doctor,
}

impl DoctorCategory {
    /// Display order, stable and exhaustive.
    pub const ALL: [Self; 13] = [
        Self::Configuration,
        Self::Storage,
        Self::Filesystems,
        Self::MountRoot,
        Self::Mounts,
        Self::Sources,
        Self::Library,
        Self::Database,
        Self::Emulators,
        Self::EmulatorProfiles,
        Self::ManagedEntries,
        Self::Transactions,
        Self::Doctor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Configuration => "Configuration",
            Self::Storage => "Storage",
            Self::Filesystems => "Filesystems",
            Self::MountRoot => "Mount root",
            Self::Mounts => "Mounts",
            Self::Sources => "Source folders",
            Self::Library => "Library",
            Self::Database => "Catalogue database",
            Self::Emulators => "Emulators",
            Self::EmulatorProfiles => "Emulator profiles",
            Self::ManagedEntries => "Managed entries",
            Self::Transactions => "Installs and rollbacks",
            Self::Doctor => "Doctor itself",
        }
    }

    fn order(self) -> u8 {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(u8::MAX as usize) as u8
    }
}

impl fmt::Display for DoctorCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Which existing module produced a finding. Kept alongside
/// [`DoctorCategory`] so provenance survives duplicate merging (see
/// [`runner::run_doctor_scan`]) and so a bug report names the real source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorSubsystem {
    /// `crate::run_doctor_read_only` → `DoctorReport`
    DoctorReport,
    /// `crate::run_setup_diagnostics` → `SetupDiagnostics`
    SetupDiagnostics,
    /// `crate::classify_archive_health` → `HealthIssue`
    ArchiveHealth,
    /// `crate::source_health_issues` → `SourceHealthIssue`
    SourceHealth,
    /// `crate::database::diagnose_database` → `DatabaseHealthReport`
    DatabaseDiagnostics,
    /// `patch_manager::destination_safety::validate_destination_root`
    DestinationSafety,
    /// `emulator_environment::retroarch::discover_retroarch_environment`
    RetroArchEnvironment,
    /// `patch_manager::shared_transaction::discover_shared_apply_history`
    SharedTransactions,
    /// `crate::plan_stale_mount_directories`
    MountRootCleanup,
    /// `crate::check_archive_index_freshness`
    ArchiveIndex,
    /// `diagnostics::environment::filesystem_stat` (`statvfs(3)`)
    FilesystemCapacity,
    /// `diagnostics::environment::mount_table` (`/proc/self/mountinfo`)
    FilesystemMountState,
    /// `discover_dolphin_profiles` / `discover_pcsx2_profiles` /
    /// `discover_xenia_profiles`
    EmulatorProfiles,
    /// ArchiveFS-managed cheat and patch entries, anchored on install
    /// journals and on each adapter's own ownership marker.
    ManagedEntries,
    /// The Doctor runner itself.
    DoctorRunner,
}

impl DoctorSubsystem {
    /// Stable, machine-readable identifier fragment. Used to namespace
    /// per-subsystem finding ids; must not change casually.
    pub fn slug(self) -> &'static str {
        match self {
            Self::DoctorReport => "doctor_report",
            Self::SetupDiagnostics => "setup_diagnostics",
            Self::ArchiveHealth => "archive_health",
            Self::SourceHealth => "source_health",
            Self::DatabaseDiagnostics => "database_diagnostics",
            Self::DestinationSafety => "destination_safety",
            Self::RetroArchEnvironment => "retroarch_environment",
            Self::SharedTransactions => "shared_transactions",
            Self::MountRootCleanup => "mount_root_cleanup",
            Self::ArchiveIndex => "archive_index",
            Self::FilesystemCapacity => "filesystem_capacity",
            Self::FilesystemMountState => "filesystem_mount_state",
            Self::EmulatorProfiles => "emulator_profiles",
            Self::ManagedEntries => "managed_entries",
            Self::DoctorRunner => "doctor_runner",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DoctorReport => "doctor report",
            Self::SetupDiagnostics => "setup diagnostics",
            Self::ArchiveHealth => "archive health",
            Self::SourceHealth => "source health",
            Self::DatabaseDiagnostics => "database diagnostics",
            Self::DestinationSafety => "destination safety",
            Self::RetroArchEnvironment => "RetroArch environment",
            Self::SharedTransactions => "install history",
            Self::MountRootCleanup => "mount-root cleanup",
            Self::ArchiveIndex => "archive index",
            Self::FilesystemCapacity => "filesystem capacity",
            Self::FilesystemMountState => "filesystem mount state",
            Self::EmulatorProfiles => "emulator profiles",
            Self::ManagedEntries => "managed entries",
            Self::DoctorRunner => "Doctor runner",
        }
    }
}

impl fmt::Display for DoctorSubsystem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

// --- Known recovery (informational only) ---------------------------------

/// A repair ArchiveFS **already** implements for this fault, recorded so
/// Doctor can honestly say one exists - and nothing more.
///
/// This is deliberately inert. It holds no callable, no closure, and no
/// argument to execute with; [`RecoveryAction`] is reused purely as a
/// descriptive label because it is already a closed enum of actions the
/// product implements. Stage 1A renders this as text only. Exposing it as
/// a button is Stage 1B's job, and doing so will require its own
/// confirmation and verification design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnownRecovery {
    /// The existing recovery action, when the producing report named one.
    /// `None` when a repair exists but is not one of the three
    /// `RecoveryAction` variants (for example removing missing catalogue
    /// rows, which lives on the Library page).
    pub action: Option<RecoveryAction>,
    /// Where the person can already do this today, in ArchiveFS's own
    /// navigation terms.
    pub available_at: &'static str,
}

impl KnownRecovery {
    fn new(action: Option<RecoveryAction>, available_at: &'static str) -> Self {
        Self {
            action,
            available_at,
        }
    }

    /// The exact wording Doctor shows. Never a button label.
    pub fn notice(&self) -> String {
        format!(
            "A repair action already exists elsewhere in ArchiveFS: {}.",
            self.available_at
        )
    }
}

// --- Finding --------------------------------------------------------------

/// One thing Doctor found. Produced only by read-only checks, and only by
/// adapting a report some existing subsystem already computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable, machine-readable, namespaced by category - for example
    /// `mount_root.not_writable`, `library.archive_missing`,
    /// `database.integrity_check_failed`. Follows the existing
    /// `retroarch::Diagnostic.code` convention. Part of the CLI's output
    /// contract: these strings must not change casually.
    pub id: String,
    pub category: DoctorCategory,
    pub subsystem: DoctorSubsystem,
    pub severity: DoctorSeverity,
    /// Short human title.
    pub title: String,
    /// What was observed, in plain language.
    pub explanation: String,
    /// Why it matters. Populated verbatim from `SetupDiagnostic
    /// ::why_it_matters` where that exists; `None` rather than invented
    /// prose everywhere else.
    pub why_it_matters: Option<String>,
    /// The recommended next step. Populated verbatim from
    /// `SetupDiagnostic::next_step` where that exists; `None` rather than
    /// invented advice everywhere else.
    pub next_step: Option<String>,
    /// Observed facts only - never a guess and never a suggestion.
    pub evidence: Vec<String>,
    /// The exact resource affected. `EncodedPath` is reused so a path
    /// containing non-UTF-8 bytes is still displayable and still flagged
    /// `lossy`, instead of being silently mangled.
    pub affected: Option<EncodedPath>,
    /// Informational only - see [`KnownRecovery`]. Present when a repair
    /// exists somewhere else in ArchiveFS but Doctor does not offer it.
    pub recovery: Option<KnownRecovery>,
    /// The repair Doctor itself offers for this finding, if any. Fieldless,
    /// so a finding can never smuggle a path or a command into a repair -
    /// the target is re-derived from live state at execution time. See
    /// [`repair::execute_doctor_repair`].
    pub repair: Option<repair::DoctorRepairAction>,
    /// Machine-readable values behind the prose, for `--json` consumers.
    ///
    /// Evidence strings stay the human-facing account; these are the same
    /// facts as typed numbers and flags, so a script never has to parse
    /// "3.2 GiB free" out of a sentence. Empty for every finding that has no
    /// measurement to report.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub measurements: BTreeMap<String, Measurement>,
}

/// One machine-readable value attached to a finding.
///
/// Deliberately not `serde_json::Value`: [`Finding`] is `Eq`, and keeping
/// these values `Eq` means findings stay directly comparable in tests. A
/// percentage is carried in hundredths for the same reason, and serialised
/// back out as an ordinary JSON number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Measurement {
    Text(String),
    Integer(u64),
    Flag(bool),
    PercentHundredths(u32),
}

impl Measurement {
    pub fn percent(value: f64) -> Self {
        Self::PercentHundredths((value.clamp(0.0, 100.0) * 100.0).round() as u32)
    }

    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

impl Serialize for Measurement {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(value) => serializer.serialize_str(value),
            Self::Integer(value) => serializer.serialize_u64(*value),
            Self::Flag(value) => serializer.serialize_bool(*value),
            Self::PercentHundredths(value) => serializer.serialize_f64(f64::from(*value) / 100.0),
        }
    }
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Flag(value) => write!(f, "{value}"),
            Self::PercentHundredths(value) => {
                write!(f, "{:.2}%", f64::from(*value) / 100.0)
            }
        }
    }
}

impl Finding {
    fn new(
        id: impl Into<String>,
        category: DoctorCategory,
        subsystem: DoctorSubsystem,
        severity: DoctorSeverity,
        title: impl Into<String>,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            subsystem,
            severity,
            title: title.into(),
            explanation: explanation.into(),
            why_it_matters: None,
            next_step: None,
            evidence: Vec::new(),
            affected: None,
            recovery: None,
            repair: None,
            measurements: BTreeMap::new(),
        }
    }

    fn with_measurements(
        mut self,
        measurements: impl IntoIterator<Item = (&'static str, Measurement)>,
    ) -> Self {
        self.measurements.extend(
            measurements
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        );
        self
    }

    /// Attaches one of Doctor's own repairs to this finding.
    fn offering(mut self, repair: repair::DoctorRepairAction) -> Self {
        self.repair = Some(repair);
        self
    }

    fn with_evidence(mut self, evidence: impl IntoIterator<Item = String>) -> Self {
        self.evidence.extend(evidence);
        self
    }

    fn with_affected_path(mut self, path: &Path) -> Self {
        self.affected = Some(EncodedPath::from_path(path));
        self
    }

    fn with_affected(mut self, affected: EncodedPath) -> Self {
        self.affected = Some(affected);
        self
    }

    fn with_guidance(
        mut self,
        why_it_matters: impl Into<String>,
        next_step: impl Into<String>,
    ) -> Self {
        self.why_it_matters = Some(why_it_matters.into());
        self.next_step = Some(next_step.into());
        self
    }

    fn with_recovery(mut self, recovery: KnownRecovery) -> Self {
        self.recovery = Some(recovery);
        self
    }

    /// Whether a repair for this fault exists somewhere in ArchiveFS,
    /// whether or not Doctor offers it here.
    pub fn repair_may_exist(&self) -> bool {
        self.recovery.is_some() || self.repair.is_some()
    }

    /// The repair Doctor offers for this finding, with its full description.
    pub fn offered_repair(&self) -> Option<repair::DoctorRepairSpec> {
        self.repair.map(repair::DoctorRepairAction::spec)
    }

    /// The identity used for duplicate suppression: the stable id plus the
    /// exact affected resource. Two findings with the same id about
    /// *different* paths are genuinely different problems and are both
    /// kept.
    pub(crate) fn duplicate_key(&self) -> (&str, Option<&str>) {
        (
            self.id.as_str(),
            self.affected.as_ref().map(|path| path.display.as_str()),
        )
    }

    /// Deterministic sort key: severity, then category, then affected
    /// resource, then id. Never depends on iteration order of a map or on
    /// the order adapters happen to run in.
    pub(crate) fn sort_key(&self) -> (u8, u8, &str, &str) {
        (
            self.severity.rank(),
            self.category.order(),
            self.affected
                .as_ref()
                .map(|path| path.display.as_str())
                .unwrap_or(""),
            self.id.as_str(),
        )
    }
}

// --- Coverage and deferred checks ----------------------------------------

/// Whether a subsystem was actually checked by this scan. Recorded so a
/// scan with zero findings can distinguish "checked and clean" from "never
/// looked".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CoverageStatus {
    Checked,
    /// The input was not available (not loaded yet, or the gatherer
    /// failed). Never presented as a pass.
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubsystemCoverage {
    pub category: DoctorCategory,
    pub subsystem: DoctorSubsystem,
    #[serde(flatten)]
    pub status: CoverageStatus,
}

impl SubsystemCoverage {
    pub fn was_checked(&self) -> bool {
        matches!(self.status, CoverageStatus::Checked)
    }
}

/// An individual check that was available but did not run in this scan, and
/// why. Distinct from [`DeferredCheck`] (which ArchiveFS cannot do at all)
/// and from [`CoverageStatus::Unavailable`] (which is whole-subsystem):
/// this is a single check inside a subsystem that *was* consulted.
///
/// Populated today by mount-root writability under
/// `run_setup_diagnostics_read_only`, which reports
/// [`SetupDiagnosticStatus::NotChecked`] rather than writing a probe file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotCheckedCheck {
    pub name: String,
    pub reason: String,
    pub next_step: String,
}

/// A check ArchiveFS does not perform yet. Shown in the product so a clean
/// Doctor result is never mistaken for complete coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DeferredCheck {
    pub name: &'static str,
    pub reason: &'static str,
}

/// Everything Stage 1A knowingly does not check. Kept as data, in core, so
/// the GUI and the CLI cannot drift apart on it.
pub const DEFERRED_CHECKS: &[DeferredCheck] = &[
    DeferredCheck {
        name: "Per-directory disk quotas",
        reason: "Free space is now reported per filesystem. Quotas that apply to one user or one directory below the filesystem are not read, so a filesystem can look free while a quota is exhausted.",
    },
    DeferredCheck {
        name: "Write access inside a sandbox",
        reason: "Read-only mounts and permissions are now assessed from metadata. A Flatpak or Snap portal can still refuse a write that permissions appear to allow, and proving that would need a write probe ArchiveFS deliberately does not perform.",
    },
    DeferredCheck {
        name: "Managed entries with no install record",
        reason: "Managed entries are matched against ArchiveFS's install history. PCSX2 .pnach files and GameHacking-managed Dolphin INIs additionally carry in-file ArchiveFS ownership markers. Xenia and RetroArch files carry no marker, so an entry whose install record was deleted cannot be recognised - and ArchiveFS will not guess, because your own entries look identical.",
    },
    DeferredCheck {
        name: "Dolphin, PCSX2 and Xenia diagnostic reports",
        reason: "These adapters have profile discovery but produce no structured diagnostics to adapt.",
    },
    DeferredCheck {
        name: "GameHacking.org cache health",
        reason: "The GameHacking providers are not part of this release; nothing to check here yet.",
    },
    DeferredCheck {
        name: "Live mount failures and recovery offers",
        reason: "These are per-session state. Doctor reads persisted and preloaded state only, so a mount failure appears here only once the library view has observed it.",
    },
    DeferredCheck {
        name: "Repairs beyond the four safe mount and index actions",
        reason: "Doctor performs only the four repairs it lists explicitly. Everything else - permission changes, remounting, database repair, removing managed cheat entries, rolling back an interrupted install - is reported and explained, never performed.",
    },
];

// --- Gatherers ------------------------------------------------------------

/// The read-only result of validating the configured mount root as a
/// destination root. Gathered outside the runner so the runner itself stays
/// a pure function.
#[derive(Debug, Clone)]
pub struct MountRootSafety {
    pub root: std::path::PathBuf,
    pub outcome: Result<DestinationRootState, DestinationSafetyError>,
}

/// Validates the configured mount root using the existing, documented
/// read-only `validate_destination_root`.
///
/// That function inspects every existing path component with
/// `fs::symlink_metadata` and **never creates anything** - see
/// `patch_manager::destination_safety`'s module documentation. This is the
/// only filesystem access any Doctor gatherer in this module performs.
pub fn assess_mount_root_safety(mount_root: &Path) -> MountRootSafety {
    MountRootSafety {
        root: mount_root.to_path_buf(),
        outcome: validate_destination_root(mount_root).map(|root| root.state()),
    }
}

// --- Adapter: DoctorReport ------------------------------------------------

/// Which Doctor category one of `complete_doctor_report`'s check names
/// belongs to. The names are the exact literals that function emits
/// (`crates/archivefs-core/src/lib.rs`); anything unrecognised falls back
/// to `Configuration` rather than being dropped.
fn doctor_check_category(name: &str) -> DoctorCategory {
    match name {
        "config file" | "config parses" | "config path" => DoctorCategory::Configuration,
        "source folder" => DoctorCategory::Sources,
        "mount root" | "mount root writable" => DoctorCategory::MountRoot,
        "ratarmount" | "unmount tool" => DoctorCategory::Configuration,
        "archive scan" => DoctorCategory::Library,
        "mount status" => DoctorCategory::Mounts,
        _ => DoctorCategory::Configuration,
    }
}

/// A stable id for one `DoctorCheck`. Derived from the check name so the id
/// does not change when the human wording of `detail` changes.
fn doctor_check_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.{slug}", doctor_check_category(name).slug())
}

impl DoctorCategory {
    fn slug(self) -> &'static str {
        match self {
            Self::Configuration => "config",
            Self::Storage => "filesystem",
            Self::Filesystems => "filesystem",
            Self::MountRoot => "mount_root",
            Self::Mounts => "mounts",
            Self::Sources => "sources",
            Self::Library => "library",
            Self::Database => "database",
            Self::Emulators => "emulators",
            Self::EmulatorProfiles => "emulator_profile",
            Self::ManagedEntries => "managed_entry",
            Self::Transactions => "transactions",
            Self::Doctor => "doctor",
        }
    }
}

fn doctor_status_severity(status: DoctorStatus) -> Option<DoctorSeverity> {
    match status {
        DoctorStatus::Fail => Some(DoctorSeverity::Error),
        DoctorStatus::Warn => Some(DoctorSeverity::Warning),
        DoctorStatus::Pass => None,
    }
}

fn finding_from_doctor_check(check: &DoctorCheck) -> Option<Finding> {
    let severity = doctor_status_severity(check.status)?;
    let category = doctor_check_category(&check.name);
    let mut finding = Finding::new(
        doctor_check_id(&check.name),
        category,
        DoctorSubsystem::DoctorReport,
        severity,
        check.name.clone(),
        check.detail.clone(),
    );
    if category == DoctorCategory::MountRoot && check.name == "mount root" {
        finding = finding.with_recovery(KnownRecovery::new(
            None,
            "Settings → Validate configuration offers Create Mount Root",
        ));
    }
    Some(finding)
}

/// Adapts an already-computed `DoctorReport`.
///
/// The caller must have produced it with `run_doctor_read_only` (or
/// `complete_doctor_report` with a preloaded snapshot). This adapter cannot
/// tell the difference, so the read-only guarantee lives with the caller -
/// which is why both callers in this repository are pointed at the
/// read-only variant and covered by tests.
pub fn findings_from_doctor_report(report: &DoctorReport) -> Vec<Finding> {
    report
        .checks
        .iter()
        .filter_map(finding_from_doctor_check)
        .collect()
}

// --- Adapter: SetupDiagnostics -------------------------------------------

fn setup_status_severity(status: SetupDiagnosticStatus) -> Option<DoctorSeverity> {
    match status {
        SetupDiagnosticStatus::Error => Some(DoctorSeverity::Error),
        SetupDiagnosticStatus::Warning => Some(DoctorSeverity::Warning),
        // A pass is not a finding, and neither is a check that did not run:
        // the latter is surfaced as a coverage gap instead (see
        // `not_checked_from_setup_diagnostics`).
        SetupDiagnosticStatus::Ready | SetupDiagnosticStatus::NotChecked => None,
    }
}

/// Maps one setup-diagnostic name to a Doctor category. Names are the exact
/// literals `run_setup_diagnostics_with_checks` emits.
fn setup_check_category(name: &str) -> DoctorCategory {
    let lower = name.to_ascii_lowercase();
    // "source folder" is tested before the `config` prefix on purpose:
    // "Configured source folder exists" starts with "config" but is a source
    // folder problem, not a configuration one.
    if lower.contains("source folder") {
        DoctorCategory::Sources
    } else if lower.starts_with("config") {
        DoctorCategory::Configuration
    } else if lower.contains("mount root") {
        DoctorCategory::MountRoot
    } else if lower.contains("mount/unmount") || lower.contains("mount and unmount") {
        DoctorCategory::Mounts
    } else if lower.contains("ratarmount") || lower.contains("unmount tool") {
        DoctorCategory::Configuration
    } else if lower.contains("scanning") {
        DoctorCategory::Library
    } else {
        DoctorCategory::Configuration
    }
}

fn finding_from_setup_diagnostic(check: &SetupDiagnostic) -> Option<Finding> {
    let severity = setup_status_severity(check.status)?;
    let category = setup_check_category(&check.name);
    let id = {
        let slug: String = check
            .name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}.setup_{slug}", category.slug())
    };
    Some(
        Finding::new(
            id,
            category,
            DoctorSubsystem::SetupDiagnostics,
            severity,
            check.name.clone(),
            check.detail.clone(),
        )
        .with_guidance(check.why_it_matters.clone(), check.next_step.clone()),
    )
}

/// The checks in an already-computed `SetupDiagnostics` that deliberately
/// did not run. Never presented as passes.
pub fn not_checked_from_setup_diagnostics(diagnostics: &SetupDiagnostics) -> Vec<NotCheckedCheck> {
    diagnostics
        .checks
        .iter()
        .filter(|check| check.status == SetupDiagnosticStatus::NotChecked)
        .map(|check| NotCheckedCheck {
            name: check.name.clone(),
            reason: check.detail.clone(),
            next_step: check.next_step.clone(),
        })
        .collect()
}

/// Adapts an already-computed `SetupDiagnostics`. This is the only existing
/// report carrying `why_it_matters`/`next_step`, so its prose is copied
/// verbatim rather than paraphrased.
pub fn findings_from_setup_diagnostics(diagnostics: &SetupDiagnostics) -> Vec<Finding> {
    let mut findings: Vec<Finding> = diagnostics
        .checks
        .iter()
        .filter_map(finding_from_setup_diagnostic)
        .collect();
    if let Some(error) = &diagnostics.config_path_error {
        findings.push(Finding::new(
            "config.path_unresolvable",
            DoctorCategory::Configuration,
            DoctorSubsystem::SetupDiagnostics,
            DoctorSeverity::Error,
            "Configuration path could not be resolved",
            error.clone(),
        ));
    }
    findings
}

// --- Adapter: HealthIssue ------------------------------------------------

fn health_category_severity(category: HealthCategory) -> DoctorSeverity {
    match category {
        // A mount failure that retrying cannot fix needs a person.
        HealthCategory::TerminalFailure => DoctorSeverity::Error,
        HealthCategory::RetryableFailure => DoctorSeverity::Warning,
        HealthCategory::RecoveryAvailable => DoctorSeverity::Warning,
        HealthCategory::Missing => DoctorSeverity::Warning,
        // Stored or incomplete evidence must never masquerade as a current
        // emergency. A loose item that needs no mount is informational too.
        HealthCategory::HistoricalMountFailure
        | HealthCategory::MountNotRequired
        | HealthCategory::MountFailureEvidenceInsufficient => DoctorSeverity::Info,
        // Both of these are ordinary transitional states, not faults.
        HealthCategory::AwaitingValidation => DoctorSeverity::Info,
        HealthCategory::CachedOnly => DoctorSeverity::Info,
        HealthCategory::UnknownPlatform => DoctorSeverity::Info,
    }
}

fn health_issue_doctor_category(category: HealthCategory) -> DoctorCategory {
    match category {
        // Live mount outcomes belong under Mounts...
        HealthCategory::TerminalFailure
        | HealthCategory::RetryableFailure
        | HealthCategory::RecoveryAvailable
        | HealthCategory::HistoricalMountFailure
        | HealthCategory::MountNotRequired
        | HealthCategory::MountFailureEvidenceInsufficient => DoctorCategory::Mounts,
        // ...catalogue facts belong under Library.
        HealthCategory::Missing
        | HealthCategory::AwaitingValidation
        | HealthCategory::CachedOnly
        | HealthCategory::UnknownPlatform => DoctorCategory::Library,
    }
}

fn health_issue_id(category: HealthCategory) -> &'static str {
    match category {
        HealthCategory::TerminalFailure => "mounts.terminal_failure",
        HealthCategory::RetryableFailure => "mounts.retryable_failure",
        HealthCategory::RecoveryAvailable => "mounts.recovery_available",
        HealthCategory::HistoricalMountFailure => "mounts.historical_failure",
        HealthCategory::MountNotRequired => "mounts.not_required",
        HealthCategory::MountFailureEvidenceInsufficient => "mounts.failure_evidence_incomplete",
        HealthCategory::Missing => "library.archive_missing",
        HealthCategory::AwaitingValidation => "library.awaiting_validation",
        HealthCategory::CachedOnly => "library.cached_only",
        HealthCategory::UnknownPlatform => "library.unknown_platform",
    }
}

fn health_issue_recovery(issue: &HealthIssue) -> Option<KnownRecovery> {
    match issue.recovery_action {
        Some(action @ RecoveryAction::RetryMount) => {
            Some(KnownRecovery::new(Some(action), "Library → Health, Retry"))
        }
        Some(action @ RecoveryAction::Remount) => Some(KnownRecovery::new(
            Some(action),
            "Library → Health, Remount",
        )),
        Some(action @ RecoveryAction::LazyUnmount) => Some(KnownRecovery::new(
            Some(action),
            "Library → Health, Force unmount",
        )),
        None if issue.category == HealthCategory::Missing => Some(KnownRecovery::new(
            None,
            "Library → Health, Review missing archives",
        )),
        None => None,
    }
}

/// Adapts `classify_archive_health`'s output. Both callers already produce
/// these: the GUI via `build_health_issues` (live records + catalogue), the
/// CLI via `catalogue_health_report` (catalogue only). The category already
/// carries whether failure evidence was current, historical, unnecessary, or
/// insufficient; this adapter preserves that distinction in IDs, severity,
/// prose, and typed measurements.
pub fn findings_from_health_issues(issues: &[HealthIssue]) -> Vec<Finding> {
    issues
        .iter()
        .map(|issue| {
            let mut evidence = vec![format!("Classification: {}", issue.category.label())];
            // The stored canonical identifier, shown with the registry's display
            // name so a person reads real hardware names while still seeing
            // exactly what the library has recorded.
            //
            // Deliberately no confidence and no re-detection here: this value
            // was persisted, possibly by an older build, and running the
            // detector against it would turn stored history into a claim about
            // the present. Confidence is only ever shown where a *current*
            // detection actually exists - see `platform-detect` and the library
            // view. Nothing here rewrites the stored row.
            match &issue.platform {
                Some(platform) => {
                    let display = crate::platform::display_name_for(platform);
                    if display == platform {
                        evidence.push(format!("Platform: {platform} (as stored)"));
                    } else {
                        evidence.push(format!("Platform: {display} (stored as {platform})"));
                    }
                }
                None => evidence.push("Platform: not assigned".to_string()),
            }
            if let Some(state) = issue.mount_state {
                evidence.push(format!("Mount state: {state}"));
            }
            if let Some(last_seen) = &issue.last_seen_at {
                evidence.push(format!("Last seen: {last_seen}"));
            }
            if let Some(size) = issue.size_bytes {
                evidence.push(format!("Size: {size} bytes"));
            }
            if let Some(modified) = issue.modified_time_unix_seconds {
                evidence.push(format!("Modified: Unix timestamp {modified}"));
            }
            evidence.push(format!("Present: {}", issue.present));
            let mut finding = Finding::new(
                health_issue_id(issue.category),
                health_issue_doctor_category(issue.category),
                DoctorSubsystem::ArchiveHealth,
                health_category_severity(issue.category),
                issue.category.label(),
                issue.reason.clone(),
            )
            .with_affected_path(&issue.path)
            .with_evidence(evidence);
            let failure_scope = match issue.category {
                HealthCategory::TerminalFailure | HealthCategory::RetryableFailure => "current",
                HealthCategory::HistoricalMountFailure => "historical",
                HealthCategory::MountNotRequired => "not_required",
                HealthCategory::MountFailureEvidenceInsufficient => "insufficient_evidence",
                _ => "not_applicable",
            };
            let media_kind = issue
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            finding = finding.with_measurements([
                ("mount_failure_scope", Measurement::text(failure_scope)),
                (
                    "mount_required",
                    Measurement::Flag(issue.category != HealthCategory::MountNotRequired),
                ),
                ("media_kind", Measurement::text(media_kind)),
                ("reason", Measurement::text(&issue.reason)),
                (
                    "platform",
                    Measurement::text(issue.platform.as_deref().unwrap_or("unassigned")),
                ),
                (
                    "platform_display_name",
                    Measurement::text(
                        issue
                            .platform
                            .as_deref()
                            .map(crate::platform::display_name_for)
                            .unwrap_or("unassigned"),
                    ),
                ),
                // States plainly that this platform came from the record, not
                // from detecting anything during this scan.
                ("platform_source_scope", Measurement::text("stored")),
            ]);
            if let Some(recovery) = health_issue_recovery(issue) {
                finding = finding.with_recovery(recovery);
            }
            finding
        })
        .collect()
}

// --- Adapter: SourceHealthIssue ------------------------------------------

fn source_availability_severity(availability: SourceAvailability) -> Option<DoctorSeverity> {
    match availability {
        SourceAvailability::Unavailable | SourceAvailability::PermissionDenied => {
            Some(DoctorSeverity::Error)
        }
        SourceAvailability::ScanFailed => Some(DoctorSeverity::Warning),
        // `source_health_issues` already excludes both of these.
        SourceAvailability::Available | SourceAvailability::Disabled => None,
    }
}

fn source_availability_id(availability: SourceAvailability) -> &'static str {
    match availability {
        SourceAvailability::Unavailable => "sources.unavailable",
        SourceAvailability::PermissionDenied => "sources.permission_denied",
        SourceAvailability::ScanFailed => "sources.scan_failed",
        SourceAvailability::Available => "sources.available",
        SourceAvailability::Disabled => "sources.disabled",
    }
}

/// Adapts `source_health_issues`. One finding per source folder - never one
/// per archive it owns, matching the safety rule that function documents.
pub fn findings_from_source_health(issues: &[SourceHealthIssue]) -> Vec<Finding> {
    issues
        .iter()
        .filter_map(|issue| {
            let severity = source_availability_severity(issue.availability)?;
            let mut evidence = vec![format!(
                "Catalogue rows preserved: {}",
                issue.archives_preserved
            )];
            if let Some(error) = &issue.last_scan_error {
                evidence.push(format!("Last scan error: {error}"));
            }
            Some(
                Finding::new(
                    source_availability_id(issue.availability),
                    DoctorCategory::Sources,
                    DoctorSubsystem::SourceHealth,
                    severity,
                    "Source folder needs attention",
                    issue.reason.clone(),
                )
                .with_affected_path(&issue.path)
                .with_evidence(evidence)
                .with_recovery(KnownRecovery::new(None, "Sources → Rescan this folder")),
            )
        })
        .collect()
}

// --- Adapter: DatabaseHealthReport ---------------------------------------

pub use database_adapter::findings_from_database_report;

mod database_adapter {
    use super::{DoctorCategory, DoctorSeverity, DoctorSubsystem, Finding, KnownRecovery};
    use crate::database::{
        DatabaseCheckStatus, DatabaseDiagnostic, DatabaseDiagnosticCode,
        DatabaseDiagnosticSeverity, DatabaseHealthReport,
    };
    use crate::emulator_environment::EncodedPath;

    /// Conservative mapping with deliberate *escalation* for codes that mean
    /// the user's catalogue may actually be damaged. Nothing is ever
    /// downgraded.
    pub(super) fn database_severity(diagnostic: &DatabaseDiagnostic) -> DoctorSeverity {
        let escalate_to_critical = matches!(
            diagnostic.code,
            DatabaseDiagnosticCode::CorruptDatabase
                | DatabaseDiagnosticCode::MalformedDatabase
                | DatabaseDiagnosticCode::IntegrityCheckFailed
                | DatabaseDiagnosticCode::RollbackRecoveryRequired
                | DatabaseDiagnosticCode::MigrationFailed
                | DatabaseDiagnosticCode::SchemaVersionUnsupported
        );
        match diagnostic.severity {
            DatabaseDiagnosticSeverity::Error if escalate_to_critical => DoctorSeverity::Critical,
            DatabaseDiagnosticSeverity::Error => DoctorSeverity::Error,
            DatabaseDiagnosticSeverity::Warning => DoctorSeverity::Warning,
            DatabaseDiagnosticSeverity::Info => DoctorSeverity::Info,
        }
    }

    /// One stable id per code. Exhaustive by construction so a new code
    /// cannot silently inherit another's id.
    fn database_code_id(code: DatabaseDiagnosticCode) -> &'static str {
        match code {
            DatabaseDiagnosticCode::MissingDatabase => "database.missing",
            DatabaseDiagnosticCode::PermissionDenied => "database.permission_denied",
            DatabaseDiagnosticCode::DatabaseLocked => "database.locked",
            DatabaseDiagnosticCode::DatabaseBusy => "database.busy",
            DatabaseDiagnosticCode::RollbackJournalPresent => "database.rollback_journal_present",
            DatabaseDiagnosticCode::HotRollbackJournal => "database.hot_rollback_journal",
            DatabaseDiagnosticCode::NonHotRollbackJournal => "database.non_hot_rollback_journal",
            DatabaseDiagnosticCode::MalformedRollbackJournal => {
                "database.malformed_rollback_journal"
            }
            DatabaseDiagnosticCode::RollbackRecoveryRequired => {
                "database.rollback_recovery_required"
            }
            DatabaseDiagnosticCode::WalPresent => "database.wal_present",
            DatabaseDiagnosticCode::ShmPresent => "database.shm_present",
            DatabaseDiagnosticCode::CorruptDatabase => "database.corrupt",
            DatabaseDiagnosticCode::MalformedDatabase => "database.malformed",
            DatabaseDiagnosticCode::IntegrityCheckFailed => "database.integrity_check_failed",
            DatabaseDiagnosticCode::SchemaVersionUnsupported => {
                "database.schema_version_unsupported"
            }
            DatabaseDiagnosticCode::MigrationFailed => "database.migration_failed",
            DatabaseDiagnosticCode::IoError => "database.io_error",
            DatabaseDiagnosticCode::SqliteError => "database.sqlite_error",
        }
    }

    fn database_code_title(code: DatabaseDiagnosticCode) -> &'static str {
        match code {
            DatabaseDiagnosticCode::MissingDatabase => "Catalogue database is missing",
            DatabaseDiagnosticCode::PermissionDenied => "Catalogue database cannot be read",
            DatabaseDiagnosticCode::DatabaseLocked => "Catalogue database is locked",
            DatabaseDiagnosticCode::DatabaseBusy => "Catalogue database is busy",
            DatabaseDiagnosticCode::RollbackJournalPresent
            | DatabaseDiagnosticCode::NonHotRollbackJournal => "A rollback journal is present",
            DatabaseDiagnosticCode::HotRollbackJournal => "A hot rollback journal is present",
            DatabaseDiagnosticCode::MalformedRollbackJournal => {
                "A rollback journal header is malformed"
            }
            DatabaseDiagnosticCode::RollbackRecoveryRequired => {
                "The catalogue database needs recovery"
            }
            DatabaseDiagnosticCode::WalPresent => "A write-ahead log is present",
            DatabaseDiagnosticCode::ShmPresent => "A shared-memory file is present",
            DatabaseDiagnosticCode::CorruptDatabase => "The catalogue database is corrupt",
            DatabaseDiagnosticCode::MalformedDatabase => "The catalogue database is malformed",
            DatabaseDiagnosticCode::IntegrityCheckFailed => "Database integrity check failed",
            DatabaseDiagnosticCode::SchemaVersionUnsupported => {
                "Catalogue schema version is unsupported"
            }
            DatabaseDiagnosticCode::MigrationFailed => "A catalogue migration failed",
            DatabaseDiagnosticCode::IoError => "Catalogue database I/O error",
            DatabaseDiagnosticCode::SqliteError => "SQLite reported an error",
        }
    }

    /// Adapts `diagnose_database`'s output. That function is documented to
    /// perform no recovery, no migration and no pragma change, which is
    /// exactly why it is safe here. No repair is offered: database repair is
    /// never automatic in ArchiveFS.
    pub fn findings_from_database_report(report: &DatabaseHealthReport) -> Vec<Finding> {
        let path = EncodedPath {
            display: report.database_path.display.clone(),
            lossy: report.database_path.lossy,
        };
        let mut findings: Vec<Finding> = report
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let mut evidence = vec![format!("Diagnostic code: {:?}", diagnostic.code)];
                if let Some(code) = diagnostic.sqlite_extended_code {
                    evidence.push(format!("SQLite extended result code: {code}"));
                }
                if let Some(raw) = &diagnostic.raw_sqlite_message {
                    evidence.push(format!("SQLite message: {raw}"));
                }
                if let Some(version) = report.schema_version {
                    evidence.push(format!("Schema version: {version}"));
                }
                if let Some(mode) = &report.journal_mode {
                    evidence.push(format!("Journal mode: {mode}"));
                }
                Finding::new(
                    database_code_id(diagnostic.code),
                    DoctorCategory::Database,
                    DoctorSubsystem::DatabaseDiagnostics,
                    database_severity(diagnostic),
                    database_code_title(diagnostic.code),
                    diagnostic.message.clone(),
                )
                .with_affected(path.clone())
                .with_evidence(evidence)
            })
            .collect();

        // The two SQLite checks are reported separately from the diagnostic
        // list so a failing check is visible even if it produced no
        // diagnostic of its own.
        for (name, id, outcome) in [
            (
                "Quick check",
                "database.quick_check_failed",
                &report.quick_check,
            ),
            (
                "Integrity check",
                "database.integrity_check_reported_problems",
                &report.integrity_check,
            ),
        ] {
            match outcome.status {
                DatabaseCheckStatus::Failed | DatabaseCheckStatus::Error => {
                    findings.push(
                        Finding::new(
                            id,
                            DoctorCategory::Database,
                            DoctorSubsystem::DatabaseDiagnostics,
                            DoctorSeverity::Critical,
                            format!("{name} reported problems"),
                            format!("SQLite's {} did not complete cleanly.", name.to_lowercase()),
                        )
                        .with_affected(path.clone())
                        .with_evidence(outcome.messages.clone())
                        .with_recovery(KnownRecovery::new(
                            None,
                            "Tools → Database Status shows the full report; ArchiveFS never repairs the catalogue automatically",
                        )),
                    );
                }
                DatabaseCheckStatus::Ok | DatabaseCheckStatus::NotRun => {}
            }
        }
        findings
    }
}

// --- Adapter: destination safety on the mount root -----------------------

fn destination_reason_id(reason: DestinationSafetyFailureReason) -> &'static str {
    match reason {
        DestinationSafetyFailureReason::RootNotDirectory => "mount_root.not_a_directory",
        DestinationSafetyFailureReason::RootSymlink => "mount_root.symlink",
        DestinationSafetyFailureReason::UnsafeComponent => "mount_root.unsafe_component",
        DestinationSafetyFailureReason::Traversal => "mount_root.path_traversal",
        DestinationSafetyFailureReason::ParentSymlink => "mount_root.parent_symlink",
        DestinationSafetyFailureReason::ParentSymlinkEscape => "mount_root.parent_symlink_escape",
        DestinationSafetyFailureReason::FinalSymlink => "mount_root.final_symlink",
        DestinationSafetyFailureReason::FinalSymlinkEscape => "mount_root.final_symlink_escape",
        DestinationSafetyFailureReason::BrokenSymlink => "mount_root.broken_symlink",
        DestinationSafetyFailureReason::SymlinkLoop => "mount_root.symlink_loop",
        DestinationSafetyFailureReason::NonDirectoryParent => "mount_root.non_directory_parent",
        DestinationSafetyFailureReason::DestinationIsDirectory => {
            "mount_root.destination_is_directory"
        }
        DestinationSafetyFailureReason::DestinationOutsideRoot => "mount_root.outside_root",
        DestinationSafetyFailureReason::UnsafeDestination => "mount_root.unsafe_destination",
        DestinationSafetyFailureReason::InspectionFailed => "mount_root.inspection_failed",
    }
}

/// Adapts [`assess_mount_root_safety`]'s result.
///
/// A merely *absent* root produces no finding here: `DoctorReport` and
/// `SetupDiagnostics` already report that, and repeating it would be the
/// third copy of one fact. Only genuine path-safety failures - a symlinked
/// root, a non-directory component, a traversal - are reported, because
/// nothing else in ArchiveFS surfaces them for the mount root today.
pub fn findings_from_mount_root_safety(safety: &MountRootSafety) -> Vec<Finding> {
    match &safety.outcome {
        Ok(_) => Vec::new(),
        Err(error) => {
            let mut evidence = vec![
                format!("Failure reason: {:?}", error.reason),
                format!("Rejected at: {}", error.path.display()),
            ];
            if let Some(state) = error.destination_state {
                evidence.push(format!("Observed state: {state:?}"));
            }
            for parent in &error.inspected_parents {
                evidence.push(format!(
                    "Inspected parent {}: {:?}",
                    parent.path.display(),
                    parent.state
                ));
            }
            vec![
                Finding::new(
                    destination_reason_id(error.reason),
                    DoctorCategory::MountRoot,
                    DoctorSubsystem::DestinationSafety,
                    DoctorSeverity::Error,
                    "Mount root fails path-safety validation",
                    format!(
                        "The configured mount root cannot be used safely: {}.",
                        error
                    ),
                )
                .with_affected_path(&safety.root)
                .with_evidence(evidence)
                .with_guidance(
                    "ArchiveFS refuses to mount beneath a symlinked or otherwise unsafe root, because doing so could place mounts outside the directory you configured.",
                    "Point mount_root at a real directory you own, with no symlinked parent components.",
                ),
            ]
        }
    }
}

// --- Adapter: RetroArch environment --------------------------------------

fn retroarch_severity(severity: RetroArchDiagnosticSeverity) -> DoctorSeverity {
    match severity {
        RetroArchDiagnosticSeverity::Error => DoctorSeverity::Error,
        RetroArchDiagnosticSeverity::Warning => DoctorSeverity::Warning,
        RetroArchDiagnosticSeverity::Info => DoctorSeverity::Info,
    }
}

fn retroarch_category_label(category: RetroArchDiagnosticCategory) -> &'static str {
    match category {
        RetroArchDiagnosticCategory::Discovery => "discovery",
        RetroArchDiagnosticCategory::ConfigParse => "configuration parsing",
        RetroArchDiagnosticCategory::PathResolution => "path resolution",
        RetroArchDiagnosticCategory::CoreInventory => "core inventory",
        RetroArchDiagnosticCategory::Filesystem => "filesystem",
        RetroArchDiagnosticCategory::PlaylistInventory => "playlist inventory",
        RetroArchDiagnosticCategory::AppImageInventory => "AppImage inventory",
    }
}

fn finding_from_retroarch_diagnostic(diagnostic: &RetroArchDiagnostic) -> Finding {
    let mut evidence = vec![
        format!("RetroArch diagnostic code: {}", diagnostic.code),
        format!("Area: {}", retroarch_category_label(diagnostic.detail_kind)),
    ];
    if let Some(profile) = diagnostic.profile {
        evidence.push(format!(
            "Profile: {:?} ({:?})",
            profile.profile_kind, profile.scope
        ));
    }
    if let Some(purpose) = diagnostic.purpose {
        evidence.push(format!("Path purpose: {purpose:?}"));
    }
    if let Some(index) = diagnostic.entry_index {
        evidence.push(format!("Playlist entry index: {index}"));
    }
    let mut finding = Finding::new(
        format!("emulators.retroarch.{}", diagnostic.code),
        DoctorCategory::Emulators,
        DoctorSubsystem::RetroArchEnvironment,
        retroarch_severity(diagnostic.severity),
        "RetroArch environment finding",
        format!(
            "RetroArch {} reported `{}`.",
            retroarch_category_label(diagnostic.detail_kind),
            diagnostic.code
        ),
    )
    .with_evidence(evidence);
    if let Some(path) = &diagnostic.path {
        finding = finding.with_affected(EncodedPath {
            display: path.display.clone(),
            lossy: path.lossy,
        });
    }
    finding
}

/// Adapts an already-discovered `RetroArchEnvironmentReport`. Doctor never
/// triggers discovery itself: the caller passes a report it already has, so
/// opening Doctor cannot start an emulator scan.
pub fn findings_from_retroarch_environment(report: &RetroArchEnvironmentReport) -> Vec<Finding> {
    report
        .diagnostics
        .iter()
        .map(finding_from_retroarch_diagnostic)
        .collect()
}

// --- Adapter: shared transaction history ---------------------------------

/// Adapts `discover_shared_apply_history`. Surfaces the interrupted and
/// failed installs that are already persisted in journals but that nothing
/// currently presents as a *problem*.
pub fn findings_from_transaction_history(report: &SharedHistoryReport) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, journal) in &report.journals {
        let (id, severity, title, explanation) = match journal.status {
            SharedApplyStatus::PartialFailure => (
                "transactions.partial_failure",
                DoctorSeverity::Error,
                "An install did not finish completely",
                "Some entries in this operation were written and others were not.",
            ),
            SharedApplyStatus::Failed => (
                "transactions.failed",
                DoctorSeverity::Warning,
                "An install failed",
                "This operation did not complete. Its journal records what was attempted.",
            ),
            SharedApplyStatus::DryRun | SharedApplyStatus::Success => continue,
        };
        let mut finding = Finding::new(
            id,
            DoctorCategory::Transactions,
            DoctorSubsystem::SharedTransactions,
            severity,
            title,
            explanation,
        )
        .with_affected(EncodedPath {
            display: path.display.clone(),
            lossy: path.unix_bytes_hex.is_some() && path.display.contains('\u{fffd}'),
        })
        .with_evidence(vec![
            format!("Operation: {}", journal.operation_id),
            format!("Adapter: {:?}", journal.context.adapter),
            format!("Entries: {}", journal.entries.len()),
            format!(
                "Recorded at: Unix timestamp {}",
                journal.timestamp_unix_seconds
            ),
            format!("Destination root: {}", journal.destination_root.display),
        ]);
        if journal.rollback_operation_id.is_none() {
            finding = finding.with_recovery(KnownRecovery::new(
                None,
                "History & Logs → Preview rollback for this operation",
            ));
        } else {
            finding
                .evidence
                .push("A rollback has already been recorded for this operation.".to_string());
        }
        findings.push(finding);
    }
    for warning in &report.warnings {
        findings.push(
            Finding::new(
                "transactions.journal_unreadable",
                DoctorCategory::Transactions,
                DoctorSubsystem::SharedTransactions,
                DoctorSeverity::Warning,
                "An install journal could not be read",
                "ArchiveFS found a journal file it could not parse. Rollback is unavailable for that operation.",
            )
            .with_affected(EncodedPath {
                display: warning.path.display.clone(),
                lossy: false,
            })
            .with_evidence(vec![format!("Failure: {:?}", warning.failure.kind)]),
        );
    }
    if !report.complete {
        findings.push(
            Finding::new(
                "transactions.history_truncated",
                DoctorCategory::Transactions,
                DoctorSubsystem::SharedTransactions,
                DoctorSeverity::Info,
                "Install history was truncated",
                "There are more install journals than ArchiveFS lists at once, so this history is incomplete.",
            )
            .with_evidence(vec![format!(
                "Journals listed: {}",
                report.journals.len()
            )]),
        );
    }
    findings
}
