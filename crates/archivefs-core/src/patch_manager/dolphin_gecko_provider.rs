//! External Gecko-code provider model and the official Dolphin upstream provider.
//!
//! Provider responsibilities stop at retrieving and validating inert code data. This module has
//! no Dolphin profile path, destination, staging, transaction, apply, or rollback API. The Dolphin
//! adapter consumes its results later and remains the sole owner of installation.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::gecko_document::parse_dolphin_ini;
use super::{CheatSourceTransferContext, CheatSourceTransport, HttpsCheatSourceTransport};

pub const DOLPHIN_UPSTREAM_PROVIDER_ID: &str = "dolphin_upstream_gamesettings";
pub const DOLPHIN_UPSTREAM_PROVIDER_NAME: &str = "Dolphin upstream GameSettings";
pub const DOLPHIN_UPSTREAM_REPOSITORY: &str = "dolphin-emu/dolphin";
pub const DOLPHIN_UPSTREAM_LICENSE: &str = "GPL-2.0-or-later";
pub const DOLPHIN_UPSTREAM_ATTRIBUTION: &str =
    "Gecko definitions from the Dolphin Emulator upstream GameSettings dataset.";
pub const GECKO_PROVIDER_MAX_RESPONSE_BYTES: u64 = 256 * 1024;
pub const GECKO_PROVIDER_TIMEOUT_SECONDS: u64 = 15;
pub const GECKO_PROVIDER_CACHE_FRESH_SECONDS: u64 = 24 * 60 * 60;
pub const GECKO_PROVIDER_MIN_REFRESH_SECONDS: u64 = 30;
const GECKO_PROVIDER_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderQuery {
    pub game_id: String,
    pub region: GeckoRegion,
    pub revision: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeckoRegion {
    Usa,
    Europe,
    Japan,
    Korea,
    Unknown(String),
}

impl GeckoRegion {
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self {
            Self::Usa => "USA",
            Self::Europe => "Europe",
            Self::Japan => "Japan",
            Self::Korea => "Korea",
            Self::Unknown(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeckoRevisionApplicability {
    Any,
    Exact(u16),
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoApplicabilityDecision {
    Offer,
    OfferWithWarning,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeckoProviderEntry {
    /// Dolphin upstream does not publish entry IDs, so this is a deterministic digest of the
    /// provider ID, exact game ID, entry name, and complete code body.
    pub provider_entry_id: String,
    pub name: String,
    pub code_lines: Vec<String>,
    pub notes: Vec<String>,
    pub region: GeckoRegion,
    pub revision_applicability: GeckoRevisionApplicability,
    pub parse_warnings: Vec<String>,
    pub safe_to_offer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeckoProviderResult {
    pub provider_id: String,
    pub provider_display_name: String,
    pub source_identity: String,
    pub retrieved_at_unix_seconds: u64,
    pub game_id: String,
    pub title: Option<String>,
    pub region: GeckoRegion,
    pub revision: u16,
    pub entries: Vec<GeckoProviderEntry>,
    pub warnings: Vec<String>,
    pub attribution: String,
    pub license: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoProviderErrorKind {
    InvalidGameId,
    RegionMismatch,
    ResponseNotUtf8,
    ResponseIdentityMissing,
    ResponseIdentityMismatch,
    NoGeckoEntries,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderError {
    pub kind: GeckoProviderErrorKind,
    pub detail: String,
}

impl std::fmt::Display for GeckoProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GeckoProviderError {}

fn provider_error(kind: GeckoProviderErrorKind, detail: impl Into<String>) -> GeckoProviderError {
    GeckoProviderError {
        kind,
        detail: detail.into(),
    }
}

pub trait GeckoCodeProvider {
    fn provider_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn source_url(&self, query: &GeckoProviderQuery) -> Result<String, GeckoProviderError>;
    fn parse_response(
        &self,
        query: &GeckoProviderQuery,
        source_identity: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
    ) -> Result<GeckoProviderResult, GeckoProviderError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DolphinUpstreamGeckoProvider;

impl GeckoCodeProvider for DolphinUpstreamGeckoProvider {
    fn provider_id(&self) -> &'static str {
        DOLPHIN_UPSTREAM_PROVIDER_ID
    }

    fn display_name(&self) -> &'static str {
        DOLPHIN_UPSTREAM_PROVIDER_NAME
    }

    fn source_url(&self, query: &GeckoProviderQuery) -> Result<String, GeckoProviderError> {
        validate_query(query)?;
        Ok(format!(
            "https://raw.githubusercontent.com/{DOLPHIN_UPSTREAM_REPOSITORY}/master/Data/Sys/GameSettings/{}.ini",
            query.game_id
        ))
    }

    fn parse_response(
        &self,
        query: &GeckoProviderQuery,
        source_identity: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
    ) -> Result<GeckoProviderResult, GeckoProviderError> {
        validate_query(query)?;
        let text = std::str::from_utf8(bytes).map_err(|_| {
            provider_error(
                GeckoProviderErrorKind::ResponseNotUtf8,
                "Dolphin upstream response is not valid UTF-8",
            )
        })?;
        let (response_game_id, title) = response_identity(text).ok_or_else(|| {
            provider_error(
                GeckoProviderErrorKind::ResponseIdentityMissing,
                "Dolphin upstream response does not declare its game ID in the leading comment",
            )
        })?;
        if response_game_id != query.game_id {
            return Err(provider_error(
                GeckoProviderErrorKind::ResponseIdentityMismatch,
                format!(
                    "requested exact game ID {}, but the response declares {response_game_id}",
                    query.game_id
                ),
            ));
        }

        let document = parse_dolphin_ini(text);
        if document.gecko_codes.is_empty() {
            return Err(provider_error(
                GeckoProviderErrorKind::NoGeckoEntries,
                format!(
                    "Dolphin upstream has no Gecko entries for {}",
                    query.game_id
                ),
            ));
        }

        let mut duplicate_names: BTreeMap<String, usize> = BTreeMap::new();
        for code in &document.gecko_codes {
            *duplicate_names.entry(code.name.clone()).or_default() += 1;
        }
        let mut entries = Vec::with_capacity(document.gecko_codes.len());
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
            parse_warnings.push(
                "Upstream does not declare disc-revision applicability; review before enabling."
                    .to_string(),
            );
            let safe_to_offer = code.is_selectable() && !duplicate_name;
            entries.push(GeckoProviderEntry {
                provider_entry_id: stable_entry_id(&query.game_id, &code.name, &code.lines),
                name: code.name,
                code_lines: code.lines,
                notes: code.notes,
                region: query.region.clone(),
                revision_applicability: GeckoRevisionApplicability::Uncertain,
                parse_warnings,
                safe_to_offer,
            });
        }

        let blocked = entries.iter().filter(|entry| !entry.safe_to_offer).count();
        let mut warnings = vec![
            "Dolphin upstream identifies this file by exact game ID and region, but does not declare which disc revision each Gecko entry supports."
                .to_string(),
        ];
        if blocked > 0 {
            warnings.push(format!(
                "{blocked} malformed or ambiguous Gecko entr{} blocked",
                if blocked == 1 { "y was" } else { "ies were" }
            ));
        }

        Ok(GeckoProviderResult {
            provider_id: self.provider_id().to_string(),
            provider_display_name: self.display_name().to_string(),
            source_identity: source_identity.to_string(),
            retrieved_at_unix_seconds,
            game_id: query.game_id.clone(),
            title,
            region: query.region.clone(),
            revision: query.revision,
            entries,
            warnings,
            attribution: DOLPHIN_UPSTREAM_ATTRIBUTION.to_string(),
            license: DOLPHIN_UPSTREAM_LICENSE.to_string(),
        })
    }
}

#[must_use]
pub fn revision_applicability(
    applicability: GeckoRevisionApplicability,
    revision: u16,
) -> GeckoApplicabilityDecision {
    match applicability {
        GeckoRevisionApplicability::Any => GeckoApplicabilityDecision::Offer,
        GeckoRevisionApplicability::Exact(expected) if expected == revision => {
            GeckoApplicabilityDecision::Offer
        }
        GeckoRevisionApplicability::Exact(_) => GeckoApplicabilityDecision::Reject,
        GeckoRevisionApplicability::Uncertain => GeckoApplicabilityDecision::OfferWithWarning,
    }
}

#[must_use]
pub fn region_for_game_id(game_id: &str) -> Option<GeckoRegion> {
    if !valid_game_id(game_id) {
        return None;
    }
    match game_id.as_bytes().get(3).copied()? {
        b'E' => Some(GeckoRegion::Usa),
        b'P' | b'D' | b'F' | b'I' | b'S' | b'H' | b'X' | b'Y' | b'Z' => Some(GeckoRegion::Europe),
        b'J' => Some(GeckoRegion::Japan),
        b'K' | b'Q' | b'T' => Some(GeckoRegion::Korea),
        other => Some(GeckoRegion::Unknown(char::from(other).to_string())),
    }
}

fn validate_query(query: &GeckoProviderQuery) -> Result<(), GeckoProviderError> {
    if !valid_game_id(&query.game_id) {
        return Err(provider_error(
            GeckoProviderErrorKind::InvalidGameId,
            "provider lookup requires an exact six-character ASCII GameCube game ID",
        ));
    }
    let encoded_region = region_for_game_id(&query.game_id).expect("validated game ID has region");
    if encoded_region != query.region {
        return Err(provider_error(
            GeckoProviderErrorKind::RegionMismatch,
            format!(
                "game ID {} encodes region {}, not {}",
                query.game_id,
                encoded_region.display_name(),
                query.region.display_name()
            ),
        ));
    }
    Ok(())
}

fn valid_game_id(game_id: &str) -> bool {
    game_id.len() == 6
        && game_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn response_identity(text: &str) -> Option<(String, Option<String>)> {
    let first = text.lines().find(|line| !line.trim().is_empty())?.trim();
    let declaration = first.strip_prefix('#')?.trim();
    let (game_id, title) = declaration
        .split_once(" - ")
        .map_or((declaration, None), |(game_id, title)| {
            (game_id.trim(), Some(title.trim().to_string()))
        });
    valid_game_id(game_id).then(|| (game_id.to_string(), title))
}

fn stable_entry_id(game_id: &str, name: &str, lines: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DOLPHIN_UPSTREAM_PROVIDER_ID.as_bytes());
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

#[derive(Debug, Clone)]
pub struct GeckoProviderFetchOptions {
    pub cache_root: PathBuf,
    pub force_refresh: bool,
    pub now_unix_seconds: u64,
}

impl GeckoProviderFetchOptions {
    pub fn with_default_cache(now_unix_seconds: u64) -> Result<Self, GeckoProviderFetchError> {
        Ok(Self {
            cache_root: default_gecko_provider_cache_root()?,
            force_refresh: false,
            now_unix_seconds,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoProviderFetchStatus {
    Downloaded,
    FreshCache,
    RateLimitedCache,
    StaleCacheFallback,
    /// Answered entirely from the local Dolphin cheat catalogue - no
    /// network request, not even a cache-freshness check against a remote
    /// server. See `dolphin_cheat_catalogue::resolve_dolphin_gecko_lookup`.
    Catalogue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderFetchResult {
    pub result: GeckoProviderResult,
    pub status: GeckoProviderFetchStatus,
    /// Populated only when a refresh failed but a validated cache remains usable.
    pub refresh_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoProviderFetchErrorKind {
    CacheUnavailable,
    CacheUnsafe,
    CacheInvalid,
    Network,
    HttpStatus,
    ResponseTooLarge,
    Parse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderFetchError {
    pub kind: GeckoProviderFetchErrorKind,
    pub detail: String,
}

impl std::fmt::Display for GeckoProviderFetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GeckoProviderFetchError {}

fn fetch_error(
    kind: GeckoProviderFetchErrorKind,
    detail: impl Into<String>,
) -> GeckoProviderFetchError {
    GeckoProviderFetchError {
        kind,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeckoProviderCacheEnvelope {
    schema_version: u32,
    result_sha256: String,
    result: GeckoProviderResult,
}

pub fn default_gecko_provider_cache_root() -> Result<PathBuf, GeckoProviderFetchError> {
    let database = crate::default_database_path().map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache root unavailable: {error}"),
        )
    })?;
    Ok(database
        .parent()
        .expect("default database path always has a parent")
        .join("gecko-provider-cache"))
}

/// Reads a validated cached single-game result, if one exists, without ever
/// touching the network - not even the rate-limit-aware refresh path
/// `fetch_dolphin_upstream_gecko` takes. Used as the fallback source when no
/// full catalogue is installed or the catalogue has no entry for this exact
/// game: showing a previously fetched result is preferable to an automatic
/// network request on every game selection.
#[must_use]
pub fn peek_cached_gecko_result(
    cache_root: &Path,
    query: &GeckoProviderQuery,
) -> Option<GeckoProviderResult> {
    let path = cache_path(cache_root, query).ok()?;
    load_cache(&path, query).ok().flatten()
}

pub fn fetch_dolphin_upstream_gecko(
    query: &GeckoProviderQuery,
    options: &GeckoProviderFetchOptions,
) -> Result<GeckoProviderFetchResult, GeckoProviderFetchError> {
    fetch_dolphin_upstream_gecko_with_transport(query, options, &HttpsCheatSourceTransport::new())
}

/// Fetches one exact-ID upstream file. The transport parameter keeps tests deterministic and
/// ensures normal tests never require internet access.
pub fn fetch_dolphin_upstream_gecko_with_transport(
    query: &GeckoProviderQuery,
    options: &GeckoProviderFetchOptions,
    transport: &dyn CheatSourceTransport,
) -> Result<GeckoProviderFetchResult, GeckoProviderFetchError> {
    let provider = DolphinUpstreamGeckoProvider;
    let source_url = provider
        .source_url(query)
        .map_err(|error| fetch_error(GeckoProviderFetchErrorKind::Parse, error.to_string()))?;
    let cache_path = cache_path(&options.cache_root, query)?;
    let cached = load_cache(&cache_path, query)?;

    if let Some(result) = cached.as_ref() {
        let age = options
            .now_unix_seconds
            .saturating_sub(result.retrieved_at_unix_seconds);
        if !options.force_refresh && age <= GECKO_PROVIDER_CACHE_FRESH_SECONDS {
            return Ok(GeckoProviderFetchResult {
                result: result.clone(),
                status: GeckoProviderFetchStatus::FreshCache,
                refresh_error: None,
            });
        }
        if age < GECKO_PROVIDER_MIN_REFRESH_SECONDS {
            return Ok(GeckoProviderFetchResult {
                result: result.clone(),
                status: GeckoProviderFetchStatus::RateLimitedCache,
                refresh_error: Some(format!(
                    "Refresh limited to one request every {GECKO_PROVIDER_MIN_REFRESH_SECONDS} seconds"
                )),
            });
        }
    }

    let downloaded = download_provider_response(transport, &source_url);
    let bytes = match downloaded {
        Ok(bytes) => bytes,
        Err(error) => return fallback_or_error(cached, error),
    };
    let result = match provider.parse_response(query, &source_url, options.now_unix_seconds, &bytes)
    {
        Ok(result) => result,
        Err(error) => {
            return fallback_or_error(
                cached,
                fetch_error(GeckoProviderFetchErrorKind::Parse, error.to_string()),
            );
        }
    };
    store_cache(&options.cache_root, &cache_path, &result)?;
    Ok(GeckoProviderFetchResult {
        result,
        status: GeckoProviderFetchStatus::Downloaded,
        refresh_error: None,
    })
}

fn download_provider_response(
    transport: &dyn CheatSourceTransport,
    source_url: &str,
) -> Result<Vec<u8>, GeckoProviderFetchError> {
    let mut bytes = Vec::new();
    let response = transport
        .get(
            source_url,
            GECKO_PROVIDER_MAX_RESPONSE_BYTES,
            &mut bytes,
            CheatSourceTransferContext {
                cancellation: None,
                progress: None,
                attempt: 1,
                overall_timeout: Duration::from_secs(GECKO_PROVIDER_TIMEOUT_SECONDS),
            },
        )
        .map_err(|error| {
            let kind = if error.code == "download_too_large" {
                GeckoProviderFetchErrorKind::ResponseTooLarge
            } else {
                GeckoProviderFetchErrorKind::Network
            };
            fetch_error(kind, format!("Gecko provider request failed: {error}"))
        })?;
    if response.downloaded_bytes > GECKO_PROVIDER_MAX_RESPONSE_BYTES
        || bytes.len() as u64 > GECKO_PROVIDER_MAX_RESPONSE_BYTES
    {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::ResponseTooLarge,
            format!("Gecko provider response exceeds {GECKO_PROVIDER_MAX_RESPONSE_BYTES} bytes"),
        ));
    }
    if response.status != 200 {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::HttpStatus,
            format!("Gecko provider returned HTTP {}", response.status),
        ));
    }
    Ok(bytes)
}

fn fallback_or_error(
    cached: Option<GeckoProviderResult>,
    error: GeckoProviderFetchError,
) -> Result<GeckoProviderFetchResult, GeckoProviderFetchError> {
    match cached {
        Some(mut result) => {
            result.warnings.push(format!(
                "Provider refresh failed; using validated cached data: {}",
                error.detail
            ));
            Ok(GeckoProviderFetchResult {
                result,
                status: GeckoProviderFetchStatus::StaleCacheFallback,
                refresh_error: Some(error.detail),
            })
        }
        None => Err(error),
    }
}

fn cache_path(
    cache_root: &Path,
    query: &GeckoProviderQuery,
) -> Result<PathBuf, GeckoProviderFetchError> {
    validate_cache_root(cache_root)?;
    Ok(cache_root.join(format!("{}-r{}.json", query.game_id, query.revision)))
}

fn validate_cache_root(path: &Path) -> Result<(), GeckoProviderFetchError> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::CacheUnsafe,
            "Gecko provider cache root must be an absolute non-filesystem-root path",
        ));
    }
    super::cheat_sources::validate_cache_path_for_read(path).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnsafe,
            format!("Gecko provider cache path is unsafe: {error}"),
        )
    })
}

fn load_cache(
    path: &Path,
    query: &GeckoProviderQuery,
) -> Result<Option<GeckoProviderResult>, GeckoProviderFetchError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(fetch_error(
                GeckoProviderFetchErrorKind::CacheUnsafe,
                format!(
                    "Gecko provider cache entry is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(fetch_error(
                GeckoProviderFetchErrorKind::CacheUnavailable,
                format!("Gecko provider cache could not be inspected: {error}"),
            ));
        }
    }
    let bytes = fs::read(path).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache could not be read: {error}"),
        )
    })?;
    let envelope: GeckoProviderCacheEnvelope = serde_json::from_slice(&bytes).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheInvalid,
            format!("Gecko provider cache is invalid: {error}"),
        )
    })?;
    if envelope.schema_version != GECKO_PROVIDER_CACHE_SCHEMA_VERSION
        || envelope.result.provider_id != DOLPHIN_UPSTREAM_PROVIDER_ID
        || envelope.result.game_id != query.game_id
        || envelope.result.region != query.region
        || envelope.result.revision != query.revision
    {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::CacheInvalid,
            "Gecko provider cache identity does not match this exact lookup",
        ));
    }
    let result_bytes = serde_json::to_vec(&envelope.result).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheInvalid,
            format!("Gecko provider cache could not be verified: {error}"),
        )
    })?;
    if hex_sha256(&result_bytes) != envelope.result_sha256 {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::CacheInvalid,
            "Gecko provider cache digest does not match its contents",
        ));
    }
    Ok(Some(envelope.result))
}

fn store_cache(
    cache_root: &Path,
    path: &Path,
    result: &GeckoProviderResult,
) -> Result<(), GeckoProviderFetchError> {
    validate_cache_root(cache_root)?;
    fs::create_dir_all(cache_root).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache directory could not be created: {error}"),
        )
    })?;
    validate_cache_root(cache_root)?;
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::CacheUnsafe,
            format!("Gecko provider cache entry is unsafe: {}", path.display()),
        ));
    }
    let result_bytes = serde_json::to_vec(result).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider result could not be serialized: {error}"),
        )
    })?;
    let envelope = GeckoProviderCacheEnvelope {
        schema_version: GECKO_PROVIDER_CACHE_SCHEMA_VERSION,
        result_sha256: hex_sha256(&result_bytes),
        result: result.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&envelope).map_err(|error| {
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache could not be serialized: {error}"),
        )
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            fetch_error(
                GeckoProviderFetchErrorKind::CacheUnsafe,
                "Gecko provider cache filename is invalid",
            )
        })?;
    let temporary = cache_root.join(format!(".{file_name}.{}.partial", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            fetch_error(
                GeckoProviderFetchErrorKind::CacheUnavailable,
                format!("Gecko provider cache staging file could not be created: {error}"),
            )
        })?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache staging write failed: {error}"),
        ));
    }
    drop(file);
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        fetch_error(
            GeckoProviderFetchErrorKind::CacheUnavailable,
            format!("Gecko provider cache could not be published: {error}"),
        )
    })
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::patch_manager::{CheatSourceError, CheatSourceErrorStage, CheatSourceHttpResponse};

    const GAFE01: &str = include_str!("../../tests/fixtures/dolphin_upstream/GAFE01.ini");

    fn query() -> GeckoProviderQuery {
        GeckoProviderQuery {
            game_id: "GAFE01".to_string(),
            region: GeckoRegion::Usa,
            revision: 0,
        }
    }

    static CACHE_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct CacheFixture(PathBuf);

    impl CacheFixture {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            Self(std::env::temp_dir().join(format!(
                "archivefs-gecko-provider-{label}-{}-{nonce}-{}",
                std::process::id(),
                CACHE_COUNTER.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    impl Drop for CacheFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FakeTransport {
        calls: Cell<usize>,
        body: Vec<u8>,
        status: u16,
        reported_bytes: Option<u64>,
        error_code: Option<&'static str>,
    }

    impl FakeTransport {
        fn ok(body: &[u8]) -> Self {
            Self {
                calls: Cell::new(0),
                body: body.to_vec(),
                status: 200,
                reported_bytes: None,
                error_code: None,
            }
        }

        fn failing(code: &'static str) -> Self {
            Self {
                calls: Cell::new(0),
                body: Vec::new(),
                status: 0,
                reported_bytes: None,
                error_code: Some(code),
            }
        }
    }

    impl CheatSourceTransport for FakeTransport {
        fn get(
            &self,
            _url: &str,
            _maximum_bytes: u64,
            destination: &mut dyn Write,
            _context: CheatSourceTransferContext<'_>,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            self.calls.set(self.calls.get() + 1);
            if let Some(code) = self.error_code {
                return Err(CheatSourceError::new(
                    CheatSourceErrorStage::Network,
                    code,
                    "recorded transport failure",
                ));
            }
            destination.write_all(&self.body).expect("fixture write");
            Ok(CheatSourceHttpResponse {
                status: self.status,
                content_type: Some("text/plain".to_string()),
                content_encoding: None,
                content_length: Some(self.body.len() as u64),
                location: None,
                etag: None,
                last_modified: None,
                downloaded_bytes: self.reported_bytes.unwrap_or(self.body.len() as u64),
                retry_after_seconds: None,
            })
        }
    }

    fn fetch_options(root: &Path, now: u64, force_refresh: bool) -> GeckoProviderFetchOptions {
        GeckoProviderFetchOptions {
            cache_root: root.to_path_buf(),
            force_refresh,
            now_unix_seconds: now,
        }
    }

    #[test]
    fn recorded_gafe01_response_parses_complete_real_gecko_body() {
        let provider = DolphinUpstreamGeckoProvider;
        let result = provider
            .parse_response(
                &query(),
                "fixture:GAFE01.ini",
                1_721_000_000,
                GAFE01.as_bytes(),
            )
            .expect("recorded upstream response parses");

        assert_eq!(result.game_id, "GAFE01");
        assert_eq!(result.title.as_deref(), Some("Animal Crossing"));
        assert_eq!(result.region, GeckoRegion::Usa);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "16:9 Widescreen");
        assert_eq!(
            result.entries[0].code_lines,
            [
                "040037A0 3C608000",
                "040037A4 C38337AC",
                "040037A8 4805ACBC",
                "040037AC 3FE38E39",
                "0405E460 4BFA5340",
            ]
        );
        assert!(result.entries[0].safe_to_offer);
        assert_eq!(
            result.entries[0].revision_applicability,
            GeckoRevisionApplicability::Uncertain
        );
    }

    #[test]
    fn exact_game_id_and_region_are_mandatory() {
        let provider = DolphinUpstreamGeckoProvider;
        let mismatch = GAFE01.replacen("GAFE01", "GAFP01", 1);
        let error = provider
            .parse_response(&query(), "fixture:mismatch", 1, mismatch.as_bytes())
            .expect_err("wrong response identity must fail");
        assert_eq!(error.kind, GeckoProviderErrorKind::ResponseIdentityMismatch);

        let mut wrong_region = query();
        wrong_region.region = GeckoRegion::Europe;
        let error = provider
            .source_url(&wrong_region)
            .expect_err("wrong query region must fail");
        assert_eq!(error.kind, GeckoProviderErrorKind::RegionMismatch);
    }

    #[test]
    fn revision_specific_entries_are_filtered_and_uncertain_entries_warn() {
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Exact(0), 0),
            GeckoApplicabilityDecision::Offer
        );
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Exact(1), 0),
            GeckoApplicabilityDecision::Reject
        );
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Uncertain, 0),
            GeckoApplicabilityDecision::OfferWithWarning
        );
    }

    #[test]
    fn malformed_and_duplicate_code_bodies_are_blocked_not_repaired() {
        let provider = DolphinUpstreamGeckoProvider;
        let body = "# GAFE01 - Animal Crossing\n[Gecko]\n$Bad\nnot code\n$Same\n040037A0 3C608000\n$Same\n040037A4 C38337AC\n";
        let result = provider
            .parse_response(&query(), "fixture:bad", 1, body.as_bytes())
            .expect("document remains inspectable");
        assert_eq!(result.entries.len(), 3);
        assert!(result.entries.iter().all(|entry| !entry.safe_to_offer));
        assert!(result.entries[0].code_lines.is_empty());
        assert!(
            result.entries[0]
                .parse_warnings
                .iter()
                .any(|warning| warning.contains("not a valid"))
        );
    }

    #[test]
    fn cache_hit_avoids_a_second_provider_request_and_refresh_replaces_it() {
        let cache = CacheFixture::new("hit-refresh");
        let first_transport = FakeTransport::ok(GAFE01.as_bytes());
        let first = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_000, false),
            &first_transport,
        )
        .expect("initial fetch");
        assert_eq!(first.status, GeckoProviderFetchStatus::Downloaded);
        assert_eq!(first_transport.calls.get(), 1);

        let unused_transport = FakeTransport::failing("must_not_run");
        let cached = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_100, false),
            &unused_transport,
        )
        .expect("fresh cache");
        assert_eq!(cached.status, GeckoProviderFetchStatus::FreshCache);
        assert_eq!(unused_transport.calls.get(), 0);

        let refreshed_body = GAFE01.replace("16:9 Widescreen", "16:9 Widescreen Updated");
        let refresh_transport = FakeTransport::ok(refreshed_body.as_bytes());
        let refreshed = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_100, true),
            &refresh_transport,
        )
        .expect("explicit refresh");
        assert_eq!(refreshed.status, GeckoProviderFetchStatus::Downloaded);
        assert_eq!(refreshed.result.entries[0].name, "16:9 Widescreen Updated");
        assert_eq!(refresh_transport.calls.get(), 1);
    }

    #[test]
    fn provider_failure_retains_a_validated_stale_cache() {
        let cache = CacheFixture::new("fallback");
        fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_000, false),
            &FakeTransport::ok(GAFE01.as_bytes()),
        )
        .expect("seed cache");

        let failure = FakeTransport::failing("overall_timeout");
        let result = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(
                &cache.0,
                1_000 + GECKO_PROVIDER_CACHE_FRESH_SECONDS + 1,
                false,
            ),
            &failure,
        )
        .expect("stale cache remains usable");
        assert_eq!(result.status, GeckoProviderFetchStatus::StaleCacheFallback);
        assert!(
            result
                .refresh_error
                .as_deref()
                .is_some_and(|error| error.contains("timeout"))
        );
        assert_eq!(result.result.game_id, "GAFE01");
    }

    #[test]
    fn response_limit_and_timeout_are_visible_errors_without_a_cache() {
        let oversized_cache = CacheFixture::new("oversized");
        let oversized = FakeTransport {
            calls: Cell::new(0),
            body: Vec::new(),
            status: 200,
            reported_bytes: Some(GECKO_PROVIDER_MAX_RESPONSE_BYTES + 1),
            error_code: None,
        };
        let error = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&oversized_cache.0, 1, false),
            &oversized,
        )
        .expect_err("oversized response fails");
        assert_eq!(error.kind, GeckoProviderFetchErrorKind::ResponseTooLarge);

        let timeout_cache = CacheFixture::new("timeout");
        let error = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&timeout_cache.0, 1, false),
            &FakeTransport::failing("overall_timeout"),
        )
        .expect_err("timeout is visible");
        assert_eq!(error.kind, GeckoProviderFetchErrorKind::Network);
        assert!(error.detail.contains("overall_timeout"));
    }

    #[test]
    fn refreshes_inside_the_provider_interval_use_cache_without_network() {
        let cache = CacheFixture::new("rate-limit");
        fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_000, false),
            &FakeTransport::ok(GAFE01.as_bytes()),
        )
        .expect("seed cache");
        let unused = FakeTransport::failing("must_not_run");
        let result = fetch_dolphin_upstream_gecko_with_transport(
            &query(),
            &fetch_options(&cache.0, 1_005, true),
            &unused,
        )
        .expect("rate-limited cache result");
        assert_eq!(result.status, GeckoProviderFetchStatus::RateLimitedCache);
        assert_eq!(unused.calls.get(), 0);
    }
}
