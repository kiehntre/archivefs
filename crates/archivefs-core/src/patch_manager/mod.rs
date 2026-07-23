//! Read-only Phase 1 PCSX2 patch metadata preview.
//!
//! This module deliberately contains no artifact retrieval, PNACH parsing,
//! destination inspection, write-capable plan, cache, manifest, or rollback
//! API. Its only output is [`AdvisoryPatchPlan`].
//!
//! PCSX2-only logic (serial/CRC normalization, PNACH filename parsing,
//! candidate conversion, hypothetical destination calculation) lives
//! behind [`adapter::EmulatorAdapter`] in `pcsx2.rs`; this module owns the
//! orchestration (fetch, catalogue read, candidate/game matching,
//! plan/plan-ID assembly) that is still PCSX2-specific for this first
//! adapter slice - see `docs/PATCH_CHEAT_MANAGER_DESIGN.md`.
//!
//! `retroarch` (added after PCSX2) is a second, independent preview: it
//! does not implement `EmulatorAdapter` and does not produce an
//! [`AdvisoryPatchPlan`] - see that module's own doc comment for why, and
//! `docs/RETROARCH_PATCH_PREVIEW.md` for the full design record. Nothing
//! in this top-level module or in `adapter.rs`/`matching.rs`/`pcsx2.rs`/
//! `retrieval.rs` was changed to add it; every PCSX2 type, plan ID, JSON
//! shape, and CLI output listed above remains exactly as it was.

mod adapter;
mod cheat_cache_lock;
mod cheat_cache_maintenance;
mod cheat_catalogue;
mod cheat_history;
mod cheat_install_result;
mod cheat_installer;
mod cheat_rollback;
mod cheat_rollback_result;
mod cheat_sources;
mod destination_safety;
mod dolphin_local;
mod import_safety;
mod matching;
mod pcsx2;
mod pcsx2_local;
mod retrieval;
mod retroarch;
mod retroarch_cheat_library;
mod retroarch_cheat_setup;
mod retroarch_inventory;
mod retroarch_materialization;
mod shared_preview;
mod shared_transaction;

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Database, PersistedArchive};

pub use adapter::{
    AdapterCapabilities, AdapterId, AdapterIdentityEvidence, DiscoveryConfidence, EmulatorAdapter,
    HypotheticalDestination, InstallationCandidate,
};
pub use cheat_cache_maintenance::{
    CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION, CachePruneDisposition, CachePruneEntryKind,
    CachePruneEntryStatus, CachePruneExecutionEntry, CachePruneExecutionResult,
    CachePruneExecutionStatus, CachePrunePlan, CachePrunePlanEntry, CachePrunePolicy,
    CachePruneReason, DEFAULT_ABANDONED_STAGING_MIN_AGE_SECONDS,
    MINIMUM_ABANDONED_STAGING_AGE_SECONDS, SnapshotInventoryEntry, SnapshotInventoryReport,
    SnapshotPinResult, SnapshotPinStatus, SnapshotVerificationFinding, SnapshotVerificationReport,
    SnapshotVerificationState, execute_retroarch_cheat_cache_prune,
    inventory_retroarch_cheat_snapshots, plan_retroarch_cheat_cache_prune,
    set_retroarch_cheat_snapshot_pin, verify_retroarch_cheat_snapshots,
};
pub use cheat_catalogue::{
    CHEAT_CATALOGUE_FORMAT_VERSION, CatalogueDiagnostic, CheatAvailabilityEntry,
    CheatAvailabilityReport, CheatAvailabilitySummary, CheatCatalogueFormat,
    CheatCatalogueSnapshot, CheatCatalogueSource, CheatDefinition, CheatGameMatch, CheatGameRecord,
    CheatInstalledState, CheatMatchCandidate, CheatMatchConfidence, CheatMatchEvidence,
    CheatStagingAction, CheatStagingPlan, JsonManifestSource, RetroarchChtDirectorySource,
    build_cheat_availability_report, load_cheat_catalogue_snapshot, match_cheat_game_record,
};
pub use cheat_history::{
    CHEAT_HISTORY_RESULT_SCHEMA_VERSION, CheatBackupAssessment, CheatDestinationAssessment,
    CheatHistoryEntry, CheatHistoryOptions, CheatHistoryReport, CheatHistoryWarning,
    CheatInspectionPath, CheatJournalInspection, CheatJournalInspectionError,
    CheatRollbackAvailability, CheatRollbackJournalMatch, discover_cheat_history,
    inspect_cheat_install_journal,
};
pub use cheat_install_result::{
    CHEAT_INSTALL_RUN_SCHEMA_VERSION, CheatInstallEntryResult, CheatInstallOutcome,
    CheatInstallPath, CheatInstallRun, CheatInstallRunSchemaError, CheatInstallRunStatus,
    CheatInstallSummary, PreviousDestinationState, parse_cheat_install_run,
    plan_cheat_install_entries, plan_cheat_install_entry, plan_cheat_install_run,
};
pub use cheat_installer::{
    CHEAT_INSTALL_BACKUPS_DIRECTORY_NAME, CHEAT_INSTALL_RUNS_DIRECTORY_NAME, CheatInstallOptions,
    CheatInstallRunOutcome, execute_cheat_install_run,
};
pub use cheat_rollback::{
    CHEAT_ROLLBACK_RUNS_DIRECTORY_NAME, CheatRollbackOptions, CheatRollbackRunOutcome,
    execute_cheat_rollback_run,
};
pub use cheat_rollback_result::{
    CHEAT_ROLLBACK_RUN_SCHEMA_VERSION, CheatRollbackEntryResult, CheatRollbackOutcome,
    CheatRollbackRun, CheatRollbackRunSchemaError, CheatRollbackRunStatus, CheatRollbackSummary,
    parse_cheat_rollback_run,
};
pub use cheat_sources::{
    CHEAT_SOURCE_RESULT_SCHEMA_VERSION, CheatSourceCacheMetadata, CheatSourceDefinition,
    CheatSourceError, CheatSourceErrorStage, CheatSourceFetchOptions, CheatSourceFetchResult,
    CheatSourceFetchStatus, CheatSourceFreshness, CheatSourceHttpResponse, CheatSourceInspection,
    CheatSourceList, CheatSourceListEntry, CheatSourceManifest, CheatSourceManifestFile,
    CheatSourceSetupContext, CheatSourceTransport, HttpsCheatSourceTransport,
    default_cheat_source_cache_root, fetch_retroarch_cheat_source, inspect_retroarch_cheat_source,
    inspect_retroarch_cheat_source_snapshot, list_retroarch_cheat_sources,
    trusted_retroarch_cheat_sources,
};
pub use destination_safety::{
    DestinationRootState, DestinationSafetyAssessment, DestinationSafetyError,
    DestinationSafetyFailureReason, DestinationState, InspectedParent, InspectedParentState,
    SafeDestination, ValidatedDestinationRoot, assess_destination, construct_safe_destination,
    inspect_safe_destination, validate_destination_root,
};
pub use dolphin_local::{
    DOLPHIN_MAX_ENTRIES_VISITED, DOLPHIN_MAX_GAME_INI_BYTES, DOLPHIN_MAX_GAME_INI_FILES,
    DOLPHIN_MAX_LINE_BYTES, DOLPHIN_MAX_LINES_PER_FILE, DOLPHIN_MAX_PROFILES,
    DOLPHIN_MAX_TOTAL_GAME_INI_BYTES, DolphinCodeKind, DolphinDirectoryIdentity,
    DolphinDiscoveryError, DolphinGameIniFile, DolphinGameIniInventory, DolphinInspectionError,
    DolphinInspectionWarning, DolphinInspectionWarningKind, DolphinInstallationType,
    DolphinMatchResult, DolphinMatchState, DolphinProfile, DolphinProfileBlocker,
    DolphinProfileBlockerKind, DolphinProfileDiscovery, DolphinProfileDiscoveryRoots,
    DolphinProfileScope, DolphinSettingsDirectoryState, discover_dolphin_profiles,
    inspect_dolphin_profile, match_dolphin_inventory,
};
pub use import_safety::{
    ActiveContentDisposition, ActiveContentPolicy, ImportConsentSummary, ImportInspectionState,
    ImportSourceKind, ImportTrustState, LocalSafetyScanningState, UNKNOWN_CODE_POLICY,
    automatic_execution_allowed, classify_active_content, trust_after_inspection,
};
pub use pcsx2::{
    HostReadOnlyFilesystem, Pcsx2CandidateKind, Pcsx2DiscoveryConfidence, Pcsx2DiscoveryRoots,
    Pcsx2InstallationCandidate, ReadOnlyFilesystem, ReadOnlyPcsx2Adapter,
};
pub use pcsx2_local::{
    PCSX2_MAX_DIRECTORIES_TRAVERSED, PCSX2_MAX_DIRECTORY_DEPTH, PCSX2_MAX_ENTRIES_VISITED,
    PCSX2_MAX_LINE_BYTES, PCSX2_MAX_LINES_PER_FILE, PCSX2_MAX_PATCH_DIRECTORIES_PER_PROFILE,
    PCSX2_MAX_PNACH_FILE_BYTES, PCSX2_MAX_PNACH_FILES, PCSX2_MAX_PROFILES,
    PCSX2_MAX_TOTAL_PNACH_BYTES, Pcsx2DirectoryIdentity, Pcsx2DiscoveryError, Pcsx2InspectionError,
    Pcsx2InspectionWarning, Pcsx2InspectionWarningKind, Pcsx2InstallationType, Pcsx2MatchResult,
    Pcsx2MatchState, Pcsx2PatchCategory, Pcsx2PatchDirectory, Pcsx2PatchDirectoryState,
    Pcsx2PnachFile, Pcsx2PnachInventory, Pcsx2Profile, Pcsx2ProfileBlocker,
    Pcsx2ProfileBlockerKind, Pcsx2ProfileDiscovery, Pcsx2ProfileDiscoveryRoots, Pcsx2ProfileScope,
    discover_pcsx2_profiles, inspect_pcsx2_profile, match_pcsx2_inventory,
};
pub use retrieval::{HttpsMetadataFetcher, MetadataFetcher};
pub use retroarch::{
    CoreAssociation, CoreMatchDisposition, CoreSelectionSource, DestinationKind, PlaylistEvidence,
    PlaylistMatchConfidence, ProposedDestination, RetroArchAdvisoryEntry, RetroArchAdvisoryPlan,
    RetroArchAdvisorySummary, RetroArchProfileOutcome,
    preview_retroarch_patch_and_cheat_destinations,
};
pub use retroarch_cheat_library::{
    RETROARCH_CHEAT_LIBRARY_MAX_DEPTH, RETROARCH_CHEAT_LIBRARY_MAX_ENTRIES,
    RetroArchCheatLibraryInspection, RetroArchCheatLibraryState, inspect_retroarch_cheat_library,
};
pub use retroarch_cheat_setup::{
    RETROARCH_CHEAT_SETUP_SCHEMA_VERSION, RetroArchCheatSetupDiscovery, RetroArchCheatSetupError,
    RetroArchCheatSetupMessage, RetroArchCheatSetupNextStep, RetroArchCheatSetupPlan,
    RetroArchCheatSetupPlannedAction, RetroArchCheatSetupPlannedEntry, RetroArchCheatSetupPreview,
    RetroArchCheatSetupPreviewSummary, RetroArchCheatSetupProfile,
    RetroArchCheatSetupProfileBlocker, RetroArchCheatSetupProfileState, RetroArchCheatSetupResult,
    RetroArchCheatSetupStatus, build_retroarch_cheat_setup_plan,
    discover_retroarch_cheat_setup_profiles, resolve_retroarch_cheat_setup_profile,
};
pub use retroarch_inventory::{
    ArtifactAssociation, ArtifactAssociationConfidence, ArtifactCatalogueGame,
    ArtifactConflictState, ArtifactDiagnostic, ArtifactDiagnosticSeverity, ArtifactKind,
    ArtifactPlaylistEvidence, CheatFileSummary, RetroArchArtifactDestination,
    RetroArchArtifactFinding, RetroArchArtifactInventory, RetroArchArtifactSummary,
};
pub use retroarch_materialization::{
    RETROARCH_MAX_MATERIALIZED_ENTRIES, RetroArchMaterializationError,
    RetroArchMaterializationErrorKind, RetroArchMaterializationRequest,
    RetroArchMaterializedPreview, RetroArchMaterializedSource,
    materialize_retroarch_shared_preview,
};
pub use shared_preview::{
    PREVIEW_MAX_BYTES_PER_FILE, PREVIEW_MAX_CONFLICTS, PREVIEW_MAX_DESTINATION_FILES_HASHED,
    PREVIEW_MAX_DESTINATION_PATHS, PREVIEW_MAX_ENTRIES, PREVIEW_MAX_SOURCE_FILES_HASHED,
    PREVIEW_MAX_TOTAL_BYTES_HASHED, PREVIEW_MAX_WARNINGS, PreviewAdapter, PreviewBlocker,
    PreviewBlockerKind, PreviewConflict, PreviewConflictKind, PreviewDestinationState,
    PreviewEligibility, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewProposedAction, PreviewSourceItem, PreviewState, PreviewSummary,
    PreviewWarning, PreviewWarningKind, SharedPreviewEntry, SharedPreviewError,
    SharedPreviewReport, SharedPreviewRequest, build_shared_preview,
};
pub use shared_transaction::{
    SHARED_APPLY_SCHEMA_VERSION, SHARED_MAX_BACKUP_BYTES, SHARED_MAX_CREATED_DIRECTORIES,
    SHARED_MAX_ENTRIES, SHARED_MAX_FAILURES, SHARED_MAX_HISTORY_JOURNALS, SHARED_MAX_JOURNAL_BYTES,
    SHARED_MAX_ROLLBACK_ENTRIES, SHARED_MAX_SOURCE_BYTES, SHARED_MAX_TEMP_FILES,
    SHARED_MAX_TOTAL_WRITTEN_BYTES, SHARED_MAX_WARNINGS, SharedAdapterWriteSupport,
    SharedApplyConfirmation, SharedApplyContext, SharedApplyEntry, SharedApplyFailure,
    SharedApplyFailureKind, SharedApplyJournal, SharedApplyOptions, SharedApplyOutcome,
    SharedApplyResult, SharedApplyStatus, SharedHistoryReport, SharedJournalWarning,
    SharedPlanEntry, SharedRollbackConfirmation, SharedRollbackEntry, SharedRollbackOptions,
    SharedRollbackOutcome, SharedRollbackPreview, SharedRollbackResult, SharedTransactionPath,
    SharedTransactionPlan, SharedTransactionStage, adapter_write_support,
    build_shared_transaction_plan, default_shared_backup_root, default_shared_history_root,
    discover_shared_apply_history, execute_shared_apply, execute_shared_rollback,
    generate_shared_operation_id, preview_shared_rollback,
};

pub const BUILT_IN_SOURCE_ID: &str = "pcsx2-official-patches-tree";
pub const BUILT_IN_SOURCE_NAME: &str = "PCSX2 official patch repository metadata";
pub const BUILT_IN_SOURCE_URL: &str =
    "https://api.github.com/repos/PCSX2/pcsx2_patches/git/trees/main?recursive=1";
pub const BUILT_IN_SOURCE_PROVENANCE: &str = "PCSX2/pcsx2_patches official Git repository";
pub const BUILT_IN_SOURCE_LICENSE: &str =
    "Repository metadata only; upstream does not declare one repository-wide patch license";
pub const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_METADATA_RECORDS: usize = 50_000;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_METADATA_STRING_BYTES: usize = 4 * 1024;
pub const METADATA_SCHEMA: &str = "github-git-tree-v1";

pub type Result<T> = std::result::Result<T, PatchManagerError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchManagerError {
    Network(String),
    RedirectRejected { status: u16 },
    ResponseTooLarge { limit: usize },
    MalformedMetadata(String),
    UnsupportedMetadata(String),
    Catalogue(String),
    Discovery(String),
}

impl fmt::Display for PatchManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(message) => write!(formatter, "metadata network error: {message}"),
            Self::RedirectRejected { status } => write!(
                formatter,
                "metadata redirect rejected (HTTP {status}); Phase 1 does not follow redirects"
            ),
            Self::ResponseTooLarge { limit } => write!(
                formatter,
                "metadata response exceeds the {limit}-byte Phase 1 limit"
            ),
            Self::MalformedMetadata(message) => {
                write!(formatter, "malformed PCSX2 metadata: {message}")
            }
            Self::UnsupportedMetadata(message) => {
                write!(formatter, "unsupported PCSX2 metadata: {message}")
            }
            Self::Catalogue(message) => write!(formatter, "read-only catalogue error: {message}"),
            Self::Discovery(message) => write!(formatter, "PCSX2 discovery error: {message}"),
        }
    }
}

impl std::error::Error for PatchManagerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataFetch {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub verification: VerificationLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VerificationLevel {
    TransportOnly,
}

impl VerificationLevel {
    pub fn explanation(self) -> &'static str {
        match self {
            Self::TransportOnly => {
                "HTTPS transport verified; downloaded metadata is not signed content"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSnapshot {
    pub id: &'static str,
    pub display_name: &'static str,
    pub endpoint: &'static str,
    pub provenance: &'static str,
    pub license_notice: &'static str,
    pub metadata_schema: &'static str,
    pub source_version: String,
    pub metadata_sha256: String,
    pub verification: VerificationLevel,
    pub verification_explanation: &'static str,
    pub freshness_explanation: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSnapshot {
    pub source: SourceSnapshot,
    pub records: Vec<PatchMetadataRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchMetadataRecord {
    pub record_id: String,
    pub repository_path: String,
    pub patch_blob_id: String,
    pub title: Option<String>,
    pub platform: String,
    pub region: Option<String>,
    pub serial: Option<String>,
    pub executable_crc: Option<String>,
    pub metadata_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueGameEvidence {
    pub archive_id: i64,
    pub is_present: bool,
    pub display_name: String,
    pub normalized_name: String,
    pub platform: Option<String>,
    pub region: Option<String>,
    /// Present only when supplied by an approved catalogue identity field.
    /// Phase 1 never derives this from a filename.
    pub serial: Option<String>,
    /// Present only when supplied by an approved catalogue identity field.
    /// Phase 1 never computes this from game content.
    pub executable_crc: Option<String>,
}

impl From<&PersistedArchive> for CatalogueGameEvidence {
    fn from(archive: &PersistedArchive) -> Self {
        Self {
            archive_id: archive.id,
            is_present: archive.last_verified_missing_at.is_none(),
            display_name: archive.display_name.clone(),
            normalized_name: archive.normalized_name.clone(),
            platform: archive.platform.clone(),
            region: None,
            serial: None,
            executable_crc: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum MatchConfidence {
    NoMatch,
    Uncertain,
    Probable,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AdvisoryDisposition {
    Preview,
    MissingGame,
    NoInstallationCandidate,
    AmbiguousGame,
    AmbiguousInstallationCandidates,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameMatch {
    pub confidence: MatchConfidence,
    pub catalogue_archive_ids: Vec<i64>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryPlanEntry {
    pub record: PatchMetadataRecord,
    pub disposition: AdvisoryDisposition,
    pub game_match: GameMatch,
    pub hypothetical_destinations: Vec<HypotheticalDestination>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryPlanSummary {
    pub metadata_records: usize,
    pub exact_matches: usize,
    pub probable_matches: usize,
    pub uncertain_matches: usize,
    pub ambiguous_matches: usize,
    pub missing_games: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryPatchPlan {
    pub format_version: u32,
    pub plan_id: String,
    pub executable: bool,
    pub source: SourceSnapshot,
    pub installation_candidates: Vec<InstallationCandidate>,
    pub entries: Vec<AdvisoryPlanEntry>,
    pub summary: AdvisoryPlanSummary,
}

#[derive(Debug, Deserialize)]
struct GitTreeDocument {
    sha: String,
    #[serde(default)]
    truncated: bool,
    tree: Vec<GitTreeEntry>,
}

#[derive(Debug, Deserialize)]
struct GitTreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    sha: String,
    size: Option<u64>,
}

pub fn fetch_built_in_metadata(fetcher: &dyn MetadataFetcher) -> Result<MetadataSnapshot> {
    let response = fetcher.fetch(BUILT_IN_SOURCE_URL)?;
    if (300..400).contains(&response.status) {
        return Err(PatchManagerError::RedirectRejected {
            status: response.status,
        });
    }
    if !(200..300).contains(&response.status) {
        return Err(PatchManagerError::Network(format!(
            "metadata server returned HTTP {}",
            response.status
        )));
    }
    if response.body.len() > MAX_METADATA_BYTES {
        return Err(PatchManagerError::ResponseTooLarge {
            limit: MAX_METADATA_BYTES,
        });
    }
    if !response.content_type.is_empty()
        && !response.content_type.contains("json")
        && !response.content_type.contains("github")
    {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "unexpected content type {}",
            response.content_type
        )));
    }
    validate_json_depth(&response.body, MAX_JSON_DEPTH)?;

    let document: GitTreeDocument = serde_json::from_slice(&response.body)
        .map_err(|error| PatchManagerError::MalformedMetadata(error.to_string()))?;
    if document.truncated {
        return Err(PatchManagerError::UnsupportedMetadata(
            "the upstream Git tree response is truncated".to_string(),
        ));
    }
    validate_git_object_id("source version", &document.sha)?;
    if document.tree.len() > MAX_METADATA_RECORDS {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "metadata contains {} entries; limit is {MAX_METADATA_RECORDS}",
            document.tree.len()
        )));
    }

    let metadata_hash = hex_sha256(&response.body);
    let mut records = Vec::new();
    let mut record_ids = BTreeSet::new();
    for entry in document.tree {
        if let Some(record) = git_tree_entry_to_record(entry)? {
            if !record_ids.insert(record.record_id.clone()) {
                return Err(PatchManagerError::MalformedMetadata(format!(
                    "duplicate patch record {}",
                    record.record_id
                )));
            }
            records.push(record);
        }
    }
    records.sort_by(|left, right| left.record_id.cmp(&right.record_id));
    let verification = response.verification;

    Ok(MetadataSnapshot {
        source: SourceSnapshot {
            id: BUILT_IN_SOURCE_ID,
            display_name: BUILT_IN_SOURCE_NAME,
            endpoint: BUILT_IN_SOURCE_URL,
            provenance: BUILT_IN_SOURCE_PROVENANCE,
            license_notice: BUILT_IN_SOURCE_LICENSE,
            metadata_schema: METADATA_SCHEMA,
            source_version: document.sha,
            metadata_sha256: metadata_hash,
            verification,
            verification_explanation: verification.explanation(),
            freshness_explanation: "No authenticated timestamp or monotonic version; first-seen replay cannot be detected",
        },
        records,
    })
}

fn git_tree_entry_to_record(entry: GitTreeEntry) -> Result<Option<PatchMetadataRecord>> {
    validate_metadata_string("repository path", &entry.path)?;
    validate_repository_path(&entry.path)?;
    validate_metadata_string("tree entry type", &entry.entry_type)?;
    validate_git_object_id("tree entry SHA", &entry.sha)?;
    if entry.entry_type != "blob"
        || !entry.path.starts_with("patches/")
        || !entry.path.ends_with(".pnach")
    {
        return Ok(None);
    }
    if entry
        .size
        .is_some_and(|size| size > MAX_METADATA_BYTES as u64)
    {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "metadata advertises an oversized patch entry at {}",
            entry.path
        )));
    }
    let file_name = entry
        .path
        .strip_prefix("patches/")
        .filter(|name| !name.contains('/') && !name.contains('\\'))
        .ok_or_else(|| {
            PatchManagerError::UnsupportedMetadata(format!(
                "unsafe or nested patch metadata path {}",
                entry.path
            ))
        })?;
    let stem = file_name.strip_suffix(".pnach").ok_or_else(|| {
        PatchManagerError::MalformedMetadata("patch path lacks .pnach suffix".to_string())
    })?;
    let (serial, executable_crc) = pcsx2::parse_patch_identity(stem);
    Ok(Some(PatchMetadataRecord {
        record_id: entry.path.clone(),
        repository_path: entry.path,
        patch_blob_id: entry.sha,
        title: None,
        platform: "PS2".to_string(),
        region: None,
        serial,
        executable_crc,
        metadata_kind: "PCSX2 repository patch record".to_string(),
    }))
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn match_record<A: EmulatorAdapter>(
    adapter: &A,
    record: &PatchMetadataRecord,
    catalogue: &[CatalogueGameEvidence],
) -> (GameMatch, bool) {
    let mut candidates = Vec::<(MatchConfidence, i64, Vec<String>)>::new();
    let mut contradictions = Vec::new();
    let record_evidence = adapter.identity_evidence_from_record(record);
    for game in catalogue {
        if !game.is_present || game.platform.as_deref() != Some("PS2") {
            continue;
        }

        let catalogue_evidence = adapter.identity_evidence_from_catalogue(game);
        match matching::exact_tier_outcome(&record_evidence, &catalogue_evidence) {
            matching::ExactTierOutcome::Conflict(reasons) => {
                contradictions.extend(reasons);
                continue;
            }
            matching::ExactTierOutcome::Exact(reasons) => {
                candidates.push((MatchConfidence::Exact, game.archive_id, reasons));
                continue;
            }
            matching::ExactTierOutcome::NotApplicable => {}
        }

        if let Some(title) = &record.title {
            let title_matches = normalize_title(title) == normalize_title(&game.normalized_name);
            let region_compatible = match (&record.region, &game.region) {
                (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                (Some(_), None) => false,
                _ => true,
            };
            if title_matches && region_compatible {
                candidates.push((
                    MatchConfidence::Probable,
                    game.archive_id,
                    vec!["normalized title and PS2 platform match".to_string()],
                ));
                continue;
            }
        }

        let display = game.display_name.to_ascii_uppercase();
        let filename_similarity = record
            .serial
            .as_ref()
            .is_some_and(|serial| display.contains(serial))
            || record
                .executable_crc
                .as_ref()
                .is_some_and(|crc| display.contains(crc));
        if filename_similarity {
            candidates.push((
                MatchConfidence::Uncertain,
                game.archive_id,
                vec![
                    "filename text contains a patch identifier; explicit review required"
                        .to_string(),
                ],
            ));
        }
    }

    let best = candidates
        .iter()
        .map(|candidate| candidate.0)
        .max()
        .unwrap_or(MatchConfidence::NoMatch);
    let best_candidates = candidates
        .into_iter()
        .filter(|candidate| candidate.0 == best)
        .collect::<Vec<_>>();
    let ambiguous = best != MatchConfidence::NoMatch && best_candidates.len() > 1;
    let mut ids = best_candidates
        .iter()
        .map(|candidate| candidate.1)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let mut reasons = best_candidates
        .iter()
        .flat_map(|candidate| candidate.2.iter().cloned())
        .collect::<Vec<_>>();
    if best == MatchConfidence::NoMatch {
        reasons.extend(contradictions);
    }
    reasons.sort();
    reasons.dedup();
    if best == MatchConfidence::NoMatch {
        reasons.push("no compatible catalogue identity evidence".to_string());
    }
    if ambiguous {
        reasons.push("multiple catalogue archives share the strongest evidence".to_string());
    }
    (
        GameMatch {
            confidence: best,
            catalogue_archive_ids: ids,
            reasons,
        },
        ambiguous,
    )
}

pub fn build_advisory_plan<A: EmulatorAdapter>(
    adapter: &A,
    snapshot: MetadataSnapshot,
    installation_candidates: Vec<InstallationCandidate>,
    catalogue: &[CatalogueGameEvidence],
) -> AdvisoryPatchPlan {
    let installation_ambiguity = installation_candidates.len() > 1;
    let missing_emulator_candidate = installation_candidates.is_empty();
    let entries = snapshot
        .records
        .iter()
        .cloned()
        .map(|record| {
            let (game_match, ambiguous_game) = match_record(adapter, &record, catalogue);
            let disposition = if missing_emulator_candidate {
                AdvisoryDisposition::NoInstallationCandidate
            } else if installation_ambiguity {
                AdvisoryDisposition::AmbiguousInstallationCandidates
            } else if ambiguous_game {
                AdvisoryDisposition::AmbiguousGame
            } else if game_match.confidence == MatchConfidence::NoMatch {
                AdvisoryDisposition::MissingGame
            } else {
                AdvisoryDisposition::Preview
            };
            let hypothetical_destinations = match adapter.hypothetical_relative_path(&record) {
                Some(relative_path) => installation_candidates
                    .iter()
                    .map(|installation| HypotheticalDestination {
                        candidate_kind: installation.kind.clone(),
                        relative_path: relative_path.clone(),
                        display_path: installation
                            .data_root
                            .join(&relative_path)
                            .to_string_lossy()
                            .into_owned(),
                        hypothetical: true,
                    })
                    .collect(),
                None => Vec::new(),
            };
            let mut reasons = game_match.reasons.clone();
            reasons.push("metadata preview only; no PNACH content was downloaded".to_string());
            if installation_ambiguity {
                reasons.push(
                    "multiple standard-path PCSX2 candidates were found; none was selected"
                        .to_string(),
                );
            }
            if missing_emulator_candidate {
                reasons.push(
                    "no supported standard-path PCSX2 candidate was found; this does not prove PCSX2 is absent"
                        .to_string(),
                );
            }
            AdvisoryPlanEntry {
                record,
                disposition,
                game_match,
                hypothetical_destinations,
                reasons,
            }
        })
        .collect::<Vec<_>>();

    let summary = AdvisoryPlanSummary {
        metadata_records: entries.len(),
        exact_matches: entries
            .iter()
            .filter(|entry| entry.game_match.confidence == MatchConfidence::Exact)
            .count(),
        probable_matches: entries
            .iter()
            .filter(|entry| entry.game_match.confidence == MatchConfidence::Probable)
            .count(),
        uncertain_matches: entries
            .iter()
            .filter(|entry| entry.game_match.confidence == MatchConfidence::Uncertain)
            .count(),
        ambiguous_matches: entries
            .iter()
            .filter(|entry| entry.disposition == AdvisoryDisposition::AmbiguousGame)
            .count(),
        missing_games: entries
            .iter()
            .filter(|entry| entry.game_match.confidence == MatchConfidence::NoMatch)
            .count(),
    };
    let plan_id = compute_plan_id(&snapshot.source, &installation_candidates, &entries);

    AdvisoryPatchPlan {
        format_version: 1,
        plan_id,
        executable: false,
        source: snapshot.source,
        installation_candidates,
        entries,
        summary,
    }
}

fn compute_plan_id(
    source: &SourceSnapshot,
    candidates: &[InstallationCandidate],
    entries: &[AdvisoryPlanEntry],
) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"archivefs-advisory-plan-v1");
    for value in [
        source.id,
        source.endpoint,
        source.metadata_schema,
        &source.source_version,
        &source.metadata_sha256,
    ] {
        hash_field(&mut hasher, value.as_bytes());
    }
    hash_field(&mut hasher, verification_tag(source.verification));
    for candidate in candidates {
        hash_candidate_kind(&mut hasher, &candidate.kind);
        hash_field(
            &mut hasher,
            candidate.data_root.as_os_str().as_encoded_bytes(),
        );
        hash_field(&mut hasher, candidate.provenance.as_bytes());
        hash_optional_string(&mut hasher, candidate.detected_version.as_deref());
    }
    for entry in entries {
        for value in [
            &entry.record.record_id,
            &entry.record.repository_path,
            &entry.record.patch_blob_id,
            &entry.record.platform,
            &entry.record.metadata_kind,
        ] {
            hash_field(&mut hasher, value.as_bytes());
        }
        for value in [
            entry.record.title.as_deref(),
            entry.record.region.as_deref(),
            entry.record.serial.as_deref(),
            entry.record.executable_crc.as_deref(),
        ] {
            hash_optional_string(&mut hasher, value);
        }
        hash_field(&mut hasher, disposition_tag(entry.disposition));
        hash_field(&mut hasher, confidence_tag(entry.game_match.confidence));
        for archive_id in &entry.game_match.catalogue_archive_ids {
            hash_field(&mut hasher, &archive_id.to_le_bytes());
        }
        for destination in &entry.hypothetical_destinations {
            hash_candidate_kind(&mut hasher, &destination.candidate_kind);
            hash_field(&mut hasher, destination.relative_path.as_bytes());
        }
    }
    encode_hex(&hasher.finalize())
}

fn hash_optional_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_field(hasher, b"some");
            hash_field(hasher, value.as_bytes());
        }
        None => hash_field(hasher, b"none"),
    }
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Hashes a neutral candidate `kind` string ("Native"/"Flatpak") the same
/// way the pre-extraction `candidate_kind_tag` hashed the old
/// `Pcsx2CandidateKind` enum - lowercased, so `plan_id` is byte-identical
/// before and after this extraction for every kind PCSX2 produces today.
/// Adapter-neutral: it makes no assumption about which kind strings exist.
fn hash_candidate_kind(hasher: &mut Sha256, kind: &str) {
    hash_field(hasher, kind.to_ascii_lowercase().as_bytes());
}

fn disposition_tag(disposition: AdvisoryDisposition) -> &'static [u8] {
    match disposition {
        AdvisoryDisposition::Preview => b"preview",
        AdvisoryDisposition::MissingGame => b"missing-game",
        AdvisoryDisposition::NoInstallationCandidate => b"no-installation-candidate",
        AdvisoryDisposition::AmbiguousGame => b"ambiguous-game",
        AdvisoryDisposition::AmbiguousInstallationCandidates => {
            b"ambiguous-installation-candidates"
        }
        AdvisoryDisposition::Unsupported => b"unsupported",
    }
}

fn confidence_tag(confidence: MatchConfidence) -> &'static [u8] {
    match confidence {
        MatchConfidence::NoMatch => b"no-match",
        MatchConfidence::Uncertain => b"uncertain",
        MatchConfidence::Probable => b"probable",
        MatchConfidence::Exact => b"exact",
    }
}

fn verification_tag(verification: VerificationLevel) -> &'static [u8] {
    match verification {
        VerificationLevel::TransportOnly => b"transport-only",
    }
}

pub fn load_catalogue_evidence_read_only(path: &Path) -> Result<Vec<CatalogueGameEvidence>> {
    let database = Database::open_read_only(path)
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    let archives = database
        .load_archives()
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    database
        .close()
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    let evidence = archives
        .iter()
        .map(CatalogueGameEvidence::from)
        .collect::<Vec<_>>();
    for game in &evidence {
        validate_catalogue_evidence(game)?;
    }
    Ok(evidence)
}

fn validate_catalogue_evidence(game: &CatalogueGameEvidence) -> Result<()> {
    validate_metadata_string("catalogue display name", &game.display_name)?;
    validate_metadata_string("catalogue normalized name", &game.normalized_name)?;
    if let Some(platform) = &game.platform {
        validate_metadata_string("catalogue platform", platform)?;
    }
    if let Some(region) = &game.region {
        validate_metadata_string("catalogue region", region)?;
    }
    if let Some(serial) = &game.serial {
        validate_metadata_string("catalogue serial", serial)?;
    }
    if let Some(crc) = &game.executable_crc {
        validate_metadata_string("catalogue executable CRC", crc)?;
    }
    Ok(())
}

pub fn preview_pcsx2_metadata<F: ReadOnlyFilesystem>(
    fetcher: &dyn MetadataFetcher,
    adapter: &ReadOnlyPcsx2Adapter<F>,
    catalogue_path: &Path,
) -> Result<AdvisoryPatchPlan> {
    let snapshot = fetch_built_in_metadata(fetcher)?;
    let installation_candidates = adapter.discover_installations()?;
    let catalogue = load_catalogue_evidence_read_only(catalogue_path)?;
    Ok(build_advisory_plan(
        adapter,
        snapshot,
        installation_candidates,
        &catalogue,
    ))
}

fn validate_metadata_string(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_METADATA_STRING_BYTES {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "{field} exceeds the {MAX_METADATA_STRING_BYTES}-byte string limit"
        )));
    }
    if value.contains('\0') {
        return Err(PatchManagerError::MalformedMetadata(format!(
            "{field} contains a NUL byte"
        )));
    }
    Ok(())
}

fn validate_git_object_id(field: &str, value: &str) -> Result<()> {
    validate_metadata_string(field, value)?;
    if value.len() != 40 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(PatchManagerError::MalformedMetadata(format!(
            "{field} is not a supported Git object ID"
        )));
    }
    Ok(())
}

fn validate_repository_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "unsafe repository path {path:?}"
        )));
    }
    Ok(())
}

fn validate_json_depth(bytes: &[u8], max_depth: usize) -> Result<()> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max_depth {
                    return Err(PatchManagerError::UnsupportedMetadata(format!(
                        "JSON nesting exceeds the depth limit of {max_depth}"
                    )));
                }
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    PatchManagerError::MalformedMetadata(
                        "JSON contains an unmatched closing delimiter".to_string(),
                    )
                })?;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(PatchManagerError::MalformedMetadata(
            "JSON document is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct StaticFetcher {
        response: MetadataFetch,
    }

    impl MetadataFetcher for StaticFetcher {
        fn fetch(&self, _url: &str) -> Result<MetadataFetch> {
            Ok(self.response.clone())
        }
    }

    fn fetcher(body: &[u8]) -> StaticFetcher {
        StaticFetcher {
            response: MetadataFetch {
                status: 200,
                content_type: "application/json".to_string(),
                body: body.to_vec(),
                verification: VerificationLevel::TransportOnly,
            },
        }
    }

    fn snapshot_with(record: PatchMetadataRecord) -> MetadataSnapshot {
        MetadataSnapshot {
            source: SourceSnapshot {
                id: BUILT_IN_SOURCE_ID,
                display_name: BUILT_IN_SOURCE_NAME,
                endpoint: BUILT_IN_SOURCE_URL,
                provenance: BUILT_IN_SOURCE_PROVENANCE,
                license_notice: BUILT_IN_SOURCE_LICENSE,
                metadata_schema: METADATA_SCHEMA,
                source_version: "version".to_string(),
                metadata_sha256: "hash".to_string(),
                verification: VerificationLevel::TransportOnly,
                verification_explanation: VerificationLevel::TransportOnly.explanation(),
                freshness_explanation: "No authenticated timestamp or monotonic version; first-seen replay cannot be detected",
            },
            records: vec![record],
        }
    }

    fn record() -> PatchMetadataRecord {
        PatchMetadataRecord {
            record_id: "patches/SLUS-20312_A1B2C3D4.pnach".to_string(),
            repository_path: "patches/SLUS-20312_A1B2C3D4.pnach".to_string(),
            patch_blob_id: "blob".to_string(),
            title: Some("Example Game".to_string()),
            platform: "PS2".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-20312".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
            metadata_kind: "synthetic matcher fixture".to_string(),
        }
    }

    fn game(id: i64) -> CatalogueGameEvidence {
        CatalogueGameEvidence {
            archive_id: id,
            is_present: true,
            display_name: "Example Game".to_string(),
            normalized_name: "examplegame".to_string(),
            platform: Some("PS2".to_string()),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-20312".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
        }
    }

    fn installation() -> InstallationCandidate {
        InstallationCandidate {
            adapter_id: "pcsx2",
            kind: "Native".to_string(),
            data_root: PathBuf::from("/home/test/.config/PCSX2"),
            provenance: "test",
            discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
            detected_version: None,
            mutation_readiness: "NotEvaluated",
        }
    }

    /// An adapter instance for tests that exercise shared matching/planning
    /// logic directly (never calling `.discover()`/`.discover_installations()`,
    /// so these placeholder roots are never actually read).
    fn test_adapter() -> ReadOnlyPcsx2Adapter<HostReadOnlyFilesystem> {
        ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused-in-tests"),
                xdg_config_home: PathBuf::from("/unused-in-tests/.config"),
            },
        )
    }

    #[test]
    fn matcher_exact_identity_requires_all_declared_exact_identifiers() {
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[game(7)],
        );

        assert_eq!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::Exact
        );
        assert_eq!(plan.entries[0].game_match.catalogue_archive_ids, vec![7]);
        assert!(!plan.executable);

        let mut crc_missing = game(8);
        crc_missing.executable_crc = None;
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[crc_missing],
        );
        assert_ne!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::Exact
        );
    }

    #[test]
    fn duplicate_exact_identity_is_ambiguous_not_actionable() {
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[game(7), game(8)],
        );

        assert_eq!(
            plan.entries[0].disposition,
            AdvisoryDisposition::AmbiguousGame
        );
        assert_eq!(plan.entries[0].game_match.catalogue_archive_ids, vec![7, 8]);
    }

    #[test]
    fn multiple_installation_candidates_remain_blocked_and_distinct() {
        let native = installation();
        let mut flatpak = installation();
        flatpak.kind = "Flatpak".to_string();
        flatpak.data_root = PathBuf::from("/home/test/.var/app/net.pcsx2.PCSX2/config/PCSX2");
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![native, flatpak],
            &[game(7)],
        );

        assert_eq!(
            plan.entries[0].disposition,
            AdvisoryDisposition::AmbiguousInstallationCandidates
        );
        assert_eq!(plan.installation_candidates.len(), 2);
        assert_eq!(plan.entries[0].hypothetical_destinations.len(), 2);
    }

    #[test]
    fn conflicting_exact_identity_cannot_fall_back_to_title_matching() {
        let mut conflicting = game(7);
        conflicting.executable_crc = Some("FFFFFFFF".to_string());
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[conflicting],
        );

        assert_eq!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::NoMatch
        );
        assert!(
            plan.entries[0]
                .game_match
                .reasons
                .iter()
                .any(|reason| reason.contains("conflicts"))
        );
    }

    #[test]
    fn incompatible_catalogue_has_no_match() {
        let mut incompatible = game(7);
        incompatible.platform = Some("PS3".to_string());
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[incompatible],
        );

        assert_eq!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::NoMatch
        );
        assert_eq!(
            plan.entries[0].disposition,
            AdvisoryDisposition::MissingGame
        );
    }

    #[test]
    fn domain_matcher_classifies_synthetic_title_and_filename_evidence_conservatively() {
        let mut probable_game = game(7);
        probable_game.serial = None;
        probable_game.executable_crc = None;
        let probable = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[probable_game],
        );
        assert_eq!(
            probable.entries[0].game_match.confidence,
            MatchConfidence::Probable
        );

        let mut uncertain_game = game(8);
        uncertain_game.display_name = "Unknown SLUS-20312 dump".to_string();
        uncertain_game.normalized_name = "unknown".to_string();
        uncertain_game.region = None;
        uncertain_game.serial = None;
        uncertain_game.executable_crc = None;
        let uncertain = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[uncertain_game],
        );
        assert_eq!(
            uncertain.entries[0].game_match.confidence,
            MatchConfidence::Uncertain
        );
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        let error = fetch_built_in_metadata(&fetcher(br#"{"sha":"x","tree":[}"#)).unwrap_err();
        assert!(matches!(error, PatchManagerError::MalformedMetadata(_)));
    }

    #[test]
    fn oversized_response_is_rejected_before_parsing() {
        let body = vec![b' '; MAX_METADATA_BYTES + 1];
        let error = fetch_built_in_metadata(&fetcher(&body)).unwrap_err();
        assert!(matches!(error, PatchManagerError::ResponseTooLarge { .. }));
    }

    #[test]
    fn redirects_are_rejected() {
        let fetcher = StaticFetcher {
            response: MetadataFetch {
                status: 302,
                content_type: "text/html".to_string(),
                body: Vec::new(),
                verification: VerificationLevel::TransportOnly,
            },
        };
        let error = fetch_built_in_metadata(&fetcher).unwrap_err();
        assert_eq!(error, PatchManagerError::RedirectRejected { status: 302 });
    }

    #[test]
    fn duplicate_identity_fields_are_rejected() {
        let body = br#"{
            "sha":"first",
            "sha":"second",
            "truncated":false,
            "tree":[]
        }"#;
        let error = fetch_built_in_metadata(&fetcher(body)).unwrap_err();
        assert!(matches!(error, PatchManagerError::MalformedMetadata(_)));
    }

    #[test]
    fn unsafe_repository_paths_are_rejected_not_rendered() {
        let body = br#"{
            "sha":"0123456789abcdef0123456789abcdef01234567",
            "truncated":false,
            "tree":[{"path":"patches/../escape.pnach","type":"blob","sha":"2222222222222222222222222222222222222222","size":42}]
        }"#;
        let error = fetch_built_in_metadata(&fetcher(body)).unwrap_err();
        assert!(matches!(error, PatchManagerError::UnsupportedMetadata(_)));
    }

    #[test]
    fn duplicate_patch_records_are_rejected() {
        let body = br#"{
            "sha":"0123456789abcdef0123456789abcdef01234567",
            "truncated":false,
            "tree":[
                {"path":"patches/12345678.pnach","type":"blob","sha":"2222222222222222222222222222222222222222","size":42},
                {"path":"patches/12345678.pnach","type":"blob","sha":"2222222222222222222222222222222222222222","size":42}
            ]
        }"#;
        let error = fetch_built_in_metadata(&fetcher(body)).unwrap_err();
        assert!(matches!(error, PatchManagerError::MalformedMetadata(_)));
    }

    #[test]
    fn official_tree_shape_yields_metadata_without_fetching_patch_content() {
        let body = br#"{
            "sha":"0123456789abcdef0123456789abcdef01234567",
            "truncated":false,
            "tree":[
                {"path":"README.md","type":"blob","sha":"1111111111111111111111111111111111111111","size":10,"url":"https://ignored.invalid/readme"},
                {"path":"patches/SLUS-20312_A1B2C3D4.pnach","type":"blob","sha":"2222222222222222222222222222222222222222","size":42,"url":"https://ignored.invalid/artifact"}
            ]
        }"#;
        let snapshot = fetch_built_in_metadata(&fetcher(body)).unwrap();

        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].serial.as_deref(), Some("SLUS-20312"));
        assert_eq!(
            snapshot.records[0].executable_crc.as_deref(),
            Some("A1B2C3D4")
        );
        assert_eq!(
            snapshot.source.verification,
            VerificationLevel::TransportOnly
        );
    }

    #[test]
    fn excessive_json_depth_is_rejected() {
        let mut body = String::from("{\"sha\":\"x\",\"tree\":");
        body.push_str(&"[".repeat(MAX_JSON_DEPTH));
        body.push_str(&"]".repeat(MAX_JSON_DEPTH));
        body.push('}');
        let error = fetch_built_in_metadata(&fetcher(body.as_bytes())).unwrap_err();
        assert!(matches!(error, PatchManagerError::UnsupportedMetadata(_)));
    }

    #[test]
    fn catalogue_rows_do_not_gain_exact_identity_from_filenames() {
        let archive = PersistedArchive {
            id: 3,
            source_folder_id: 1,
            relative_path: PathBuf::from("SLUS-20312_A1B2C3D4.iso"),
            absolute_path: PathBuf::from("/games/SLUS-20312_A1B2C3D4.iso"),
            archive_kind: "iso".to_string(),
            display_name: "SLUS-20312_A1B2C3D4".to_string(),
            normalized_name: "slus20312a1b2c3d4".to_string(),
            size_bytes: None,
            modified_time_unix_seconds: None,
            platform: Some("PS2".to_string()),
            platform_source: Some("automatic".to_string()),
            last_known_health: "present".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_verified_missing_at: None,
        };
        let evidence = CatalogueGameEvidence::from(&archive);
        assert_eq!(evidence.serial, None);
        assert_eq!(evidence.executable_crc, None);
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[evidence],
        );
        assert_eq!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::Uncertain
        );
    }

    #[test]
    fn preview_does_not_change_fixture_paths_or_catalogue_bytes() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-patch-preview-no-write-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let native_root = home.join(".config/PCSX2");
        fs::create_dir_all(&native_root).unwrap();
        let catalogue_path = root.join("library.sqlite3");
        Database::open_or_create(&catalogue_path)
            .unwrap()
            .close()
            .unwrap();
        let before_database = fs::read(&catalogue_path).unwrap();
        let before_entries = tree_entries(&root);
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home,
                xdg_config_home: native_root.parent().unwrap().to_path_buf(),
            },
        );
        let body = br#"{
            "sha":"0123456789abcdef0123456789abcdef01234567",
            "truncated":false,
            "tree":[{"path":"patches/1234ABCD.pnach","type":"blob","sha":"2222222222222222222222222222222222222222","size":42}]
        }"#;

        let plan = preview_pcsx2_metadata(&fetcher(body), &adapter, &catalogue_path).unwrap();

        assert!(!plan.executable);
        assert_eq!(tree_entries(&root), before_entries);
        assert_eq!(fs::read(&catalogue_path).unwrap(), before_database);
        assert!(!native_root.join("patches").exists());
        assert!(!native_root.join("cheats").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_catalogue_rows_do_not_match_patch_metadata() {
        let mut missing = game(7);
        missing.is_present = false;
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[missing],
        );

        assert_eq!(
            plan.entries[0].game_match.confidence,
            MatchConfidence::NoMatch
        );
        assert!(plan.entries[0].game_match.catalogue_archive_ids.is_empty());
    }

    #[test]
    fn oversized_catalogue_identity_text_is_rejected() {
        let mut evidence = game(7);
        evidence.display_name = "x".repeat(MAX_METADATA_STRING_BYTES + 1);

        assert!(matches!(
            validate_catalogue_evidence(&evidence),
            Err(PatchManagerError::UnsupportedMetadata(_))
        ));
    }

    #[test]
    fn plan_id_ignores_presentation_only_reason_and_display_path_text() {
        let snapshot = snapshot_with(record());
        let source = snapshot.source.clone();
        let candidates = vec![installation()];
        let plan = build_advisory_plan(&test_adapter(), snapshot, candidates.clone(), &[game(7)]);
        let mut presentation_changed = plan.entries.clone();
        presentation_changed[0]
            .reasons
            .push("different presentation".to_string());
        presentation_changed[0].hypothetical_destinations[0].display_path =
            "different display text".to_string();

        assert_eq!(
            plan.plan_id,
            compute_plan_id(&source, &candidates, &presentation_changed)
        );
    }

    #[test]
    fn advisory_patch_plan_json_preserves_the_pre_extraction_kind_string_shape() {
        let plan = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[game(7)],
        );

        let json = serde_json::to_value(&plan).unwrap();
        // Pre-extraction, `installation_candidates[].kind` serialized as a
        // plain JSON string via the derived enum tag (`"Native"`); the
        // post-extraction neutral `String` field must produce the exact
        // same JSON shape and value, not a nested object or a different
        // case.
        assert_eq!(json["installation_candidates"][0]["kind"], "Native");
        assert!(json["installation_candidates"][0]["kind"].is_string());
        assert_eq!(
            json["entries"][0]["hypothetical_destinations"][0]["candidate_kind"],
            "Native"
        );
    }

    #[test]
    fn plan_id_is_deterministic_across_repeated_calls_with_identical_inputs() {
        let first = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[game(7)],
        );
        let second = build_advisory_plan(
            &test_adapter(),
            snapshot_with(record()),
            vec![installation()],
            &[game(7)],
        );

        assert_eq!(first.plan_id, second.plan_id);
        assert!(!first.plan_id.is_empty());
    }

    #[test]
    fn preview_never_migrates_or_changes_the_catalogue_schema_version() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-patch-preview-no-migration-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let native_root = home.join(".config/PCSX2");
        fs::create_dir_all(&native_root).unwrap();
        let catalogue_path = root.join("library.sqlite3");
        Database::open_or_create(&catalogue_path)
            .unwrap()
            .close()
            .unwrap();
        let schema_before = Database::open_read_only(&catalogue_path)
            .unwrap()
            .schema_version()
            .unwrap();
        assert_eq!(schema_before, crate::latest_schema_version());

        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home,
                xdg_config_home: native_root.parent().unwrap().to_path_buf(),
            },
        );
        let body = br#"{
            "sha":"0123456789abcdef0123456789abcdef01234567",
            "truncated":false,
            "tree":[]
        }"#;
        let _ = preview_pcsx2_metadata(&fetcher(body), &adapter, &catalogue_path).unwrap();

        let schema_after = Database::open_read_only(&catalogue_path)
            .unwrap()
            .schema_version()
            .unwrap();
        assert_eq!(schema_after, schema_before);
        let _ = fs::remove_dir_all(root);
    }

    // ---- Fixed pre-extraction plan-ID regression fixture ----
    //
    // These exact values, and the three `GOLDEN_*_PLAN_ID` hashes below,
    // were produced by literally re-running the pre-extraction
    // `compute_plan_id`/`build_advisory_plan`/`candidate_kind_tag`
    // implementation from commit 52d6ef5 (copied verbatim into a
    // standalone throwaway harness, never committed) against this exact
    // fixed scenario. They are not merely "the same value the current
    // code produces twice" - they anchor the current code's plan IDs to
    // what history actually produced, so a change to field order, hashed
    // bytes, or candidate-kind casing would be caught even if it happened
    // to be internally self-consistent.

    fn golden_source() -> SourceSnapshot {
        SourceSnapshot {
            id: "golden-source-id",
            display_name: "Golden Source",
            endpoint: "https://golden.invalid/endpoint",
            provenance: "golden provenance",
            license_notice: "golden license",
            metadata_schema: "golden-schema-v1",
            source_version: "golden-version".to_string(),
            metadata_sha256: "golden-metadata-hash".to_string(),
            verification: VerificationLevel::TransportOnly,
            verification_explanation: "golden verification explanation",
            freshness_explanation: "golden freshness explanation",
        }
    }

    fn golden_record() -> PatchMetadataRecord {
        PatchMetadataRecord {
            record_id: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
            repository_path: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
            patch_blob_id: "golden-blob-id".to_string(),
            title: None,
            platform: "PS2".to_string(),
            region: None,
            serial: Some("GOLD-00001".to_string()),
            executable_crc: Some("DEADBEEF".to_string()),
            metadata_kind: "golden fixture".to_string(),
        }
    }

    fn golden_native() -> InstallationCandidate {
        InstallationCandidate {
            adapter_id: "pcsx2",
            kind: "Native".to_string(),
            data_root: PathBuf::from("/golden/home/.config/PCSX2"),
            provenance: "golden native provenance",
            discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
            detected_version: None,
            mutation_readiness: "NotEvaluated",
        }
    }

    fn golden_flatpak() -> InstallationCandidate {
        InstallationCandidate {
            adapter_id: "pcsx2",
            kind: "Flatpak".to_string(),
            data_root: PathBuf::from("/golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2"),
            provenance: "golden flatpak provenance",
            discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
            detected_version: None,
            mutation_readiness: "NotEvaluated",
        }
    }

    fn golden_snapshot() -> MetadataSnapshot {
        MetadataSnapshot {
            source: golden_source(),
            records: vec![golden_record()],
        }
    }

    const GOLDEN_NATIVE_ONLY_PLAN_ID: &str =
        "1cf5adde6763fe1a7f1573d9b02c7de184728a43a4103ce11c8aa41ac4dadbf0";
    const GOLDEN_FLATPAK_ONLY_PLAN_ID: &str =
        "1fa5bfc287c3e870aa162ee3d5bdf762e694ad54f8f14a28e6ff233159750bb4";
    const GOLDEN_NATIVE_PLUS_FLATPAK_PLAN_ID: &str =
        "09ef31b00d2b1e65ff86dd17e8a39d8498cf5796f16dccd6e1a2938b44f99af1";

    #[test]
    fn plan_id_matches_the_pre_extraction_hash_for_a_native_candidate() {
        let plan = build_advisory_plan(
            &test_adapter(),
            golden_snapshot(),
            vec![golden_native()],
            &[],
        );
        assert_eq!(plan.plan_id, GOLDEN_NATIVE_ONLY_PLAN_ID);
    }

    #[test]
    fn plan_id_matches_the_pre_extraction_hash_for_a_flatpak_candidate() {
        let plan = build_advisory_plan(
            &test_adapter(),
            golden_snapshot(),
            vec![golden_flatpak()],
            &[],
        );
        assert_eq!(plan.plan_id, GOLDEN_FLATPAK_ONLY_PLAN_ID);
    }

    #[test]
    fn plan_id_matches_the_pre_extraction_hash_for_native_plus_flatpak_candidates() {
        let plan = build_advisory_plan(
            &test_adapter(),
            golden_snapshot(),
            vec![golden_native(), golden_flatpak()],
            &[],
        );
        assert_eq!(plan.plan_id, GOLDEN_NATIVE_PLUS_FLATPAK_PLAN_ID);
    }

    #[test]
    fn installation_candidate_json_has_exactly_the_pre_extraction_key_set() {
        let plan = build_advisory_plan(
            &test_adapter(),
            golden_snapshot(),
            vec![golden_native()],
            &[],
        );
        let json = serde_json::to_value(&plan).unwrap();

        let mut candidate_keys: Vec<String> = json["installation_candidates"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        candidate_keys.sort();
        assert_eq!(
            candidate_keys,
            vec![
                "data_root",
                "detected_version",
                "discovery_confidence",
                "kind",
                "mutation_readiness",
                "provenance",
            ],
            "installation_candidates[] must serialize exactly the pre-extraction \
             Pcsx2InstallationCandidate field set - no adapter_id or other new key"
        );

        let mut destination_keys: Vec<String> = json["entries"][0]["hypothetical_destinations"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        destination_keys.sort();
        assert_eq!(
            destination_keys,
            vec![
                "candidate_kind",
                "display_path",
                "hypothetical",
                "relative_path"
            ],
            "hypothetical_destinations[] must serialize exactly the pre-extraction \
             HypotheticalPnachDestination field set"
        );
    }

    #[test]
    fn build_advisory_plan_preserves_whatever_candidate_order_it_is_given() {
        // `build_advisory_plan` itself never sorts its
        // `installation_candidates` argument - ordering is entirely the
        // caller's responsibility. In production that caller is
        // `ReadOnlyPcsx2Adapter::discover_installations`, proven to yield
        // Native before Flatpak by
        // `discover_installations_yields_native_before_flatpak_when_both_exist`
        // in `pcsx2.rs`. This test only proves the plan is a faithful,
        // order-preserving projection of whatever it was given - passing
        // Flatpak first here deliberately, to prove this function is not
        // silently re-sorting behind the scenes.
        let plan = build_advisory_plan(
            &test_adapter(),
            golden_snapshot(),
            vec![golden_flatpak(), golden_native()],
            &[],
        );

        assert_eq!(plan.installation_candidates[0].kind, "Flatpak");
        assert_eq!(plan.installation_candidates[1].kind, "Native");
        assert_eq!(
            plan.entries[0].hypothetical_destinations[0].candidate_kind,
            "Flatpak"
        );
        assert_eq!(
            plan.entries[0].hypothetical_destinations[1].candidate_kind,
            "Native"
        );
    }

    fn tree_entries(root: &Path) -> Vec<PathBuf> {
        fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
            let mut children = fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                entries.push(child.strip_prefix(root).unwrap().to_path_buf());
                if child.is_dir() {
                    visit(root, &child, entries);
                }
            }
        }

        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries
    }
}
