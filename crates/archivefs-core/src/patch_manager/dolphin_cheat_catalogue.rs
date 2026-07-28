//! Locally cached, offline-searchable catalogue of Dolphin upstream Gecko
//! definitions.
//!
//! This is a *second* Dolphin Gecko data source alongside
//! [`super::dolphin_gecko_provider`]: instead of fetching one exact game's
//! `.ini` from `master` on every lookup, it downloads a single archive of
//! the upstream repository pinned to a resolved commit, keeps only the
//! `Data/Sys/GameSettings/*.ini` entries whose filename is an exact
//! six-character GameCube Game ID, parses each with the same
//! [`super::gecko_document::parse_dolphin_ini`] reader the single-game
//! provider already uses, and persists a compact JSON index. Selecting a
//! game afterwards is then a pure in-memory lookup - no network, no
//! re-parsing every cached file.
//!
//! The full upstream working tree at one commit is small (tens of
//! megabytes; the `Data/Sys/GameSettings` directory itself is under 2 MB
//! across roughly two thousand files), so a single pinned archive is the
//! "prefer a single archive download" path rather than a many-request
//! per-file crawl. Every other entry in the archive - the emulator's own
//! source code, non-GameCube data - is never decompressed: entry names and
//! modes are still validated for every archive entry (traversal, symlinks,
//! device files), but bytes are only read for entries that already matched
//! the expected `<repo>-<sha>/Data/Sys/GameSettings/<GAMEID>.ini` shape.
//!
//! The persisted catalogue never touches a Dolphin profile. It lives in
//! ArchiveFS's own cache, entirely separate from `User/GameSettings`,
//! transaction journals, or installed cheat files.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::cheat_cache_lock::LockedCheatCache;
use super::cheat_sources::{
    CHEAT_SOURCE_REDIRECT_LIMIT, CheatSourceCancellation, CheatSourceError,
    CheatSourceHttpResponse, CheatSourceProgress, CheatSourceProgressPhase,
    CheatSourceProgressReporter, CheatSourceTransferContext, CheatSourceTransport,
    HttpsCheatSourceTransport, atomic_write_json, prepare_cache_root, secure_create,
    validate_archive_entry_name, validate_cache_path_for_read, validate_downloaded_size,
    validate_entry_count, validate_unix_entry_mode,
};
use super::dolphin_gecko_provider::{
    GeckoProviderEntry, GeckoProviderQuery, GeckoProviderResult, GeckoRegion,
    GeckoRevisionApplicability, peek_cached_gecko_result, region_for_game_id,
};
use super::gecko_document::parse_dolphin_ini;

pub const DOLPHIN_CATALOGUE_SCHEMA_VERSION: u32 = 1;
pub const DOLPHIN_CATALOGUE_PROVIDER_ID: &str = "dolphin_upstream_catalogue";
pub const DOLPHIN_CATALOGUE_REPOSITORY: &str = "dolphin-emu/dolphin";
pub const DOLPHIN_CATALOGUE_LICENSE: &str = "GPL-2.0-or-later";
pub const DOLPHIN_CATALOGUE_ATTRIBUTION: &str =
    "Gecko definitions from the Dolphin Emulator upstream Data/Sys/GameSettings dataset.";
const DOLPHIN_CATALOGUE_REVISION_URL: &str =
    "https://api.github.com/repos/dolphin-emu/dolphin/commits/master";
const DOLPHIN_CATALOGUE_REVISION_HOST: &str = "api.github.com";
const DOLPHIN_CATALOGUE_DOWNLOAD_HOST: &str = "codeload.github.com";
const DOLPHIN_CATALOGUE_CANONICAL_REPOSITORY_URL: &str = "https://github.com/dolphin-emu/dolphin";
const DOLPHIN_CATALOGUE_LICENSE_URL: &str =
    "https://github.com/dolphin-emu/dolphin/blob/master/COPYING";
const CACHE_DIRECTORY: &str = "dolphin-cheat-catalogue";
const CATALOGUE_FILE: &str = "catalogue.json";
const STATE_FILE: &str = "state.json";
const REVISION_RESPONSE_LIMIT: u64 = 64 * 1024;
/// The full pinned-commit archive of dolphin-emu/dolphin. Measured against
/// the real repository this is on the order of tens of megabytes; bounded
/// generously above that so a normal upstream growth spurt does not start
/// failing catalogue updates.
pub const DOLPHIN_CATALOGUE_MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
/// Per-entry cap applied only to the small number of entries that are
/// actually decompressed (`Data/Sys/GameSettings/<GAMEID>.ini`). Real files
/// are a few kilobytes; this is generous headroom against a corrupted or
/// hostile archive claiming an oversized single file.
const MAX_GAME_SETTINGS_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Total decompressed bytes across all `GameSettings/*.ini` entries.
const MAX_TOTAL_GAME_SETTINGS_BYTES: u64 = 128 * 1024 * 1024;
const CATALOGUE_FRESH_SECONDS: u64 = 24 * 60 * 60;
const RETRY_ATTEMPTS: usize = 3;
#[cfg(not(test))]
const RETRY_DELAY_SECONDS: u64 = 5;
#[cfg(test)]
const RETRY_DELAY_SECONDS: u64 = 0;
const OVERALL_TIMEOUT_SECONDS: u64 = 20 * 60;

// ---------------------------------------------------------------------
// Catalogue data model
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolphinCatalogueCode {
    pub name: String,
    pub code_lines: Vec<String>,
    pub notes: Vec<String>,
    pub enabled_by_default: bool,
    pub revision_applicability: GeckoRevisionApplicability,
    pub parse_warnings: Vec<String>,
    pub safe_to_offer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolphinCatalogueGame {
    pub game_id: String,
    pub title: Option<String>,
    pub region: GeckoRegion,
    pub source_relative_path: String,
    pub codes: Vec<DolphinCatalogueCode>,
    pub file_warnings: Vec<String>,
}

impl DolphinCatalogueGame {
    #[must_use]
    pub fn has_usable_gecko(&self) -> bool {
        self.codes.iter().any(|code| code.safe_to_offer)
    }

    #[must_use]
    pub fn usable_gecko_count(&self) -> usize {
        self.codes.iter().filter(|code| code.safe_to_offer).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolphinCatalogueMetadata {
    pub schema_version: u32,
    pub repository: String,
    pub canonical_repository_url: String,
    pub resolved_commit: String,
    pub source_archive_url: String,
    pub license: String,
    pub license_url: String,
    pub attribution: String,
    pub fetched_at_unix_seconds: u64,
    pub archive_sha256: String,
    pub downloaded_bytes: u64,
    pub archive_entry_count: usize,
    pub game_settings_files_inspected: usize,
    pub games_with_usable_gecko: usize,
    pub total_usable_gecko_entries: usize,
    pub malformed_or_skipped_files: usize,
    pub non_matching_files_skipped: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolphinCatalogue {
    pub metadata: DolphinCatalogueMetadata,
    pub games: Vec<DolphinCatalogueGame>,
}

impl DolphinCatalogueMetadata {
    /// True once the catalogue has not been refreshed for longer than the
    /// normal freshness window. This never blocks use of the catalogue - it
    /// only drives the GUI's "Update catalogue" affordance.
    #[must_use]
    pub fn is_stale(&self, now_unix_seconds: u64) -> bool {
        now_unix_seconds.saturating_sub(self.fetched_at_unix_seconds) > CATALOGUE_FRESH_SECONDS
    }
}

impl DolphinCatalogue {
    /// O(log n) lookup by exact Game ID. Callers that perform many lookups
    /// against the same loaded catalogue should build their own `HashMap`
    /// once instead of calling this repeatedly, but for the GUI's
    /// one-lookup-per-game-selection use this is already a pure in-memory
    /// binary search over an already-deserialized `Vec` - no I/O and no
    /// re-parsing of any `.ini` file.
    #[must_use]
    pub fn find(&self, game_id: &str) -> Option<&DolphinCatalogueGame> {
        self.games
            .binary_search_by(|game| game.game_id.as_str().cmp(game_id))
            .ok()
            .map(|index| &self.games[index])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DolphinCatalogueUpdateState {
    pub last_check_unix_seconds: Option<u64>,
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DolphinCatalogueErrorKind {
    CacheUnavailable,
    CacheUnsafe,
    CacheInvalid,
    Network,
    HttpStatus,
    DownloadTooLarge,
    Archive,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinCatalogueError {
    pub kind: DolphinCatalogueErrorKind,
    pub detail: String,
}

impl std::fmt::Display for DolphinCatalogueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for DolphinCatalogueError {}

fn catalogue_error(
    kind: DolphinCatalogueErrorKind,
    detail: impl Into<String>,
) -> DolphinCatalogueError {
    DolphinCatalogueError {
        kind,
        detail: detail.into(),
    }
}

fn from_cache_error(error: CheatSourceError) -> DolphinCatalogueError {
    let kind = match error.stage {
        super::cheat_sources::CheatSourceErrorStage::Cache => {
            DolphinCatalogueErrorKind::CacheUnsafe
        }
        super::cheat_sources::CheatSourceErrorStage::Extraction => {
            DolphinCatalogueErrorKind::Archive
        }
        super::cheat_sources::CheatSourceErrorStage::Download => {
            DolphinCatalogueErrorKind::DownloadTooLarge
        }
        _ => DolphinCatalogueErrorKind::Network,
    };
    catalogue_error(kind, error.to_string())
}

// ---------------------------------------------------------------------
// Cache root
// ---------------------------------------------------------------------

pub fn default_dolphin_catalogue_cache_root() -> Result<PathBuf, DolphinCatalogueError> {
    let database = crate::default_database_path().map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheUnavailable,
            format!("Dolphin catalogue cache root unavailable: {error}"),
        )
    })?;
    Ok(database
        .parent()
        .expect("default database path always has a parent")
        .join(CACHE_DIRECTORY))
}

// ---------------------------------------------------------------------
// Loading the persisted catalogue (no network, used on every game select)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolphinCatalogueLoad {
    /// No catalogue has ever been downloaded.
    NotInstalled,
    /// A catalogue exists and parsed successfully.
    Ready(Box<DolphinCatalogue>),
}

/// Loads the active catalogue from disk, if any. This is a single JSON
/// deserialization - it never walks or re-parses the upstream `.ini` files
/// again.
pub fn load_dolphin_catalogue(
    cache_root: &Path,
) -> Result<DolphinCatalogueLoad, DolphinCatalogueError> {
    let locked = LockedCheatCache::acquire_existing(cache_root).map_err(from_cache_error)?;
    if !locked.present_at_acquisition() {
        return Ok(DolphinCatalogueLoad::NotInstalled);
    }
    let path = locked.root().join(CATALOGUE_FILE);
    validate_cache_path_for_read(&path).map_err(from_cache_error)?;
    if !path.exists() {
        return Ok(DolphinCatalogueLoad::NotInstalled);
    }
    reject_symlink_or_non_file(&path)?;
    let bytes = fs::read(&path).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheUnavailable,
            format!("Dolphin catalogue could not be read: {error}"),
        )
    })?;
    let catalogue: DolphinCatalogue = serde_json::from_slice(&bytes).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheInvalid,
            format!("Dolphin catalogue is invalid: {error}"),
        )
    })?;
    if catalogue.metadata.schema_version != DOLPHIN_CATALOGUE_SCHEMA_VERSION {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::CacheInvalid,
            "Dolphin catalogue schema version is not supported",
        ));
    }
    Ok(DolphinCatalogueLoad::Ready(Box::new(catalogue)))
}

pub fn load_dolphin_catalogue_update_state(
    cache_root: &Path,
) -> Result<DolphinCatalogueUpdateState, DolphinCatalogueError> {
    let locked = LockedCheatCache::acquire_existing(cache_root).map_err(from_cache_error)?;
    if !locked.present_at_acquisition() {
        return Ok(DolphinCatalogueUpdateState {
            last_check_unix_seconds: None,
        });
    }
    let path = locked.root().join(STATE_FILE);
    validate_cache_path_for_read(&path).map_err(from_cache_error)?;
    if !path.exists() {
        return Ok(DolphinCatalogueUpdateState {
            last_check_unix_seconds: None,
        });
    }
    reject_symlink_or_non_file(&path)?;
    let bytes = fs::read(&path).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheUnavailable,
            format!("Dolphin catalogue update state could not be read: {error}"),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheInvalid,
            format!("Dolphin catalogue update state is invalid: {error}"),
        )
    })
}

fn reject_symlink_or_non_file(path: &Path) -> Result<(), DolphinCatalogueError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheUnsafe,
            format!("Dolphin catalogue path could not be inspected: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::CacheUnsafe,
            format!(
                "Dolphin catalogue path is not a regular file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------

/// Removes only ArchiveFS's own catalogue cache. Never touches Dolphin's
/// `User/GameSettings`, installed codes, or transaction history - those
/// live entirely outside `cache_root`.
pub fn remove_dolphin_catalogue(cache_root: &Path) -> Result<(), DolphinCatalogueError> {
    let locked = LockedCheatCache::acquire_existing(cache_root).map_err(from_cache_error)?;
    if !locked.present_at_acquisition() {
        return Ok(());
    }
    for file in [CATALOGUE_FILE, STATE_FILE] {
        let path = locked.root().join(file);
        validate_cache_path_for_read(&path).map_err(from_cache_error)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(catalogue_error(
                    DolphinCatalogueErrorKind::CacheUnsafe,
                    format!(
                        "refusing to remove unexpected cache entry: {}",
                        path.display()
                    ),
                ));
            }
            Ok(_) => fs::remove_file(&path).map_err(|error| {
                catalogue_error(
                    DolphinCatalogueErrorKind::CacheUnavailable,
                    format!("Dolphin catalogue file could not be removed: {error}"),
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(catalogue_error(
                    DolphinCatalogueErrorKind::CacheUnavailable,
                    format!("Dolphin catalogue cache could not be inspected: {error}"),
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Lookup used by the Dolphin cheat workflow
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolphinCatalogueLookup<'a> {
    /// The catalogue has no entry for this exact Game ID.
    NotFound,
    /// The Game ID's declared region does not match the requested region.
    RegionMismatch,
    /// An entry exists, but the upstream file had no usable Gecko codes.
    NoUsableGecko { warnings: &'a [String] },
    /// Usable Gecko codes were found.
    Found(&'a DolphinCatalogueGame),
}

#[must_use]
pub fn lookup_dolphin_catalogue<'a>(
    catalogue: &'a DolphinCatalogue,
    game_id: &str,
    region: &GeckoRegion,
) -> DolphinCatalogueLookup<'a> {
    let Some(game) = catalogue.find(game_id) else {
        return DolphinCatalogueLookup::NotFound;
    };
    if &game.region != region {
        return DolphinCatalogueLookup::RegionMismatch;
    }
    if game.has_usable_gecko() {
        DolphinCatalogueLookup::Found(game)
    } else {
        DolphinCatalogueLookup::NoUsableGecko {
            warnings: &game.file_warnings,
        }
    }
}

// ---------------------------------------------------------------------
// Fetch options / progress
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DolphinCatalogueFetchOptions {
    pub cache_root: PathBuf,
    pub cancellation: Option<CheatSourceCancellation>,
    pub progress: Option<CheatSourceProgressReporter>,
}

impl DolphinCatalogueFetchOptions {
    pub fn with_default_cache() -> Result<Self, DolphinCatalogueError> {
        Ok(Self {
            cache_root: default_dolphin_catalogue_cache_root()?,
            cancellation: None,
            progress: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinCatalogueFetchResult {
    pub catalogue: DolphinCatalogue,
}

// ---------------------------------------------------------------------
// Update check (cheap: resolves the commit only, never downloads)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinCatalogueUpdateCheck {
    pub checked_at_unix_seconds: u64,
    pub latest_commit: String,
    pub update_available: bool,
}

pub fn check_dolphin_catalogue_update(
    cache_root: &Path,
) -> Result<DolphinCatalogueUpdateCheck, DolphinCatalogueError> {
    check_dolphin_catalogue_update_with_transport(cache_root, &HttpsCheatSourceTransport::new())
}

pub fn check_dolphin_catalogue_update_with_transport(
    cache_root: &Path,
    transport: &dyn CheatSourceTransport,
) -> Result<DolphinCatalogueUpdateCheck, DolphinCatalogueError> {
    if !cache_root.exists() {
        prepare_cache_root(cache_root).map_err(from_cache_error)?;
    }
    let locked = LockedCheatCache::acquire_required(cache_root).map_err(from_cache_error)?;
    let resolved = resolve_upstream_commit(transport, None, Instant::now())?;
    let now = super::cheat_sources::now_seconds();

    let existing_commit = {
        let path = locked.root().join(CATALOGUE_FILE);
        if path.exists() {
            reject_symlink_or_non_file(&path)?;
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<DolphinCatalogue>(&bytes).ok())
                .map(|catalogue| catalogue.metadata.resolved_commit)
        } else {
            None
        }
    };

    let state_path = locked.root().join(STATE_FILE);
    let state = DolphinCatalogueUpdateState {
        last_check_unix_seconds: Some(now),
    };
    atomic_write_json(&state_path, &state).map_err(from_cache_error)?;

    Ok(DolphinCatalogueUpdateCheck {
        checked_at_unix_seconds: now,
        latest_commit: resolved.commit_id.clone(),
        update_available: existing_commit.is_none_or(|commit| commit != resolved.commit_id),
    })
}

// ---------------------------------------------------------------------
// Full download / update
// ---------------------------------------------------------------------

pub fn fetch_dolphin_catalogue(
    options: &DolphinCatalogueFetchOptions,
) -> Result<DolphinCatalogueFetchResult, DolphinCatalogueError> {
    fetch_dolphin_catalogue_with_transport(options, &HttpsCheatSourceTransport::new())
}

pub fn fetch_dolphin_catalogue_with_transport(
    options: &DolphinCatalogueFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<DolphinCatalogueFetchResult, DolphinCatalogueError> {
    fetch_dolphin_catalogue_at_with_transport(options, transport, None)
}

/// Re-downloads and re-parses the archive for the Game ID index's *current*
/// pinned commit, without resolving `master` again - i.e. "Rebuild local
/// index" rather than "Update". Fails clearly if no catalogue is installed
/// yet, since there is no pinned commit to rebuild against.
pub fn rebuild_dolphin_catalogue_index_with_transport(
    options: &DolphinCatalogueFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<DolphinCatalogueFetchResult, DolphinCatalogueError> {
    let current_commit = match load_dolphin_catalogue(&options.cache_root)? {
        DolphinCatalogueLoad::Ready(catalogue) => catalogue.metadata.resolved_commit,
        DolphinCatalogueLoad::NotInstalled => {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::CacheInvalid,
                "no catalogue is installed, so there is no pinned commit to rebuild against; use Download instead",
            ));
        }
    };
    fetch_dolphin_catalogue_at_with_transport(options, transport, Some(current_commit))
}

fn fetch_dolphin_catalogue_at_with_transport(
    options: &DolphinCatalogueFetchOptions,
    transport: &dyn CheatSourceTransport,
    pinned_commit: Option<String>,
) -> Result<DolphinCatalogueFetchResult, DolphinCatalogueError> {
    if !options.cache_root.exists() {
        prepare_cache_root(&options.cache_root).map_err(from_cache_error)?;
    }
    let locked =
        LockedCheatCache::acquire_required(&options.cache_root).map_err(from_cache_error)?;
    let transfer_started = Instant::now();
    check_cancelled(options)?;

    report(
        options,
        CheatSourceProgressPhase::ResolvingRevision,
        0,
        None,
    );
    let resolved = match pinned_commit {
        Some(commit_id) => ResolvedCommit {
            archive_url: format!(
                "https://{DOLPHIN_CATALOGUE_DOWNLOAD_HOST}/{DOLPHIN_CATALOGUE_REPOSITORY}/zip/{commit_id}"
            ),
            commit_id,
        },
        None => {
            resolve_upstream_commit(transport, options.cancellation.as_ref(), transfer_started)?
        }
    };

    report(options, CheatSourceProgressPhase::Connecting, 0, None);
    let temporary_archive = locked.root().join(format!(
        ".dolphin-catalogue-{}-{}.zip.partial",
        std::process::id(),
        now_nanos()
    ));
    let cleanup = TempFileGuard(temporary_archive.clone());
    let download = download_archive_with_retries(
        transport,
        &resolved.archive_url,
        &temporary_archive,
        options,
        transfer_started,
    )?;

    report(options, CheatSourceProgressPhase::VerifyingArchive, 0, None);
    let archive_bytes = fs::metadata(&temporary_archive)
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    let archive_sha256 = sha256_of_file(&temporary_archive)?;
    check_cancelled(options)?;

    report(options, CheatSourceProgressPhase::Extracting, 0, None);
    let extraction = extract_and_parse_game_settings(&temporary_archive, options)?;

    report(options, CheatSourceProgressPhase::VerifyingFiles, 0, None);
    let mut games: Vec<DolphinCatalogueGame> = extraction.games.into_values().collect();
    games.sort_by(|a, b| a.game_id.cmp(&b.game_id));
    let games_with_usable_gecko = games.iter().filter(|game| game.has_usable_gecko()).count();
    let total_usable_gecko_entries: usize = games
        .iter()
        .map(DolphinCatalogueGame::usable_gecko_count)
        .sum();

    let metadata = DolphinCatalogueMetadata {
        schema_version: DOLPHIN_CATALOGUE_SCHEMA_VERSION,
        repository: DOLPHIN_CATALOGUE_REPOSITORY.to_string(),
        canonical_repository_url: DOLPHIN_CATALOGUE_CANONICAL_REPOSITORY_URL.to_string(),
        resolved_commit: resolved.commit_id,
        source_archive_url: resolved.archive_url,
        license: DOLPHIN_CATALOGUE_LICENSE.to_string(),
        license_url: DOLPHIN_CATALOGUE_LICENSE_URL.to_string(),
        attribution: DOLPHIN_CATALOGUE_ATTRIBUTION.to_string(),
        fetched_at_unix_seconds: super::cheat_sources::now_seconds(),
        archive_sha256,
        downloaded_bytes: archive_bytes.max(download.downloaded_bytes),
        archive_entry_count: extraction.archive_entry_count,
        game_settings_files_inspected: extraction.game_settings_files_inspected,
        games_with_usable_gecko,
        total_usable_gecko_entries,
        malformed_or_skipped_files: extraction.malformed_or_skipped_files,
        non_matching_files_skipped: extraction.non_matching_files_skipped,
        warnings: extraction.warnings,
    };
    let catalogue = DolphinCatalogue { metadata, games };

    report(options, CheatSourceProgressPhase::Activating, 0, None);
    check_cancelled(options)?;
    let catalogue_path = locked.root().join(CATALOGUE_FILE);
    atomic_write_json(&catalogue_path, &catalogue).map_err(from_cache_error)?;
    let state_path = locked.root().join(STATE_FILE);
    let state = DolphinCatalogueUpdateState {
        last_check_unix_seconds: Some(super::cheat_sources::now_seconds()),
    };
    atomic_write_json(&state_path, &state).map_err(from_cache_error)?;
    drop(cleanup);

    Ok(DolphinCatalogueFetchResult { catalogue })
}

struct TempFileGuard(PathBuf);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn check_cancelled(options: &DolphinCatalogueFetchOptions) -> Result<(), DolphinCatalogueError> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(CheatSourceCancellation::is_cancelled)
    {
        report(options, CheatSourceProgressPhase::Cancelled, 0, None);
        Err(catalogue_error(
            DolphinCatalogueErrorKind::Cancelled,
            "Dolphin catalogue retrieval was cancelled before activation",
        ))
    } else {
        Ok(())
    }
}

fn report(
    options: &DolphinCatalogueFetchOptions,
    phase: CheatSourceProgressPhase,
    attempt: usize,
    bytes_received: Option<u64>,
) {
    if let Some(reporter) = &options.progress {
        reporter.report(CheatSourceProgress {
            phase,
            attempt,
            maximum_attempts: RETRY_ATTEMPTS,
            bytes_received: bytes_received.unwrap_or(0),
            total_bytes: None,
            retry_delay_seconds: None,
        });
    }
}

// ---------------------------------------------------------------------
// Commit resolution
// ---------------------------------------------------------------------

struct ResolvedCommit {
    commit_id: String,
    archive_url: String,
}

#[derive(Deserialize)]
struct RevisionResponse {
    sha: String,
}

fn resolve_upstream_commit(
    transport: &dyn CheatSourceTransport,
    cancellation: Option<&CheatSourceCancellation>,
    transfer_started: Instant,
) -> Result<ResolvedCommit, DolphinCatalogueError> {
    validate_permitted_host_matches(
        DOLPHIN_CATALOGUE_REVISION_URL,
        DOLPHIN_CATALOGUE_REVISION_HOST,
    )?;
    let mut bytes = Vec::new();
    let response = transport
        .get(
            DOLPHIN_CATALOGUE_REVISION_URL,
            REVISION_RESPONSE_LIMIT,
            &mut bytes,
            CheatSourceTransferContext {
                cancellation,
                progress: None,
                attempt: 1,
                overall_timeout: remaining_time(transfer_started)?,
            },
        )
        .map_err(from_cache_error)?;
    if !(200..300).contains(&response.status) || response.downloaded_bytes != bytes.len() as u64 {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::Network,
            "the Dolphin upstream revision lookup was unsuccessful or incomplete",
        ));
    }
    let revision: RevisionResponse = serde_json::from_slice(&bytes).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::Network,
            format!("Dolphin upstream revision response was invalid: {error}"),
        )
    })?;
    if revision.sha.len() != 40 || !revision.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::Network,
            "resolved Dolphin upstream revision is not an exact 40-character commit ID",
        ));
    }
    let commit_id = revision.sha.to_ascii_lowercase();
    let archive_url = format!(
        "https://{DOLPHIN_CATALOGUE_DOWNLOAD_HOST}/{DOLPHIN_CATALOGUE_REPOSITORY}/zip/{commit_id}"
    );
    Ok(ResolvedCommit {
        commit_id,
        archive_url,
    })
}

fn remaining_time(transfer_started: Instant) -> Result<Duration, DolphinCatalogueError> {
    let elapsed = transfer_started.elapsed();
    let overall = Duration::from_secs(OVERALL_TIMEOUT_SECONDS);
    overall.checked_sub(elapsed).ok_or_else(|| {
        catalogue_error(
            DolphinCatalogueErrorKind::Network,
            "Dolphin catalogue retrieval exceeded its overall time budget",
        )
    })
}

// ---------------------------------------------------------------------
// Download with bounded redirects and retries
// ---------------------------------------------------------------------

struct DownloadOutcome {
    downloaded_bytes: u64,
}

fn download_archive_with_retries(
    transport: &dyn CheatSourceTransport,
    initial_url: &str,
    destination_path: &Path,
    options: &DolphinCatalogueFetchOptions,
    transfer_started: Instant,
) -> Result<DownloadOutcome, DolphinCatalogueError> {
    let mut last_error = None;
    for attempt in 1..=RETRY_ATTEMPTS {
        check_cancelled(options)?;
        let mut file = secure_create(destination_path).map_err(from_cache_error)?;
        let result = download_with_redirects(
            transport,
            initial_url,
            &mut file,
            options,
            attempt,
            transfer_started,
        );
        drop(file);
        match result {
            Ok(response) => {
                return Ok(DownloadOutcome {
                    downloaded_bytes: response.downloaded_bytes,
                });
            }
            Err(error) => {
                let _ = fs::remove_file(destination_path);
                let retryable = matches!(
                    error.kind,
                    DolphinCatalogueErrorKind::Network | DolphinCatalogueErrorKind::HttpStatus
                );
                if !retryable || attempt == RETRY_ATTEMPTS {
                    return Err(error);
                }
                report(options, CheatSourceProgressPhase::Retrying, attempt, None);
                last_error = Some(error);
                std::thread::sleep(Duration::from_secs(RETRY_DELAY_SECONDS));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        catalogue_error(
            DolphinCatalogueErrorKind::Network,
            "Dolphin catalogue download failed",
        )
    }))
}

fn download_with_redirects(
    transport: &dyn CheatSourceTransport,
    initial_url: &str,
    destination: &mut dyn Write,
    options: &DolphinCatalogueFetchOptions,
    attempt: usize,
    transfer_started: Instant,
) -> Result<CheatSourceHttpResponse, DolphinCatalogueError> {
    let mut url = initial_url.to_string();
    let mut visited = HashSet::new();
    for redirects in 0..=CHEAT_SOURCE_REDIRECT_LIMIT {
        validate_permitted_host(&url)?;
        if !visited.insert(url.clone()) {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Network,
                "redirect loop detected while downloading the Dolphin catalogue archive",
            ));
        }
        let response = transport
            .get(
                &url,
                DOLPHIN_CATALOGUE_MAX_DOWNLOAD_BYTES,
                destination,
                CheatSourceTransferContext {
                    cancellation: options.cancellation.as_ref(),
                    progress: options.progress.as_ref(),
                    attempt,
                    overall_timeout: remaining_time(transfer_started)?,
                },
            )
            .map_err(from_cache_error)?;
        if (300..400).contains(&response.status) {
            if redirects == CHEAT_SOURCE_REDIRECT_LIMIT {
                return Err(catalogue_error(
                    DolphinCatalogueErrorKind::Network,
                    "redirect limit exceeded while downloading the Dolphin catalogue archive",
                ));
            }
            let location = response.location.as_deref().ok_or_else(|| {
                catalogue_error(
                    DolphinCatalogueErrorKind::Network,
                    "redirect response omitted a Location header",
                )
            })?;
            let base = url::Url::parse(&url).map_err(|error| {
                catalogue_error(DolphinCatalogueErrorKind::Network, error.to_string())
            })?;
            url = base
                .join(location)
                .map_err(|error| {
                    catalogue_error(DolphinCatalogueErrorKind::Network, error.to_string())
                })?
                .to_string();
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::HttpStatus,
                format!(
                    "Dolphin catalogue archive request returned HTTP {}",
                    response.status
                ),
            ));
        }
        validate_downloaded_size(
            response.downloaded_bytes,
            DOLPHIN_CATALOGUE_MAX_DOWNLOAD_BYTES,
        )
        .map_err(from_cache_error)?;
        return Ok(response);
    }
    unreachable!()
}

fn validate_permitted_host(url: &str) -> Result<(), DolphinCatalogueError> {
    let parsed = url::Url::parse(url)
        .map_err(|error| catalogue_error(DolphinCatalogueErrorKind::Network, error.to_string()))?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https"
        || (host != DOLPHIN_CATALOGUE_DOWNLOAD_HOST && host != "github.com")
    {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::Network,
            format!("unexpected host for the Dolphin catalogue archive: {host}"),
        ));
    }
    Ok(())
}

fn validate_permitted_host_matches(
    url: &str,
    expected_host: &str,
) -> Result<(), DolphinCatalogueError> {
    let parsed = url::Url::parse(url)
        .map_err(|error| catalogue_error(DolphinCatalogueErrorKind::Network, error.to_string()))?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https" || host != expected_host {
        return Err(catalogue_error(
            DolphinCatalogueErrorKind::Network,
            format!("unexpected host for the Dolphin catalogue revision lookup: {host}"),
        ));
    }
    Ok(())
}

fn sha256_of_file(path: &Path) -> Result<String, DolphinCatalogueError> {
    let mut file = File::open(path).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::CacheUnavailable,
            format!("Dolphin catalogue archive could not be reopened: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            catalogue_error(
                DolphinCatalogueErrorKind::CacheUnavailable,
                format!("Dolphin catalogue archive could not be hashed: {error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// ---------------------------------------------------------------------
// Safe, bounded extraction restricted to Data/Sys/GameSettings/*.ini
// ---------------------------------------------------------------------

struct ExtractionOutcome {
    games: BTreeMap<String, DolphinCatalogueGame>,
    archive_entry_count: usize,
    game_settings_files_inspected: usize,
    malformed_or_skipped_files: usize,
    non_matching_files_skipped: usize,
    warnings: Vec<String>,
}

fn extract_and_parse_game_settings(
    archive_path: &Path,
    options: &DolphinCatalogueFetchOptions,
) -> Result<ExtractionOutcome, DolphinCatalogueError> {
    let file = File::open(archive_path).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::Archive,
            format!("Dolphin catalogue archive could not be opened: {error}"),
        )
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        catalogue_error(
            DolphinCatalogueErrorKind::Archive,
            format!("Dolphin catalogue archive is not a valid ZIP: {error}"),
        )
    })?;
    validate_entry_count(archive.len()).map_err(from_cache_error)?;

    let mut games: BTreeMap<String, DolphinCatalogueGame> = BTreeMap::new();
    let mut paths = HashSet::new();
    let mut folded = HashSet::new();
    let mut total_expanded = 0u64;
    let mut malformed_or_skipped_files = 0usize;
    let mut non_matching_files_skipped = 0usize;
    let mut warnings = Vec::new();

    for index in 0..archive.len() {
        check_cancelled(options)?;
        let mut entry = archive.by_index(index).map_err(|error| {
            catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("Dolphin catalogue archive entry could not be read: {error}"),
            )
        })?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| {
                catalogue_error(
                    DolphinCatalogueErrorKind::Archive,
                    "Dolphin catalogue archive entry name is not UTF-8",
                )
            })?
            .to_string();
        validate_archive_entry_name(&name).map_err(from_cache_error)?;
        let normalized = name.trim_end_matches('/').to_string();
        if !paths.insert(normalized.clone()) {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("duplicate archive path: {normalized}"),
            ));
        }
        if !folded.insert(normalized.to_ascii_lowercase()) {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("case-folding archive path collision: {normalized}"),
            ));
        }
        let mode = entry
            .unix_mode()
            .unwrap_or(if entry.is_dir() { 0o040755 } else { 0o100644 });
        validate_unix_entry_mode(mode, &normalized).map_err(from_cache_error)?;
        if entry.is_dir() {
            continue;
        }
        let Some(game_id) = matches_game_settings_ini(&normalized) else {
            non_matching_files_skipped += 1;
            continue;
        };

        let file_size = entry.size();
        if file_size > MAX_GAME_SETTINGS_FILE_BYTES {
            malformed_or_skipped_files += 1;
            warnings.push(format!(
                "{normalized}: exceeds the maximum supported GameSettings file size and was skipped"
            ));
            continue;
        }
        total_expanded = total_expanded.checked_add(file_size).ok_or_else(|| {
            catalogue_error(DolphinCatalogueErrorKind::Archive, "expanded size overflow")
        })?;
        if total_expanded > MAX_TOTAL_GAME_SETTINGS_BYTES {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                "Dolphin catalogue GameSettings content exceeds the configured bound",
            ));
        }

        let mut buffer = Vec::with_capacity(file_size as usize);
        std::io::copy(
            &mut entry.by_ref().take(MAX_GAME_SETTINGS_FILE_BYTES + 1),
            &mut buffer,
        )
        .map_err(|error| {
            catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("{normalized}: could not be decompressed: {error}"),
            )
        })?;
        if buffer.len() as u64 != file_size {
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("{normalized}: entry size mismatch"),
            ));
        }

        let Ok(text) = std::str::from_utf8(&buffer) else {
            malformed_or_skipped_files += 1;
            warnings.push(format!("{normalized}: not valid UTF-8 and was skipped"));
            continue;
        };
        let Some(region) = region_for_game_id(&game_id) else {
            malformed_or_skipped_files += 1;
            warnings.push(format!(
                "{normalized}: {game_id:?} does not encode a recognised region and was skipped"
            ));
            continue;
        };
        let game = parse_catalogue_game(&game_id, region, &normalized, text);
        if games.insert(game_id.clone(), game).is_some() {
            // Zip entry paths were already deduplicated above, so this can
            // only happen if two distinct paths both normalize to the same
            // Game ID - never silently pick one.
            return Err(catalogue_error(
                DolphinCatalogueErrorKind::Archive,
                format!("conflicting duplicate Game ID in archive: {game_id}"),
            ));
        }
    }

    Ok(ExtractionOutcome {
        game_settings_files_inspected: games.len() + malformed_or_skipped_files,
        archive_entry_count: archive.len(),
        games,
        malformed_or_skipped_files,
        non_matching_files_skipped,
        warnings,
    })
}

/// Matches `<anything>/Data/Sys/GameSettings/<GAMEID>.ini` exactly - a
/// single path component before `Data/Sys/GameSettings`, no further
/// nesting, and a filename stem that is an exact six-character GameCube
/// Game ID. Anything else (wildcard-prefix files, non-ini files, nested
/// paths, the rest of the repository) is left unread.
fn matches_game_settings_ini(normalized: &str) -> Option<String> {
    let components: Vec<&str> = normalized.split('/').collect();
    if components.len() != 5
        || components[1] != "Data"
        || components[2] != "Sys"
        || components[3] != "GameSettings"
    {
        return None;
    }
    let file_name = components[4];
    let stem = file_name.strip_suffix(".ini")?;
    valid_game_id(stem).then(|| stem.to_string())
}

fn valid_game_id(game_id: &str) -> bool {
    game_id.len() == 6
        && game_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn parse_catalogue_game(
    game_id: &str,
    region: GeckoRegion,
    source_relative_path: &str,
    text: &str,
) -> DolphinCatalogueGame {
    let title = extract_title(text, game_id);
    let document = parse_dolphin_ini(text);
    let has_gecko_section = document.has_gecko_section();

    let mut duplicate_names: BTreeMap<String, usize> = BTreeMap::new();
    for code in &document.gecko_codes {
        *duplicate_names.entry(code.name.clone()).or_default() += 1;
    }

    let mut codes = Vec::with_capacity(document.gecko_codes.len());
    for code in document.gecko_codes {
        let mut parse_warnings: Vec<String> = code
            .warnings
            .iter()
            .map(|warning| warning.detail.clone())
            .collect();
        let duplicate_name = duplicate_names
            .get(code.name.as_str())
            .copied()
            .unwrap_or(0)
            > 1;
        if duplicate_name {
            parse_warnings.push(format!(
                "duplicate Gecko name {:?} is ambiguous and cannot be installed safely",
                code.name
            ));
        }
        let safe_to_offer = code.is_selectable() && !duplicate_name;
        codes.push(DolphinCatalogueCode {
            name: code.name,
            code_lines: code.lines,
            notes: code.notes,
            enabled_by_default: code.enabled_by_default,
            revision_applicability: GeckoRevisionApplicability::Uncertain,
            parse_warnings,
            safe_to_offer,
        });
    }

    let mut file_warnings = Vec::new();
    if !has_gecko_section {
        file_warnings.push("upstream file has no [Gecko] section".to_string());
    } else if codes.is_empty() {
        file_warnings.push("upstream [Gecko] section has no codes".to_string());
    }
    let blocked = codes.iter().filter(|code| !code.safe_to_offer).count();
    if blocked > 0 {
        file_warnings.push(format!(
            "{blocked} malformed or ambiguous Gecko entr{} blocked",
            if blocked == 1 { "y was" } else { "ies were" }
        ));
    }
    if !codes.is_empty() {
        file_warnings.push(
            "Dolphin upstream does not declare which disc revision each Gecko entry supports."
                .to_string(),
        );
    }

    DolphinCatalogueGame {
        game_id: game_id.to_string(),
        title,
        region,
        source_relative_path: source_relative_path.to_string(),
        codes,
        file_warnings,
    }
}

fn extract_title(text: &str, expected_game_id: &str) -> Option<String> {
    let first = text.lines().find(|line| !line.trim().is_empty())?.trim();
    let declaration = first.strip_prefix('#')?.trim();
    let (declared_game_id, title) = declaration
        .split_once(" - ")
        .map_or((declaration, None), |(game_id, title)| {
            (game_id.trim(), Some(title.trim().to_string()))
        });
    (declared_game_id == expected_game_id)
        .then_some(title)
        .flatten()
}

// ---------------------------------------------------------------------
// Integration with the existing Dolphin Gecko provider / install flow
// ---------------------------------------------------------------------

/// Converts one indexed catalogue game into exactly the same
/// [`GeckoProviderResult`] shape the single-game upstream provider already
/// produces, so every downstream consumer (compatibility labels, selection,
/// install staging, "Show exact changes") works unchanged regardless of
/// which source supplied the data.
#[must_use]
pub fn gecko_provider_result_from_catalogue_entry(
    game: &DolphinCatalogueGame,
    metadata: &DolphinCatalogueMetadata,
    revision: u16,
) -> GeckoProviderResult {
    let entries = game
        .codes
        .iter()
        .map(|code| GeckoProviderEntry {
            provider_entry_id: stable_catalogue_entry_id(
                &game.game_id,
                &code.name,
                &code.code_lines,
            ),
            name: code.name.clone(),
            code_lines: code.code_lines.clone(),
            notes: code.notes.clone(),
            region: game.region.clone(),
            revision_applicability: code.revision_applicability,
            parse_warnings: code.parse_warnings.clone(),
            safe_to_offer: code.safe_to_offer,
        })
        .collect();
    GeckoProviderResult {
        provider_id: DOLPHIN_CATALOGUE_PROVIDER_ID.to_string(),
        provider_display_name: "Dolphin cheat catalogue".to_string(),
        source_identity: format!("{}@{}", metadata.repository, metadata.resolved_commit),
        retrieved_at_unix_seconds: metadata.fetched_at_unix_seconds,
        game_id: game.game_id.clone(),
        title: game.title.clone(),
        region: game.region.clone(),
        revision,
        entries,
        warnings: game.file_warnings.clone(),
        attribution: metadata.attribution.clone(),
        license: metadata.license.clone(),
    }
}

fn stable_catalogue_entry_id(game_id: &str, name: &str, lines: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DOLPHIN_CATALOGUE_PROVIDER_ID.as_bytes());
    hasher.update([0]);
    hasher.update(game_id.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    for line in lines {
        hasher.update([0]);
        hasher.update(line.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Every state the beginner Dolphin cheat workflow must distinguish, per
/// game selection, without ever issuing a network request. `cached` is the
/// fallback path (2) - a previously fetched, still-parseable single-game
/// result, read straight from disk - populated whenever available so the
/// caller can offer it even when the catalogue itself has nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolphinGeckoLookupResult {
    /// Fallback path (1) is empty because no catalogue has been downloaded.
    NoCatalogueInstalled { cached: Option<GeckoProviderResult> },
    /// The catalogue is installed but has no file for this exact Game ID.
    NotInCatalogue { cached: Option<GeckoProviderResult> },
    /// The catalogue's file matched, but declares a different region.
    RegionMismatch { cached: Option<GeckoProviderResult> },
    /// The catalogue's file matched, but had no usable Gecko codes.
    CatalogueEntryHasNoUsableCodes {
        warnings: Vec<String>,
        cached: Option<GeckoProviderResult>,
    },
    /// Fallback path (1), the local full catalogue, produced a usable
    /// result - this is the normal, no-network case.
    Found(GeckoProviderResult),
}

/// The GUI's single, network-free entry point for "what do we already know
/// about this GameCube game's Gecko codes" - called on every game
/// selection. Priority order: (1) the local full catalogue, (2) a validated
/// cached single-game result. Never issues a network request; an explicit
/// per-game fetch (path 3) remains a separate, user-triggered action under
/// Details.
pub fn resolve_dolphin_gecko_lookup(
    catalogue_cache_root: &Path,
    provider_cache_root: &Path,
    game_id: &str,
    region: &GeckoRegion,
    revision: u16,
) -> Result<DolphinGeckoLookupResult, DolphinCatalogueError> {
    let cached = peek_cached_gecko_result(
        provider_cache_root,
        &GeckoProviderQuery {
            game_id: game_id.to_string(),
            region: region.clone(),
            revision,
        },
    );
    match load_dolphin_catalogue(catalogue_cache_root)? {
        DolphinCatalogueLoad::NotInstalled => {
            Ok(DolphinGeckoLookupResult::NoCatalogueInstalled { cached })
        }
        DolphinCatalogueLoad::Ready(catalogue) => {
            match lookup_dolphin_catalogue(&catalogue, game_id, region) {
                DolphinCatalogueLookup::NotFound => {
                    Ok(DolphinGeckoLookupResult::NotInCatalogue { cached })
                }
                DolphinCatalogueLookup::RegionMismatch => {
                    Ok(DolphinGeckoLookupResult::RegionMismatch { cached })
                }
                DolphinCatalogueLookup::NoUsableGecko { warnings } => {
                    Ok(DolphinGeckoLookupResult::CatalogueEntryHasNoUsableCodes {
                        warnings: warnings.to_vec(),
                        cached,
                    })
                }
                DolphinCatalogueLookup::Found(game) => Ok(DolphinGeckoLookupResult::Found(
                    gecko_provider_result_from_catalogue_entry(game, &catalogue.metadata, revision),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests;
