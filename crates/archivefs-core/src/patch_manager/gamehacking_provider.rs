//! Cached PS2 catalogue and per-library-game access to GameHacking.org.
//!
//! Retrieval, HTML parsing, identity matching, PNACH parsing, and installation
//! remain separate. The explicit index command walks only the numbered public
//! PS2 table pages. Runtime matching is local, and only one selected game's
//! PNACH is requested after an automatic match or user confirmation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::gamehacking_catalogue::ordered_contiguous_page_numbers;
use super::gamehacking_catalogue::{
    GameHackingCatalogueCrawler, GameHackingCatalogueHooks, GameHackingCatalogueMetadata,
    GameHackingCataloguePageMetadata, GameHackingCatalogueSpec,
};
use super::gamehacking_shared::{
    BASE_URL, EXPORT_URL, GameHackingClient, GameHackingRequestOptions, GameHackingRequestSpec,
    ProviderResponse, UreqGameHackingTransport, bounded_read, decode_provider_text, provider_error,
};
pub use super::gamehacking_shared::{
    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE, GameHackingError, GameHackingErrorKind,
    GameHackingFetchOutcome,
};
#[cfg(test)]
use super::gamehacking_shared::{
    GameHackingHttpClassification, cancellable_delay, charset_from_content_type,
    classify_gamehacking_http_response, classify_gamehacking_transport_error,
    clear_cloudflare_marker, cloudflare_cooldown_remaining, mark_cloudflare_blocked,
    retrieved_cache_path, robots_disallows_archivefs, unix_seconds_now, validate_provider_url,
};

use super::pcsx2::normalize_crc;
use super::pcsx2_identity::Pcsx2GameIdentity;
use super::pcsx2_provider::{
    Pcsx2CheatCategory, Pcsx2CheatConfidence, Pcsx2CheatProviderCatalogue,
    Pcsx2CheatProviderRecord, Pcsx2ProviderTrust,
};

pub const GAMEHACKING_PROVIDER_ID: &str = "gamehacking.org";
const PS2_INDEX_URL: &str = "https://gamehacking.org/system/ps2/all";
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EXPORT_BYTES: usize = 2 * 1024 * 1024;
const PS2_CATALOGUE_SCHEMA_VERSION: u32 = 1;
const PS2_CATALOGUE_FILE: &str = "ps2-catalogue.json";
const PS2_INDEX_ROOT_CACHE_FILE: &str = "ps2-index-root.html";
const LEGACY_PS2_INDEX_CACHE_FILE: &str = "ps2-index.html";
const MAX_PS2_INDEX_PAGES: usize = 512;

fn error(kind: GameHackingErrorKind, detail: impl Into<String>) -> GameHackingError {
    provider_error(kind, detail)
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
    pub cached_fallback: bool,
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

impl GameHackingRequestOptions for GameHackingFetchOptions {
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

pub fn gamehacking_cache_root() -> Result<PathBuf, GameHackingError> {
    let database = crate::default_database_path().map_err(|failure| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            format!("EmuWiz data directory is unavailable: {failure}"),
        )
    })?;
    let parent = database.parent().ok_or_else(|| {
        error(
            GameHackingErrorKind::CacheUnavailable,
            "EmuWiz database path has no parent directory",
        )
    })?;
    Ok(parent.join("cache/gamehacking"))
}

pub struct GameHackingProvider<A = Ps2GameHackingAdapter> {
    adapter: A,
    client: GameHackingClient,
}

struct Ps2CatalogueHooks;

impl GameHackingCatalogueHooks for Ps2CatalogueHooks {
    type Record = GameHackingIndexRecord;
    type Page = GameHackingIndexPage;
    type Catalogue = GameHackingPs2Catalogue;

    fn discover_page_numbers(
        &self,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<u32>, GameHackingError> {
        Ok(discover_ps2_index_page_numbers(bytes, charset))
    }

    fn parse_page(
        &self,
        source_url: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<Self::Record>, GameHackingError> {
        parse_ps2_index_page(source_url, retrieved_at_unix_seconds, bytes, charset)
    }

    fn record_id(&self, record: &Self::Record) -> u64 {
        record.game_id
    }

    fn record_title<'a>(&self, record: &'a Self::Record) -> &'a str {
        &record.title
    }

    fn make_page(&self, metadata: GameHackingCataloguePageMetadata) -> Self::Page {
        GameHackingIndexPage {
            page_number: metadata.page_number,
            source_url: metadata.source_url,
            retrieved_at_unix_seconds: metadata.retrieved_at_unix_seconds,
            sha256: metadata.sha256,
            game_count: metadata.game_count,
        }
    }

    fn make_catalogue(
        &self,
        metadata: GameHackingCatalogueMetadata<'_>,
        pages: Vec<Self::Page>,
        games: Vec<Self::Record>,
    ) -> Self::Catalogue {
        GameHackingPs2Catalogue {
            schema_version: metadata.schema_version,
            provider: metadata.provider.to_string(),
            system: metadata.system.to_string(),
            source_url: metadata.source_url.to_string(),
            retrieved_at_unix_seconds: metadata.retrieved_at_unix_seconds,
            pages,
            games,
        }
    }
}

impl Default for GameHackingProvider<Ps2GameHackingAdapter> {
    fn default() -> Self {
        Self {
            adapter: Ps2GameHackingAdapter,
            client: GameHackingClient::default(),
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
        let root_cache_files = [PS2_INDEX_ROOT_CACHE_FILE, LEGACY_PS2_INDEX_CACHE_FILE];
        let spec = GameHackingCatalogueSpec {
            schema_version: PS2_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_PROVIDER_ID,
            system: self.adapter.system_name(),
            index_url: self.adapter.index_url(),
            robots_path: "/system/ps2/all",
            root_cache_files: &root_cache_files,
            page_cache_prefix: "ps2-index-page-",
            page_cache_suffix: ".html",
            catalogue_cache_file: PS2_CATALOGUE_FILE,
            maximum_index_bytes: MAX_INDEX_BYTES,
            maximum_pages: MAX_PS2_INDEX_PAGES,
            insert_root_page_zero: false,
            no_pages_error: "GameHacking.org PS2 root index contained no numbered pages",
            page_count_error: "GameHacking.org PS2 index page count is invalid",
            incomplete_pagination_error: "GameHacking.org PS2 index pagination is incomplete",
            page_limit_error: "GameHacking.org PS2 index exceeded the page limit",
        };
        let result = GameHackingCatalogueCrawler::new(&self.client).crawl(
            &spec,
            options,
            &Ps2CatalogueHooks,
            |transport, url, maximum_bytes| transport.get(url, maximum_bytes),
            |event| {
                progress(GameHackingIndexProgress {
                    pages_complete: event.pages_complete,
                    pages_total: event.pages_total,
                    page_number: event.page_number,
                    downloaded: event.downloaded,
                    games_collected: event.games_collected,
                });
            },
        )?;
        Ok(GameHackingIndexRefreshResult {
            catalogue_path: result.catalogue_path,
            pages_total: result.pages_total,
            pages_downloaded: result.pages_downloaded,
            pages_reused: result.pages_reused,
            games: result.games,
            retrieved_at_unix_seconds: result.retrieved_at_unix_seconds,
            cached_fallback: result.cached_fallback,
        })
    }

    pub fn fetch_cheats(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        self.fetch_cheats_with_status(identity, game, options)
            .map(|outcome| outcome.data)
    }

    pub fn fetch_cheats_with_status(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingCheat>>, GameHackingError> {
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
        Ok(GameHackingFetchOutcome {
            data: parse_gamehacking_pnach(game, &bytes.bytes)?,
            cached_fallback: bytes.cached_fallback,
            retrieved_at_unix_seconds: bytes.retrieved_at_unix_seconds,
        })
    }

    pub fn fetch_cheats_for_confirmed_candidate(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<Vec<GameHackingCheat>, GameHackingError> {
        self.fetch_cheats_for_confirmed_candidate_with_status(identity, game, options)
            .map(|outcome| outcome.data)
    }

    pub fn fetch_cheats_for_confirmed_candidate_with_status(
        &self,
        identity: &Pcsx2GameIdentity,
        game: &GameHackingGame,
        options: &GameHackingFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingCheat>>, GameHackingError> {
        authorize_catalogue_match(identity, game, true)?;
        self.fetch_export(game, identity, options)
    }

    fn fetch_export(
        &self,
        game: &GameHackingGame,
        identity: &Pcsx2GameIdentity,
        options: &GameHackingFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingCheat>>, GameHackingError> {
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
        Ok(GameHackingFetchOutcome {
            data: parse_gamehacking_pnach(game, &bytes.bytes)?,
            cached_fallback: bytes.cached_fallback,
            retrieved_at_unix_seconds: bytes.retrieved_at_unix_seconds,
        })
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
        url: &str,
        maximum_bytes: usize,
        options: &GameHackingFetchOptions,
        request: F,
    ) -> Result<ProviderResponse, GameHackingError>
    where
        F: Fn(&UreqGameHackingTransport) -> Result<ProviderResponse, GameHackingError>,
    {
        self.client.cached_request(
            GameHackingRequestSpec {
                cache_file: file_name,
                url,
                maximum_bytes,
            },
            options,
            request,
        )
    }

    fn check_robots(
        &self,
        options: &GameHackingFetchOptions,
        paths: &[&str],
    ) -> Result<(), GameHackingError> {
        self.client.check_robots(options, paths)
    }
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

#[cfg(test)]
fn parse_ps2_index_page_numbers(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<u32>, GameHackingError> {
    ordered_contiguous_page_numbers(
        discover_ps2_index_page_numbers(bytes, charset),
        false,
        "GameHacking.org PS2 root index contained no numbered pages",
        "GameHacking.org PS2 index page count is invalid",
        "GameHacking.org PS2 index pagination is incomplete",
    )
}

fn discover_ps2_index_page_numbers(bytes: &[u8], charset: Option<&str>) -> Vec<u32> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let selector = Selector::parse("a[href^='/system/ps2/all/']").expect("static selector");
    let mut pages = Vec::new();
    for node in document.select(&selector) {
        if let Some(page) = node
            .value()
            .attr("href")
            .and_then(|href| href.trim_end_matches('/').rsplit('/').next())
            .and_then(|page| page.parse::<u32>().ok())
        {
            pages.push(page);
        }
    }
    pages
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
    let mut pending = PendingPnachCheat::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if let Some(section) = pnach_section_title(trimmed) {
            flush_pending_pnach(game, &mut pending, &mut cheats);
            pending.name = Some(section);
            continue;
        }
        if let Some(comment) = line.trim_start().strip_prefix("//") {
            if !pending.patch_lines.is_empty() {
                flush_pending_pnach(game, &mut pending, &mut cheats);
            }
            pending.comments.push(comment.trim().to_string());
            continue;
        }
        if let Some(value) = strip_assignment(trimmed, "author") {
            pending.author = nonempty_decoded(value);
            pending.reading_description = false;
            continue;
        }
        if let Some(value) = strip_assignment(trimmed, "description") {
            if let Some(value) = nonempty_decoded(value) {
                pending.description.push(value);
            }
            pending.reading_description = true;
            continue;
        }
        if let Some(value) =
            strip_assignment(trimmed, "note").or_else(|| strip_assignment(trimmed, "notes"))
        {
            if let Some(value) = nonempty_decoded(value) {
                pending.description.push(value);
            }
            pending.reading_description = true;
            continue;
        }
        if line.trim_start().starts_with("patch=") {
            pending.reading_description = false;
            pending.patch_lines.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if !pending.patch_lines.is_empty() {
                flush_pending_pnach(game, &mut pending, &mut cheats);
            }
            continue;
        }
        if pending.reading_description && pending.patch_lines.is_empty() {
            pending.description.push(decode_html_text(trimmed));
        }
    }
    flush_pending_pnach(game, &mut pending, &mut cheats);
    if cheats.is_empty() {
        return Err(error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org export contained no supported PCSX2 patch lines",
        ));
    }
    Ok(cheats)
}

#[derive(Debug, Default)]
struct PendingPnachCheat {
    name: Option<String>,
    author: Option<String>,
    description: Vec<String>,
    comments: Vec<String>,
    patch_lines: Vec<String>,
    reading_description: bool,
}

fn flush_pending_pnach(
    game: &GameHackingGame,
    pending: &mut PendingPnachCheat,
    cheats: &mut Vec<GameHackingCheat>,
) {
    if pending.patch_lines.is_empty() {
        *pending = PendingPnachCheat::default();
        return;
    }
    for comment in std::mem::take(&mut pending.comments) {
        let trimmed = comment.trim();
        if let Some(value) = strip_label(trimmed, "author") {
            pending.author = nonempty_decoded(value);
        } else if let Some(value) = strip_label(trimmed, "description")
            .or_else(|| strip_label(trimmed, "note"))
            .or_else(|| strip_label(trimmed, "notes"))
        {
            if let Some(value) = nonempty_decoded(value) {
                pending.description.push(value);
            }
        } else if pending.name.is_none() && trustworthy_cheat_comment(game, trimmed) {
            pending.name = Some(decode_html_text(trimmed));
        } else if !trimmed.is_empty() && !is_generated_pnach_comment(game, trimmed) {
            pending.description.push(decode_html_text(trimmed));
        }
    }
    let index = cheats.len() + 1;
    let name = pending
        .name
        .take()
        .unwrap_or_else(|| format!("Cheat {index}"));
    cheats.push(GameHackingCheat {
        id: format!("gh-{}-{index}", game.game_id),
        name,
        author: pending.author.take(),
        description: normalized_description(std::mem::take(&mut pending.description)),
        patch_lines: std::mem::take(&mut pending.patch_lines),
        source_game_id: game.game_id,
        source_url: game.source_url.clone(),
    });
    *pending = PendingPnachCheat::default();
}

fn pnach_section_title(line: &str) -> Option<String> {
    let title = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    if title.is_empty() {
        return None;
    }
    Some(
        title
            .split('\\')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(decode_html_text)
            .collect::<Vec<_>>()
            .join(" › "),
    )
}

fn strip_assignment<'a>(value: &'a str, label: &str) -> Option<&'a str> {
    let (head, tail) = value.split_once('=')?;
    head.trim()
        .eq_ignore_ascii_case(label)
        .then_some(tail.trim())
}

fn nonempty_decoded(value: &str) -> Option<String> {
    let value = decode_html_text(value);
    (!value.trim().is_empty()).then(|| value.trim().to_string())
}

fn decode_html_text(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let fragment = Html::parse_fragment(value);
    fragment.root_element().text().collect::<String>()
}

fn normalized_description(lines: Vec<String>) -> Option<String> {
    let mut normalized = Vec::new();
    for line in lines {
        let line = line.trim();
        if !line.is_empty() {
            normalized.push(line.to_string());
        }
    }
    (!normalized.is_empty()).then(|| normalized.join("\n"))
}

fn trustworthy_cheat_comment(game: &GameHackingGame, comment: &str) -> bool {
    !comment.is_empty() && !is_generated_pnach_comment(game, comment)
}

fn is_generated_pnach_comment(game: &GameHackingGame, comment: &str) -> bool {
    let comment_lower = comment.to_ascii_lowercase();
    let title_lower = game.title.to_ascii_lowercase();
    comment_lower == title_lower
        || comment_lower.starts_with(&format!("{title_lower} ("))
        || comment
            .to_ascii_lowercase()
            .starts_with("file generated by gamehacking.org")
}

fn strip_label<'a>(value: &'a str, label: &str) -> Option<&'a str> {
    let (head, tail) = value.split_once(':')?;
    head.trim()
        .eq_ignore_ascii_case(label)
        .then_some(tail.trim())
        .filter(|tail| !tail.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps2_cache_paths_and_sidecars_remain_compatible() {
        let root = Path::new("/tmp/archivefs-cache-contract");
        assert_eq!(
            root.join(PS2_CATALOGUE_FILE),
            root.join("ps2-catalogue.json")
        );
        assert_eq!(
            root.join(PS2_INDEX_ROOT_CACHE_FILE),
            root.join("ps2-index-root.html")
        );
        assert_eq!(
            root.join(LEGACY_PS2_INDEX_CACHE_FILE),
            root.join("ps2-index.html")
        );
        assert_eq!(
            root.join(format!("ps2-index-page-{}.html", 37)),
            root.join("ps2-index-page-37.html")
        );
        let export = root.join(format!("export-{}.pnach", 42));
        assert_eq!(export, root.join("export-42.pnach"));
        assert_eq!(
            super::super::gamehacking_shared::charset_cache_path(&export),
            root.join("export-42.pnach.charset")
        );
        assert_eq!(
            retrieved_cache_path(&export),
            root.join("export-42.pnach.retrieved")
        );
    }

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
    fn native_section_headers_keep_multiple_real_cheat_names_and_notes() {
        let source = include_bytes!("../../tests/fixtures/gamehacking/named-export.pnach");
        let cheats = parse_gamehacking_pnach(&game(), source).unwrap();
        assert_eq!(cheats.len(), 3);
        assert_eq!(cheats[0].name, "Player Codes › Infinite Health");
        assert_eq!(cheats[0].author.as_deref(), Some("Ada"));
        assert_eq!(
            cheats[0].description.as_deref(),
            Some("Health never decreases.")
        );
        assert_eq!(
            cheats[1].name,
            "Inventory › Unlock All Items [Save + Reload]"
        );
        assert_eq!(cheats[1].author.as_deref(), Some("Grace"));
        assert_eq!(
            cheats[1].description.as_deref(),
            Some(
                "Open the inventory after reloading.\nThis second line is retained as part of the notes."
            )
        );
        assert_eq!(cheats[1].patch_lines.len(), 1);
        assert_eq!(cheats[2].name, "Camera follows arrow [hold select]");
        assert_eq!(
            cheats[2].description.as_deref(),
            Some("It's useful for exploring.")
        );
        assert!(cheats.iter().all(|cheat| !cheat.name.starts_with("Cheat ")));
    }

    #[test]
    fn numbered_name_is_used_only_when_export_has_no_trustworthy_title() {
        let cheats =
            parse_gamehacking_pnach(&game(), b"patch=1,EE,20123456,word,00000001\n").unwrap();
        assert_eq!(cheats[0].name, "Cheat 1");
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
        let mut progress = Vec::new();
        let first = provider
            .refresh_ps2_index(&options, |event| progress.push(event))
            .unwrap();
        assert_eq!(first.pages_downloaded, 0);
        assert_eq!(first.pages_reused, 2);
        assert_eq!(first.games, 2);
        assert_eq!(
            progress,
            vec![
                GameHackingIndexProgress {
                    pages_complete: 1,
                    pages_total: 2,
                    page_number: Some(0),
                    downloaded: false,
                    games_collected: 1,
                },
                GameHackingIndexProgress {
                    pages_complete: 2,
                    pages_total: 2,
                    page_number: Some(1),
                    downloaded: false,
                    games_collected: 2,
                },
            ]
        );
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

    #[test]
    fn a_403_fronted_by_cloudflares_server_header_is_classified_as_blocked() {
        assert_eq!(
            classify_gamehacking_http_response(403, Some("cloudflare"), b""),
            GameHackingHttpClassification::CloudflareBlocked
        );
    }

    #[test]
    fn an_ordinary_403_without_any_cloudflare_signal_is_access_denied_not_blocked() {
        assert_eq!(
            classify_gamehacking_http_response(403, None, b"Forbidden"),
            GameHackingHttpClassification::AccessDenied
        );
        assert_eq!(
            classify_gamehacking_http_response(403, Some("nginx"), b"Forbidden"),
            GameHackingHttpClassification::AccessDenied
        );
    }

    #[test]
    fn a_cloudflare_challenge_page_served_with_an_ordinary_200_is_still_classified_as_blocked() {
        let challenge_body = b"<html><head><title>Just a moment...</title></head><body>Enable JavaScript and cookies to continue<div>Cloudflare Ray ID: 89abc123</div></body></html>";
        assert_eq!(
            classify_gamehacking_http_response(200, None, challenge_body),
            GameHackingHttpClassification::CloudflareBlocked
        );
    }

    #[test]
    fn ordinary_200_content_is_never_misclassified_as_a_cloudflare_challenge() {
        assert_eq!(
            classify_gamehacking_http_response(
                200,
                None,
                b"<html><body>Real content</body></html>"
            ),
            GameHackingHttpClassification::Success
        );
    }

    #[test]
    fn rate_limit_and_server_failures_remain_distinct_from_cloudflare() {
        assert_eq!(
            classify_gamehacking_http_response(429, None, b"slow down"),
            GameHackingHttpClassification::RateLimited
        );
        assert_eq!(
            classify_gamehacking_http_response(500, None, b"origin error"),
            GameHackingHttpClassification::ServerError
        );
    }

    #[test]
    fn dns_failure_is_a_distinct_network_failure() {
        let failure = classify_gamehacking_transport_error(ureq::Error::HostNotFound);
        assert_eq!(failure.kind, GameHackingErrorKind::NetworkFailure);
        assert!(!failure.detail.contains("HTTP 500"));
    }

    #[test]
    fn blocked_refresh_uses_the_exact_ps2_cache_without_rewriting_metadata() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamehacking-ps2-fallback-{}-{}",
            std::process::id(),
            unix_seconds_now()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("export-42.pnach");
        let original =
            b"gametitle=Fixture\ncomment=Real cached export\npatch=1,EE,20123456,word,00000001\n";
        fs::write(&path, original).unwrap();
        fs::write(retrieved_cache_path(&path), b"123").unwrap();
        let provider = GameHackingProvider::default();
        let options = GameHackingFetchOptions {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        let response = provider
            .cached_request(
                "export-42.pnach",
                EXPORT_URL,
                MAX_EXPORT_BYTES,
                &options,
                |_| {
                    Err(error(
                        GameHackingErrorKind::CloudflareBlocked,
                        GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                    ))
                },
            )
            .unwrap();
        assert!(response.cached_fallback);
        assert_eq!(response.retrieved_at_unix_seconds, Some(123));
        assert_eq!(response.bytes, original);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(retrieved_cache_path(&path)).unwrap(), b"123");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cloudflare_cooldown_marker_gates_and_then_clears() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamehacking-cloudflare-cooldown-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(cloudflare_cooldown_remaining(&root).is_none());
        mark_cloudflare_blocked(&root).unwrap();
        assert!(cloudflare_cooldown_remaining(&root).is_some());
        clear_cloudflare_marker(&root);
        assert!(cloudflare_cooldown_remaining(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    /// Pins the original bug this module fixes: a Cloudflare-fronted 403
    /// (or a 200 masking a JS challenge) must never surface as a generic
    /// "HTTP 500" / temporarily-unavailable message. A genuine 500, with no
    /// Cloudflare signal at all, must still say exactly that.
    #[test]
    fn a_cloudflare_response_never_produces_a_misleading_500_style_message() {
        let blocked = classify_gamehacking_http_response(403, Some("cloudflare"), b"");
        assert_eq!(blocked, GameHackingHttpClassification::CloudflareBlocked);
        assert_ne!(blocked, GameHackingHttpClassification::ServerError);

        let challenge_body = b"<html><head><title>Just a moment...</title></head><body>Cloudflare Ray ID: 1</body></html>";
        let challenge = classify_gamehacking_http_response(200, None, challenge_body);
        assert_eq!(challenge, GameHackingHttpClassification::CloudflareBlocked);
        assert_ne!(challenge, GameHackingHttpClassification::ServerError);

        let genuine_server_error = classify_gamehacking_http_response(500, None, b"");
        assert_eq!(
            genuine_server_error,
            GameHackingHttpClassification::ServerError,
            "an actual 500 with no Cloudflare signal must still be reported as a server error"
        );
    }
}
