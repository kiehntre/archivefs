//! Optional read-only adapter for Andrew Mackrodt's BSFree Archive.
//!
//! The upstream database remains an immutable third-party artifact. EmuWiz
//! never migrates it, never opens it writable, and Stage 1 deliberately has no
//! installation or conversion API.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{default_database_path, normalize_path_segment};

use super::{
    CheatProviderIdentity, CheatProviderLicence, CheatProviderLicenceStatus,
    CheatProviderProvenance, CheatProviderSourceState, CheatSourceCancellation,
    CheatSourceProgressReporter, CheatSourceTransferContext, CheatSourceTransport,
    DeviceFormatCompatibility, ImmutableSourceFingerprint, PageRequest, PlatformMappingStatus,
    ProviderDeviceMapping, ProviderGameMatchConfidence, ProviderPage, ProviderPlatformMapping,
    ProviderValidationResult, ProviderValidationStatus, ReadOnlyCheatCatalogue,
};

pub const BSFREE_PROVIDER_FORMAT_VERSION: u32 = 1;
pub const BSFREE_PROVIDER_ID: &str = "bsfree-archive";
pub const BSFREE_DATABASE_FILE: &str = "bsfree.db";
pub const BSFREE_DATABASE_URL: &str = "https://static.mackrodt.io/files/bsfree.4cfee26.db";
pub const BSFREE_DOWNLOAD_HOST: &str = "static.mackrodt.io";
pub const BSFREE_UPSTREAM_PROJECT: &str = "https://github.com/andrewmackrodt/bsfree";
pub const BSFREE_EXPECTED_SHA256: &str =
    "4cfee2640e5584adc52977bc56192f6b026814a8e3711687dd02519643631a06";
pub const BSFREE_EXPECTED_SIZE_BYTES: u64 = 296_218_624;
pub const BSFREE_MAX_DATABASE_BYTES: u64 = 512 * 1024 * 1024;
pub const BSFREE_REDIRECT_LIMIT: usize = 3;
const SOURCE_DIRECTORY: &str = "bsfree";
const SOURCE_METADATA_FILE: &str = "source.json";
const SOURCE_HASH_FILE: &str = "bsfree.db.sha256";
const VALIDATION_FILE: &str = "last-validation.json";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_SHORT_TEXT: usize = 1024;
const MAX_CODE_BODY: usize = 16 * 1024;
const MAX_NOTE: usize = 64 * 1024;
const HASH_CHUNK: usize = 128 * 1024;
const SCHEMA_FINGERPRINT: &str = "bsfree-v1:authors(id:INTEGER:pk,name:TEXT!,qty:INTEGER!);codes(id:INTEGER:pk,name:TEXT!,code:TEXT!,note:TEXT?,game_uid:INTEGER!,game_id:INTEGER!,system_id:INTEGER!,device_id:INTEGER!,section_id:INTEGER?,author_id:INTEGER?);devices(id:INTEGER:pk,name:TEXT!,qty:INTEGER!);games(id:INTEGER:pk,game_id:INTEGER!,name:TEXT!,version:TEXT?,system_id:INTEGER!,device_id:INTEGER!,qty:INTEGER!);sections(id:INTEGER:pk,game_id:INTEGER!,name:TEXT!,qty:INTEGER!);system_devices(system_id:INTEGER:pk1,device_id:INTEGER:pk2);systems(id:INTEGER:pk,group_id:INTEGER!,name:TEXT!,qty:INTEGER!)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BsFreeErrorKind {
    NotInstalled,
    UnsafePath,
    SourceUnreadable,
    Download,
    DownloadTooLarge,
    RedirectRejected,
    HashMismatch,
    NotSqlite,
    UnsupportedSchema,
    Validation,
    Query,
    CacheWrite,
    Cancelled,
    ConfirmationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeError {
    pub kind: BsFreeErrorKind,
    pub message: String,
}

impl BsFreeError {
    fn new(kind: BsFreeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BsFreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BsFreeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BsFreePaths {
    pub root: PathBuf,
    pub database: PathBuf,
    pub metadata: PathBuf,
    pub hash: PathBuf,
    pub validation: PathBuf,
}

impl BsFreePaths {
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            database: root.join(BSFREE_DATABASE_FILE),
            metadata: root.join(SOURCE_METADATA_FILE),
            hash: root.join(SOURCE_HASH_FILE),
            validation: root.join(VALIDATION_FILE),
            root,
        }
    }
}

pub fn default_bsfree_source_root() -> Result<PathBuf, BsFreeError> {
    let database = default_database_path()
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::UnsafePath, error.to_string()))?;
    Ok(database
        .parent()
        .expect("default database path has a parent")
        .join("cheat-sources")
        .join(SOURCE_DIRECTORY))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeCounts {
    pub systems: u64,
    pub devices: u64,
    pub system_devices: u64,
    pub games: u64,
    pub sections: u64,
    pub authors: u64,
    pub codes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeValidation {
    pub result: ProviderValidationResult,
    pub counts: BsFreeCounts,
    pub sqlite_version: String,
    pub database_path: PathBuf,
    pub opened_read_only: bool,
    pub query_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BsFreeSourceMetadata {
    format_version: u32,
    provider_id: String,
    enabled: bool,
    state: CheatProviderSourceState,
    last_operation_at_unix_seconds: u64,
    validation: Option<BsFreeValidation>,
    last_error: Option<BsFreeError>,
}

impl Default for BsFreeSourceMetadata {
    fn default() -> Self {
        Self {
            format_version: BSFREE_PROVIDER_FORMAT_VERSION,
            provider_id: BSFREE_PROVIDER_ID.to_string(),
            enabled: true,
            state: CheatProviderSourceState::NotInstalled,
            last_operation_at_unix_seconds: now_unix_seconds(),
            validation: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeSourceStatus {
    pub format_version: u32,
    pub provider: CheatProviderIdentity,
    pub state: CheatProviderSourceState,
    pub enabled: bool,
    pub usable: bool,
    pub database_path: PathBuf,
    pub fingerprint: Option<ImmutableSourceFingerprint>,
    pub validation: Option<BsFreeValidation>,
    pub last_error: Option<BsFreeError>,
    pub provenance: CheatProviderProvenance,
    pub licence: CheatProviderLicence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeSystem {
    pub upstream_id: i64,
    pub name: String,
    pub group_id: i64,
    pub cheat_count: u64,
    pub mapping: ProviderPlatformMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeDevice {
    pub upstream_id: i64,
    pub name: String,
    pub cheat_count: u64,
    pub mapping: ProviderDeviceMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeGame {
    pub upstream_uid: i64,
    pub upstream_game_id: i64,
    pub name: String,
    pub version: Option<String>,
    pub system: BsFreeSystemSummary,
    pub device: BsFreeDeviceSummary,
    pub cheat_count: u64,
    pub match_confidence: Option<ProviderGameMatchConfidence>,
    pub match_explanation: Option<String>,
    pub revision_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeSystemSummary {
    pub upstream_id: i64,
    pub name: String,
    pub archivefs_platform_id: Option<String>,
    pub archivefs_platform_display_name: Option<String>,
    pub mapping_status: PlatformMappingStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeDeviceSummary {
    pub upstream_id: i64,
    pub name: String,
    pub compatibility: DeviceFormatCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeCheat {
    pub upstream_id: i64,
    pub name: String,
    pub note: Option<String>,
    pub code: String,
    pub section: Option<BsFreeNamedRow>,
    pub author: Option<BsFreeNamedRow>,
    pub device: BsFreeDeviceSummary,
    pub compatibility: DeviceFormatCompatibility,
    pub truncated_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeNamedRow {
    pub upstream_id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeGameSearchRequest {
    pub platform_id: Option<String>,
    pub title: String,
    pub version: Option<String>,
    pub device_id: Option<i64>,
    pub upstream_game_id: Option<i64>,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeGameSearchResult {
    pub confidence: ProviderGameMatchConfidence,
    pub exact_revision_verified: bool,
    pub explanation: String,
    pub page: ProviderPage<BsFreeGame>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeAttribution {
    pub provider: String,
    pub upstream_project: String,
    pub original_archive: String,
    pub database_sha256: String,
    pub licence: String,
}

#[derive(Debug, Clone)]
pub struct BsFreeDownloadOptions {
    pub cancellation: Option<CheatSourceCancellation>,
    pub progress: Option<CheatSourceProgressReporter>,
    pub overall_timeout: Duration,
}

impl Default for BsFreeDownloadOptions {
    fn default() -> Self {
        Self {
            cancellation: None,
            progress: None,
            overall_timeout: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BsFreeActivationResult {
    pub status: BsFreeSourceStatus,
    pub imported_from_local_file: bool,
    pub network_used: bool,
}

pub fn bsfree_provider_identity() -> CheatProviderIdentity {
    CheatProviderIdentity {
        id: BSFREE_PROVIDER_ID.to_string(),
        display_name: "BSFree Archive".to_string(),
        upstream_project: BSFREE_UPSTREAM_PROJECT.to_string(),
    }
}

pub fn bsfree_provenance() -> CheatProviderProvenance {
    CheatProviderProvenance {
        source: "BSFree Archive".to_string(),
        maintainer: "Andrew Mackrodt".to_string(),
        origin: "Historical bsfree.org database".to_string(),
        distribution_status: "Optional third-party download".to_string(),
        verification: "Historical community data, not verified by EmuWiz".to_string(),
    }
}

pub fn bsfree_licence() -> CheatProviderLicence {
    CheatProviderLicence {
        status: CheatProviderLicenceStatus::NotEstablished,
        statement: "Database-content licence not established".to_string(),
    }
}

pub fn bsfree_attribution() -> BsFreeAttribution {
    BsFreeAttribution {
        provider: "BSFree Archive".to_string(),
        upstream_project: "andrewmackrodt/bsfree".to_string(),
        original_archive: "bsfree.org".to_string(),
        database_sha256: BSFREE_EXPECTED_SHA256.to_string(),
        licence: "Database-content licence not established".to_string(),
    }
}

pub fn inspect_bsfree_source(paths: &BsFreePaths) -> Result<BsFreeSourceStatus, BsFreeError> {
    let metadata = read_metadata(paths)?.unwrap_or_default();
    let database_present = safe_regular_file_if_present(&paths.database)?;
    let fingerprint = if database_present {
        metadata
            .validation
            .as_ref()
            .map(|validation| validation.result.source_fingerprint.clone())
    } else {
        None
    };
    let enabled = metadata.enabled;
    let usable = enabled
        && database_present
        && metadata
            .validation
            .as_ref()
            .is_some_and(|validation| validation.result.status == ProviderValidationStatus::Valid);
    let state = if !enabled {
        CheatProviderSourceState::Disabled
    } else if !database_present {
        match metadata.state {
            CheatProviderSourceState::Downloading
            | CheatProviderSourceState::Validating
            | CheatProviderSourceState::DownloadFailed
            | CheatProviderSourceState::ValidationFailed
            | CheatProviderSourceState::Invalid
            | CheatProviderSourceState::UnsupportedSchema => metadata.state,
            _ => CheatProviderSourceState::NotInstalled,
        }
    } else {
        metadata.state
    };
    Ok(BsFreeSourceStatus {
        format_version: BSFREE_PROVIDER_FORMAT_VERSION,
        provider: bsfree_provider_identity(),
        state,
        enabled,
        usable,
        database_path: paths.database.clone(),
        fingerprint,
        validation: metadata.validation,
        last_error: metadata.last_error,
        provenance: bsfree_provenance(),
        licence: bsfree_licence(),
    })
}

pub fn validate_bsfree_database(path: &Path) -> Result<BsFreeValidation, BsFreeError> {
    validate_bsfree_database_with_hash(path, Some(BSFREE_EXPECTED_SHA256))
}

fn validate_bsfree_database_with_hash(
    path: &Path,
    expected_hash: Option<&str>,
) -> Result<BsFreeValidation, BsFreeError> {
    let fingerprint = fingerprint_regular_file(path)?;
    if let Some(expected) = expected_hash
        && fingerprint.sha256 != expected
    {
        return Err(BsFreeError::new(
            BsFreeErrorKind::HashMismatch,
            format!(
                "BSFree database SHA-256 was {}, expected {expected}",
                fingerprint.sha256
            ),
        ));
    }
    validate_sqlite_header(path)?;
    let connection = open_immutable_connection(path)?;
    validate_schema(&connection)?;
    validate_relationships(&connection)?;
    let counts = read_counts(&connection)?;
    if counts.systems == 0 || counts.devices == 0 || counts.games == 0 || counts.codes == 0 {
        return Err(BsFreeError::new(
            BsFreeErrorKind::Validation,
            "BSFree sanity counts contain an empty required entity",
        ));
    }
    let query_only = connection
        .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
        .map_err(query_error)?
        == 1;
    let sqlite_version = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(query_error)?;
    Ok(BsFreeValidation {
        result: ProviderValidationResult {
            status: ProviderValidationStatus::Valid,
            validated_at_unix_seconds: now_unix_seconds(),
            schema_fingerprint: Some(hex_sha256(SCHEMA_FINGERPRINT.as_bytes())),
            source_fingerprint: fingerprint,
            diagnostics: vec![
                "Required BSFree tables and columns are present".to_string(),
                "Bounded relationship checks found no orphaned required rows".to_string(),
            ],
        },
        counts,
        sqlite_version,
        database_path: path.to_path_buf(),
        opened_read_only: true,
        query_only,
    })
}

pub fn validate_installed_bsfree_source(
    paths: &BsFreePaths,
) -> Result<BsFreeSourceStatus, BsFreeError> {
    update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
    match validate_bsfree_database(&paths.database) {
        Ok(validation) => {
            activate_validation_metadata(paths, validation)?;
            inspect_bsfree_source(paths)
        }
        Err(error) => {
            let state = state_for_validation_error(&error);
            update_metadata_state(paths, state, None, Some(error.clone()))?;
            Err(error)
        }
    }
}

pub fn set_bsfree_enabled(
    paths: &BsFreePaths,
    enabled: bool,
) -> Result<BsFreeSourceStatus, BsFreeError> {
    let mut metadata = read_metadata(paths)?.unwrap_or_default();
    metadata.enabled = enabled;
    metadata.state = if enabled {
        if metadata.validation.is_some() && safe_regular_file_if_present(&paths.database)? {
            CheatProviderSourceState::Ready
        } else {
            CheatProviderSourceState::NotInstalled
        }
    } else {
        CheatProviderSourceState::Disabled
    };
    metadata.last_operation_at_unix_seconds = now_unix_seconds();
    write_metadata(paths, &metadata)?;
    inspect_bsfree_source(paths)
}

pub fn import_local_bsfree_database(
    paths: &BsFreePaths,
    source: &Path,
) -> Result<BsFreeActivationResult, BsFreeError> {
    import_local_with_expected_hash(paths, source, Some(BSFREE_EXPECTED_SHA256))
}

fn import_local_with_expected_hash(
    paths: &BsFreePaths,
    source: &Path,
    expected_hash: Option<&str>,
) -> Result<BsFreeActivationResult, BsFreeError> {
    prepare_source_root(paths)?;
    let mut input = open_regular_nofollow(source)?;
    let source_metadata = input
        .metadata()
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string()))?;
    if source_metadata.len() > BSFREE_MAX_DATABASE_BYTES {
        return Err(BsFreeError::new(
            BsFreeErrorKind::DownloadTooLarge,
            "selected BSFree database exceeds the configured size limit",
        ));
    }
    update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
    let staging = staging_path(paths, "import");
    let result = (|| {
        copy_bounded(&mut input, &staging, BSFREE_MAX_DATABASE_BYTES)?;
        let validation = validate_bsfree_database_with_hash(&staging, expected_hash)?;
        publish_validated_database(paths, &staging, validation)?;
        Ok(BsFreeActivationResult {
            status: inspect_bsfree_source(paths)?,
            imported_from_local_file: true,
            network_used: false,
        })
    })();
    if let Err(error) = &result {
        let _ = fs::remove_file(&staging);
        let state = state_for_validation_error(error);
        let _ = update_metadata_state(paths, state, None, Some(error.clone()));
    }
    result
}

pub fn download_bsfree_database(
    paths: &BsFreePaths,
    options: &BsFreeDownloadOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<BsFreeActivationResult, BsFreeError> {
    download_with_expected_hash(paths, options, transport, Some(BSFREE_EXPECTED_SHA256))
}

fn download_with_expected_hash(
    paths: &BsFreePaths,
    options: &BsFreeDownloadOptions,
    transport: &dyn CheatSourceTransport,
    expected_hash: Option<&str>,
) -> Result<BsFreeActivationResult, BsFreeError> {
    prepare_source_root(paths)?;
    update_metadata_state(paths, CheatProviderSourceState::Downloading, None, None)?;
    let staging = staging_path(paths, "download");
    let result = (|| {
        let mut current = BSFREE_DATABASE_URL.to_string();
        let mut visited = BTreeSet::new();
        for redirect_index in 0..=BSFREE_REDIRECT_LIMIT {
            validate_download_url(&current, redirect_index == 0)?;
            if !visited.insert(current.clone()) {
                return Err(BsFreeError::new(
                    BsFreeErrorKind::RedirectRejected,
                    "BSFree download redirect loop rejected",
                ));
            }
            let mut output = create_new_nofollow(&staging)?;
            let response = transport
                .get(
                    &current,
                    BSFREE_MAX_DATABASE_BYTES,
                    &mut output,
                    CheatSourceTransferContext {
                        cancellation: options.cancellation.as_ref(),
                        progress: options.progress.as_ref(),
                        attempt: 1,
                        overall_timeout: options.overall_timeout,
                    },
                )
                .map_err(|error| {
                    let kind = if error.code == "cancelled" {
                        BsFreeErrorKind::Cancelled
                    } else {
                        BsFreeErrorKind::Download
                    };
                    BsFreeError::new(kind, error.to_string())
                })?;
            output.sync_all().map_err(cache_write_error)?;
            drop(output);
            if (300..400).contains(&response.status) {
                fs::remove_file(&staging).map_err(cache_write_error)?;
                let location = response.location.ok_or_else(|| {
                    BsFreeError::new(
                        BsFreeErrorKind::RedirectRejected,
                        "BSFree redirect omitted Location",
                    )
                })?;
                if redirect_index == BSFREE_REDIRECT_LIMIT {
                    return Err(BsFreeError::new(
                        BsFreeErrorKind::RedirectRejected,
                        "BSFree redirect limit exceeded",
                    ));
                }
                current = Url::parse(&current)
                    .and_then(|base| base.join(&location))
                    .map_err(|_| {
                        BsFreeError::new(
                            BsFreeErrorKind::RedirectRejected,
                            "BSFree redirect URL is invalid",
                        )
                    })?
                    .to_string();
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(BsFreeError::new(
                    BsFreeErrorKind::Download,
                    format!("BSFree server returned HTTP {}", response.status),
                ));
            }
            update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
            let validation = validate_bsfree_database_with_hash(&staging, expected_hash)?;
            publish_validated_database(paths, &staging, validation)?;
            return Ok(BsFreeActivationResult {
                status: inspect_bsfree_source(paths)?,
                imported_from_local_file: false,
                network_used: true,
            });
        }
        unreachable!("bounded redirect loop always returns")
    })();
    if let Err(error) = &result {
        let _ = fs::remove_file(&staging);
        let state = match error.kind {
            BsFreeErrorKind::UnsupportedSchema => CheatProviderSourceState::UnsupportedSchema,
            BsFreeErrorKind::HashMismatch
            | BsFreeErrorKind::NotSqlite
            | BsFreeErrorKind::Validation => CheatProviderSourceState::ValidationFailed,
            _ => CheatProviderSourceState::DownloadFailed,
        };
        let _ = update_metadata_state(paths, state, None, Some(error.clone()));
    }
    result
}

pub fn remove_local_bsfree_source(paths: &BsFreePaths, confirmed: bool) -> Result<(), BsFreeError> {
    if !confirmed {
        return Err(BsFreeError::new(
            BsFreeErrorKind::ConfirmationRequired,
            "removing the BSFree local copy requires explicit confirmation",
        ));
    }
    for path in [
        &paths.database,
        &paths.hash,
        &paths.validation,
        &paths.metadata,
    ] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(BsFreeError::new(
                    BsFreeErrorKind::UnsafePath,
                    format!("refusing unsafe BSFree cache entry {}", path.display()),
                ));
            }
            Ok(_) => fs::remove_file(path).map_err(cache_write_error)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(cache_write_error(error)),
        }
    }
    match fs::remove_dir(&paths.root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Err(BsFreeError::new(
                BsFreeErrorKind::UnsafePath,
                "BSFree source directory contains an unknown entry and was left intact",
            ))
        }
        Err(error) => Err(cache_write_error(error)),
    }
}

pub struct BsFreeCatalogue {
    connection: Connection,
    fingerprint: ImmutableSourceFingerprint,
}

impl BsFreeCatalogue {
    pub fn open(path: &Path) -> Result<Self, BsFreeError> {
        Self::open_with_expected_hash(path, Some(BSFREE_EXPECTED_SHA256))
    }

    /// Opens a catalogue against an optional expected SHA-256. Public only
    /// within the crate so sibling adapter tests (e.g. the BSFree GameCube
    /// bridge) can exercise the search/confirm path with a synthetic fixture.
    pub(crate) fn open_with_expected_hash(
        path: &Path,
        expected_hash: Option<&str>,
    ) -> Result<Self, BsFreeError> {
        let validation = validate_bsfree_database_with_hash(path, expected_hash)?;
        let connection = open_immutable_connection(path)?;
        Ok(Self {
            connection,
            fingerprint: validation.result.source_fingerprint,
        })
    }

    pub fn open_installed(paths: &BsFreePaths) -> Result<Self, BsFreeError> {
        let status = inspect_bsfree_source(paths)?;
        if !status.usable {
            return Err(BsFreeError::new(
                BsFreeErrorKind::NotInstalled,
                "BSFree source is not installed, enabled and validated",
            ));
        }
        let fingerprint = status.fingerprint.ok_or_else(|| {
            BsFreeError::new(
                BsFreeErrorKind::Validation,
                "BSFree source has no validated immutable fingerprint",
            )
        })?;
        if fingerprint.sha256 != BSFREE_EXPECTED_SHA256 {
            return Err(BsFreeError::new(
                BsFreeErrorKind::HashMismatch,
                "BSFree source metadata does not match the supported immutable database",
            ));
        }
        let size = fs::metadata(&paths.database)
            .map_err(|error| {
                BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string())
            })?
            .len();
        if size != fingerprint.size_bytes {
            return Err(BsFreeError::new(
                BsFreeErrorKind::Validation,
                "BSFree database size changed after validation; validate it again before browsing",
            ));
        }
        let connection = open_immutable_connection(&paths.database)?;
        validate_schema(&connection)?;
        Ok(Self {
            connection,
            fingerprint,
        })
    }

    #[must_use]
    pub fn fingerprint(&self) -> &ImmutableSourceFingerprint {
        &self.fingerprint
    }

    pub fn search_games(
        &self,
        request: &BsFreeGameSearchRequest,
    ) -> Result<BsFreeGameSearchResult, BsFreeError> {
        let page = request.page.bounded();
        let normalized_title = normalize_path_segment(&request.title);
        if normalized_title.is_empty() && request.upstream_game_id.is_none() {
            return Err(BsFreeError::new(
                BsFreeErrorKind::Query,
                "BSFree search requires a title or upstream game ID",
            ));
        }
        let system_ids = request
            .platform_id
            .as_deref()
            .map(system_ids_for_platform)
            .unwrap_or_else(all_mapped_system_ids);
        if request.platform_id.is_some() && system_ids.is_empty() {
            return Ok(BsFreeGameSearchResult {
                confidence: ProviderGameMatchConfidence::NoMatch,
                exact_revision_verified: false,
                explanation: "No BSFree system maps to the requested EmuWiz platform".to_string(),
                page: ProviderPage {
                    offset: page.offset,
                    limit: page.limit,
                    total: 0,
                    rows: Vec::new(),
                    has_more: false,
                },
            });
        }
        let title_probe = search_probe(&request.title);
        let mut where_parts = vec![format!(
            "g.system_id IN ({})",
            vec!["?"; system_ids.len()].join(",")
        )];
        let mut values = system_ids
            .iter()
            .map(|value| rusqlite::types::Value::Integer(*value))
            .collect::<Vec<_>>();
        if !title_probe.is_empty() {
            where_parts.push("lower(g.name) LIKE ?".to_string());
            values.push(rusqlite::types::Value::Text(format!("%{title_probe}%")));
        }
        if let Some(device_id) = request.device_id {
            where_parts.push("g.device_id = ?".to_string());
            values.push(rusqlite::types::Value::Integer(device_id));
        }
        if let Some(game_id) = request.upstream_game_id {
            where_parts.push("g.game_id = ?".to_string());
            values.push(rusqlite::types::Value::Integer(game_id));
        }
        let where_sql = where_parts.join(" AND ");
        let total_sql = format!("SELECT count(*) FROM games g WHERE {where_sql}");
        let total = self
            .connection
            .query_row(&total_sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0).map(|value| value.max(0) as u64)
            })
            .map_err(query_error)?;
        let sql = format!(
            "SELECT g.id,g.game_id,g.name,g.version,g.qty,s.id,s.name,s.group_id,d.id,d.name \
             FROM games g JOIN systems s ON s.id=g.system_id JOIN devices d ON d.id=g.device_id \
             WHERE {where_sql} ORDER BY lower(g.name),g.name,g.version,g.id LIMIT ? OFFSET ?"
        );
        let mut query_values = values;
        query_values.push(rusqlite::types::Value::Integer(i64::from(page.limit)));
        query_values.push(rusqlite::types::Value::Integer(i64::from(page.offset)));
        let mut statement = self.connection.prepare(&sql).map_err(query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(query_values.iter()), game_from_row)
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        let requested_version = request.version.as_deref().map(normalize_path_segment);
        for game in &mut rows {
            let title = normalize_path_segment(&game.name);
            let title_exact = !normalized_title.is_empty() && title == normalized_title;
            let title_probable = !normalized_title.is_empty()
                && (title.contains(&normalized_title) || normalized_title.contains(&title));
            let version_matches = requested_version.as_ref().is_none_or(|version| {
                game.version.as_deref().map(normalize_path_segment).as_ref() == Some(version)
            });
            game.match_confidence = Some(if title_exact && version_matches {
                ProviderGameMatchConfidence::ExactTitlePlatform
            } else if title_exact || title_probable {
                ProviderGameMatchConfidence::ProbableTitlePlatform
            } else {
                ProviderGameMatchConfidence::NoMatch
            });
            game.match_explanation = Some(
                "Match based on platform and title; exact game revision not verified".to_string(),
            );
        }
        rows.retain(|game| game.match_confidence != Some(ProviderGameMatchConfidence::NoMatch));
        let exact_count = rows
            .iter()
            .filter(|game| {
                game.match_confidence == Some(ProviderGameMatchConfidence::ExactTitlePlatform)
            })
            .count();
        let confidence = if exact_count > 1 {
            ProviderGameMatchConfidence::Ambiguous
        } else if exact_count == 1 {
            ProviderGameMatchConfidence::ExactTitlePlatform
        } else if rows.len() > 1 {
            ProviderGameMatchConfidence::Ambiguous
        } else if rows.len() == 1 {
            ProviderGameMatchConfidence::ProbableTitlePlatform
        } else {
            ProviderGameMatchConfidence::NoMatch
        };
        let returned = rows.len();
        Ok(BsFreeGameSearchResult {
            confidence,
            exact_revision_verified: false,
            explanation: "Match based on platform and title; exact game revision not verified"
                .to_string(),
            page: ProviderPage {
                offset: page.offset,
                limit: page.limit,
                total,
                rows,
                has_more: u64::from(page.offset).saturating_add(returned as u64) < total,
            },
        })
    }
}

impl ReadOnlyCheatCatalogue for BsFreeCatalogue {
    type System = BsFreeSystem;
    type Device = BsFreeDevice;
    type Game = BsFreeGame;
    type Cheat = BsFreeCheat;
    type Error = BsFreeError;

    fn identity(&self) -> CheatProviderIdentity {
        bsfree_provider_identity()
    }

    fn systems(&self, page: PageRequest) -> Result<ProviderPage<Self::System>, Self::Error> {
        let page = page.bounded();
        let total = self
            .connection
            .query_row("SELECT count(*) FROM systems", [], |row| {
                row.get::<_, i64>(0).map(|value| value.max(0) as u64)
            })
            .map_err(query_error)?;
        let mut statement = self
            .connection
            .prepare("SELECT id,group_id,name,qty FROM systems ORDER BY name,id LIMIT ? OFFSET ?")
            .map_err(query_error)?;
        let rows = statement
            .query_map(params![page.limit, page.offset], |row| {
                let id = row.get(0)?;
                let name: String = row.get(2)?;
                Ok(BsFreeSystem {
                    upstream_id: id,
                    group_id: row.get(1)?,
                    name: bounded_required_text(name, MAX_SHORT_TEXT, "system name")?,
                    cheat_count: row.get::<_, i64>(3)?.max(0) as u64,
                    mapping: bsfree_platform_mapping(id, &row.get::<_, String>(2)?),
                })
            })
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        Ok(provider_page(page, total, rows))
    }

    fn devices(&self, page: PageRequest) -> Result<ProviderPage<Self::Device>, Self::Error> {
        let page = page.bounded();
        let total = self
            .connection
            .query_row("SELECT count(*) FROM devices", [], |row| {
                row.get::<_, i64>(0).map(|value| value.max(0) as u64)
            })
            .map_err(query_error)?;
        let mut statement = self
            .connection
            .prepare("SELECT id,name,qty FROM devices ORDER BY name,id LIMIT ? OFFSET ?")
            .map_err(query_error)?;
        let rows = statement
            .query_map(params![page.limit, page.offset], |row| {
                let id = row.get(0)?;
                let name: String = row.get(1)?;
                Ok(BsFreeDevice {
                    upstream_id: id,
                    name: bounded_required_text(name.clone(), MAX_SHORT_TEXT, "device name")?,
                    cheat_count: row.get::<_, i64>(2)?.max(0) as u64,
                    mapping: bsfree_device_mapping(id, &name),
                })
            })
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        Ok(provider_page(page, total, rows))
    }

    fn game(&self, upstream_uid: i64) -> Result<Option<Self::Game>, Self::Error> {
        self.connection
            .query_row(
                "SELECT g.id,g.game_id,g.name,g.version,g.qty,s.id,s.name,s.group_id,d.id,d.name \
                 FROM games g JOIN systems s ON s.id=g.system_id JOIN devices d ON d.id=g.device_id \
                 WHERE g.id=?",
                [upstream_uid],
                game_from_row,
            )
            .optional()
            .map_err(query_error)
    }

    fn cheats(
        &self,
        upstream_uid: i64,
        page: PageRequest,
    ) -> Result<ProviderPage<Self::Cheat>, Self::Error> {
        let page = page.bounded();
        let total = self
            .connection
            .query_row(
                "SELECT count(*) FROM codes WHERE game_uid=?",
                [upstream_uid],
                |row| row.get::<_, i64>(0).map(|value| value.max(0) as u64),
            )
            .map_err(query_error)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT c.id,c.name,c.note,c.code,s.id,s.name,a.id,a.name,d.id,d.name \
                 FROM codes c LEFT JOIN sections s ON s.id=c.section_id \
                 LEFT JOIN authors a ON a.id=c.author_id JOIN devices d ON d.id=c.device_id \
                 WHERE c.game_uid=? ORDER BY c.id LIMIT ? OFFSET ?",
            )
            .map_err(query_error)?;
        let rows = statement
            .query_map(params![upstream_uid, page.limit, page.offset], |row| {
                let device_id = row.get(8)?;
                let device_name: String = row.get(9)?;
                let mut truncated_fields = Vec::new();
                let note =
                    bounded_optional_text(row.get(2)?, MAX_NOTE, "note", &mut truncated_fields);
                let code = bounded_text(row.get(3)?, MAX_CODE_BODY, "code", &mut truncated_fields);
                let compatibility = bsfree_device_mapping(device_id, &device_name).compatibility;
                Ok(BsFreeCheat {
                    upstream_id: row.get(0)?,
                    name: bounded_required_text(row.get(1)?, MAX_SHORT_TEXT, "cheat name")?,
                    note,
                    code,
                    section: optional_named_row(row.get(4)?, row.get(5)?)?,
                    author: optional_named_row(row.get(6)?, row.get(7)?)?,
                    device: BsFreeDeviceSummary {
                        upstream_id: device_id,
                        name: device_name,
                        compatibility,
                    },
                    compatibility,
                    truncated_fields,
                })
            })
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        Ok(provider_page(page, total, rows))
    }
}

pub fn bsfree_platform_mapping(upstream_id: i64, name: &str) -> ProviderPlatformMapping {
    let mapped = match name {
        "Playstation" | "Playstation (Japan)" => Some(("PSX", PlatformMappingStatus::Alias)),
        "Nintendo Entertainment System" => Some(("NES", PlatformMappingStatus::Exact)),
        "Nesten" | "FCE Ultra Debug NES" => Some(("NES", PlatformMappingStatus::Alias)),
        "Sega Genesis" | "Genesis" => Some(("MegaDrive", PlatformMappingStatus::Alias)),
        "Super Nintendo" => Some(("SNES", PlatformMappingStatus::Alias)),
        "Sega Game Gear" => Some(("GameGear", PlatformMappingStatus::Alias)),
        "Gameboy" => Some(("Game Boy", PlatformMappingStatus::Alias)),
        "Sega 32x" => Some(("Sega 32X", PlatformMappingStatus::Alias)),
        "Virtual Boy" => Some(("Virtual Boy", PlatformMappingStatus::Exact)),
        "PSP" => Some(("PSP", PlatformMappingStatus::Exact)),
        "Mame" => Some(("Arcade", PlatformMappingStatus::Alias)),
        "Sega Master System" => Some(("MasterSystem", PlatformMappingStatus::Alias)),
        "GameCube" | "GameCube (Japan)" | "GameCube (UK)" => {
            Some(("GameCube", PlatformMappingStatus::Alias))
        }
        "Nintendo 64" | "Nintendo 64 (Japan)" | "Nintendo 64 (UK)" => {
            Some(("N64", PlatformMappingStatus::Alias))
        }
        "Dreamcast" | "Dreamcast (UK)" => Some(("Dreamcast", PlatformMappingStatus::Alias)),
        "Sega Saturn" => Some(("Saturn", PlatformMappingStatus::Alias)),
        "Playstation 2"
        | "Playstation 2 (Version 1 and 2 - Europe)"
        | "Playstation 2 (Version 3)"
        | "Playstation 2 (United Kingdom)"
        | "Playstation 2 (United States)"
        | "Playstation 2 (Japan)"
        | "Playstation 2 (UK)" => Some(("PS2", PlatformMappingStatus::Alias)),
        "Gameboy Advance"
        | "Gameboy Advance (Version 1 and 2 - Japan)"
        | "Gameboy Advance (Version 1 and 2)"
        | "Gameboy Advance (Version 1 and 2 - Europe)"
        | "Gameboy Advance SP"
        | "Gameboy Advance (Japan)"
        | "Gameboy Advance (Europe)"
        | "Gameboy Advance (Version 3)" => Some(("Game Boy Advance", PlatformMappingStatus::Alias)),
        "Nintendo DS" | "Nintendo DS (PAL)" | "Nintendo DS (USA)" | "Nintendo DS (Japan)" => {
            Some(("Nintendo DS", PlatformMappingStatus::Alias))
        }
        "3D0" => Some(("3DO", PlatformMappingStatus::Alias)),
        // BSFree contains no Wii rows in the shipped snapshot, but the mapping
        // is name-based so any future database row whose system name is "Wii"
        // is recognised as the Wii platform rather than as unknown.
        "Wii" => Some(("Wii", PlatformMappingStatus::Exact)),
        _ => None,
    };
    let (archivefs_platform_id, archivefs_platform_display_name, status, explanation) = match mapped
    {
        Some((platform_id, status)) => match crate::platform::platform_by_id(platform_id) {
            Some(platform) => (
                Some(platform.id.to_string()),
                Some(platform.display_name.to_string()),
                status,
                format!(
                    "Explicit BSFree mapping from {name} to EmuWiz {} ({})",
                    platform.id, platform.display_name
                ),
            ),
            None => (
                None,
                None,
                PlatformMappingStatus::Unknown,
                format!(
                    "Explicit BSFree mapping target {platform_id} is absent from the canonical platform registry"
                ),
            ),
        },
        None => (
            None,
            None,
            PlatformMappingStatus::Unknown,
            "No explicit BSFree-to-EmuWiz platform mapping exists".to_string(),
        ),
    };
    ProviderPlatformMapping {
        upstream_id,
        upstream_name: name.to_string(),
        archivefs_platform_id,
        archivefs_platform_display_name,
        status,
        explanation,
    }
}

pub fn bsfree_device_mapping(upstream_id: i64, name: &str) -> ProviderDeviceMapping {
    let (compatibility, explanation) = match name {
        "Game Genie" | "Pro Action Replay" | "Action Replay" | "Xploder" | "GameShark"
        | "CodeBreaker" | "Action Replay Max" | "CWCheats" => (
            DeviceFormatCompatibility::PotentiallyConvertible,
            "Recognised device family, but BSFree Stage 1 performs no decoding, conversion or installation",
        ),
        "Game Busters" | "Red Dragon" | "GameGuru" => (
            DeviceFormatCompatibility::ReferenceOnly,
            "Historical device is browse-only; EmuWiz has no verified parser or installer for this exact format",
        ),
        _ => (
            DeviceFormatCompatibility::Unknown,
            "Unknown BSFree device; never treated as installable",
        ),
    };
    ProviderDeviceMapping {
        upstream_id,
        upstream_name: name.to_string(),
        compatibility,
        explanation: explanation.to_string(),
    }
}

fn all_mapped_system_ids() -> Vec<i64> {
    (1..=44)
        .filter(|id| {
            // The verified database IDs are sparse in meaning but contiguous
            // in this snapshot. Unknown future IDs remain queryable via
            // `systems`; search-by-canonical-platform never guesses them.
            verified_system_name(*id).is_some_and(|name| {
                bsfree_platform_mapping(*id, name)
                    .archivefs_platform_id
                    .is_some()
            })
        })
        .collect()
}

fn system_ids_for_platform(platform: &str) -> Vec<i64> {
    all_mapped_system_ids()
        .into_iter()
        .filter(|id| {
            verified_system_name(*id).is_some_and(|name| {
                bsfree_platform_mapping(*id, name)
                    .archivefs_platform_id
                    .as_deref()
                    == Some(platform)
            })
        })
        .collect()
}

fn verified_system_name(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "Playstation",
        2 => "Nintendo Entertainment System",
        3 => "Sega Genesis",
        4 => "Super Nintendo",
        5 => "Sega Game Gear",
        6 => "Gameboy",
        7 => "Sega 32x",
        8 => "Virtual Boy",
        9 => "PSP",
        10 => "Mame",
        11 => "Genesis",
        12 => "Sega Master System",
        13 => "Nesten",
        14 => "FCE Ultra Debug NES",
        15 => "GameCube (Japan)",
        16 => "GameCube (UK)",
        17 => "GameCube",
        18 => "Nintendo 64",
        19 => "Gameboy Advance (Version 1 and 2 - Japan)",
        20 => "Dreamcast",
        21 => "Playstation 2 (Version 1 and 2 - Europe)",
        22 => "Gameboy Advance (Version 1 and 2)",
        23 => "Sega Saturn",
        24 => "Gameboy Advance (Version 1 and 2 - Europe)",
        25 => "Playstation 2 (Version 3)",
        26 => "Gameboy Advance SP",
        27 => "Playstation 2",
        28 => "Gameboy Advance (Japan)",
        29 => "Gameboy Advance (Europe)",
        30 => "Gameboy Advance",
        31 => "Nintendo DS",
        32 => "Playstation 2 (United Kingdom)",
        33 => "Playstation 2 (United States)",
        34 => "Playstation (Japan)",
        35 => "Nintendo DS (PAL)",
        36 => "Nintendo 64 (Japan)",
        37 => "Playstation 2 (Japan)",
        38 => "Dreamcast (UK)",
        39 => "Nintendo 64 (UK)",
        40 => "Nintendo DS (USA)",
        41 => "Nintendo DS (Japan)",
        42 => "Playstation 2 (UK)",
        43 => "Gameboy Advance (Version 3)",
        44 => "3D0",
        _ => return None,
    })
}

fn game_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BsFreeGame> {
    let system_id = row.get(5)?;
    let system_name: String = row.get(6)?;
    let device_id = row.get(8)?;
    let device_name: String = row.get(9)?;
    let platform = bsfree_platform_mapping(system_id, &system_name);
    let device = bsfree_device_mapping(device_id, &device_name);
    Ok(BsFreeGame {
        upstream_uid: row.get(0)?,
        upstream_game_id: row.get(1)?,
        name: bounded_required_text(row.get(2)?, MAX_SHORT_TEXT, "game name")?,
        version: row.get(3)?,
        cheat_count: row.get::<_, i64>(4)?.max(0) as u64,
        system: BsFreeSystemSummary {
            upstream_id: system_id,
            name: system_name,
            archivefs_platform_id: platform.archivefs_platform_id,
            archivefs_platform_display_name: platform.archivefs_platform_display_name,
            mapping_status: platform.status,
        },
        device: BsFreeDeviceSummary {
            upstream_id: device_id,
            name: device_name,
            compatibility: device.compatibility,
        },
        match_confidence: None,
        match_explanation: None,
        revision_verified: false,
    })
}

fn provider_page<T>(page: PageRequest, total: u64, rows: Vec<T>) -> ProviderPage<T> {
    let returned = rows.len() as u64;
    ProviderPage {
        offset: page.offset,
        limit: page.limit,
        total,
        rows,
        has_more: u64::from(page.offset).saturating_add(returned) < total,
    }
}

fn search_probe(title: &str) -> String {
    title.trim().to_lowercase().chars().take(128).collect()
}

fn bounded_required_text(value: String, maximum: usize, field: &str) -> rusqlite::Result<String> {
    if value.len() > maximum {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            format!("BSFree {field} exceeds {maximum} bytes").into(),
        ));
    }
    Ok(value)
}

fn bounded_text(value: String, maximum: usize, field: &str, truncated: &mut Vec<String>) -> String {
    if value.len() <= maximum {
        return value;
    }
    truncated.push(field.to_string());
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn bounded_optional_text(
    value: Option<String>,
    maximum: usize,
    field: &str,
    truncated: &mut Vec<String>,
) -> Option<String> {
    value.map(|value| bounded_text(value, maximum, field, truncated))
}

fn optional_named_row(
    id: Option<i64>,
    name: Option<String>,
) -> rusqlite::Result<Option<BsFreeNamedRow>> {
    match (id, name) {
        (None, None) => Ok(None),
        (Some(id), Some(name)) => Ok(Some(BsFreeNamedRow {
            upstream_id: id,
            name: bounded_required_text(name, MAX_SHORT_TEXT, "related name")?,
        })),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn validate_schema(connection: &Connection) -> Result<(), BsFreeError> {
    let required: BTreeMap<&str, &[(&str, &str, bool, i64)]> = BTreeMap::from([
        (
            "authors",
            &[
                ("id", "INTEGER", true, 1),
                ("name", "TEXT", true, 0),
                ("qty", "INTEGER", true, 0),
            ][..],
        ),
        (
            "codes",
            &[
                ("id", "INTEGER", true, 1),
                ("name", "TEXT", true, 0),
                ("code", "TEXT", true, 0),
                ("note", "TEXT", false, 0),
                ("game_uid", "INTEGER", true, 0),
                ("game_id", "INTEGER", true, 0),
                ("system_id", "INTEGER", true, 0),
                ("device_id", "INTEGER", true, 0),
                ("section_id", "INTEGER", false, 0),
                ("author_id", "INTEGER", false, 0),
            ][..],
        ),
        (
            "devices",
            &[
                ("id", "INTEGER", true, 1),
                ("name", "TEXT", true, 0),
                ("qty", "INTEGER", true, 0),
            ][..],
        ),
        (
            "games",
            &[
                ("id", "INTEGER", true, 1),
                ("game_id", "INTEGER", true, 0),
                ("name", "TEXT", true, 0),
                ("version", "TEXT", false, 0),
                ("system_id", "INTEGER", true, 0),
                ("device_id", "INTEGER", true, 0),
                ("qty", "INTEGER", true, 0),
            ][..],
        ),
        (
            "sections",
            &[
                ("id", "INTEGER", true, 1),
                ("game_id", "INTEGER", true, 0),
                ("name", "TEXT", true, 0),
                ("qty", "INTEGER", true, 0),
            ][..],
        ),
        (
            "system_devices",
            &[
                ("system_id", "INTEGER", true, 1),
                ("device_id", "INTEGER", true, 2),
            ][..],
        ),
        (
            "systems",
            &[
                ("id", "INTEGER", true, 1),
                ("group_id", "INTEGER", true, 0),
                ("name", "TEXT", true, 0),
                ("qty", "INTEGER", true, 0),
            ][..],
        ),
    ]);
    for (table, columns) in required {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
                [table],
                |row| row.get(0),
            )
            .map_err(query_error)?;
        if !exists {
            return Err(BsFreeError::new(
                BsFreeErrorKind::UnsupportedSchema,
                format!("BSFree required table {table} is missing"),
            ));
        }
        let sql = format!("SELECT name,type,\"notnull\",pk FROM pragma_table_info('{table}')");
        let mut statement = connection.prepare(&sql).map_err(query_error)?;
        let actual = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)?,
                    ),
                ))
            })
            .map_err(query_error)?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(query_error)?;
        for (name, data_type, not_null, primary_key) in columns {
            match actual.get(*name) {
                Some((actual_type, actual_not_null, actual_primary_key))
                    if actual_type.eq_ignore_ascii_case(data_type)
                        && actual_not_null == not_null
                        && actual_primary_key == primary_key => {}
                Some(_) => {
                    return Err(BsFreeError::new(
                        BsFreeErrorKind::UnsupportedSchema,
                        format!("BSFree column {table}.{name} has an unsupported definition"),
                    ));
                }
                None => {
                    return Err(BsFreeError::new(
                        BsFreeErrorKind::UnsupportedSchema,
                        format!("BSFree required column {table}.{name} is missing"),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_relationships(connection: &Connection) -> Result<(), BsFreeError> {
    for (label, sql) in [
        (
            "games.system",
            "SELECT 1 FROM games g LEFT JOIN systems s ON s.id=g.system_id WHERE s.id IS NULL LIMIT 1",
        ),
        (
            "games.device",
            "SELECT 1 FROM games g LEFT JOIN devices d ON d.id=g.device_id WHERE d.id IS NULL LIMIT 1",
        ),
        (
            "codes.game",
            "SELECT 1 FROM codes c LEFT JOIN games g ON g.id=c.game_uid WHERE g.id IS NULL LIMIT 1",
        ),
        (
            "codes.system",
            "SELECT 1 FROM codes c LEFT JOIN systems s ON s.id=c.system_id WHERE s.id IS NULL LIMIT 1",
        ),
        (
            "codes.device",
            "SELECT 1 FROM codes c LEFT JOIN devices d ON d.id=c.device_id WHERE d.id IS NULL LIMIT 1",
        ),
        (
            "codes.section",
            "SELECT 1 FROM codes c LEFT JOIN sections s ON s.id=c.section_id WHERE c.section_id IS NOT NULL AND s.id IS NULL LIMIT 1",
        ),
        (
            "codes.author",
            "SELECT 1 FROM codes c LEFT JOIN authors a ON a.id=c.author_id WHERE c.author_id IS NOT NULL AND a.id IS NULL LIMIT 1",
        ),
    ] {
        let orphan = connection
            .query_row(sql, [], |_| Ok(true))
            .optional()
            .map_err(query_error)?
            .unwrap_or(false);
        if orphan {
            return Err(BsFreeError::new(
                BsFreeErrorKind::Validation,
                format!("BSFree relationship {label} contains orphaned rows"),
            ));
        }
    }
    Ok(())
}

fn read_counts(connection: &Connection) -> Result<BsFreeCounts, BsFreeError> {
    fn count(connection: &Connection, table: &str) -> Result<u64, BsFreeError> {
        let sql = format!("SELECT count(*) FROM {table}");
        connection
            .query_row(&sql, [], |row| {
                row.get::<_, i64>(0).map(|value| value.max(0) as u64)
            })
            .map_err(query_error)
    }
    Ok(BsFreeCounts {
        systems: count(connection, "systems")?,
        devices: count(connection, "devices")?,
        system_devices: count(connection, "system_devices")?,
        games: count(connection, "games")?,
        sections: count(connection, "sections")?,
        authors: count(connection, "authors")?,
        codes: count(connection, "codes")?,
    })
}

fn open_immutable_connection(path: &Path) -> Result<Connection, BsFreeError> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string()))?;
    let mut url = Url::from_file_path(&canonical).map_err(|()| {
        BsFreeError::new(
            BsFreeErrorKind::UnsafePath,
            "BSFree path cannot be encoded as a file URI",
        )
    })?;
    url.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let connection = Connection::open_with_flags(
        url.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(query_error)?;
    connection
        .execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;",
        )
        .map_err(query_error)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(query_error)?;
    Ok(connection)
}

fn validate_sqlite_header(path: &Path) -> Result<(), BsFreeError> {
    let mut file = open_regular_nofollow(path)?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header)
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::NotSqlite, error.to_string()))?;
    if &header != SQLITE_HEADER {
        return Err(BsFreeError::new(
            BsFreeErrorKind::NotSqlite,
            "selected file does not have a SQLite 3 header",
        ));
    }
    Ok(())
}

fn fingerprint_regular_file(path: &Path) -> Result<ImmutableSourceFingerprint, BsFreeError> {
    let mut file = open_regular_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string()))?;
    if metadata.len() > BSFREE_MAX_DATABASE_BYTES {
        return Err(BsFreeError::new(
            BsFreeErrorKind::DownloadTooLarge,
            "BSFree database exceeds the configured size limit",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string())
        })?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(ImmutableSourceFingerprint {
        sha256: hex_bytes(hash.finalize().as_slice()),
        size_bytes: metadata.len(),
    })
}

fn validate_download_url(value: &str, initial: bool) -> Result<(), BsFreeError> {
    if initial && value != BSFREE_DATABASE_URL {
        return Err(BsFreeError::new(
            BsFreeErrorKind::RedirectRejected,
            "BSFree download must begin at the compiled documented URL",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        BsFreeError::new(BsFreeErrorKind::RedirectRejected, "BSFree URL is invalid")
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some(BSFREE_DOWNLOAD_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(BsFreeError::new(
            BsFreeErrorKind::RedirectRejected,
            "BSFree URL must remain HTTPS on the documented public host",
        ));
    }
    Ok(())
}

fn prepare_source_root(paths: &BsFreePaths) -> Result<(), BsFreeError> {
    if let Some(parent) = paths.root.parent() {
        ensure_no_symlink_components(parent)?;
    }
    match fs::symlink_metadata(&paths.root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            BsFreeError::new(BsFreeErrorKind::UnsafePath, "BSFree source root is unsafe"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&paths.root).map_err(cache_write_error)
        }
        Err(error) => Err(cache_write_error(error)),
    }
}

fn ensure_no_symlink_components(path: &Path) -> Result<(), BsFreeError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BsFreeError::new(
                    BsFreeErrorKind::UnsafePath,
                    format!("symlinked path component rejected: {}", current.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(cache_write_error(error)),
        }
    }
    Ok(())
}

fn safe_regular_file_if_present(path: &Path) -> Result<bool, BsFreeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(BsFreeError::new(
                BsFreeErrorKind::UnsafePath,
                format!("unsafe BSFree cache path {}", path.display()),
            ))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(cache_write_error(error)),
    }
}

fn open_regular_nofollow(path: &Path) -> Result<File, BsFreeError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string()))?;
    if !file.metadata().map_err(cache_write_error)?.is_file() {
        return Err(BsFreeError::new(
            BsFreeErrorKind::UnsafePath,
            "BSFree source must be a regular file",
        ));
    }
    Ok(file)
}

fn create_new_nofollow(path: &Path) -> Result<File, BsFreeError> {
    let _ = fs::remove_file(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path).map_err(cache_write_error)
}

fn copy_bounded(input: &mut File, destination: &Path, maximum: u64) -> Result<(), BsFreeError> {
    let mut output = create_new_nofollow(destination)?;
    let mut total = 0u64;
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            BsFreeError::new(BsFreeErrorKind::SourceUnreadable, error.to_string())
        })?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > maximum {
            return Err(BsFreeError::new(
                BsFreeErrorKind::DownloadTooLarge,
                "BSFree input exceeds the configured size limit",
            ));
        }
        output
            .write_all(&buffer[..read])
            .map_err(cache_write_error)?;
    }
    output.sync_all().map_err(cache_write_error)
}

fn staging_path(paths: &BsFreePaths, label: &str) -> PathBuf {
    paths.root.join(format!(
        ".{label}-{}-{}.tmp",
        std::process::id(),
        now_unix_seconds()
    ))
}

fn publish_validated_database(
    paths: &BsFreePaths,
    staging: &Path,
    mut validation: BsFreeValidation,
) -> Result<(), BsFreeError> {
    validation.database_path = paths.database.clone();
    fs::rename(staging, &paths.database).map_err(cache_write_error)?;
    atomic_write(
        &paths.hash,
        format!(
            "{}  {}\n",
            validation.result.source_fingerprint.sha256, BSFREE_DATABASE_FILE
        )
        .as_bytes(),
    )?;
    atomic_write_json(&paths.validation, &validation)?;
    activate_validation_metadata(paths, validation)
}

fn activate_validation_metadata(
    paths: &BsFreePaths,
    validation: BsFreeValidation,
) -> Result<(), BsFreeError> {
    update_metadata_state(
        paths,
        CheatProviderSourceState::Ready,
        Some(validation),
        None,
    )
}

fn update_metadata_state(
    paths: &BsFreePaths,
    state: CheatProviderSourceState,
    validation: Option<BsFreeValidation>,
    error: Option<BsFreeError>,
) -> Result<(), BsFreeError> {
    prepare_source_root(paths)?;
    let mut metadata = read_metadata(paths)?.unwrap_or_default();
    metadata.state = state;
    metadata.last_operation_at_unix_seconds = now_unix_seconds();
    if validation.is_some() {
        metadata.validation = validation;
    }
    metadata.last_error = error;
    write_metadata(paths, &metadata)
}

fn state_for_validation_error(error: &BsFreeError) -> CheatProviderSourceState {
    match error.kind {
        BsFreeErrorKind::UnsupportedSchema => CheatProviderSourceState::UnsupportedSchema,
        BsFreeErrorKind::NotSqlite => CheatProviderSourceState::Invalid,
        _ => CheatProviderSourceState::ValidationFailed,
    }
}

fn read_metadata(paths: &BsFreePaths) -> Result<Option<BsFreeSourceMetadata>, BsFreeError> {
    if !safe_regular_file_if_present(&paths.metadata)? {
        return Ok(None);
    }
    let mut file = open_regular_nofollow(&paths.metadata)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(cache_write_error)?;
    let metadata: BsFreeSourceMetadata = serde_json::from_slice(&bytes)
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::Validation, error.to_string()))?;
    if metadata.format_version != BSFREE_PROVIDER_FORMAT_VERSION
        || metadata.provider_id != BSFREE_PROVIDER_ID
    {
        return Err(BsFreeError::new(
            BsFreeErrorKind::UnsupportedSchema,
            "BSFree source metadata version or provider binding is unsupported",
        ));
    }
    Ok(Some(metadata))
}

fn write_metadata(paths: &BsFreePaths, metadata: &BsFreeSourceMetadata) -> Result<(), BsFreeError> {
    atomic_write_json(&paths.metadata, metadata)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), BsFreeError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| BsFreeError::new(BsFreeErrorKind::CacheWrite, error.to_string()))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), BsFreeError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(BsFreeError::new(
            BsFreeErrorKind::UnsafePath,
            format!("unsafe BSFree destination {}", path.display()),
        ));
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut output = create_new_nofollow(&temporary)?;
    output.write_all(bytes).map_err(cache_write_error)?;
    output.sync_all().map_err(cache_write_error)?;
    drop(output);
    fs::rename(&temporary, path).map_err(cache_write_error)
}

fn query_error(error: rusqlite::Error) -> BsFreeError {
    BsFreeError::new(BsFreeErrorKind::Query, error.to_string())
}

fn cache_write_error(error: std::io::Error) -> BsFreeError {
    BsFreeError::new(BsFreeErrorKind::CacheWrite, error.to_string())
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex_bytes(Sha256::digest(bytes).as_slice())
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::patch_manager::{
        CHEAT_SOURCE_RESULT_SCHEMA_VERSION, CheatSourceError, CheatSourceErrorStage,
        CheatSourceHttpResponse,
    };

    static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

    fn fixture_root(label: &str) -> PathBuf {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "archivefs-bsfree-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn create_fixture(path: &Path) {
        create_fixture_variant(path, true, true);
    }

    fn create_fixture_variant(path: &Path, include_authors: bool, include_note: bool) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys=OFF;")
            .unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE systems(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,group_id INTEGER NOT NULL,name TEXT NOT NULL,qty INTEGER NOT NULL DEFAULT 0);\
                 CREATE TABLE devices(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,qty INTEGER NOT NULL DEFAULT 0);\
                 CREATE TABLE system_devices(system_id INTEGER NOT NULL REFERENCES systems,device_id INTEGER NOT NULL REFERENCES devices,PRIMARY KEY(system_id,device_id));\
                 CREATE TABLE games(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,game_id INTEGER NOT NULL,name TEXT NOT NULL,version TEXT DEFAULT NULL,system_id INTEGER NOT NULL REFERENCES systems,device_id INTEGER NOT NULL REFERENCES devices,qty INTEGER NOT NULL DEFAULT 0);\
                 CREATE TABLE sections(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,game_id INTEGER NOT NULL REFERENCES games(game_id),name TEXT NOT NULL,qty INTEGER NOT NULL DEFAULT 0);\
                 {}\
                 CREATE TABLE codes(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,code TEXT NOT NULL,{} game_uid INTEGER NOT NULL REFERENCES games,game_id INTEGER NOT NULL REFERENCES games(game_id),system_id INTEGER NOT NULL REFERENCES systems,device_id INTEGER NOT NULL REFERENCES devices,section_id INTEGER DEFAULT NULL REFERENCES sections,author_id INTEGER DEFAULT NULL REFERENCES authors);",
                if include_authors {
                    "CREATE TABLE authors(id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,qty INTEGER NOT NULL DEFAULT 0);"
                } else {
                    ""
                },
                if include_note { "note TEXT DEFAULT NULL," } else { "" }
            ))
            .unwrap();
        if !include_authors || !include_note {
            return;
        }
        connection
            .execute_batch(
                "INSERT INTO systems(id,group_id,name,qty) VALUES(2,2,'Nintendo Entertainment System',3),(3,3,'Sega Genesis',1),(999,999,'Mystery System',1);\
                 INSERT INTO devices(id,name,qty) VALUES(2,'Game Genie',3),(99,'Mystery Device',2);\
                 INSERT INTO system_devices VALUES(2,2),(3,99),(999,99);\
                 INSERT INTO games(id,game_id,name,version,system_id,device_id,qty) VALUES
                   (1,100,'Super Mario Bros.','USA',2,2,2),
                   (2,101,'Sonic Test',NULL,3,99,1),
                   (3,102,'Unknown Game',NULL,999,99,1),
                   (4,103,'Super Mario Bros.','Rev A',2,2,1);\
                 INSERT INTO sections(id,game_id,name,qty) VALUES(1,100,'Player',1);\
                 INSERT INTO authors(id,name,qty) VALUES(1,'Community Author',1);\
                 INSERT INTO codes(id,name,code,note,game_uid,game_id,system_id,device_id,section_id,author_id) VALUES
                   (1,'Infinite lives','AAAA-BBBB','A note',1,100,2,2,1,1),
                   (2,'No section','CCCC-DDDD',NULL,1,100,2,2,NULL,NULL),
                   (3,'Sonic code','1234',NULL,2,101,3,99,NULL,NULL),
                   (4,'Unknown code','5678',NULL,3,102,999,99,NULL,NULL),
                   (5,'Other revision','EEEE-FFFF',NULL,4,103,2,2,NULL,NULL);",
            )
            .unwrap();
    }

    fn fixture_catalogue(path: &Path) -> BsFreeCatalogue {
        BsFreeCatalogue::open_with_expected_hash(path, None).unwrap()
    }

    type FakeReply = (u16, Option<String>, Vec<u8>);

    #[derive(Default)]
    struct FakeTransport {
        replies: RefCell<Vec<FakeReply>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeTransport {
        fn with(replies: Vec<FakeReply>) -> Self {
            Self {
                replies: RefCell::new(replies.into_iter().rev().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CheatSourceTransport for FakeTransport {
        fn get(
            &self,
            url: &str,
            _maximum_bytes: u64,
            destination: &mut dyn Write,
            context: CheatSourceTransferContext<'_>,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            self.calls.borrow_mut().push(url.to_string());
            if context
                .cancellation
                .is_some_and(CheatSourceCancellation::is_cancelled)
            {
                return Err(CheatSourceError {
                    schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
                    stage: CheatSourceErrorStage::Download,
                    code: "cancelled".to_string(),
                    message: "cancelled".to_string(),
                    retry_after_seconds: None,
                });
            }
            let (status, location, bytes) = self.replies.borrow_mut().pop().unwrap();
            if (200..300).contains(&status) {
                destination.write_all(&bytes).unwrap();
            }
            Ok(CheatSourceHttpResponse {
                status,
                content_type: Some("application/octet-stream".to_string()),
                content_encoding: None,
                content_length: Some(bytes.len() as u64),
                location,
                etag: None,
                last_modified: None,
                downloaded_bytes: bytes.len() as u64,
                retry_after_seconds: None,
            })
        }
    }

    #[test]
    fn valid_real_shape_schema_is_accepted_and_arbitrary_schema_is_rejected() {
        let root = fixture_root("schema");
        let valid = root.join("valid.db");
        create_fixture(&valid);
        let report = validate_bsfree_database_with_hash(&valid, None).unwrap();
        assert_eq!(report.counts.codes, 5);
        assert!(report.opened_read_only && report.query_only);

        let arbitrary = root.join("arbitrary.db");
        let connection = Connection::open(&arbitrary).unwrap();
        connection
            .execute("CREATE TABLE unrelated(value TEXT)", [])
            .unwrap();
        drop(connection);
        assert_eq!(
            validate_bsfree_database_with_hash(&arbitrary, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::UnsupportedSchema
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_required_table_and_column_are_rejected() {
        let root = fixture_root("missing-schema");
        let missing_table = root.join("missing-table.db");
        create_fixture_variant(&missing_table, false, true);
        assert_eq!(
            validate_bsfree_database_with_hash(&missing_table, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::UnsupportedSchema
        );
        let missing_column = root.join("missing-column.db");
        create_fixture_variant(&missing_column, true, false);
        assert_eq!(
            validate_bsfree_database_with_hash(&missing_column, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::UnsupportedSchema
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn non_sqlite_and_wrong_hash_never_activate() {
        let root = fixture_root("invalid");
        let source = root.join("not.db");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"not sqlite").unwrap();
        assert_eq!(
            validate_bsfree_database_with_hash(&source, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::NotSqlite
        );
        let fixture = root.join("fixture.db");
        create_fixture(&fixture);
        let paths = BsFreePaths::at(root.join("owned"));
        assert_eq!(
            import_local_bsfree_database(&paths, &fixture)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::HashMismatch
        );
        assert!(!paths.database.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_queries_create_no_sidecars_and_leave_bytes_identical() {
        let root = fixture_root("readonly");
        let database = root.join("fixture.db");
        create_fixture(&database);
        let before = fs::read(&database).unwrap();
        let before_entries = fs::read_dir(&root).unwrap().count();
        let catalogue = fixture_catalogue(&database);
        assert_eq!(
            catalogue
                .systems(PageRequest {
                    offset: 0,
                    limit: 5
                })
                .unwrap()
                .rows
                .len(),
            3
        );
        assert!(
            catalogue
                .connection
                .execute("DELETE FROM codes", [])
                .is_err()
        );
        drop(catalogue);
        assert_eq!(fs::read(&database).unwrap(), before);
        assert_eq!(fs::read_dir(&root).unwrap().count(), before_entries);
        assert!(!database.with_extension("db-wal").exists());
        assert!(!database.with_extension("db-shm").exists());
        assert!(!database.with_extension("db-journal").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn systems_games_and_codes_are_paginated_and_hard_bounded() {
        let root = fixture_root("pages");
        let database = root.join("fixture.db");
        create_fixture(&database);
        let catalogue = fixture_catalogue(&database);
        let huge = PageRequest {
            offset: 0,
            limit: u16::MAX,
        };
        assert_eq!(
            catalogue.systems(huge).unwrap().limit,
            PageRequest::HARD_LIMIT
        );
        assert_eq!(
            catalogue.devices(huge).unwrap().limit,
            PageRequest::HARD_LIMIT
        );
        assert_eq!(
            catalogue
                .cheats(
                    1,
                    PageRequest {
                        offset: 0,
                        limit: 1
                    }
                )
                .unwrap()
                .rows
                .len(),
            1
        );
        let second = catalogue
            .cheats(
                1,
                PageRequest {
                    offset: 1,
                    limit: 1,
                },
            )
            .unwrap();
        assert_eq!(second.rows.len(), 1);
        assert!(!second.has_more);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sections_authors_and_nullable_values_map_without_invention() {
        let root = fixture_root("relations");
        let database = root.join("fixture.db");
        create_fixture(&database);
        let catalogue = fixture_catalogue(&database);
        let rows = catalogue.cheats(1, PageRequest::cheats(0)).unwrap().rows;
        assert_eq!(rows[0].section.as_ref().unwrap().upstream_id, 1);
        assert_eq!(rows[0].author.as_ref().unwrap().name, "Community Author");
        assert!(rows[1].note.is_none());
        assert!(rows[1].section.is_none());
        assert!(rows[1].author.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_code_and_note_fields_are_bounded_and_marked() {
        let root = fixture_root("oversized");
        let database = root.join("fixture.db");
        create_fixture(&database);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE codes SET code=?,note=? WHERE id=1",
                params!["X".repeat(MAX_CODE_BODY + 10), "Y".repeat(MAX_NOTE + 10)],
            )
            .unwrap();
        drop(connection);
        let catalogue = fixture_catalogue(&database);
        let cheat = catalogue
            .cheats(1, PageRequest::cheats(0))
            .unwrap()
            .rows
            .remove(0);
        assert_eq!(cheat.code.len(), MAX_CODE_BODY);
        assert_eq!(cheat.note.unwrap().len(), MAX_NOTE);
        assert_eq!(cheat.truncated_fields, ["note", "code"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn platform_and_device_mappings_are_explicit_and_unknowns_remain_visible() {
        let nes = bsfree_platform_mapping(2, "Nintendo Entertainment System");
        assert_eq!(nes.archivefs_platform_id.as_deref(), Some("NES"));
        assert_eq!(
            nes.archivefs_platform_display_name.as_deref(),
            Some("Nintendo Entertainment System")
        );
        let unknown = bsfree_platform_mapping(999, "Mystery System");
        assert_eq!(unknown.status, PlatformMappingStatus::Unknown);
        assert!(unknown.archivefs_platform_id.is_none());
        assert!(unknown.archivefs_platform_display_name.is_none());
        assert_eq!(
            bsfree_device_mapping(2, "Game Genie").compatibility,
            DeviceFormatCompatibility::PotentiallyConvertible
        );
        assert_eq!(
            bsfree_device_mapping(99, "Mystery Device").compatibility,
            DeviceFormatCompatibility::Unknown
        );
        assert_ne!(
            bsfree_device_mapping(6, "Action Replay").compatibility,
            DeviceFormatCompatibility::DirectlyInstallable
        );
    }

    #[test]
    fn all_verified_bsfree_targets_resolve_through_the_one_canonical_registry() {
        assert_eq!(crate::platform::canonical_ids().len(), 74);
        for upstream_id in 1..=44 {
            let upstream_name = verified_system_name(upstream_id).unwrap();
            let mapping = bsfree_platform_mapping(upstream_id, upstream_name);
            let canonical_id = mapping.archivefs_platform_id.as_deref().unwrap_or_else(|| {
                panic!("BSFree system {upstream_id} {upstream_name:?} has no canonical target")
            });
            let platform = crate::platform::platform_by_id(canonical_id).unwrap_or_else(|| {
                panic!("BSFree target {canonical_id:?} is absent from the canonical registry")
            });
            assert_eq!(
                mapping.archivefs_platform_display_name.as_deref(),
                Some(platform.display_name)
            );
        }
        let provider_source = include_str!("bsfree.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!provider_source.contains("const PLATFORMS"));
    }

    #[test]
    fn all_verified_devices_remain_browse_only_and_none_are_directly_installable() {
        let devices = [
            (1, "Game Busters"),
            (2, "Game Genie"),
            (3, "Red Dragon"),
            (4, "CWCheats"),
            (5, "Pro Action Replay"),
            (6, "Action Replay"),
            (7, "Xploder"),
            (8, "GameShark"),
            (9, "CodeBreaker"),
            (10, "Action Replay Max"),
            (11, "GameGuru"),
        ];
        assert_eq!(devices.len(), 11);
        assert!(devices.iter().all(|(id, name)| {
            bsfree_device_mapping(*id, name).compatibility
                != DeviceFormatCompatibility::DirectlyInstallable
        }));
    }

    #[test]
    fn title_platform_matching_is_conservative_and_multiple_rows_are_ambiguous() {
        let root = fixture_root("matching");
        let database = root.join("fixture.db");
        create_fixture(&database);
        let catalogue = fixture_catalogue(&database);
        let result = catalogue
            .search_games(&BsFreeGameSearchRequest {
                platform_id: Some("NES".to_string()),
                title: "Super Mario Bros.".to_string(),
                version: None,
                device_id: None,
                upstream_game_id: None,
                page: PageRequest::games(0),
            })
            .unwrap();
        assert_eq!(result.confidence, ProviderGameMatchConfidence::Ambiguous);
        assert_eq!(result.page.rows.len(), 2);
        assert!(result.page.rows.iter().all(|game| !game.revision_verified));
        assert!(result.explanation.contains("revision not verified"));

        let none = catalogue
            .search_games(&BsFreeGameSearchRequest {
                platform_id: Some("GameCube".to_string()),
                title: "Super Mario Bros.".to_string(),
                version: None,
                device_id: None,
                upstream_game_id: None,
                page: PageRequest::games(0),
            })
            .unwrap();
        assert_eq!(none.confidence, ProviderGameMatchConfidence::NoMatch);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_import_copies_atomically_and_never_changes_original() {
        let root = fixture_root("import");
        let source = root.join("selected.db");
        create_fixture(&source);
        let original = fs::read(&source).unwrap();
        let paths = BsFreePaths::at(root.join("owned"));
        let result = import_local_with_expected_hash(&paths, &source, None).unwrap();
        assert!(result.status.usable);
        assert!(!result.network_used);
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(&paths.database).unwrap(), original);
        assert!(!paths.root.join("derived-index.sqlite").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn symlink_local_import_is_rejected() {
        let root = fixture_root("symlink");
        let source = root.join("selected.db");
        create_fixture(&source);
        let link = root.join("linked.db");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let paths = BsFreePaths::at(root.join("owned"));
        assert!(matches!(
            import_local_with_expected_hash(&paths, &link, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::SourceUnreadable | BsFreeErrorKind::UnsafePath
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_update_preserves_previous_known_good_database() {
        let root = fixture_root("preserve");
        let source = root.join("selected.db");
        create_fixture(&source);
        let paths = BsFreePaths::at(root.join("owned"));
        import_local_with_expected_hash(&paths, &source, None).unwrap();
        let before = fs::read(&paths.database).unwrap();
        let transport = FakeTransport::with(vec![(200, None, b"not sqlite".to_vec())]);
        assert!(
            download_with_expected_hash(
                &paths,
                &BsFreeDownloadOptions::default(),
                &transport,
                None
            )
            .is_err()
        );
        assert_eq!(fs::read(&paths.database).unwrap(), before);
        assert!(inspect_bsfree_source(&paths).unwrap().usable);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn redirect_limit_and_private_redirect_are_rejected() {
        let root = fixture_root("redirects");
        let paths = BsFreePaths::at(root.join("owned"));
        let private = FakeTransport::with(vec![(
            302,
            Some("https://127.0.0.1/bsfree.db".to_string()),
            Vec::new(),
        )]);
        assert_eq!(
            download_with_expected_hash(&paths, &BsFreeDownloadOptions::default(), &private, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::RedirectRejected
        );

        let redirects = (0..=BSFREE_REDIRECT_LIMIT)
            .map(|index| {
                (
                    302,
                    Some(format!("https://{BSFREE_DOWNLOAD_HOST}/redirect-{index}")),
                    Vec::new(),
                )
            })
            .collect();
        let transport = FakeTransport::with(redirects);
        assert_eq!(
            download_with_expected_hash(
                &paths,
                &BsFreeDownloadOptions::default(),
                &transport,
                None
            )
            .unwrap_err()
            .kind,
            BsFreeErrorKind::RedirectRejected
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_leaves_an_existing_source_usable() {
        let root = fixture_root("cancel");
        let source = root.join("selected.db");
        create_fixture(&source);
        let paths = BsFreePaths::at(root.join("owned"));
        import_local_with_expected_hash(&paths, &source, None).unwrap();
        let cancellation = CheatSourceCancellation::default();
        cancellation.cancel();
        let options = BsFreeDownloadOptions {
            cancellation: Some(cancellation),
            ..BsFreeDownloadOptions::default()
        };
        let transport = FakeTransport::with(Vec::new());
        assert_eq!(
            download_with_expected_hash(&paths, &options, &transport, None)
                .unwrap_err()
                .kind,
            BsFreeErrorKind::Cancelled
        );
        assert!(inspect_bsfree_source(&paths).unwrap().usable);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removal_requires_confirmation_and_cannot_touch_emulator_profiles() {
        let root = fixture_root("remove");
        let source = root.join("selected.db");
        create_fixture(&source);
        let paths = BsFreePaths::at(root.join("owned-bsfree"));
        import_local_with_expected_hash(&paths, &source, None).unwrap();
        let emulator = root.join("Dolphin/User/GameSettings/TEST.ini");
        fs::create_dir_all(emulator.parent().unwrap()).unwrap();
        fs::write(&emulator, b"user content").unwrap();
        assert_eq!(
            remove_local_bsfree_source(&paths, false).unwrap_err().kind,
            BsFreeErrorKind::ConfirmationRequired
        );
        remove_local_bsfree_source(&paths, true).unwrap();
        assert_eq!(fs::read(&emulator).unwrap(), b"user content");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_has_no_installation_capability_or_background_network_api() {
        let source = include_str!("bsfree.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "install_bsfree",
            "stage_bsfree",
            "convert_bsfree",
            "schedule_bsfree",
        ] {
            assert!(!source.contains(forbidden));
        }
        assert!(source.contains("pub fn download_bsfree_database"));
        assert!(!source.contains("pub fn download_bsfree_database_from_url"));
    }
}
