//! Shared, local, bounded and strictly read-only Cheats & Mods previewing.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::destination_safety::{
    DestinationRootState, DestinationSafetyError, DestinationSafetyFailureReason, DestinationState,
    assess_destination, validate_destination_root,
};

pub const PREVIEW_MAX_ENTRIES: usize = 512;
pub const PREVIEW_MAX_SOURCE_FILES_HASHED: usize = 256;
pub const PREVIEW_MAX_DESTINATION_FILES_HASHED: usize = 256;
pub const PREVIEW_MAX_BYTES_PER_FILE: u64 = 1024 * 1024;
pub const PREVIEW_MAX_TOTAL_BYTES_HASHED: u64 = 32 * 1024 * 1024;
pub const PREVIEW_MAX_DESTINATION_PATHS: usize = 1024;
pub const PREVIEW_MAX_CONFLICTS: usize = 128;
pub const PREVIEW_MAX_WARNINGS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewAdapter {
    RetroArch,
    Pcsx2,
    Dolphin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIdentityKind {
    RetroArchCatalogueMatch,
    Pcsx2ExecutableCrc,
    DolphinGameId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIdentityState {
    Verified,
    Candidate,
    Missing,
    Stale,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewIdentity {
    pub kind: PreviewIdentityKind,
    pub state: PreviewIdentityState,
    pub value: Option<String>,
    pub archive_path: PathBuf,
    pub revision: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMatchStrength {
    VerifiedExact,
    Strong,
    Candidate,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSourceItem {
    pub adapter: PreviewAdapter,
    pub source_path: PathBuf,
    pub expected_source_digest: Option<String>,
    /// Each path must contain exactly two normal components beneath the root.
    pub destination_relative_paths: Vec<PathBuf>,
    pub match_strength: PreviewMatchStrength,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedPreviewRequest {
    pub adapter: PreviewAdapter,
    pub selected_archive: PathBuf,
    pub platform: Option<String>,
    pub identity: PreviewIdentity,
    pub destination_root: PathBuf,
    pub source_items: Vec<PreviewSourceItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewState {
    InstallNew,
    AlreadyInstalled,
    ReplaceDifferent,
    Conflict,
    Ambiguous,
    NotEligible,
    Unsupported,
    UnsafeDestination,
    DestinationUnavailable,
    SourceUnavailable,
    IdentityUnavailable,
    ResourceLimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewDestinationState {
    Missing,
    RegularFileIdentical,
    RegularFileDifferent,
    Directory,
    Symlink,
    SpecialFile,
    Inaccessible,
    ChangedDuringInspection,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewProposedAction {
    Install,
    Skip,
    Replace,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewEligibility {
    Eligible,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewBlockerKind {
    AdapterMismatch,
    PlatformMismatch,
    IdentityMissing,
    IdentityCandidateOnly,
    IdentityStale,
    IdentityAmbiguous,
    MatchCandidateOnly,
    MatchAmbiguous,
    MatchUnsupported,
    SourceMissing,
    SourceChanged,
    SourceSymlink,
    SourceOversized,
    SourceSpecialFile,
    SourceInaccessible,
    DestinationUnsafe,
    DestinationUnavailable,
    DestinationChanged,
    ExistingDifferentContent,
    ReplacementPermissionRequired,
    MultipleDestinations,
    MultipleExactMatches,
    DuplicateDestination,
    DuplicateSourceContent,
    CaseCollision,
    FilenameCollision,
    ResourceLimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewBlocker {
    pub kind: PreviewBlockerKind,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewConflictKind {
    MultipleSourcesOneDestination,
    OneSourceMultipleDestinations,
    MultipleExactIdentityMatches,
    ExistingDifferentContent,
    CaseCollision,
    FilenameCollision,
    AdapterMismatch,
    PlatformMismatch,
    StaleVerifiedIdentity,
    SourceChanged,
    DestinationChanged,
    DuplicateSourceContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewConflict {
    pub kind: PreviewConflictKind,
    pub source_path: Option<PathBuf>,
    pub destination_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewWarningKind {
    DestinationParentsMissing,
    BackupWouldBeRequired,
    ReplacementPermissionWouldBeRequired,
    WarningLimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewWarning {
    pub kind: PreviewWarningKind,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedPreviewEntry {
    pub adapter: PreviewAdapter,
    pub selected_archive: PathBuf,
    pub verified_identity: Option<String>,
    pub match_strength: PreviewMatchStrength,
    pub source_path: Option<PathBuf>,
    pub source_digest: Option<String>,
    pub destination_root: PathBuf,
    pub destination_relative_path: Option<PathBuf>,
    pub destination_path: Option<PathBuf>,
    pub destination_state: PreviewDestinationState,
    pub existing_destination_digest: Option<String>,
    pub state: PreviewState,
    pub proposed_action: PreviewProposedAction,
    pub eligibility: PreviewEligibility,
    pub blockers: Vec<PreviewBlocker>,
    pub warnings: Vec<PreviewWarning>,
    pub backup_required: bool,
    pub explicit_replacement_permission_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PreviewSummary {
    pub entries: usize,
    pub install_new: usize,
    pub already_installed: usize,
    pub replace_different: usize,
    pub conflicts: usize,
    pub blocked: usize,
    pub source_files_hashed: usize,
    pub destination_files_hashed: usize,
    pub bytes_hashed: u64,
    pub destination_paths_inspected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedPreviewReport {
    pub request_archive: PathBuf,
    pub adapter: PreviewAdapter,
    pub entries: Vec<SharedPreviewEntry>,
    pub conflicts: Vec<PreviewConflict>,
    pub warnings: Vec<PreviewWarning>,
    pub summary: PreviewSummary,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedPreviewError {
    InvalidRequest(PreviewBlockerKind),
    DestinationSafety(DestinationSafetyError),
}

impl fmt::Display for SharedPreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(kind) => write!(formatter, "invalid preview request: {kind:?}"),
            Self::DestinationSafety(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SharedPreviewError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HashOutcome {
    Hashed { digest: String, bytes: u64 },
    Missing,
    Symlink,
    SpecialFile,
    Inaccessible,
    Changed,
    Oversized,
}

trait PreviewHasher {
    fn hash(&self, path: &Path) -> HashOutcome;
}

struct LocalPreviewHasher;

impl PreviewHasher for LocalPreviewHasher {
    fn hash(&self, path: &Path) -> HashOutcome {
        hash_local_regular_file(path)
    }
}

#[derive(Default)]
struct Budget {
    source_files: usize,
    destination_files: usize,
    bytes: u64,
    destination_paths: usize,
    exhausted: bool,
    source_cache: BTreeMap<PathBuf, HashOutcome>,
    destination_cache: BTreeMap<PathBuf, HashOutcome>,
}

pub fn build_shared_preview(
    request: &SharedPreviewRequest,
) -> Result<SharedPreviewReport, SharedPreviewError> {
    build_with_hasher(request, &LocalPreviewHasher)
}

fn build_with_hasher(
    request: &SharedPreviewRequest,
    hasher: &dyn PreviewHasher,
) -> Result<SharedPreviewReport, SharedPreviewError> {
    if request.selected_archive.as_os_str().is_empty() {
        return Err(SharedPreviewError::InvalidRequest(
            PreviewBlockerKind::IdentityMissing,
        ));
    }
    if !request.destination_root.is_absolute() || is_filesystem_root(&request.destination_root) {
        return Err(SharedPreviewError::InvalidRequest(
            PreviewBlockerKind::DestinationUnsafe,
        ));
    }
    validate_destination_root(&request.destination_root)
        .map_err(SharedPreviewError::DestinationSafety)?;

    let mut report = SharedPreviewReport {
        request_archive: request.selected_archive.clone(),
        adapter: request.adapter,
        entries: Vec::new(),
        conflicts: Vec::new(),
        warnings: Vec::new(),
        summary: PreviewSummary::default(),
        complete: true,
    };
    let mut budget = Budget::default();
    let mut sources = request.source_items.clone();
    sources.sort_by(|left, right| left.source_path.cmp(&right.source_path));

    if sources.is_empty() {
        report.entries.push(unavailable_entry(
            request,
            PreviewState::SourceUnavailable,
            PreviewBlockerKind::SourceMissing,
        ));
        finish_report(&mut report, &budget);
        return Ok(report);
    }

    for source in &sources {
        let mut destinations = source.destination_relative_paths.clone();
        destinations.sort();
        destinations.dedup();
        if destinations.is_empty() {
            report.entries.push(entry_with_blocker(
                request,
                source,
                None,
                PreviewState::DestinationUnavailable,
                PreviewBlockerKind::DestinationUnavailable,
                None,
            ));
            continue;
        }
        for relative in destinations {
            if report.entries.len() >= PREVIEW_MAX_ENTRIES
                || budget.destination_paths >= PREVIEW_MAX_DESTINATION_PATHS
            {
                budget.exhausted = true;
                break;
            }
            budget.destination_paths += 1;
            report
                .entries
                .push(preview_one(request, source, relative, hasher, &mut budget));
        }
        if budget.exhausted {
            break;
        }
    }

    detect_cross_entry_conflicts(request, &mut report);
    if budget.exhausted {
        report.complete = false;
        push_report_warning(
            &mut report,
            PreviewWarning {
                kind: PreviewWarningKind::WarningLimitReached,
                path: None,
            },
        );
        if report.entries.len() < PREVIEW_MAX_ENTRIES {
            report.entries.push(unavailable_entry(
                request,
                PreviewState::ResourceLimitReached,
                PreviewBlockerKind::ResourceLimitReached,
            ));
        }
    }
    finish_report(&mut report, &budget);
    Ok(report)
}

fn preview_one(
    request: &SharedPreviewRequest,
    source: &PreviewSourceItem,
    relative: PathBuf,
    hasher: &dyn PreviewHasher,
    budget: &mut Budget,
) -> SharedPreviewEntry {
    let mut entry = base_entry(request, source, Some(relative.clone()));
    apply_eligibility_blockers(request, source, &mut entry);
    let Some((directory, filename)) = two_safe_components(&relative) else {
        block(
            &mut entry,
            PreviewState::UnsafeDestination,
            PreviewBlockerKind::DestinationUnsafe,
            Some(request.destination_root.join(&relative)),
        );
        return entry;
    };

    let source_cached = budget.source_cache.contains_key(&source.source_path);
    let source_outcome = if let Some(outcome) = budget.source_cache.get(&source.source_path) {
        outcome.clone()
    } else if budget.source_files >= PREVIEW_MAX_SOURCE_FILES_HASHED {
        budget.exhausted = true;
        HashOutcome::Oversized
    } else {
        budget.source_files += 1;
        let outcome = hasher.hash(&source.source_path);
        budget
            .source_cache
            .insert(source.source_path.clone(), outcome.clone());
        outcome
    };
    let source_digest = match source_outcome {
        HashOutcome::Hashed { digest, bytes } => {
            if !source_cached && !consume_bytes(budget, bytes) {
                block(
                    &mut entry,
                    PreviewState::ResourceLimitReached,
                    PreviewBlockerKind::ResourceLimitReached,
                    Some(source.source_path.clone()),
                );
                return entry;
            }
            if source.expected_source_digest.as_deref() != Some(digest.as_str())
                && source.expected_source_digest.is_some()
            {
                block(
                    &mut entry,
                    PreviewState::Conflict,
                    PreviewBlockerKind::SourceChanged,
                    Some(source.source_path.clone()),
                );
            }
            entry.source_digest = Some(digest.clone());
            digest
        }
        outcome => {
            let (state, blocker) = source_failure(outcome);
            block(&mut entry, state, blocker, Some(source.source_path.clone()));
            return entry;
        }
    };

    let assessment = match assess_destination(
        &request.destination_root,
        directory.as_os_str(),
        filename.as_os_str(),
    ) {
        Ok(assessment) => assessment,
        Err(error) => {
            entry.destination_path = Some(error.path.clone());
            entry.destination_state = destination_error_state(&error);
            let state = match error.reason {
                DestinationSafetyFailureReason::InspectionFailed => {
                    PreviewState::DestinationUnavailable
                }
                _ => PreviewState::UnsafeDestination,
            };
            block(
                &mut entry,
                state,
                if state == PreviewState::DestinationUnavailable {
                    PreviewBlockerKind::DestinationUnavailable
                } else {
                    PreviewBlockerKind::DestinationUnsafe
                },
                Some(error.path),
            );
            return entry;
        }
    };
    entry.destination_path = Some(assessment.proposed_destination.path().to_path_buf());
    if assessment.validated_root.state() == DestinationRootState::Absent
        || assessment
            .inspected_parents
            .iter()
            .any(|parent| matches!(parent.state, super::InspectedParentState::Missing))
    {
        let destination_path = entry.destination_path.clone();
        push_entry_warning(
            &mut entry,
            PreviewWarning {
                kind: PreviewWarningKind::DestinationParentsMissing,
                path: destination_path,
            },
        );
    }
    if assessment.destination_state == DestinationState::Absent {
        entry.destination_state = PreviewDestinationState::Missing;
        if entry.blockers.is_empty() {
            entry.state = PreviewState::InstallNew;
            entry.proposed_action = PreviewProposedAction::Install;
            entry.eligibility = PreviewEligibility::Eligible;
        }
        return entry;
    }

    let destination_path = entry.destination_path.clone().unwrap_or_default();
    if !budget.destination_cache.contains_key(&destination_path)
        && budget.destination_files >= PREVIEW_MAX_DESTINATION_FILES_HASHED
    {
        budget.exhausted = true;
        let destination_path = entry.destination_path.clone();
        block(
            &mut entry,
            PreviewState::ResourceLimitReached,
            PreviewBlockerKind::ResourceLimitReached,
            destination_path,
        );
        return entry;
    }
    let destination_cached = budget.destination_cache.contains_key(&destination_path);
    let destination_outcome = if let Some(outcome) = budget.destination_cache.get(&destination_path)
    {
        outcome.clone()
    } else {
        budget.destination_files += 1;
        let outcome = hasher.hash(&destination_path);
        budget
            .destination_cache
            .insert(destination_path.clone(), outcome.clone());
        outcome
    };
    match destination_outcome {
        HashOutcome::Hashed { digest, bytes } => {
            if !destination_cached && !consume_bytes(budget, bytes) {
                block(
                    &mut entry,
                    PreviewState::ResourceLimitReached,
                    PreviewBlockerKind::ResourceLimitReached,
                    Some(destination_path),
                );
                return entry;
            }
            entry.existing_destination_digest = Some(digest.clone());
            if digest == source_digest {
                entry.destination_state = PreviewDestinationState::RegularFileIdentical;
                if entry.blockers.is_empty() {
                    entry.state = PreviewState::AlreadyInstalled;
                    entry.proposed_action = PreviewProposedAction::Skip;
                    entry.eligibility = PreviewEligibility::Eligible;
                }
            } else {
                entry.destination_state = PreviewDestinationState::RegularFileDifferent;
                entry.backup_required = true;
                entry.explicit_replacement_permission_required = true;
                push_entry_warning(
                    &mut entry,
                    PreviewWarning {
                        kind: PreviewWarningKind::BackupWouldBeRequired,
                        path: Some(destination_path.clone()),
                    },
                );
                push_entry_warning(
                    &mut entry,
                    PreviewWarning {
                        kind: PreviewWarningKind::ReplacementPermissionWouldBeRequired,
                        path: Some(destination_path.clone()),
                    },
                );
                if entry.blockers.is_empty() {
                    entry.state = PreviewState::ReplaceDifferent;
                    entry.proposed_action = PreviewProposedAction::Replace;
                    entry.eligibility = PreviewEligibility::Eligible;
                }
            }
        }
        HashOutcome::Changed => {
            entry.destination_state = PreviewDestinationState::ChangedDuringInspection;
            block(
                &mut entry,
                PreviewState::Conflict,
                PreviewBlockerKind::DestinationChanged,
                Some(destination_path),
            );
        }
        HashOutcome::Symlink => {
            entry.destination_state = PreviewDestinationState::Symlink;
            block(
                &mut entry,
                PreviewState::UnsafeDestination,
                PreviewBlockerKind::DestinationUnsafe,
                Some(destination_path),
            );
        }
        HashOutcome::SpecialFile => {
            entry.destination_state = PreviewDestinationState::SpecialFile;
            block(
                &mut entry,
                PreviewState::UnsafeDestination,
                PreviewBlockerKind::DestinationUnsafe,
                Some(destination_path),
            );
        }
        HashOutcome::Missing | HashOutcome::Inaccessible => {
            entry.destination_state = PreviewDestinationState::Inaccessible;
            block(
                &mut entry,
                PreviewState::DestinationUnavailable,
                PreviewBlockerKind::DestinationUnavailable,
                Some(destination_path),
            );
        }
        HashOutcome::Oversized => {
            entry.destination_state = PreviewDestinationState::Unavailable;
            block(
                &mut entry,
                PreviewState::ResourceLimitReached,
                PreviewBlockerKind::ResourceLimitReached,
                Some(destination_path),
            );
        }
    }
    entry
}

fn apply_eligibility_blockers(
    request: &SharedPreviewRequest,
    source: &PreviewSourceItem,
    entry: &mut SharedPreviewEntry,
) {
    if source.adapter != request.adapter {
        block(
            entry,
            PreviewState::Conflict,
            PreviewBlockerKind::AdapterMismatch,
            None,
        );
    }
    if !platform_matches(request.adapter, request.platform.as_deref()) {
        block(
            entry,
            PreviewState::Conflict,
            PreviewBlockerKind::PlatformMismatch,
            None,
        );
    }
    if request.identity.archive_path != request.selected_archive {
        block(
            entry,
            PreviewState::Conflict,
            PreviewBlockerKind::IdentityStale,
            Some(request.identity.archive_path.clone()),
        );
    }
    match request.identity.state {
        PreviewIdentityState::Verified => {}
        PreviewIdentityState::Candidate => block(
            entry,
            PreviewState::IdentityUnavailable,
            PreviewBlockerKind::IdentityCandidateOnly,
            None,
        ),
        PreviewIdentityState::Missing => block(
            entry,
            PreviewState::IdentityUnavailable,
            PreviewBlockerKind::IdentityMissing,
            None,
        ),
        PreviewIdentityState::Stale => block(
            entry,
            PreviewState::Conflict,
            PreviewBlockerKind::IdentityStale,
            None,
        ),
        PreviewIdentityState::Ambiguous => block(
            entry,
            PreviewState::Ambiguous,
            PreviewBlockerKind::IdentityAmbiguous,
            None,
        ),
    }
    let strength_eligible = match request.adapter {
        PreviewAdapter::RetroArch => matches!(
            source.match_strength,
            PreviewMatchStrength::VerifiedExact | PreviewMatchStrength::Strong
        ),
        PreviewAdapter::Pcsx2 | PreviewAdapter::Dolphin => {
            source.match_strength == PreviewMatchStrength::VerifiedExact
        }
    };
    if !strength_eligible {
        let blocker = match source.match_strength {
            PreviewMatchStrength::Candidate => PreviewBlockerKind::MatchCandidateOnly,
            PreviewMatchStrength::Ambiguous => PreviewBlockerKind::MatchAmbiguous,
            PreviewMatchStrength::Unsupported => PreviewBlockerKind::MatchUnsupported,
            PreviewMatchStrength::Strong | PreviewMatchStrength::VerifiedExact => {
                PreviewBlockerKind::MatchCandidateOnly
            }
        };
        let state = match source.match_strength {
            PreviewMatchStrength::Ambiguous => PreviewState::Ambiguous,
            PreviewMatchStrength::Unsupported => PreviewState::Unsupported,
            _ => PreviewState::NotEligible,
        };
        block(entry, state, blocker, None);
    }
}

fn detect_cross_entry_conflicts(request: &SharedPreviewRequest, report: &mut SharedPreviewReport) {
    let mut destinations: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    let mut folded: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut source_destinations: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    let mut digests: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, entry) in report.entries.iter().enumerate() {
        if let Some(destination) = &entry.destination_path {
            destinations
                .entry(destination.clone())
                .or_default()
                .push(index);
            folded
                .entry(destination.to_string_lossy().to_lowercase())
                .or_default()
                .push(index);
            if let Some(source) = &entry.source_path {
                source_destinations
                    .entry(source.clone())
                    .or_default()
                    .insert(destination.clone());
            }
        }
        if let Some(digest) = &entry.source_digest {
            digests.entry(digest.clone()).or_default().push(index);
        }
    }
    for indices in destinations.values().filter(|indices| indices.len() > 1) {
        for &index in indices {
            mark_conflict(
                report,
                index,
                PreviewBlockerKind::DuplicateDestination,
                PreviewConflictKind::MultipleSourcesOneDestination,
            );
            let destination_path = report.entries[index].destination_path.clone();
            add_blocker_once(
                &mut report.entries[index],
                PreviewBlockerKind::FilenameCollision,
                destination_path,
            );
        }
    }
    let case_collisions: Vec<Vec<usize>> = folded
        .values()
        .filter(|indices| {
            indices.len() > 1
                && indices
                    .iter()
                    .filter_map(|index| report.entries[*index].destination_path.as_ref())
                    .collect::<BTreeSet<_>>()
                    .len()
                    > 1
        })
        .cloned()
        .collect();
    for indices in case_collisions {
        for index in indices {
            mark_conflict(
                report,
                index,
                PreviewBlockerKind::CaseCollision,
                PreviewConflictKind::CaseCollision,
            );
        }
    }
    for (source, destinations) in source_destinations {
        if destinations.len() > 1 {
            for index in 0..report.entries.len() {
                if report.entries[index].source_path.as_ref() == Some(&source) {
                    mark_conflict(
                        report,
                        index,
                        PreviewBlockerKind::MultipleDestinations,
                        PreviewConflictKind::OneSourceMultipleDestinations,
                    );
                }
            }
        }
    }
    let duplicate_digests: Vec<Vec<usize>> = digests
        .values()
        .filter(|indices| {
            indices
                .iter()
                .filter_map(|index| report.entries[*index].source_path.as_ref())
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .cloned()
        .collect();
    for indices in duplicate_digests {
        for index in indices {
            mark_conflict(
                report,
                index,
                PreviewBlockerKind::DuplicateSourceContent,
                PreviewConflictKind::DuplicateSourceContent,
            );
        }
    }
    let exact_sources = report
        .entries
        .iter()
        .filter(|entry| entry.match_strength == PreviewMatchStrength::VerifiedExact)
        .filter_map(|entry| entry.source_path.as_ref())
        .collect::<BTreeSet<_>>();
    if matches!(
        request.adapter,
        PreviewAdapter::Pcsx2 | PreviewAdapter::Dolphin
    ) && exact_sources.len() > 1
    {
        for index in 0..report.entries.len() {
            mark_conflict(
                report,
                index,
                PreviewBlockerKind::MultipleExactMatches,
                PreviewConflictKind::MultipleExactIdentityMatches,
            );
        }
    }
}

fn mark_conflict(
    report: &mut SharedPreviewReport,
    index: usize,
    blocker: PreviewBlockerKind,
    kind: PreviewConflictKind,
) {
    let source_path = report.entries[index].source_path.clone();
    let destination_path = report.entries[index].destination_path.clone();
    block(
        &mut report.entries[index],
        PreviewState::Conflict,
        blocker,
        destination_path.clone(),
    );
    if report.conflicts.len() < PREVIEW_MAX_CONFLICTS
        && !report.conflicts.iter().any(|conflict| {
            conflict.kind == kind
                && conflict.source_path == source_path
                && conflict.destination_path == destination_path
        })
    {
        report.conflicts.push(PreviewConflict {
            kind,
            source_path,
            destination_path,
        });
    } else if report.conflicts.len() >= PREVIEW_MAX_CONFLICTS {
        report.complete = false;
    }
}

fn base_entry(
    request: &SharedPreviewRequest,
    source: &PreviewSourceItem,
    relative: Option<PathBuf>,
) -> SharedPreviewEntry {
    SharedPreviewEntry {
        adapter: request.adapter,
        selected_archive: request.selected_archive.clone(),
        verified_identity: (request.identity.state == PreviewIdentityState::Verified)
            .then(|| request.identity.value.clone())
            .flatten(),
        match_strength: source.match_strength,
        source_path: Some(source.source_path.clone()),
        source_digest: None,
        destination_root: request.destination_root.clone(),
        destination_relative_path: relative,
        destination_path: None,
        destination_state: PreviewDestinationState::Unavailable,
        existing_destination_digest: None,
        state: PreviewState::NotEligible,
        proposed_action: PreviewProposedAction::Blocked,
        eligibility: PreviewEligibility::Blocked,
        blockers: Vec::new(),
        warnings: Vec::new(),
        backup_required: false,
        explicit_replacement_permission_required: false,
    }
}

fn entry_with_blocker(
    request: &SharedPreviewRequest,
    source: &PreviewSourceItem,
    relative: Option<PathBuf>,
    state: PreviewState,
    blocker: PreviewBlockerKind,
    path: Option<PathBuf>,
) -> SharedPreviewEntry {
    let mut entry = base_entry(request, source, relative);
    block(&mut entry, state, blocker, path);
    entry
}

fn unavailable_entry(
    request: &SharedPreviewRequest,
    state: PreviewState,
    blocker: PreviewBlockerKind,
) -> SharedPreviewEntry {
    SharedPreviewEntry {
        adapter: request.adapter,
        selected_archive: request.selected_archive.clone(),
        verified_identity: (request.identity.state == PreviewIdentityState::Verified)
            .then(|| request.identity.value.clone())
            .flatten(),
        match_strength: PreviewMatchStrength::Unsupported,
        source_path: None,
        source_digest: None,
        destination_root: request.destination_root.clone(),
        destination_relative_path: None,
        destination_path: None,
        destination_state: PreviewDestinationState::Unavailable,
        existing_destination_digest: None,
        state,
        proposed_action: PreviewProposedAction::Blocked,
        eligibility: PreviewEligibility::Blocked,
        blockers: vec![PreviewBlocker {
            kind: blocker,
            path: None,
        }],
        warnings: Vec::new(),
        backup_required: false,
        explicit_replacement_permission_required: false,
    }
}

fn block(
    entry: &mut SharedPreviewEntry,
    state: PreviewState,
    kind: PreviewBlockerKind,
    path: Option<PathBuf>,
) {
    entry.state = state;
    entry.proposed_action = PreviewProposedAction::Blocked;
    entry.eligibility = PreviewEligibility::Blocked;
    add_blocker_once(entry, kind, path);
}

fn add_blocker_once(
    entry: &mut SharedPreviewEntry,
    kind: PreviewBlockerKind,
    path: Option<PathBuf>,
) {
    if !entry
        .blockers
        .iter()
        .any(|blocker| blocker.kind == kind && blocker.path == path)
    {
        entry.blockers.push(PreviewBlocker { kind, path });
        entry.blockers.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.path.cmp(&right.path))
        });
    }
}

fn push_entry_warning(entry: &mut SharedPreviewEntry, warning: PreviewWarning) {
    if entry.warnings.len() < PREVIEW_MAX_WARNINGS {
        entry.warnings.push(warning);
    }
}

fn push_report_warning(report: &mut SharedPreviewReport, warning: PreviewWarning) {
    if report.warnings.len() < PREVIEW_MAX_WARNINGS {
        report.warnings.push(warning);
    }
}

fn finish_report(report: &mut SharedPreviewReport, budget: &Budget) {
    report.entries.sort_by(|left, right| {
        left.destination_path
            .cmp(&right.destination_path)
            .then_with(|| left.source_path.cmp(&right.source_path))
    });
    report.conflicts.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.destination_path.cmp(&right.destination_path))
            .then_with(|| left.source_path.cmp(&right.source_path))
    });
    report.summary = PreviewSummary {
        entries: report.entries.len(),
        install_new: report
            .entries
            .iter()
            .filter(|entry| entry.state == PreviewState::InstallNew)
            .count(),
        already_installed: report
            .entries
            .iter()
            .filter(|entry| entry.state == PreviewState::AlreadyInstalled)
            .count(),
        replace_different: report
            .entries
            .iter()
            .filter(|entry| entry.state == PreviewState::ReplaceDifferent)
            .count(),
        conflicts: report
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.state,
                    PreviewState::Conflict
                        | PreviewState::Ambiguous
                        | PreviewState::UnsafeDestination
                )
            })
            .count(),
        blocked: report
            .entries
            .iter()
            .filter(|entry| entry.eligibility == PreviewEligibility::Blocked)
            .count(),
        source_files_hashed: budget.source_files,
        destination_files_hashed: budget.destination_files,
        bytes_hashed: budget.bytes,
        destination_paths_inspected: budget.destination_paths,
    };
}

fn source_failure(outcome: HashOutcome) -> (PreviewState, PreviewBlockerKind) {
    match outcome {
        HashOutcome::Missing => (
            PreviewState::SourceUnavailable,
            PreviewBlockerKind::SourceMissing,
        ),
        HashOutcome::Symlink => (
            PreviewState::SourceUnavailable,
            PreviewBlockerKind::SourceSymlink,
        ),
        HashOutcome::SpecialFile => (
            PreviewState::SourceUnavailable,
            PreviewBlockerKind::SourceSpecialFile,
        ),
        HashOutcome::Inaccessible => (
            PreviewState::SourceUnavailable,
            PreviewBlockerKind::SourceInaccessible,
        ),
        HashOutcome::Changed => (PreviewState::Conflict, PreviewBlockerKind::SourceChanged),
        HashOutcome::Oversized => (
            PreviewState::ResourceLimitReached,
            PreviewBlockerKind::SourceOversized,
        ),
        HashOutcome::Hashed { .. } => unreachable!(),
    }
}

fn destination_error_state(error: &DestinationSafetyError) -> PreviewDestinationState {
    match error.destination_state {
        Some(DestinationState::Directory) => PreviewDestinationState::Directory,
        Some(DestinationState::Symlink) => PreviewDestinationState::Symlink,
        Some(DestinationState::Unsafe) => PreviewDestinationState::SpecialFile,
        _ if error.reason == DestinationSafetyFailureReason::InspectionFailed => {
            PreviewDestinationState::Inaccessible
        }
        _ => PreviewDestinationState::Unavailable,
    }
}

fn two_safe_components(relative: &Path) -> Option<(OsString, OsString)> {
    if relative.is_absolute() {
        return None;
    }
    let components = relative.components().collect::<Vec<_>>();
    match components.as_slice() {
        [Component::Normal(directory), Component::Normal(filename)]
            if !directory.is_empty() && !filename.is_empty() =>
        {
            Some((directory.to_os_string(), filename.to_os_string()))
        }
        _ => None,
    }
}

fn is_filesystem_root(path: &Path) -> bool {
    path.is_absolute() && path.parent().is_none()
}

fn platform_matches(adapter: PreviewAdapter, platform: Option<&str>) -> bool {
    let normalized = platform.unwrap_or_default().trim().to_ascii_lowercase();
    match adapter {
        PreviewAdapter::Pcsx2 => matches!(
            normalized.as_str(),
            "ps2" | "playstation 2" | "playstation2" | "sony playstation 2"
        ),
        PreviewAdapter::Dolphin => matches!(
            normalized.as_str(),
            "gamecube" | "nintendo gamecube" | "gc" | "gcn" | "wii" | "nintendo wii"
        ),
        PreviewAdapter::RetroArch => !normalized.is_empty(),
    }
}

fn consume_bytes(budget: &mut Budget, bytes: u64) -> bool {
    if budget.bytes.saturating_add(bytes) > PREVIEW_MAX_TOTAL_BYTES_HASHED {
        budget.exhausted = true;
        false
    } else {
        budget.bytes += bytes;
        true
    }
}

fn hash_local_regular_file(path: &Path) -> HashOutcome {
    if !path.is_absolute() {
        return HashOutcome::Inaccessible;
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return HashOutcome::Symlink,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return HashOutcome::Missing,
            Err(_) => return HashOutcome::Inaccessible,
        }
    }
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => return HashOutcome::Symlink,
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return HashOutcome::SpecialFile,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return HashOutcome::Missing,
        Err(_) => return HashOutcome::Inaccessible,
    };
    if before.len() > PREVIEW_MAX_BYTES_PER_FILE {
        return HashOutcome::Oversized;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(_) => return HashOutcome::Inaccessible,
    };
    let opened = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return HashOutcome::Inaccessible,
    };
    if !same_file_identity(&before, &opened) {
        return HashOutcome::Changed;
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    if file
        .by_ref()
        .take(PREVIEW_MAX_BYTES_PER_FILE + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return HashOutcome::Inaccessible;
    }
    if bytes.len() as u64 > PREVIEW_MAX_BYTES_PER_FILE {
        return HashOutcome::Oversized;
    }
    let after = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return HashOutcome::Inaccessible,
    };
    if !same_file_identity(&opened, &after) || after.len() != bytes.len() as u64 {
        return HashOutcome::Changed;
    }
    let digest = Sha256::digest(&bytes);
    HashOutcome::Hashed {
        digest: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        bytes: bytes.len() as u64,
    }
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-shared-preview-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn request(fixture: &Fixture, source: &Path, destination: &str) -> SharedPreviewRequest {
        SharedPreviewRequest {
            adapter: PreviewAdapter::Pcsx2,
            selected_archive: fixture.0.join("game.iso"),
            platform: Some("PS2".into()),
            identity: PreviewIdentity {
                kind: PreviewIdentityKind::Pcsx2ExecutableCrc,
                state: PreviewIdentityState::Verified,
                value: Some("DEADBEEF".into()),
                archive_path: fixture.0.join("game.iso"),
                revision: None,
            },
            destination_root: fixture.0.join("PCSX2"),
            source_items: vec![PreviewSourceItem {
                adapter: PreviewAdapter::Pcsx2,
                source_path: source.to_path_buf(),
                expected_source_digest: Some(digest(b"source")),
                destination_relative_paths: vec![PathBuf::from(destination)],
                match_strength: PreviewMatchStrength::VerifiedExact,
            }],
        }
    }

    #[test]
    fn install_new_identical_and_replace_different_are_distinct() {
        let fixture = Fixture::new("states");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let destination_dir = fixture.0.join("PCSX2/cheats");
        fs::create_dir_all(&destination_dir).unwrap();
        let request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        let install = build_shared_preview(&request).unwrap();
        assert_eq!(install.entries[0].state, PreviewState::InstallNew);
        assert_eq!(
            install.entries[0].proposed_action,
            PreviewProposedAction::Install
        );

        fs::write(destination_dir.join("DEADBEEF.pnach"), b"source").unwrap();
        let identical = build_shared_preview(&request).unwrap();
        assert_eq!(identical.entries[0].state, PreviewState::AlreadyInstalled);

        fs::write(destination_dir.join("DEADBEEF.pnach"), b"different").unwrap();
        let replace = build_shared_preview(&request).unwrap();
        assert_eq!(replace.entries[0].state, PreviewState::ReplaceDifferent);
        assert!(replace.entries[0].backup_required);
        assert!(replace.entries[0].explicit_replacement_permission_required);
    }

    #[test]
    fn candidate_identity_and_candidate_match_are_blocked() {
        let fixture = Fixture::new("candidate");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let mut request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        request.identity.state = PreviewIdentityState::Candidate;
        request.source_items[0].match_strength = PreviewMatchStrength::Candidate;
        let report = build_shared_preview(&request).unwrap();
        assert_eq!(report.entries[0].eligibility, PreviewEligibility::Blocked);
        assert!(
            report.entries[0]
                .blockers
                .iter()
                .any(|blocker| blocker.kind == PreviewBlockerKind::IdentityCandidateOnly)
        );
        assert!(
            report.entries[0]
                .blockers
                .iter()
                .any(|blocker| blocker.kind == PreviewBlockerKind::MatchCandidateOnly)
        );
    }

    #[test]
    fn duplicate_destination_and_multiple_exact_matches_conflict() {
        let fixture = Fixture::new("duplicate");
        let one = fixture.0.join("one.pnach");
        let two = fixture.0.join("two.pnach");
        fs::write(&one, b"one").unwrap();
        fs::write(&two, b"two").unwrap();
        let mut request = request(&fixture, &one, "cheats/DEADBEEF.pnach");
        request.source_items[0].expected_source_digest = Some(digest(b"one"));
        request.source_items.push(PreviewSourceItem {
            adapter: PreviewAdapter::Pcsx2,
            source_path: two,
            expected_source_digest: Some(digest(b"two")),
            destination_relative_paths: vec![PathBuf::from("cheats/DEADBEEF.pnach")],
            match_strength: PreviewMatchStrength::VerifiedExact,
        });
        let report = build_shared_preview(&request).unwrap();
        assert!(
            report
                .entries
                .iter()
                .all(|entry| entry.state == PreviewState::Conflict)
        );
        assert!(report.conflicts.iter().any(|conflict| conflict.kind == PreviewConflictKind::MultipleSourcesOneDestination));
        assert!(
            report
                .conflicts
                .iter()
                .any(|conflict| conflict.kind == PreviewConflictKind::MultipleExactIdentityMatches)
        );
    }

    #[test]
    fn one_source_multiple_destinations_and_case_collision_are_not_resolved() {
        let fixture = Fixture::new("case-collision");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let mut request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        request.source_items[0]
            .destination_relative_paths
            .push(PathBuf::from("cheats/deadbeef.pnach"));
        let report = build_shared_preview(&request).unwrap();
        assert_eq!(report.entries.len(), 2);
        assert!(report.conflicts.iter().any(|conflict| {
            conflict.kind == PreviewConflictKind::OneSourceMultipleDestinations
        }));
        assert!(
            report
                .conflicts
                .iter()
                .any(|conflict| conflict.kind == PreviewConflictKind::CaseCollision)
        );
    }

    #[test]
    fn changed_source_symlink_and_traversal_are_refused() {
        let fixture = Fixture::new("unsafe");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"changed").unwrap();
        let traversal_request = request(&fixture, &source, "../escape.pnach");
        let report = build_shared_preview(&traversal_request).unwrap();
        assert!(
            report.entries[0]
                .blockers
                .iter()
                .any(|blocker| blocker.kind == PreviewBlockerKind::DestinationUnsafe)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = fixture.0.join("target.pnach");
            fs::write(&target, b"source").unwrap();
            let link = fixture.0.join("link.pnach");
            symlink(&target, &link).unwrap();
            let report =
                build_shared_preview(&request(&fixture, &link, "cheats/DEADBEEF.pnach")).unwrap();
            assert!(
                report.entries[0]
                    .blockers
                    .iter()
                    .any(|blocker| blocker.kind == PreviewBlockerKind::SourceSymlink)
            );
        }
    }

    #[test]
    fn filesystem_root_is_rejected_with_typed_error() {
        let fixture = Fixture::new("root");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let mut request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        request.destination_root = PathBuf::from("/");
        assert_eq!(
            build_shared_preview(&request),
            Err(SharedPreviewError::InvalidRequest(
                PreviewBlockerKind::DestinationUnsafe
            ))
        );
        request.destination_root = PathBuf::from("relative");
        assert_eq!(
            build_shared_preview(&request),
            Err(SharedPreviewError::InvalidRequest(
                PreviewBlockerKind::DestinationUnsafe
            ))
        );
    }

    struct ChangedHasher;
    impl PreviewHasher for ChangedHasher {
        fn hash(&self, _path: &Path) -> HashOutcome {
            HashOutcome::Changed
        }
    }

    #[test]
    fn changed_during_hash_is_blocked_deterministically() {
        let fixture = Fixture::new("changed");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let report = build_with_hasher(
            &request(&fixture, &source, "cheats/DEADBEEF.pnach"),
            &ChangedHasher,
        )
        .unwrap();
        assert_eq!(report.entries[0].state, PreviewState::Conflict);
        assert!(
            report.entries[0]
                .blockers
                .iter()
                .any(|blocker| blocker.kind == PreviewBlockerKind::SourceChanged)
        );
    }

    struct DestinationOutcomeHasher(HashOutcome);
    impl PreviewHasher for DestinationOutcomeHasher {
        fn hash(&self, path: &Path) -> HashOutcome {
            if path.file_name().is_some_and(|name| name == "source.pnach") {
                HashOutcome::Hashed {
                    digest: digest(b"source"),
                    bytes: 6,
                }
            } else {
                self.0.clone()
            }
        }
    }

    #[test]
    fn destination_changed_and_inaccessible_are_distinct() {
        let fixture = Fixture::new("destination-outcomes");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        fs::create_dir_all(fixture.0.join("PCSX2/cheats")).unwrap();
        fs::write(fixture.0.join("PCSX2/cheats/DEADBEEF.pnach"), b"old").unwrap();
        let request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        let changed =
            build_with_hasher(&request, &DestinationOutcomeHasher(HashOutcome::Changed)).unwrap();
        assert_eq!(
            changed.entries[0].destination_state,
            PreviewDestinationState::ChangedDuringInspection
        );
        let inaccessible = build_with_hasher(
            &request,
            &DestinationOutcomeHasher(HashOutcome::Inaccessible),
        )
        .unwrap();
        assert_eq!(
            inaccessible.entries[0].state,
            PreviewState::DestinationUnavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_special_file_destinations_are_refused() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let fixture = Fixture::new("destination-types");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, b"source").unwrap();
        let destination_dir = fixture.0.join("PCSX2/cheats");
        fs::create_dir_all(&destination_dir).unwrap();
        let destination = destination_dir.join("DEADBEEF.pnach");
        symlink(&source, &destination).unwrap();
        let request = request(&fixture, &source, "cheats/DEADBEEF.pnach");
        let symlink_report = build_shared_preview(&request).unwrap();
        assert_eq!(
            symlink_report.entries[0].destination_state,
            PreviewDestinationState::Symlink
        );
        fs::remove_file(&destination).unwrap();
        let _listener = UnixListener::bind(&destination).unwrap();
        let special_report = build_shared_preview(&request).unwrap();
        assert_eq!(
            special_report.entries[0].destination_state,
            PreviewDestinationState::SpecialFile
        );
    }

    #[test]
    fn resource_limit_and_summary_counts_are_deterministic() {
        let fixture = Fixture::new("limits");
        let source = fixture.0.join("source.pnach");
        fs::write(&source, vec![0_u8; PREVIEW_MAX_BYTES_PER_FILE as usize + 1]).unwrap();
        let report =
            build_shared_preview(&request(&fixture, &source, "cheats/DEADBEEF.pnach")).unwrap();
        assert_eq!(report.entries[0].state, PreviewState::ResourceLimitReached);
        assert_eq!(report.summary.entries, 1);
        assert_eq!(report.summary.blocked, 1);
        assert_eq!(report.summary.source_files_hashed, 1);
        assert_eq!(report.summary.destination_paths_inspected, 1);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_preserved_and_order_is_deterministic() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new("nonutf8");
        let source = fixture.0.join(OsString::from_vec(vec![b's', 0xff]));
        fs::write(&source, b"source").unwrap();
        let report =
            build_shared_preview(&request(&fixture, &source, "cheats/DEADBEEF.pnach")).unwrap();
        assert_eq!(
            report.entries[0].source_path.as_deref(),
            Some(source.as_path())
        );
        assert_eq!(report.summary.entries, 1);
    }

    #[test]
    fn production_preview_has_no_write_execution_or_network_path() {
        let production = include_str!("shared_preview.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "File::create",
            "fs::write",
            ".write(",
            "create_dir",
            "remove_dir",
            "Command::",
            "std::process",
            "TcpStream",
            "ureq::",
            "http://",
            "https://",
        ] {
            assert!(
                !production.contains(forbidden),
                "forbidden production path: {forbidden}"
            );
        }
    }
}
