//! Doctor Stage 1B: exposing existing safe repairs.
//!
//! # What this module is
//!
//! A closed set of four repairs, each of which does nothing but call a
//! function that already exists and is already tested elsewhere in
//! ArchiveFS:
//!
//! | Action | Calls |
//! |---|---|
//! | [`DoctorRepairAction::CleanMountRoot`] | `crate::clean_mount_root` |
//! | [`DoctorRepairAction::CleanMountPath`] | `crate::cleanup_selected_mount_tree` |
//! | [`DoctorRepairAction::RetryMount`] | `crate::mount_one_archive_path` |
//! | [`DoctorRepairAction::RebuildIndex`] | `crate::build_and_write_archive_index_to` |
//!
//! # What this module deliberately is not
//!
//! - **Not a command dispatcher.** [`DoctorRepairAction`] is a fieldless
//!   enum. It carries no path, no string, no closure and no argument, so a
//!   finding cannot smuggle a target into a repair. The target is always
//!   re-derived from the *current* state of the finding named in the
//!   request.
//! - **Not a generic executor.** There is no way to express a repair
//!   ArchiveFS does not already implement. Adding one means adding a variant
//!   here and wiring it to a real function, in a reviewed change.
//! - **Not automatic.** Every repair requires
//!   [`DoctorRepairRequest::confirmed`], including the ones classified
//!   `Safe`. Expanding a finding or pressing Run Doctor can never mutate
//!   anything.
//!
//! # Revalidation
//!
//! A path captured during a scan is never trusted. [`execute_doctor_repair`]
//! re-resolves the finding against live state and refuses on any of:
//! unknown action, unknown finding, action not offered for that finding,
//! missing confirmation, the finding no longer existing, the affected
//! resource's identity having changed, a target under a configured source
//! root, a target outside the mount root, or a symlink escape. See
//! [`DoctorRepairRejection`].

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{DoctorScan, DoctorSeverity, Finding, FindingLookup};
use crate::emulator_environment::EncodedPath;
use crate::{
    ArchiveHealth, Config, MountState, build_and_write_archive_index_to, clean_mount_root,
    cleanup_selected_mount_tree, mount_one_archive_path, plan_stale_mount_directories,
};

// --- The closed action set ------------------------------------------------

/// Every repair Doctor can perform. Fieldless on purpose: see the module
/// documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairAction {
    CleanMountRoot,
    CleanMountPath,
    RetryMount,
    RebuildIndex,
}

/// How much trust a repair needs before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairRisk {
    /// Provably cannot destroy user data. Still requires confirmation in
    /// Stage 1B - "safe" describes the blast radius, not the consent model.
    Safe,
    /// Mutates real state in a way a person should approve knowingly.
    NeedsConfirmation,
}

/// Whether an existing rollback mechanism covers this repair. Never
/// optimistic: `Unavailable` is the honest default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairUndo {
    /// Nothing to undo, because nothing was destroyed - an empty directory
    /// removal has no content to restore.
    NothingToUndo,
    /// No existing mechanism reverses this. The UI must say so plainly.
    Unavailable,
    /// An existing mechanism reverses it; the text names where.
    Existing(&'static str),
}

impl DoctorRepairUndo {
    pub fn label(self) -> &'static str {
        match self {
            Self::NothingToUndo => "Undo not needed: only empty directories are removed.",
            Self::Unavailable => "Undo unavailable.",
            Self::Existing(where_to) => where_to,
        }
    }
}

/// The complete, static description of one repair. Everything the
/// confirmation screen and the History entry need, declared in one place so
/// the GUI and the CLI cannot describe a repair differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DoctorRepairSpec {
    pub action: DoctorRepairAction,
    /// Stable, machine-readable. Part of the CLI's contract.
    pub id: &'static str,
    pub title: &'static str,
    /// The exact existing function this repair invokes. Shown in the
    /// confirmation screen so the user can see nothing new is being run.
    pub invokes: &'static str,
    pub risk: DoctorRepairRisk,
    /// Always true in Stage 1B.
    pub confirmation_required: bool,
    /// The exact mutation, in plain language.
    pub expected_mutation: &'static str,
    /// What this repair will not touch.
    pub never_touches: &'static str,
    /// How success is verified afterwards.
    pub verification: &'static str,
    pub undo: DoctorRepairUndo,
    /// True when the repair performs a full library scan as part of its
    /// existing implementation. The user must be told.
    pub performs_library_scan: bool,
}

impl DoctorRepairAction {
    pub const ALL: [Self; 4] = [
        Self::CleanMountRoot,
        Self::CleanMountPath,
        Self::RetryMount,
        Self::RebuildIndex,
    ];

    /// Parses a stable action id. Anything else is rejected - there is no
    /// fallback and no fuzzy matching, so an arbitrary string can never
    /// become an action.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.spec().id == id)
    }

    pub fn spec(self) -> DoctorRepairSpec {
        match self {
            Self::CleanMountRoot => DoctorRepairSpec {
                action: self,
                id: "clean_mount_root",
                title: "Clean up leftover mount folders",
                invokes: "archivefs_core::clean_mount_root",
                risk: DoctorRepairRisk::Safe,
                confirmation_required: true,
                expected_mutation: "Removes empty, unmounted folders beneath the configured mount root, and their now-empty parents up to (but never including) the mount root itself.",
                never_touches: "No file is ever removed - only empty directories. Nothing under a configured source folder, no archive, no ROM, no emulator profile, and no active mount point is touched.",
                verification: "The mount-root scan is re-run and the remaining leftover folders are re-listed.",
                undo: DoctorRepairUndo::NothingToUndo,
                // `clean_mount_root` derives planned mount points via
                // `ArchiveScanner::mount_plans()`.
                performs_library_scan: true,
            },
            Self::CleanMountPath => DoctorRepairSpec {
                action: self,
                id: "clean_mount_path",
                title: "Remove this leftover mount folder",
                invokes: "archivefs_core::cleanup_selected_mount_tree",
                risk: DoctorRepairRisk::Safe,
                confirmation_required: true,
                expected_mutation: "Removes this one empty folder, then each of its parents that becomes empty, stopping at the configured mount root.",
                never_touches: "A folder containing anything at all is refused. Symlinks are never followed, active mount points are never entered, and the mount root itself is never removed.",
                verification: "The mount-root scan is re-run and this exact folder is checked for.",
                undo: DoctorRepairUndo::NothingToUndo,
                performs_library_scan: false,
            },
            Self::RetryMount => DoctorRepairSpec {
                action: self,
                id: "retry_mount",
                title: "Try mounting this archive again",
                invokes: "archivefs_core::mount_one_archive_path",
                risk: DoctorRepairRisk::Safe,
                confirmation_required: true,
                expected_mutation: "Mounts this one archive read-only at its planned mount point, using the same mount path the Mount page uses.",
                never_touches: "The archive is opened read-only and never modified. No other archive is mounted or unmounted.",
                verification: "The archive's health is re-derived and checked for a retryable failure.",
                undo: DoctorRepairUndo::Existing(
                    "Unmount from the Active Mounts page reverses this.",
                ),
                performs_library_scan: false,
            },
            Self::RebuildIndex => DoctorRepairSpec {
                action: self,
                id: "rebuild_index",
                title: "Rebuild the archive index",
                invokes: "archivefs_core::build_and_write_archive_index_to",
                risk: DoctorRepairRisk::NeedsConfirmation,
                confirmation_required: true,
                expected_mutation: "Rebuilds the archive search index (index.json) from a fresh scan of every configured source folder, then replaces the old index file atomically.",
                never_touches: "Only the index file is written. No archive, no ROM, no catalogue row and no emulator file is changed. The previous index stays in place if the rebuild fails.",
                verification: "The rebuilt index's freshness is re-checked against the files on disk.",
                undo: DoctorRepairUndo::Unavailable,
                // `build_archive_index` walks every configured source folder.
                performs_library_scan: true,
            },
        }
    }
}

// --- Request, rejection, outcome -----------------------------------------

/// One repair the caller wants performed. The target is *not* here: it is
/// re-derived from `finding_id` against live state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRepairRequest {
    pub action: DoctorRepairAction,
    /// The stable id of the finding this repair is for.
    pub finding_id: String,
    /// The affected resource, when the id alone does not identify one
    /// finding. This is **not** a repair target: it is matched against the
    /// current scan's findings, so a value that names nothing in the scan is
    /// rejected as unknown rather than acted upon.
    pub affected: Option<String>,
    /// Explicit approval. Without it every repair is rejected.
    pub confirmed: bool,
    /// Validate everything, mutate nothing.
    pub dry_run: bool,
}

/// Why a repair was refused before anything was changed. Each variant is a
/// distinct safety gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairRejection {
    /// The named finding is not in the current scan any more.
    UnknownFinding,
    /// Several findings share that id and no resource was given, so acting
    /// would repair a guess.
    AmbiguousFinding,
    /// A resource was named that the finding does not carry. A supplied
    /// resource selects among findings this scan reproduced; it can never
    /// introduce a new target, even one that exists on disk and would
    /// otherwise be a legitimate target of the same repair.
    ResourceNotAttachedToFinding,
    /// The finding exists but does not offer this action.
    ActionNotOfferedForFinding,
    /// The finding offers a repair but carries no affected resource, so
    /// there is nothing to re-resolve.
    MissingAffectedResource,
    ConfirmationMissing,
    /// The condition the finding described no longer holds.
    StaleFinding,
    /// The affected path exists but is not the same object it was.
    ResourceIdentityChanged,
    /// The target is a configured source folder, or inside one.
    PathUnderSourceRoot,
    /// The target is not beneath the configured mount root.
    PathOutsideMountRoot,
    /// The target, or a component of it, is a symlink, or resolves outside
    /// the expected boundary.
    SymlinkEscape,
    /// The archive's health is not one retrying can help.
    NotRetryable,
    /// The archive named by the finding is gone.
    SourceMissing,
    /// The mount point is already in use.
    ConflictingActiveMount,
    /// The mount root is not configured or not usable.
    MountRootUnavailable,
}

impl DoctorRepairRejection {
    pub fn explanation(self) -> &'static str {
        match self {
            Self::UnknownFinding => {
                "That finding is not in the current Doctor results. Run Doctor again."
            }
            Self::AmbiguousFinding => {
                "Several findings share that identifier. Name the exact resource to repair."
            }
            Self::ResourceNotAttachedToFinding => {
                "That resource is not the one this finding reported. A resource can only pick out a finding Doctor already found; it cannot point a repair at something else. Run Doctor again and use the resource it reports."
            }
            Self::ActionNotOfferedForFinding => "That repair is not offered for that finding.",
            Self::MissingAffectedResource => {
                "That finding names no specific resource, so there is nothing to repair."
            }
            Self::ConfirmationMissing => "This repair changes real state and was not confirmed.",
            Self::StaleFinding => {
                "The problem this repair addresses is no longer present. Nothing was changed."
            }
            Self::ResourceIdentityChanged => {
                "The affected file or folder is not the same one Doctor saw. Nothing was changed."
            }
            Self::PathUnderSourceRoot => {
                "That path is inside a configured source folder. ArchiveFS never modifies anything there."
            }
            Self::PathOutsideMountRoot => {
                "That path is not inside the configured mount root, so this repair refuses to touch it."
            }
            Self::SymlinkEscape => {
                "That path involves a symbolic link, so it cannot be verified as safe."
            }
            Self::NotRetryable => "This archive's last failure is not one that retrying can fix.",
            Self::SourceMissing => "The archive is no longer where Doctor found it.",
            Self::ConflictingActiveMount => "Something is already mounted at that location.",
            Self::MountRootUnavailable => "The configured mount root is missing or unusable.",
        }
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairStatus {
    /// Every gate passed and nothing was changed, because this was a dry run.
    DryRun,
    Rejected,
    Succeeded,
    Failed,
}

/// Whether the repair actually resolved the finding. Never inferred from the
/// underlying function returning `Ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairVerification {
    /// The originating check was re-run and the finding is gone.
    Verified,
    /// The repair reported success but the finding is still there.
    FindingRemains,
    /// The check could not be re-run, so success cannot be claimed.
    CouldNotComplete,
    /// Not attempted: the repair was rejected, failed, or was a dry run.
    NotAttempted,
}

impl DoctorRepairVerification {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "Repair verified",
            Self::FindingRemains => "Repair completed but finding remains",
            Self::CouldNotComplete => "Verification could not complete",
            Self::NotAttempted => "Not verified",
        }
    }
}

/// One History-ready record of an attempted repair. Plain data, so the GUI's
/// existing `OperationHistory` and the CLI's log can both render it without
/// a new store or a migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorRepairRecord {
    pub action_id: &'static str,
    pub action_title: &'static str,
    pub finding_id: String,
    pub affected: Option<EncodedPath>,
    pub confirmed: bool,
    pub dry_run: bool,
    pub status: DoctorRepairStatus,
    pub verification: DoctorRepairVerification,
    /// Exactly what changed on disk. Empty for a rejection or a dry run.
    pub changed_paths: Vec<EncodedPath>,
    pub undo: DoctorRepairUndo,
    /// One-line human summary, suitable for a History row.
    pub summary: String,
    pub rejection: Option<DoctorRepairRejection>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorRepairOutcome {
    pub action: DoctorRepairAction,
    pub spec: DoctorRepairSpec,
    #[serde(flatten)]
    pub record: DoctorRepairRecord,
}

impl DoctorRepairOutcome {
    pub fn succeeded(&self) -> bool {
        self.record.status == DoctorRepairStatus::Succeeded
    }
}

/// Live state a repair needs. Borrowed, and explicitly supplied, so nothing
/// here reads `$HOME` behind the caller's back and tests never touch a real
/// installation.
#[derive(Debug)]
pub struct DoctorRepairContext<'a> {
    pub config: &'a Config,
    /// The scan the finding came from.
    pub scan: &'a DoctorScan,
    /// Where the archive index lives. Supplied rather than derived so a test
    /// can rebuild an index without depending on `$HOME`.
    pub index_path: &'a Path,
}

// --- Execution ------------------------------------------------------------

/// Validates and, unless `dry_run`, performs one repair.
///
/// The order of gates matters and is deliberate: cheap identity checks
/// first, then confirmation, then live revalidation, and only then the
/// existing repair function. Nothing is mutated until every gate has passed.
pub fn execute_doctor_repair(
    request: &DoctorRepairRequest,
    context: &DoctorRepairContext<'_>,
) -> DoctorRepairOutcome {
    let spec = request.action.spec();

    // Gate 1: exactly one finding must still match, by full identity.
    //
    // `request.affected` is consumed *here and nowhere else*: every step
    // below reads `finding.affected`, the value this scan actually recorded.
    // A resource the finding does not carry is refused now, before any
    // planning and long before any mutation - it cannot become a target.
    let finding = match context
        .scan
        .finding_for(&request.finding_id, request.affected.as_deref())
    {
        FindingLookup::Found(finding) => finding,
        FindingLookup::UnknownId => {
            return reject(request, spec, None, DoctorRepairRejection::UnknownFinding);
        }
        FindingLookup::ResourceNotAttached => {
            return reject(
                request,
                spec,
                // Deliberately not echoed into the record: an unmatched
                // user-supplied path must never appear where a validated
                // target belongs.
                None,
                DoctorRepairRejection::ResourceNotAttachedToFinding,
            );
        }
        FindingLookup::Ambiguous(_) => {
            return reject(request, spec, None, DoctorRepairRejection::AmbiguousFinding);
        }
    };
    // Gate 2: this action must be the one that finding offers.
    if finding.repair != Some(request.action) {
        return reject(
            request,
            spec,
            finding.affected.clone(),
            DoctorRepairRejection::ActionNotOfferedForFinding,
        );
    }
    // Gate 3: explicit confirmation, for every action without exception -
    // except a dry run, which has nothing to consent to because it changes
    // nothing. A dry run still passes through every gate below.
    if !request.confirmed && !request.dry_run {
        return reject(
            request,
            spec,
            finding.affected.clone(),
            DoctorRepairRejection::ConfirmationMissing,
        );
    }

    // Gates 4-9 are per-action, because "still valid" means something
    // different for a leftover folder than for a failed mount.
    let validated = match request.action {
        DoctorRepairAction::CleanMountRoot => validate_clean_mount_root(context),
        DoctorRepairAction::CleanMountPath => validate_clean_mount_path(finding, context),
        DoctorRepairAction::RetryMount => validate_retry_mount(finding, context),
        DoctorRepairAction::RebuildIndex => validate_rebuild_index(finding, context),
    };
    let target = match validated {
        Ok(target) => target,
        Err(rejection) => return reject(request, spec, finding.affected.clone(), rejection),
    };

    if request.dry_run {
        return DoctorRepairOutcome {
            action: request.action,
            spec,
            record: DoctorRepairRecord {
                action_id: spec.id,
                action_title: spec.title,
                finding_id: request.finding_id.clone(),
                affected: finding.affected.clone(),
                confirmed: request.confirmed,
                dry_run: true,
                status: DoctorRepairStatus::DryRun,
                verification: DoctorRepairVerification::NotAttempted,
                changed_paths: Vec::new(),
                undo: spec.undo,
                summary: format!(
                    "Dry run: {} would run against {}. Nothing was changed.",
                    spec.title,
                    target.describe()
                ),
                rejection: None,
                error: None,
            },
        };
    }

    // Only now does anything change.
    let performed = match &target {
        RepairTarget::MountRoot => clean_mount_root(context.config)
            .map(RepairEffect::Removed)
            .map_err(|error| error.to_string()),
        RepairTarget::MountPath(path) => cleanup_selected_mount_tree(context.config, path)
            .map(RepairEffect::Removed)
            .map_err(|error| error.to_string()),
        RepairTarget::Archive(path) => mount_one_archive_path(context.config, path)
            .map(|plan| RepairEffect::Mounted(plan.mount_path))
            .map_err(|error| error.to_string()),
        RepairTarget::Index => build_and_write_archive_index_to(context.config, context.index_path)
            .map(|index| RepairEffect::Rebuilt(index.archives.len()))
            .map_err(|error| error.to_string()),
    };

    match performed {
        Ok(effect) => {
            let (verification, verification_detail) =
                verify_repair(request.action, &target, context);
            DoctorRepairOutcome {
                action: request.action,
                spec,
                record: DoctorRepairRecord {
                    action_id: spec.id,
                    action_title: spec.title,
                    finding_id: request.finding_id.clone(),
                    affected: finding.affected.clone(),
                    confirmed: true,
                    dry_run: false,
                    status: DoctorRepairStatus::Succeeded,
                    verification,
                    changed_paths: effect.changed_paths(),
                    undo: spec.undo,
                    summary: format!(
                        "{}: {}. {}{}",
                        spec.title,
                        effect.describe(),
                        verification.label(),
                        verification_detail
                    ),
                    rejection: None,
                    error: None,
                },
            }
        }
        Err(error) => DoctorRepairOutcome {
            action: request.action,
            spec,
            record: DoctorRepairRecord {
                action_id: spec.id,
                action_title: spec.title,
                finding_id: request.finding_id.clone(),
                affected: finding.affected.clone(),
                confirmed: true,
                dry_run: false,
                status: DoctorRepairStatus::Failed,
                verification: DoctorRepairVerification::NotAttempted,
                changed_paths: Vec::new(),
                undo: spec.undo,
                summary: format!("{} failed. State was left unchanged.", spec.title),
                rejection: None,
                error: Some(error),
            },
        },
    }
}

/// The revalidated target. Built only from live state, never from the
/// finding's stored path alone.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RepairTarget {
    MountRoot,
    MountPath(PathBuf),
    Archive(PathBuf),
    Index,
}

impl RepairTarget {
    fn describe(&self) -> String {
        match self {
            Self::MountRoot => "the configured mount root".to_string(),
            Self::MountPath(path) => path.display().to_string(),
            Self::Archive(path) => path.display().to_string(),
            Self::Index => "the archive index".to_string(),
        }
    }
}

enum RepairEffect {
    Removed(Vec<PathBuf>),
    Mounted(PathBuf),
    Rebuilt(usize),
}

impl RepairEffect {
    fn changed_paths(&self) -> Vec<EncodedPath> {
        match self {
            Self::Removed(paths) => paths
                .iter()
                .map(|path| EncodedPath::from_path(path))
                .collect(),
            Self::Mounted(path) => vec![EncodedPath::from_path(path)],
            Self::Rebuilt(_) => Vec::new(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Removed(paths) if paths.is_empty() => "no folder needed removing".to_string(),
            Self::Removed(paths) => format!(
                "removed {} empty folder(s): {}",
                paths.len(),
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Mounted(path) => format!("mounted at {}", path.display()),
            Self::Rebuilt(count) => format!("indexed {count} archive(s)"),
        }
    }
}

// --- Per-action revalidation ---------------------------------------------

fn validate_clean_mount_root(
    context: &DoctorRepairContext<'_>,
) -> Result<RepairTarget, DoctorRepairRejection> {
    if !context.config.mount_root.is_dir() {
        return Err(DoctorRepairRejection::MountRootUnavailable);
    }
    // The finding must still be true: there must still be something to clean.
    match plan_stale_mount_directories(context.config) {
        Ok(stale) if stale.is_empty() => Err(DoctorRepairRejection::StaleFinding),
        Ok(_) => Ok(RepairTarget::MountRoot),
        Err(_) => Err(DoctorRepairRejection::MountRootUnavailable),
    }
}

fn validate_clean_mount_path(
    finding: &Finding,
    context: &DoctorRepairContext<'_>,
) -> Result<RepairTarget, DoctorRepairRejection> {
    let path = affected_path(finding)?;
    guard_against_source_roots(&path, context.config)?;
    guard_inside_mount_root(&path, context.config)?;
    guard_no_symlink_components(&path)?;

    // Re-resolve: this exact folder must still be one the cleanup would
    // remove. `plan_stale_mount_directories` applies the same predicate the
    // remover applies, so agreeing with it here is agreeing with the
    // remover.
    let stale = plan_stale_mount_directories(context.config)
        .map_err(|_| DoctorRepairRejection::MountRootUnavailable)?;
    if !stale.iter().any(|candidate| candidate == &path) {
        return Err(DoctorRepairRejection::StaleFinding);
    }
    Ok(RepairTarget::MountPath(path))
}

fn validate_retry_mount(
    finding: &Finding,
    context: &DoctorRepairContext<'_>,
) -> Result<RepairTarget, DoctorRepairRejection> {
    let path = affected_path(finding)?;
    // The archive must still be there, and still be the same file.
    let metadata = fs::symlink_metadata(&path).map_err(|_| DoctorRepairRejection::SourceMissing)?;
    if metadata.file_type().is_symlink() {
        return Err(DoctorRepairRejection::SymlinkEscape);
    }
    if !metadata.is_file() {
        return Err(DoctorRepairRejection::ResourceIdentityChanged);
    }
    // The finding must still describe a retryable failure. `mounts
    // .retryable_failure` is only ever produced from
    // `ArchiveHealth::is_retryable`, so re-checking the current records
    // re-checks that same rule rather than a second opinion.
    let records = crate::current_archive_records(context.config)
        .map_err(|_| DoctorRepairRejection::SourceMissing)?;
    let record = records
        .iter()
        .find(|record| record.mount_plan.archive.path == path)
        .ok_or(DoctorRepairRejection::SourceMissing)?;
    if !retryable_health(record.health) {
        return Err(DoctorRepairRejection::NotRetryable);
    }
    if record.mount_state == MountState::Mounted {
        return Err(DoctorRepairRejection::StaleFinding);
    }
    // A different archive must not already own the mount point.
    let mount_path = &record.mount_plan.mount_path;
    guard_inside_mount_root(mount_path, context.config)?;
    if mount_path.exists() {
        let occupied = fs::read_dir(mount_path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(true);
        if occupied {
            return Err(DoctorRepairRejection::ConflictingActiveMount);
        }
    }
    Ok(RepairTarget::Archive(path))
}

fn validate_rebuild_index(
    finding: &Finding,
    context: &DoctorRepairContext<'_>,
) -> Result<RepairTarget, DoctorRepairRejection> {
    // The index path is supplied by the caller (always `default_index_path()`
    // in production; injected in tests), which makes it the one target not
    // derived from the finding. So require the finding to name it: the
    // index-freshness adapter records the exact path it checked, and this
    // refuses to rebuild anything else. Together with `affected_path`'s lossy
    // check, a repair can only ever write the index the finding reported.
    let named = affected_path(finding)?;
    if named != context.index_path {
        return Err(DoctorRepairRejection::ResourceNotAttachedToFinding);
    }
    guard_against_source_roots(context.index_path, context.config)?;
    if !context.index_path.is_absolute() {
        return Err(DoctorRepairRejection::PathOutsideMountRoot);
    }
    if let Ok(metadata) = fs::symlink_metadata(context.index_path)
        && metadata.file_type().is_symlink()
    {
        return Err(DoctorRepairRejection::SymlinkEscape);
    }
    // At least one configured source folder must still be readable, or a
    // rebuild would replace a good index with an empty one.
    if !context
        .config
        .source_folders
        .iter()
        .any(|folder| folder.is_dir())
    {
        return Err(DoctorRepairRejection::SourceMissing);
    }
    Ok(RepairTarget::Index)
}

// --- Shared safety guards -----------------------------------------------

fn affected_path(finding: &Finding) -> Result<PathBuf, DoctorRepairRejection> {
    let affected = finding
        .affected
        .as_ref()
        .ok_or(DoctorRepairRejection::MissingAffectedResource)?;
    if affected.lossy {
        // A lossily-rendered path cannot be turned back into the exact
        // bytes on disk, so it can never be a repair target.
        return Err(DoctorRepairRejection::ResourceIdentityChanged);
    }
    Ok(PathBuf::from(&affected.display))
}

/// Refuses a configured source folder, and anything inside one. This is the
/// hard boundary: ArchiveFS never modifies the user's library.
fn guard_against_source_roots(path: &Path, config: &Config) -> Result<(), DoctorRepairRejection> {
    for source in &config.source_folders {
        if path == source.as_path() || path.starts_with(source) {
            return Err(DoctorRepairRejection::PathUnderSourceRoot);
        }
        // Also compare resolved forms, so a symlinked source folder cannot
        // be sidestepped.
        if let (Ok(resolved_path), Ok(resolved_source)) =
            (fs::canonicalize(path), fs::canonicalize(source))
            && (resolved_path == resolved_source || resolved_path.starts_with(&resolved_source))
        {
            return Err(DoctorRepairRejection::PathUnderSourceRoot);
        }
    }
    Ok(())
}

fn guard_inside_mount_root(path: &Path, config: &Config) -> Result<(), DoctorRepairRejection> {
    let root = &config.mount_root;
    if path == root.as_path() || !path.starts_with(root) {
        return Err(DoctorRepairRejection::PathOutsideMountRoot);
    }
    // Canonicalised containment, so a symlink cannot point outside.
    let (Ok(resolved_root), Ok(resolved_path)) = (fs::canonicalize(root), fs::canonicalize(path))
    else {
        // A path that cannot be resolved cannot be proven safe. The one
        // legitimate case - the target no longer exists - is caught by the
        // per-action staleness check instead.
        return Ok(());
    };
    if resolved_path == resolved_root || !resolved_path.starts_with(&resolved_root) {
        return Err(DoctorRepairRejection::SymlinkEscape);
    }
    Ok(())
}

fn guard_no_symlink_components(path: &Path) -> Result<(), DoctorRepairRejection> {
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(DoctorRepairRejection::SymlinkEscape);
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DoctorRepairRejection::SymlinkEscape);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

fn retryable_health(health: ArchiveHealth) -> bool {
    // Reuses the existing rule rather than restating it.
    health.is_retryable()
}

// --- Post-repair verification -------------------------------------------

/// Re-runs **only** the check that produced the finding, and reports what it
/// now says. Never infers success from the repair function returning `Ok`.
fn verify_repair(
    action: DoctorRepairAction,
    target: &RepairTarget,
    context: &DoctorRepairContext<'_>,
) -> (DoctorRepairVerification, String) {
    match (action, target) {
        (DoctorRepairAction::CleanMountRoot, _) => {
            match plan_stale_mount_directories(context.config) {
                Ok(stale) if stale.is_empty() => {
                    (DoctorRepairVerification::Verified, String::new())
                }
                Ok(stale) => (
                    DoctorRepairVerification::FindingRemains,
                    format!(" {} leftover folder(s) still present.", stale.len()),
                ),
                Err(error) => (
                    DoctorRepairVerification::CouldNotComplete,
                    format!(" The mount root could not be re-checked: {error}"),
                ),
            }
        }
        (DoctorRepairAction::CleanMountPath, RepairTarget::MountPath(path)) => {
            match plan_stale_mount_directories(context.config) {
                Ok(stale) if !stale.iter().any(|candidate| candidate == path) => {
                    (DoctorRepairVerification::Verified, String::new())
                }
                Ok(_) => (
                    DoctorRepairVerification::FindingRemains,
                    format!(" {} is still listed as a leftover folder.", path.display()),
                ),
                Err(error) => (
                    DoctorRepairVerification::CouldNotComplete,
                    format!(" The mount root could not be re-checked: {error}"),
                ),
            }
        }
        (DoctorRepairAction::RetryMount, RepairTarget::Archive(path)) => {
            match crate::current_archive_records(context.config) {
                Ok(records) => match records
                    .iter()
                    .find(|record| &record.mount_plan.archive.path == path)
                {
                    Some(record) if !retryable_health(record.health) => {
                        (DoctorRepairVerification::Verified, String::new())
                    }
                    Some(record) => (
                        DoctorRepairVerification::FindingRemains,
                        format!(" The archive still reports {}.", record.health),
                    ),
                    None => (
                        DoctorRepairVerification::CouldNotComplete,
                        " The archive is no longer in the library.".to_string(),
                    ),
                },
                Err(error) => (
                    DoctorRepairVerification::CouldNotComplete,
                    format!(" The library could not be re-checked: {error}"),
                ),
            }
        }
        (DoctorRepairAction::RebuildIndex, _) => {
            match crate::read_archive_index(context.index_path) {
                Ok(index) => {
                    let freshness = crate::check_archive_index_freshness(&index);
                    if freshness.has_warnings() {
                        (
                            DoctorRepairVerification::FindingRemains,
                            format!(
                                " {} archive(s) missing and {} stale after the rebuild.",
                                freshness.missing_archive_paths.len(),
                                freshness.stale_archive_paths.len()
                            ),
                        )
                    } else {
                        (DoctorRepairVerification::Verified, String::new())
                    }
                }
                Err(error) => (
                    DoctorRepairVerification::CouldNotComplete,
                    format!(" The rebuilt index could not be read back: {error}"),
                ),
            }
        }
        // A target/action pair the dispatcher cannot produce.
        _ => (
            DoctorRepairVerification::CouldNotComplete,
            " The repair could not be matched to a check.".to_string(),
        ),
    }
}

fn reject(
    request: &DoctorRepairRequest,
    spec: DoctorRepairSpec,
    affected: Option<EncodedPath>,
    rejection: DoctorRepairRejection,
) -> DoctorRepairOutcome {
    DoctorRepairOutcome {
        action: request.action,
        spec,
        record: DoctorRepairRecord {
            action_id: spec.id,
            action_title: spec.title,
            finding_id: request.finding_id.clone(),
            affected,
            confirmed: request.confirmed,
            dry_run: request.dry_run,
            status: DoctorRepairStatus::Rejected,
            verification: DoctorRepairVerification::NotAttempted,
            changed_paths: Vec::new(),
            undo: spec.undo,
            summary: format!("{} was refused: {}", spec.title, rejection.explanation()),
            rejection: Some(rejection),
            error: None,
        },
    }
}

// --- Findings that carry these repairs -----------------------------------

/// How many leftover folders are reported individually before Doctor stops
/// listing them one by one.
///
/// A real installation can legitimately accumulate thousands of empty mount
/// folders (measured: 4,041 on one live library), and one finding each would
/// bury every other result. This is the same "never flood the dashboard with
/// one entry per item" rule `source_health_issues` already documents, and the
/// same bounded-sample approach `DoctorReport::unknown_platform_examples`
/// already uses.
pub const MAX_INDIVIDUAL_STALE_MOUNT_FINDINGS: usize = 10;

/// How many example paths the summary finding lists as evidence.
const STALE_MOUNT_EVIDENCE_SAMPLE: usize = 10;

/// Leftover mount folders.
///
/// Always produces one summary finding offering
/// [`DoctorRepairAction::CleanMountRoot`]. Additionally produces one finding
/// per folder offering [`DoctorRepairAction::CleanMountPath`], but only while
/// there are at most [`MAX_INDIVIDUAL_STALE_MOUNT_FINDINGS`] of them - beyond
/// that, listing each one would flood the dashboard and the summary is the
/// useful unit.
///
/// The input comes from `plan_stale_mount_directories`, which is read-only
/// and shares its removability predicate with the remover.
pub fn findings_from_stale_mount_directories(stale: &[PathBuf]) -> Vec<Finding> {
    if stale.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    if stale.len() <= MAX_INDIVIDUAL_STALE_MOUNT_FINDINGS {
        findings.extend(stale.iter().map(|path| {
            Finding::new(
                "mount_root.stale_mount_directory",
                super::DoctorCategory::MountRoot,
                super::DoctorSubsystem::MountRootCleanup,
                DoctorSeverity::Info,
                "Leftover empty mount folder",
                "This folder is empty, nothing is mounted on it, and ArchiveFS no longer needs it.",
            )
            .with_affected_path(path)
            .offering(DoctorRepairAction::CleanMountPath)
        }));
    }
    let mut summary = Finding::new(
        "mount_root.stale_mount_directories",
        super::DoctorCategory::MountRoot,
        super::DoctorSubsystem::MountRootCleanup,
        DoctorSeverity::Info,
        if stale.len() == 1 {
            "One leftover empty mount folder"
        } else {
            "Leftover empty mount folders"
        },
        format!(
            "{} empty, unmounted folder(s) are left over beneath the mount root. They are harmless, but ArchiveFS can tidy them up.",
            stale.len()
        ),
    )
    .offering(DoctorRepairAction::CleanMountRoot);
    summary.evidence = stale
        .iter()
        .take(STALE_MOUNT_EVIDENCE_SAMPLE)
        .map(|path| path.display().to_string())
        .collect();
    if stale.len() > STALE_MOUNT_EVIDENCE_SAMPLE {
        summary.evidence.push(format!(
            "... and {} more",
            stale.len() - STALE_MOUNT_EVIDENCE_SAMPLE
        ));
    }
    if stale.len() > MAX_INDIVIDUAL_STALE_MOUNT_FINDINGS {
        summary.evidence.push(format!(
            "Individual folders are not listed separately above {MAX_INDIVIDUAL_STALE_MOUNT_FINDINGS}, so this one result covers them all."
        ));
    }
    findings.push(summary);
    findings
}

/// Index freshness, from the existing `check_archive_index_freshness`.
pub fn findings_from_index_freshness(
    freshness: &crate::ArchiveIndexFreshness,
    index_path: &Path,
) -> Vec<Finding> {
    if !freshness.has_warnings() {
        return Vec::new();
    }
    let mut evidence: Vec<String> = freshness
        .missing_archive_paths
        .iter()
        .take(10)
        .map(|path| format!("No longer on disk: {}", path.display()))
        .collect();
    evidence.extend(
        freshness
            .stale_archive_paths
            .iter()
            .take(10)
            .map(|path| format!("Changed since indexing: {}", path.display())),
    );
    let mut finding = Finding::new(
        "library.index_out_of_date",
        super::DoctorCategory::Library,
        super::DoctorSubsystem::ArchiveIndex,
        DoctorSeverity::Warning,
        "The archive index is out of date",
        format!(
            "{} indexed archive(s) are no longer on disk and {} have changed since they were indexed. Search results may be wrong.",
            freshness.missing_archive_paths.len(),
            freshness.stale_archive_paths.len()
        ),
    )
    .offering(DoctorRepairAction::RebuildIndex);
    finding.affected = Some(EncodedPath::from_path(index_path));
    finding.evidence = evidence;
    vec![finding]
}

/// A `HashSet` of every action currently offered by a scan - used by the CLI
/// to list what can be repaired without duplicating the mapping.
pub fn offered_repairs(scan: &DoctorScan) -> HashSet<DoctorRepairAction> {
    scan.findings
        .iter()
        .filter_map(|finding| finding.repair)
        .collect()
}

#[cfg(test)]
mod tests;
