//! Trusted remote RetroArch cheat catalogue retrieval.
//!
//! Network bytes are never passed to the installer. They are bounded,
//! authenticated by HTTPS, hashed, safely extracted, validated with the
//! existing local catalogue parser, and atomically published as an immutable
//! local snapshot first.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use zip::ZipArchive;

use crate::default_database_path;
use crate::emulator_environment::{EncodedPath, HostReadOnlyFilesystem};

use super::cheat_cache_lock::LockedCheatCache;
use super::cheat_catalogue::{CatalogueEntryExclusionKind, MAX_CATALOGUE_EXCLUSION_EXAMPLES};
use super::load_cheat_catalogue_snapshot;

pub const CHEAT_SOURCE_RESULT_SCHEMA_VERSION: u32 = 3;
const CHEAT_SOURCE_LEGACY_SCHEMA_VERSION: u32 = 1;
const CACHE_DIRECTORY: &str = "cheat-sources";
pub(super) const METADATA_FILE: &str = "metadata.json";
pub(super) const SNAPSHOTS_DIRECTORY: &str = "snapshots";
pub(super) const MANIFESTS_DIRECTORY: &str = "manifests";
pub(super) const STAGING_DIRECTORY: &str = ".staging";
const FRESH_SECONDS: u64 = 24 * 60 * 60;
pub const CHEAT_SOURCE_REDIRECT_LIMIT: usize = 3;
const HEADER_BYTES_LIMIT: usize = 32 * 1024;
pub const CHEAT_SOURCE_ENTRY_LIMIT: usize = 60_000;
pub const CHEAT_SOURCE_FILE_SIZE_LIMIT: u64 = 8 * 1024 * 1024;
pub const CHEAT_SOURCE_EXPANDED_SIZE_LIMIT: u64 = 1024 * 1024 * 1024;
const COMPRESSION_RATIO_LIMIT: u64 = 250;
pub const CHEAT_SOURCE_PATH_BYTES_LIMIT: usize = 1024;
const PATH_COMPONENT_LIMIT: usize = 24;
pub const CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT: u64 = 256 * 1024 * 1024;
pub const CHEAT_SOURCE_MANIFEST_BYTES_LIMIT: usize = 16 * 1024 * 1024;
pub const CHEAT_SOURCE_TIMEOUT_SECONDS: u64 = 180;
pub const CHEAT_SOURCE_RETAINED_SNAPSHOTS_MINIMUM: usize = 2;
const REVISION_RESPONSE_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatSourceArchiveType {
    Zip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceDefinition {
    pub source_id: String,
    pub display_name: String,
    pub download_url: String,
    pub permitted_host: String,
    pub canonical_repository_url: String,
    pub revision_url: String,
    pub revision_host: String,
    pub archive_type: CheatSourceArchiveType,
    pub maximum_expected_bytes: u64,
    pub pinned_version: Option<String>,
    pub expected_sha256: Option<String>,
    pub provenance: String,
    pub licence_url: Option<String>,
    pub enabled: bool,
    pub experimental: bool,
    /// Exact directory inside the archive which becomes the local catalogue.
    pub catalogue_prefix: String,
}

pub fn trusted_retroarch_cheat_sources() -> Vec<CheatSourceDefinition> {
    vec![CheatSourceDefinition {
        source_id: "libretro-buildbot-cheats".to_string(),
        display_name: "Official Libretro cheat database".to_string(),
        download_url:
            "https://codeload.github.com/libretro/libretro-database/zip/{revision}".to_string(),
        permitted_host: "codeload.github.com".to_string(),
        canonical_repository_url: "https://github.com/libretro/libretro-database".to_string(),
        revision_url:
            "https://api.github.com/repos/libretro/libretro-database/commits/master".to_string(),
        revision_host: "api.github.com".to_string(),
        archive_type: CheatSourceArchiveType::Zip,
        maximum_expected_bytes: CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT,
        pinned_version: None,
        expected_sha256: None,
        provenance: "Official Libretro database repository; the moving master reference is resolved to an exact commit before its immutable archive is downloaded".to_string(),
        licence_url: Some(
            "https://github.com/libretro/libretro-database/blob/master/LICENSE".to_string(),
        ),
        enabled: true,
        experimental: false,
        catalogue_prefix: String::new(),
    }]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatSourceErrorStage {
    Registry,
    Network,
    Download,
    Validation,
    Extraction,
    Cache,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceError {
    pub schema_version: u32,
    pub stage: CheatSourceErrorStage,
    pub code: String,
    pub message: String,
}

impl CheatSourceError {
    pub(super) fn new(
        stage: CheatSourceErrorStage,
        code: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            stage,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CheatSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CheatSourceError {}

#[derive(Debug, Clone)]
pub struct CheatSourceFetchOptions {
    pub cache_root: PathBuf,
    pub force_refresh: bool,
    pub offline: bool,
    pub expected_sha256: Option<String>,
    pub max_download_bytes: Option<u64>,
    pub cancellation: Option<CheatSourceCancellation>,
}

impl CheatSourceFetchOptions {
    pub fn with_default_cache() -> Result<Self, CheatSourceError> {
        Ok(Self {
            cache_root: default_cheat_source_cache_root()?,
            force_refresh: false,
            offline: false,
            expected_sha256: None,
            max_download_bytes: None,
            cancellation: None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct CheatSourceCancellation(Arc<AtomicBool>);

impl CheatSourceCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub fn default_cheat_source_cache_root() -> Result<PathBuf, CheatSourceError> {
    let database = default_database_path().map_err(|error| {
        CheatSourceError::new(
            CheatSourceErrorStage::Cache,
            "cache_root_unavailable",
            error.to_string(),
        )
    })?;
    Ok(database
        .parent()
        .expect("the default database path always has a parent")
        .join(CACHE_DIRECTORY))
}

#[derive(Debug, Clone)]
pub struct CheatSourceHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub content_length: Option<u64>,
    pub location: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub downloaded_bytes: u64,
}

pub trait CheatSourceTransport {
    fn get(
        &self,
        url: &str,
        maximum_bytes: u64,
        destination: &mut dyn Write,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError>;
}

#[derive(Debug, Clone)]
pub struct HttpsCheatSourceTransport {
    agent: ureq::Agent,
}

impl HttpsCheatSourceTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(CHEAT_SOURCE_TIMEOUT_SECONDS)))
            .timeout_resolve(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_recv_response(Some(Duration::from_secs(10)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: config.new_agent(),
        }
    }
}

impl Default for HttpsCheatSourceTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CheatSourceTransport for HttpsCheatSourceTransport {
    fn get(
        &self,
        url: &str,
        maximum_bytes: u64,
        destination: &mut dyn Write,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
        validate_public_resolution(url)?;
        let mut response = self
            .agent
            .get(url)
            .header(
                "Accept",
                "application/vnd.github+json, application/zip, application/octet-stream",
            )
            .header("Accept-Encoding", "identity")
            .header(
                "User-Agent",
                concat!("archivefs/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(|error| {
                CheatSourceError::new(
                    CheatSourceErrorStage::Network,
                    "request_failed",
                    error.to_string(),
                )
            })?;
        let header_bytes = response
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
            .sum::<usize>();
        if header_bytes > HEADER_BYTES_LIMIT {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Network,
                "response_headers_too_large",
                "response headers exceed the configured limit",
            ));
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let content_encoding = header("content-encoding");
        if content_encoding
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
        {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Download,
                "compressed_transfer_rejected",
                "only identity transfer encoding is accepted",
            ));
        }
        let content_length = header("content-length").and_then(|value| value.parse::<u64>().ok());
        if content_length.is_some_and(|size| size > maximum_bytes) {
            let size = content_length.unwrap_or_default();
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Download,
                "download_too_large",
                format!(
                    "declared response size {size} bytes exceeds configured limit {maximum_bytes} bytes"
                ),
            ));
        }
        let status = response.status().as_u16();
        let content_type = header("content-type");
        let location = header("location");
        let etag = header("etag");
        let last_modified = header("last-modified");
        let mut downloaded_bytes = 0u64;
        if (200..300).contains(&status) {
            let mut reader = response.body_mut().as_reader();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = reader.read(&mut buffer).map_err(|error| {
                    CheatSourceError::new(
                        CheatSourceErrorStage::Download,
                        "response_interrupted",
                        error.to_string(),
                    )
                })?;
                if count == 0 {
                    break;
                }
                downloaded_bytes = downloaded_bytes.saturating_add(count as u64);
                if downloaded_bytes > maximum_bytes {
                    return Err(CheatSourceError::new(
                        CheatSourceErrorStage::Download,
                        "download_too_large",
                        format!(
                            "received at least {downloaded_bytes} bytes, exceeding configured limit {maximum_bytes} bytes"
                        ),
                    ));
                }
                destination
                    .write_all(&buffer[..count])
                    .map_err(|error| cache_error("staging_write_failed", error))?;
            }
            if content_length.is_some_and(|declared| declared != downloaded_bytes) {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Download,
                    "incomplete_response",
                    "received byte count differs from Content-Length",
                ));
            }
        }
        Ok(CheatSourceHttpResponse {
            status,
            content_type,
            content_encoding,
            content_length,
            location,
            etag,
            last_modified,
            downloaded_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatSourceFreshness {
    Fresh,
    Stale,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatSourceFetchStatus {
    Fetched,
    CacheReused,
    OfflineReused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceExclusionExample {
    pub kind: CheatSourceExclusionKind,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatSourceExclusionKind {
    MalformedCht,
    UnsupportedContentEncoding,
    UnsupportedPathEncoding,
    UnsupportedContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceManifest {
    pub format_version: u32,
    pub source_id: String,
    pub source_url: String,
    #[serde(default)]
    pub canonical_repository_url: String,
    #[serde(default)]
    pub resolved_revision: String,
    pub pinned_version: Option<String>,
    pub fetched_at_unix_seconds: u64,
    pub downloaded_bytes: u64,
    pub extracted_bytes: u64,
    pub archive_entry_count: usize,
    pub archive_sha256: String,
    pub response_content_type: Option<String>,
    pub response_etag: Option<String>,
    pub response_last_modified: Option<String>,
    pub catalogue_file_count: usize,
    #[serde(default)]
    pub indexed_file_count: usize,
    pub valid_cheat_count: usize,
    pub malformed_cheat_count: usize,
    pub skipped_entry_count: usize,
    #[serde(default)]
    pub excluded_unsupported_count: usize,
    #[serde(default)]
    pub excluded_path_encoding_count: usize,
    #[serde(default)]
    pub exclusion_examples: Vec<CheatSourceExclusionExample>,
    pub discovered_platforms: Vec<String>,
    pub validation_complete: bool,
    pub warnings: Vec<String>,
    pub catalogue_relative_path: String,
    pub cache_relative_path: String,
    pub files: Vec<CheatSourceManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceManifestFile {
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceCacheMetadata {
    pub format_version: u32,
    pub source_id: String,
    pub current_snapshot: Option<String>,
    pub manifest: Option<CheatSourceManifest>,
    pub last_fetch_succeeded: bool,
    pub last_error: Option<CheatSourceError>,
    #[serde(default)]
    pub last_error_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatSourceFetchResult {
    pub schema_version: u32,
    pub status: CheatSourceFetchStatus,
    pub source: CheatSourceDefinition,
    pub local_catalogue_path: EncodedPath,
    pub immutable_snapshot_path: EncodedPath,
    pub manifest: CheatSourceManifest,
    pub freshness: CheatSourceFreshness,
    pub from_cache: bool,
    pub stale: bool,
    pub warnings: Vec<String>,
}

/// Provenance attached to guided-setup output after retrieval has produced a
/// validated local snapshot. This is deliberately metadata only; setup still
/// consumes `local_catalogue_path` through the existing local parser.
#[derive(Debug, Clone, Serialize)]
pub struct CheatSourceSetupContext {
    pub source_id: String,
    pub display_name: String,
    pub source_url: String,
    pub fetched_at_unix_seconds: u64,
    pub archive_sha256: String,
    pub validation_complete: bool,
    pub retrieval_status: CheatSourceFetchStatus,
    pub from_cache: bool,
    pub stale: bool,
    pub immutable_snapshot_path: EncodedPath,
    pub warnings: Vec<String>,
}

impl From<&CheatSourceFetchResult> for CheatSourceSetupContext {
    fn from(result: &CheatSourceFetchResult) -> Self {
        Self {
            source_id: result.source.source_id.clone(),
            display_name: result.source.display_name.clone(),
            source_url: result.source.download_url.clone(),
            fetched_at_unix_seconds: result.manifest.fetched_at_unix_seconds,
            archive_sha256: result.manifest.archive_sha256.clone(),
            validation_complete: result.manifest.validation_complete,
            retrieval_status: result.status,
            from_cache: result.from_cache,
            stale: result.stale,
            immutable_snapshot_path: result.immutable_snapshot_path.clone(),
            warnings: result.warnings.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatSourceInspection {
    pub schema_version: u32,
    pub source: CheatSourceDefinition,
    pub cache_root: EncodedPath,
    pub current_snapshot_path: Option<EncodedPath>,
    pub current_catalogue_path: Option<EncodedPath>,
    pub manifest: Option<CheatSourceManifest>,
    pub freshness: CheatSourceFreshness,
    pub last_fetch_succeeded: Option<bool>,
    pub last_error: Option<CheatSourceError>,
    pub last_error_at_unix_seconds: Option<u64>,
    pub setup_usable: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatSourceListEntry {
    pub source: CheatSourceDefinition,
    pub trust_status: String,
    pub freshness: CheatSourceFreshness,
    pub current_cached_version: Option<String>,
    pub fetched_at_unix_seconds: Option<u64>,
    pub archive_sha256: Option<String>,
    pub catalogue_file_count: Option<usize>,
    pub indexed_file_count: Option<usize>,
    pub excluded_file_count: Option<usize>,
    pub exclusion_examples: Vec<CheatSourceExclusionExample>,
    pub setup_usable: bool,
    pub status: CheatCatalogueStatus,
    pub total_bytes: Option<u64>,
    pub last_error: Option<CheatSourceError>,
    pub last_error_at_unix_seconds: Option<u64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatCatalogueStatus {
    Missing,
    Ready,
    ReadyWithWarnings,
    Stale,
    InvalidManifest,
    Incomplete,
    UnsupportedSchema,
    VerificationFailed,
    RetrievalFailed,
    Cancelled,
    ResourceLimitReached,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatSourceList {
    pub schema_version: u32,
    pub entries: Vec<CheatSourceListEntry>,
}

pub fn list_retroarch_cheat_sources(
    cache_root: &Path,
) -> Result<CheatSourceList, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    let entries = trusted_retroarch_cheat_sources()
        .into_iter()
        .map(
            |source| match inspect_retroarch_cheat_source_locked(&source.source_id, &locked) {
                Ok(inspection) => CheatSourceListEntry {
                    current_cached_version: inspection
                        .manifest
                        .as_ref()
                        .and_then(|m| m.pinned_version.clone()),
                    fetched_at_unix_seconds: inspection
                        .manifest
                        .as_ref()
                        .map(|m| m.fetched_at_unix_seconds),
                    archive_sha256: inspection
                        .manifest
                        .as_ref()
                        .map(|m| m.archive_sha256.clone()),
                    catalogue_file_count: inspection.manifest.as_ref().map(|m| m.files.len()),
                    indexed_file_count: inspection
                        .manifest
                        .as_ref()
                        .map(manifest_indexed_file_count),
                    excluded_file_count: inspection
                        .manifest
                        .as_ref()
                        .map(manifest_excluded_file_count),
                    exclusion_examples: inspection
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.exclusion_examples.clone())
                        .unwrap_or_default(),
                    setup_usable: inspection.setup_usable,
                    status: match inspection.freshness {
                        CheatSourceFreshness::Missing => CheatCatalogueStatus::Missing,
                        CheatSourceFreshness::Stale => CheatCatalogueStatus::Stale,
                        CheatSourceFreshness::Fresh | CheatSourceFreshness::Unknown
                            if inspection.setup_usable
                                && inspection.manifest.as_ref().is_some_and(|manifest| {
                                    manifest_excluded_file_count(manifest) > 0
                                }) =>
                        {
                            CheatCatalogueStatus::ReadyWithWarnings
                        }
                        CheatSourceFreshness::Fresh | CheatSourceFreshness::Unknown
                            if inspection.setup_usable =>
                        {
                            CheatCatalogueStatus::Ready
                        }
                        CheatSourceFreshness::Fresh | CheatSourceFreshness::Unknown => {
                            CheatCatalogueStatus::Incomplete
                        }
                    },
                    total_bytes: inspection.manifest.as_ref().map(|m| m.extracted_bytes),
                    last_error: inspection.last_error.clone(),
                    last_error_at_unix_seconds: inspection.last_error_at_unix_seconds,
                    warnings: inspection.warnings,
                    freshness: inspection.freshness,
                    trust_status: "built_in_reviewed".to_string(),
                    source,
                },
                Err(error) => CheatSourceListEntry {
                    source,
                    trust_status: "built_in_reviewed".to_string(),
                    freshness: CheatSourceFreshness::Unknown,
                    current_cached_version: None,
                    fetched_at_unix_seconds: None,
                    archive_sha256: None,
                    catalogue_file_count: None,
                    indexed_file_count: None,
                    excluded_file_count: None,
                    exclusion_examples: Vec::new(),
                    setup_usable: false,
                    status: status_for_error(&error),
                    total_bytes: None,
                    last_error: Some(error.clone()),
                    last_error_at_unix_seconds: None,
                    warnings: vec![error.to_string()],
                },
            },
        )
        .collect();
    Ok(CheatSourceList {
        schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        entries,
    })
}

fn manifest_indexed_file_count(manifest: &CheatSourceManifest) -> usize {
    if manifest.format_version >= 3 {
        manifest.indexed_file_count
    } else {
        manifest
            .catalogue_file_count
            .saturating_sub(manifest.malformed_cheat_count)
    }
}

fn manifest_excluded_file_count(manifest: &CheatSourceManifest) -> usize {
    if manifest.format_version >= 3 {
        manifest
            .malformed_cheat_count
            .saturating_add(manifest.excluded_unsupported_count)
            .saturating_add(manifest.excluded_path_encoding_count)
    } else {
        let unsupported = manifest
            .warnings
            .iter()
            .find(|warning| warning.contains("non-UTF-8 catalogue files"))
            .and_then(|warning| warning.split_whitespace().next())
            .and_then(|count| count.parse::<usize>().ok())
            .unwrap_or_else(|| {
                manifest
                    .skipped_entry_count
                    .saturating_sub(manifest.malformed_cheat_count)
            });
        manifest.malformed_cheat_count.saturating_add(unsupported)
    }
}

fn source_exclusion_kind(kind: CatalogueEntryExclusionKind) -> CheatSourceExclusionKind {
    match kind {
        CatalogueEntryExclusionKind::MalformedCht => CheatSourceExclusionKind::MalformedCht,
        CatalogueEntryExclusionKind::UnsupportedContentEncoding => {
            CheatSourceExclusionKind::UnsupportedContentEncoding
        }
        CatalogueEntryExclusionKind::UnsupportedPathEncoding => {
            CheatSourceExclusionKind::UnsupportedPathEncoding
        }
        CatalogueEntryExclusionKind::UnsupportedContent => {
            CheatSourceExclusionKind::UnsupportedContent
        }
    }
}

pub fn inspect_retroarch_cheat_source(
    source_id: &str,
    cache_root: &Path,
) -> Result<CheatSourceInspection, CheatSourceError> {
    let locked = LockedCheatCache::acquire_existing(cache_root)?;
    inspect_retroarch_cheat_source_locked(source_id, &locked)
}

fn inspect_retroarch_cheat_source_locked(
    source_id: &str,
    locked: &LockedCheatCache,
) -> Result<CheatSourceInspection, CheatSourceError> {
    let cache_root = locked.root();
    let source = resolve_source(source_id)?;
    if !locked.present_at_acquisition() {
        return Ok(CheatSourceInspection {
            schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            source,
            cache_root: EncodedPath::from_path(cache_root),
            current_snapshot_path: None,
            current_catalogue_path: None,
            manifest: None,
            freshness: CheatSourceFreshness::Missing,
            last_fetch_succeeded: None,
            last_error: None,
            last_error_at_unix_seconds: None,
            setup_usable: false,
            warnings: Vec::new(),
        });
    }
    validate_cache_path_for_read(cache_root)?;
    let source_root = cache_root.join(source_id);
    let metadata_path = source_root.join(METADATA_FILE);
    validate_cache_path_for_read(&metadata_path)?;
    if !metadata_path.exists() {
        return Ok(CheatSourceInspection {
            schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            source,
            cache_root: EncodedPath::from_path(cache_root),
            current_snapshot_path: None,
            current_catalogue_path: None,
            manifest: None,
            freshness: CheatSourceFreshness::Missing,
            last_fetch_succeeded: None,
            last_error: None,
            last_error_at_unix_seconds: None,
            setup_usable: false,
            warnings: Vec::new(),
        });
    }
    reject_symlink(&metadata_path)?;
    let bytes =
        fs::read(&metadata_path).map_err(|error| cache_error("metadata_read_failed", error))?;
    let metadata: CheatSourceCacheMetadata =
        serde_json::from_slice(&bytes).map_err(|error| cache_error("metadata_invalid", error))?;
    if !supported_cheat_source_schema(metadata.format_version) || metadata.source_id != source_id {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Cache,
            "metadata_binding_invalid",
            "cache metadata version or source binding is invalid",
        ));
    }
    let (snapshot_path, catalogue_path, usable) = if let (Some(snapshot), Some(manifest)) =
        (&metadata.current_snapshot, &metadata.manifest)
    {
        validate_snapshot_name(snapshot)?;
        validate_catalogue_prefix(&manifest.catalogue_relative_path)?;
        if !supported_cheat_source_schema(manifest.format_version)
            || manifest.source_id != source_id
            || snapshot != &manifest.archive_sha256
            || manifest.cache_relative_path != format!("{SNAPSHOTS_DIRECTORY}/{snapshot}")
        {
            return Err(cache_error(
                "snapshot_manifest_binding_invalid",
                "current snapshot provenance does not match its source, schema, or archive hash",
            ));
        }
        let snapshot_path = source_root.join(SNAPSHOTS_DIRECTORY).join(snapshot);
        let catalogue_path = snapshot_path.join(&manifest.catalogue_relative_path);
        safe_regular_or_directory(&snapshot_path, true)?;
        safe_regular_or_directory(&catalogue_path, true)?;
        verify_catalogue_manifest(&catalogue_path, &manifest.files)?;
        (
            Some(snapshot_path),
            Some(catalogue_path),
            manifest.validation_complete,
        )
    } else {
        (None, None, false)
    };
    let freshness = metadata
        .manifest
        .as_ref()
        .map_or(CheatSourceFreshness::Missing, manifest_freshness);
    let warnings = metadata
        .manifest
        .as_ref()
        .map(|manifest| manifest.warnings.clone())
        .unwrap_or_default();
    Ok(CheatSourceInspection {
        schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        source,
        cache_root: EncodedPath::from_path(cache_root),
        current_snapshot_path: snapshot_path.as_deref().map(EncodedPath::from_path),
        current_catalogue_path: catalogue_path.as_deref().map(EncodedPath::from_path),
        manifest: metadata.manifest,
        freshness,
        last_fetch_succeeded: Some(metadata.last_fetch_succeeded),
        last_error: metadata.last_error,
        last_error_at_unix_seconds: metadata.last_error_at_unix_seconds,
        setup_usable: usable,
        warnings,
    })
}

pub(super) fn supported_cheat_source_schema(version: u32) -> bool {
    (CHEAT_SOURCE_LEGACY_SCHEMA_VERSION..=CHEAT_SOURCE_RESULT_SCHEMA_VERSION).contains(&version)
}

pub fn inspect_retroarch_cheat_source_snapshot(
    snapshot_path: &Path,
) -> Result<CheatSourceInspection, CheatSourceError> {
    validate_cache_path_for_read(snapshot_path)?;
    let hash = snapshot_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            cache_error(
                "snapshot_path_invalid",
                "snapshot path has no UTF-8 content identity",
            )
        })?;
    validate_sha256(hash)?;
    let snapshots_root = snapshot_path
        .parent()
        .ok_or_else(|| cache_error("snapshot_path_invalid", "snapshot path has no parent"))?;
    if snapshots_root.file_name().and_then(|value| value.to_str()) != Some(SNAPSHOTS_DIRECTORY) {
        return Err(cache_error(
            "snapshot_path_invalid",
            "snapshot path must name a snapshots/<sha256> directory",
        ));
    }
    let source_root = snapshots_root
        .parent()
        .ok_or_else(|| cache_error("snapshot_path_invalid", "snapshot path has no source root"))?;
    let source_id = source_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| cache_error("snapshot_path_invalid", "snapshot path has no source ID"))?;
    let source = resolve_source(source_id)?;
    let cache_root = source_root
        .parent()
        .ok_or_else(|| cache_error("snapshot_path_invalid", "snapshot path has no cache root"))?;
    let _locked = LockedCheatCache::acquire_required(cache_root)?;
    safe_regular_or_directory(snapshot_path, true)?;
    let manifest_path = source_root
        .join(MANIFESTS_DIRECTORY)
        .join(format!("{hash}.json"));
    validate_cache_path_for_read(&manifest_path)?;
    reject_symlink(&manifest_path)?;
    let bytes = fs::read(&manifest_path)
        .map_err(|error| cache_error("snapshot_manifest_read_failed", error))?;
    let manifest: CheatSourceManifest = serde_json::from_slice(&bytes)
        .map_err(|error| cache_error("snapshot_manifest_invalid", error))?;
    if !supported_cheat_source_schema(manifest.format_version) {
        return Err(cache_error(
            "snapshot_manifest_unsupported_schema",
            format!(
                "unsupported snapshot manifest schema {}",
                manifest.format_version
            ),
        ));
    }
    if manifest.source_id != source_id
        || manifest.archive_sha256 != hash
        || manifest.cache_relative_path != format!("{SNAPSHOTS_DIRECTORY}/{hash}")
    {
        return Err(cache_error(
            "snapshot_manifest_binding_invalid",
            "snapshot provenance does not bind to this source and content identity",
        ));
    }
    let catalogue_path = snapshot_path.join(&manifest.catalogue_relative_path);
    safe_regular_or_directory(&catalogue_path, true)?;
    verify_catalogue_manifest(&catalogue_path, &manifest.files)?;
    let freshness = manifest_freshness(&manifest);
    let warnings = manifest.warnings.clone();
    Ok(CheatSourceInspection {
        schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        source,
        cache_root: EncodedPath::from_path(cache_root),
        current_snapshot_path: Some(EncodedPath::from_path(snapshot_path)),
        current_catalogue_path: Some(EncodedPath::from_path(&catalogue_path)),
        setup_usable: manifest.validation_complete,
        freshness,
        manifest: Some(manifest),
        last_fetch_succeeded: None,
        last_error: None,
        last_error_at_unix_seconds: None,
        warnings,
    })
}

pub fn fetch_retroarch_cheat_source(
    source_id: &str,
    options: &CheatSourceFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<CheatSourceFetchResult, CheatSourceError> {
    check_cancelled(options)?;
    let source = resolve_source(source_id)?;
    validate_source(&source)?;
    if let Some(expected) = &options.expected_sha256 {
        validate_sha256(expected)?;
    }
    if options.offline && options.force_refresh {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Offline,
            "offline_force_refresh_conflict",
            "--offline and --force-refresh cannot be used together",
        ));
    }
    if !options.offline && !options.cache_root.exists() {
        prepare_cache_root(&options.cache_root)?;
    }
    let locked = if options.cache_root.exists() {
        LockedCheatCache::acquire_required(&options.cache_root)?
    } else {
        LockedCheatCache::acquire_existing(&options.cache_root)?
    };
    if let Ok(inspection) = inspect_retroarch_cheat_source_locked(source_id, &locked) {
        let expected_matches = options.expected_sha256.as_ref().is_none_or(|expected| {
            inspection
                .manifest
                .as_ref()
                .is_some_and(|manifest| manifest.archive_sha256.eq_ignore_ascii_case(expected))
        });
        let reusable = inspection.setup_usable
            && expected_matches
            && (options.offline
                || (!options.force_refresh && inspection.freshness == CheatSourceFreshness::Fresh));
        if reusable {
            let manifest = inspection.manifest.expect("usable cache has a manifest");
            let mut warnings = manifest.warnings.clone();
            let snapshot = inspection
                .current_snapshot_path
                .expect("usable cache has a snapshot");
            let catalogue = inspection
                .current_catalogue_path
                .expect("usable cache has a catalogue");
            let stale = inspection.freshness == CheatSourceFreshness::Stale;
            if stale {
                warnings.push("cached snapshot is stale".to_string());
            }
            return Ok(CheatSourceFetchResult {
                schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
                status: if options.offline {
                    CheatSourceFetchStatus::OfflineReused
                } else {
                    CheatSourceFetchStatus::CacheReused
                },
                source,
                local_catalogue_path: catalogue,
                immutable_snapshot_path: snapshot,
                manifest,
                freshness: inspection.freshness,
                from_cache: true,
                stale,
                warnings,
            });
        }
    }
    if options.offline {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Offline,
            "offline_cache_unavailable",
            "no valid cached snapshot is available",
        ));
    }
    let result = fetch_and_publish(&source, options, transport);
    if let Err(error) = &result {
        record_fetch_failure(&source, &options.cache_root, error);
    }
    result
}

fn fetch_and_publish(
    source: &CheatSourceDefinition,
    options: &CheatSourceFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<CheatSourceFetchResult, CheatSourceError> {
    check_cancelled(options)?;
    prepare_cache_root(&options.cache_root)?;
    let source_root = options.cache_root.join(&source.source_id);
    let staging_root = source_root.join(STAGING_DIRECTORY);
    let snapshots_root = source_root.join(SNAPSHOTS_DIRECTORY);
    let manifests_root = source_root.join(MANIFESTS_DIRECTORY);
    create_safe_directory(&source_root)?;
    create_safe_directory(&staging_root)?;
    create_safe_directory(&snapshots_root)?;
    create_safe_directory(&manifests_root)?;
    let unique = format!("{}-{}", std::process::id(), now_nanos());
    let work = staging_root.join(unique);
    create_safe_directory(&work)?;
    let _cleanup = WorkCleanup(work.clone());
    let maximum = options
        .max_download_bytes
        .unwrap_or(source.maximum_expected_bytes)
        .min(source.maximum_expected_bytes);
    if maximum == 0 {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Download,
            "invalid_download_limit",
            "download limit must be greater than zero",
        ));
    }
    let archive_path = work.join("archive.zip");
    let mut archive_file = secure_create(&archive_path)?;
    let resolved = resolve_source_revision(source, transport, options)?;
    check_cancelled(options)?;
    let response = download_with_redirects(
        source,
        &resolved.archive_url,
        maximum,
        transport,
        &mut archive_file,
    )?;
    check_cancelled(options)?;
    if response
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            let media_type = content_type.split(';').next().unwrap_or_default().trim();
            !matches!(
                media_type,
                "application/zip" | "application/octet-stream" | "application/x-zip-compressed"
            )
        })
    {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "unsupported_content_type",
            format!(
                "response Content-Type is not a supported ZIP type: {}",
                response.content_type.as_deref().unwrap_or_default()
            ),
        ));
    }
    archive_file
        .flush()
        .and_then(|_| archive_file.sync_all())
        .map_err(|error| cache_error("staging_sync_failed", error))?;
    drop(archive_file);
    let archive_sha256 = sha256_file(&archive_path)?;
    let expected = options
        .expected_sha256
        .as_ref()
        .or(source.expected_sha256.as_ref());
    if let Some(expected) = expected {
        validate_sha256(expected)?;
        if !archive_sha256.eq_ignore_ascii_case(expected) {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Validation,
                "sha256_mismatch",
                format!("expected {expected}, received {archive_sha256}"),
            ));
        }
    }
    if !zip_magic_valid(&archive_path)? {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "archive_magic_invalid",
            "download is not a ZIP archive by content",
        ));
    }
    let extracted = work.join("extracted");
    create_safe_directory(&extracted)?;
    let extraction = extract_zip_safely(&archive_path, &extracted, options)?;
    check_cancelled(options)?;
    validate_catalogue_prefix(&source.catalogue_prefix)?;
    let catalogue_path = extracted.join(&source.catalogue_prefix);
    safe_regular_or_directory(&catalogue_path, true)?;
    let snapshot =
        load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, &source.source_id, &catalogue_path);
    let malformed = snapshot.excluded_malformed_count();
    let excluded_unsupported = snapshot.excluded_unsupported_count();
    let excluded_path_encoding = snapshot.excluded_path_encoding_count();
    if snapshot.games.is_empty() || !snapshot.complete {
        let diagnostic_codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "catalogue_validation_incomplete",
            format!(
                "extracted catalogue is empty or structurally incomplete (games: {}, diagnostics: {}; codes: {})",
                snapshot.games.len(),
                snapshot.diagnostics.len(),
                diagnostic_codes
            ),
        ));
    }
    let catalogue_files = snapshot.total_candidate_files;
    let indexed_files = snapshot.games.len();
    let valid_cheats = snapshot
        .games
        .iter()
        .filter(|game| game.parsing_complete)
        .map(|game| game.cheat_count)
        .sum();
    let platforms = snapshot
        .games
        .iter()
        .filter_map(|game| game.source_platform.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let files = collect_catalogue_manifest(&catalogue_path)?;
    let fetched_at = now_seconds();
    let snapshot_name = archive_sha256.clone();
    let snapshot_path = snapshots_root.join(&snapshot_name);
    if snapshot_path.exists() {
        verify_catalogue_manifest(&snapshot_path.join(&source.catalogue_prefix), &files).map_err(
            |_| {
                cache_error(
                    "snapshot_hash_collision",
                    "existing content-addressed snapshot differs from the validated download",
                )
            },
        )?;
    } else {
        fs::rename(&extracted, &snapshot_path)
            .map_err(|error| cache_error("snapshot_publish_failed", error))?;
        sync_directory(&snapshots_root)?;
    }
    let mut warnings = Vec::new();
    if malformed != 0 {
        warnings.push(format!(
            "{malformed} catalogue files were retained but are non-actionable because parsing was incomplete"
        ));
    }
    if excluded_unsupported != 0 {
        warnings.push(format!(
            "{excluded_unsupported} catalogue files used unsupported content or encoding and were excluded"
        ));
    }
    if excluded_path_encoding != 0 {
        warnings.push(format!(
            "{excluded_path_encoding} catalogue paths used unsupported encoding and were excluded"
        ));
    }
    let exclusion_examples = snapshot
        .excluded_entries
        .iter()
        .take(MAX_CATALOGUE_EXCLUSION_EXAMPLES)
        .map(|entry| CheatSourceExclusionExample {
            kind: source_exclusion_kind(entry.kind),
            relative_path: (!entry.path.lossy)
                .then(|| {
                    Path::new(&entry.path.display)
                        .strip_prefix(&catalogue_path)
                        .ok()
                })
                .flatten()
                .and_then(Path::to_str)
                .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/")),
        })
        .collect();
    let mut manifest = CheatSourceManifest {
        format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        source_id: source.source_id.clone(),
        source_url: resolved.archive_url,
        canonical_repository_url: source.canonical_repository_url.clone(),
        resolved_revision: resolved.commit_id.clone(),
        pinned_version: Some(resolved.commit_id),
        fetched_at_unix_seconds: fetched_at,
        downloaded_bytes: response.downloaded_bytes,
        extracted_bytes: extraction.expanded_bytes,
        archive_entry_count: extraction.entry_count,
        archive_sha256,
        response_content_type: response.content_type,
        response_etag: response.etag,
        response_last_modified: response.last_modified,
        catalogue_file_count: catalogue_files,
        indexed_file_count: indexed_files,
        valid_cheat_count: valid_cheats,
        malformed_cheat_count: malformed,
        skipped_entry_count: snapshot.excluded_entries.len(),
        excluded_unsupported_count: excluded_unsupported,
        excluded_path_encoding_count: excluded_path_encoding,
        exclusion_examples,
        discovered_platforms: platforms,
        validation_complete: true,
        warnings: warnings.clone(),
        catalogue_relative_path: source.catalogue_prefix.clone(),
        cache_relative_path: format!("{SNAPSHOTS_DIRECTORY}/{snapshot_name}"),
        files,
    };
    let manifest_path = manifests_root.join(format!("{snapshot_name}.json"));
    if !atomic_write_json_new(&manifest_path, &manifest)? {
        let existing_bytes = fs::read(&manifest_path)
            .map_err(|error| cache_error("snapshot_manifest_read_failed", error))?;
        let existing: CheatSourceManifest = serde_json::from_slice(&existing_bytes)
            .map_err(|error| cache_error("snapshot_manifest_invalid", error))?;
        if existing.source_id != manifest.source_id
            || existing.archive_sha256 != manifest.archive_sha256
            || existing.files != manifest.files
        {
            return Err(cache_error(
                "snapshot_manifest_collision",
                "existing immutable snapshot provenance does not match downloaded content",
            ));
        }
        manifest = existing;
    }
    let metadata = CheatSourceCacheMetadata {
        format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        source_id: source.source_id.clone(),
        current_snapshot: Some(snapshot_name.clone()),
        manifest: Some(manifest.clone()),
        last_fetch_succeeded: true,
        last_error: None,
        last_error_at_unix_seconds: None,
    };
    check_cancelled(options)?;
    atomic_write_json(&source_root.join(METADATA_FILE), &metadata)?;
    let _ = fs::remove_dir_all(&work);
    let catalogue = snapshot_path.join(&source.catalogue_prefix);
    Ok(CheatSourceFetchResult {
        schema_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
        status: CheatSourceFetchStatus::Fetched,
        source: source.clone(),
        local_catalogue_path: EncodedPath::from_path(&catalogue),
        immutable_snapshot_path: EncodedPath::from_path(&snapshot_path),
        manifest,
        freshness: CheatSourceFreshness::Fresh,
        from_cache: false,
        stale: false,
        warnings,
    })
}

#[derive(Debug, Deserialize)]
struct RevisionResponse {
    sha: String,
}

struct ResolvedSourceRevision {
    commit_id: String,
    archive_url: String,
}

fn resolve_source_revision(
    source: &CheatSourceDefinition,
    transport: &dyn CheatSourceTransport,
    options: &CheatSourceFetchOptions,
) -> Result<ResolvedSourceRevision, CheatSourceError> {
    validate_url_host(&source.revision_url, &source.revision_host)?;
    let mut bytes = Vec::new();
    let response = transport.get(&source.revision_url, REVISION_RESPONSE_LIMIT, &mut bytes)?;
    check_cancelled(options)?;
    if !(200..300).contains(&response.status) || response.downloaded_bytes != bytes.len() as u64 {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Network,
            "revision_resolution_failed",
            "the authoritative revision response was unsuccessful or incomplete",
        ));
    }
    let revision: RevisionResponse = serde_json::from_slice(&bytes).map_err(|error| {
        CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "revision_response_invalid",
            error.to_string(),
        )
    })?;
    if revision.sha.len() != 40 || !revision.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "revision_invalid",
            "resolved revision is not an exact 40-character commit ID",
        ));
    }
    let archive_url = source.download_url.replace("{revision}", &revision.sha);
    validate_url_for_source(&archive_url, source)?;
    Ok(ResolvedSourceRevision {
        commit_id: revision.sha.to_ascii_lowercase(),
        archive_url,
    })
}

fn check_cancelled(options: &CheatSourceFetchOptions) -> Result<(), CheatSourceError> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(CheatSourceCancellation::is_cancelled)
    {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Download,
            "cancelled",
            "catalogue retrieval was cancelled before activation",
        ))
    } else {
        Ok(())
    }
}

fn download_with_redirects(
    source: &CheatSourceDefinition,
    initial_url: &str,
    maximum: u64,
    transport: &dyn CheatSourceTransport,
    destination: &mut dyn Write,
) -> Result<CheatSourceHttpResponse, CheatSourceError> {
    let mut url = initial_url.to_string();
    let mut visited = HashSet::new();
    for redirects in 0..=CHEAT_SOURCE_REDIRECT_LIMIT {
        validate_url_for_source(&url, source)?;
        if !visited.insert(url.clone()) {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Network,
                "redirect_loop",
                "redirect loop detected",
            ));
        }
        let response = transport.get(&url, maximum, destination)?;
        if (300..400).contains(&response.status) {
            if redirects == CHEAT_SOURCE_REDIRECT_LIMIT {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Network,
                    "redirect_limit_exceeded",
                    "redirect limit exceeded",
                ));
            }
            let location = response.location.as_deref().ok_or_else(|| {
                CheatSourceError::new(
                    CheatSourceErrorStage::Network,
                    "redirect_location_missing",
                    "redirect omitted Location",
                )
            })?;
            let base = Url::parse(&url).map_err(|error| registry_error("invalid_url", error))?;
            url = base
                .join(location)
                .map_err(|error| registry_error("invalid_redirect", error))?
                .to_string();
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Network,
                "http_status",
                format!("server returned HTTP {}", response.status),
            ));
        }
        validate_downloaded_size(response.downloaded_bytes, maximum)?;
        return Ok(response);
    }
    unreachable!()
}

fn validate_downloaded_size(actual: u64, maximum: u64) -> Result<(), CheatSourceError> {
    if actual > maximum {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Download,
            "download_too_large",
            format!("received {actual} bytes, exceeding configured limit {maximum} bytes"),
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtractionSummary {
    entry_count: usize,
    expanded_bytes: u64,
}

fn extract_zip_safely(
    archive_path: &Path,
    root: &Path,
    options: &CheatSourceFetchOptions,
) -> Result<ExtractionSummary, CheatSourceError> {
    let file =
        File::open(archive_path).map_err(|error| extraction_error("archive_open_failed", error))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| extraction_error("corrupt_zip", error))?;
    validate_entry_count(archive.len())?;
    let mut expanded = 0u64;
    let mut paths = HashSet::new();
    let mut folded = HashSet::new();
    for index in 0..archive.len() {
        check_cancelled(options)?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| extraction_error("entry_read_failed", error))?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| {
                CheatSourceError::new(
                    CheatSourceErrorStage::Extraction,
                    "entry_name_not_utf8",
                    "ZIP entry name is not UTF-8",
                )
            })?
            .to_string();
        validate_archive_entry_name(&name)?;
        let normalized = name.trim_end_matches('/').to_string();
        if !paths.insert(normalized.clone()) {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Extraction,
                "duplicate_entry",
                format!("duplicate archive path: {normalized}"),
            ));
        }
        if !folded.insert(normalized.to_ascii_lowercase()) {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Extraction,
                "case_fold_collision",
                format!("case-folding archive path collision: {normalized}"),
            ));
        }
        let mode = entry
            .unix_mode()
            .unwrap_or(if entry.is_dir() { 0o040755 } else { 0o100644 });
        validate_unix_entry_mode(mode, &normalized)?;
        if !entry.is_dir() && !entry.is_file() {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Extraction,
                "unsupported_entry_type",
                format!("unsupported archive entry: {normalized}"),
            ));
        }
        let next_expanded = expanded.checked_add(entry.size()).ok_or_else(|| {
            CheatSourceError::new(
                CheatSourceErrorStage::Extraction,
                "expanded_size_overflow",
                "expanded size overflow",
            )
        })?;
        validate_extraction_sizes(
            entry.size(),
            entry.compressed_size(),
            next_expanded,
            &normalized,
        )?;
        expanded = next_expanded;
        let entry_size = entry.size();
        let destination = root.join(&normalized);
        if entry.is_dir() {
            create_safe_directory(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            create_safe_directory(parent)?;
        }
        let mut output = secure_create(&destination)?;
        let copied = std::io::copy(
            &mut entry.by_ref().take(CHEAT_SOURCE_FILE_SIZE_LIMIT + 1),
            &mut output,
        )
        .map_err(|error| extraction_error("entry_extract_failed", error))?;
        if copied != entry_size {
            return Err(CheatSourceError::new(
                CheatSourceErrorStage::Extraction,
                "entry_size_mismatch",
                format!("entry size mismatch: {normalized}"),
            ));
        }
        output
            .flush()
            .map_err(|error| extraction_error("entry_flush_failed", error))?;
    }
    Ok(ExtractionSummary {
        entry_count: archive.len(),
        expanded_bytes: expanded,
    })
}

fn validate_extraction_sizes(
    file_size: u64,
    compressed_size: u64,
    expanded_total: u64,
    name: &str,
) -> Result<(), CheatSourceError> {
    if file_size > CHEAT_SOURCE_FILE_SIZE_LIMIT {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "file_size_limit_exceeded",
            format!("entry exceeds {CHEAT_SOURCE_FILE_SIZE_LIMIT} bytes: {name}"),
        ));
    }
    if expanded_total > CHEAT_SOURCE_EXPANDED_SIZE_LIMIT {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "expanded_size_limit_exceeded",
            format!("expanded archive exceeds {CHEAT_SOURCE_EXPANDED_SIZE_LIMIT} bytes"),
        ));
    }
    if file_size > 0
        && (compressed_size == 0 || file_size / compressed_size.max(1) > COMPRESSION_RATIO_LIMIT)
    {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "compression_ratio_limit_exceeded",
            format!("entry compression ratio is unsafe: {name}"),
        ));
    }
    Ok(())
}

fn validate_entry_count(count: usize) -> Result<(), CheatSourceError> {
    if count > CHEAT_SOURCE_ENTRY_LIMIT {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "entry_limit_exceeded",
            format!("archive has more than {CHEAT_SOURCE_ENTRY_LIMIT} entries"),
        ))
    } else {
        Ok(())
    }
}

fn validate_archive_entry_name(name: &str) -> Result<(), CheatSourceError> {
    if name.is_empty()
        || name.as_bytes().contains(&0)
        || name.contains('\\')
        || name.starts_with('/')
        || name.starts_with("//")
    {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "unsafe_entry_path",
            format!("unsafe archive entry path: {name:?}"),
        ));
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed.len() > CHEAT_SOURCE_PATH_BYTES_LIMIT
        || trimmed.split('/').count() > PATH_COMPONENT_LIMIT
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "unsafe_entry_path",
            format!("unsafe archive entry path: {name:?}"),
        ));
    }
    let first = trimmed.split('/').next().unwrap_or_default();
    if first.as_bytes().get(1) == Some(&b':') {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "windows_drive_path_rejected",
            format!("Windows drive path rejected: {name:?}"),
        ));
    }
    Ok(())
}

fn validate_unix_entry_mode(mode: u32, name: &str) -> Result<(), CheatSourceError> {
    let kind = mode & 0o170000;
    if kind == 0 || kind == 0o040000 || kind == 0o100000 {
        Ok(())
    } else {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Extraction,
            "special_entry_rejected",
            format!("archive special entry rejected: {name}"),
        ))
    }
}

pub(super) fn collect_catalogue_manifest(
    root: &Path,
) -> Result<Vec<CheatSourceManifestFile>, CheatSourceError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    while let Some(directory) = pending.pop() {
        validate_cache_path_for_read(&directory)?;
        let entries = fs::read_dir(&directory)
            .map_err(|error| cache_error("catalogue_manifest_read_failed", error))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| cache_error("catalogue_manifest_read_failed", error))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| cache_error("catalogue_manifest_metadata_failed", error))?;
            if metadata.file_type().is_symlink() {
                return Err(cache_error(
                    "catalogue_manifest_symlink",
                    format!("symlink in catalogue snapshot: {}", path.display()),
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(cache_error(
                    "catalogue_manifest_special_file",
                    format!("special file in catalogue snapshot: {}", path.display()),
                ));
            }
            if metadata.len() > CHEAT_SOURCE_FILE_SIZE_LIMIT {
                return Err(cache_error(
                    "catalogue_manifest_file_size_limit",
                    format!(
                        "catalogue file exceeds {CHEAT_SOURCE_FILE_SIZE_LIMIT} bytes: {}",
                        path.display()
                    ),
                ));
            }
            total_bytes = total_bytes.saturating_add(metadata.len());
            if total_bytes > CHEAT_SOURCE_EXPANDED_SIZE_LIMIT {
                return Err(cache_error(
                    "catalogue_manifest_total_size_limit",
                    "catalogue files exceed the expanded-size verification limit",
                ));
            }
            let relative = path.strip_prefix(root).map_err(|_| {
                cache_error(
                    "catalogue_manifest_escape",
                    "catalogue entry escaped its root",
                )
            })?;
            let relative = relative
                .to_str()
                .ok_or_else(|| {
                    cache_error(
                        "catalogue_manifest_non_utf8",
                        "extracted catalogue path is not UTF-8",
                    )
                })?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let bytes = fs::read(&path)
                .map_err(|error| cache_error("catalogue_manifest_file_read_failed", error))?;
            files.push(CheatSourceManifestFile {
                relative_path: relative,
                size: metadata.len(),
                sha256: sha256_hex(&bytes),
            });
            if files.len() > CHEAT_SOURCE_ENTRY_LIMIT {
                return Err(cache_error(
                    "catalogue_manifest_entry_limit",
                    "catalogue manifest entry limit exceeded",
                ));
            }
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn verify_catalogue_manifest(
    root: &Path,
    expected: &[CheatSourceManifestFile],
) -> Result<(), CheatSourceError> {
    let actual = collect_catalogue_manifest(root)?;
    if actual == expected {
        Ok(())
    } else {
        Err(cache_error(
            "catalogue_manifest_mismatch",
            "cached catalogue files differ from the validated manifest",
        ))
    }
}

fn resolve_source(source_id: &str) -> Result<CheatSourceDefinition, CheatSourceError> {
    trusted_retroarch_cheat_sources()
        .into_iter()
        .find(|source| source.source_id == source_id)
        .ok_or_else(|| {
            CheatSourceError::new(
                CheatSourceErrorStage::Registry,
                "unknown_source",
                format!("unknown trusted source ID: {source_id}"),
            )
        })
}

fn validate_source(source: &CheatSourceDefinition) -> Result<(), CheatSourceError> {
    if !source.enabled {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Registry,
            "source_disabled",
            "source is disabled",
        ));
    }
    validate_snapshot_name(&source.source_id)?;
    validate_catalogue_prefix(&source.catalogue_prefix)?;
    if !source.download_url.contains("{revision}") {
        return Err(registry_error(
            "revision_template_missing",
            "trusted repository archive URL must bind an exact revision",
        ));
    }
    validate_url_for_source(
        &source.download_url.replace("{revision}", &"0".repeat(40)),
        source,
    )?;
    validate_url_host(&source.revision_url, &source.revision_host)?;
    validate_url_host(&source.canonical_repository_url, "github.com")
}

fn validate_url_for_source(
    value: &str,
    source: &CheatSourceDefinition,
) -> Result<(), CheatSourceError> {
    validate_url_host(value, &source.permitted_host)
}

fn validate_url_host(value: &str, permitted_host: &str) -> Result<(), CheatSourceError> {
    let url = Url::parse(value).map_err(|error| registry_error("invalid_url", error))?;
    if url.scheme() != "https" {
        return Err(registry_error(
            "non_https_url",
            "trusted sources require HTTPS",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(registry_error(
            "url_authentication_rejected",
            "URL authentication is not permitted",
        ));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(registry_error(
            "unsupported_port",
            "only the default HTTPS port is permitted",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| registry_error("url_host_missing", "URL has no host"))?;
    if !host.eq_ignore_ascii_case(permitted_host) {
        return Err(registry_error(
            "unapproved_host",
            format!("host {host} is not approved for this trusted source endpoint"),
        ));
    }
    if is_local_hostname(host) || host.parse::<IpAddr>().is_ok_and(is_non_public_ip) {
        return Err(registry_error(
            "local_address_rejected",
            "local or private endpoints are forbidden",
        ));
    }
    Ok(())
}

fn validate_public_resolution(value: &str) -> Result<(), CheatSourceError> {
    let url = Url::parse(value).map_err(|error| registry_error("invalid_url", error))?;
    let host = url
        .host_str()
        .ok_or_else(|| registry_error("url_host_missing", "URL has no host"))?;
    let addresses = (host, url.port_or_known_default().unwrap_or(443))
        .to_socket_addrs()
        .map_err(|error| {
            CheatSourceError::new(
                CheatSourceErrorStage::Network,
                "dns_resolution_failed",
                error.to_string(),
            )
        })?
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_non_public_ip(address.ip()))
    {
        return Err(CheatSourceError::new(
            CheatSourceErrorStage::Network,
            "unsafe_dns_resolution",
            "hostname resolved to a local, private, or non-routable address",
        ));
    }
    Ok(())
}

fn is_local_hostname(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost")
}

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

fn validate_sha256(value: &str) -> Result<(), CheatSourceError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Validation,
            "expected_sha256_invalid",
            "expected SHA-256 must be exactly 64 hexadecimal characters",
        ))
    }
}
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_file(path: &Path) -> Result<String, CheatSourceError> {
    let mut file =
        File::open(path).map_err(|error| cache_error("staged_archive_open_failed", error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| cache_error("staged_archive_read_failed", error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn zip_magic_valid(path: &Path) -> Result<bool, CheatSourceError> {
    let mut file =
        File::open(path).map_err(|error| cache_error("staged_archive_open_failed", error))?;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return Ok(false);
    }
    Ok(matches!(
        magic,
        [0x50, 0x4b, 0x03, 0x04] | [0x50, 0x4b, 0x05, 0x06] | [0x50, 0x4b, 0x07, 0x08]
    ))
}
pub(super) fn manifest_freshness(manifest: &CheatSourceManifest) -> CheatSourceFreshness {
    if now_seconds().saturating_sub(manifest.fetched_at_unix_seconds) <= FRESH_SECONDS {
        CheatSourceFreshness::Fresh
    } else {
        CheatSourceFreshness::Stale
    }
}
pub(super) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn prepare_cache_root(path: &Path) -> Result<(), CheatSourceError> {
    validate_cache_path_for_read(path)?;
    create_safe_directory(path)?;
    validate_cache_path_for_read(path)
}
pub(super) fn validate_cache_path_for_read(path: &Path) -> Result<(), CheatSourceError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(cache_error(
            "unsafe_cache_root",
            "cache root contains traversal",
        ));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(cache_error(
                    "unsafe_cache_symlink",
                    format!("cache path contains a symlink: {}", current.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(cache_error("cache_path_inaccessible", error)),
        }
    }
    Ok(())
}

fn create_safe_directory(path: &Path) -> Result<(), CheatSourceError> {
    validate_cache_path_for_read(path)?;
    fs::create_dir_all(path).map_err(|error| cache_error("directory_create_failed", error))?;
    reject_symlink(path)
}

fn reject_symlink(path: &Path) -> Result<(), CheatSourceError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| cache_error("path_inaccessible", error))?;
    if metadata.file_type().is_symlink() {
        Err(cache_error(
            "unsafe_symlink",
            format!("symlink refused: {}", path.display()),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn safe_regular_or_directory(
    path: &Path,
    directory: bool,
) -> Result<(), CheatSourceError> {
    validate_cache_path_for_read(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| cache_error("snapshot_inaccessible", error))?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(cache_error(
            "unsafe_snapshot_path",
            format!("unexpected snapshot path type: {}", path.display()),
        ));
    }
    Ok(())
}

pub(super) fn validate_snapshot_name(value: &str) -> Result<(), CheatSourceError> {
    if !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
        && value != "."
        && value != ".."
    {
        Ok(())
    } else {
        Err(cache_error(
            "unsafe_snapshot_name",
            "snapshot/source identifier is unsafe",
        ))
    }
}
fn validate_relative_path(value: &str) -> Result<(), CheatSourceError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(cache_error(
            "unsafe_relative_path",
            "relative cache path contains traversal or special components",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn validate_catalogue_prefix(value: &str) -> Result<(), CheatSourceError> {
    if value.is_empty() {
        Ok(())
    } else {
        validate_relative_path(value)
    }
}

fn secure_create(path: &Path) -> Result<File, CheatSourceError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .map_err(|error| cache_error("file_create_failed", error))
}
fn sync_directory(path: &Path) -> Result<(), CheatSourceError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| cache_error("directory_sync_failed", error))
}
pub(super) fn atomic_write_json(
    path: &Path,
    value: &impl Serialize,
) -> Result<(), CheatSourceError> {
    let parent = path
        .parent()
        .ok_or_else(|| cache_error("metadata_path_invalid", "metadata path has no parent"))?;
    let temporary = parent.join(format!(
        ".metadata-{}-{}.tmp",
        std::process::id(),
        now_nanos()
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| cache_error("metadata_serialize_failed", error))?;
    let mut file = secure_create(&temporary)?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(cache_error("metadata_write_failed", error));
    }
    drop(file);
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        cache_error("metadata_publish_failed", error)
    })?;
    sync_directory(parent)
}

fn atomic_write_json_new(path: &Path, value: &impl Serialize) -> Result<bool, CheatSourceError> {
    let parent = path
        .parent()
        .ok_or_else(|| cache_error("metadata_path_invalid", "metadata path has no parent"))?;
    let temporary = parent.join(format!(
        ".manifest-{}-{}.tmp",
        std::process::id(),
        now_nanos()
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| cache_error("metadata_serialize_failed", error))?;
    if bytes.len() > CHEAT_SOURCE_MANIFEST_BYTES_LIMIT {
        return Err(cache_error(
            "manifest_size_limit",
            format!(
                "serialized manifest exceeds {} bytes",
                CHEAT_SOURCE_MANIFEST_BYTES_LIMIT
            ),
        ));
    }
    let mut file = secure_create(&temporary)?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(cache_error("metadata_write_failed", error));
    }
    drop(file);
    let published = match fs::hard_link(&temporary, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(cache_error("metadata_publish_failed", error));
        }
    };
    fs::remove_file(&temporary)
        .map_err(|error| cache_error("metadata_temp_cleanup_failed", error))?;
    sync_directory(parent)?;
    Ok(published)
}

fn record_fetch_failure(
    source: &CheatSourceDefinition,
    cache_root: &Path,
    error: &CheatSourceError,
) {
    let path = cache_root.join(&source.source_id).join(METADATA_FILE);
    if validate_cache_path_for_read(&path).is_err() {
        return;
    }
    let mut metadata = if path.exists() {
        if reject_symlink(&path).is_err() {
            return;
        }
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        let Ok(metadata) = serde_json::from_slice::<CheatSourceCacheMetadata>(&bytes) else {
            return;
        };
        metadata
    } else {
        CheatSourceCacheMetadata {
            format_version: CHEAT_SOURCE_RESULT_SCHEMA_VERSION,
            source_id: source.source_id.clone(),
            current_snapshot: None,
            manifest: None,
            last_fetch_succeeded: false,
            last_error: None,
            last_error_at_unix_seconds: None,
        }
    };
    if metadata.source_id != source.source_id {
        return;
    }
    metadata.last_fetch_succeeded = false;
    metadata.last_error = Some(error.clone());
    metadata.last_error_at_unix_seconds = Some(now_seconds());
    let _ = atomic_write_json(&path, &metadata);
}

fn registry_error(code: &str, message: impl std::fmt::Display) -> CheatSourceError {
    CheatSourceError::new(CheatSourceErrorStage::Registry, code, message.to_string())
}
pub(super) fn cache_error(code: &str, message: impl std::fmt::Display) -> CheatSourceError {
    CheatSourceError::new(CheatSourceErrorStage::Cache, code, message.to_string())
}
fn extraction_error(code: &str, message: impl std::fmt::Display) -> CheatSourceError {
    CheatSourceError::new(CheatSourceErrorStage::Extraction, code, message.to_string())
}

fn status_for_error(error: &CheatSourceError) -> CheatCatalogueStatus {
    if error.code == "cancelled" {
        CheatCatalogueStatus::Cancelled
    } else if error.code.contains("limit") || error.code == "download_too_large" {
        CheatCatalogueStatus::ResourceLimitReached
    } else if error.code.contains("schema") || error.code == "metadata_binding_invalid" {
        CheatCatalogueStatus::UnsupportedSchema
    } else if error.code.contains("manifest") || error.code.contains("metadata") {
        CheatCatalogueStatus::InvalidManifest
    } else if error.stage == CheatSourceErrorStage::Validation {
        CheatCatalogueStatus::VerificationFailed
    } else {
        CheatCatalogueStatus::RetrievalFailed
    }
}

struct WorkCleanup(PathBuf);
impl Drop for WorkCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    struct TempDirectory(PathBuf);
    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-cheat-source-test-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakeTransport {
        responses: RefCell<Vec<Result<FakeResponse, CheatSourceError>>>,
        calls: RefCell<Vec<String>>,
    }
    struct FakeResponse {
        metadata: CheatSourceHttpResponse,
        body: Vec<u8>,
    }
    impl FakeTransport {
        fn new(responses: Vec<Result<FakeResponse, CheatSourceError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl CheatSourceTransport for FakeTransport {
        fn get(
            &self,
            url: &str,
            maximum_bytes: u64,
            destination: &mut dyn Write,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            self.calls.borrow_mut().push(url.to_string());
            if url.contains("api.github.com/repos/libretro/libretro-database/commits/") {
                let body = format!(r#"{{"sha":"{}"}}"#, "1".repeat(40)).into_bytes();
                destination.write_all(&body).unwrap();
                return Ok(CheatSourceHttpResponse {
                    status: 200,
                    content_type: Some("application/json".into()),
                    content_encoding: None,
                    content_length: Some(body.len() as u64),
                    location: None,
                    etag: None,
                    last_modified: None,
                    downloaded_bytes: body.len() as u64,
                });
            }
            let mut response = self.responses.borrow_mut().remove(0)?;
            if response.body.len() as u64 > maximum_bytes {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Download,
                    "download_too_large",
                    format!(
                        "response size {} bytes exceeds configured limit {maximum_bytes} bytes",
                        response.body.len()
                    ),
                ));
            }
            if response
                .metadata
                .content_length
                .is_some_and(|value| value != response.body.len() as u64)
            {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Download,
                    "incomplete_response",
                    "fake response was truncated",
                ));
            }
            if (200..300).contains(&response.metadata.status) {
                destination.write_all(&response.body).unwrap();
                response.metadata.downloaded_bytes = response.body.len() as u64;
            }
            Ok(response.metadata)
        }
    }

    fn response(body: Vec<u8>) -> FakeResponse {
        let metadata = CheatSourceHttpResponse {
            status: 200,
            content_type: Some("application/zip".into()),
            content_encoding: None,
            content_length: Some(body.len() as u64),
            location: None,
            etag: Some("fixture".into()),
            last_modified: None,
            downloaded_bytes: 0,
        };
        FakeResponse { metadata, body }
    }
    fn zip(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        for (name, bytes, mode) in entries {
            let mut options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            if let Some(mode) = mode {
                options = options.unix_permissions(*mode);
            }
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn valid_zip() -> Vec<u8> {
        zip(&[(
            "libretro-database-1.22.1/cht/Nintendo - SNES/Game.cht",
            b"cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_code = \"00FF\"\n",
            None,
        )])
    }
    fn options(root: &Path) -> CheatSourceFetchOptions {
        CheatSourceFetchOptions {
            cache_root: root.to_path_buf(),
            force_refresh: false,
            offline: false,
            expected_sha256: None,
            max_download_bytes: Some(2 * 1024 * 1024),
            cancellation: None,
        }
    }

    #[test]
    fn registry_is_stable_and_valid() {
        let first = trusted_retroarch_cheat_sources();
        let second = trusted_retroarch_cheat_sources();
        assert_eq!(first, second);
        let ids = first
            .iter()
            .map(|source| &source.source_id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), first.len());
        for source in first {
            validate_source(&source).unwrap();
        }
        assert_eq!(
            resolve_source("unknown-source").unwrap_err().code,
            "unknown_source"
        );
    }

    #[test]
    fn cache_lock_blocks_retrieval_before_transport_or_publication() {
        let temp = TempDirectory::new();
        let _held = LockedCheatCache::acquire_required(&temp.0).unwrap();
        let transport = FakeTransport::new(Vec::new());
        let error =
            fetch_retroarch_cheat_source("libretro-buildbot-cheats", &options(&temp.0), &transport)
                .unwrap_err();
        assert_eq!(error.code, "cache_lock_timeout");
        assert!(transport.calls.borrow().is_empty());
        assert!(!temp.0.join("libretro-buildbot-cheats").exists());
    }

    #[test]
    fn url_policy_rejects_non_https_auth_ports_and_wrong_hosts() {
        let mut source = resolve_source("libretro-buildbot-cheats").unwrap();
        for url in [
            "http://buildbot.libretro.com/a.zip",
            "https://user@buildbot.libretro.com/a.zip",
            "https://buildbot.libretro.com:444/a.zip",
            "https://example.com/a.zip",
            "https://127.0.0.1/a.zip",
        ] {
            source.download_url = url.into();
            assert!(validate_source(&source).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn ip_policy_rejects_loopback_private_and_link_local() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "fc00::1",
        ] {
            assert!(is_non_public_ip(ip.parse().unwrap()));
        }
        assert!(!is_non_public_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn successful_fetch_publishes_valid_snapshot_and_reuses_it() {
        let temp = TempDirectory::new();
        let transport = FakeTransport::new(vec![Ok(response(valid_zip()))]);
        let fetched =
            fetch_retroarch_cheat_source("libretro-buildbot-cheats", &options(&temp.0), &transport)
                .unwrap();
        assert_eq!(fetched.status, CheatSourceFetchStatus::Fetched);
        assert!(fetched.manifest.validation_complete);
        assert_eq!(fetched.manifest.catalogue_file_count, 1);
        assert_eq!(fetched.manifest.valid_cheat_count, 1);
        assert_eq!(fetched.manifest.archive_entry_count, 1);
        assert_eq!(fetched.manifest.resolved_revision, "1".repeat(40));
        assert_eq!(
            fetched.manifest.pinned_version.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert!(fetched.manifest.source_url.ends_with(&"1".repeat(40)));
        assert!(fetched.manifest.extracted_bytes > 0);
        let snapshot_inspection = inspect_retroarch_cheat_source_snapshot(Path::new(
            &fetched.immutable_snapshot_path.display,
        ))
        .unwrap();
        assert!(snapshot_inspection.setup_usable);
        let provenance_json = serde_json::to_value(snapshot_inspection.manifest).unwrap();
        assert_eq!(provenance_json["source_id"], "libretro-buildbot-cheats");
        assert!(provenance_json["archive_sha256"].as_str().unwrap().len() == 64);
        let reused = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![]),
        )
        .unwrap();
        assert_eq!(reused.status, CheatSourceFetchStatus::CacheReused);
        assert!(reused.from_cache);
    }

    #[test]
    fn verified_snapshot_with_bounded_entry_exclusions_remains_usable() {
        let temp = TempDirectory::new();
        let archive = zip(&[
            (
                "libretro-database-1/cht/Sega - Mega Drive - Genesis/Alien 3 (USA, Europe).cht",
                b"cheats = 1\ncheat0_desc = \"Lives\"\ncheat0_code = \"00FF\"\n",
                None,
            ),
            (
                "libretro-database-1/cht/Sega - Mega Drive - Genesis/Broken.cht",
                b"not an assignment\n",
                None,
            ),
            (
                "libretro-database-1/cht/Nintendo - Super Nintendo Entertainment System/Unsupported.cht",
                b"cheats = 1\n\xff",
                None,
            ),
        ]);
        let fetched = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(archive))]),
        )
        .unwrap();
        assert!(fetched.manifest.validation_complete);
        assert_eq!(fetched.manifest.catalogue_file_count, 3);
        assert_eq!(fetched.manifest.indexed_file_count, 1);
        assert_eq!(fetched.manifest.malformed_cheat_count, 1);
        assert_eq!(fetched.manifest.excluded_unsupported_count, 1);
        assert_eq!(fetched.manifest.exclusion_examples.len(), 2);
        assert!(
            inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0)
                .unwrap()
                .setup_usable
        );
    }

    #[test]
    fn legacy_snapshot_manifest_remains_readable_after_schema_upgrade() {
        let temp = TempDirectory::new();
        let fetched = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let mut value = serde_json::to_value(fetched.manifest).unwrap();
        value["format_version"] = serde_json::json!(CHEAT_SOURCE_LEGACY_SCHEMA_VERSION);
        value
            .as_object_mut()
            .unwrap()
            .remove("canonical_repository_url");
        value.as_object_mut().unwrap().remove("resolved_revision");
        let legacy: CheatSourceManifest = serde_json::from_value(value).unwrap();
        assert!(supported_cheat_source_schema(legacy.format_version));
        assert!(legacy.canonical_repository_url.is_empty());
        assert!(legacy.resolved_revision.is_empty());

        let mut schema_two = serde_json::to_value(legacy).unwrap();
        schema_two["format_version"] = serde_json::json!(2);
        let schema_two: CheatSourceManifest = serde_json::from_value(schema_two).unwrap();
        assert!(supported_cheat_source_schema(schema_two.format_version));
    }

    #[test]
    fn revised_download_bound_accepts_realistic_size_and_still_fails_closed() {
        let realistic = 129 * 1024 * 1024;
        assert!(realistic > 128 * 1024 * 1024);
        validate_downloaded_size(realistic, CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT).unwrap();
        let error = validate_downloaded_size(
            CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT + 1,
            CHEAT_SOURCE_DEFAULT_DOWNLOAD_LIMIT,
        )
        .unwrap_err();
        assert_eq!(error.code, "download_too_large");
        assert!(error.message.contains("268435457"));
        assert!(error.message.contains("268435456"));
        assert_eq!(CHEAT_SOURCE_TIMEOUT_SECONDS, 180);
    }

    #[test]
    fn cancellation_before_activation_retains_the_previous_snapshot() {
        let temp = TempDirectory::new();
        let initial = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let cancellation = CheatSourceCancellation::default();
        cancellation.cancel();
        let mut update = options(&temp.0);
        update.force_refresh = true;
        update.cancellation = Some(cancellation);
        let transport = FakeTransport::new(vec![Ok(response(zip(&[(
            "libretro-database-new/cht/Sega - Mega Drive - Genesis/Alien 3 (USA, Europe).cht",
            b"cheats = 1\ncheat0_desc = \"Lives\"\ncheat0_code = \"00FF\"\n",
            None,
        )])))]);
        let error = fetch_retroarch_cheat_source("libretro-buildbot-cheats", &update, &transport)
            .unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert!(transport.calls.borrow().is_empty());
        let inspection =
            inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0).unwrap();
        assert_eq!(
            inspection.manifest.unwrap().archive_sha256,
            initial.manifest.archive_sha256
        );
    }

    #[test]
    fn offline_reuses_cache_and_missing_cache_fails_without_network() {
        let temp = TempDirectory::new();
        let transport = FakeTransport::new(vec![Ok(response(valid_zip()))]);
        fetch_retroarch_cheat_source("libretro-buildbot-cheats", &options(&temp.0), &transport)
            .unwrap();
        let mut offline = options(&temp.0);
        offline.offline = true;
        let unused = FakeTransport::new(vec![]);
        assert_eq!(
            fetch_retroarch_cheat_source("libretro-buildbot-cheats", &offline, &unused)
                .unwrap()
                .status,
            CheatSourceFetchStatus::OfflineReused
        );
        assert!(unused.calls.borrow().is_empty());
        let empty = TempDirectory::new();
        offline.cache_root = empty.0.clone();
        assert_eq!(
            fetch_retroarch_cheat_source("libretro-buildbot-cheats", &offline, &unused)
                .unwrap_err()
                .code,
            "offline_cache_unavailable"
        );
    }

    #[test]
    fn expected_hash_is_enforced_and_failure_preserves_snapshot() {
        let temp = TempDirectory::new();
        fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let before = inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0)
            .unwrap()
            .manifest
            .unwrap()
            .archive_sha256;
        let mut refresh = options(&temp.0);
        refresh.force_refresh = true;
        refresh.expected_sha256 = Some("00".repeat(32));
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &refresh,
                &FakeTransport::new(vec![Ok(response(valid_zip()))])
            )
            .unwrap_err()
            .code,
            "sha256_mismatch"
        );
        let after = inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0)
            .unwrap()
            .manifest
            .unwrap()
            .archive_sha256;
        assert_eq!(before, after);

        let mut offline = options(&temp.0);
        offline.offline = true;
        offline.expected_sha256 = Some("11".repeat(32));
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &offline,
                &FakeTransport::new(vec![]),
            )
            .unwrap_err()
            .code,
            "offline_cache_unavailable"
        );
    }

    #[test]
    fn cached_file_mutation_is_detected_before_reuse() {
        let temp = TempDirectory::new();
        let fetched = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let cheat = temp
            .0
            .join("libretro-buildbot-cheats/snapshots")
            .join(&fetched.manifest.archive_sha256)
            .join("libretro-database-1.22.1/cht/Nintendo - SNES/Game.cht");
        fs::write(cheat, "cheats = 0\n").unwrap();
        assert_eq!(
            inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0)
                .unwrap_err()
                .code,
            "catalogue_manifest_mismatch"
        );
    }

    #[test]
    fn network_failures_and_redirects_are_structured() {
        let temp = TempDirectory::new();
        for status in [404, 500] {
            let mut value = response(Vec::new());
            value.metadata.status = status;
            assert_eq!(
                fetch_retroarch_cheat_source(
                    "libretro-buildbot-cheats",
                    &options(&temp.0),
                    &FakeTransport::new(vec![Ok(value)])
                )
                .unwrap_err()
                .code,
                "http_status"
            );
        }
        let mut redirect = response(Vec::new());
        redirect.metadata.status = 302;
        redirect.metadata.location = Some("https://localhost/archive.zip".into());
        assert!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &options(&temp.0),
                &FakeTransport::new(vec![Ok(redirect)])
            )
            .is_err()
        );
    }

    #[test]
    fn byte_limit_truncation_wrong_magic_and_corrupt_zip_are_rejected() {
        let temp = TempDirectory::new();
        let mut small = options(&temp.0);
        small.max_download_bytes = Some(3);
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &small,
                &FakeTransport::new(vec![Ok(response(valid_zip()))])
            )
            .unwrap_err()
            .code,
            "download_too_large"
        );
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &options(&temp.0),
                &FakeTransport::new(vec![Ok(response(b"<html>error</html>".to_vec()))])
            )
            .unwrap_err()
            .code,
            "archive_magic_invalid"
        );
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &options(&temp.0),
                &FakeTransport::new(vec![Ok(response(b"PK\x03\x04broken".to_vec()))])
            )
            .unwrap_err()
            .code,
            "corrupt_zip"
        );
    }

    #[test]
    fn extraction_rejects_traversal_absolute_windows_and_symlinks() {
        for (name, mode) in [("../escape", None), ("/absolute", None), ("C:/drive", None)] {
            let temp = TempDirectory::new();
            let archive_path = temp.0.join("bad.zip");
            fs::write(&archive_path, zip(&[(name, b"x", mode)])).unwrap();
            let output = temp.0.join("out");
            fs::create_dir(&output).unwrap();
            assert!(
                extract_zip_safely(&archive_path, &output, &options(&temp.0)).is_err(),
                "accepted {name}"
            );
        }
        for mode in [0o120777, 0o060644, 0o020644, 0o010644, 0o140777] {
            assert_eq!(
                validate_unix_entry_mode(mode, "special").unwrap_err().code,
                "special_entry_rejected"
            );
        }
    }

    #[test]
    fn extraction_rejects_duplicate_case_collision_and_does_not_expand_nested_zip() {
        let temp = TempDirectory::new();
        let archive_path = temp.0.join("collision.zip");
        fs::write(&archive_path, zip(&[("a", b"1", None), ("A", b"2", None)])).unwrap();
        let output = temp.0.join("out");
        fs::create_dir(&output).unwrap();
        assert_eq!(
            extract_zip_safely(&archive_path, &output, &options(&temp.0))
                .unwrap_err()
                .code,
            "case_fold_collision"
        );
        let nested = zip(&[("inside", b"payload", None)]);
        let archive_path = temp.0.join("nested-container.zip");
        fs::write(&archive_path, zip(&[("nested.zip", &nested, None)])).unwrap();
        let output = temp.0.join("nested-out");
        fs::create_dir(&output).unwrap();
        extract_zip_safely(&archive_path, &output, &options(&temp.0)).unwrap();
        assert!(output.join("nested.zip").is_file());
        assert!(!output.join("inside").exists());
    }

    #[test]
    fn every_extraction_bound_fails_closed() {
        assert_eq!(
            validate_extraction_sizes(CHEAT_SOURCE_FILE_SIZE_LIMIT + 1, 1, 1, "large")
                .unwrap_err()
                .code,
            "file_size_limit_exceeded"
        );
        assert_eq!(
            validate_extraction_sizes(1, 1, CHEAT_SOURCE_EXPANDED_SIZE_LIMIT + 1, "total")
                .unwrap_err()
                .code,
            "expanded_size_limit_exceeded"
        );
        assert_eq!(
            validate_extraction_sizes(
                COMPRESSION_RATIO_LIMIT + 1,
                1,
                COMPRESSION_RATIO_LIMIT + 1,
                "ratio"
            )
            .unwrap_err()
            .code,
            "compression_ratio_limit_exceeded"
        );
        let deep = (0..=PATH_COMPONENT_LIMIT)
            .map(|_| "x")
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            validate_archive_entry_name(&deep).unwrap_err().code,
            "unsafe_entry_path"
        );
        assert_eq!(
            validate_entry_count(CHEAT_SOURCE_ENTRY_LIMIT + 1)
                .unwrap_err()
                .code,
            "entry_limit_exceeded"
        );
    }

    #[test]
    fn oversized_update_retains_a_usable_active_snapshot() {
        let temp = TempDirectory::new();
        let initial = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let mut update = options(&temp.0);
        update.force_refresh = true;
        update.max_download_bytes = Some(3);
        let failure = fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &update,
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap_err();
        assert_eq!(failure.code, "download_too_large");
        let inspection =
            inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0).unwrap();
        assert!(inspection.setup_usable);
        assert_eq!(
            inspection.manifest.unwrap().archive_sha256,
            initial.manifest.archive_sha256
        );
        assert_eq!(inspection.last_error.unwrap().code, "download_too_large");
    }

    #[test]
    fn oversized_manifest_is_never_published() {
        let temp = TempDirectory::new();
        let path = temp.0.join("oversized.json");
        let oversized = vec!["x".repeat(CHEAT_SOURCE_MANIFEST_BYTES_LIMIT)];
        let error = atomic_write_json_new(&path, &oversized).unwrap_err();
        assert_eq!(error.code, "manifest_size_limit");
        assert!(!path.exists());
    }

    #[test]
    fn failed_download_removes_unique_staging_files_and_publishes_nothing() {
        let temp = TempDirectory::new();
        let error =
            CheatSourceError::new(CheatSourceErrorStage::Network, "timeout", "fixture timeout");
        assert_eq!(
            fetch_retroarch_cheat_source(
                "libretro-buildbot-cheats",
                &options(&temp.0),
                &FakeTransport::new(vec![Err(error)])
            )
            .unwrap_err()
            .code,
            "timeout"
        );
        let source = temp.0.join("libretro-buildbot-cheats");
        let metadata: CheatSourceCacheMetadata =
            serde_json::from_slice(&fs::read(source.join(METADATA_FILE)).unwrap()).unwrap();
        assert!(metadata.current_snapshot.is_none());
        assert!(!metadata.last_fetch_succeeded);
        assert_eq!(metadata.last_error.unwrap().code, "timeout");
        assert_eq!(
            fs::read_dir(source.join(STAGING_DIRECTORY))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(source.join(SNAPSHOTS_DIRECTORY))
                .unwrap()
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_and_inspection_reject_symlinks_and_inspection_does_not_write() {
        use std::os::unix::fs::symlink;
        let temp = TempDirectory::new();
        let outside = TempDirectory::new();
        let link = temp.0.join("cache-link");
        symlink(&outside.0, &link).unwrap();
        assert!(inspect_retroarch_cheat_source("libretro-buildbot-cheats", &link).is_err());
        fetch_retroarch_cheat_source(
            "libretro-buildbot-cheats",
            &options(&temp.0),
            &FakeTransport::new(vec![Ok(response(valid_zip()))]),
        )
        .unwrap();
        let metadata = temp.0.join("libretro-buildbot-cheats").join(METADATA_FILE);
        let before = fs::metadata(&metadata).unwrap().modified().unwrap();
        inspect_retroarch_cheat_source("libretro-buildbot-cheats", &temp.0).unwrap();
        assert_eq!(before, fs::metadata(&metadata).unwrap().modified().unwrap());
    }
}
