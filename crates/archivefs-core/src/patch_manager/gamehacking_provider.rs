//! Bounded, per-library-game access to GameHacking.org.
//!
//! Retrieval, HTML parsing, identity matching, PNACH parsing, and installation
//! remain separate. The provider never enumerates more than the one PS2 index
//! needed to find title candidates and fetches candidate game pages serially.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

use super::pcsx2::normalize_crc;
use super::pcsx2_identity::Pcsx2GameIdentity;
use super::pcsx2_provider::{
    Pcsx2CheatCategory, Pcsx2CheatConfidence, Pcsx2CheatProviderCatalogue,
    Pcsx2CheatProviderRecord, Pcsx2ProviderTrust,
};

pub const GAMEHACKING_PROVIDER_ID: &str = "gamehacking.org";
const BASE_URL: &str = "https://gamehacking.org";
const PS2_INDEX_URL: &str = "https://gamehacking.org/system/ps2/all";
const EXPORT_URL: &str = "https://gamehacking.org/inc/sub.exportCodes.php";
const ROBOTS_URL: &str = "https://gamehacking.org/robots.txt";
const USER_AGENT: &str = concat!(
    "ArchiveFS/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/davedap/archivefs; one-game-at-a-time cheat provider)"
);
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXPORT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RETRIES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameHackingErrorKind {
    UnsupportedSystem,
    IdentityIncomplete,
    IdentityConflict,
    NoMatch,
    AccessDenied,
    RateLimited,
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

fn error(kind: GameHackingErrorKind, detail: impl Into<String>) -> GameHackingError {
    GameHackingError {
        kind,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGame {
    pub game_id: u64,
    pub title: String,
    pub system: String,
    pub region: Option<String>,
    pub serial: Option<String>,
    pub crc: Option<String>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingCheat {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub patch_lines: Vec<String>,
    pub source_game_id: u64,
    pub source_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameHackingMatchStatus {
    Matched,
    NoMatch,
    IdentityConflict,
    IdentityIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingMatch {
    pub status: GameHackingMatchStatus,
    pub game: Option<GameHackingGame>,
    pub detail: String,
}

pub trait GameHackingSystemAdapter: Send + Sync {
    fn system_name(&self) -> &'static str;
    fn system_id(&self) -> u16;
    fn index_url(&self) -> &'static str;
    fn export_format(&self) -> &'static str;
    fn supports(&self, identity: &Pcsx2GameIdentity) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Ps2GameHackingAdapter;

impl GameHackingSystemAdapter for Ps2GameHackingAdapter {
    fn system_name(&self) -> &'static str {
        "PlayStation 2"
    }

    fn system_id(&self) -> u16 {
        16
    }

    fn index_url(&self) -> &'static str {
        PS2_INDEX_URL
    }

    fn export_format(&self) -> &'static str {
        "PCSX2"
    }

    fn supports(&self, identity: &Pcsx2GameIdentity) -> bool {
        identity.verified_crc().is_some() && identity.serial.is_some() && identity.region.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct GameHackingFetchOptions {
    pub cache_root: PathBuf,
    pub force_refresh: bool,
    pub delay: Duration,
    pub cancellation: Option<std::sync::Arc<AtomicBool>>,
}

impl GameHackingFetchOptions {
    pub fn defaults() -> Result<Self, GameHackingError> {
        Ok(Self {
            cache_root: gamehacking_cache_root()?,
            force_refresh: false,
            delay: Duration::from_secs(3),
            cancellation: None,
        })
    }
}

pub fn gamehacking_cache_root() -> Result<PathBuf, GameHackingError> {
    let database = crate::default_database_path().map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("ArchiveFS data directory is unavailable: {failure}"),
        )
    })?;
    let parent = database.parent().ok_or_else(|| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            "ArchiveFS database path has no parent directory",
        )
    })?;
    Ok(parent.join("cache/gamehacking"))
}

trait GameHackingTransport {
    fn get(&self, url: &str, maximum_bytes: usize) -> Result<Vec<u8>, GameHackingError>;
    fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, GameHackingError>;
}

#[derive(Debug, Clone)]
struct UreqGameHackingTransport {
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
    ) -> Result<Vec<u8>, GameHackingError> {
        let status = response.status().as_u16();
        if status == 403 || status == 401 {
            return Err(error(
                GameHackingErrorKind::AccessDenied,
                format!("GameHacking.org denied access (HTTP {status})"),
            ));
        }
        if status == 429 {
            return Err(error(
                GameHackingErrorKind::RateLimited,
                "GameHacking.org asked ArchiveFS to slow down (HTTP 429)",
            ));
        }
        if (500..600).contains(&status) {
            return Err(error(
                GameHackingErrorKind::TemporaryFailure,
                format!("GameHacking.org is temporarily unavailable (HTTP {status})"),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(error(
                GameHackingErrorKind::PermanentHttpFailure,
                format!("GameHacking.org returned HTTP {status}"),
            ));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((maximum_bytes + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|failure| {
                error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org response could not be read: {failure}"),
                )
            })?;
        if bytes.len() > maximum_bytes {
            return Err(error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org response exceeded the bounded size limit",
            ));
        }
        Ok(bytes)
    }
}

impl GameHackingTransport for UreqGameHackingTransport {
    fn get(&self, url: &str, maximum_bytes: usize) -> Result<Vec<u8>, GameHackingError> {
        validate_provider_url(url)?;
        let response = self
            .agent
            .get(url)
            .header("Accept", "text/html, text/plain")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(classify_transport_error)?;
        Self::read_response(response, maximum_bytes)
    }

    fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, GameHackingError> {
        validate_provider_url(url)?;
        let response = self
            .agent
            .post(url)
            .header("Accept", "text/plain, application/octet-stream")
            .header("Accept-Encoding", "identity")
            .header("User-Agent", USER_AGENT)
            .send_form(form.iter().map(|(key, value)| (*key, value.as_str())))
            .map_err(classify_transport_error)?;
        Self::read_response(response, maximum_bytes)
    }
}

fn validate_provider_url(value: &str) -> Result<(), GameHackingError> {
    let url = Url::parse(value).map_err(|_| {
        error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is invalid",
        )
    })?;
    if url.scheme() != "https" || url.host_str() != Some("gamehacking.org") {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is outside the fixed GameHacking.org HTTPS origin",
        ));
    }
    Ok(())
}

fn classify_transport_error(failure: ureq::Error) -> GameHackingError {
    error(
        GameHackingErrorKind::TemporaryFailure,
        format!("GameHacking.org request failed: {failure}"),
    )
}

#[derive(Debug, Clone)]
struct GameLink {
    game_id: u64,
    title: String,
}

pub struct GameHackingProvider<A = Ps2GameHackingAdapter> {
    adapter: A,
    transport: UreqGameHackingTransport,
}

impl Default for GameHackingProvider<Ps2GameHackingAdapter> {
    fn default() -> Self {
        Self {
            adapter: Ps2GameHackingAdapter,
            transport: UreqGameHackingTransport::new(),
        }
    }
}

impl<A: GameHackingSystemAdapter> GameHackingProvider<A> {
    pub fn match_game(
        &self,
        identity: &Pcsx2GameIdentity,
        options: &GameHackingFetchOptions,
    ) -> Result<GameHackingMatch, GameHackingError> {
        if !self.adapter.supports(identity) {
            return Ok(GameHackingMatch {
                status: GameHackingMatchStatus::IdentityIncomplete,
                game: None,
                detail: "A verified PS2 serial, region, and PCSX2 CRC are required before checking GameHacking.org.".to_string(),
            });
        }
        self.check_robots(options, &["/system/ps2/all", "/game/"])?;
        let index = self.cached_request(
            "ps2-index.html",
            self.adapter.index_url(),
            MAX_INDEX_BYTES,
            options,
            |transport| transport.get(self.adapter.index_url(), MAX_INDEX_BYTES),
        )?;
        let title_key = normalized_title(&identity.title);
        let links = parse_game_links(&index)?;
        let candidates: Vec<GameLink> = links
            .into_iter()
            .filter(|link| normalized_title(&link.title) == title_key)
            .collect();
        if candidates.is_empty() {
            return Ok(GameHackingMatch {
                status: GameHackingMatchStatus::NoMatch,
                game: None,
                detail: "No GameHacking.org title matched this local game.".to_string(),
            });
        }
        let mut conflicts = Vec::new();
        for link in candidates {
            check_cancelled(options)?;
            let url = format!("{BASE_URL}/game/{}", link.game_id);
            let cache_name = format!("game-{}.html", link.game_id);
            let page =
                self.cached_request(&cache_name, &url, MAX_PAGE_BYTES, options, |transport| {
                    transport.get(&url, MAX_PAGE_BYTES)
                })?;
            let game = parse_gamehacking_game_page(link.game_id, &url, &page)?;
            match exact_identity_match(identity, &game) {
                Ok(()) => {
                    return Ok(GameHackingMatch {
                        status: GameHackingMatchStatus::Matched,
                        detail: format!(
                            "Matched {} by serial, region, and verified CRC.",
                            game.title
                        ),
                        game: Some(game),
                    });
                }
                Err(detail) => conflicts.push(detail),
            }
        }
        Ok(GameHackingMatch {
            status: GameHackingMatchStatus::IdentityConflict,
            game: None,
            detail: format!(
                "Title matched, but identity conflicted: {}",
                conflicts.join("; ")
            ),
        })
    }

    pub fn fetch_cheats(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        self.check_robots(options, &["/inc/sub.exportCodes.php"])?;
        exact_identity_match(identity, game)
            .map_err(|detail| error(GameHackingErrorKind::IdentityConflict, detail))?;
        let filename = identity
            .serial
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&identity.title);
        let form = [
            ("format", self.adapter.export_format().to_string()),
            ("codID", String::new()),
            ("filename", filename.to_string()),
            ("sysID", self.adapter.system_id().to_string()),
            ("gamID", game.game_id.to_string()),
            ("download", "true".to_string()),
        ];
        let cache_name = format!("export-{}.pnach", game.game_id);
        let bytes = self.cached_request(
            &cache_name,
            EXPORT_URL,
            MAX_EXPORT_BYTES,
            options,
            |transport| transport.post_form(EXPORT_URL, &form, MAX_EXPORT_BYTES),
        )?;
        parse_gamehacking_pnach(game, &bytes)
    }

    pub fn catalogue(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        cheats: &[GameHackingCheat],
    ) -> Result<Pcsx2CheatProviderCatalogue, GameHackingError> {
        exact_identity_match(identity, game)
            .map_err(|detail| error(GameHackingErrorKind::IdentityConflict, detail))?;
        let crc = identity.verified_crc().ok_or_else(|| {
            error(
                GameHackingErrorKind::IdentityIncomplete,
                "verified local PCSX2 CRC is required",
            )
        })?;
        Ok(Pcsx2CheatProviderCatalogue {
            provider_id: GAMEHACKING_PROVIDER_ID.to_string(),
            provider_name: "GameHacking.org".to_string(),
            source: game.source_url.clone(),
            trust: Pcsx2ProviderTrust::Approved,
            records: cheats
                .iter()
                .map(|cheat| Pcsx2CheatProviderRecord {
                    id: cheat.id.clone(),
                    name: cheat.name.clone(),
                    description: cheat.description.clone(),
                    author: cheat.author.clone(),
                    source_game_id: Some(game.game_id.to_string()),
                    source_url: Some(cheat.source_url.clone()),
                    game_crc: crc.to_string(),
                    serial_constraint: game.serial.clone(),
                    region_constraint: game.region.clone(),
                    patch_lines: cheat.patch_lines.clone(),
                    category: Pcsx2CheatCategory::OrdinaryCheat,
                    confidence: Pcsx2CheatConfidence::VerifiedCrcAndConstraints,
                })
                .collect(),
        })
    }

    fn cached_request<F>(
        &self,
        file_name: &str,
        _url: &str,
        maximum_bytes: usize,
        options: &GameHackingFetchOptions,
        request: F,
    ) -> Result<Vec<u8>, GameHackingError>
    where
        F: Fn(&UreqGameHackingTransport) -> Result<Vec<u8>, GameHackingError>,
    {
        prepare_cache(&options.cache_root)?;
        let path = options.cache_root.join(file_name);
        if !options.force_refresh && path.is_file() {
            return bounded_read(&path, maximum_bytes);
        }
        let mut last_error = None;
        for attempt in 0..MAX_RETRIES {
            check_cancelled(options)?;
            if attempt > 0 || request_delay_needed(&options.cache_root) {
                cancellable_delay(options, options.delay.saturating_mul(1_u32 << attempt))?;
            }
            match request(&self.transport) {
                Ok(bytes) => {
                    atomic_write(&path, &bytes)?;
                    touch_request_marker(&options.cache_root)?;
                    return Ok(bytes);
                }
                Err(failure)
                    if matches!(
                        failure.kind,
                        GameHackingErrorKind::RateLimited | GameHackingErrorKind::TemporaryFailure
                    ) =>
                {
                    last_error = Some(failure);
                }
                Err(failure) => return Err(failure),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            error(
                GameHackingErrorKind::TemporaryFailure,
                "GameHacking.org retry limit reached",
            )
        }))
    }

    fn check_robots(
        &self,
        options: &GameHackingFetchOptions,
        paths: &[&str],
    ) -> Result<(), GameHackingError> {
        let robots =
            self.cached_request("robots.txt", ROBOTS_URL, 256 * 1024, options, |transport| {
                transport.get(ROBOTS_URL, 256 * 1024)
            })?;
        let text = std::str::from_utf8(&robots).map_err(|_| {
            error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org robots.txt is not UTF-8",
            )
        })?;
        for path in paths {
            if robots_disallows_archivefs(text, path) {
                return Err(error(
                    GameHackingErrorKind::AccessDenied,
                    format!("GameHacking.org robots.txt does not allow access to {path}"),
                ));
            }
        }
        Ok(())
    }
}

fn robots_disallows_archivefs(text: &str, path: &str) -> bool {
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

fn exact_identity_match(
    identity: &Pcsx2GameIdentity,
    game: &GameHackingGame,
) -> Result<(), String> {
    let local_crc = identity
        .verified_crc()
        .ok_or_else(|| "local CRC is not verified".to_string())?;
    let local_serial = identity
        .serial
        .as_deref()
        .ok_or_else(|| "local serial is not verified".to_string())?;
    let remote_serial = game
        .serial
        .as_deref()
        .ok_or_else(|| "provider page has no serial".to_string())?;
    if normalize_identity_token(local_serial) != normalize_identity_token(remote_serial) {
        return Err(format!(
            "serial {remote_serial} does not match {local_serial}"
        ));
    }
    let remote_crc = game
        .crc
        .as_deref()
        .and_then(normalize_crc)
        .ok_or_else(|| "provider page has no valid CRC".to_string())?;
    if remote_crc != local_crc {
        return Err(format!("CRC {remote_crc} does not match {local_crc}"));
    }
    let local_region = identity
        .region
        .as_deref()
        .ok_or_else(|| "local region is not verified".to_string())?;
    let remote_region = game
        .region
        .as_deref()
        .ok_or_else(|| "provider page has no region".to_string())?;
    if region_family(local_region) != region_family(remote_region) {
        return Err(format!(
            "region {remote_region} does not match {local_region}"
        ));
    }
    Ok(())
}

fn region_family(value: &str) -> String {
    let normalized = normalize_identity_token(value);
    if normalized.contains("PAL") || normalized.contains("EUROPE") || normalized == "EU" {
        "PAL".to_string()
    } else if normalized.contains("NTSCU")
        || normalized.contains("USA")
        || normalized.contains("NORTHAMERICA")
    {
        "NTSCU".to_string()
    } else if normalized.contains("NTSCJ")
        || normalized.contains("JAPAN")
        || normalized.contains("JAPANESE")
    {
        "NTSCJ".to_string()
    } else {
        normalized
    }
}

fn normalized_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_identity_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

fn parse_game_links(bytes: &[u8]) -> Result<Vec<GameLink>, GameHackingError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org index is not UTF-8",
        )
    })?;
    let document = Html::parse_document(text);
    let selector = Selector::parse("a[href^='/game/']").expect("static selector");
    let mut seen = BTreeSet::new();
    let mut links = Vec::new();
    for node in document.select(&selector) {
        let Some(id) = node
            .value()
            .attr("href")
            .and_then(|href| href.trim_start_matches("/game/").split('/').next())
            .and_then(|id| id.parse::<u64>().ok())
        else {
            continue;
        };
        let title = node.text().collect::<String>().trim().to_string();
        if !title.is_empty() && seen.insert(id) {
            links.push(GameLink { game_id: id, title });
        }
    }
    if links.is_empty() {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PS2 index contained no game links",
        ));
    }
    Ok(links)
}

pub fn parse_gamehacking_game_page(
    game_id: u64,
    source_url: &str,
    bytes: &[u8],
) -> Result<GameHackingGame, GameHackingError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org game page is not UTF-8",
        )
    })?;
    let document = Html::parse_document(text);
    let heading = Selector::parse("h1, h2.game-title, .game-title").expect("static selector");
    let title = document
        .select(&heading)
        .map(|node| node.text().collect::<String>().trim().to_string())
        .find(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org game page has no title",
            )
        })?;
    let row_selector = Selector::parse("tr, dt, .game-info-row").expect("static selector");
    let mut fields = BTreeMap::new();
    for row in document.select(&row_selector) {
        let value = row.text().collect::<Vec<_>>().join(" ");
        let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
        for label in ["System", "Region", "Serial", "CRC"] {
            if let Some(rest) = compact
                .strip_prefix(label)
                .and_then(|rest| rest.trim_start_matches([':', ' ']).split('|').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                fields.entry(label).or_insert_with(|| rest.to_string());
            }
        }
    }
    Ok(GameHackingGame {
        game_id,
        title,
        system: fields
            .remove("System")
            .unwrap_or_else(|| "PlayStation 2".to_string()),
        region: fields.remove("Region"),
        serial: fields.remove("Serial"),
        crc: fields.remove("CRC").and_then(|value| normalize_crc(&value)),
        source_url: source_url.to_string(),
    })
}

pub fn parse_gamehacking_pnach(
    game: &GameHackingGame,
    bytes: &[u8],
) -> Result<Vec<GameHackingCheat>, GameHackingError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PNACH export is not UTF-8",
        )
    })?;
    let mut cheats = Vec::new();
    let mut comments = Vec::new();
    let mut patches = Vec::new();
    let flush = |comments: &mut Vec<String>,
                 patches: &mut Vec<String>,
                 cheats: &mut Vec<GameHackingCheat>| {
        if patches.is_empty() {
            return;
        }
        let mut author = None;
        let mut description = Vec::new();
        let mut name = None;
        for comment in comments.drain(..) {
            let trimmed = comment.trim();
            if let Some(value) = strip_label(trimmed, "author") {
                author = Some(value.to_string());
            } else if let Some(value) = strip_label(trimmed, "description") {
                description.push(value.to_string());
            } else if name.is_none() && !trimmed.is_empty() {
                name = Some(trimmed.to_string());
            } else if !trimmed.is_empty() {
                description.push(trimmed.to_string());
            }
        }
        let index = cheats.len() + 1;
        let name = name.unwrap_or_else(|| format!("Cheat {index}"));
        cheats.push(GameHackingCheat {
            id: format!("gh-{}-{index}", game.game_id),
            name,
            author,
            description: (!description.is_empty()).then(|| description.join(" ")),
            patch_lines: std::mem::take(patches),
            source_game_id: game.game_id,
            source_url: game.source_url.clone(),
        });
    };
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(comment) = line.trim_start().strip_prefix("//") {
            if !patches.is_empty() {
                flush(&mut comments, &mut patches, &mut cheats);
            }
            comments.push(comment.trim().to_string());
        } else if line.trim_start().starts_with("patch=") {
            patches.push(line.to_string());
        } else if line.trim().is_empty() && !patches.is_empty() {
            flush(&mut comments, &mut patches, &mut cheats);
        }
    }
    flush(&mut comments, &mut patches, &mut cheats);
    if cheats.is_empty() {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org export contained no supported PCSX2 patch lines",
        ));
    }
    Ok(cheats)
}

fn strip_label<'a>(value: &'a str, label: &str) -> Option<&'a str> {
    let (head, tail) = value.split_once(':')?;
    head.trim()
        .eq_ignore_ascii_case(label)
        .then_some(tail.trim())
        .filter(|tail| !tail.is_empty())
}

fn prepare_cache(root: &Path) -> Result<(), GameHackingError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(error(
            GameHackingErrorKind::CacheUnavailable,
            "GameHacking.org cache root must be an absolute non-root path",
        ));
    }
    fs::create_dir_all(root).map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("GameHacking.org cache could not be created: {failure}"),
        )
    })
}

fn bounded_read(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, GameHackingError> {
    let metadata = path.symlink_metadata().map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("cached provider response could not be inspected: {failure}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum_bytes as u64
    {
        return Err(error(
            GameHackingErrorKind::CacheUnavailable,
            "cached provider response is unsafe or oversized",
        ));
    }
    fs::read(path).map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("cached provider response could not be read: {failure}"),
        )
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), GameHackingError> {
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
        return Err(error(
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
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    fs::write(root.join("last-request"), timestamp.to_string()).map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("provider rate-limit marker could not be written: {failure}"),
        )
    })
}

fn check_cancelled(options: &GameHackingFetchOptions) -> Result<(), GameHackingError> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return Err(error(
            GameHackingErrorKind::Cancelled,
            "GameHacking.org request was cancelled",
        ));
    }
    Ok(())
}

fn cancellable_delay(
    options: &GameHackingFetchOptions,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> GameHackingGame {
        GameHackingGame {
            game_id: 42,
            title: "Example Game".to_string(),
            system: "PlayStation 2".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-12345".to_string()),
            crc: Some("A1B2C3D4".to_string()),
            source_url: "https://gamehacking.org/game/42".to_string(),
        }
    }

    #[test]
    fn game_page_identity_fields_are_parsed_without_network() {
        let html = br#"<h1>Example Game</h1><table>
            <tr><th>System</th><td>PlayStation 2</td></tr>
            <tr><th>Region</th><td>NTSC-U</td></tr>
            <tr><th>Serial</th><td>SLUS-12345</td></tr>
            <tr><th>CRC</th><td>A1B2C3D4</td></tr></table>"#;
        let parsed =
            parse_gamehacking_game_page(42, "https://gamehacking.org/game/42", html).unwrap();
        assert_eq!(parsed, game());
    }

    #[test]
    fn native_pnach_preserves_names_authors_descriptions_and_lines() {
        let source = b"// Infinite health\n// Author: Ada\n// Description: Never decreases\npatch=1,EE,20123456,word,00000001\n\n// Max money\npatch=1,EE,20123458,word,0000FFFF\n";
        let cheats = parse_gamehacking_pnach(&game(), source).unwrap();
        assert_eq!(cheats.len(), 2);
        assert_eq!(cheats[0].author.as_deref(), Some("Ada"));
        assert_eq!(cheats[0].description.as_deref(), Some("Never decreases"));
        assert_eq!(
            cheats[0].patch_lines[0],
            "patch=1,EE,20123456,word,00000001"
        );
    }

    #[test]
    fn unknown_or_encrypted_exports_are_rejected() {
        assert_eq!(
            parse_gamehacking_pnach(&game(), b"// encrypted\nDEADBEEF 00000001")
                .unwrap_err()
                .kind,
            GameHackingErrorKind::InvalidResponse
        );
    }

    #[test]
    fn title_index_deduplicates_game_ids() {
        let links = parse_game_links(
            br#"<a href="/game/42">Example Game</a><a href="/game/42">Duplicate</a>"#,
        )
        .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].game_id, 42);
    }

    #[test]
    fn provider_urls_are_fixed_to_https_origin() {
        assert!(validate_provider_url("https://gamehacking.org/game/42").is_ok());
        assert!(validate_provider_url("http://gamehacking.org/game/42").is_err());
        assert!(validate_provider_url("https://example.test/game/42").is_err());
    }

    #[test]
    fn robots_rules_for_archivefs_and_wildcard_are_respected() {
        assert!(robots_disallows_archivefs(
            "User-agent: *\nDisallow: /inc/\n",
            "/inc/sub.exportCodes.php"
        ));
        assert!(!robots_disallows_archivefs(
            "User-agent: *\nDisallow: /private/\n",
            "/game/42"
        ));
    }

    #[test]
    fn exact_identity_requires_serial_crc_and_nonconflicting_region() {
        use crate::patch_manager::Pcsx2IdentityState;

        let identity = Pcsx2GameIdentity {
            archive_path: PathBuf::from("/games/example.iso"),
            title: "Example Game".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-12345".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
            state: Pcsx2IdentityState::Verified,
            evidence: Vec::new(),
            plain_failure_reason: None,
        };
        assert!(exact_identity_match(&identity, &game()).is_ok());
        let mut conflict = game();
        conflict.serial = Some("SLES-99999".to_string());
        assert!(exact_identity_match(&identity, &conflict).is_err());
        conflict = game();
        conflict.crc = Some("FFFFFFFF".to_string());
        assert!(exact_identity_match(&identity, &conflict).is_err());
        conflict = game();
        conflict.region = Some("PAL".to_string());
        assert!(exact_identity_match(&identity, &conflict).is_err());
    }

    #[test]
    fn cancellation_interrupts_rate_limit_delay() {
        let cancellation = std::sync::Arc::new(AtomicBool::new(true));
        let options = GameHackingFetchOptions {
            cache_root: PathBuf::from("/tmp/archivefs-unused-gamehacking-test"),
            force_refresh: false,
            delay: Duration::from_secs(3),
            cancellation: Some(cancellation),
        };
        assert_eq!(
            cancellable_delay(&options, Duration::from_secs(3))
                .unwrap_err()
                .kind,
            GameHackingErrorKind::Cancelled
        );
    }
}
