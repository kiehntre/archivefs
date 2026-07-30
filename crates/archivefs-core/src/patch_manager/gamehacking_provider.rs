//! Cached PS2 catalogue and per-library-game access to GameHacking.org.
//!
//! Retrieval, HTML parsing, identity matching, PNACH parsing, and installation
//! remain separate. The explicit index command walks only the numbered public
//! PS2 table pages. Runtime matching is local, and only one selected game's
//! PNACH is requested after an automatic match or user confirmation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const MAX_EXPORT_BYTES: usize = 2 * 1024 * 1024;
const MAX_RETRIES: u8 = 3;
const PS2_CATALOGUE_SCHEMA_VERSION: u32 = 1;
const PS2_CATALOGUE_FILE: &str = "ps2-catalogue.json";
const PS2_INDEX_ROOT_CACHE_FILE: &str = "ps2-index-root.html";
const LEGACY_PS2_INDEX_CACHE_FILE: &str = "ps2-index.html";
const MAX_PS2_INDEX_PAGES: usize = 512;

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
    Candidates,
    NoMatch,
    IdentityConflict,
    IdentityIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingMatch {
    pub status: GameHackingMatchStatus,
    pub game: Option<GameHackingGame>,
    pub candidates: Vec<GameHackingMatchCandidate>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GameHackingMatchStrength {
    ExactSerialAndCrc,
    ExactSerialAndRegion,
    ExactCrc,
    NormalizedTitle,
}

impl GameHackingMatchStrength {
    fn priority(self) -> u8 {
        match self {
            Self::ExactSerialAndCrc => 1,
            Self::ExactSerialAndRegion => 2,
            Self::ExactCrc => 3,
            Self::NormalizedTitle => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ExactSerialAndCrc => "exact serial + CRC",
            Self::ExactSerialAndRegion => "exact serial + compatible region",
            Self::ExactCrc => "exact CRC",
            Self::NormalizedTitle => "normalized title only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingMatchCandidate {
    pub game: GameHackingGame,
    pub strength: GameHackingMatchStrength,
    pub requires_user_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingIndexPage {
    pub page_number: u32,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub sha256: String,
    pub game_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingIndexRecord {
    pub game_id: u64,
    pub title: String,
    pub serial: Option<String>,
    pub region: Option<String>,
    pub crc: Option<String>,
    pub source_url: String,
    pub index_source_url: String,
    pub retrieved_at_unix_seconds: u64,
}

impl GameHackingIndexRecord {
    fn as_game(&self) -> GameHackingGame {
        GameHackingGame {
            game_id: self.game_id,
            title: self.title.clone(),
            system: "PlayStation 2".to_string(),
            region: self.region.clone(),
            serial: self.serial.clone(),
            crc: self.crc.clone(),
            source_url: self.source_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingPs2Catalogue {
    pub schema_version: u32,
    pub provider: String,
    pub system: String,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub pages: Vec<GameHackingIndexPage>,
    pub games: Vec<GameHackingIndexRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GameHackingIndexRefreshResult {
    pub catalogue_path: PathBuf,
    pub pages_total: usize,
    pub pages_downloaded: usize,
    pub pages_reused: usize,
    pub games: usize,
    pub retrieved_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingIndexProgress {
    pub pages_complete: usize,
    pub pages_total: usize,
    pub page_number: Option<u32>,
    pub downloaded: bool,
    pub games_collected: usize,
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
        identity.verified_crc().is_some()
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
    fn get(&self, url: &str, maximum_bytes: usize) -> Result<ProviderResponse, GameHackingError>;
    fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        maximum_bytes: usize,
    ) -> Result<ProviderResponse, GameHackingError>;
}

#[derive(Debug, Clone)]
struct ProviderResponse {
    bytes: Vec<u8>,
    charset: Option<String>,
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
    ) -> Result<ProviderResponse, GameHackingError> {
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
        Ok(ProviderResponse { bytes, charset })
    }
}

impl GameHackingTransport for UreqGameHackingTransport {
    fn get(&self, url: &str, maximum_bytes: usize) -> Result<ProviderResponse, GameHackingError> {
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
    ) -> Result<ProviderResponse, GameHackingError> {
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

fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_ascii_lowercase())
    })
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
                candidates: Vec::new(),
                detail: "A verified local PCSX2 executable CRC is required before checking the cached GameHacking.org PS2 catalogue.".to_string(),
            });
        }
        let catalogue = load_ps2_catalogue(&options.cache_root)?;
        let mut candidates = match_ps2_catalogue(identity, &catalogue);
        if candidates.is_empty() {
            return Ok(GameHackingMatch {
                status: GameHackingMatchStatus::NoMatch,
                game: None,
                candidates: Vec::new(),
                detail: "No serial, CRC, or normalized-title match exists in the cached GameHacking.org PS2 catalogue.".to_string(),
            });
        }
        candidates.sort_by(|left, right| {
            left.strength
                .priority()
                .cmp(&right.strength.priority())
                .then_with(|| left.game.title.cmp(&right.game.title))
                .then_with(|| left.game.game_id.cmp(&right.game.game_id))
        });
        let best_priority = candidates[0].strength.priority();
        candidates.retain(|candidate| candidate.strength.priority() == best_priority);
        if candidates.len() == 1 && !candidates[0].requires_user_confirmation {
            let selected = candidates.remove(0);
            return Ok(GameHackingMatch {
                status: GameHackingMatchStatus::Matched,
                detail: format!(
                    "Matched {} by {} from the cached PS2 catalogue.",
                    selected.game.title,
                    selected.strength.label()
                ),
                game: Some(selected.game),
                candidates: Vec::new(),
            });
        }
        Ok(GameHackingMatch {
            status: GameHackingMatchStatus::Candidates,
            game: None,
            detail: if best_priority == GameHackingMatchStrength::NormalizedTitle.priority() {
                "Only normalized-title candidates were found. Confirm the correct GameHacking.org game before downloading its PCSX2 export.".to_string()
            } else {
                "More than one equally strong identity match was found. Confirm the correct GameHacking.org game before downloading its PCSX2 export.".to_string()
            },
            candidates,
        })
    }

    pub fn refresh_ps2_index<F>(
        &self,
        options: &GameHackingFetchOptions,
        mut progress: F,
    ) -> Result<GameHackingIndexRefreshResult, GameHackingError>
    where
        F: FnMut(GameHackingIndexProgress),
    {
        self.check_robots(options, &["/system/ps2/all"])?;
        prepare_cache(&options.cache_root)?;
        let preferred_root = options.cache_root.join(PS2_INDEX_ROOT_CACHE_FILE);
        let legacy_root = options.cache_root.join(LEGACY_PS2_INDEX_CACHE_FILE);
        let root_cache_name = if preferred_root.is_file() {
            PS2_INDEX_ROOT_CACHE_FILE
        } else if legacy_root.is_file() {
            LEGACY_PS2_INDEX_CACHE_FILE
        } else {
            PS2_INDEX_ROOT_CACHE_FILE
        };
        let root_path = options.cache_root.join(root_cache_name);
        let root_was_cached = root_path.is_file();
        let resume_options = GameHackingFetchOptions {
            cache_root: options.cache_root.clone(),
            force_refresh: false,
            delay: Duration::from_secs(2),
            cancellation: options.cancellation.clone(),
        };
        let root = self.cached_request(
            root_cache_name,
            self.adapter.index_url(),
            MAX_INDEX_BYTES,
            &resume_options,
            |transport| transport.get(self.adapter.index_url(), MAX_INDEX_BYTES),
        )?;
        let page_numbers = parse_ps2_index_page_numbers(&root.bytes, root.charset.as_deref())?;
        if page_numbers.len() > MAX_PS2_INDEX_PAGES {
            return Err(error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org PS2 index exceeded the page limit",
            ));
        }
        let mut pages = Vec::with_capacity(page_numbers.len());
        let mut games_by_id = BTreeMap::<u64, GameHackingIndexRecord>::new();
        let mut downloaded = 0usize;
        let mut reused = 0usize;
        for (position, page_number) in page_numbers.iter().copied().enumerate() {
            check_cancelled(&resume_options)?;
            let url = format!("{}/{}", self.adapter.index_url(), page_number);
            let cache_name = format!("ps2-index-page-{page_number}.html");
            let cache_path = options.cache_root.join(&cache_name);
            let (response, was_cached, retrieval_path, page_source_url) = if page_number == 0 {
                (
                    root.clone(),
                    root_was_cached,
                    root_path.clone(),
                    self.adapter.index_url().to_string(),
                )
            } else {
                let was_cached = cache_path.is_file();
                let response = self.cached_request(
                    &cache_name,
                    &url,
                    MAX_INDEX_BYTES,
                    &resume_options,
                    |transport| transport.get(&url, MAX_INDEX_BYTES),
                )?;
                (response, was_cached, cache_path, url.clone())
            };
            if was_cached {
                reused += 1;
            } else {
                downloaded += 1;
            }
            let retrieved_at = cache_retrieved_at(&retrieval_path)?;
            let mut page_games = parse_ps2_index_page(
                &page_source_url,
                retrieved_at,
                &response.bytes,
                response.charset.as_deref(),
            )?;
            page_games.sort_by_key(|game| game.game_id);
            for game in &page_games {
                games_by_id
                    .entry(game.game_id)
                    .or_insert_with(|| game.clone());
            }
            pages.push(GameHackingIndexPage {
                page_number,
                source_url: page_source_url,
                retrieved_at_unix_seconds: retrieved_at,
                sha256: sha256_hex(&response.bytes),
                game_count: page_games.len(),
            });
            progress(GameHackingIndexProgress {
                pages_complete: position + 1,
                pages_total: page_numbers.len(),
                page_number: Some(page_number),
                downloaded: !was_cached,
                games_collected: games_by_id.len(),
            });
        }
        let mut games = games_by_id.into_values().collect::<Vec<_>>();
        games.sort_by(|left, right| {
            left.game_id
                .cmp(&right.game_id)
                .then_with(|| left.title.cmp(&right.title))
        });
        pages.sort_by_key(|page| page.page_number);
        let retrieved_at = pages
            .iter()
            .map(|page| page.retrieved_at_unix_seconds)
            .max()
            .unwrap_or_else(unix_seconds_now);
        let catalogue = GameHackingPs2Catalogue {
            schema_version: PS2_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_PROVIDER_ID.to_string(),
            system: self.adapter.system_name().to_string(),
            source_url: self.adapter.index_url().to_string(),
            retrieved_at_unix_seconds: retrieved_at,
            pages,
            games,
        };
        let catalogue_path = options.cache_root.join(PS2_CATALOGUE_FILE);
        let mut bytes = serde_json::to_vec_pretty(&catalogue).map_err(|failure| {
            error(
                GameHackingErrorKind::CacheUnavailable,
                format!("GameHacking.org catalogue could not be serialized: {failure}"),
            )
        })?;
        bytes.push(b'\n');
        atomic_write(&catalogue_path, &bytes)?;
        Ok(GameHackingIndexRefreshResult {
            catalogue_path,
            pages_total: catalogue.pages.len(),
            pages_downloaded: downloaded,
            pages_reused: reused,
            games: catalogue.games.len(),
            retrieved_at_unix_seconds: retrieved_at,
        })
    }

    pub fn fetch_cheats(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        self.check_robots(options, &["/inc/sub.exportCodes.php"])?;
        authorize_catalogue_match(identity, game, false)?;
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
        parse_gamehacking_pnach(game, &bytes.bytes)
    }

    pub fn fetch_cheats_for_confirmed_candidate(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        authorize_catalogue_match(identity, game, true)?;
        self.fetch_export(game, identity, options)
    }

    fn fetch_export(
        &self,
        game: &GameHackingGame,
        identity: &Pcsx2GameIdentity,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        self.check_robots(options, &["/inc/sub.exportCodes.php"])?;
        let filename = identity.serial.as_deref().unwrap_or(&identity.title);
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
        parse_gamehacking_pnach(game, &bytes.bytes)
    }

    pub fn catalogue(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        cheats: &[GameHackingCheat],
    ) -> Result<Pcsx2CheatProviderCatalogue, GameHackingError> {
        authorize_catalogue_match(identity, game, false)?;
        self.catalogue_from_authorized_game(identity, game, cheats)
    }

    pub fn catalogue_for_confirmed_candidate(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        cheats: &[GameHackingCheat],
    ) -> Result<Pcsx2CheatProviderCatalogue, GameHackingError> {
        authorize_catalogue_match(identity, game, true)?;
        self.catalogue_from_authorized_game(identity, game, cheats)
    }

    fn catalogue_from_authorized_game(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        cheats: &[GameHackingCheat],
    ) -> Result<Pcsx2CheatProviderCatalogue, GameHackingError> {
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
                    serial_constraint: identity.serial.clone(),
                    region_constraint: identity.region.clone(),
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
    ) -> Result<ProviderResponse, GameHackingError>
    where
        F: Fn(&UreqGameHackingTransport) -> Result<ProviderResponse, GameHackingError>,
    {
        prepare_cache(&options.cache_root)?;
        let path = options.cache_root.join(file_name);
        if !options.force_refresh && path.is_file() {
            return Ok(ProviderResponse {
                bytes: bounded_read(&path, maximum_bytes)?,
                charset: read_cached_charset(&path)?,
            });
        }
        let mut last_error = None;
        for attempt in 0..MAX_RETRIES {
            check_cancelled(options)?;
            if attempt > 0 || request_delay_needed(&options.cache_root) {
                cancellable_delay(options, options.delay.saturating_mul(1_u32 << attempt))?;
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
                    touch_request_marker(&options.cache_root)?;
                    return Ok(response);
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
        let text = decode_provider_text(&robots.bytes, robots.charset.as_deref());
        for path in paths {
            if robots_disallows_archivefs(&text, path) {
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

fn authorize_catalogue_match(
    identity: &Pcsx2GameIdentity,
    game: &GameHackingGame,
    user_confirmed: bool,
) -> Result<GameHackingMatchStrength, GameHackingError> {
    let strength = classify_catalogue_match(identity, game).ok_or_else(|| {
        error(
            GameHackingErrorKind::IdentityConflict,
            "selected GameHacking.org game no longer matches local serial, CRC, region, or title",
        )
    })?;
    if strength == GameHackingMatchStrength::NormalizedTitle && !user_confirmed {
        return Err(error(
            GameHackingErrorKind::IdentityConflict,
            "normalized-title-only GameHacking.org candidate requires explicit user confirmation",
        ));
    }
    Ok(strength)
}

fn match_ps2_catalogue(
    identity: &Pcsx2GameIdentity,
    catalogue: &GameHackingPs2Catalogue,
) -> Vec<GameHackingMatchCandidate> {
    catalogue
        .games
        .iter()
        .filter_map(|record| {
            let game = record.as_game();
            let strength = classify_catalogue_match(identity, &game)?;
            Some(GameHackingMatchCandidate {
                game,
                strength,
                requires_user_confirmation: strength == GameHackingMatchStrength::NormalizedTitle,
            })
        })
        .collect()
}

fn classify_catalogue_match(
    identity: &Pcsx2GameIdentity,
    game: &GameHackingGame,
) -> Option<GameHackingMatchStrength> {
    let local_serial = identity.serial.as_deref().and_then(normalize_ps2_serial);
    let remote_serial = game.serial.as_deref().and_then(normalize_ps2_serial);
    let serial_matches = local_serial.is_some() && local_serial == remote_serial;
    let local_crc = identity.verified_crc();
    let remote_crc = game.crc.as_deref().and_then(normalize_crc);
    let crc_matches = local_crc.is_some() && local_crc == remote_crc.as_deref();
    if serial_matches && crc_matches {
        return Some(GameHackingMatchStrength::ExactSerialAndCrc);
    }
    let regions_match = identity
        .region
        .as_deref()
        .zip(game.region.as_deref())
        .is_some_and(|(local, remote)| region_family(local) == region_family(remote));
    if serial_matches && regions_match {
        return Some(GameHackingMatchStrength::ExactSerialAndRegion);
    }
    if crc_matches {
        return Some(GameHackingMatchStrength::ExactCrc);
    }
    (normalized_title(&identity.title) == normalized_title(&game.title))
        .then_some(GameHackingMatchStrength::NormalizedTitle)
}

pub fn normalize_ps2_serial(value: &str) -> Option<String> {
    let normalized = normalize_identity_token(value);
    if normalized.len() != 9 {
        return None;
    }
    let (prefix, digits) = normalized.split_at(4);
    (prefix
        .chars()
        .all(|character| character.is_ascii_alphabetic())
        && digits.chars().all(|character| character.is_ascii_digit()))
    .then_some(normalized)
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

fn decode_provider_text<'a>(bytes: &'a [u8], charset: Option<&str>) -> std::borrow::Cow<'a, str> {
    if let Some(encoding) =
        charset.and_then(|label| encoding_rs::Encoding::for_label(label.trim().as_bytes()))
    {
        return encoding.decode(bytes).0;
    }
    String::from_utf8_lossy(bytes)
}

fn parse_ps2_index_page_numbers(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<u32>, GameHackingError> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let selector = Selector::parse("a[href^='/system/ps2/all/']").expect("static selector");
    let mut pages = BTreeSet::new();
    for node in document.select(&selector) {
        if let Some(page) = node
            .value()
            .attr("href")
            .and_then(|href| href.trim_end_matches('/').rsplit('/').next())
            .and_then(|page| page.parse::<u32>().ok())
        {
            pages.insert(page);
        }
    }
    if pages.is_empty() {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PS2 root index contained no numbered pages",
        ));
    }
    let pages = pages.into_iter().collect::<Vec<_>>();
    let expected_len = u32::try_from(pages.len()).map_err(|_| {
        error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PS2 index page count is invalid",
        )
    })?;
    if pages.first() != Some(&0) || pages.iter().copied().ne(0..expected_len) {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PS2 index pagination is incomplete",
        ));
    }
    Ok(pages)
}

pub fn parse_gamehacking_ps2_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
) -> Result<Vec<GameHackingIndexRecord>, GameHackingError> {
    parse_ps2_index_page(source_url, retrieved_at_unix_seconds, bytes, None)
}

fn parse_ps2_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<GameHackingIndexRecord>, GameHackingError> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let row_selector = Selector::parse("tr").expect("static selector");
    let cell_selector = Selector::parse("th, td").expect("static selector");
    let game_selector = Selector::parse("a[href^='/game/']").expect("static selector");
    let mut current_title = None::<String>;
    let mut games = BTreeMap::<u64, GameHackingIndexRecord>::new();
    for row in document.select(&row_selector) {
        let cells = row
            .select(&cell_selector)
            .map(|cell| {
                cell.text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|cell| !cell.is_empty())
            .collect::<Vec<_>>();
        let game_link = row.select(&game_selector).find_map(|node| {
            let href = node.value().attr("href")?;
            let id = href
                .trim_start_matches("/game/")
                .split('/')
                .next()?
                .parse::<u64>()
                .ok()?;
            let label = node
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            Some((id, href.to_string(), label))
        });
        let Some((game_id, href, link_label)) = game_link else {
            if cells.len() == 1
                && !cells[0].eq_ignore_ascii_case("Version")
                && !cells[0].contains("Number of Codes")
            {
                current_title = Some(cells[0].clone());
            }
            continue;
        };
        let Some(title) = current_title.clone() else {
            continue;
        };
        let serial = cells
            .iter()
            .find(|cell| normalize_ps2_serial(cell).is_some())
            .cloned();
        let crc = cells.iter().find_map(|cell| normalize_crc(cell));
        let region = (!link_label.is_empty()).then_some(link_label);
        let source = if href.starts_with("https://") {
            href
        } else {
            format!("{BASE_URL}{href}")
        };
        games.entry(game_id).or_insert(GameHackingIndexRecord {
            game_id,
            title,
            serial,
            region,
            crc,
            source_url: source,
            index_source_url: source_url.to_string(),
            retrieved_at_unix_seconds,
        });
    }
    if games.is_empty() {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org PS2 index page contained no game rows: {source_url}"),
        ));
    }
    Ok(games.into_values().collect())
}

pub fn parse_gamehacking_game_page(
    game_id: u64,
    source_url: &str,
    bytes: &[u8],
) -> Result<GameHackingGame, GameHackingError> {
    parse_gamehacking_game_page_with_charset(game_id, source_url, bytes, None)
}

fn parse_gamehacking_game_page_with_charset(
    game_id: u64,
    source_url: &str,
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<GameHackingGame, GameHackingError> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
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

pub fn load_ps2_catalogue(root: &Path) -> Result<GameHackingPs2Catalogue, GameHackingError> {
    let path = root.join(PS2_CATALOGUE_FILE);
    let bytes = bounded_read(&path, 32 * 1024 * 1024).map_err(|failure| {
        error(
            failure.kind,
            format!(
                "GameHacking.org PS2 catalogue is unavailable; run `archivefs-cli gamehacking-ps2-index-refresh` first: {}",
                failure.detail
            ),
        )
    })?;
    let catalogue: GameHackingPs2Catalogue = serde_json::from_slice(&bytes).map_err(|failure| {
        error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org PS2 catalogue is invalid: {failure}"),
        )
    })?;
    if catalogue.schema_version != PS2_CATALOGUE_SCHEMA_VERSION
        || catalogue.provider != GAMEHACKING_PROVIDER_ID
        || !catalogue.system.eq_ignore_ascii_case("PlayStation 2")
    {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org PS2 catalogue metadata is unsupported",
        ));
    }
    Ok(catalogue)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn retrieved_cache_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.retrieved",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("cache")
    ))
}

fn cache_retrieved_at(path: &Path) -> Result<u64, GameHackingError> {
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
            error(
                GameHackingErrorKind::CacheUnavailable,
                format!("cached retrieval date is unavailable: {}", path.display()),
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

fn charset_cache_path(path: &Path) -> PathBuf {
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
        error(
            GameHackingErrorKind::CacheUnavailable,
            "cached provider charset metadata is invalid",
        )
    })?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| value.to_string()))
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
    fn numbered_index_pages_are_sorted_and_deduplicated() {
        let pages = parse_ps2_index_page_numbers(
            br#"<a href="/system/ps2/all/2">two</a>
                <a href="/system/ps2/all/0">zero</a>
                <a href="/system/ps2/all/1">one</a>
                <a href="/system/ps2/all/2">duplicate</a>"#,
            None,
        )
        .unwrap();
        assert_eq!(pages, vec![0, 1, 2]);
    }

    #[test]
    fn index_page_collects_title_serial_region_crc_and_source() {
        let html = br#"<table><tbody>
            <tr><td colspan="5">Example Game</td></tr>
            <tr><td><a href="/game/42">(PAL-M5)</a></td><td>SLES_546.58</td><td>0K</td><td>A1B2C3D4</td><td>12</td></tr>
            <tr><td><a href="/game/43">(NTSC-U)</a></td><td>SLUS-99999</td><td>0K</td><td></td><td>8</td></tr>
        </tbody></table>"#;
        let records =
            parse_gamehacking_ps2_index_page("https://gamehacking.org/system/ps2/all/7", 123, html)
                .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].title, "Example Game");
        assert_eq!(records[0].serial.as_deref(), Some("SLES_546.58"));
        assert_eq!(records[0].region.as_deref(), Some("(PAL-M5)"));
        assert_eq!(records[0].crc.as_deref(), Some("A1B2C3D4"));
        assert_eq!(records[0].source_url, "https://gamehacking.org/game/42");
        assert_eq!(records[0].retrieved_at_unix_seconds, 123);
        assert_eq!(records[1].crc, None);
    }

    #[test]
    fn http_charset_parameter_is_read_case_insensitively() {
        assert_eq!(
            charset_from_content_type("text/html; Charset=Windows-1252"),
            Some("windows-1252".to_string())
        );
        assert_eq!(charset_from_content_type("text/html"), None);
    }

    #[test]
    fn windows_1252_index_page_with_invalid_utf8_still_parses() {
        let mut html = br#"<table><tbody><tr><td colspan="5">Example "#.to_vec();
        html.push(0x96);
        html.extend_from_slice(
            br#" Game</td></tr><tr><td><a href="/game/77">(PAL)</a></td><td>SLES-54658</td><td>0K</td><td></td><td>2</td></tr></tbody></table>"#,
        );
        assert!(std::str::from_utf8(&html).is_err());
        let records = parse_ps2_index_page(
            "https://gamehacking.org/system/ps2/all/0",
            123,
            &html,
            Some("windows-1252"),
        )
        .unwrap();
        assert_eq!(records[0].title, "Example – Game");
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
    fn serial_variants_normalize_and_matching_uses_declared_priority() {
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
        assert_eq!(
            normalize_ps2_serial("SLES-54658").as_deref(),
            Some("SLES54658")
        );
        assert_eq!(
            normalize_ps2_serial("SLES_546.58").as_deref(),
            Some("SLES54658")
        );
        assert_eq!(
            normalize_ps2_serial("SLES54658").as_deref(),
            Some("SLES54658")
        );
        assert_eq!(
            classify_catalogue_match(&identity, &game()),
            Some(GameHackingMatchStrength::ExactSerialAndCrc)
        );
        let mut crc_only = game();
        crc_only.serial = Some("SLES-99999".to_string());
        assert_eq!(
            classify_catalogue_match(&identity, &crc_only),
            Some(GameHackingMatchStrength::ExactCrc)
        );
        let mut serial_and_region = game();
        serial_and_region.serial = Some("SLUS_123.45".to_string());
        serial_and_region.crc = Some("FFFFFFFF".to_string());
        assert_eq!(
            classify_catalogue_match(&identity, &serial_and_region),
            Some(GameHackingMatchStrength::ExactSerialAndRegion)
        );
        let mut title_only = game();
        title_only.serial = Some("SLES-99999".to_string());
        title_only.crc = Some("FFFFFFFF".to_string());
        title_only.region = Some("PAL".to_string());
        assert_eq!(
            classify_catalogue_match(&identity, &title_only),
            Some(GameHackingMatchStrength::NormalizedTitle)
        );
    }

    #[test]
    fn catalogue_matching_ranks_exact_identity_and_gates_titles() {
        use crate::patch_manager::Pcsx2IdentityState;

        let identity = Pcsx2GameIdentity {
            archive_path: PathBuf::from("/games/example.iso"),
            title: "Example Game".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS_123.45".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
            state: Pcsx2IdentityState::Verified,
            evidence: Vec::new(),
            plain_failure_reason: None,
        };
        let record = |game: GameHackingGame| GameHackingIndexRecord {
            game_id: game.game_id,
            title: game.title,
            serial: game.serial,
            region: game.region,
            crc: game.crc,
            source_url: game.source_url,
            index_source_url: PS2_INDEX_URL.to_string(),
            retrieved_at_unix_seconds: 123,
        };
        let mut title_only = game();
        title_only.game_id = 43;
        title_only.serial = Some("SLES-99999".to_string());
        title_only.crc = Some("FFFFFFFF".to_string());
        let catalogue = GameHackingPs2Catalogue {
            schema_version: PS2_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_PROVIDER_ID.to_string(),
            system: "PlayStation 2".to_string(),
            source_url: PS2_INDEX_URL.to_string(),
            retrieved_at_unix_seconds: 123,
            pages: Vec::new(),
            games: vec![record(title_only.clone()), record(game())],
        };
        let matches = match_ps2_catalogue(&identity, &catalogue);
        assert!(matches.iter().any(|candidate| {
            candidate.game.game_id == 42
                && candidate.strength == GameHackingMatchStrength::ExactSerialAndCrc
        }));
        assert_eq!(
            authorize_catalogue_match(&identity, &title_only, false)
                .unwrap_err()
                .kind,
            GameHackingErrorKind::IdentityConflict
        );
        assert_eq!(
            authorize_catalogue_match(&identity, &title_only, true).unwrap(),
            GameHackingMatchStrength::NormalizedTitle
        );
    }

    #[test]
    fn cached_index_pages_resume_without_network_and_json_is_deterministic() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamehacking-index-{}-{}",
            std::process::id(),
            unix_seconds_now()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("robots.txt"), b"User-agent: *\nDisallow:\n").unwrap();
        fs::write(
            root.join(PS2_INDEX_ROOT_CACHE_FILE),
            br#"<a href="/system/ps2/all/0">zero</a><a href="/system/ps2/all/1">one</a>
                <table><tbody><tr><td colspan="5">Game 0</td></tr><tr><td><a href="/game/100">(PAL)</a></td><td>SLES-54650</td><td>0K</td><td></td><td>2</td></tr></tbody></table>"#,
        )
        .unwrap();
        for page in 0..2 {
            fs::write(
                root.join(format!("ps2-index-page-{page}.html")),
                format!(
                    "<table><tbody><tr><td colspan=\"5\">Game {page}</td></tr><tr><td><a href=\"/game/{}\">(PAL)</a></td><td>SLES-5465{page}</td><td>0K</td><td></td><td>2</td></tr></tbody></table>",
                    100 + page
                ),
            )
            .unwrap();
        }
        let options = GameHackingFetchOptions {
            cache_root: root.clone(),
            force_refresh: false,
            delay: Duration::ZERO,
            cancellation: None,
        };
        let provider = GameHackingProvider::default();
        let first = provider.refresh_ps2_index(&options, |_| {}).unwrap();
        assert_eq!(first.pages_downloaded, 0);
        assert_eq!(first.pages_reused, 2);
        assert_eq!(first.games, 2);
        let first_json = fs::read(&first.catalogue_path).unwrap();
        let second = provider.refresh_ps2_index(&options, |_| {}).unwrap();
        assert_eq!(first_json, fs::read(&second.catalogue_path).unwrap());
        let _ = fs::remove_dir_all(root);
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
