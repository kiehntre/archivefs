//! Immutable, browse-only adapter for the historical CheatBase SQLite source.
//!
//! Stage 1 intentionally exposes discovery, identity lookup, and bounded
//! browsing only. It has no conversion, installation, emulator-write, or ROM-
//! write API. The upstream SQLite file is always opened read-only with
//! `immutable=1`; ArchiveFS metadata is stored beside its owned copy.

use std::collections::BTreeMap;
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

pub const CHEATBASE_PROVIDER_FORMAT_VERSION: u32 = 1;
pub const CHEATBASE_PROVIDER_ID: &str = "cheatbase";
pub const CHEATBASE_DATABASE_FILE: &str = "cheatbase.sqlite";
pub const CHEATBASE_UPSTREAM_PROJECT: &str = "https://github.com/CheatBase/CheatBase";
pub const CHEATBASE_UPSTREAM_COMMIT: &str = "5894b60d58804d66e15c6b86b062b72d32163391";
pub const CHEATBASE_DATABASE_URL: &str = "https://raw.githubusercontent.com/CheatBase/CheatBase/5894b60d58804d66e15c6b86b062b72d32163391/cheatbase.sqlite";
pub const CHEATBASE_DOWNLOAD_HOST: &str = "raw.githubusercontent.com";
pub const CHEATBASE_EXPECTED_SHA256: &str =
    "917f16ce55afa1a21cfdd106239cbaa65317cef21e47152b6471eb5c017a76e3";
pub const CHEATBASE_EXPECTED_SIZE_BYTES: u64 = 67_366_912;
pub const CHEATBASE_MAX_DATABASE_BYTES: u64 = 128 * 1024 * 1024;
const SOURCE_DIRECTORY: &str = "cheatbase";
const SOURCE_METADATA_FILE: &str = "source.json";
const SOURCE_HASH_FILE: &str = "cheatbase.sqlite.sha256";
const VALIDATION_FILE: &str = "last-validation.json";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const HASH_CHUNK: usize = 128 * 1024;
const MAX_TITLE: usize = 512;
const MAX_SHORT_TEXT: usize = 1024;
const MAX_NOTE: usize = 4096;
const MAX_CODE_BODY: usize = 16 * 1024;
const SCHEMA_DESCRIPTION: &str = "cheatbase-v1:ROMS(romID,systemID,regionID,romHashCRC,romHashMD5,romHashSHA1,romSize,romFileName,romExtensionlessFileName,romParent,romSerial,romHeader,romLanguage,romDumpSource,lastModified);RELEASES(releaseID,romID,releaseTitleName,regionLocalizedID,releaseCoverFront,releaseCoverBack,releaseCoverCart,releaseCoverDisc,releaseDescription,releaseDeveloper,releasePublisher,releaseGenre,releaseDate,releaseReferenceURL,releaseReferenceImageURL,lastModified);REGIONS(regionID,regionName,lastModified);SYSTEMS(systemID,systemName,systemShortName,systemHeaderSizeBytes,systemHashless,systemHeader,systemSerial,systemOEID,lastModified);CHEATS(cheatID,romID,cheatName,cheatActivation,cheatDescription,cheatSideEffect,cheatFolderName,cheatCategoryID,cheatCode,cheatDeviceID,cheatCredit,lastModified);CHEAT_DEVICES(cheatDeviceID,systemID,cheatDeviceName,cheatDeviceBrandName,cheatDeviceFormat,lastModified);CHEAT_CATEGORIES(cheatCategoryID,cheatCategory,cheatCategoryDescription,lastModified)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatBaseErrorKind {
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
    InvalidIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseError {
    pub kind: CheatBaseErrorKind,
    pub message: String,
}

impl CheatBaseError {
    fn new(kind: CheatBaseErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CheatBaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CheatBaseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheatBasePaths {
    pub root: PathBuf,
    pub database: PathBuf,
    pub metadata: PathBuf,
    pub hash: PathBuf,
    pub validation: PathBuf,
}

impl CheatBasePaths {
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            database: root.join(CHEATBASE_DATABASE_FILE),
            metadata: root.join(SOURCE_METADATA_FILE),
            hash: root.join(SOURCE_HASH_FILE),
            validation: root.join(VALIDATION_FILE),
            root,
        }
    }
}

pub fn default_cheatbase_source_root() -> Result<PathBuf, CheatBaseError> {
    let database = default_database_path()
        .map_err(|error| CheatBaseError::new(CheatBaseErrorKind::UnsafePath, error.to_string()))?;
    Ok(database
        .parent()
        .expect("database path has a parent")
        .join("cheat-sources")
        .join(SOURCE_DIRECTORY))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseCounts {
    pub systems: u64,
    pub regions: u64,
    pub roms: u64,
    pub releases: u64,
    pub devices: u64,
    pub categories: u64,
    pub cheats: u64,
    pub systems_with_cheats: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseValidation {
    pub result: ProviderValidationResult,
    pub counts: CheatBaseCounts,
    pub sqlite_version: String,
    pub database_path: PathBuf,
    pub opened_read_only: bool,
    pub immutable: bool,
    pub query_only: bool,
    pub upstream_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CheatBaseSourceMetadata {
    format_version: u32,
    provider_id: String,
    enabled: bool,
    state: CheatProviderSourceState,
    last_operation_at_unix_seconds: u64,
    validation: Option<CheatBaseValidation>,
    last_error: Option<CheatBaseError>,
}

impl Default for CheatBaseSourceMetadata {
    fn default() -> Self {
        Self {
            format_version: CHEATBASE_PROVIDER_FORMAT_VERSION,
            provider_id: CHEATBASE_PROVIDER_ID.to_string(),
            enabled: true,
            state: CheatProviderSourceState::NotInstalled,
            last_operation_at_unix_seconds: now_unix_seconds(),
            validation: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseSourceStatus {
    pub format_version: u32,
    pub provider: CheatProviderIdentity,
    pub state: CheatProviderSourceState,
    pub enabled: bool,
    pub usable: bool,
    pub database_path: PathBuf,
    pub fingerprint: Option<ImmutableSourceFingerprint>,
    pub validation: Option<CheatBaseValidation>,
    pub last_error: Option<CheatBaseError>,
    pub provenance: CheatProviderProvenance,
    pub licence: CheatProviderLicence,
    pub browse_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseSystem {
    pub upstream_id: i64,
    pub name: String,
    pub short_name: String,
    pub rom_count: u64,
    pub cheat_count: u64,
    pub mapping: ProviderPlatformMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseDevice {
    pub upstream_id: i64,
    pub system_id: i64,
    pub name: String,
    pub brand: Option<String>,
    pub format: String,
    pub cheat_count: u64,
    pub mapping: ProviderDeviceMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseGame {
    pub upstream_release_id: i64,
    pub upstream_rom_id: i64,
    pub title: String,
    pub platform: ProviderPlatformMapping,
    pub upstream_system_name: String,
    pub rom_region: String,
    pub release_region: String,
    pub serial: Option<String>,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub rom_size: Option<u64>,
    pub release_date: Option<String>,
    pub cheat_count: u64,
    pub match_confidence: Option<ProviderGameMatchConfidence>,
    pub match_evidence: Vec<String>,
    pub revision_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseCheat {
    pub upstream_id: i64,
    pub name: String,
    pub activation: Option<String>,
    pub description: Option<String>,
    pub side_effect: Option<String>,
    pub folder: Option<String>,
    pub category_id: i64,
    pub category: String,
    pub category_description: Option<String>,
    pub code: String,
    pub device: CheatBaseDeviceSummary,
    pub credit: Option<String>,
    pub truncated_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseDeviceSummary {
    pub upstream_id: i64,
    pub name: String,
    pub format: String,
    pub compatibility: DeviceFormatCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseGameSearchRequest {
    pub platform_id: Option<String>,
    pub title: String,
    pub region: Option<String>,
    pub upstream_release_id: Option<i64>,
    pub page: PageRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseGameSearchResult {
    pub confidence: ProviderGameMatchConfidence,
    pub explanation: String,
    pub page: ProviderPage<CheatBaseGame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatBaseHashAlgorithm {
    Crc32,
    Md5,
    Sha1,
}

impl CheatBaseHashAlgorithm {
    fn expected_length(self) -> usize {
        match self {
            Self::Crc32 => 8,
            Self::Md5 => 32,
            Self::Sha1 => 40,
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Crc32 => "romHashCRC",
            Self::Md5 => "romHashMD5",
            Self::Sha1 => "romHashSHA1",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
        }
    }
}

impl std::str::FromStr for CheatBaseHashAlgorithm {
    type Err = CheatBaseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().replace('-', "").as_str() {
            "crc32" | "crc" => Ok(Self::Crc32),
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            _ => Err(CheatBaseError::new(
                CheatBaseErrorKind::InvalidIdentity,
                "algorithm must be crc32, md5 or sha1",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseIdentityLookup {
    pub confidence: ProviderGameMatchConfidence,
    pub evidence: Vec<String>,
    pub page: ProviderPage<CheatBaseGame>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseAttribution {
    pub provider: String,
    pub upstream_project: String,
    pub upstream_commit: String,
    pub database_sha256: String,
    pub licence: String,
}

#[derive(Debug, Clone)]
pub struct CheatBaseDownloadOptions {
    pub cancellation: Option<CheatSourceCancellation>,
    pub progress: Option<CheatSourceProgressReporter>,
    pub overall_timeout: Duration,
}

impl Default for CheatBaseDownloadOptions {
    fn default() -> Self {
        Self {
            cancellation: None,
            progress: None,
            overall_timeout: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatBaseActivationResult {
    pub status: CheatBaseSourceStatus,
    pub imported_from_local_file: bool,
    pub network_used: bool,
}

pub fn cheatbase_provider_identity() -> CheatProviderIdentity {
    CheatProviderIdentity {
        id: CHEATBASE_PROVIDER_ID.to_string(),
        display_name: "CheatBase".to_string(),
        upstream_project: CHEATBASE_UPSTREAM_PROJECT.to_string(),
    }
}

pub fn cheatbase_provenance() -> CheatProviderProvenance {
    CheatProviderProvenance {
        source: "CheatBase".to_string(),
        maintainer: "Noah Keck / CheatBase organization".to_string(),
        origin: "OpenVGDB-derived ROM metadata with historical community cheat data".to_string(),
        distribution_status: "Optional third-party download from the upstream repository"
            .to_string(),
        verification:
            "Community data; identity fields are validated syntactically, not endorsed by ArchiveFS"
                .to_string(),
    }
}

pub fn cheatbase_licence() -> CheatProviderLicence {
    CheatProviderLicence { status: CheatProviderLicenceStatus::NotEstablished, statement: "No dataset licence is declared by the upstream repository; redistribution rights are not established".to_string() }
}

pub fn cheatbase_attribution() -> CheatBaseAttribution {
    CheatBaseAttribution {
        provider: "CheatBase".to_string(),
        upstream_project: "CheatBase/CheatBase".to_string(),
        upstream_commit: CHEATBASE_UPSTREAM_COMMIT.to_string(),
        database_sha256: CHEATBASE_EXPECTED_SHA256.to_string(),
        licence: cheatbase_licence().statement,
    }
}

pub fn inspect_cheatbase_source(
    paths: &CheatBasePaths,
) -> Result<CheatBaseSourceStatus, CheatBaseError> {
    let metadata = read_metadata(paths)?.unwrap_or_default();
    let present = safe_regular_file_if_present(&paths.database)?;
    let fingerprint = present
        .then(|| {
            metadata
                .validation
                .as_ref()
                .map(|v| v.result.source_fingerprint.clone())
        })
        .flatten();
    let usable = metadata.enabled
        && present
        && metadata
            .validation
            .as_ref()
            .is_some_and(|v| v.result.status == ProviderValidationStatus::Valid);
    let state = if !metadata.enabled {
        CheatProviderSourceState::Disabled
    } else if !present {
        match metadata.state {
            CheatProviderSourceState::DownloadFailed
            | CheatProviderSourceState::ValidationFailed
            | CheatProviderSourceState::Invalid
            | CheatProviderSourceState::UnsupportedSchema => metadata.state,
            _ => CheatProviderSourceState::NotInstalled,
        }
    } else {
        metadata.state
    };
    Ok(CheatBaseSourceStatus {
        format_version: CHEATBASE_PROVIDER_FORMAT_VERSION,
        provider: cheatbase_provider_identity(),
        state,
        enabled: metadata.enabled,
        usable,
        database_path: paths.database.clone(),
        fingerprint,
        validation: metadata.validation,
        last_error: metadata.last_error,
        provenance: cheatbase_provenance(),
        licence: cheatbase_licence(),
        browse_only: true,
    })
}

pub fn validate_cheatbase_database(path: &Path) -> Result<CheatBaseValidation, CheatBaseError> {
    validate_database_with_hash(path, Some(CHEATBASE_EXPECTED_SHA256))
}

fn validate_database_with_hash(
    path: &Path,
    expected: Option<&str>,
) -> Result<CheatBaseValidation, CheatBaseError> {
    let fingerprint = fingerprint_regular_file(path)?;
    if let Some(expected) = expected
        && fingerprint.sha256 != expected
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::HashMismatch,
            format!(
                "CheatBase database SHA-256 was {}, expected {expected}",
                fingerprint.sha256
            ),
        ));
    }
    if expected.is_some() && fingerprint.size_bytes != CHEATBASE_EXPECTED_SIZE_BYTES {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::Validation,
            format!(
                "CheatBase database size was {}, expected {}",
                fingerprint.size_bytes, CHEATBASE_EXPECTED_SIZE_BYTES
            ),
        ));
    }
    validate_sqlite_header(path)?;
    let connection = open_immutable_connection(path)?;
    validate_schema(&connection)?;
    validate_relationships(&connection)?;
    validate_hashes(&connection)?;
    let counts = read_counts(&connection)?;
    if counts.systems != 43
        || counts.regions != 39
        || counts.devices != 24
        || counts.categories != 45
        || counts.roms == 0
        || counts.releases == 0
        || counts.cheats == 0
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::Validation,
            "CheatBase sanity counts do not match the supported source snapshot",
        ));
    }
    let query_only = connection
        .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
        .map_err(query_error)?
        == 1;
    let sqlite_version = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(query_error)?;
    Ok(CheatBaseValidation {
        result: ProviderValidationResult { status: ProviderValidationStatus::Valid, validated_at_unix_seconds: now_unix_seconds(), schema_fingerprint: Some(hex_sha256(SCHEMA_DESCRIPTION.as_bytes())), source_fingerprint: fingerprint, diagnostics: vec!["Verified CheatBase seven-table schema, seven maintenance triggers and zero source indexes".to_string(), "All required relationships and hash syntax checks passed".to_string(), "Only Nintendo DS currently contains cheat rows; other systems are metadata-only".to_string()] },
        counts, sqlite_version, database_path: path.to_path_buf(), opened_read_only: true, immutable: true, query_only, upstream_commit: CHEATBASE_UPSTREAM_COMMIT.to_string(),
    })
}

pub fn validate_installed_cheatbase_source(
    paths: &CheatBasePaths,
) -> Result<CheatBaseSourceStatus, CheatBaseError> {
    update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
    match validate_cheatbase_database(&paths.database) {
        Ok(validation) => {
            activate_validation_metadata(paths, validation)?;
            inspect_cheatbase_source(paths)
        }
        Err(error) => {
            update_metadata_state(
                paths,
                state_for_validation_error(&error),
                None,
                Some(error.clone()),
            )?;
            Err(error)
        }
    }
}

pub fn set_cheatbase_enabled(
    paths: &CheatBasePaths,
    enabled: bool,
) -> Result<CheatBaseSourceStatus, CheatBaseError> {
    let mut metadata = read_metadata(paths)?.unwrap_or_default();
    metadata.enabled = enabled;
    metadata.state = if enabled
        && metadata.validation.is_some()
        && safe_regular_file_if_present(&paths.database)?
    {
        CheatProviderSourceState::Ready
    } else if enabled {
        CheatProviderSourceState::NotInstalled
    } else {
        CheatProviderSourceState::Disabled
    };
    metadata.last_operation_at_unix_seconds = now_unix_seconds();
    write_metadata(paths, &metadata)?;
    inspect_cheatbase_source(paths)
}

pub fn import_local_cheatbase_database(
    paths: &CheatBasePaths,
    source: &Path,
) -> Result<CheatBaseActivationResult, CheatBaseError> {
    import_local_with_expected_hash(paths, source, Some(CHEATBASE_EXPECTED_SHA256))
}

fn import_local_with_expected_hash(
    paths: &CheatBasePaths,
    source: &Path,
    expected: Option<&str>,
) -> Result<CheatBaseActivationResult, CheatBaseError> {
    prepare_source_root(paths)?;
    let mut input = open_regular_nofollow(source)?;
    if input.metadata().map_err(source_error)?.len() > CHEATBASE_MAX_DATABASE_BYTES {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::DownloadTooLarge,
            "selected CheatBase database exceeds the size limit",
        ));
    }
    update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
    let staging = staging_path(paths, "import");
    let result = (|| {
        copy_bounded(&mut input, &staging, CHEATBASE_MAX_DATABASE_BYTES)?;
        let validation = validate_database_with_hash(&staging, expected)?;
        publish_validated_database(paths, &staging, validation)?;
        Ok(CheatBaseActivationResult {
            status: inspect_cheatbase_source(paths)?,
            imported_from_local_file: true,
            network_used: false,
        })
    })();
    if let Err(error) = &result {
        let _ = fs::remove_file(&staging);
        let _ = update_metadata_state(
            paths,
            state_for_validation_error(error),
            None,
            Some(error.clone()),
        );
    }
    result
}

pub fn download_cheatbase_database(
    paths: &CheatBasePaths,
    options: &CheatBaseDownloadOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<CheatBaseActivationResult, CheatBaseError> {
    prepare_source_root(paths)?;
    update_metadata_state(paths, CheatProviderSourceState::Downloading, None, None)?;
    validate_download_url(CHEATBASE_DATABASE_URL)?;
    let staging = staging_path(paths, "download");
    let result = (|| {
        let mut output = create_new_nofollow(&staging)?;
        let response = transport
            .get(
                CHEATBASE_DATABASE_URL,
                CHEATBASE_MAX_DATABASE_BYTES,
                &mut output,
                CheatSourceTransferContext {
                    cancellation: options.cancellation.as_ref(),
                    progress: options.progress.as_ref(),
                    attempt: 1,
                    overall_timeout: options.overall_timeout,
                },
            )
            .map_err(|error| {
                CheatBaseError::new(
                    if error.code == "cancelled" {
                        CheatBaseErrorKind::Cancelled
                    } else {
                        CheatBaseErrorKind::Download
                    },
                    error.to_string(),
                )
            })?;
        output.sync_all().map_err(cache_error)?;
        drop(output);
        if (300..400).contains(&response.status) {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::RedirectRejected,
                "CheatBase download redirects are disabled for the pinned immutable source",
            ));
        }
        if !(200..300).contains(&response.status) {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::Download,
                format!("CheatBase server returned HTTP {}", response.status),
            ));
        }
        update_metadata_state(paths, CheatProviderSourceState::Validating, None, None)?;
        let validation = validate_cheatbase_database(&staging)?;
        publish_validated_database(paths, &staging, validation)?;
        Ok(CheatBaseActivationResult {
            status: inspect_cheatbase_source(paths)?,
            imported_from_local_file: false,
            network_used: true,
        })
    })();
    if let Err(error) = &result {
        let _ = fs::remove_file(&staging);
        let state = if matches!(
            error.kind,
            CheatBaseErrorKind::UnsupportedSchema
                | CheatBaseErrorKind::HashMismatch
                | CheatBaseErrorKind::NotSqlite
                | CheatBaseErrorKind::Validation
        ) {
            state_for_validation_error(error)
        } else {
            CheatProviderSourceState::DownloadFailed
        };
        let _ = update_metadata_state(paths, state, None, Some(error.clone()));
    }
    result
}

pub fn remove_local_cheatbase_source(
    paths: &CheatBasePaths,
    confirmed: bool,
) -> Result<(), CheatBaseError> {
    if !confirmed {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::ConfirmationRequired,
            "removing ArchiveFS's local CheatBase copy requires confirmation",
        ));
    }
    if !paths.root.exists() {
        return Ok(());
    }
    for path in [
        &paths.database,
        &paths.metadata,
        &paths.hash,
        &paths.validation,
    ] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(CheatBaseError::new(
                    CheatBaseErrorKind::UnsafePath,
                    format!("refusing unsafe CheatBase cache entry {}", path.display()),
                ));
            }
            Ok(_) => fs::remove_file(path).map_err(cache_error)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(cache_error(error)),
        }
    }
    match fs::remove_dir(&paths.root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            Err(CheatBaseError::new(
                CheatBaseErrorKind::UnsafePath,
                "CheatBase source directory contains an unknown entry and was left intact",
            ))
        }
        Err(error) => Err(cache_error(error)),
    }
}

pub struct CheatBaseCatalogue {
    connection: Connection,
    fingerprint: ImmutableSourceFingerprint,
}

impl CheatBaseCatalogue {
    pub fn open(path: &Path) -> Result<Self, CheatBaseError> {
        Self::open_with_expected_hash(path, Some(CHEATBASE_EXPECTED_SHA256))
    }

    fn open_with_expected_hash(
        path: &Path,
        expected: Option<&str>,
    ) -> Result<Self, CheatBaseError> {
        let validation = validate_database_with_hash(path, expected)?;
        Ok(Self {
            connection: open_immutable_connection(path)?,
            fingerprint: validation.result.source_fingerprint,
        })
    }

    pub fn open_installed(paths: &CheatBasePaths) -> Result<Self, CheatBaseError> {
        let status = inspect_cheatbase_source(paths)?;
        if !status.usable {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::NotInstalled,
                "CheatBase is not installed, enabled and validated",
            ));
        }
        let fingerprint = status.fingerprint.ok_or_else(|| {
            CheatBaseError::new(
                CheatBaseErrorKind::Validation,
                "CheatBase has no validated fingerprint",
            )
        })?;
        let current = fingerprint_regular_file(&paths.database)?;
        if fingerprint.sha256 != CHEATBASE_EXPECTED_SHA256 || current != fingerprint {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::Validation,
                "CheatBase source changed after validation; validate it again",
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
        request: &CheatBaseGameSearchRequest,
    ) -> Result<CheatBaseGameSearchResult, CheatBaseError> {
        let page = request.page.bounded();
        if request.upstream_release_id.is_none()
            && normalize_path_segment(&request.title).is_empty()
        {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::Query,
                "CheatBase search requires a title or upstream release ID",
            ));
        }
        let system_ids = request
            .platform_id
            .as_deref()
            .map(system_ids_for_platform)
            .unwrap_or_else(all_known_system_ids);
        if request.platform_id.is_some() && system_ids.is_empty() {
            return Ok(empty_search(
                page,
                "No CheatBase system maps to the requested canonical platform",
            ));
        }
        let mut clauses = vec![format!(
            "r.systemID IN ({})",
            vec!["?"; system_ids.len()].join(",")
        )];
        let mut values = system_ids
            .into_iter()
            .map(rusqlite::types::Value::Integer)
            .collect::<Vec<_>>();
        if let Some(id) = request.upstream_release_id {
            clauses.push("e.releaseID=?".to_string());
            values.push(id.into());
        }
        let probe = request.title.trim().to_ascii_lowercase();
        if !probe.is_empty() {
            clauses.push("lower(e.releaseTitleName) LIKE ?".to_string());
            values.push(format!("%{probe}%").into());
        }
        if let Some(region) = request.region.as_deref().filter(|v| !v.trim().is_empty()) {
            clauses.push("lower(lg.regionName)=lower(?)".to_string());
            values.push(region.trim().to_string().into());
        }
        let where_sql = clauses.join(" AND ");
        let total_sql = format!(
            "SELECT count(*) FROM RELEASES e JOIN ROMS r ON r.romID=e.romID JOIN REGIONS lg ON lg.regionID=e.regionLocalizedID WHERE {where_sql}"
        );
        let total = self
            .connection
            .query_row(&total_sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(query_error)?
            .max(0) as u64;
        let sql = format!(
            "{} WHERE {where_sql} ORDER BY lower(e.releaseTitleName),e.releaseTitleName,lg.regionName,e.releaseID LIMIT ? OFFSET ?",
            GAME_SELECT
        );
        let mut query_values = values;
        query_values.push(i64::from(page.limit).into());
        query_values.push(i64::from(page.offset).into());
        let normalized = normalize_path_segment(&request.title);
        let mut statement = self.connection.prepare(&sql).map_err(query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(query_values.iter()), game_from_row)
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        for game in &mut rows {
            let exact_release = request.upstream_release_id == Some(game.upstream_release_id);
            let title = normalize_path_segment(&game.title);
            let title_exact = !normalized.is_empty() && title == normalized;
            let title_probable = !normalized.is_empty()
                && (title.contains(&normalized) || normalized.contains(&title));
            game.match_confidence = Some(if exact_release {
                ProviderGameMatchConfidence::ExactUpstreamRelease
            } else if title_exact {
                ProviderGameMatchConfidence::ExactTitlePlatform
            } else if title_probable {
                ProviderGameMatchConfidence::ProbableTitlePlatform
            } else {
                ProviderGameMatchConfidence::NoMatch
            });
            game.match_evidence = if exact_release {
                vec![format!(
                    "Exact CheatBase release ID {}",
                    game.upstream_release_id
                )]
            } else {
                vec![
                    "Platform and title comparison; revision is not represented by CheatBase"
                        .to_string(),
                ]
            };
        }
        rows.retain(|g| g.match_confidence != Some(ProviderGameMatchConfidence::NoMatch));
        let confidence = if total > 1 && request.upstream_release_id.is_none() {
            ProviderGameMatchConfidence::Ambiguous
        } else {
            aggregate_confidence(&rows)
        };
        let returned = rows.len();
        Ok(CheatBaseGameSearchResult {
            confidence,
            explanation: match confidence {
                ProviderGameMatchConfidence::Ambiguous => {
                    "Multiple plausible CheatBase releases require user selection".to_string()
                }
                ProviderGameMatchConfidence::ExactUpstreamRelease => {
                    "Exact upstream release ID match".to_string()
                }
                _ => "Match based on canonical platform and title; exact revision is not verified"
                    .to_string(),
            },
            page: ProviderPage {
                offset: page.offset,
                limit: page.limit,
                total,
                rows,
                has_more: u64::from(page.offset).saturating_add(returned as u64) < total,
            },
        })
    }

    pub fn lookup_hash(
        &self,
        algorithm: CheatBaseHashAlgorithm,
        value: &str,
        platform_id: Option<&str>,
        page: PageRequest,
    ) -> Result<CheatBaseIdentityLookup, CheatBaseError> {
        let normalized = normalize_hash(algorithm, value)?;
        let page = page.bounded();
        let system_ids = platform_id
            .map(system_ids_for_platform)
            .unwrap_or_else(all_known_system_ids);
        if platform_id.is_some() && system_ids.is_empty() {
            return Ok(empty_lookup(
                page,
                "No CheatBase system maps to the requested canonical platform",
            ));
        }
        let where_sql = format!(
            "upper(r.{})=? AND r.systemID IN ({})",
            algorithm.column(),
            vec!["?"; system_ids.len()].join(",")
        );
        let mut values = vec![rusqlite::types::Value::Text(normalized.clone())];
        values.extend(system_ids.into_iter().map(rusqlite::types::Value::Integer));
        let total_sql = format!(
            "SELECT count(*) FROM RELEASES e JOIN ROMS r ON r.romID=e.romID WHERE {where_sql}"
        );
        let total = self
            .connection
            .query_row(&total_sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(query_error)?
            .max(0) as u64;
        let sql = format!(
            "{} WHERE {where_sql} ORDER BY e.releaseID LIMIT ? OFFSET ?",
            GAME_SELECT
        );
        let mut query_values = values;
        query_values.push(i64::from(page.limit).into());
        query_values.push(i64::from(page.offset).into());
        let mut statement = self.connection.prepare(&sql).map_err(query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(query_values.iter()), game_from_row)
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        let confidence = if rows.is_empty() {
            ProviderGameMatchConfidence::NoMatch
        } else if rows.len() == 1 && total == 1 {
            ProviderGameMatchConfidence::ExactHashPlatform
        } else {
            ProviderGameMatchConfidence::Ambiguous
        };
        let evidence = vec![format!(
            "Exact {} {}{}",
            algorithm.label(),
            normalized,
            platform_id
                .map(|id| format!(" on canonical platform {id}"))
                .unwrap_or_default()
        )];
        for game in &mut rows {
            game.match_confidence = Some(confidence);
            game.match_evidence = evidence.clone();
        }
        let returned = rows.len();
        Ok(CheatBaseIdentityLookup {
            confidence,
            evidence,
            page: ProviderPage {
                offset: page.offset,
                limit: page.limit,
                total,
                rows,
                has_more: u64::from(page.offset).saturating_add(returned as u64) < total,
            },
        })
    }

    pub fn lookup_serial(
        &self,
        serial: &str,
        platform_id: &str,
        region: Option<&str>,
        page: PageRequest,
    ) -> Result<CheatBaseIdentityLookup, CheatBaseError> {
        let serial = serial.trim();
        if serial.is_empty() || serial.len() > 128 || serial.chars().any(char::is_control) {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::InvalidIdentity,
                "serial is empty or malformed",
            ));
        }
        let page = page.bounded();
        let system_ids = system_ids_for_platform(platform_id);
        if system_ids.is_empty() {
            return Ok(empty_lookup(
                page,
                "No CheatBase system maps to the requested canonical platform",
            ));
        }
        let mut clauses = vec![
            "upper(r.romSerial)=upper(?)".to_string(),
            format!("r.systemID IN ({})", vec!["?"; system_ids.len()].join(",")),
        ];
        let mut values = vec![serial.to_string().into()];
        values.extend(system_ids.into_iter().map(rusqlite::types::Value::Integer));
        if let Some(region) = region.filter(|v| !v.trim().is_empty()) {
            clauses.push("lower(lg.regionName)=lower(?)".to_string());
            values.push(region.trim().to_string().into());
        }
        let where_sql = clauses.join(" AND ");
        let total_sql = format!(
            "SELECT count(*) FROM RELEASES e JOIN ROMS r ON r.romID=e.romID JOIN REGIONS lg ON lg.regionID=e.regionLocalizedID WHERE {where_sql}"
        );
        let total = self
            .connection
            .query_row(&total_sql, params_from_iter(values.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(query_error)?
            .max(0) as u64;
        let sql = format!(
            "{} WHERE {where_sql} ORDER BY e.releaseID LIMIT ? OFFSET ?",
            GAME_SELECT
        );
        let mut q = values;
        q.push(i64::from(page.limit).into());
        q.push(i64::from(page.offset).into());
        let mut statement = self.connection.prepare(&sql).map_err(query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(q.iter()), game_from_row)
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        let confidence = if rows.is_empty() {
            ProviderGameMatchConfidence::NoMatch
        } else if rows.len() == 1 && total == 1 {
            ProviderGameMatchConfidence::ExactSerialPlatformRegion
        } else {
            ProviderGameMatchConfidence::Ambiguous
        };
        let evidence = vec![format!(
            "Exact serial {serial} on canonical platform {platform_id}{}",
            region
                .map(|v| format!(" in region {v}"))
                .unwrap_or_default()
        )];
        for game in &mut rows {
            game.match_confidence = Some(confidence);
            game.match_evidence = evidence.clone();
        }
        let returned = rows.len();
        Ok(CheatBaseIdentityLookup {
            confidence,
            evidence,
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

const GAME_SELECT: &str = "SELECT e.releaseID,r.romID,e.releaseTitleName,s.systemID,s.systemName,s.systemShortName,rr.regionName,lg.regionName,r.romSerial,r.romHashCRC,r.romHashMD5,r.romHashSHA1,r.romSize,e.releaseDate,(SELECT count(*) FROM CHEATS c WHERE c.romID=r.romID) FROM RELEASES e JOIN ROMS r ON r.romID=e.romID JOIN SYSTEMS s ON s.systemID=r.systemID JOIN REGIONS rr ON rr.regionID=r.regionID JOIN REGIONS lg ON lg.regionID=e.regionLocalizedID";

impl ReadOnlyCheatCatalogue for CheatBaseCatalogue {
    type System = CheatBaseSystem;
    type Device = CheatBaseDevice;
    type Game = CheatBaseGame;
    type Cheat = CheatBaseCheat;
    type Error = CheatBaseError;
    fn identity(&self) -> CheatProviderIdentity {
        cheatbase_provider_identity()
    }
    fn systems(&self, page: PageRequest) -> Result<ProviderPage<Self::System>, Self::Error> {
        let page = page.bounded();
        let total = self
            .connection
            .query_row("SELECT count(*) FROM SYSTEMS", [], |r| r.get::<_, i64>(0))
            .map_err(query_error)?
            .max(0) as u64;
        let mut statement=self.connection.prepare("SELECT s.systemID,s.systemName,s.systemShortName,(SELECT count(*) FROM ROMS r WHERE r.systemID=s.systemID),(SELECT count(*) FROM CHEATS c JOIN ROMS r ON r.romID=c.romID WHERE r.systemID=s.systemID) FROM SYSTEMS s ORDER BY s.systemName,s.systemID LIMIT ? OFFSET ?").map_err(query_error)?;
        let rows = statement
            .query_map(params![page.limit, page.offset], |row| {
                let id = row.get(0)?;
                let name: String = row.get(1)?;
                Ok(CheatBaseSystem {
                    upstream_id: id,
                    name: bounded_required(row.get(1)?, MAX_SHORT_TEXT, "system name")?,
                    short_name: bounded_required(row.get(2)?, MAX_SHORT_TEXT, "system short name")?,
                    rom_count: row.get::<_, i64>(3)?.max(0) as u64,
                    cheat_count: row.get::<_, i64>(4)?.max(0) as u64,
                    mapping: cheatbase_platform_mapping(id, &name),
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
            .query_row("SELECT count(*) FROM CHEAT_DEVICES", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(query_error)?
            .max(0) as u64;
        let mut statement=self.connection.prepare("SELECT d.cheatDeviceID,d.systemID,d.cheatDeviceName,d.cheatDeviceBrandName,d.cheatDeviceFormat,(SELECT count(*) FROM CHEATS c WHERE c.cheatDeviceID=d.cheatDeviceID) FROM CHEAT_DEVICES d ORDER BY d.cheatDeviceName,d.cheatDeviceID LIMIT ? OFFSET ?").map_err(query_error)?;
        let rows = statement
            .query_map(params![page.limit, page.offset], |row| {
                let id = row.get(0)?;
                let name: String = row.get(2)?;
                Ok(CheatBaseDevice {
                    upstream_id: id,
                    system_id: row.get(1)?,
                    name: bounded_required(name.clone(), MAX_SHORT_TEXT, "device name")?,
                    brand: bounded_optional(row.get(3)?, MAX_SHORT_TEXT, &mut Vec::new(), "brand"),
                    format: bounded_required(row.get(4)?, MAX_SHORT_TEXT, "device format")?,
                    cheat_count: row.get::<_, i64>(5)?.max(0) as u64,
                    mapping: cheatbase_device_mapping(id, &name),
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
                &format!("{GAME_SELECT} WHERE e.releaseID=?"),
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
        let rom_id = self
            .connection
            .query_row(
                "SELECT romID FROM RELEASES WHERE releaseID=?",
                [upstream_uid],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(query_error)?;
        let Some(rom_id) = rom_id else {
            return Ok(provider_page(page, 0, Vec::new()));
        };
        let total = self
            .connection
            .query_row("SELECT count(*) FROM CHEATS WHERE romID=?", [rom_id], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(query_error)?
            .max(0) as u64;
        let mut statement=self.connection.prepare("SELECT c.cheatID,c.cheatName,c.cheatActivation,c.cheatDescription,c.cheatSideEffect,c.cheatFolderName,c.cheatCategoryID,g.cheatCategory,g.cheatCategoryDescription,c.cheatCode,c.cheatDeviceID,d.cheatDeviceName,d.cheatDeviceFormat,c.cheatCredit FROM CHEATS c JOIN CHEAT_CATEGORIES g ON g.cheatCategoryID=c.cheatCategoryID JOIN CHEAT_DEVICES d ON d.cheatDeviceID=c.cheatDeviceID WHERE c.romID=? ORDER BY c.cheatID LIMIT ? OFFSET ?").map_err(query_error)?;
        let rows = statement
            .query_map(params![rom_id, page.limit, page.offset], |row| {
                let mut truncated = Vec::new();
                let device_id = row.get(10)?;
                let device_name: String = row.get(11)?;
                let compatibility = cheatbase_device_mapping(device_id, &device_name).compatibility;
                Ok(CheatBaseCheat {
                    upstream_id: row.get(0)?,
                    name: bounded_required(row.get(1)?, MAX_SHORT_TEXT, "cheat name")?,
                    activation: bounded_optional(
                        row.get(2)?,
                        MAX_NOTE,
                        &mut truncated,
                        "activation",
                    ),
                    description: bounded_optional(
                        row.get(3)?,
                        MAX_NOTE,
                        &mut truncated,
                        "description",
                    ),
                    side_effect: bounded_optional(
                        row.get(4)?,
                        MAX_NOTE,
                        &mut truncated,
                        "side effect",
                    ),
                    folder: bounded_optional(row.get(5)?, MAX_SHORT_TEXT, &mut truncated, "folder"),
                    category_id: row.get(6)?,
                    category: bounded_required(row.get(7)?, MAX_SHORT_TEXT, "category")?,
                    category_description: bounded_optional(
                        row.get(8)?,
                        MAX_NOTE,
                        &mut truncated,
                        "category description",
                    ),
                    code: bounded_text(row.get(9)?, MAX_CODE_BODY, &mut truncated, "code"),
                    device: CheatBaseDeviceSummary {
                        upstream_id: device_id,
                        name: device_name,
                        format: row.get(12)?,
                        compatibility,
                    },
                    credit: bounded_optional(
                        row.get(13)?,
                        MAX_SHORT_TEXT,
                        &mut truncated,
                        "credit",
                    ),
                    truncated_fields: truncated,
                })
            })
            .map_err(query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(query_error)?;
        Ok(provider_page(page, total, rows))
    }
}

fn game_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheatBaseGame> {
    let system_id = row.get(3)?;
    let system_name: String = row.get(4)?;
    Ok(CheatBaseGame {
        upstream_release_id: row.get(0)?,
        upstream_rom_id: row.get(1)?,
        title: bounded_required(row.get(2)?, MAX_TITLE, "title")?,
        platform: cheatbase_platform_mapping(system_id, &system_name),
        upstream_system_name: system_name,
        rom_region: row.get(6)?,
        release_region: row.get(7)?,
        serial: normalized_optional(row.get(8)?),
        crc32: normalized_optional(row.get::<_, Option<String>>(9)?)
            .map(|v| v.to_ascii_uppercase()),
        md5: normalized_optional(row.get::<_, Option<String>>(10)?).map(|v| v.to_ascii_uppercase()),
        sha1: normalized_optional(row.get::<_, Option<String>>(11)?)
            .map(|v| v.to_ascii_uppercase()),
        rom_size: row
            .get::<_, Option<i64>>(12)?
            .and_then(|v| u64::try_from(v).ok()),
        release_date: normalized_optional(row.get(13)?),
        cheat_count: row.get::<_, i64>(14)?.max(0) as u64,
        match_confidence: None,
        match_evidence: Vec::new(),
        revision_verified: false,
    })
}

pub fn cheatbase_platform_mapping(upstream_id: i64, name: &str) -> ProviderPlatformMapping {
    let mapped = match upstream_id {
        1 => Some(("3DO", PlatformMappingStatus::Exact)),
        2 => Some(("Arcade", PlatformMappingStatus::Alias)),
        3 => Some(("Atari2600", PlatformMappingStatus::Alias)),
        4 => Some(("Atari5200", PlatformMappingStatus::Alias)),
        5 => Some(("Atari7800", PlatformMappingStatus::Alias)),
        6 => Some(("Atari Lynx", PlatformMappingStatus::Alias)),
        7 => Some(("Atari Jaguar", PlatformMappingStatus::Alias)),
        8 => None,
        9 => Some(("WonderSwan", PlatformMappingStatus::Alias)),
        10 => Some(("WonderSwan Color", PlatformMappingStatus::Alias)),
        11 => Some(("ColecoVision", PlatformMappingStatus::Alias)),
        12 => Some(("Vectrex", PlatformMappingStatus::Alias)),
        13 => Some(("Intellivision", PlatformMappingStatus::Exact)),
        14 => None,
        15 => Some(("PC Engine CD", PlatformMappingStatus::Alias)),
        16 => None,
        17 => None,
        18 => Some(("NES", PlatformMappingStatus::Alias)),
        19 => Some(("Game Boy", PlatformMappingStatus::Alias)),
        20 => Some(("Game Boy Advance", PlatformMappingStatus::Alias)),
        21 => Some(("Game Boy Color", PlatformMappingStatus::Alias)),
        22 => Some(("GameCube", PlatformMappingStatus::Alias)),
        23 => Some(("N64", PlatformMappingStatus::Alias)),
        24 => Some(("Nintendo DS", PlatformMappingStatus::Alias)),
        25 => Some(("NES", PlatformMappingStatus::Alias)),
        26 => Some(("SNES", PlatformMappingStatus::Alias)),
        27 => Some(("Virtual Boy", PlatformMappingStatus::Alias)),
        28 => Some(("Wii", PlatformMappingStatus::Alias)),
        29 => Some(("Sega 32X", PlatformMappingStatus::Exact)),
        30 => Some(("GameGear", PlatformMappingStatus::Alias)),
        31 => Some(("MasterSystem", PlatformMappingStatus::Alias)),
        32 => Some(("Sega CD", PlatformMappingStatus::Alias)),
        33 => Some(("MegaDrive", PlatformMappingStatus::Alias)),
        34 => Some(("Saturn", PlatformMappingStatus::Alias)),
        35 => None,
        36 => Some(("Neo Geo Pocket", PlatformMappingStatus::Alias)),
        37 => Some(("Neo Geo Pocket Color", PlatformMappingStatus::Alias)),
        38 => Some(("PSX", PlatformMappingStatus::Alias)),
        39 => Some(("PSP", PlatformMappingStatus::Alias)),
        40 => None,
        41 => Some(("Commodore 64", PlatformMappingStatus::Alias)),
        42 => Some(("MSX", PlatformMappingStatus::Alias)),
        43 => Some(("MSX2", PlatformMappingStatus::Alias)),
        _ => None,
    };
    let ambiguous = matches!(upstream_id, 14);
    let unsupported = matches!(upstream_id, 8 | 16 | 17 | 35 | 40);
    let (archivefs_platform_id, archivefs_platform_display_name, status, explanation) = if ambiguous
    {
        (None,None,PlatformMappingStatus::Ambiguous,"Combined PC Engine/TurboGrafx-16 row maps to two canonical IDs; ArchiveFS does not guess".to_string())
    } else if let Some((id, status)) = mapped {
        match crate::platform::platform_by_id(id) {
            Some(platform) => (
                Some(platform.id.to_string()),
                Some(platform.display_name.to_string()),
                status,
                format!(
                    "Explicit CheatBase mapping from {name} to canonical platform {}",
                    platform.id
                ),
            ),
            None => (
                None,
                None,
                PlatformMappingStatus::Unknown,
                format!("Explicit mapping target {id} is absent from the canonical registry"),
            ),
        }
    } else if unsupported {
        (
            None,
            None,
            PlatformMappingStatus::Unsupported,
            "ArchiveFS has no distinct canonical platform for this upstream system".to_string(),
        )
    } else {
        (
            None,
            None,
            PlatformMappingStatus::Unknown,
            "Unknown future CheatBase system remains visible and unmapped".to_string(),
        )
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

pub fn cheatbase_device_mapping(upstream_id: i64, name: &str) -> ProviderDeviceMapping {
    let compatibility = match upstream_id {
        1 | 4 | 12 | 16 | 18 | 20 => DeviceFormatCompatibility::PotentiallyConvertible,
        2 | 3 | 8 | 9 | 13 | 14 | 15 | 17 | 19 | 21 | 22 | 23 | 24 => {
            DeviceFormatCompatibility::ReferenceOnly
        }
        5 | 6 | 7 | 10 | 11 => DeviceFormatCompatibility::PotentiallyConvertible,
        _ => DeviceFormatCompatibility::Unknown,
    };
    ProviderDeviceMapping{upstream_id,upstream_name:name.to_string(),compatibility,explanation:match compatibility{DeviceFormatCompatibility::PotentiallyConvertible=>"Recognised device family, but this exact encoding is browse-only until a verified converter exists",DeviceFormatCompatibility::ReferenceOnly=>"Historical/raw format is reference-only in CheatBase Stage 1",DeviceFormatCompatibility::Unknown=>"Unknown device is never treated as installable",_=>"CheatBase Stage 1 never installs codes"}.to_string()}
}

fn all_mapped_system_ids() -> Vec<i64> {
    (1..=43)
        .filter(|id| {
            verified_system_name(*id).is_some_and(|name| {
                cheatbase_platform_mapping(*id, name)
                    .archivefs_platform_id
                    .is_some()
            })
        })
        .collect()
}
fn all_known_system_ids() -> Vec<i64> {
    (1..=43).collect()
}
fn system_ids_for_platform(platform: &str) -> Vec<i64> {
    all_mapped_system_ids()
        .into_iter()
        .filter(|id| {
            verified_system_name(*id).is_some_and(|name| {
                cheatbase_platform_mapping(*id, name)
                    .archivefs_platform_id
                    .as_deref()
                    == Some(platform)
            })
        })
        .collect()
}
fn verified_system_name(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "3DO Interactive Multiplayer",
        2 => "Arcade",
        3 => "Atari 2600",
        4 => "Atari 5200",
        5 => "Atari 7800",
        6 => "Atari Lynx",
        7 => "Atari Jaguar",
        8 => "Atari Jaguar CD",
        9 => "Bandai WonderSwan",
        10 => "Bandai WonderSwan Color",
        11 => "Coleco ColecoVision",
        12 => "GCE Vectrex",
        13 => "Intellivision",
        14 => "NEC PC Engine/TurboGrafx-16",
        15 => "NEC PC Engine CD/TurboGrafx-CD",
        16 => "NEC PC-FX",
        17 => "NEC SuperGrafx",
        18 => "Nintendo Famicom Disk System",
        19 => "Nintendo Game Boy",
        20 => "Nintendo Game Boy Advance",
        21 => "Nintendo Game Boy Color",
        22 => "Nintendo GameCube",
        23 => "Nintendo 64",
        24 => "Nintendo DS",
        25 => "Nintendo Entertainment System",
        26 => "Nintendo Super Nintendo Entertainment System",
        27 => "Nintendo Virtual Boy",
        28 => "Nintendo Wii",
        29 => "Sega 32X",
        30 => "Sega Game Gear",
        31 => "Sega Master System",
        32 => "Sega CD/Mega-CD",
        33 => "Sega Genesis/Mega Drive",
        34 => "Sega Saturn",
        35 => "Sega SG-1000",
        36 => "SNK Neo Geo Pocket",
        37 => "SNK Neo Geo Pocket Color",
        38 => "Sony PlayStation",
        39 => "Sony PlayStation Portable",
        40 => "Magnavox Odyssey2",
        41 => "Commodore 64",
        42 => "Microsoft MSX",
        43 => "Microsoft MSX2",
        _ => return None,
    })
}

fn validate_schema(connection: &Connection) -> Result<(), CheatBaseError> {
    let required: BTreeMap<&str, &[(&str, &str, bool, i64)]> = BTreeMap::from([
        (
            "ROMS",
            &[
                ("romID", "INTEGER", false, 1),
                ("systemID", "INTEGER", true, 0),
                ("regionID", "INTEGER", true, 0),
                ("romHashCRC", "TEXT", false, 0),
                ("romHashMD5", "TEXT", false, 0),
                ("romHashSHA1", "TEXT", false, 0),
                ("romSize", "INTEGER", false, 0),
                ("romFileName", "TEXT", true, 0),
                ("romExtensionlessFileName", "TEXT", true, 0),
                ("romParent", "TEXT", false, 0),
                ("romSerial", "TEXT", false, 0),
                ("romHeader", "TEXT", false, 0),
                ("romLanguage", "TEXT", false, 0),
                ("romDumpSource", "TEXT", true, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "RELEASES",
            &[
                ("releaseID", "INTEGER", false, 1),
                ("romID", "INTEGER", true, 0),
                ("releaseTitleName", "TEXT", true, 0),
                ("regionLocalizedID", "INTEGER", true, 0),
                ("releaseCoverFront", "TEXT", false, 0),
                ("releaseCoverBack", "TEXT", false, 0),
                ("releaseCoverCart", "TEXT", false, 0),
                ("releaseCoverDisc", "TEXT", false, 0),
                ("releaseDescription", "TEXT", false, 0),
                ("releaseDeveloper", "TEXT", false, 0),
                ("releasePublisher", "TEXT", false, 0),
                ("releaseGenre", "TEXT", false, 0),
                ("releaseDate", "TEXT", false, 0),
                ("releaseReferenceURL", "TEXT", false, 0),
                ("releaseReferenceImageURL", "TEXT", false, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "REGIONS",
            &[
                ("regionID", "INTEGER", false, 1),
                ("regionName", "TEXT", true, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "SYSTEMS",
            &[
                ("systemID", "INTEGER", false, 1),
                ("systemName", "TEXT", true, 0),
                ("systemShortName", "TEXT", true, 0),
                ("systemHeaderSizeBytes", "INTEGER", false, 0),
                ("systemHashless", "INTEGER", false, 0),
                ("systemHeader", "INTEGER", false, 0),
                ("systemSerial", "TEXT", false, 0),
                ("systemOEID", "TEXT", false, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "CHEATS",
            &[
                ("cheatID", "INTEGER", false, 1),
                ("romID", "INTEGER", true, 0),
                ("cheatName", "TEXT", true, 0),
                ("cheatActivation", "TEXT", false, 0),
                ("cheatDescription", "TEXT", false, 0),
                ("cheatSideEffect", "TEXT", false, 0),
                ("cheatFolderName", "TEXT", false, 0),
                ("cheatCategoryID", "INTEGER", true, 0),
                ("cheatCode", "TEXT", true, 0),
                ("cheatDeviceID", "INTEGER", true, 0),
                ("cheatCredit", "TEXT", false, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "CHEAT_DEVICES",
            &[
                ("cheatDeviceID", "INTEGER", false, 1),
                ("systemID", "INTEGER", true, 0),
                ("cheatDeviceName", "TEXT", true, 0),
                ("cheatDeviceBrandName", "TEXT", false, 0),
                ("cheatDeviceFormat", "TEXT", false, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
        (
            "CHEAT_CATEGORIES",
            &[
                ("cheatCategoryID", "INTEGER", false, 1),
                ("cheatCategory", "TEXT", true, 0),
                ("cheatCategoryDescription", "TEXT", false, 0),
                ("lastModified", "DATETIME", true, 0),
            ][..],
        ),
    ]);
    for (table, columns) in required {
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?)",
                [table],
                |r| r.get::<_, bool>(0),
            )
            .map_err(query_error)?;
        if !exists {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::UnsupportedSchema,
                format!("CheatBase required table {table} is missing"),
            ));
        }
        let sql = format!("SELECT name,type,\"notnull\",pk FROM pragma_table_info('{table}')");
        let mut statement = connection.prepare(&sql).map_err(query_error)?;
        let actual = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? != 0,
                        r.get::<_, i64>(3)?,
                    ),
                ))
            })
            .map_err(query_error)?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(query_error)?;
        for (name, kind, notnull, pk) in columns {
            match actual.get(*name) {
                Some((actual_kind, actual_notnull, actual_pk))
                    if actual_kind.eq_ignore_ascii_case(kind)
                        && actual_notnull == notnull
                        && actual_pk == pk => {}
                Some(_) => {
                    return Err(CheatBaseError::new(
                        CheatBaseErrorKind::UnsupportedSchema,
                        format!("CheatBase column {table}.{name} has an unsupported definition"),
                    ));
                }
                None => {
                    return Err(CheatBaseError::new(
                        CheatBaseErrorKind::UnsupportedSchema,
                        format!("CheatBase required column {table}.{name} is missing"),
                    ));
                }
            }
        }
    }
    let indexes = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='index'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(query_error)?;
    let triggers = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='trigger'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(query_error)?;
    if indexes != 0 || triggers != 7 {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsupportedSchema,
            format!(
                "CheatBase schema expected 0 indexes and 7 triggers, found {indexes} and {triggers}"
            ),
        ));
    }
    Ok(())
}

fn validate_relationships(c: &Connection) -> Result<(), CheatBaseError> {
    for (label, sql) in [
        (
            "ROMS.system",
            "SELECT 1 FROM ROMS r LEFT JOIN SYSTEMS s ON s.systemID=r.systemID WHERE s.systemID IS NULL LIMIT 1",
        ),
        (
            "ROMS.region",
            "SELECT 1 FROM ROMS r LEFT JOIN REGIONS g ON g.regionID=r.regionID WHERE g.regionID IS NULL LIMIT 1",
        ),
        (
            "RELEASES.rom",
            "SELECT 1 FROM RELEASES e LEFT JOIN ROMS r ON r.romID=e.romID WHERE r.romID IS NULL LIMIT 1",
        ),
        (
            "RELEASES.region",
            "SELECT 1 FROM RELEASES e LEFT JOIN REGIONS g ON g.regionID=e.regionLocalizedID WHERE g.regionID IS NULL LIMIT 1",
        ),
        (
            "CHEAT_DEVICES.system",
            "SELECT 1 FROM CHEAT_DEVICES d LEFT JOIN SYSTEMS s ON s.systemID=d.systemID WHERE s.systemID IS NULL LIMIT 1",
        ),
        (
            "CHEATS.rom",
            "SELECT 1 FROM CHEATS c LEFT JOIN ROMS r ON r.romID=c.romID WHERE r.romID IS NULL LIMIT 1",
        ),
        (
            "CHEATS.device",
            "SELECT 1 FROM CHEATS c LEFT JOIN CHEAT_DEVICES d ON d.cheatDeviceID=c.cheatDeviceID WHERE d.cheatDeviceID IS NULL LIMIT 1",
        ),
        (
            "CHEATS.category",
            "SELECT 1 FROM CHEATS c LEFT JOIN CHEAT_CATEGORIES g ON g.cheatCategoryID=c.cheatCategoryID WHERE g.cheatCategoryID IS NULL LIMIT 1",
        ),
    ] {
        if c.query_row(sql, [], |_| Ok(true))
            .optional()
            .map_err(query_error)?
            .unwrap_or(false)
        {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::Validation,
                format!("CheatBase relationship {label} contains orphaned rows"),
            ));
        }
    }
    Ok(())
}
fn validate_hashes(c: &Connection) -> Result<(), CheatBaseError> {
    let malformed=c.query_row("SELECT count(*) FROM ROMS WHERE (romHashCRC IS NOT NULL AND (length(romHashCRC)!=8 OR romHashCRC GLOB '*[^0-9A-Fa-f]*')) OR (romHashMD5 IS NOT NULL AND (length(romHashMD5)!=32 OR romHashMD5 GLOB '*[^0-9A-Fa-f]*')) OR (romHashSHA1 IS NOT NULL AND (length(romHashSHA1)!=40 OR romHashSHA1 GLOB '*[^0-9A-Fa-f]*'))",[],|r|r.get::<_,i64>(0)).map_err(query_error)?;
    if malformed != 0 {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::Validation,
            format!("CheatBase contains {malformed} malformed hash row(s)"),
        ));
    }
    Ok(())
}
fn read_counts(c: &Connection) -> Result<CheatBaseCounts, CheatBaseError> {
    fn count(c: &Connection, table: &str) -> Result<u64, CheatBaseError> {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|v| v.max(0) as u64)
        .map_err(query_error)
    }
    Ok(CheatBaseCounts {
        systems: count(c, "SYSTEMS")?,
        regions: count(c, "REGIONS")?,
        roms: count(c, "ROMS")?,
        releases: count(c, "RELEASES")?,
        devices: count(c, "CHEAT_DEVICES")?,
        categories: count(c, "CHEAT_CATEGORIES")?,
        cheats: count(c, "CHEATS")?,
        systems_with_cheats: c
            .query_row(
                "SELECT count(DISTINCT r.systemID) FROM CHEATS c JOIN ROMS r ON r.romID=c.romID",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(query_error)?
            .max(0) as u64,
    })
}

fn open_immutable_connection(path: &Path) -> Result<Connection, CheatBaseError> {
    let canonical = fs::canonicalize(path).map_err(source_error)?;
    let mut url = Url::from_file_path(&canonical).map_err(|()| {
        CheatBaseError::new(
            CheatBaseErrorKind::UnsafePath,
            "CheatBase path cannot be encoded as a file URI",
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
    connection.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY;").map_err(query_error)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(query_error)?;
    Ok(connection)
}
fn validate_sqlite_header(path: &Path) -> Result<(), CheatBaseError> {
    let mut file = open_regular_nofollow(path)?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header)
        .map_err(|e| CheatBaseError::new(CheatBaseErrorKind::NotSqlite, e.to_string()))?;
    if &header != SQLITE_HEADER {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::NotSqlite,
            "selected file does not have a SQLite 3 header",
        ));
    }
    Ok(())
}
fn fingerprint_regular_file(path: &Path) -> Result<ImmutableSourceFingerprint, CheatBaseError> {
    let mut file = open_regular_nofollow(path)?;
    let metadata = file.metadata().map_err(source_error)?;
    if metadata.len() > CHEATBASE_MAX_DATABASE_BYTES {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::DownloadTooLarge,
            "CheatBase database exceeds the size limit",
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = file.read(&mut buffer).map_err(source_error)?;
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
fn validate_download_url(value: &str) -> Result<(), CheatBaseError> {
    if value != CHEATBASE_DATABASE_URL {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::RedirectRejected,
            "CheatBase download must use the compiled pinned URL",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        CheatBaseError::new(
            CheatBaseErrorKind::RedirectRejected,
            "CheatBase URL is invalid",
        )
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some(CHEATBASE_DOWNLOAD_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::RedirectRejected,
            "CheatBase URL must remain HTTPS on the approved host",
        ));
    }
    Ok(())
}
fn prepare_source_root(paths: &CheatBasePaths) -> Result<(), CheatBaseError> {
    if let Some(parent) = paths.root.parent() {
        ensure_no_symlink_components(parent)?;
    }
    match fs::symlink_metadata(&paths.root) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsafePath,
            "CheatBase source root is unsafe",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&paths.root).map_err(cache_error)
        }
        Err(e) => Err(cache_error(e)),
    }
}
fn ensure_no_symlink_components(path: &Path) -> Result<(), CheatBaseError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(CheatBaseError::new(
                    CheatBaseErrorKind::UnsafePath,
                    format!("symlinked path component rejected: {}", current.display()),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(cache_error(e)),
        }
    }
    Ok(())
}
fn safe_regular_file_if_present(path: &Path) -> Result<bool, CheatBaseError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsafePath,
            format!("unsafe CheatBase cache path {}", path.display()),
        )),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(cache_error(e)),
    }
}
fn open_regular_nofollow(path: &Path) -> Result<File, CheatBaseError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(source_error)?;
    if !file.metadata().map_err(source_error)?.is_file() {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsafePath,
            "CheatBase source must be a regular file",
        ));
    }
    Ok(file)
}
fn create_new_nofollow(path: &Path) -> Result<File, CheatBaseError> {
    let _ = fs::remove_file(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path).map_err(cache_error)
}
fn copy_bounded(input: &mut File, destination: &Path, maximum: u64) -> Result<(), CheatBaseError> {
    let mut output = create_new_nofollow(destination)?;
    let mut total = 0u64;
    let mut buffer = vec![0u8; HASH_CHUNK];
    loop {
        let read = input.read(&mut buffer).map_err(source_error)?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or_else(|| {
            CheatBaseError::new(
                CheatBaseErrorKind::DownloadTooLarge,
                "CheatBase input size overflow",
            )
        })?;
        if total > maximum {
            return Err(CheatBaseError::new(
                CheatBaseErrorKind::DownloadTooLarge,
                "CheatBase input exceeds the size limit",
            ));
        }
        output.write_all(&buffer[..read]).map_err(cache_error)?;
    }
    output.sync_all().map_err(cache_error)
}
fn staging_path(paths: &CheatBasePaths, label: &str) -> PathBuf {
    paths.root.join(format!(
        ".{label}-{}-{}.tmp",
        std::process::id(),
        now_unix_seconds()
    ))
}
fn publish_validated_database(
    paths: &CheatBasePaths,
    staging: &Path,
    mut validation: CheatBaseValidation,
) -> Result<(), CheatBaseError> {
    validation.database_path = paths.database.clone();
    fs::rename(staging, &paths.database).map_err(cache_error)?;
    atomic_write(
        &paths.hash,
        format!(
            "{}  {}\n",
            validation.result.source_fingerprint.sha256, CHEATBASE_DATABASE_FILE
        )
        .as_bytes(),
    )?;
    atomic_write_json(&paths.validation, &validation)?;
    activate_validation_metadata(paths, validation)
}
fn activate_validation_metadata(
    paths: &CheatBasePaths,
    validation: CheatBaseValidation,
) -> Result<(), CheatBaseError> {
    update_metadata_state(
        paths,
        CheatProviderSourceState::Ready,
        Some(validation),
        None,
    )
}
fn update_metadata_state(
    paths: &CheatBasePaths,
    state: CheatProviderSourceState,
    validation: Option<CheatBaseValidation>,
    error: Option<CheatBaseError>,
) -> Result<(), CheatBaseError> {
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
fn state_for_validation_error(error: &CheatBaseError) -> CheatProviderSourceState {
    match error.kind {
        CheatBaseErrorKind::UnsupportedSchema => CheatProviderSourceState::UnsupportedSchema,
        CheatBaseErrorKind::NotSqlite => CheatProviderSourceState::Invalid,
        _ => CheatProviderSourceState::ValidationFailed,
    }
}
fn read_metadata(
    paths: &CheatBasePaths,
) -> Result<Option<CheatBaseSourceMetadata>, CheatBaseError> {
    if !safe_regular_file_if_present(&paths.metadata)? {
        return Ok(None);
    }
    let mut file = open_regular_nofollow(&paths.metadata)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(cache_error)?;
    let metadata: CheatBaseSourceMetadata = serde_json::from_slice(&bytes)
        .map_err(|e| CheatBaseError::new(CheatBaseErrorKind::Validation, e.to_string()))?;
    if metadata.format_version != CHEATBASE_PROVIDER_FORMAT_VERSION
        || metadata.provider_id != CHEATBASE_PROVIDER_ID
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsupportedSchema,
            "CheatBase source metadata binding is unsupported",
        ));
    }
    Ok(Some(metadata))
}
fn write_metadata(
    paths: &CheatBasePaths,
    metadata: &CheatBaseSourceMetadata,
) -> Result<(), CheatBaseError> {
    atomic_write_json(&paths.metadata, metadata)
}
fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CheatBaseError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| CheatBaseError::new(CheatBaseErrorKind::CacheWrite, e.to_string()))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CheatBaseError> {
    if let Ok(m) = fs::symlink_metadata(path)
        && (m.file_type().is_symlink() || !m.is_file())
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::UnsafePath,
            format!("unsafe CheatBase destination {}", path.display()),
        ));
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut output = create_new_nofollow(&temporary)?;
    output.write_all(bytes).map_err(cache_error)?;
    output.sync_all().map_err(cache_error)?;
    drop(output);
    fs::rename(temporary, path).map_err(cache_error)
}
fn normalize_hash(
    algorithm: CheatBaseHashAlgorithm,
    value: &str,
) -> Result<String, CheatBaseError> {
    let normalized = value.trim().to_ascii_uppercase();
    if normalized.len() != algorithm.expected_length()
        || !normalized.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(CheatBaseError::new(
            CheatBaseErrorKind::InvalidIdentity,
            format!(
                "{} must contain exactly {} hexadecimal characters",
                algorithm.label(),
                algorithm.expected_length()
            ),
        ));
    }
    Ok(normalized)
}
fn normalized_optional(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let t = v.trim();
        (!t.is_empty()).then(|| t.to_string())
    })
}
fn bounded_required(value: String, max: usize, field: &str) -> rusqlite::Result<String> {
    if value.len() > max {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "CheatBase {field} exceeds {max} bytes"
        )));
    }
    Ok(value)
}
fn bounded_text(mut value: String, max: usize, truncated: &mut Vec<String>, field: &str) -> String {
    if value.len() > max {
        let mut end = max;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        truncated.push(field.to_string());
    }
    value
}
fn bounded_optional(
    value: Option<String>,
    max: usize,
    truncated: &mut Vec<String>,
    field: &str,
) -> Option<String> {
    value.map(|v| bounded_text(v, max, truncated, field))
}
fn provider_page<T>(page: PageRequest, total: u64, rows: Vec<T>) -> ProviderPage<T> {
    let count = rows.len();
    ProviderPage {
        offset: page.offset,
        limit: page.limit,
        total,
        rows,
        has_more: u64::from(page.offset).saturating_add(count as u64) < total,
    }
}
fn aggregate_confidence(rows: &[CheatBaseGame]) -> ProviderGameMatchConfidence {
    if rows.is_empty() {
        ProviderGameMatchConfidence::NoMatch
    } else if rows.len() > 1 {
        ProviderGameMatchConfidence::Ambiguous
    } else {
        rows[0]
            .match_confidence
            .unwrap_or(ProviderGameMatchConfidence::NoMatch)
    }
}
fn empty_search(page: PageRequest, message: &str) -> CheatBaseGameSearchResult {
    CheatBaseGameSearchResult {
        confidence: ProviderGameMatchConfidence::NoMatch,
        explanation: message.to_string(),
        page: provider_page(page, 0, Vec::new()),
    }
}
fn empty_lookup(page: PageRequest, message: &str) -> CheatBaseIdentityLookup {
    CheatBaseIdentityLookup {
        confidence: ProviderGameMatchConfidence::NoMatch,
        evidence: vec![message.to_string()],
        page: provider_page(page, 0, Vec::new()),
    }
}
fn query_error(error: rusqlite::Error) -> CheatBaseError {
    CheatBaseError::new(CheatBaseErrorKind::Query, error.to_string())
}
fn cache_error(error: std::io::Error) -> CheatBaseError {
    CheatBaseError::new(CheatBaseErrorKind::CacheWrite, error.to_string())
}
fn source_error(error: std::io::Error) -> CheatBaseError {
    CheatBaseError::new(CheatBaseErrorKind::SourceUnreadable, error.to_string())
}
fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .unwrap_or(0)
}
fn hex_sha256(bytes: &[u8]) -> String {
    hex_bytes(Sha256::digest(bytes).as_slice())
}
fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests;
