//! Shared low-level GameHacking.org transport and cache mechanics.
//!
//! Platform identity, matching, HTML parsing, export parsing, and emulator
//! installation deliberately remain in their PS2 and GameCube providers.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use url::Url;

pub(crate) const BASE_URL: &str = "https://gamehacking.org";
pub(crate) const EXPORT_URL: &str = "https://gamehacking.org/inc/sub.exportCodes.php";
pub(crate) const ROBOTS_URL: &str = "https://gamehacking.org/robots.txt";
const USER_AGENT: &str = concat!(
    "EmuWiz/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/davedap/archivefs; one-game-at-a-time cheat provider)"
);
const MAX_RETRIES: u8 = 3;
const CLOUDFLARE_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const CLOUDFLARE_MARKER_FILE: &str = "cloudflare-blocked-at";

/// The exact, stable wording shown to the user (GUI and CLI alike) whenever
/// a PS2 or GameCube request is classified as a provider challenge rather
/// than an ordinary failure.
pub const GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE: &str = "GameHacking.org blocked this automated request. Cached data is being used where available. Try again later.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameHackingErrorKind {
    UnsupportedSystem,
    IdentityIncomplete,
    IdentityConflict,
    NoMatch,
    AccessDenied,
    /// The provider answered with a Cloudflare (or similarly-shaped)
    /// bot-challenge/interstitial response rather than real content.
    CloudflareBlocked,
    RateLimited,
    NetworkFailure,
    TemporaryFailure,
    PermanentHttpFailure,
    InvalidResponse,
    CacheUnavailable,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingError {
    pub kind: GameHackingErrorKind,
    pub detail: String,
}

impl std::fmt::Display for GameHackingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GameHackingError {}

pub(crate) fn provider_error(
    kind: GameHackingErrorKind,
    detail: impl Into<String>,
) -> GameHackingError {
    GameHackingError {
        kind,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingFetchOutcome<T> {
    pub data: T,
    pub cached_fallback: bool,
    pub retrieved_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GameHackingHttpClassification {
    Success,
    CloudflareBlocked,
    AccessDenied,
    RateLimited,
    ServerError,
    OtherHttpError,
}

pub(crate) fn classify_gamehacking_http_response(
    status: u16,
    server: Option<&str>,
    body: &[u8],
) -> GameHackingHttpClassification {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    let strong_challenge_marker = [
        "cdn-cgi/challenge-platform",
        "cf-chl-",
        "_cf_chl_opt",
        "cf-error-details",
        "cf-browser-verification",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    let cloudflare_interstitial = text.contains("cloudflare")
        && [
            "attention required",
            "just a moment",
            "checking your browser",
            "cloudflare ray id",
            "enable javascript and cookies to continue",
        ]
        .iter()
        .any(|marker| text.contains(marker));
    let cloudflare_server = server.is_some_and(|value| {
        value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("cloudflare"))
    });

    if status == 429 {
        GameHackingHttpClassification::RateLimited
    } else if strong_challenge_marker
        || cloudflare_interstitial
        || (status == 403 && cloudflare_server)
    {
        GameHackingHttpClassification::CloudflareBlocked
    } else if status == 401 || status == 403 {
        GameHackingHttpClassification::AccessDenied
    } else if (500..600).contains(&status) {
        GameHackingHttpClassification::ServerError
    } else if (200..300).contains(&status) {
        GameHackingHttpClassification::Success
    } else {
        GameHackingHttpClassification::OtherHttpError
    }
}

pub(crate) fn classify_gamehacking_transport_error(failure: ureq::Error) -> GameHackingError {
    provider_error(
        GameHackingErrorKind::NetworkFailure,
        format!("GameHacking.org request failed: {failure}"),
    )
}

pub(crate) fn cached_bytes_are_cloudflare_challenge(bytes: &[u8]) -> bool {
    classify_gamehacking_http_response(200, None, bytes)
        == GameHackingHttpClassification::CloudflareBlocked
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GameHackingRequestSpec<'a> {
    pub cache_file: &'a str,
    pub url: &'a str,
    pub maximum_bytes: usize,
}

pub(crate) trait GameHackingRequestOptions {
    fn cache_root(&self) -> &Path;
    fn force_refresh(&self) -> bool;
    fn delay(&self) -> Duration;
    fn cancellation(&self) -> Option<&AtomicBool>;
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderResponse {
    pub bytes: Vec<u8>,
    pub charset: Option<String>,
    pub cached_fallback: bool,
    pub retrieved_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct UreqGameHackingTransport {
    agent: ureq::Agent,
}

impl UreqGameHackingTransport {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(15)))
            .build();
        Self {
            agent: config.new_agent(),
        }
    }

    fn read_response(
        mut response: http::Response<ureq::Body>,
        maximum_bytes: usize,
    ) -> Result<ProviderResponse, GameHackingError> {
        let status = response.status().as_u16();
        let server = response
            .headers()
            .get("server")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let charset = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .and_then(charset_from_content_type);
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((maximum_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|failure| {
                provider_error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org response could not be read: {failure}"),
                )
            })?;
        match classify_gamehacking_http_response(status, server.as_deref(), &bytes) {
            GameHackingHttpClassification::Success => {}
            GameHackingHttpClassification::CloudflareBlocked => {
                return Err(provider_error(
                    GameHackingErrorKind::CloudflareBlocked,
                    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                ));
            }
            GameHackingHttpClassification::AccessDenied => {
                return Err(provider_error(
                    GameHackingErrorKind::AccessDenied,
                    format!("GameHacking.org denied access (HTTP {status})"),
                ));
            }
            GameHackingHttpClassification::RateLimited => {
                return Err(provider_error(
                    GameHackingErrorKind::RateLimited,
                    "GameHacking.org asked EmuWiz to slow down (HTTP 429)",
                ));
            }
            GameHackingHttpClassification::ServerError => {
                return Err(provider_error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org is temporarily unavailable (HTTP {status})"),
                ));
            }
            GameHackingHttpClassification::OtherHttpError => {
                return Err(provider_error(
                    GameHackingErrorKind::PermanentHttpFailure,
                    format!("GameHacking.org returned HTTP {status}"),
                ));
            }
        }
        if bytes.len() > maximum_bytes {
            return Err(provider_error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org response exceeded the bounded size limit",
            ));
        }
        Ok(ProviderResponse {
            bytes,
            charset,
            cached_fallback: false,
            retrieved_at_unix_seconds: None,
        })
    }

    pub(crate) fn get(
        &self,
        url: &str,
        maximum_bytes: usize,
    ) -> Result<ProviderResponse, GameHackingError> {
        validate_provider_url(url)?;
        let response = self
            .agent
            .get(url)
            .header("Accept", "text/html, text/plain")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(classify_gamehacking_transport_error)?;
        Self::read_response(response, maximum_bytes)
    }

    pub(crate) fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        maximum_bytes: usize,
    ) -> Result<ProviderResponse, GameHackingError> {
        validate_provider_url(url)?;
        let response = self
            .agent
            .post(url)
            .header("Accept", "text/plain, application/octet-stream")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", USER_AGENT)
            .send_form(form.iter().map(|(key, value)| (*key, value.as_str())))
            .map_err(classify_gamehacking_transport_error)?;
        Self::read_response(response, maximum_bytes)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GameHackingClient {
    transport: UreqGameHackingTransport,
}

impl Default for GameHackingClient {
    fn default() -> Self {
        Self {
            transport: UreqGameHackingTransport::new(),
        }
    }
}

impl GameHackingClient {
    pub(crate) fn cached_request<O, F>(
        &self,
        spec: GameHackingRequestSpec<'_>,
        options: &O,
        request: F,
    ) -> Result<ProviderResponse, GameHackingError>
    where
        O: GameHackingRequestOptions,
        F: Fn(&UreqGameHackingTransport) -> Result<ProviderResponse, GameHackingError>,
    {
        prepare_cache(options.cache_root())?;
        let path = options.cache_root().join(spec.cache_file);
        if !options.force_refresh() && path.is_file() {
            let bytes = bounded_read(&path, spec.maximum_bytes)?;
            if cached_bytes_are_cloudflare_challenge(&bytes) {
                return Err(provider_error(
                    GameHackingErrorKind::CloudflareBlocked,
                    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                ));
            }
            let response = ProviderResponse {
                bytes,
                charset: read_cached_charset(&path)?,
                cached_fallback: false,
                retrieved_at_unix_seconds: Some(cache_retrieved_at(&path)?),
            };
            let age =
                unix_seconds_now().saturating_sub(response.retrieved_at_unix_seconds.unwrap_or(0));
            log::info!(
                "gamehacking request_url={} classification=cached cache_fallback=false cache_age_seconds={}",
                spec.url,
                age
            );
            return Ok(response);
        }
        if cloudflare_cooldown_remaining(options.cache_root()).is_some() {
            if let Some(response) = cached_fallback_response(&path, spec.maximum_bytes)? {
                log_cached_fallback(spec.url, &path, &response);
                return Ok(response);
            }
            return Err(provider_error(
                GameHackingErrorKind::CloudflareBlocked,
                blocked_without_cache_message(options.cache_root(), spec.cache_file),
            ));
        }
        let mut last_error = None;
        for attempt in 0..MAX_RETRIES {
            check_cancelled(options)?;
            if attempt > 0 || request_delay_needed(options.cache_root()) {
                cancellable_delay(options, options.delay().saturating_mul(1_u32 << attempt))?;
            }
            match request(&self.transport) {
                Ok(response) => {
                    atomic_write(&path, &response.bytes)?;
                    atomic_write(
                        &charset_cache_path(&path),
                        response.charset.as_deref().unwrap_or_default().as_bytes(),
                    )?;
                    atomic_write(
                        &retrieved_cache_path(&path),
                        unix_seconds_now().to_string().as_bytes(),
                    )?;
                    touch_request_marker(options.cache_root())?;
                    clear_cloudflare_marker(options.cache_root());
                    log::info!(
                        "gamehacking request_url={} classification=success cache_fallback=false cache_write=completed",
                        spec.url
                    );
                    return Ok(response);
                }
                Err(failure) if failure.kind == GameHackingErrorKind::CloudflareBlocked => {
                    mark_cloudflare_blocked(options.cache_root())?;
                    log::warn!(
                        "gamehacking request_url={} status=blocked classification=cloudflare cache_write=skipped",
                        spec.url
                    );
                    if let Some(response) = cached_fallback_response(&path, spec.maximum_bytes)? {
                        log_cached_fallback(spec.url, &path, &response);
                        return Ok(response);
                    }
                    return Err(provider_error(
                        failure.kind,
                        blocked_without_cache_message(options.cache_root(), spec.cache_file),
                    ));
                }
                Err(failure)
                    if matches!(
                        failure.kind,
                        GameHackingErrorKind::RateLimited | GameHackingErrorKind::TemporaryFailure
                    ) =>
                {
                    log::warn!(
                        "gamehacking request_url={} classification={:?} retry_attempt={}",
                        spec.url,
                        failure.kind,
                        attempt + 1
                    );
                    last_error = Some(failure);
                }
                Err(failure) => {
                    log::warn!(
                        "gamehacking request_url={} classification={:?} cache_fallback=false",
                        spec.url,
                        failure.kind
                    );
                    return Err(failure);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            provider_error(
                GameHackingErrorKind::TemporaryFailure,
                "GameHacking.org retry limit reached",
            )
        }))
    }

    pub(crate) fn check_robots<O: GameHackingRequestOptions>(
        &self,
        options: &O,
        paths: &[&str],
    ) -> Result<(), GameHackingError> {
        let spec = GameHackingRequestSpec {
            cache_file: "robots.txt",
            url: ROBOTS_URL,
            maximum_bytes: 256 * 1024,
        };
        let robots = self.cached_request(spec, options, |transport| {
            transport.get(ROBOTS_URL, 256 * 1024)
        })?;
        let text = decode_provider_text(&robots.bytes, robots.charset.as_deref());
        for path in paths {
            if robots_disallows_archivefs(&text, path) {
                return Err(provider_error(
                    GameHackingErrorKind::AccessDenied,
                    format!("GameHacking.org robots.txt does not allow access to {path}"),
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_provider_url(value: &str) -> Result<(), GameHackingError> {
    let url = Url::parse(value).map_err(|_| {
        provider_error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is invalid",
        )
    })?;
    if url.scheme() != "https" || url.host_str() != Some("gamehacking.org") {
        return Err(provider_error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is outside the fixed GameHacking.org HTTPS origin",
        ));
    }
    Ok(())
}

pub(crate) fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_ascii_lowercase())
    })
}

pub(crate) fn prepare_cache(root: &Path) -> Result<(), GameHackingError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(provider_error(
            GameHackingErrorKind::CacheUnavailable,
            "GameHacking.org cache root must be an absolute non-root path",
        ));
    }
    fs::create_dir_all(root).map_err(|failure| {
        provider_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("GameHacking.org cache could not be created: {failure}"),
        )
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn retrieved_cache_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.retrieved",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("cache")
    ))
}

pub(crate) fn cache_retrieved_at(path: &Path) -> Result<u64, GameHackingError> {
    let sidecar = retrieved_cache_path(path);
    if sidecar.is_file() {
        let bytes = bounded_read(&sidecar, 64)?;
        if let Ok(value) = String::from_utf8_lossy(&bytes).trim().parse::<u64>() {
            return Ok(value);
        }
    }
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .ok_or_else(|| {
            provider_error(
                GameHackingErrorKind::CacheUnavailable,
                format!("cached retrieval date is unavailable: {}", path.display()),
            )
        })
}

fn cached_fallback_response(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Option<ProviderResponse>, GameHackingError> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = bounded_read(path, maximum_bytes)?;
    if cached_bytes_are_cloudflare_challenge(&bytes) {
        log::warn!(
            "gamehacking cache_path={} classification=cloudflare cache_fallback=false cache_write=skipped",
            path.display()
        );
        return Ok(None);
    }
    Ok(Some(ProviderResponse {
        bytes,
        charset: read_cached_charset(path)?,
        cached_fallback: true,
        retrieved_at_unix_seconds: Some(cache_retrieved_at(path)?),
    }))
}

fn log_cached_fallback(url: &str, path: &Path, response: &ProviderResponse) {
    let retrieved_at = response.retrieved_at_unix_seconds.unwrap_or(0);
    let age = unix_seconds_now().saturating_sub(retrieved_at);
    log::warn!(
        "gamehacking request_url={} classification=cloudflare cache_fallback=true cache_path={} cache_age_seconds={} cache_write=skipped",
        url,
        path.display(),
        age
    );
}

pub(crate) fn blocked_without_cache_message(cache_root: &Path, file_name: &str) -> String {
    let message = if file_name.starts_with("export-") {
        "GameHacking.org blocked the live request and no cached cheat export is available."
    } else {
        GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE
    };
    match fs::read_to_string(cloudflare_marker_path(cache_root))
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(timestamp) => format!("{message} Last attempted: Unix timestamp {timestamp}."),
        None => message.to_string(),
    }
}

pub(crate) fn bounded_read(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, GameHackingError> {
    let metadata = path.symlink_metadata().map_err(|failure| {
        provider_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("cached provider response could not be inspected: {failure}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum_bytes as u64
    {
        return Err(provider_error(
            GameHackingErrorKind::CacheUnavailable,
            "cached provider response is unsafe or oversized",
        ));
    }
    fs::read(path).map_err(|failure| {
        provider_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("cached provider response could not be read: {failure}"),
        )
    })
}

pub(crate) fn charset_cache_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("response");
    path.with_file_name(format!("{file_name}.charset"))
}

fn read_cached_charset(path: &Path) -> Result<Option<String>, GameHackingError> {
    let path = charset_cache_path(path);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = bounded_read(&path, 128)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| {
        provider_error(
            GameHackingErrorKind::CacheUnavailable,
            "cached provider charset metadata is invalid",
        )
    })?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), GameHackingError> {
    let temporary = path.with_extension(format!("partial-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if let Err(failure) = result {
        let _ = fs::remove_file(&temporary);
        return Err(provider_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("provider cache could not be updated atomically: {failure}"),
        ));
    }
    Ok(())
}

fn request_delay_needed(root: &Path) -> bool {
    root.join("last-request").is_file()
}

fn touch_request_marker(root: &Path) -> Result<(), GameHackingError> {
    fs::write(root.join("last-request"), unix_seconds_now().to_string()).map_err(|failure| {
        provider_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("provider rate-limit marker could not be written: {failure}"),
        )
    })
}

fn cloudflare_marker_path(cache_root: &Path) -> PathBuf {
    cache_root.join(CLOUDFLARE_MARKER_FILE)
}

pub(crate) fn cloudflare_cooldown_remaining(cache_root: &Path) -> Option<Duration> {
    let contents = fs::read_to_string(cloudflare_marker_path(cache_root)).ok()?;
    let blocked_at = contents.trim().parse::<u64>().ok()?;
    let elapsed = Duration::from_secs(unix_seconds_now().saturating_sub(blocked_at));
    CLOUDFLARE_COOLDOWN
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
}

pub(crate) fn mark_cloudflare_blocked(cache_root: &Path) -> Result<(), GameHackingError> {
    atomic_write(
        &cloudflare_marker_path(cache_root),
        unix_seconds_now().to_string().as_bytes(),
    )
}

pub(crate) fn clear_cloudflare_marker(cache_root: &Path) {
    let _ = fs::remove_file(cloudflare_marker_path(cache_root));
}

pub(crate) fn check_cancelled<O: GameHackingRequestOptions>(
    options: &O,
) -> Result<(), GameHackingError> {
    if options
        .cancellation()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return Err(provider_error(
            GameHackingErrorKind::Cancelled,
            "GameHacking.org request was cancelled",
        ));
    }
    Ok(())
}

pub(crate) fn cancellable_delay<O: GameHackingRequestOptions>(
    options: &O,
    duration: Duration,
) -> Result<(), GameHackingError> {
    let slice = Duration::from_millis(100);
    let mut remaining = duration;
    while !remaining.is_zero() {
        check_cancelled(options)?;
        let pause = remaining.min(slice);
        std::thread::sleep(pause);
        remaining = remaining.saturating_sub(pause);
    }
    check_cancelled(options)
}

pub(crate) fn decode_provider_text<'a>(
    bytes: &'a [u8],
    charset: Option<&str>,
) -> std::borrow::Cow<'a, str> {
    if let Some(encoding) =
        charset.and_then(|label| encoding_rs::Encoding::for_label(label.trim().as_bytes()))
    {
        return encoding.decode(bytes).0;
    }
    String::from_utf8_lossy(bytes)
}

pub(crate) fn robots_disallows_archivefs(text: &str, path: &str) -> bool {
    let mut relevant_group = false;
    let mut saw_rule = false;
    let mut strongest_rule: Option<(usize, bool)> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("user-agent") {
            if saw_rule {
                relevant_group = false;
                saw_rule = false;
            }
            relevant_group |= value == "*" || value.eq_ignore_ascii_case("archivefs");
        } else if name.eq_ignore_ascii_case("disallow") {
            saw_rule = true;
            if relevant_group
                && !value.is_empty()
                && path.starts_with(value)
                && strongest_rule.is_none_or(|(length, _)| value.len() > length)
            {
                strongest_rule = Some((value.len(), false));
            }
        } else if name.eq_ignore_ascii_case("allow") {
            saw_rule = true;
            if relevant_group
                && !value.is_empty()
                && path.starts_with(value)
                && strongest_rule.is_none_or(|(length, allowed)| {
                    value.len() > length || (value.len() == length && !allowed)
                })
            {
                strongest_rule = Some((value.len(), true));
            }
        }
    }
    strongest_rule.is_some_and(|(_, allowed)| !allowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug)]
    struct Options {
        cache_root: PathBuf,
        force_refresh: bool,
        delay: Duration,
        cancellation: Option<Arc<AtomicBool>>,
    }

    impl GameHackingRequestOptions for Options {
        fn cache_root(&self) -> &Path {
            &self.cache_root
        }
        fn force_refresh(&self) -> bool {
            self.force_refresh
        }
        fn delay(&self) -> Duration {
            self.delay
        }
        fn cancellation(&self) -> Option<&AtomicBool> {
            self.cancellation.as_deref()
        }
    }

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "archivefs-gamehacking-shared-{label}-{}-{}",
            std::process::id(),
            unix_seconds_now()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn two_system_specs_share_one_cached_request_path_without_platform_branching() {
        let root = root("specs");
        let options = Options {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::ZERO,
            cancellation: None,
        };
        let client = GameHackingClient::default();
        for spec in [
            GameHackingRequestSpec {
                cache_file: "export-42.pnach",
                url: EXPORT_URL,
                maximum_bytes: 1024,
            },
            GameHackingRequestSpec {
                cache_file: "export-42.txt",
                url: EXPORT_URL,
                maximum_bytes: 1024,
            },
        ] {
            let expected = spec.cache_file.as_bytes().to_vec();
            let response = client
                .cached_request(spec, &options, |_| {
                    Ok(ProviderResponse {
                        bytes: expected.clone(),
                        charset: Some("utf-8".to_string()),
                        cached_fallback: false,
                        retrieved_at_unix_seconds: None,
                    })
                })
                .unwrap();
            assert_eq!(response.bytes, expected);
            assert_eq!(fs::read(root.join(spec.cache_file)).unwrap(), expected);
        }
        assert!(root.join("export-42.pnach").is_file());
        assert!(root.join("export-42.txt").is_file());
        assert_eq!(
            charset_cache_path(&root.join("export-42.pnach")),
            root.join("export-42.pnach.charset")
        );
        assert_eq!(
            retrieved_cache_path(&root.join("export-42.txt")),
            root.join("export-42.txt.retrieved")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cloudflare_classification_is_system_independent() {
        let body = b"<html><title>Just a moment...</title>Cloudflare Ray ID: 1</html>";
        for status in [200, 403] {
            assert_eq!(
                classify_gamehacking_http_response(status, Some("cloudflare"), body),
                GameHackingHttpClassification::CloudflareBlocked
            );
        }
    }

    #[test]
    fn both_specs_use_the_single_shared_transport_type() {
        let client = GameHackingClient::default();
        assert_eq!(
            std::any::type_name_of_val(&client.transport),
            std::any::type_name::<UreqGameHackingTransport>()
        );
    }

    #[test]
    fn shared_request_preserves_raw_parser_input_bytes_exactly() {
        let root = root("raw-bytes");
        let options = Options {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::ZERO,
            cancellation: None,
        };
        let client = GameHackingClient::default();
        let raw = b"\x00\x80<html>\xff\r\nraw provider bytes".to_vec();
        let spec = GameHackingRequestSpec {
            cache_file: "game-7.html",
            url: "https://gamehacking.org/game/7",
            maximum_bytes: 1024,
        };
        let live = client
            .cached_request(spec, &options, |_| {
                Ok(ProviderResponse {
                    bytes: raw.clone(),
                    charset: Some("windows-1252".to_string()),
                    cached_fallback: false,
                    retrieved_at_unix_seconds: None,
                })
            })
            .unwrap();
        assert_eq!(live.bytes, raw);

        let cached = client
            .cached_request(
                spec,
                &Options {
                    cache_root: root.clone(),
                    force_refresh: false,
                    delay: Duration::ZERO,
                    cancellation: None,
                },
                |_| panic!("cache hit must not execute transport"),
            )
            .unwrap();
        assert_eq!(cached.bytes, raw);
        assert_eq!(cached.charset.as_deref(), Some("windows-1252"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shared_cooldown_keeps_exact_cache_entries_isolated() {
        let root = root("cooldown-isolation");
        let retrieved = unix_seconds_now().to_string();
        for (name, bytes) in [
            ("export-42.pnach", b"ps2 bytes".as_slice()),
            ("export-42.txt", b"gamecube bytes".as_slice()),
        ] {
            fs::write(root.join(name), bytes).unwrap();
            fs::write(retrieved_cache_path(&root.join(name)), &retrieved).unwrap();
        }
        mark_cloudflare_blocked(&root).unwrap();
        let options = Options {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::ZERO,
            cancellation: None,
        };
        let client = GameHackingClient::default();
        for (name, expected) in [
            ("export-42.pnach", b"ps2 bytes".as_slice()),
            ("export-42.txt", b"gamecube bytes".as_slice()),
        ] {
            let response = client
                .cached_request(
                    GameHackingRequestSpec {
                        cache_file: name,
                        url: EXPORT_URL,
                        maximum_bytes: 1024,
                    },
                    &options,
                    |_| panic!("cooldown must not execute transport"),
                )
                .unwrap();
            assert!(response.cached_fallback);
            assert_eq!(response.bytes, expected);
        }
        assert_eq!(
            fs::read(root.join("export-42.pnach")).unwrap(),
            b"ps2 bytes"
        );
        assert_eq!(
            fs::read(root.join("export-42.txt")).unwrap(),
            b"gamecube bytes"
        );
        let _ = fs::remove_dir_all(root);
    }
}
