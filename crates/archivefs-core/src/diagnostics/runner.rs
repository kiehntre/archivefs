//! The read-only Doctor runner.
//!
//! # This file is deliberately pure
//!
//! [`run_doctor_scan`] is a pure function over already-gathered inputs. It
//! performs no filesystem access, starts no process, opens no database, and
//! makes no network request. Everything it needs was collected by the
//! caller before it was invoked.
//!
//! That is not merely a convention here - it is checked. The test
//! `runner_source_contains_no_io_or_mutation_calls` reads this file's own
//! source and fails if it mentions `fs::`, `File`, `Command`, `ureq`,
//! `create_dir`, `remove_file`, `remove_dir`, `scan_archives`, or
//! `refresh`. If a future change needs any of those, it belongs in a
//! gatherer in the parent module, not here.
//!
//! # Why gathering is separate
//!
//! Splitting gathering from evaluation buys three things at once:
//!
//! - the no-mutation guarantee becomes provable rather than asserted;
//! - the runner is trivially testable with hand-built inputs, so the
//!   adapter and ordering tests need no temporary directories at all;
//! - a subsystem whose input is unavailable is reported honestly as
//!   *unavailable* instead of quietly contributing zero findings, which
//!   would look identical to "checked and clean".

use serde::Serialize;

use super::environment::{
    FreeSpacePolicy, StorageAssessment, findings_from_free_space,
    findings_from_read_only_filesystems, not_checked_from_storage,
};
use super::managed::{
    ManagedEntryScan, findings_from_managed_entries, not_checked_from_managed_entries,
};
use super::profiles::{
    ProfileAssessmentReport, findings_from_emulator_profiles, not_checked_from_emulator_profiles,
};
use super::repair::{findings_from_index_freshness, findings_from_stale_mount_directories};
use super::{
    CoverageStatus, DEFERRED_CHECKS, DeferredCheck, DoctorCategory, DoctorSeverity,
    DoctorSubsystem, Finding, MountRootSafety, NotCheckedCheck, SubsystemCoverage,
    findings_from_database_report, findings_from_doctor_report, findings_from_health_issues,
    findings_from_mount_root_safety, findings_from_retroarch_environment,
    findings_from_setup_diagnostics, findings_from_source_health,
    findings_from_transaction_history, not_checked_from_setup_diagnostics,
};
use crate::ArchiveIndexFreshness;
use crate::database::DatabaseHealthReport;
use crate::emulator_environment::retroarch::RetroArchEnvironmentReport;
use crate::patch_manager::SharedHistoryReport;
use crate::{DoctorReport, HealthIssue, SetupDiagnostics, SourceHealthIssue};

/// One subsystem's gathered input.
///
/// [`Gathered::Failed`] exists so an adapter's *input* failing becomes a
/// visible Doctor finding instead of a panic or a silent gap - the runner
/// must finish even when one subsystem could not be collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gathered<T> {
    Ready(T),
    /// Collecting this subsystem's input failed. Becomes an
    /// `Error`-severity finding under [`DoctorCategory::Doctor`].
    Failed(String),
    /// This subsystem's input is not loaded in the current session (for
    /// example the library has not been scanned yet, or RetroArch profiles
    /// have not been discovered). Recorded as *unavailable*, never as a
    /// pass.
    NotLoaded(&'static str),
}

impl<T> Gathered<T> {
    pub fn as_ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Failed(_) | Self::NotLoaded(_) => None,
        }
    }

    fn coverage_status(&self) -> CoverageStatus {
        match self {
            Self::Ready(_) => CoverageStatus::Checked,
            Self::Failed(reason) => CoverageStatus::Unavailable {
                reason: reason.clone(),
            },
            Self::NotLoaded(reason) => CoverageStatus::Unavailable {
                reason: (*reason).to_string(),
            },
        }
    }
}

/// Everything the runner evaluates. All borrowed: the caller already owns
/// this data, and the runner never needs to keep any of it.
#[derive(Debug)]
pub struct DoctorScanInputs<'a> {
    /// From `run_doctor_read_only` (never `run_doctor`, which creates the
    /// mount root) or from `complete_doctor_report` with a preloaded
    /// snapshot.
    pub doctor_report: Gathered<&'a DoctorReport>,
    /// From `run_setup_diagnostics`.
    pub setup: Gathered<&'a SetupDiagnostics>,
    /// From `build_health_issues` (GUI, live) or `catalogue_health_report`
    /// (CLI, catalogue-only) - both call `classify_archive_health`.
    pub health_issues: Gathered<&'a [HealthIssue]>,
    /// From `source_health_issues`.
    pub source_health: Gathered<&'a [SourceHealthIssue]>,
    /// From `diagnose_database`.
    pub database: Gathered<&'a DatabaseHealthReport>,
    /// From `assess_mount_root_safety`.
    pub mount_root_safety: Gathered<&'a MountRootSafety>,
    /// From an already-completed RetroArch discovery. Doctor never starts
    /// one.
    pub retroarch: Gathered<&'a RetroArchEnvironmentReport>,
    /// From `discover_shared_apply_history`.
    pub transactions: Gathered<&'a SharedHistoryReport>,
    /// From `plan_stale_mount_directories` - the read-only list of empty,
    /// unmounted folders left beneath the mount root.
    pub stale_mount_directories: Gathered<&'a [std::path::PathBuf]>,
    /// From `check_archive_index_freshness`, paired with the index path it
    /// was computed for.
    pub index_freshness: Gathered<(&'a ArchiveIndexFreshness, &'a std::path::Path)>,
    /// From `environment::assess_storage` - free space and mount mode for
    /// every filesystem EmuWiz depends on.
    pub storage: Gathered<&'a StorageAssessment>,
    /// From `profiles::assess_emulator_profiles`.
    pub emulator_profiles: Gathered<&'a ProfileAssessmentReport>,
    /// From `managed::scan_managed_entries`.
    pub managed_entries: Gathered<&'a ManagedEntryScan>,
    /// The free-space thresholds to apply. Not a `Gathered`: policy is always
    /// available, and the default is the documented one.
    pub free_space_policy: FreeSpacePolicy,
}

impl<'a> DoctorScanInputs<'a> {
    /// Every subsystem unavailable - the honest starting point. Callers
    /// replace only what they actually have.
    pub fn none_loaded() -> Self {
        Self {
            doctor_report: Gathered::NotLoaded("Doctor report has not been produced yet."),
            setup: Gathered::NotLoaded("Setup diagnostics have not run yet."),
            health_issues: Gathered::NotLoaded("The library has not been loaded yet."),
            source_health: Gathered::NotLoaded("Source folder status has not been loaded yet."),
            database: Gathered::NotLoaded("The catalogue database has not been inspected yet."),
            mount_root_safety: Gathered::NotLoaded("No mount root is configured or resolvable."),
            retroarch: Gathered::NotLoaded(
                "RetroArch profiles have not been discovered in this session.",
            ),
            transactions: Gathered::NotLoaded("Install history has not been loaded yet."),
            stale_mount_directories: Gathered::NotLoaded(
                "The mount root has not been inspected for leftover folders yet.",
            ),
            index_freshness: Gathered::NotLoaded("The archive index has not been checked yet."),
            storage: Gathered::NotLoaded(
                "Filesystem capacity and mount state have not been inspected yet.",
            ),
            emulator_profiles: Gathered::NotLoaded(
                "Emulator profiles have not been discovered in this session.",
            ),
            managed_entries: Gathered::NotLoaded(
                "EmuWiz-managed cheat entries have not been scanned yet.",
            ),
            free_space_policy: FreeSpacePolicy::default(),
        }
    }
}

/// The total outcome of resolving one finding by identity.
///
/// Deliberately total rather than `Option`: each way a lookup can fail is a
/// different refusal with a different explanation, and collapsing them would
/// make "that resource is not attached to that finding" indistinguishable
/// from "that finding does not exist".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingLookup<'a> {
    Found(&'a Finding),
    /// No finding in this scan has that id.
    UnknownId,
    /// Findings with that id exist, but none carries that affected resource.
    /// A supplied resource selects among existing findings; it can never
    /// introduce a new target.
    ResourceNotAttached,
    /// Several findings share that id and no resource was supplied, so
    /// acting would repair a guess.
    Ambiguous(usize),
}

impl<'a> FindingLookup<'a> {
    pub fn found(self) -> Option<&'a Finding> {
        match self {
            Self::Found(finding) => Some(finding),
            Self::UnknownId | Self::ResourceNotAttached | Self::Ambiguous(_) => None,
        }
    }
}

/// What one Doctor run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorScan {
    /// Deterministically ordered: severity, then category, then affected
    /// resource, then finding id.
    pub findings: Vec<Finding>,
    /// Which subsystems were actually checked, and why any were not.
    pub coverage: Vec<SubsystemCoverage>,
    /// Checks EmuWiz does not perform yet. Always the full list, so a
    /// clean result cannot be read as complete coverage.
    pub deferred: &'static [DeferredCheck],
    /// How many duplicate findings were merged away. Surfaced so the
    /// suppression is visible rather than mysterious.
    pub merged_duplicate_count: usize,
    /// Individual checks that were available but did not run, and why.
    pub not_checked: Vec<NotCheckedCheck>,
}

impl DoctorScan {
    /// [`DoctorSeverity::Healthy`] only when there is nothing actionable at
    /// all. Note that a healthy verdict still says nothing about the
    /// subsystems in [`Self::unavailable_subsystems`] or the checks in
    /// [`Self::deferred`].
    pub fn overall_severity(&self) -> DoctorSeverity {
        self.findings
            .iter()
            .map(|finding| finding.severity)
            .min_by_key(|severity| severity.rank())
            .unwrap_or(DoctorSeverity::Healthy)
    }

    pub fn is_healthy(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn count(&self, severity: DoctorSeverity) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    }

    /// Counts in display order, most severe first. Includes zeroes so the
    /// dashboard layout is stable between runs.
    pub fn counts(&self) -> Vec<(DoctorSeverity, usize)> {
        DoctorSeverity::ACTIONABLE
            .iter()
            .map(|severity| (*severity, self.count(*severity)))
            .collect()
    }

    pub fn blocking_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity.is_blocking())
            .count()
    }

    pub fn checked_subsystems(&self) -> Vec<&SubsystemCoverage> {
        self.coverage
            .iter()
            .filter(|entry| entry.was_checked())
            .collect()
    }

    pub fn unavailable_subsystems(&self) -> Vec<&SubsystemCoverage> {
        self.coverage
            .iter()
            .filter(|entry| !entry.was_checked())
            .collect()
    }

    /// Findings grouped for display, in stable category order. Categories
    /// with no findings are omitted.
    pub fn by_category(&self) -> Vec<(DoctorCategory, Vec<&Finding>)> {
        DoctorCategory::ALL
            .iter()
            .filter_map(|category| {
                let group: Vec<&Finding> = self
                    .findings
                    .iter()
                    .filter(|finding| finding.category == *category)
                    .collect();
                (!group.is_empty()).then_some((*category, group))
            })
            .collect()
    }

    /// The first finding with this id. Convenient, but note that several
    /// findings can legitimately share an id about *different* resources
    /// (one per leftover folder, one per missing archive), so a caller that
    /// is about to act must use [`Self::finding_for`] instead.
    pub fn finding(&self, id: &str) -> Option<&Finding> {
        self.findings.iter().find(|finding| finding.id == id)
    }

    pub fn findings_with_id(&self, id: &str) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.id == id)
            .collect()
    }

    /// Resolves exactly one finding by its full identity - the same
    /// `(id, affected resource)` pair duplicate suppression keys on.
    ///
    /// # A supplied resource can only *select*, never *introduce*
    ///
    /// `affected` is matched for exact equality against the `affected`
    /// [`EncodedPath::display`] of a finding this scan actually reproduced.
    /// It is a selector among existing findings and nothing else:
    ///
    /// - it cannot name a resource the finding does not carry, even if that
    ///   resource exists on disk and would itself be a legitimate target of
    ///   the same repair;
    /// - it cannot attach a resource to a finding whose `affected` is `None`;
    /// - it cannot borrow a resource from a finding with a different id;
    /// - there is no fallback: a non-matching resource resolves to
    ///   [`FindingLookup::ResourceNotAttached`], never to the first candidate.
    ///
    /// [`EncodedPath::display`]: crate::emulator_environment::EncodedPath::display
    pub fn finding_for(&self, id: &str, affected: Option<&str>) -> FindingLookup<'_> {
        let candidates = self.findings_with_id(id);
        if candidates.is_empty() {
            return FindingLookup::UnknownId;
        }
        match affected {
            Some(affected) => candidates
                .into_iter()
                .find(|finding| {
                    finding
                        .affected
                        .as_ref()
                        .is_some_and(|path| path.display == affected)
                })
                .map_or(FindingLookup::ResourceNotAttached, FindingLookup::Found),
            None if candidates.len() > 1 => FindingLookup::Ambiguous(candidates.len()),
            // `unwrap` is unreachable: emptiness was handled above.
            None => candidates
                .into_iter()
                .next()
                .map_or(FindingLookup::UnknownId, FindingLookup::Found),
        }
    }
}

/// Evaluates every available subsystem and returns a deterministic scan.
///
/// Pure: no I/O of any kind. Every subsystem is independent, so one
/// unavailable or failed input never prevents the others from being
/// reported. A failed input becomes a finding; a not-loaded input becomes a
/// coverage entry.
pub fn run_doctor_scan(inputs: &DoctorScanInputs<'_>) -> DoctorScan {
    let mut findings = Vec::new();
    let mut coverage = Vec::new();

    // The authoritative answer to "is the config file genuinely, confirmed
    // absent" - computed once, the same way `SetupDiagnostics.config_missing`
    // already computes it (`PathInspection::is_confirmed_missing`), rather
    // than re-derived per gatherer from OS error text. That text alone
    // cannot make this distinction: a broken symlink at the config path
    // produces the identical "No such file or directory" from a plain
    // `fs::read_to_string` that a genuinely missing file does. Using this
    // flag keeps `adapter_failure_finding` from softening a gatherer that
    // failed to read an ambiguous (possibly broken) config path, while
    // `SetupDiagnostics`'s own check for that same path correctly stays an
    // Error - see `a_failed_adapter_input_caused_by_an_ambiguous_config_path_stays_error`.
    let config_confirmed_missing = inputs
        .setup
        .as_ready()
        .is_some_and(|report| report.config_missing);

    // One closure per subsystem so the "collect, or record why not" shape
    // is identical everywhere and cannot drift between subsystems.
    macro_rules! subsystem {
        ($field:expr, $category:expr, $subsystem:expr, $adapt:expr) => {{
            coverage.push(SubsystemCoverage {
                category: $category,
                subsystem: $subsystem,
                status: $field.coverage_status(),
            });
            match &$field {
                Gathered::Ready(value) => findings.extend($adapt(value)),
                Gathered::Failed(reason) => findings.push(adapter_failure_finding(
                    $category,
                    $subsystem,
                    reason.clone(),
                    config_confirmed_missing,
                )),
                Gathered::NotLoaded(_) => {}
            }
        }};
    }

    subsystem!(
        inputs.doctor_report,
        DoctorCategory::Configuration,
        DoctorSubsystem::DoctorReport,
        |report: &&DoctorReport| findings_from_doctor_report(report)
    );
    subsystem!(
        inputs.setup,
        DoctorCategory::Configuration,
        DoctorSubsystem::SetupDiagnostics,
        |report: &&SetupDiagnostics| findings_from_setup_diagnostics(report)
    );
    subsystem!(
        inputs.mount_root_safety,
        DoctorCategory::MountRoot,
        DoctorSubsystem::DestinationSafety,
        |safety: &&MountRootSafety| findings_from_mount_root_safety(safety)
    );
    subsystem!(
        inputs.health_issues,
        DoctorCategory::Library,
        DoctorSubsystem::ArchiveHealth,
        |issues: &&[HealthIssue]| findings_from_health_issues(issues)
    );
    subsystem!(
        inputs.source_health,
        DoctorCategory::Sources,
        DoctorSubsystem::SourceHealth,
        |issues: &&[SourceHealthIssue]| findings_from_source_health(issues)
    );
    subsystem!(
        inputs.database,
        DoctorCategory::Database,
        DoctorSubsystem::DatabaseDiagnostics,
        |report: &&DatabaseHealthReport| findings_from_database_report(report)
    );
    subsystem!(
        inputs.retroarch,
        DoctorCategory::Emulators,
        DoctorSubsystem::RetroArchEnvironment,
        |report: &&RetroArchEnvironmentReport| findings_from_retroarch_environment(report)
    );
    subsystem!(
        inputs.transactions,
        DoctorCategory::Transactions,
        DoctorSubsystem::SharedTransactions,
        |report: &&SharedHistoryReport| findings_from_transaction_history(report)
    );
    subsystem!(
        inputs.stale_mount_directories,
        DoctorCategory::MountRoot,
        DoctorSubsystem::MountRootCleanup,
        |stale: &&[std::path::PathBuf]| findings_from_stale_mount_directories(stale)
    );
    subsystem!(
        inputs.index_freshness,
        DoctorCategory::Library,
        DoctorSubsystem::ArchiveIndex,
        |input: &(&ArchiveIndexFreshness, &std::path::Path)| findings_from_index_freshness(
            input.0, input.1
        )
    );
    // Storage is two subsystems over one gathered assessment: capacity and
    // mount state are different questions with different severities, and a
    // person should be able to see one covered and the other not.
    let policy = inputs.free_space_policy;
    subsystem!(
        inputs.storage,
        DoctorCategory::Storage,
        DoctorSubsystem::FilesystemCapacity,
        |assessment: &&StorageAssessment| findings_from_free_space(assessment, &policy)
    );
    subsystem!(
        inputs.storage,
        DoctorCategory::Filesystems,
        DoctorSubsystem::FilesystemMountState,
        |assessment: &&StorageAssessment| findings_from_read_only_filesystems(assessment)
    );
    subsystem!(
        inputs.emulator_profiles,
        DoctorCategory::EmulatorProfiles,
        DoctorSubsystem::EmulatorProfiles,
        |report: &&ProfileAssessmentReport| findings_from_emulator_profiles(report)
    );
    subsystem!(
        inputs.managed_entries,
        DoctorCategory::ManagedEntries,
        DoctorSubsystem::ManagedEntries,
        |scan: &&ManagedEntryScan| findings_from_managed_entries(scan)
    );

    let mut not_checked = inputs
        .setup
        .as_ready()
        .map(|setup| not_checked_from_setup_diagnostics(setup))
        .unwrap_or_default();
    if let Some(assessment) = inputs.storage.as_ready() {
        not_checked.extend(not_checked_from_storage(assessment));
    }
    if let Some(report) = inputs.emulator_profiles.as_ready() {
        not_checked.extend(not_checked_from_emulator_profiles(report));
    }
    if let Some(scan) = inputs.managed_entries.as_ready() {
        not_checked.extend(not_checked_from_managed_entries(scan));
    }

    let before = findings.len();
    let mut findings = merge_duplicates(findings);
    let merged_duplicate_count = before - findings.len();

    findings.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    coverage.sort_by_key(|entry| (entry.category, entry.subsystem));

    DoctorScan {
        findings,
        coverage,
        deferred: DEFERRED_CHECKS,
        merged_duplicate_count,
        not_checked,
    }
}

/// Whether a gatherer's failure reason describes an input that is simply
/// absent (a config file or catalogue database that has never been
/// created) rather than one that exists but is broken.
///
/// Two different kinds of evidence are used, deliberately not interchanged:
///
/// - Gatherers that fail because *the config file itself* could not be read
///   (`"configuration could not be read"` / `"source folders could not be
///   listed"`, from `Config::load_default`/`list_source_folder_views_default`
///   failing) are gated on `config_confirmed_missing`, the authoritative
///   answer `SetupDiagnostics` already computed. Their own OS error text
///   cannot make this distinction itself: a broken symlink at the config
///   path produces the identical "No such file or directory" that a
///   genuinely missing file does, and softening on that text alone would
///   contradict `SetupDiagnostics`'s own, correctly-Error verdict for the
///   same ambiguous path.
/// - Every other gatherer failure (a missing catalogue database, in
///   practice) is judged on its own wording, matching the database layer's
///   "does not exist" phrasing - `database_severity`'s deliberate exception
///   for `DatabaseDiagnosticCode::MissingDatabase` establishes that this
///   specific absence is always expected, independent of config state.
fn failure_reason_is_expected_first_run_absence(
    reason: &str,
    config_confirmed_missing: bool,
) -> bool {
    let describes_a_missing_config_read = reason.starts_with("configuration could not be read")
        || reason.starts_with("source folders could not be listed");
    if describes_a_missing_config_read {
        return config_confirmed_missing;
    }
    reason.contains("No such file or directory") || reason.contains("does not exist")
}

/// One finding per failed subsystem.
///
/// The id is namespaced by subsystem (`doctor.adapter_failed.archive_health`
/// rather than a bare `doctor.adapter_failed`) precisely so two different
/// subsystems failing in the same run stay two distinct findings. Merging
/// them would hide genuinely distinct evidence behind one title.
fn adapter_failure_finding(
    category: DoctorCategory,
    subsystem: DoctorSubsystem,
    reason: String,
    config_confirmed_missing: bool,
) -> Finding {
    let severity =
        if failure_reason_is_expected_first_run_absence(&reason, config_confirmed_missing) {
            DoctorSeverity::Info
        } else {
            DoctorSeverity::Error
        };
    let mut finding = Finding::new(
        format!("doctor.adapter_failed.{}", subsystem.slug()),
        DoctorCategory::Doctor,
        DoctorSubsystem::DoctorRunner,
        severity,
        format!("The {} check could not run", subsystem.label()),
        format!(
            "This scan does not cover {} because its input could not be collected.",
            category.label().to_lowercase()
        ),
    );
    finding.evidence.push(reason);
    finding
        .evidence
        .push(format!("Subsystem: {}", subsystem.label()));
    finding
}

/// Merges findings that describe the same fault about the same resource.
///
/// Identity is `(finding id, affected resource)` - never the wording. Two
/// findings with the same id about different paths stay separate, because
/// they are different problems.
///
/// When two reports describe one fault:
///
/// - the **highest severity wins** (a warning can never mask an error);
/// - evidence from both is preserved, deduplicated, with the surviving
///   finding's own evidence first;
/// - `why_it_matters` / `next_step` are filled in from the other finding
///   when the survivor lacks them - this is what lets the richer
///   `SetupDiagnostic` prose reach a fault the terser `DoctorReport` also
///   found;
/// - the survivor's `subsystem` is kept as the canonical source, and the
///   merged-away subsystem is recorded in evidence so provenance is not
///   lost;
/// - `recovery` is filled in from the other finding when the survivor has
///   none.
///
/// Deterministic: input order is fixed by the runner, and merging never
/// reorders.
pub(super) fn merge_duplicates(findings: Vec<Finding>) -> Vec<Finding> {
    let mut merged: Vec<Finding> = Vec::with_capacity(findings.len());
    for finding in findings {
        let existing = merged
            .iter_mut()
            .find(|candidate| candidate.duplicate_key() == finding.duplicate_key());
        match existing {
            Some(existing) => merge_into(existing, finding),
            None => merged.push(finding),
        }
    }
    merged
}

fn merge_into(existing: &mut Finding, other: Finding) {
    // Highest severity wins. `rank` is lower for more severe.
    if other.severity.rank() < existing.severity.rank() {
        existing.severity = other.severity;
    }
    if existing.why_it_matters.is_none() {
        existing.why_it_matters = other.why_it_matters;
    }
    if existing.next_step.is_none() {
        existing.next_step = other.next_step;
    }
    if existing.recovery.is_none() {
        existing.recovery = other.recovery;
    }
    if other.subsystem != existing.subsystem {
        existing
            .evidence
            .push(format!("Also reported by: {}", other.subsystem.label()));
    }
    for item in other.evidence {
        if !existing.evidence.contains(&item) {
            existing.evidence.push(item);
        }
    }
    // The other finding's explanation is kept only when it adds something.
    if other.explanation != existing.explanation && !existing.evidence.contains(&other.explanation)
    {
        existing.evidence.push(other.explanation);
    }
}
