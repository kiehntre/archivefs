//! Read-mostly lifecycle management for immutable RetroArch cheat snapshots.
//!
//! Inventory and verification never write. Pins live beside, never inside,
//! snapshots. Pruning is split into a pure plan and an explicitly confirmed
//! execution which revalidates every candidate immediately before deletion.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::emulator_environment::EncodedPath;

use super::cheat_cache_lock::LockedCheatCache;
#[cfg(test)]
use super::cheat_sources::CHEAT_SOURCE_RESULT_SCHEMA_VERSION;
use super::cheat_sources::{
    CheatSourceCacheMetadata, CheatSourceError, CheatSourceFreshness, CheatSourceManifest,
    MANIFESTS_DIRECTORY, METADATA_FILE, SNAPSHOTS_DIRECTORY, STAGING_DIRECTORY, atomic_write_json,
    cache_error, collect_catalogue_manifest, manifest_freshness, now_seconds,
    safe_regular_or_directory, supported_cheat_source_schema, validate_cache_path_for_read,
    validate_catalogue_prefix, validate_snapshot_name,
};

pub const CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION: u32 = 1;
const PINS_FILE: &str = "pins.json";
pub const DEFAULT_ABANDONED_STAGING_MIN_AGE_SECONDS: u64 = 24 * 60 * 60;
pub const MINIMUM_ABANDONED_STAGING_AGE_SECONDS: u64 = 60 * 60;
const MAINTENANCE_TREE_ENTRY_LIMIT: usize = 60_000;
const MAINTENANCE_TREE_BYTES_LIMIT: u64 = 512 * 1024 * 1024;
const MAINTENANCE_INVENTORY_ENTRY_LIMIT: usize = 60_000;
// Retrieval permits up to 60,000 paths of up to 1,024 bytes. These limits
// bound hostile JSON while remaining above the largest valid retrieval output.
const MAINTENANCE_MANIFEST_BYTES_LIMIT: u64 = 128 * 1024 * 1024;
const MAINTENANCE_SOURCE_METADATA_BYTES_LIMIT: u64 = 256 * 1024 * 1024;
const MAINTENANCE_PIN_METADATA_BYTES_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotVerificationState {
    Valid,
    InvalidManifest,
    IdentityMismatch,
    MissingFile,
    ChangedFile,
    UnexpectedFile,
    SizeMismatch,
    DigestMismatch,
    UnsupportedSchema,
    UnsafePath,
    IncompleteStagingArtifact,
    UnreadablePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotVerificationFinding {
    pub state: SnapshotVerificationState,
    pub path: Option<EncodedPath>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotInventoryEntry {
    pub source_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub archive_sha256: Option<String>,
    pub source_url: Option<String>,
    pub retrieved_at_unix_seconds: Option<u64>,
    pub archive_size: Option<u64>,
    pub extracted_size: Option<u64>,
    pub entry_count: Option<usize>,
    pub cache_path: EncodedPath,
    pub manifest_version: Option<u32>,
    pub verification_state: SnapshotVerificationState,
    pub verification_findings: Vec<SnapshotVerificationFinding>,
    pub freshness: CheatSourceFreshness,
    pub current: bool,
    pub last_known_good: bool,
    pub pinned: bool,
    pub pin_metadata_valid: bool,
    pub source_metadata_valid: bool,
    pub last_successful_use_unix_seconds: Option<u64>,
    pub warnings: Vec<String>,
}

impl SnapshotInventoryEntry {
    fn valid(&self) -> bool {
        self.verification_state == SnapshotVerificationState::Valid
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotInventoryReport {
    pub schema_version: u32,
    pub cache_root: EncodedPath,
    pub entries: Vec<SnapshotInventoryEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotVerificationReport {
    pub schema_version: u32,
    pub cache_root: EncodedPath,
    pub entries: Vec<SnapshotInventoryEntry>,
    pub valid_count: usize,
    pub invalid_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SnapshotPins {
    format_version: u32,
    source_id: String,
    pinned_snapshots: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotPinsDocument {
    format_version: u32,
    source_id: String,
    pinned_snapshots: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotPinStatus {
    Pinned,
    AlreadyPinned,
    Unpinned,
    AlreadyUnpinned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotPinResult {
    pub schema_version: u32,
    pub status: SnapshotPinStatus,
    pub source_id: String,
    pub snapshot_id: String,
    pub snapshot_path: EncodedPath,
    pub pins_path: EncodedPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePrunePolicy {
    pub keep_newest_per_source: Option<usize>,
    pub retain_newer_than_seconds: Option<u64>,
    pub max_cache_bytes: Option<u64>,
    pub source_filter: Option<String>,
    pub include_abandoned_staging: bool,
    pub abandoned_staging_min_age_seconds: u64,
}

impl Default for CachePrunePolicy {
    fn default() -> Self {
        Self {
            keep_newest_per_source: None,
            retain_newer_than_seconds: None,
            max_cache_bytes: None,
            source_filter: None,
            include_abandoned_staging: false,
            abandoned_staging_min_age_seconds: DEFAULT_ABANDONED_STAGING_MIN_AGE_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneReason {
    OlderThanRetention,
    ExceedsKeepCount,
    ExceedsCacheBudget,
    InvalidSnapshot,
    IncompleteStagingEntry,
    Pinned,
    Current,
    LastKnownGood,
    WithinRetention,
    RequiredKeepCount,
    VerificationRequired,
    UnsafeOrAmbiguousPath,
    RecentOrActiveStaging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneDisposition {
    Candidate,
    Protected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneEntryKind {
    Snapshot,
    Staging,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePrunePlanEntry {
    pub kind: CachePruneEntryKind,
    pub disposition: CachePruneDisposition,
    pub source_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub path: EncodedPath,
    pub bytes: u64,
    pub retrieved_at_unix_seconds: Option<u64>,
    pub reasons: Vec<CachePruneReason>,
    pub identity_token: Option<String>,
    #[serde(skip)]
    resolved_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePrunePlan {
    pub schema_version: u32,
    pub cache_root: EncodedPath,
    pub policy: CachePrunePolicy,
    pub entries: Vec<CachePrunePlanEntry>,
    pub candidate_count: usize,
    pub protected_count: usize,
    pub candidate_bytes: u64,
    #[serde(skip)]
    resolved_cache_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneExecutionStatus {
    Preview,
    Completed,
    PartialFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePruneEntryStatus {
    Deleted,
    Skipped,
    Changed,
    Unsafe,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneExecutionEntry {
    pub kind: CachePruneEntryKind,
    pub path: EncodedPath,
    pub status: CachePruneEntryStatus,
    pub bytes_reclaimed: u64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachePruneExecutionResult {
    pub schema_version: u32,
    pub status: CachePruneExecutionStatus,
    pub confirmed: bool,
    pub entries: Vec<CachePruneExecutionEntry>,
    pub bytes_reclaimed: u64,
    pub snapshots_deleted: usize,
    pub staging_entries_removed: usize,
}

pub fn inventory_retroarch_cheat_snapshots(
    cache_root: &Path,
) -> Result<SnapshotInventoryReport, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    inventory_retroarch_cheat_snapshots_locked(&locked)
}

fn inventory_retroarch_cheat_snapshots_locked(
    locked: &LockedCheatCache,
) -> Result<SnapshotInventoryReport, CheatSourceError> {
    let cache_root = locked.root();
    if !locked.present_at_acquisition() {
        return Ok(SnapshotInventoryReport {
            schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
            cache_root: EncodedPath::from_path(cache_root),
            entries: Vec::new(),
            warnings: Vec::new(),
        });
    }
    safe_regular_or_directory(cache_root, true)?;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut examined_entries = 0usize;
    let source_entries = fs::read_dir(cache_root)
        .map_err(|error| cache_error("cache_inventory_read_failed", error))?;
    for source_entry in source_entries {
        examined_entries = examined_entries.saturating_add(1);
        enforce_inventory_entry_limit(examined_entries)?;
        let source_entry = match source_entry {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!("unreadable cache entry: {error}"));
                continue;
            }
        };
        let source_path = source_entry.path();
        let source_metadata = match fs::symlink_metadata(&source_path) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!(
                    "unreadable cache entry {}: {error}",
                    source_path.display()
                ));
                continue;
            }
        };
        if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
            warnings.push(format!(
                "unsafe non-directory source entry: {}",
                source_path.display()
            ));
            continue;
        }
        let Some(source_id) = source_path.file_name().and_then(|value| value.to_str()) else {
            warnings.push(format!(
                "non-UTF-8 source directory was not trusted: {}",
                source_path.display()
            ));
            continue;
        };
        let (current, source_metadata_valid) =
            read_current_snapshot(&source_path, source_id, &mut warnings);
        let pins = load_pins(&source_path, source_id);
        let (pinned, mut pin_metadata_valid) = match pins {
            Ok(value) => (value.pinned_snapshots, true),
            Err(error) => {
                warnings.push(format!("pin metadata for {source_id} is invalid: {error}"));
                (BTreeSet::new(), false)
            }
        };
        let snapshots_root = source_path.join(SNAPSHOTS_DIRECTORY);
        if !snapshots_root.exists() {
            if !pinned.is_empty() {
                warnings.push(format!(
                    "pin metadata for {source_id} references snapshots, but the snapshot directory is missing"
                ));
            }
            continue;
        }
        if let Err(error) = safe_regular_or_directory(&snapshots_root, true) {
            warnings.push(error.to_string());
            continue;
        }
        let snapshot_entries = match fs::read_dir(&snapshots_root) {
            Ok(value) => value,
            Err(error) => {
                warnings.push(format!(
                    "cannot enumerate {}: {error}",
                    snapshots_root.display()
                ));
                continue;
            }
        };
        let mut snapshot_paths = Vec::new();
        for snapshot_entry in snapshot_entries {
            examined_entries = examined_entries.saturating_add(1);
            enforce_inventory_entry_limit(examined_entries)?;
            match snapshot_entry {
                Ok(value) => snapshot_paths.push(value.path()),
                Err(error) => {
                    warnings.push(format!("unreadable snapshot entry: {error}"));
                }
            }
        }
        let known_snapshots = snapshot_paths
            .iter()
            .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
            .collect::<BTreeSet<_>>();
        if pinned
            .iter()
            .any(|snapshot| !known_snapshots.contains(snapshot.as_str()))
        {
            pin_metadata_valid = false;
            warnings.push(format!(
                "pin metadata for {source_id} references an unknown snapshot"
            ));
        }
        for path in snapshot_paths {
            let snapshot_id = path
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string);
            let mut entry =
                inspect_inventory_entry(cache_root, source_id, snapshot_id.as_deref(), &path);
            entry.current = snapshot_id
                .as_ref()
                .is_some_and(|id| current.as_ref() == Some(id));
            entry.last_known_good = entry.current;
            entry.pinned = snapshot_id.as_ref().is_some_and(|id| pinned.contains(id));
            entry.pin_metadata_valid = pin_metadata_valid;
            entry.source_metadata_valid = source_metadata_valid;
            if !pin_metadata_valid {
                entry
                    .warnings
                    .push("pin metadata is invalid; pruning must protect this snapshot".into());
            }
            if !source_metadata_valid {
                entry.warnings.push(
                    "source metadata is missing or invalid; current and last-known-good state is unknown"
                        .into(),
                );
            }
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| {
                right
                    .retrieved_at_unix_seconds
                    .cmp(&left.retrieved_at_unix_seconds)
            })
            .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
            .then_with(|| left.cache_path.display.cmp(&right.cache_path.display))
    });
    warnings.sort();
    warnings.dedup();
    Ok(SnapshotInventoryReport {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        cache_root: EncodedPath::from_path(cache_root),
        entries,
        warnings,
    })
}

fn inspect_inventory_entry(
    cache_root: &Path,
    source_id: &str,
    snapshot_id: Option<&str>,
    snapshot_path: &Path,
) -> SnapshotInventoryEntry {
    let mut findings = Vec::new();
    let mut manifest = None;
    if validate_snapshot_name(source_id).is_err() {
        finding(
            &mut findings,
            SnapshotVerificationState::UnsafePath,
            Some(snapshot_path),
            "source identity is not a safe cache component",
        );
    } else if snapshot_id.is_none() || !is_sha256(snapshot_id.unwrap_or_default()) {
        finding(
            &mut findings,
            SnapshotVerificationState::UnsafePath,
            Some(snapshot_path),
            "snapshot identity is not a safe UTF-8 component",
        );
    } else if let Err(error) = safe_regular_or_directory(snapshot_path, true) {
        finding(
            &mut findings,
            SnapshotVerificationState::UnsafePath,
            Some(snapshot_path),
            error.to_string(),
        );
    } else {
        let id = snapshot_id.unwrap_or_default();
        let manifest_path = cache_root
            .join(source_id)
            .join(MANIFESTS_DIRECTORY)
            .join(format!("{id}.json"));
        match read_manifest(&manifest_path) {
            Ok(value) => {
                if !supported_cheat_source_schema(value.format_version) {
                    finding(
                        &mut findings,
                        SnapshotVerificationState::UnsupportedSchema,
                        Some(&manifest_path),
                        format!("unsupported manifest schema {}", value.format_version),
                    );
                } else if value.source_id != source_id
                    || value.archive_sha256 != id
                    || value.cache_relative_path != format!("{SNAPSHOTS_DIRECTORY}/{id}")
                {
                    finding(
                        &mut findings,
                        SnapshotVerificationState::IdentityMismatch,
                        Some(&manifest_path),
                        "manifest identity does not bind to the source and snapshot directory",
                    );
                } else if let Err(error) = validate_catalogue_prefix(&value.catalogue_relative_path)
                {
                    finding(
                        &mut findings,
                        SnapshotVerificationState::UnsafePath,
                        Some(&manifest_path),
                        error.to_string(),
                    );
                } else {
                    verify_manifest_files(snapshot_path, &value, &mut findings);
                }
                manifest = Some(value);
            }
            Err((state, message)) => finding(&mut findings, state, Some(&manifest_path), message),
        }
    }
    let state = findings
        .first()
        .map_or(SnapshotVerificationState::Valid, |value| value.state);
    let freshness = manifest
        .as_ref()
        .map_or(CheatSourceFreshness::Unknown, manifest_freshness);
    SnapshotInventoryEntry {
        source_id: Some(source_id.to_string()),
        snapshot_id: snapshot_id.map(str::to_string),
        archive_sha256: manifest.as_ref().map(|value| value.archive_sha256.clone()),
        source_url: manifest.as_ref().map(|value| value.source_url.clone()),
        retrieved_at_unix_seconds: manifest.as_ref().map(|value| value.fetched_at_unix_seconds),
        archive_size: manifest.as_ref().map(|value| value.downloaded_bytes),
        extracted_size: manifest.as_ref().map(|value| value.extracted_bytes),
        entry_count: manifest.as_ref().map(|value| value.archive_entry_count),
        cache_path: EncodedPath::from_path(snapshot_path),
        manifest_version: manifest.as_ref().map(|value| value.format_version),
        verification_state: state,
        verification_findings: findings,
        freshness,
        current: false,
        last_known_good: false,
        pinned: false,
        pin_metadata_valid: true,
        source_metadata_valid: true,
        last_successful_use_unix_seconds: None,
        warnings: manifest.map_or_else(Vec::new, |value| value.warnings),
    }
}

fn read_manifest(path: &Path) -> Result<CheatSourceManifest, (SnapshotVerificationState, String)> {
    if let Err(error) = validate_cache_path_for_read(path) {
        return Err((SnapshotVerificationState::UnsafePath, error.to_string()));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        let state = if error.kind() == std::io::ErrorKind::NotFound {
            SnapshotVerificationState::InvalidManifest
        } else {
            SnapshotVerificationState::UnreadablePath
        };
        (state, format!("manifest is unavailable: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err((
            SnapshotVerificationState::UnsafePath,
            "manifest is a symlink or non-file".into(),
        ));
    }
    let bytes = read_bounded_file(path, MAINTENANCE_MANIFEST_BYTES_LIMIT).map_err(|error| {
        let state = if error.code == "maintenance_metadata_size_limit" {
            SnapshotVerificationState::InvalidManifest
        } else {
            SnapshotVerificationState::UnreadablePath
        };
        (state, error.to_string())
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        (
            SnapshotVerificationState::InvalidManifest,
            error.to_string(),
        )
    })
}

fn verify_manifest_files(
    snapshot_path: &Path,
    manifest: &CheatSourceManifest,
    findings: &mut Vec<SnapshotVerificationFinding>,
) {
    let catalogue = snapshot_path.join(&manifest.catalogue_relative_path);
    if let Err(error) = safe_regular_or_directory(&catalogue, true) {
        let state = if catalogue.exists() {
            SnapshotVerificationState::UnsafePath
        } else {
            SnapshotVerificationState::MissingFile
        };
        finding(findings, state, Some(&catalogue), error.to_string());
        return;
    }
    let actual = match collect_catalogue_manifest(&catalogue) {
        Ok(value) => value,
        Err(error) => {
            let state = if error.code.contains("symlink")
                || error.code.contains("escape")
                || error.code.contains("special")
            {
                SnapshotVerificationState::UnsafePath
            } else {
                SnapshotVerificationState::UnreadablePath
            };
            finding(findings, state, Some(&catalogue), error.to_string());
            return;
        }
    };
    let mut expected = BTreeMap::new();
    for file in &manifest.files {
        let relative = Path::new(&file.relative_path);
        if file.relative_path.is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            finding(
                findings,
                SnapshotVerificationState::InvalidManifest,
                None,
                format!("manifest file path is unsafe: {}", file.relative_path),
            );
            continue;
        }
        if expected.insert(&file.relative_path, file).is_some() {
            finding(
                findings,
                SnapshotVerificationState::InvalidManifest,
                None,
                format!(
                    "manifest contains a duplicate file path: {}",
                    file.relative_path
                ),
            );
        }
    }
    let found = actual
        .iter()
        .map(|file| (&file.relative_path, file))
        .collect::<BTreeMap<_, _>>();
    for (path, expected_file) in &expected {
        let full = catalogue.join(path);
        match found.get(path) {
            None => finding(
                findings,
                SnapshotVerificationState::MissingFile,
                Some(&full),
                "file recorded by the manifest is missing",
            ),
            Some(actual_file) if actual_file.size != expected_file.size => {
                finding(
                    findings,
                    SnapshotVerificationState::SizeMismatch,
                    Some(&full),
                    format!(
                        "expected {} bytes, found {}",
                        expected_file.size, actual_file.size
                    ),
                );
                finding(
                    findings,
                    SnapshotVerificationState::ChangedFile,
                    Some(&full),
                    "cached file changed after publication",
                );
            }
            Some(actual_file) if actual_file.sha256 != expected_file.sha256 => {
                finding(
                    findings,
                    SnapshotVerificationState::DigestMismatch,
                    Some(&full),
                    "cached file SHA-256 differs from the manifest",
                );
                finding(
                    findings,
                    SnapshotVerificationState::ChangedFile,
                    Some(&full),
                    "cached file changed after publication",
                );
            }
            Some(_) => {}
        }
    }
    for path in found.keys() {
        if !expected.contains_key(path) {
            finding(
                findings,
                SnapshotVerificationState::UnexpectedFile,
                Some(&catalogue.join(path)),
                "file is not recorded by the immutable manifest",
            );
        }
    }
}

fn finding(
    findings: &mut Vec<SnapshotVerificationFinding>,
    state: SnapshotVerificationState,
    path: Option<&Path>,
    message: impl Into<String>,
) {
    findings.push(SnapshotVerificationFinding {
        state,
        path: path.map(EncodedPath::from_path),
        message: message.into(),
    });
}

fn read_current_snapshot(
    source_root: &Path,
    source_id: &str,
    warnings: &mut Vec<String>,
) -> (Option<String>, bool) {
    let path = source_root.join(METADATA_FILE);
    if !path.exists() {
        return (None, false);
    }
    if validate_cache_path_for_read(&path).is_err() {
        warnings.push(format!("unsafe source metadata: {}", path.display()));
        return (None, false);
    }
    let metadata = read_bounded_file(&path, MAINTENANCE_SOURCE_METADATA_BYTES_LIMIT)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CheatSourceCacheMetadata>(&bytes).ok());
    match metadata {
        Some(value)
            if supported_cheat_source_schema(value.format_version)
                && value.source_id == source_id
                && metadata_current_binding_valid(&value, source_id) =>
        {
            (value.current_snapshot, true)
        }
        _ => {
            warnings.push(format!("invalid source metadata: {}", path.display()));
            (None, false)
        }
    }
}

fn metadata_current_binding_valid(metadata: &CheatSourceCacheMetadata, source_id: &str) -> bool {
    match (&metadata.current_snapshot, &metadata.manifest) {
        (None, None) => true,
        (Some(snapshot), Some(manifest)) => {
            validate_snapshot_name(snapshot).is_ok()
                && supported_cheat_source_schema(manifest.format_version)
                && manifest.source_id == source_id
                && manifest.archive_sha256 == *snapshot
                && manifest.cache_relative_path == format!("{SNAPSHOTS_DIRECTORY}/{snapshot}")
        }
        _ => false,
    }
}

fn pins_path(source_root: &Path) -> PathBuf {
    source_root.join(PINS_FILE)
}

fn load_pins(source_root: &Path, source_id: &str) -> Result<SnapshotPins, CheatSourceError> {
    let path = pins_path(source_root);
    validate_cache_path_for_read(&path)?;
    if !path.exists() {
        return Ok(SnapshotPins {
            format_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
            source_id: source_id.to_string(),
            pinned_snapshots: BTreeSet::new(),
        });
    }
    safe_regular_or_directory(&path, false)?;
    let bytes = read_bounded_file(&path, MAINTENANCE_PIN_METADATA_BYTES_LIMIT)?;
    let document: SnapshotPinsDocument = serde_json::from_slice(&bytes)
        .map_err(|error| cache_error("pin_metadata_invalid", error))?;
    if document.format_version != CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION
        || document.source_id != source_id
    {
        return Err(cache_error(
            "pin_metadata_binding_invalid",
            "pin metadata schema or source binding is invalid",
        ));
    }
    let pin_count = document.pinned_snapshots.len();
    let pinned_snapshots = document
        .pinned_snapshots
        .into_iter()
        .collect::<BTreeSet<_>>();
    if pinned_snapshots.len() != pin_count {
        return Err(cache_error(
            "pin_metadata_duplicate",
            "pin metadata contains duplicate snapshot identities",
        ));
    }
    for snapshot in &pinned_snapshots {
        validate_snapshot_name(snapshot)?;
    }
    Ok(SnapshotPins {
        format_version: document.format_version,
        source_id: document.source_id,
        pinned_snapshots,
    })
}

pub fn verify_retroarch_cheat_snapshots(
    cache_root: &Path,
    snapshot_id: Option<&str>,
    source_id: Option<&str>,
) -> Result<SnapshotVerificationReport, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    verify_retroarch_cheat_snapshots_locked(&locked, snapshot_id, source_id)
}

fn verify_retroarch_cheat_snapshots_locked(
    locked: &LockedCheatCache,
    snapshot_id: Option<&str>,
    source_id: Option<&str>,
) -> Result<SnapshotVerificationReport, CheatSourceError> {
    let cache_root = locked.root();
    let inventory = inventory_retroarch_cheat_snapshots_locked(locked)?;
    let mut entries = if let Some(id) = snapshot_id {
        vec![resolve_snapshot(&inventory.entries, id)?.clone()]
    } else {
        inventory
            .entries
            .into_iter()
            .filter(|entry| {
                source_id.is_none_or(|source| entry.source_id.as_deref() == Some(source))
            })
            .collect::<Vec<_>>()
    };
    if snapshot_id.is_none() {
        if locked.present_at_acquisition() {
            entries.extend(staging_verification_entries(cache_root, source_id)?);
        }
        entries.sort_by(|left, right| {
            left.source_id
                .cmp(&right.source_id)
                .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
                .then_with(|| left.cache_path.display.cmp(&right.cache_path.display))
        });
    }
    if source_id.is_some() && entries.is_empty() {
        return Err(cache_error(
            "source_snapshots_not_found",
            "no cached snapshots match the source ID",
        ));
    }
    let valid_count = entries.iter().filter(|entry| entry.valid()).count();
    let invalid_count = entries.len() - valid_count;
    Ok(SnapshotVerificationReport {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        cache_root: EncodedPath::from_path(cache_root),
        entries,
        valid_count,
        invalid_count,
    })
}

fn staging_verification_entries(
    cache_root: &Path,
    source_filter: Option<&str>,
) -> Result<Vec<SnapshotInventoryEntry>, CheatSourceError> {
    let mut entries = Vec::new();
    let mut examined_entries = 0usize;
    if !cache_root.exists() {
        return Ok(entries);
    }
    for source in fs::read_dir(cache_root)
        .map_err(|error| cache_error("cache_inventory_read_failed", error))?
    {
        examined_entries = examined_entries.saturating_add(1);
        enforce_inventory_entry_limit(examined_entries)?;
        let source = source.map_err(|error| cache_error("cache_inventory_read_failed", error))?;
        let source_id = source.file_name().to_str().map(str::to_string);
        if source_filter.is_some_and(|filter| source_id.as_deref() != Some(filter)) {
            continue;
        }
        let staging = source.path().join(STAGING_DIRECTORY);
        if !staging.exists() {
            continue;
        }
        if let Err(error) = safe_regular_or_directory(&staging, true) {
            entries.push(staging_inventory_entry(
                source_id.clone(),
                &staging,
                SnapshotVerificationState::UnsafePath,
                error.to_string(),
            ));
            continue;
        }
        let items = match fs::read_dir(&staging) {
            Ok(items) => items,
            Err(error) => {
                entries.push(staging_inventory_entry(
                    source_id.clone(),
                    &staging,
                    SnapshotVerificationState::UnreadablePath,
                    error.to_string(),
                ));
                continue;
            }
        };
        for item in items {
            examined_entries = examined_entries.saturating_add(1);
            enforce_inventory_entry_limit(examined_entries)?;
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    entries.push(staging_inventory_entry(
                        source_id.clone(),
                        &staging,
                        SnapshotVerificationState::UnreadablePath,
                        error.to_string(),
                    ));
                    break;
                }
            };
            let path = item.path();
            let source_safe = source_id
                .as_deref()
                .is_some_and(|source| validate_snapshot_name(source).is_ok());
            let safe = source_safe
                && fs::symlink_metadata(&path).is_ok_and(|metadata| {
                    !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir())
                });
            let state = if safe {
                SnapshotVerificationState::IncompleteStagingArtifact
            } else {
                SnapshotVerificationState::UnsafePath
            };
            entries.push(staging_inventory_entry(
                source_id.clone(),
                &path,
                state,
                if safe {
                    "unpublished staging artifact is incomplete and is not a snapshot"
                } else {
                    "staging artifact is a symlink or unreadable path"
                },
            ));
        }
    }
    Ok(entries)
}

fn staging_inventory_entry(
    source_id: Option<String>,
    path: &Path,
    state: SnapshotVerificationState,
    message: impl Into<String>,
) -> SnapshotInventoryEntry {
    SnapshotInventoryEntry {
        source_id,
        snapshot_id: None,
        archive_sha256: None,
        source_url: None,
        retrieved_at_unix_seconds: None,
        archive_size: None,
        extracted_size: safe_tree_bytes(path).ok(),
        entry_count: None,
        cache_path: EncodedPath::from_path(path),
        manifest_version: None,
        verification_state: state,
        verification_findings: vec![SnapshotVerificationFinding {
            state,
            path: Some(EncodedPath::from_path(path)),
            message: message.into(),
        }],
        freshness: CheatSourceFreshness::Unknown,
        current: false,
        last_known_good: false,
        pinned: false,
        pin_metadata_valid: true,
        source_metadata_valid: false,
        last_successful_use_unix_seconds: None,
        warnings: Vec::new(),
    }
}

fn resolve_snapshot<'a>(
    entries: &'a [SnapshotInventoryEntry],
    identifier: &str,
) -> Result<&'a SnapshotInventoryEntry, CheatSourceError> {
    if identifier.len() < 8 || !identifier.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(cache_error(
            "snapshot_id_invalid",
            "snapshot ID must be at least eight hexadecimal characters",
        ));
    }
    let matches = entries
        .iter()
        .filter(|entry| {
            entry
                .snapshot_id
                .as_deref()
                .is_some_and(|id| id.starts_with(identifier))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(cache_error(
            "snapshot_not_found",
            "no cached snapshot matches the identifier",
        )),
        [entry] => Ok(entry),
        _ => Err(cache_error(
            "snapshot_id_ambiguous",
            "snapshot identifier matches more than one cached snapshot",
        )),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn set_retroarch_cheat_snapshot_pin(
    cache_root: &Path,
    snapshot_id: &str,
    pinned: bool,
) -> Result<SnapshotPinResult, CheatSourceError> {
    let locked = LockedCheatCache::acquire_required(cache_root)?;
    set_retroarch_cheat_snapshot_pin_locked(&locked, snapshot_id, pinned)
}

fn set_retroarch_cheat_snapshot_pin_locked(
    locked: &LockedCheatCache,
    snapshot_id: &str,
    pinned: bool,
) -> Result<SnapshotPinResult, CheatSourceError> {
    let cache_root = locked.root();
    let inventory = inventory_retroarch_cheat_snapshots_locked(locked)?;
    let entry = resolve_snapshot(&inventory.entries, snapshot_id)?;
    if !entry.valid() {
        return Err(cache_error(
            "snapshot_not_verifiable",
            "only a manifest-bound valid snapshot can be pinned",
        ));
    }
    let source_id = entry
        .source_id
        .clone()
        .ok_or_else(|| cache_error("snapshot_source_missing", "snapshot has no safe source ID"))?;
    let identity = entry
        .snapshot_id
        .clone()
        .ok_or_else(|| cache_error("snapshot_identity_missing", "snapshot has no safe identity"))?;
    let source_root = cache_root.join(&source_id);
    let mut pins = load_pins(&source_root, &source_id)?;
    let changed = if pinned {
        pins.pinned_snapshots.insert(identity.clone())
    } else {
        pins.pinned_snapshots.remove(&identity)
    };
    if changed {
        atomic_write_json(&pins_path(&source_root), &pins)?;
    }
    Ok(SnapshotPinResult {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        status: match (pinned, changed) {
            (true, true) => SnapshotPinStatus::Pinned,
            (true, false) => SnapshotPinStatus::AlreadyPinned,
            (false, true) => SnapshotPinStatus::Unpinned,
            (false, false) => SnapshotPinStatus::AlreadyUnpinned,
        },
        source_id,
        snapshot_id: identity,
        snapshot_path: entry.cache_path.clone(),
        pins_path: EncodedPath::from_path(&pins_path(&source_root)),
    })
}

pub fn plan_retroarch_cheat_cache_prune(
    cache_root: &Path,
    policy: &CachePrunePolicy,
) -> Result<CachePrunePlan, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    plan_retroarch_cheat_cache_prune_locked(&locked, policy)
}

fn plan_retroarch_cheat_cache_prune_locked(
    locked: &LockedCheatCache,
    policy: &CachePrunePolicy,
) -> Result<CachePrunePlan, CheatSourceError> {
    let cache_root = locked.root();
    if policy.include_abandoned_staging
        && policy.abandoned_staging_min_age_seconds < MINIMUM_ABANDONED_STAGING_AGE_SECONDS
    {
        return Err(cache_error(
            "staging_minimum_age_too_short",
            format!(
                "abandoned staging cleanup requires at least {} seconds",
                MINIMUM_ABANDONED_STAGING_AGE_SECONDS
            ),
        ));
    }
    let inventory = inventory_retroarch_cheat_snapshots_locked(locked)?;
    let now = now_seconds();
    let mut entries = Vec::new();
    let mut source_rank = BTreeMap::<String, usize>::new();
    let explicit_policy = policy.keep_newest_per_source.is_some()
        || policy.retain_newer_than_seconds.is_some()
        || policy.max_cache_bytes.is_some();
    for snapshot in inventory.entries {
        if policy
            .source_filter
            .as_ref()
            .is_some_and(|source| snapshot.source_id.as_ref() != Some(source))
        {
            continue;
        }
        let mut reasons = BTreeSet::new();
        let mut protected = false;
        if snapshot.pinned {
            reasons.insert(CachePruneReason::Pinned);
            protected = true;
        }
        if snapshot.current {
            reasons.insert(CachePruneReason::Current);
            protected = true;
        }
        if snapshot.last_known_good {
            reasons.insert(CachePruneReason::LastKnownGood);
            protected = true;
        }
        if !snapshot.pin_metadata_valid || !snapshot.source_metadata_valid || !snapshot.valid() {
            reasons.insert(if snapshot.valid() {
                CachePruneReason::VerificationRequired
            } else {
                CachePruneReason::InvalidSnapshot
            });
            reasons.insert(CachePruneReason::VerificationRequired);
            protected = true;
        }
        let rank = snapshot
            .source_id
            .as_ref()
            .filter(|_| snapshot.valid())
            .map(|source| {
                let rank = source_rank.entry(source.clone()).or_default();
                let current = *rank;
                *rank += 1;
                current
            })
            .unwrap_or(usize::MAX);
        let keep_allows = match policy.keep_newest_per_source {
            Some(keep) if rank < keep => {
                reasons.insert(CachePruneReason::RequiredKeepCount);
                protected = true;
                false
            }
            Some(_) => {
                reasons.insert(CachePruneReason::ExceedsKeepCount);
                true
            }
            None => true,
        };
        let retention_allows = match (
            policy.retain_newer_than_seconds,
            snapshot.retrieved_at_unix_seconds,
        ) {
            (Some(age), Some(timestamp)) if now.saturating_sub(timestamp) <= age => {
                reasons.insert(CachePruneReason::WithinRetention);
                protected = true;
                false
            }
            (Some(_), Some(_)) => {
                reasons.insert(CachePruneReason::OlderThanRetention);
                true
            }
            (Some(_), None) => {
                reasons.insert(CachePruneReason::VerificationRequired);
                protected = true;
                false
            }
            (None, _) => true,
        };
        if !explicit_policy {
            reasons.insert(CachePruneReason::VerificationRequired);
            protected = true;
        }
        let identity_token = manifest_identity_token(cache_root, &snapshot).ok();
        if identity_token.is_none() {
            reasons.insert(CachePruneReason::VerificationRequired);
            protected = true;
        }
        let bytes = snapshot_bytes(cache_root, &snapshot).unwrap_or(0);
        let resolved_path = match (&snapshot.source_id, &snapshot.snapshot_id) {
            (Some(source), Some(snapshot)) => cache_root
                .join(source)
                .join(SNAPSHOTS_DIRECTORY)
                .join(snapshot),
            _ => PathBuf::new(),
        };
        let policy_selects =
            policy.keep_newest_per_source.is_some() || policy.retain_newer_than_seconds.is_some();
        entries.push(CachePrunePlanEntry {
            kind: CachePruneEntryKind::Snapshot,
            disposition: if policy_selects && !protected && keep_allows && retention_allows {
                CachePruneDisposition::Candidate
            } else {
                CachePruneDisposition::Protected
            },
            source_id: snapshot.source_id,
            snapshot_id: snapshot.snapshot_id,
            path: snapshot.cache_path,
            bytes,
            retrieved_at_unix_seconds: snapshot.retrieved_at_unix_seconds,
            reasons: reasons.into_iter().collect(),
            identity_token,
            resolved_path,
        });
    }
    apply_budget(policy.max_cache_bytes, &mut entries);
    if policy.include_abandoned_staging && locked.present_at_acquisition() {
        entries.extend(plan_staging(cache_root, policy, now)?);
    }
    entries.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.snapshot_id.cmp(&right.snapshot_id))
            .then_with(|| left.path.display.cmp(&right.path.display))
    });
    let candidate_count = entries
        .iter()
        .filter(|entry| entry.disposition == CachePruneDisposition::Candidate)
        .count();
    let protected_count = entries.len() - candidate_count;
    let candidate_bytes = entries
        .iter()
        .filter(|entry| entry.disposition == CachePruneDisposition::Candidate)
        .map(|entry| entry.bytes)
        .fold(0u64, u64::saturating_add);
    Ok(CachePrunePlan {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        cache_root: EncodedPath::from_path(cache_root),
        policy: policy.clone(),
        entries,
        candidate_count,
        protected_count,
        candidate_bytes,
        resolved_cache_root: cache_root.to_path_buf(),
    })
}

fn apply_budget(maximum: Option<u64>, entries: &mut [CachePrunePlanEntry]) {
    let Some(maximum) = maximum else {
        return;
    };
    let total = entries
        .iter()
        .filter(|entry| entry.kind == CachePruneEntryKind::Snapshot)
        .map(|entry| entry.bytes)
        .fold(0u64, u64::saturating_add);
    let already_planned = entries
        .iter()
        .filter(|entry| {
            entry.kind == CachePruneEntryKind::Snapshot
                && entry.disposition == CachePruneDisposition::Candidate
        })
        .map(|entry| entry.bytes)
        .fold(0u64, u64::saturating_add);
    let retained = total.saturating_sub(already_planned);
    if retained <= maximum {
        return;
    }
    let mut needed = retained - maximum;
    let mut indices = (0..entries.len())
        .filter(|index| {
            let entry = &entries[*index];
            entry.kind == CachePruneEntryKind::Snapshot
                && entry.disposition == CachePruneDisposition::Protected
                && !has_hard_protection(&entry.reasons)
        })
        .collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        entries[*left]
            .retrieved_at_unix_seconds
            .cmp(&entries[*right].retrieved_at_unix_seconds)
            .then_with(|| entries[*left].source_id.cmp(&entries[*right].source_id))
            .then_with(|| entries[*left].snapshot_id.cmp(&entries[*right].snapshot_id))
    });
    for index in indices {
        if needed == 0 {
            break;
        }
        if !entries[index]
            .reasons
            .contains(&CachePruneReason::ExceedsCacheBudget)
        {
            entries[index]
                .reasons
                .push(CachePruneReason::ExceedsCacheBudget);
            entries[index].reasons.sort();
        }
        entries[index].disposition = CachePruneDisposition::Candidate;
        needed = needed.saturating_sub(entries[index].bytes);
    }
}

fn has_hard_protection(reasons: &[CachePruneReason]) -> bool {
    reasons.iter().any(|reason| {
        matches!(
            reason,
            CachePruneReason::Pinned
                | CachePruneReason::Current
                | CachePruneReason::LastKnownGood
                | CachePruneReason::WithinRetention
                | CachePruneReason::RequiredKeepCount
                | CachePruneReason::VerificationRequired
                | CachePruneReason::UnsafeOrAmbiguousPath
        )
    })
}

fn plan_staging(
    cache_root: &Path,
    policy: &CachePrunePolicy,
    now: u64,
) -> Result<Vec<CachePrunePlanEntry>, CheatSourceError> {
    let mut planned = Vec::new();
    let mut examined_entries = 0usize;
    if !cache_root.exists() {
        return Ok(planned);
    }
    for source in fs::read_dir(cache_root)
        .map_err(|error| cache_error("cache_inventory_read_failed", error))?
    {
        examined_entries = examined_entries.saturating_add(1);
        enforce_inventory_entry_limit(examined_entries)?;
        let source = source.map_err(|error| cache_error("cache_inventory_read_failed", error))?;
        let source_id = source.file_name().to_str().map(str::to_string);
        if policy
            .source_filter
            .as_ref()
            .is_some_and(|filter| source_id.as_ref() != Some(filter))
        {
            continue;
        }
        let root = source.path().join(STAGING_DIRECTORY);
        if !root.exists() {
            continue;
        }
        if safe_regular_or_directory(&root, true).is_err() {
            planned.push(CachePrunePlanEntry {
                kind: CachePruneEntryKind::Staging,
                disposition: CachePruneDisposition::Protected,
                source_id,
                snapshot_id: None,
                path: EncodedPath::from_path(&root),
                bytes: 0,
                retrieved_at_unix_seconds: None,
                reasons: vec![CachePruneReason::UnsafeOrAmbiguousPath],
                identity_token: None,
                resolved_path: root.clone(),
            });
            continue;
        }
        let items = match fs::read_dir(&root) {
            Ok(items) => items,
            Err(_) => {
                planned.push(CachePrunePlanEntry {
                    kind: CachePruneEntryKind::Staging,
                    disposition: CachePruneDisposition::Protected,
                    source_id: source_id.clone(),
                    snapshot_id: None,
                    path: EncodedPath::from_path(&root),
                    bytes: 0,
                    retrieved_at_unix_seconds: None,
                    reasons: vec![CachePruneReason::UnsafeOrAmbiguousPath],
                    identity_token: None,
                    resolved_path: root.clone(),
                });
                continue;
            }
        };
        for item in items {
            examined_entries = examined_entries.saturating_add(1);
            enforce_inventory_entry_limit(examined_entries)?;
            let item = match item {
                Ok(item) => item,
                Err(_) => {
                    planned.push(CachePrunePlanEntry {
                        kind: CachePruneEntryKind::Staging,
                        disposition: CachePruneDisposition::Protected,
                        source_id: source_id.clone(),
                        snapshot_id: None,
                        path: EncodedPath::from_path(&root),
                        bytes: 0,
                        retrieved_at_unix_seconds: None,
                        reasons: vec![CachePruneReason::UnsafeOrAmbiguousPath],
                        identity_token: None,
                        resolved_path: root.clone(),
                    });
                    break;
                }
            };
            let path = item.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(value) => value,
                Err(_) => {
                    planned.push(CachePrunePlanEntry {
                        kind: CachePruneEntryKind::Staging,
                        disposition: CachePruneDisposition::Protected,
                        source_id: source_id.clone(),
                        snapshot_id: None,
                        path: EncodedPath::from_path(&path),
                        bytes: 0,
                        retrieved_at_unix_seconds: None,
                        reasons: vec![CachePruneReason::UnsafeOrAmbiguousPath],
                        identity_token: None,
                        resolved_path: path,
                    });
                    continue;
                }
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_secs());
            let path_type_safe =
                !metadata.file_type().is_symlink() && (metadata.is_dir() || metadata.is_file());
            let identity = path_type_safe
                .then(|| staging_identity(&path))
                .transpose()
                .ok()
                .flatten();
            let safe = path_type_safe && identity.is_some();
            let old = modified.is_some_and(|timestamp| {
                now.saturating_sub(timestamp) >= policy.abandoned_staging_min_age_seconds
            });
            let source_safe = source_id
                .as_deref()
                .is_some_and(|source| validate_snapshot_name(source).is_ok());
            let reasons = if !safe || !source_safe || modified.is_none() {
                vec![CachePruneReason::UnsafeOrAmbiguousPath]
            } else if old {
                vec![CachePruneReason::IncompleteStagingEntry]
            } else {
                vec![CachePruneReason::RecentOrActiveStaging]
            };
            let (bytes, content_token) = identity.unwrap_or((0, String::new()));
            let token = modified.map(|value| format!("{value}:{content_token}"));
            planned.push(CachePrunePlanEntry {
                kind: CachePruneEntryKind::Staging,
                disposition: if safe && source_safe && old {
                    CachePruneDisposition::Candidate
                } else {
                    CachePruneDisposition::Protected
                },
                source_id: source_id.clone(),
                snapshot_id: None,
                path: EncodedPath::from_path(&path),
                bytes,
                retrieved_at_unix_seconds: modified,
                reasons,
                identity_token: token,
                resolved_path: path,
            });
        }
    }
    Ok(planned)
}

pub fn execute_retroarch_cheat_cache_prune(
    cache_root: &Path,
    plan: &CachePrunePlan,
    confirmed: bool,
) -> Result<CachePruneExecutionResult, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    execute_retroarch_cheat_cache_prune_locked(&locked, plan, confirmed)
}

fn execute_retroarch_cheat_cache_prune_locked(
    locked: &LockedCheatCache,
    plan: &CachePrunePlan,
    confirmed: bool,
) -> Result<CachePruneExecutionResult, CheatSourceError> {
    let cache_root = locked.root();
    if plan.resolved_cache_root != cache_root {
        return Err(cache_error(
            "prune_plan_root_mismatch",
            "prune plan is bound to a different cache root",
        ));
    }
    if !confirmed {
        return Ok(CachePruneExecutionResult {
            schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
            status: CachePruneExecutionStatus::Preview,
            confirmed: false,
            entries: Vec::new(),
            bytes_reclaimed: 0,
            snapshots_deleted: 0,
            staging_entries_removed: 0,
        });
    }
    let mut results = Vec::new();
    for candidate in plan
        .entries
        .iter()
        .filter(|entry| entry.disposition == CachePruneDisposition::Candidate)
    {
        results.push(match candidate.kind {
            CachePruneEntryKind::Snapshot => execute_snapshot_candidate(locked, candidate),
            CachePruneEntryKind::Staging => execute_staging_candidate(
                cache_root,
                candidate,
                plan.policy.abandoned_staging_min_age_seconds,
            ),
        });
    }
    let bytes_reclaimed = results
        .iter()
        .map(|entry| entry.bytes_reclaimed)
        .fold(0u64, u64::saturating_add);
    let snapshots_deleted = results
        .iter()
        .filter(|entry| {
            entry.kind == CachePruneEntryKind::Snapshot
                && entry.status == CachePruneEntryStatus::Deleted
        })
        .count();
    let staging_entries_removed = results
        .iter()
        .filter(|entry| {
            entry.kind == CachePruneEntryKind::Staging
                && entry.status == CachePruneEntryStatus::Deleted
        })
        .count();
    let status = if results
        .iter()
        .all(|entry| entry.status == CachePruneEntryStatus::Deleted)
    {
        CachePruneExecutionStatus::Completed
    } else {
        CachePruneExecutionStatus::PartialFailure
    };
    Ok(CachePruneExecutionResult {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        status,
        confirmed: true,
        entries: results,
        bytes_reclaimed,
        snapshots_deleted,
        staging_entries_removed,
    })
}

fn execute_snapshot_candidate(
    locked: &LockedCheatCache,
    candidate: &CachePrunePlanEntry,
) -> CachePruneExecutionEntry {
    let cache_root = locked.root();
    let outcome = (|| -> Result<u64, (CachePruneEntryStatus, String)> {
        let source = candidate.source_id.as_deref().ok_or((
            CachePruneEntryStatus::Unsafe,
            "candidate has no source ID".into(),
        ))?;
        let snapshot = candidate.snapshot_id.as_deref().ok_or((
            CachePruneEntryStatus::Unsafe,
            "candidate has no snapshot ID".into(),
        ))?;
        validate_snapshot_name(source)
            .map_err(|error| (CachePruneEntryStatus::Unsafe, error.to_string()))?;
        if !is_sha256(snapshot) {
            return Err((
                CachePruneEntryStatus::Unsafe,
                "candidate snapshot identity is not a full SHA-256".into(),
            ));
        }
        let expected_path = cache_root
            .join(source)
            .join(SNAPSHOTS_DIRECTORY)
            .join(snapshot);
        if candidate.resolved_path != expected_path
            || candidate.path != EncodedPath::from_path(&expected_path)
            || expected_path == cache_root
        {
            return Err((
                CachePruneEntryStatus::Unsafe,
                "candidate path is not exactly bound beneath the cache root".into(),
            ));
        }
        let inventory = inventory_retroarch_cheat_snapshots_locked(locked)
            .map_err(|error| (CachePruneEntryStatus::Failed, error.to_string()))?;
        let current = inventory
            .entries
            .iter()
            .find(|entry| {
                entry.source_id.as_deref() == Some(source)
                    && entry.snapshot_id.as_deref() == Some(snapshot)
            })
            .ok_or((
                CachePruneEntryStatus::Changed,
                "snapshot disappeared after planning".into(),
            ))?;
        if current.verification_state == SnapshotVerificationState::UnsafePath {
            return Err((
                CachePruneEntryStatus::Unsafe,
                "snapshot path became unsafe after planning".into(),
            ));
        }
        if current.current
            || current.last_known_good
            || current.pinned
            || !current.pin_metadata_valid
            || !current.source_metadata_valid
        {
            return Err((
                CachePruneEntryStatus::Skipped,
                "snapshot became protected after planning".into(),
            ));
        }
        if !current.valid() {
            return Err((
                CachePruneEntryStatus::Changed,
                "snapshot content or manifest changed after planning".into(),
            ));
        }
        let token = manifest_identity_token(cache_root, current)
            .map_err(|error| (CachePruneEntryStatus::Changed, error.to_string()))?;
        if candidate.identity_token.as_deref() != Some(&token) {
            return Err((
                CachePruneEntryStatus::Changed,
                "snapshot manifest changed after planning".into(),
            ));
        }
        let bytes = snapshot_bytes(cache_root, current)
            .map_err(|error| (CachePruneEntryStatus::Changed, error.to_string()))?;
        if bytes != candidate.bytes {
            return Err((
                CachePruneEntryStatus::Changed,
                "snapshot size changed after planning".into(),
            ));
        }
        safe_regular_or_directory(&expected_path, true)
            .map_err(|error| (CachePruneEntryStatus::Unsafe, error.to_string()))?;
        let manifest_path = cache_root
            .join(source)
            .join(MANIFESTS_DIRECTORY)
            .join(format!("{snapshot}.json"));
        safe_regular_or_directory(&manifest_path, false)
            .map_err(|error| (CachePruneEntryStatus::Unsafe, error.to_string()))?;
        fs::remove_dir_all(&expected_path)
            .map_err(|error| (CachePruneEntryStatus::Failed, error.to_string()))?;
        fs::remove_file(&manifest_path)
            .map_err(|error| (CachePruneEntryStatus::Failed, error.to_string()))?;
        Ok(bytes)
    })();
    execution_entry(candidate, outcome)
}

fn execute_staging_candidate(
    cache_root: &Path,
    candidate: &CachePrunePlanEntry,
    minimum_age: u64,
) -> CachePruneExecutionEntry {
    let outcome = (|| -> Result<u64, (CachePruneEntryStatus, String)> {
        let source = candidate.source_id.as_deref().ok_or((
            CachePruneEntryStatus::Unsafe,
            "staging candidate has no source ID".into(),
        ))?;
        validate_snapshot_name(source)
            .map_err(|error| (CachePruneEntryStatus::Unsafe, error.to_string()))?;
        let staging_root = cache_root.join(source).join(STAGING_DIRECTORY);
        let path = candidate.resolved_path.clone();
        if path.parent() != Some(staging_root.as_path())
            || path == staging_root
            || path == cache_root
        {
            return Err((
                CachePruneEntryStatus::Unsafe,
                "staging candidate is not an exact child of the staging root".into(),
            ));
        }
        validate_cache_path_for_read(&path)
            .map_err(|error| (CachePruneEntryStatus::Unsafe, error.to_string()))?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| (CachePruneEntryStatus::Changed, error.to_string()))?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err((
                CachePruneEntryStatus::Unsafe,
                "staging candidate became a symlink or special file".into(),
            ));
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs())
            .ok_or((
                CachePruneEntryStatus::Changed,
                "staging modification time is unavailable".into(),
            ))?;
        if now_seconds().saturating_sub(modified) < minimum_age {
            return Err((
                CachePruneEntryStatus::Changed,
                "staging entry is no longer old enough".into(),
            ));
        }
        let (bytes, content_token) = staging_identity(&path)
            .map_err(|error| (CachePruneEntryStatus::Changed, error.to_string()))?;
        if candidate.identity_token.as_deref() != Some(&format!("{modified}:{content_token}")) {
            return Err((
                CachePruneEntryStatus::Changed,
                "staging entry changed after planning".into(),
            ));
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .map_err(|error| (CachePruneEntryStatus::Failed, error.to_string()))?;
        Ok(bytes)
    })();
    execution_entry(candidate, outcome)
}

fn execution_entry(
    candidate: &CachePrunePlanEntry,
    outcome: Result<u64, (CachePruneEntryStatus, String)>,
) -> CachePruneExecutionEntry {
    match outcome {
        Ok(bytes) => CachePruneExecutionEntry {
            kind: candidate.kind,
            path: candidate.path.clone(),
            status: CachePruneEntryStatus::Deleted,
            bytes_reclaimed: bytes,
            detail: "deleted after immediate safety revalidation".into(),
        },
        Err((status, detail)) => CachePruneExecutionEntry {
            kind: candidate.kind,
            path: candidate.path.clone(),
            status,
            bytes_reclaimed: 0,
            detail,
        },
    }
}

fn manifest_identity_token(
    cache_root: &Path,
    entry: &SnapshotInventoryEntry,
) -> Result<String, CheatSourceError> {
    let source = entry
        .source_id
        .as_deref()
        .ok_or_else(|| cache_error("snapshot_source_missing", "snapshot has no source"))?;
    let snapshot = entry
        .snapshot_id
        .as_deref()
        .ok_or_else(|| cache_error("snapshot_identity_missing", "snapshot has no identity"))?;
    let path = cache_root
        .join(source)
        .join(MANIFESTS_DIRECTORY)
        .join(format!("{snapshot}.json"));
    safe_regular_or_directory(&path, false)?;
    let bytes =
        fs::read(path).map_err(|error| cache_error("snapshot_manifest_read_failed", error))?;
    Ok(sha256_bytes(&bytes))
}

fn snapshot_bytes(
    cache_root: &Path,
    entry: &SnapshotInventoryEntry,
) -> Result<u64, CheatSourceError> {
    let source = entry
        .source_id
        .as_deref()
        .ok_or_else(|| cache_error("snapshot_source_missing", "snapshot has no source"))?;
    let snapshot = entry
        .snapshot_id
        .as_deref()
        .ok_or_else(|| cache_error("snapshot_identity_missing", "snapshot has no identity"))?;
    let manifest_path = cache_root
        .join(source)
        .join(MANIFESTS_DIRECTORY)
        .join(format!("{snapshot}.json"));
    let manifest = read_manifest(&manifest_path)
        .map_err(|(_, message)| cache_error("snapshot_manifest_invalid", message))?;
    let file_bytes = manifest
        .files
        .iter()
        .map(|file| file.size)
        .fold(0u64, u64::saturating_add);
    let manifest_bytes = fs::metadata(&manifest_path)
        .map_err(|error| cache_error("snapshot_manifest_metadata_failed", error))?
        .len();
    Ok(file_bytes.saturating_add(manifest_bytes))
}

fn safe_tree_bytes(path: &Path) -> Result<u64, CheatSourceError> {
    staging_identity(path).map(|(bytes, _)| bytes)
}

fn staging_identity(path: &Path) -> Result<(u64, String), CheatSourceError> {
    validate_cache_path_for_read(path)?;
    let mut pending = vec![path.to_path_buf()];
    let mut discovered = Vec::new();
    let mut total = 0u64;
    let mut count = 0usize;
    while let Some(current) = pending.pop() {
        count += 1;
        if count > MAINTENANCE_TREE_ENTRY_LIMIT {
            return Err(cache_error(
                "maintenance_tree_entry_limit",
                "maintenance tree contains too many entries",
            ));
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| cache_error("path_inaccessible", error))?;
        if metadata.file_type().is_symlink() {
            return Err(cache_error("unsafe_symlink", "symlink refused"));
        }
        let relative = current.strip_prefix(path).map_err(|_| {
            cache_error(
                "maintenance_tree_escape",
                "maintenance tree escaped its root",
            )
        })?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            if total > MAINTENANCE_TREE_BYTES_LIMIT {
                return Err(cache_error(
                    "maintenance_tree_size_limit",
                    "maintenance tree exceeds the verification byte limit",
                ));
            }
            discovered.push((relative.to_path_buf(), false, metadata.len(), current));
        } else if metadata.is_dir() {
            discovered.push((relative.to_path_buf(), true, 0, current.clone()));
            for entry in
                fs::read_dir(&current).map_err(|error| cache_error("path_inaccessible", error))?
            {
                pending.push(
                    entry
                        .map_err(|error| cache_error("path_inaccessible", error))?
                        .path(),
                );
            }
        } else {
            return Err(cache_error("unsafe_special_file", "special file refused"));
        }
    }
    discovered.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    for (relative, directory, size, full_path) in discovered {
        digest.update(if directory { b"D" } else { b"F" });
        let path_bytes = relative.as_os_str().as_encoded_bytes();
        digest.update((path_bytes.len() as u64).to_le_bytes());
        digest.update(path_bytes);
        digest.update(size.to_le_bytes());
        if !directory {
            let mut file = fs::File::open(&full_path)
                .map_err(|error| cache_error("path_inaccessible", error))?;
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|error| cache_error("path_inaccessible", error))?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
        }
    }
    let identity = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((total, identity))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, CheatSourceError> {
    safe_regular_or_directory(path, false)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| cache_error("maintenance_metadata_read_failed", error))?;
    if metadata.len() > maximum {
        return Err(cache_error(
            "maintenance_metadata_size_limit",
            format!(
                "maintenance metadata exceeds the {maximum}-byte safety limit: {}",
                path.display()
            ),
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| cache_error("maintenance_metadata_read_failed", error))?;
    let mut bytes = Vec::with_capacity(metadata.len().min(maximum).min(64 * 1024) as usize);
    file.by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| cache_error("maintenance_metadata_read_failed", error))?;
    if bytes.len() as u64 > maximum {
        return Err(cache_error(
            "maintenance_metadata_size_limit",
            format!(
                "maintenance metadata grew beyond the {maximum}-byte safety limit while being read: {}",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

fn enforce_inventory_entry_limit(count: usize) -> Result<(), CheatSourceError> {
    if count > MAINTENANCE_INVENTORY_ENTRY_LIMIT {
        Err(cache_error(
            "maintenance_inventory_entry_limit",
            format!(
                "cache inventory exceeds the {}-entry safety limit",
                MAINTENANCE_INVENTORY_ENTRY_LIMIT
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-cache-maintenance-{}-{}",
                std::process::id(),
                super::super::cheat_sources::now_seconds()
            ));
            let mut candidate = path;
            candidate.push(
                std::thread::current()
                    .name()
                    .unwrap_or("test")
                    .replace(':', "_"),
            );
            let _ = fs::remove_dir_all(&candidate);
            fs::create_dir_all(&candidate).unwrap();
            Self(candidate)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(root: &Path, source: &str, id: &str, timestamp: u64, current: bool) -> PathBuf {
        let source_root = root.join(source);
        let snapshot = source_root.join(SNAPSHOTS_DIRECTORY).join(id);
        let manifests = source_root.join(MANIFESTS_DIRECTORY);
        fs::create_dir_all(&snapshot).unwrap();
        fs::create_dir_all(&manifests).unwrap();
        let relative = "Game.cht";
        let bytes = b"cheats = 0\n";
        fs::write(snapshot.join(relative), bytes).unwrap();
        let manifest = CheatSourceManifest {
            format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            source_id: source.into(),
            source_url: "https://example.invalid/cheats.zip".into(),
            canonical_repository_url: "https://github.com/libretro/libretro-database".into(),
            resolved_revision: "1".repeat(40),
            pinned_version: None,
            fetched_at_unix_seconds: timestamp,
            downloaded_bytes: 20,
            extracted_bytes: bytes.len() as u64,
            archive_entry_count: 1,
            archive_sha256: id.into(),
            response_content_type: Some("application/zip".into()),
            response_etag: None,
            response_last_modified: None,
            catalogue_file_count: 1,
            indexed_file_count: 1,
            valid_cheat_count: 0,
            malformed_cheat_count: 0,
            skipped_entry_count: 0,
            excluded_unsupported_count: 0,
            excluded_path_encoding_count: 0,
            exclusion_examples: vec![],
            discovered_platforms: vec![],
            validation_complete: true,
            warnings: vec![],
            catalogue_relative_path: String::new(),
            cache_relative_path: format!("snapshots/{id}"),
            files: vec![super::super::cheat_sources::CheatSourceManifestFile {
                relative_path: relative.into(),
                size: bytes.len() as u64,
                sha256: sha256_bytes(bytes),
            }],
        };
        fs::write(
            manifests.join(format!("{id}.json")),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        if current {
            let metadata = CheatSourceCacheMetadata {
                format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
                source_id: source.into(),
                current_snapshot: Some(id.into()),
                manifest: Some(manifest),
                last_fetch_succeeded: true,
                last_error: None,
                last_error_at_unix_seconds: None,
            };
            fs::write(
                source_root.join(METADATA_FILE),
                serde_json::to_vec_pretty(&metadata).unwrap(),
            )
            .unwrap();
        }
        snapshot
    }

    #[test]
    fn inventory_is_empty_read_only_and_deterministic() {
        let temp = Temp::new();
        let missing = temp.0.join("missing");
        let first = inventory_retroarch_cheat_snapshots(&missing).unwrap();
        let second = inventory_retroarch_cheat_snapshots(&missing).unwrap();
        assert_eq!(first, second);
        assert!(first.entries.is_empty());
        assert!(!missing.exists());
        assert!(inventory_retroarch_cheat_snapshots(Path::new("/")).is_err());
        assert!(inventory_retroarch_cheat_snapshots(Path::new("relative-cache")).is_err());
    }

    #[test]
    fn missing_root_state_does_not_begin_unlocked_reads_if_root_appears() {
        let temp = Temp::new();
        let missing = temp.0.join("appears-later");
        let locked = LockedCheatCache::acquire_existing(&missing).unwrap();
        fs::create_dir(&missing).unwrap();
        fixture(&missing, "source", &"10".repeat(32), 1, false);
        let report = inventory_retroarch_cheat_snapshots_locked(&locked).unwrap();
        assert!(report.entries.is_empty());
    }

    #[test]
    fn inventory_binds_manifests_across_sources_and_detects_identity_mismatch() {
        let temp = Temp::new();
        let a = "11".repeat(32);
        let b = "22".repeat(32);
        let c = "33".repeat(32);
        fixture(&temp.0, "source-a", &a, 10, true);
        fixture(&temp.0, "source-b", &b, 20, false);
        let bad = fixture(&temp.0, "source-a", &c, 30, false);
        let manifest = temp.0.join("source-a/manifests").join(format!("{c}.json"));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["archive_sha256"] = serde_json::Value::String(a);
        fs::write(manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let report = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        assert_eq!(report.entries.len(), 3);
        assert_eq!(report.entries[0].retrieved_at_unix_seconds, Some(30));
        assert_eq!(
            report.entries[0].verification_state,
            SnapshotVerificationState::IdentityMismatch
        );
        assert!(bad.exists());
    }

    #[test]
    fn verification_distinguishes_missing_size_digest_and_unexpected_files_without_writes() {
        let temp = Temp::new();
        let id = "44".repeat(32);
        let snapshot = fixture(&temp.0, "source", &id, 1, false);
        let before = fs::metadata(&snapshot).unwrap().modified().unwrap();
        assert_eq!(
            verify_retroarch_cheat_snapshots(&temp.0, Some(&id), None)
                .unwrap()
                .valid_count,
            1
        );
        fs::write(snapshot.join("Game.cht"), b"changed-data\n").unwrap();
        fs::write(snapshot.join("extra.cht"), b"x").unwrap();
        let report = verify_retroarch_cheat_snapshots(&temp.0, Some(&id), None).unwrap();
        let states = report.entries[0]
            .verification_findings
            .iter()
            .map(|value| value.state)
            .collect::<BTreeSet<_>>();
        assert!(states.contains(&SnapshotVerificationState::SizeMismatch));
        assert!(states.contains(&SnapshotVerificationState::UnexpectedFile));
        assert!(fs::metadata(&snapshot).unwrap().modified().unwrap() >= before);
        fs::remove_file(snapshot.join("Game.cht")).unwrap();
        let report = verify_retroarch_cheat_snapshots(&temp.0, Some(&id), None).unwrap();
        assert!(
            report.entries[0]
                .verification_findings
                .iter()
                .any(|value| value.state == SnapshotVerificationState::MissingFile)
        );
    }

    #[test]
    fn verification_detects_same_size_digest_changes_and_unsupported_schema() {
        let temp = Temp::new();
        let id = "45".repeat(32);
        let snapshot = fixture(&temp.0, "source", &id, 1, false);
        fs::write(snapshot.join("Game.cht"), b"cheats = 1\n").unwrap();
        let report = verify_retroarch_cheat_snapshots(&temp.0, Some(&id), None).unwrap();
        assert!(
            report.entries[0]
                .verification_findings
                .iter()
                .any(|finding| finding.state == SnapshotVerificationState::DigestMismatch)
        );

        let manifest = temp.0.join("source/manifests").join(format!("{id}.json"));
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["format_version"] = 999.into();
        fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let report = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        assert_eq!(
            report.entries[0].verification_state,
            SnapshotVerificationState::UnsupportedSchema
        );
    }

    #[test]
    fn abbreviated_snapshot_ids_must_be_unambiguous() {
        let temp = Temp::new();
        let first = format!("deadbeef{}", "01".repeat(28));
        let second = format!("deadbeef{}", "02".repeat(28));
        fixture(&temp.0, "source", &first, 1, false);
        fixture(&temp.0, "source", &second, 2, false);
        assert_eq!(
            verify_retroarch_cheat_snapshots(&temp.0, Some("deadbeef"), None)
                .unwrap_err()
                .code,
            "snapshot_id_ambiguous"
        );
    }

    #[test]
    fn pin_round_trip_is_atomic_idempotent_and_does_not_change_snapshot() {
        let temp = Temp::new();
        let id = "55".repeat(32);
        let snapshot = fixture(&temp.0, "source", &id, 1, false);
        let before = fs::read(snapshot.join("Game.cht")).unwrap();
        let manifest_path = temp.0.join("source/manifests").join(format!("{id}.json"));
        let manifest_before = fs::read(&manifest_path).unwrap();
        assert_eq!(
            set_retroarch_cheat_snapshot_pin(&temp.0, &id, true)
                .unwrap()
                .status,
            SnapshotPinStatus::Pinned
        );
        assert_eq!(
            set_retroarch_cheat_snapshot_pin(&temp.0, &id, true)
                .unwrap()
                .status,
            SnapshotPinStatus::AlreadyPinned
        );
        assert!(
            inventory_retroarch_cheat_snapshots(&temp.0)
                .unwrap()
                .entries[0]
                .pinned
        );
        assert_eq!(
            set_retroarch_cheat_snapshot_pin(&temp.0, &id, false)
                .unwrap()
                .status,
            SnapshotPinStatus::Unpinned
        );
        assert_eq!(fs::read(snapshot.join("Game.cht")).unwrap(), before);
        assert_eq!(fs::read(manifest_path).unwrap(), manifest_before);
        assert!(set_retroarch_cheat_snapshot_pin(&temp.0, "deadbeef", true).is_err());
    }

    #[test]
    fn malformed_pin_metadata_protects_snapshots() {
        let temp = Temp::new();
        let id = "66".repeat(32);
        fixture(&temp.0, "source", &id, 1, false);
        fs::write(temp.0.join("source/pins.json"), b"bad").unwrap();
        assert!(set_retroarch_cheat_snapshot_pin(&temp.0, &id, true).is_err());
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(
            plan.entries[0]
                .reasons
                .contains(&CachePruneReason::VerificationRequired)
        );
    }

    #[test]
    fn oversized_pin_metadata_is_bounded_and_protects_snapshots() {
        let temp = Temp::new();
        let id = "67".repeat(32);
        let snapshot = fixture(&temp.0, "source", &id, 1, false);
        let pins = temp.0.join("source/pins.json");
        let file = fs::File::create(&pins).unwrap();
        file.set_len(MAINTENANCE_PIN_METADATA_BYTES_LIMIT + 1)
            .unwrap();

        let error = set_retroarch_cheat_snapshot_pin(&temp.0, &id, true).unwrap_err();
        assert_eq!(error.code, "maintenance_metadata_size_limit");
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(snapshot.exists());
    }

    #[test]
    fn duplicate_pin_entries_are_rejected_and_protect_snapshots() {
        let temp = Temp::new();
        let id = "69".repeat(32);
        let snapshot = fixture(&temp.0, "source", &id, 1, false);
        fs::write(
            temp.0.join("source/pins.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "format_version": CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
                "source_id": "source",
                "pinned_snapshots": [&id, &id],
            }))
            .unwrap(),
        )
        .unwrap();

        let error = set_retroarch_cheat_snapshot_pin(&temp.0, &id, true).unwrap_err();
        assert_eq!(error.code, "pin_metadata_duplicate");
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(snapshot.exists());
    }

    #[test]
    fn forged_manifest_sizes_cannot_overflow_accounting() {
        let temp = Temp::new();
        let id = "68".repeat(32);
        fixture(&temp.0, "source", &id, 1, false);
        let manifest_path = temp.0.join("source/manifests").join(format!("{id}.json"));
        let mut manifest: CheatSourceManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.files[0].size = u64::MAX;
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let inventory = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        assert_eq!(
            snapshot_bytes(&temp.0, &inventory.entries[0]).unwrap(),
            u64::MAX
        );
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                max_cache_bytes: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_bytes, 0);
        assert_eq!(plan.entries[0].bytes, u64::MAX);
        assert_eq!(
            plan.entries[0].disposition,
            CachePruneDisposition::Protected
        );
    }

    #[test]
    fn planner_protects_current_pinned_keep_and_retention_deterministically() {
        let temp = Temp::new();
        let old = "77".repeat(32);
        let current = "88".repeat(32);
        fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, now_seconds(), true);
        set_retroarch_cheat_snapshot_pin(&temp.0, &old, true).unwrap();
        let default_plan = plan_retroarch_cheat_cache_prune(&temp.0, &Default::default()).unwrap();
        assert_eq!(default_plan.candidate_count, 0);
        let policy = CachePrunePolicy {
            keep_newest_per_source: Some(1),
            retain_newer_than_seconds: Some(60),
            ..Default::default()
        };
        let first = plan_retroarch_cheat_cache_prune(&temp.0, &policy).unwrap();
        let second = plan_retroarch_cheat_cache_prune(&temp.0, &policy).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.candidate_count, 0);
    }

    #[test]
    fn size_budget_selects_only_enough_old_unprotected_snapshots() {
        let temp = Temp::new();
        let old = "12".repeat(32);
        let middle = "13".repeat(32);
        let current = "14".repeat(32);
        fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &middle, 2, false);
        fixture(&temp.0, "source", &current, 3, true);
        let inventory = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        let one_snapshot = snapshot_bytes(&temp.0, &inventory.entries[0]).unwrap();
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                max_cache_bytes: Some(one_snapshot * 2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 1);
        assert!(plan.entries.iter().any(|entry| {
            entry.snapshot_id.as_deref() == Some(old.as_str())
                && entry
                    .reasons
                    .contains(&CachePruneReason::ExceedsCacheBudget)
        }));
    }

    #[test]
    fn confirmed_prune_deletes_only_old_unprotected_and_reports_exact_bytes() {
        let temp = Temp::new();
        let old = "99".repeat(32);
        let current = "aa".repeat(32);
        fixture(&temp.0, "source", &old, 1, false);
        let current_path = fixture(&temp.0, "source", &current, 2, true);
        let policy = CachePrunePolicy {
            keep_newest_per_source: Some(1),
            ..Default::default()
        };
        let plan = plan_retroarch_cheat_cache_prune(&temp.0, &policy).unwrap();
        assert_eq!(plan.candidate_count, 1);
        let preview = execute_retroarch_cheat_cache_prune(&temp.0, &plan, false).unwrap();
        assert_eq!(preview.status, CachePruneExecutionStatus::Preview);
        assert!(temp.0.join("source/snapshots").join(&old).exists());
        let expected = plan.candidate_bytes;
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.bytes_reclaimed, expected);
        assert_eq!(result.snapshots_deleted, 1);
        assert!(current_path.exists());
    }

    #[test]
    fn changed_after_plan_and_symlink_replacement_are_rejected() {
        let temp = Temp::new();
        let old = "bb".repeat(32);
        let current = "cc".repeat(32);
        let old_path = fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, 2, true);
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(old_path.join("Game.cht"), b"changed\n").unwrap();
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.entries[0].status, CachePruneEntryStatus::Changed);
        assert!(old_path.exists());
    }

    #[test]
    fn pin_added_after_planning_causes_a_safe_skip() {
        let temp = Temp::new();
        let old = "b1".repeat(32);
        let current = "b2".repeat(32);
        let old_path = fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, 2, true);
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        set_retroarch_cheat_snapshot_pin(&temp.0, &old, true).unwrap();
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.entries[0].status, CachePruneEntryStatus::Skipped);
        assert!(old_path.exists());
    }

    #[test]
    fn current_pointer_change_after_planning_causes_a_safe_skip() {
        let temp = Temp::new();
        let old = "b3".repeat(32);
        let current = "b4".repeat(32);
        let old_path = fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, 2, true);
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let manifest_path = temp.0.join("source/manifests").join(format!("{old}.json"));
        let manifest: CheatSourceManifest =
            serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
        let metadata = CheatSourceCacheMetadata {
            format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            source_id: "source".into(),
            current_snapshot: Some(old.clone()),
            manifest: Some(manifest),
            last_fetch_succeeded: true,
            last_error: None,
            last_error_at_unix_seconds: None,
        };
        fs::write(
            temp.0.join("source").join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.entries[0].status, CachePruneEntryStatus::Skipped);
        assert!(old_path.exists());
    }

    #[test]
    fn held_cache_lock_blocks_pin_and_prune_without_mutation() {
        let temp = Temp::new();
        let old = "b5".repeat(32);
        let current = "b6".repeat(32);
        let old_path = fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, 2, true);
        let _held =
            super::super::cheat_cache_lock::LockedCheatCache::acquire_required(&temp.0).unwrap();

        let pin_error = set_retroarch_cheat_snapshot_pin(&temp.0, &old, true).unwrap_err();
        assert_eq!(pin_error.code, "cache_lock_timeout");
        let prune_error = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(prune_error.code, "cache_lock_timeout");
        assert!(old_path.exists());
        assert!(!temp.0.join("source/pins.json").exists());
    }

    #[test]
    fn partial_failure_continues_and_outside_root_plan_path_is_rejected() {
        let temp = Temp::new();
        let first = "bc".repeat(32);
        let second = "bd".repeat(32);
        let current = "be".repeat(32);
        let first_path = fixture(&temp.0, "source", &first, 1, false);
        let second_path = fixture(&temp.0, "source", &second, 2, false);
        fixture(&temp.0, "source", &current, 3, true);
        let mut plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let first_entry = plan
            .entries
            .iter_mut()
            .find(|entry| entry.snapshot_id.as_deref() == Some(first.as_str()))
            .unwrap();
        first_entry.path = EncodedPath::from_path(&temp.0.parent().unwrap().join("outside"));
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.status, CachePruneExecutionStatus::PartialFailure);
        assert!(first_path.exists());
        assert!(!second_path.exists());
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.status == CachePruneEntryStatus::Unsafe)
        );
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.status == CachePruneEntryStatus::Deleted)
        );
    }

    #[test]
    fn abandoned_staging_is_planned_and_cleaned_but_recent_is_preserved() {
        let temp = Temp::new();
        let staging = temp.0.join("source/.staging");
        fs::create_dir_all(&staging).unwrap();
        let old = staging.join("old");
        let recent = staging.join("recent");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("part"), b"abc").unwrap();
        fs::create_dir(&recent).unwrap();
        let old_time = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(2 * 60 * 60))
            .unwrap();
        fs::File::open(&old)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        let policy = CachePrunePolicy {
            include_abandoned_staging: true,
            abandoned_staging_min_age_seconds: MINIMUM_ABANDONED_STAGING_AGE_SECONDS,
            ..Default::default()
        };
        let plan = plan_retroarch_cheat_cache_prune(&temp.0, &policy).unwrap();
        assert_eq!(plan.candidate_count, 1);
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_eq!(result.staging_entries_removed, 1);
        assert!(!old.exists() && recent.exists());
    }

    #[test]
    fn recent_staging_is_conservatively_protected_and_reported_by_verification() {
        let temp = Temp::new();
        let staging = temp.0.join("source/.staging/recent");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("part"), b"abc").unwrap();
        let policy = CachePrunePolicy {
            include_abandoned_staging: true,
            ..Default::default()
        };
        let plan = plan_retroarch_cheat_cache_prune(&temp.0, &policy).unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(
            plan.entries[0]
                .reasons
                .contains(&CachePruneReason::RecentOrActiveStaging)
        );
        let verification = verify_retroarch_cheat_snapshots(&temp.0, None, Some("source")).unwrap();
        assert_eq!(verification.invalid_count, 1);
        assert_eq!(
            verification.entries[0].verification_state,
            SnapshotVerificationState::IncompleteStagingArtifact
        );
        assert!(
            plan_retroarch_cheat_cache_prune(
                &temp.0,
                &CachePrunePolicy {
                    include_abandoned_staging: true,
                    abandoned_staging_min_age_seconds: MINIMUM_ABANDONED_STAGING_AGE_SECONDS - 1,
                    ..Default::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn future_dated_staging_is_never_considered_abandoned() {
        let temp = Temp::new();
        let staging = temp.0.join("source/.staging/future");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("part"), b"abc").unwrap();
        let future = std::time::SystemTime::now()
            .checked_add(std::time::Duration::from_secs(2 * 60 * 60))
            .unwrap();
        fs::File::open(&staging)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(future))
            .unwrap();
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                include_abandoned_staging: true,
                abandoned_staging_min_age_seconds: MINIMUM_ABANDONED_STAGING_AGE_SECONDS,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(
            plan.entries[0]
                .reasons
                .contains(&CachePruneReason::RecentOrActiveStaging)
        );
        assert!(staging.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_snapshot_and_pin_metadata_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let outside = Temp::new();
        let id = "dd".repeat(32);
        fs::create_dir_all(temp.0.join("source/snapshots")).unwrap();
        symlink(&outside.0, temp.0.join("source/snapshots").join(&id)).unwrap();
        let report = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        assert_eq!(
            report.entries[0].verification_state,
            SnapshotVerificationState::UnsafePath
        );
        let valid = "ee".repeat(32);
        fixture(&temp.0, "source2", &valid, 1, false);
        symlink(outside.0.join("pins"), temp.0.join("source2/pins.json")).unwrap();
        assert!(set_retroarch_cheat_snapshot_pin(&temp.0, &valid, true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_after_plan_is_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let outside = Temp::new();
        let old = "ef".repeat(32);
        let current = "f0".repeat(32);
        let old_path = fixture(&temp.0, "source", &old, 1, false);
        fixture(&temp.0, "source", &current, 2, true);
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        fs::remove_dir_all(&old_path).unwrap();
        symlink(&outside.0, &old_path).unwrap();
        let result = execute_retroarch_cheat_cache_prune(&temp.0, &plan, true).unwrap();
        assert_ne!(result.entries[0].status, CachePruneEntryStatus::Deleted);
        assert!(outside.0.exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_snapshot_names_are_reported_losslessly_and_never_pruned() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let temp = Temp::new();
        let snapshots = temp.0.join("source/snapshots");
        fs::create_dir_all(&snapshots).unwrap();
        let path = snapshots.join(OsString::from_vec(vec![b'x', 0xff]));
        fs::create_dir(&path).unwrap();
        let report = inventory_retroarch_cheat_snapshots(&temp.0).unwrap();
        assert_eq!(report.entries.len(), 1);
        assert!(report.entries[0].snapshot_id.is_none());
        assert!(report.entries[0].cache_path.lossy);
        let plan = plan_retroarch_cheat_cache_prune(
            &temp.0,
            &CachePrunePolicy {
                keep_newest_per_source: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.candidate_count, 0);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_plan_root_binding_uses_original_non_utf8_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = Temp::new();
        let first_root = temp.0.join(OsString::from_vec(vec![b'c', 0xfe]));
        let second_root = temp.0.join(OsString::from_vec(vec![b'c', 0xff]));
        assert_eq!(
            EncodedPath::from_path(&first_root),
            EncodedPath::from_path(&second_root)
        );
        let old = "f1".repeat(32);
        let current = "f2".repeat(32);
        fixture(&first_root, "source", &old, 1, false);
        fixture(&first_root, "source", &current, 2, true);
        let second_old = fixture(&second_root, "source", &old, 1, false);
        fixture(&second_root, "source", &current, 2, true);
        let plan = plan_retroarch_cheat_cache_prune(
            &first_root,
            &CachePrunePolicy {
                keep_newest_per_source: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let error = execute_retroarch_cheat_cache_prune(&second_root, &plan, true).unwrap_err();
        assert_eq!(error.code, "prune_plan_root_mismatch");
        assert!(second_old.exists());
    }
}
