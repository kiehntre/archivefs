//! External provider for the maintained `xenia-canary/game-patches`
//! upstream dataset.
//!
//! Provider responsibilities stop at retrieving, caching, and normalising
//! `.patch.toml` files as inert data (via
//! [`super::xenia_patch_document`]). This module has no Xenia profile
//! path, destination, staging, transaction, apply, or rollback API - the
//! Xenia adapter (`xenia_install_plan.rs`) consumes these results later
//! and remains the sole owner of installation.
//!
//! Retrieval never clones the repository. A single, cached, revision-
//! pinned index of `patches/*.patch.toml` paths (fetched via GitHub's Git
//! Trees API) is filtered locally for the requested Title ID, and only
//! the exact matching files are then downloaded individually via
//! `raw.githubusercontent.com`, each independently and immutably cached
//! by (commit, path).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::xenia_patch_document::{XeniaPatchDocument, parse_xenia_patch_toml};
use super::{
    CheatSourceError, CheatSourceHttpResponse, CheatSourceTransferContext, CheatSourceTransport,
    HttpsCheatSourceTransport,
};

pub const XENIA_PROVIDER_ID: &str = "xenia_canary_game_patches";
pub const XENIA_PROVIDER_NAME: &str = "Xenia Canary game-patches";
pub const XENIA_UPSTREAM_REPOSITORY: &str = "xenia-canary/game-patches";
pub const XENIA_UPSTREAM_ATTRIBUTION: &str =
    "Patch definitions from the xenia-canary/game-patches upstream dataset.";
/// The repository publishes no LICENSE file (confirmed via the GitHub API
/// license field, which is `null`) - ArchiveFS records this honestly
/// rather than asserting terms upstream never declared.
pub const XENIA_UPSTREAM_LICENSE: &str = "No LICENSE file is published by xenia-canary/game-patches at the time of writing; contents remain the property of their individual authors pending upstream clarification.";

pub const XENIA_PROVIDER_INDEX_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const XENIA_PROVIDER_FILE_MAX_BYTES: u64 = 256 * 1024;
pub const XENIA_PROVIDER_MAX_INDEX_ENTRIES: usize = 20_000;
pub const XENIA_PROVIDER_MAX_MATCHED_FILES: usize = 32;
pub const XENIA_PROVIDER_TIMEOUT_SECONDS: u64 = 20;
pub const XENIA_PROVIDER_INDEX_FRESH_SECONDS: u64 = 24 * 60 * 60;
pub const XENIA_PROVIDER_MIN_REFRESH_SECONDS: u64 = 30;
const XENIA_PROVIDER_CACHE_SCHEMA_VERSION: u32 = 1;
const XENIA_PROVIDER_COMMIT_URL: &str =
    "https://api.github.com/repos/xenia-canary/game-patches/commits/main";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XeniaProviderDocument {
    /// Repository-relative path, e.g.
    /// `"patches/415607D2 - Quake 4.patch.toml"`.
    pub source_path: String,
    pub document: XeniaPatchDocument,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XeniaProviderResult {
    pub provider_id: String,
    pub provider_display_name: String,
    pub source_repository: String,
    pub source_commit: String,
    pub retrieved_at_unix_seconds: u64,
    pub title_id: String,
    pub documents: Vec<XeniaProviderDocument>,
    pub attribution: String,
    pub license: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct XeniaProviderFetchOptions {
    pub cache_root: PathBuf,
    pub force_refresh: bool,
    pub offline: bool,
    pub now_unix_seconds: u64,
}

impl XeniaProviderFetchOptions {
    pub fn with_default_cache(now_unix_seconds: u64) -> Result<Self, XeniaProviderFetchError> {
        Ok(Self {
            cache_root: default_xenia_provider_cache_root()?,
            force_refresh: false,
            offline: false,
            now_unix_seconds,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XeniaProviderFetchStatus {
    Downloaded,
    FreshCache,
    RateLimitedCache,
    StaleCacheFallback,
    OfflineCache,
}

#[derive(Debug, Clone, PartialEq)]
pub struct XeniaProviderFetchResult {
    pub result: XeniaProviderResult,
    pub status: XeniaProviderFetchStatus,
    /// Populated only when a refresh was skipped or failed but a
    /// validated cache remains usable.
    pub refresh_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XeniaProviderFetchErrorKind {
    InvalidTitleId,
    CacheUnavailable,
    CacheUnsafe,
    CacheInvalid,
    Network,
    HttpStatus,
    ResponseTooLarge,
    Parse,
    Offline,
}

#[derive(Debug, Clone, PartialEq)]
pub struct XeniaProviderFetchError {
    pub kind: XeniaProviderFetchErrorKind,
    pub detail: String,
}

impl std::fmt::Display for XeniaProviderFetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for XeniaProviderFetchError {}

fn fetch_error(
    kind: XeniaProviderFetchErrorKind,
    detail: impl Into<String>,
) -> XeniaProviderFetchError {
    XeniaProviderFetchError {
        kind,
        detail: detail.into(),
    }
}

pub fn default_xenia_provider_cache_root() -> Result<PathBuf, XeniaProviderFetchError> {
    let database = crate::default_database_path().map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider cache root unavailable: {error}"),
        )
    })?;
    Ok(database
        .parent()
        .expect("default database path always has a parent")
        .join("xenia-provider-cache"))
}

fn validate_cache_root(path: &Path) -> Result<(), XeniaProviderFetchError> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::CacheUnsafe,
            "Xenia provider cache root must be an absolute non-filesystem-root path",
        ));
    }
    super::cheat_sources::validate_cache_path_for_read(path).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnsafe,
            format!("Xenia provider cache path is unsafe: {error}"),
        )
    })
}

fn validate_title_id(title_id: &str) -> Result<(), XeniaProviderFetchError> {
    if title_id.len() == 8 && title_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(fetch_error(
            XeniaProviderFetchErrorKind::InvalidTitleId,
            "Title ID must be exactly eight hex characters",
        ))
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn fetch_xenia_provider_patches(
    title_id: &str,
    options: &XeniaProviderFetchOptions,
) -> Result<XeniaProviderFetchResult, XeniaProviderFetchError> {
    fetch_xenia_provider_patches_with_transport(
        title_id,
        options,
        &HttpsCheatSourceTransport::new(),
    )
}

/// Fetches (or reuses cached) patch files for one exact Title ID. The
/// transport parameter keeps tests deterministic and ensures normal tests
/// never require internet access.
pub fn fetch_xenia_provider_patches_with_transport(
    title_id: &str,
    options: &XeniaProviderFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<XeniaProviderFetchResult, XeniaProviderFetchError> {
    let title_id = title_id.to_ascii_uppercase();
    validate_title_id(&title_id)?;

    let index = ensure_index(options, transport)?;
    let matching_paths: Vec<&String> = index
        .paths
        .iter()
        .filter(|path| path_title_id(path).as_deref() == Some(title_id.as_str()))
        .take(XENIA_PROVIDER_MAX_MATCHED_FILES)
        .collect();

    let mut documents = Vec::new();
    let mut warnings = Vec::new();
    for path in matching_paths {
        match ensure_file(&index.commit, path, options, transport) {
            Ok(raw_text) => {
                let document = parse_xenia_patch_toml(&raw_text);
                if !document.is_fatally_malformed() && document.title_id != title_id {
                    warnings.push(format!(
                        "{path}: file declares Title ID {}, not the requested {title_id}; skipped",
                        document.title_id
                    ));
                    continue;
                }
                documents.push(XeniaProviderDocument {
                    source_path: path.clone(),
                    document,
                });
            }
            Err(error) => warnings.push(format!("{path}: {error}")),
        }
    }

    Ok(XeniaProviderFetchResult {
        result: XeniaProviderResult {
            provider_id: XENIA_PROVIDER_ID.to_string(),
            provider_display_name: XENIA_PROVIDER_NAME.to_string(),
            source_repository: XENIA_UPSTREAM_REPOSITORY.to_string(),
            source_commit: index.commit,
            retrieved_at_unix_seconds: options.now_unix_seconds,
            title_id,
            documents,
            attribution: XENIA_UPSTREAM_ATTRIBUTION.to_string(),
            license: XENIA_UPSTREAM_LICENSE.to_string(),
            warnings,
        },
        status: index.status,
        refresh_error: index.refresh_error,
    })
}

/// Extracts the leading eight-hex-character Title ID from a repository
/// path's filename, matching Xenia's own patch filename convention
/// (`^[A-Fa-f0-9]{8}.*\.patch\.toml$`).
fn path_title_id(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let prefix = name.get(0..8)?;
    prefix
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then(|| prefix.to_ascii_uppercase())
}

fn transfer_context() -> CheatSourceTransferContext<'static> {
    CheatSourceTransferContext {
        cancellation: None,
        progress: None,
        attempt: 1,
        overall_timeout: Duration::from_secs(XENIA_PROVIDER_TIMEOUT_SECONDS),
    }
}

fn map_transport_error(error: CheatSourceError) -> XeniaProviderFetchError {
    let kind = if error.code == "download_too_large" {
        XeniaProviderFetchErrorKind::ResponseTooLarge
    } else {
        XeniaProviderFetchErrorKind::Network
    };
    fetch_error(kind, format!("Xenia provider request failed: {error}"))
}

fn check_http_ok(response: &CheatSourceHttpResponse) -> Result<(), XeniaProviderFetchError> {
    if response.status != 200 {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::HttpStatus,
            format!("Xenia provider request returned HTTP {}", response.status),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Index (list of patches/*.patch.toml paths at one pinned commit)
// ---------------------------------------------------------------------

struct IndexOutcome {
    commit: String,
    paths: Vec<String>,
    status: XeniaProviderFetchStatus,
    refresh_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XeniaIndexCache {
    schema_version: u32,
    commit: String,
    fetched_at_unix_seconds: u64,
    paths: Vec<String>,
}

fn index_cache_path(cache_root: &Path) -> Result<PathBuf, XeniaProviderFetchError> {
    validate_cache_root(cache_root)?;
    Ok(cache_root.join("index.json"))
}

fn ensure_index(
    options: &XeniaProviderFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<IndexOutcome, XeniaProviderFetchError> {
    let cache_path = index_cache_path(&options.cache_root)?;
    let cached = load_index_cache(&cache_path)?;

    if let Some(cache) = &cached {
        let age = options
            .now_unix_seconds
            .saturating_sub(cache.fetched_at_unix_seconds);
        if !options.force_refresh && age <= XENIA_PROVIDER_INDEX_FRESH_SECONDS {
            return Ok(IndexOutcome {
                commit: cache.commit.clone(),
                paths: cache.paths.clone(),
                status: XeniaProviderFetchStatus::FreshCache,
                refresh_error: None,
            });
        }
        if options.offline {
            return Ok(IndexOutcome {
                commit: cache.commit.clone(),
                paths: cache.paths.clone(),
                status: XeniaProviderFetchStatus::OfflineCache,
                refresh_error: None,
            });
        }
        if age < XENIA_PROVIDER_MIN_REFRESH_SECONDS {
            return Ok(IndexOutcome {
                commit: cache.commit.clone(),
                paths: cache.paths.clone(),
                status: XeniaProviderFetchStatus::RateLimitedCache,
                refresh_error: Some(format!(
                    "Refresh limited to one request every {XENIA_PROVIDER_MIN_REFRESH_SECONDS} seconds"
                )),
            });
        }
    } else if options.offline {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Offline,
            "no cached patch index is available while offline",
        ));
    }

    match download_index(transport) {
        Ok((commit, paths)) => {
            store_index_cache(&options.cache_root, &cache_path, &commit, options, &paths)?;
            Ok(IndexOutcome {
                commit,
                paths,
                status: XeniaProviderFetchStatus::Downloaded,
                refresh_error: None,
            })
        }
        Err(error) => match cached {
            Some(cache) => Ok(IndexOutcome {
                commit: cache.commit,
                paths: cache.paths,
                status: XeniaProviderFetchStatus::StaleCacheFallback,
                refresh_error: Some(error.detail),
            }),
            None => Err(error),
        },
    }
}

fn download_index(
    transport: &dyn CheatSourceTransport,
) -> Result<(String, Vec<String>), XeniaProviderFetchError> {
    let commit = resolve_commit(transport)?;
    let paths = download_tree(transport, &commit)?;
    Ok((commit, paths))
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

fn resolve_commit(transport: &dyn CheatSourceTransport) -> Result<String, XeniaProviderFetchError> {
    let mut bytes = Vec::new();
    let response = transport
        .get(
            XENIA_PROVIDER_COMMIT_URL,
            4 * 1024,
            &mut bytes,
            transfer_context(),
        )
        .map_err(map_transport_error)?;
    check_http_ok(&response)?;
    let parsed: CommitResponse = serde_json::from_slice(&bytes).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            format!("commit resolution response is invalid: {error}"),
        )
    })?;
    if parsed.sha.len() != 40 || !parsed.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            "resolved commit is not an exact 40-character commit ID",
        ));
    }
    Ok(parsed.sha.to_ascii_lowercase())
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    tree: Vec<TreeEntry>,
}

fn download_tree(
    transport: &dyn CheatSourceTransport,
    commit: &str,
) -> Result<Vec<String>, XeniaProviderFetchError> {
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            "commit is not a 40-character hex SHA",
        ));
    }
    let url = format!(
        "https://api.github.com/repos/{XENIA_UPSTREAM_REPOSITORY}/git/trees/{commit}?recursive=1"
    );
    let mut bytes = Vec::new();
    let response = transport
        .get(
            &url,
            XENIA_PROVIDER_INDEX_MAX_BYTES,
            &mut bytes,
            transfer_context(),
        )
        .map_err(map_transport_error)?;
    check_http_ok(&response)?;
    let parsed: TreeResponse = serde_json::from_slice(&bytes).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            format!("repository tree response is invalid: {error}"),
        )
    })?;
    let mut paths: Vec<String> = parsed
        .tree
        .into_iter()
        .filter(|entry| {
            entry.kind == "blob"
                && entry.path.starts_with("patches/")
                && entry.path.ends_with(".patch.toml")
        })
        .map(|entry| entry.path)
        .collect();
    paths.sort();
    paths.truncate(XENIA_PROVIDER_MAX_INDEX_ENTRIES);
    Ok(paths)
}

fn load_index_cache(path: &Path) -> Result<Option<XeniaIndexCache>, XeniaProviderFetchError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(fetch_error(
                XeniaProviderFetchErrorKind::CacheUnsafe,
                format!(
                    "Xenia provider index cache entry is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fetch_error(
                XeniaProviderFetchErrorKind::CacheUnavailable,
                format!("Xenia provider index cache could not be inspected: {error}"),
            ));
        }
    }
    let bytes = fs::read(path).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider index cache could not be read: {error}"),
        )
    })?;
    let cache: XeniaIndexCache = serde_json::from_slice(&bytes).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheInvalid,
            format!("Xenia provider index cache is invalid: {error}"),
        )
    })?;
    if cache.schema_version != XENIA_PROVIDER_CACHE_SCHEMA_VERSION {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::CacheInvalid,
            "Xenia provider index cache schema is out of date",
        ));
    }
    Ok(Some(cache))
}

fn atomic_write_json(
    cache_root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), XeniaProviderFetchError> {
    validate_cache_root(cache_root)?;
    fs::create_dir_all(cache_root).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider cache directory could not be created: {error}"),
        )
    })?;
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::CacheUnsafe,
            format!("Xenia provider cache entry is unsafe: {}", path.display()),
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            fetch_error(
                XeniaProviderFetchErrorKind::CacheUnsafe,
                "Xenia provider cache filename is invalid",
            )
        })?;
    let temporary = cache_root.join(format!(".{file_name}.{}.partial", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            fetch_error(
                XeniaProviderFetchErrorKind::CacheUnavailable,
                format!("Xenia provider cache staging file could not be created: {error}"),
            )
        })?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider cache staging write failed: {error}"),
        ));
    }
    drop(file);
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider cache could not be published: {error}"),
        )
    })
}

fn store_index_cache(
    cache_root: &Path,
    path: &Path,
    commit: &str,
    options: &XeniaProviderFetchOptions,
    paths: &[String],
) -> Result<(), XeniaProviderFetchError> {
    let cache = XeniaIndexCache {
        schema_version: XENIA_PROVIDER_CACHE_SCHEMA_VERSION,
        commit: commit.to_string(),
        fetched_at_unix_seconds: options.now_unix_seconds,
        paths: paths.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&cache).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider index cache could not be serialized: {error}"),
        )
    })?;
    atomic_write_json(cache_root, path, &bytes)
}

// ---------------------------------------------------------------------
// Per-file content cache (immutable: keyed by exact commit + path)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XeniaFileCache {
    schema_version: u32,
    commit: String,
    path: String,
    raw_text: String,
}

fn file_cache_path(
    cache_root: &Path,
    commit: &str,
    path: &str,
) -> Result<PathBuf, XeniaProviderFetchError> {
    validate_cache_root(cache_root)?;
    let key = hex_sha256(format!("{commit}:{path}").as_bytes());
    Ok(cache_root.join("files").join(format!("{key}.json")))
}

fn ensure_file(
    commit: &str,
    path: &str,
    options: &XeniaProviderFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<String, XeniaProviderFetchError> {
    let cache_path = file_cache_path(&options.cache_root, commit, path)?;
    if let Some(cached) = load_file_cache(&cache_path, commit, path)? {
        return Ok(cached);
    }
    if options.offline {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Offline,
            format!("{path} is not cached and network access is disabled"),
        ));
    }
    let raw_text = download_file(transport, commit, path)?;
    store_file_cache(&options.cache_root, &cache_path, commit, path, &raw_text)?;
    Ok(raw_text)
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn download_file(
    transport: &dyn CheatSourceTransport,
    commit: &str,
    path: &str,
) -> Result<String, XeniaProviderFetchError> {
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            "commit is not a 40-character hex SHA",
        ));
    }
    if !path.starts_with("patches/") || !path.ends_with(".patch.toml") || path.contains("..") {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            "refusing to fetch a path outside the patches directory",
        ));
    }
    let url = format!(
        "https://raw.githubusercontent.com/{XENIA_UPSTREAM_REPOSITORY}/{commit}/{}",
        percent_encode_path(path)
    );
    let mut bytes = Vec::new();
    let response = transport
        .get(
            &url,
            XENIA_PROVIDER_FILE_MAX_BYTES,
            &mut bytes,
            transfer_context(),
        )
        .map_err(map_transport_error)?;
    check_http_ok(&response)?;
    String::from_utf8(bytes).map_err(|_| {
        fetch_error(
            XeniaProviderFetchErrorKind::Parse,
            "patch file is not valid UTF-8",
        )
    })
}

fn load_file_cache(
    path: &Path,
    commit: &str,
    expected_path: &str,
) -> Result<Option<String>, XeniaProviderFetchError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(fetch_error(
                XeniaProviderFetchErrorKind::CacheUnsafe,
                format!(
                    "Xenia provider file cache entry is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fetch_error(
                XeniaProviderFetchErrorKind::CacheUnavailable,
                format!("Xenia provider file cache could not be inspected: {error}"),
            ));
        }
    }
    let bytes = fs::read(path).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider file cache could not be read: {error}"),
        )
    })?;
    let cache: XeniaFileCache = serde_json::from_slice(&bytes).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheInvalid,
            format!("Xenia provider file cache is invalid: {error}"),
        )
    })?;
    if cache.schema_version != XENIA_PROVIDER_CACHE_SCHEMA_VERSION
        || cache.commit != commit
        || cache.path != expected_path
    {
        return Err(fetch_error(
            XeniaProviderFetchErrorKind::CacheInvalid,
            "Xenia provider file cache identity does not match this exact lookup",
        ));
    }
    Ok(Some(cache.raw_text))
}

fn store_file_cache(
    cache_root: &Path,
    path: &Path,
    commit: &str,
    file_path: &str,
    raw_text: &str,
) -> Result<(), XeniaProviderFetchError> {
    let cache = XeniaFileCache {
        schema_version: XENIA_PROVIDER_CACHE_SCHEMA_VERSION,
        commit: commit.to_string(),
        path: file_path.to_string(),
        raw_text: raw_text.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&cache).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider file cache could not be serialized: {error}"),
        )
    })?;
    let parent = path.parent().unwrap_or(cache_root);
    fs::create_dir_all(parent).map_err(|error| {
        fetch_error(
            XeniaProviderFetchErrorKind::CacheUnavailable,
            format!("Xenia provider cache directory could not be created: {error}"),
        )
    })?;
    atomic_write_json(parent, path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::super::CheatSourceErrorStage;
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-xenia-provider-test-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TEST_COMMIT: &str = "1111111111111111111111111111111111111111";
    const OTHER_COMMIT: &str = "2222222222222222222222222222222222222222";

    struct FakeTransport {
        responses: RefCell<Vec<Result<Vec<u8>, CheatSourceError>>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<Result<Vec<u8>, CheatSourceError>>) -> Self {
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
            _context: CheatSourceTransferContext<'_>,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            self.calls.borrow_mut().push(url.to_string());
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                panic!("FakeTransport ran out of queued responses for {url}");
            }
            let bytes = responses.remove(0)?;
            if bytes.len() as u64 > maximum_bytes {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Download,
                    "download_too_large",
                    "fixture response exceeds the maximum bytes",
                ));
            }
            destination.write_all(&bytes).unwrap();
            Ok(CheatSourceHttpResponse {
                status: 200,
                content_type: Some("application/json".to_string()),
                content_encoding: None,
                content_length: Some(bytes.len() as u64),
                location: None,
                etag: None,
                last_modified: None,
                downloaded_bytes: bytes.len() as u64,
                retry_after_seconds: None,
            })
        }
    }

    fn commit_response(sha: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "sha": sha })).unwrap()
    }

    fn tree_response(paths: &[&str]) -> Vec<u8> {
        let tree: Vec<_> = paths
            .iter()
            .map(|path| serde_json::json!({ "path": path, "type": "blob" }))
            .collect();
        serde_json::to_vec(&serde_json::json!({ "tree": tree, "truncated": false })).unwrap()
    }

    const QUAKE4_TOML: &str = r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"

[[patch]]
    name = "Performance fix"
    desc = ""
    author = "Sowa_95"
    is_enabled = false
    [[patch.be32]]
        address = 0x821b7140
        value = 0x39600001
"#;

    #[test]
    fn exact_provider_lookup_downloads_index_and_matching_file() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&[
                "patches/415607D2 - Quake 4.patch.toml",
                "patches/415607D3 - Gun.patch.toml",
            ])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let fetched =
            fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();
        assert_eq!(fetched.status, XeniaProviderFetchStatus::Downloaded);
        assert_eq!(fetched.result.documents.len(), 1);
        assert_eq!(fetched.result.documents[0].document.title_id, "415607D2");
        assert_eq!(fetched.result.source_commit, TEST_COMMIT);
        assert_eq!(transport.calls.borrow().len(), 3);
    }

    #[test]
    fn cache_hit_avoids_any_network_request() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&["patches/415607D2 - Quake 4.patch.toml"])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let first =
            fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();
        assert_eq!(first.status, XeniaProviderFetchStatus::Downloaded);

        // No responses queued for a second call: any network attempt panics.
        let empty_transport = FakeTransport::new(vec![]);
        let options_soon_after = XeniaProviderFetchOptions {
            now_unix_seconds: 1_010,
            ..options
        };
        let second = fetch_xenia_provider_patches_with_transport(
            "415607D2",
            &options_soon_after,
            &empty_transport,
        )
        .unwrap();
        assert_eq!(second.status, XeniaProviderFetchStatus::FreshCache);
        assert_eq!(second.result.documents.len(), 1);
        assert!(empty_transport.calls.borrow().is_empty());
    }

    #[test]
    fn explicit_refresh_bypasses_a_fresh_cache_and_refetches() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&["patches/415607D2 - Quake 4.patch.toml"])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();

        let refresh_transport = FakeTransport::new(vec![
            Ok(commit_response(OTHER_COMMIT)),
            Ok(tree_response(&["patches/415607D2 - Quake 4.patch.toml"])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let refresh_options = XeniaProviderFetchOptions {
            force_refresh: true,
            now_unix_seconds: 100_000,
            ..options
        };
        let refreshed = fetch_xenia_provider_patches_with_transport(
            "415607D2",
            &refresh_options,
            &refresh_transport,
        )
        .unwrap();
        assert_eq!(refreshed.status, XeniaProviderFetchStatus::Downloaded);
        assert_eq!(refreshed.result.source_commit, OTHER_COMMIT);
    }

    #[test]
    fn offline_mode_uses_only_the_cache_and_never_touches_the_network() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&["patches/415607D2 - Quake 4.patch.toml"])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();

        let offline_transport = FakeTransport::new(vec![]);
        let offline_options = XeniaProviderFetchOptions {
            offline: true,
            now_unix_seconds: 1_000_000,
            ..options
        };
        let offline_result = fetch_xenia_provider_patches_with_transport(
            "415607D2",
            &offline_options,
            &offline_transport,
        )
        .unwrap();
        assert_eq!(
            offline_result.status,
            XeniaProviderFetchStatus::OfflineCache
        );
        assert_eq!(offline_result.result.documents.len(), 1);
        assert!(offline_transport.calls.borrow().is_empty());
    }

    #[test]
    fn offline_mode_without_any_cache_is_a_clear_error() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: true,
            now_unix_seconds: 1_000,
        };
        let error = fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport)
            .unwrap_err();
        assert_eq!(error.kind, XeniaProviderFetchErrorKind::Offline);
    }

    #[test]
    fn response_size_limit_is_enforced_and_reported() {
        let cache = TempDirectory::new();
        let oversized = vec![b'a'; (XENIA_PROVIDER_INDEX_MAX_BYTES + 1) as usize];
        let transport = FakeTransport::new(vec![Ok(commit_response(TEST_COMMIT)), Ok(oversized)]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let error = fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport)
            .unwrap_err();
        assert_eq!(error.kind, XeniaProviderFetchErrorKind::ResponseTooLarge);
    }

    #[test]
    fn retrieval_failure_without_a_cache_is_a_clear_error() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![Err(CheatSourceError::new(
            CheatSourceErrorStage::Download,
            "connection_interrupted",
            "connection reset",
        ))]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let error = fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport)
            .unwrap_err();
        assert_eq!(error.kind, XeniaProviderFetchErrorKind::Network);
    }

    #[test]
    fn a_stale_cache_is_used_when_refresh_fails() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&["patches/415607D2 - Quake 4.patch.toml"])),
            Ok(QUAKE4_TOML.as_bytes().to_vec()),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();

        let failing_transport = FakeTransport::new(vec![Err(CheatSourceError::new(
            CheatSourceErrorStage::Download,
            "connection_interrupted",
            "connection reset",
        ))]);
        let refresh_options = XeniaProviderFetchOptions {
            force_refresh: true,
            now_unix_seconds: 1_000_000,
            ..options
        };
        let fallback = fetch_xenia_provider_patches_with_transport(
            "415607D2",
            &refresh_options,
            &failing_transport,
        )
        .unwrap();
        assert_eq!(
            fallback.status,
            XeniaProviderFetchStatus::StaleCacheFallback
        );
        assert!(fallback.refresh_error.is_some());
        assert_eq!(fallback.result.documents.len(), 1);
    }

    #[test]
    fn no_matching_title_id_produces_an_empty_but_successful_result() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![
            Ok(commit_response(TEST_COMMIT)),
            Ok(tree_response(&["patches/415607D3 - Gun.patch.toml"])),
        ]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let fetched =
            fetch_xenia_provider_patches_with_transport("415607D2", &options, &transport).unwrap();
        assert!(fetched.result.documents.is_empty());
        assert!(fetched.result.warnings.is_empty());
    }

    #[test]
    fn invalid_title_id_is_rejected_before_any_network_call() {
        let cache = TempDirectory::new();
        let transport = FakeTransport::new(vec![]);
        let options = XeniaProviderFetchOptions {
            cache_root: cache.0.clone(),
            force_refresh: false,
            offline: false,
            now_unix_seconds: 1_000,
        };
        let error = fetch_xenia_provider_patches_with_transport("not-hex!", &options, &transport)
            .unwrap_err();
        assert_eq!(error.kind, XeniaProviderFetchErrorKind::InvalidTitleId);
        assert!(transport.calls.borrow().is_empty());
    }
}
