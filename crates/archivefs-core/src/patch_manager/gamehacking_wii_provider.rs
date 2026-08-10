//! GameHacking.org Wii catalogue, conservative matching, manual page import,
//! and Dolphin installation adapter.
//!
//! Low-level HTTP, cache classification, cooldown, cancellation and catalogue
//! crawling are deliberately delegated to `gamehacking_shared` and
//! `gamehacking_catalogue`. Wii keeps only its verified URL/slug, HTML hooks,
//! Dolphin identity policy, explicit label policy, and safety checks here.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use scraper::{Element, Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::gamehacking_catalogue::{
    GameHackingCatalogueCrawler, GameHackingCatalogueHooks, GameHackingCatalogueMetadata,
    GameHackingCataloguePageMetadata, GameHackingCatalogueSpec,
};
use super::gamehacking_gamecube_install_plan::{
    GameCubeCheatSelection, GameCubeGameHackingInstallPreview,
    GameCubeGameHackingInstallPreviewRequest, GameCubeInstallPlanError, StagedGameCubeIni,
    build_dolphin_gamehacking_install_preview, stage_gamecube_gamehacking_install,
    stage_gamecube_gamehacking_removal,
};
use super::gamehacking_gamecube_provider::{
    GameCubeCodeFormat, GameHackingGameCubeCheat, GameHackingGameCubeFetchOptions,
};
use super::gamehacking_provider::{GameHackingError, GameHackingErrorKind};
use super::gamehacking_shared::{
    BASE_URL, GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE, GameHackingClient, GameHackingFetchOutcome,
    GameHackingRequestSpec, atomic_write, bounded_read, cached_bytes_are_cloudflare_challenge,
    charset_cache_path, decode_provider_text, provider_error, retrieved_cache_path,
    unix_seconds_now,
};
use super::gecko_document::DolphinIniDocument;
use crate::game_identity::{
    GameIdentityReport, IdentityConfidence, IdentityKind, IdentityPlatform, IdentityStatus,
};

pub const GAMEHACKING_WII_PROVIDER_ID: &str = "gamehacking.org";
pub const WII_INDEX_URL: &str = "https://gamehacking.org/system/wii/all";
const WII_CATALOGUE_SCHEMA_VERSION: u32 = 1;
const WII_CATALOGUE_FILE: &str = "wii-catalogue.json";
const WII_INDEX_ROOT_CACHE_FILE: &str = "wii-index-root.html";
const WII_INDEX_PAGE_PREFIX: &str = "wii-index-page-";
const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_GAME_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_WII_INDEX_PAGES: usize = 512;
const VERIFIED_WII_SYS_ID: u16 = 22;

pub type GameHackingWiiFetchOptions = GameHackingGameCubeFetchOptions;

fn wii_error(kind: GameHackingErrorKind, detail: impl Into<String>) -> GameHackingError {
    provider_error(kind, detail)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiiIdentityState {
    Verified,
    MissingGameId,
    Deferred,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiiGameIdentity {
    pub source_path: PathBuf,
    pub title: String,
    pub dolphin_game_id: Option<String>,
    pub region: Option<String>,
    pub disc_number: Option<u8>,
    /// Reserved for future partition-aware evidence. The current identity
    /// reader intentionally leaves this unset for Wii.
    pub verified_revision: Option<u16>,
    /// Outer-header revision retained as non-authoritative diagnostics only.
    pub candidate_revision: Option<u16>,
    pub state: WiiIdentityState,
    pub evidence: Vec<String>,
    pub plain_failure_reason: Option<String>,
}

impl WiiGameIdentity {
    pub fn from_report(title: impl Into<String>, report: &GameIdentityReport) -> Self {
        let title = title.into();
        let dolphin_game_id = report
            .verified_dolphin_game_id()
            .and_then(normalize_wii_game_id);
        let region = report
            .verified_value(IdentityKind::DolphinRegion)
            .map(str::to_owned);
        let disc_number = report
            .verified_value(IdentityKind::DolphinDiscNumber)
            .and_then(|value| value.parse().ok());
        let candidate_revision = report.evidence.iter().find_map(|item| {
            (item.kind == IdentityKind::DolphinRevision
                && item.status == IdentityStatus::Candidate
                && item.confidence == IdentityConfidence::StructuredMetadata)
                .then_some(item.value.as_deref())
                .flatten()
                .and_then(|value| value.parse().ok())
        });
        let game_id_evidence = report
            .evidence
            .iter()
            .find(|item| item.kind == IdentityKind::DolphinGameId);
        let state = if report.platform != IdentityPlatform::Wii {
            WiiIdentityState::Unsupported
        } else if dolphin_game_id.is_some() {
            WiiIdentityState::Verified
        } else {
            match game_id_evidence.map(|item| item.status) {
                Some(IdentityStatus::Deferred) => WiiIdentityState::Deferred,
                Some(IdentityStatus::Ambiguous | IdentityStatus::ResourceLimitReached) => {
                    WiiIdentityState::Ambiguous
                }
                Some(IdentityStatus::Unsupported | IdentityStatus::Invalid) => {
                    WiiIdentityState::Unsupported
                }
                _ => WiiIdentityState::MissingGameId,
            }
        };
        let plain_failure_reason = match state {
            WiiIdentityState::Verified => None,
            WiiIdentityState::MissingGameId => Some(
                "EmuWiz could not prove the six-character Wii Game ID required for matching."
                    .to_string(),
            ),
            WiiIdentityState::Deferred => {
                Some("Wii identity is not available for this image format yet.".to_string())
            }
            WiiIdentityState::Ambiguous => {
                Some("EmuWiz found ambiguous Wii identity evidence and will not guess.".to_string())
            }
            WiiIdentityState::Unsupported => {
                Some("This selection is not a supported Wii disc image.".to_string())
            }
        };
        let evidence = report
            .evidence
            .iter()
            .filter(|item| {
                matches!(
                    item.kind,
                    IdentityKind::DolphinGameId
                        | IdentityKind::DolphinRegion
                        | IdentityKind::DolphinDiscNumber
                        | IdentityKind::DolphinRevision
                )
            })
            .map(|item| {
                format!(
                    "{}: {} ({:?}; {}; {})",
                    item.kind,
                    item.status,
                    item.confidence,
                    item.provenance.method,
                    item.diagnostic
                )
            })
            .collect();
        Self {
            source_path: report.archive_path.clone(),
            title,
            dolphin_game_id,
            region,
            disc_number,
            verified_revision: report.verified_dolphin_revision(),
            candidate_revision,
            state,
            evidence,
            plain_failure_reason,
        }
    }

    pub fn verified_game_id(&self) -> Option<&str> {
        (self.state == WiiIdentityState::Verified)
            .then_some(self.dolphin_game_id.as_deref())
            .flatten()
    }
}

pub fn normalize_wii_game_id(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    (value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(value)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingWiiGame {
    pub game_id: u64,
    pub title: String,
    pub system: String,
    pub region: Option<String>,
    pub dolphin_game_id: Option<String>,
    pub revision: Option<u16>,
    pub disc_number: Option<u8>,
    pub crc32: Option<String>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingWiiIndexRecord {
    pub game_id: u64,
    pub title: String,
    pub dolphin_game_id: Option<String>,
    pub region: Option<String>,
    pub revision: Option<u16>,
    pub disc_number: Option<u8>,
    pub crc32: Option<String>,
    pub source_url: String,
    pub index_source_url: String,
    pub retrieved_at_unix_seconds: u64,
}

impl GameHackingWiiIndexRecord {
    pub fn as_game(&self) -> GameHackingWiiGame {
        GameHackingWiiGame {
            game_id: self.game_id,
            title: self.title.clone(),
            system: "Wii".to_string(),
            region: self.region.clone(),
            dolphin_game_id: self.dolphin_game_id.clone(),
            revision: self.revision,
            disc_number: self.disc_number,
            crc32: self.crc32.clone(),
            source_url: self.source_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingWiiIndexPage {
    pub page_number: u32,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub sha256: String,
    pub game_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingWiiCatalogue {
    pub schema_version: u32,
    pub provider: String,
    pub system: String,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub pages: Vec<GameHackingWiiIndexPage>,
    pub games: Vec<GameHackingWiiIndexRecord>,
    #[serde(default)]
    pub coverage: WiiCatalogueCoverage,
    #[serde(default)]
    pub browser_imports: Vec<WiiBrowserImportProvenance>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WiiCatalogueCoverage {
    BrowserAssistedPartial,
    #[default]
    CompleteCrawl,
}

impl WiiCatalogueCoverage {
    pub fn label(self) -> &'static str {
        match self {
            Self::BrowserAssistedPartial => "browser-assisted partial cache",
            Self::CompleteCrawl => "complete catalogue crawl",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WiiBrowserImportProvenance {
    pub parser_schema_version: u32,
    pub game_id: u64,
    pub dolphin_game_id: String,
    pub imported_at_unix_seconds: u64,
    pub content_sha256: String,
    pub cache_file: String,
    pub game: GameHackingWiiIndexRecord,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GameHackingWiiIndexRefreshResult {
    pub catalogue_path: PathBuf,
    pub pages_total: usize,
    pub pages_downloaded: usize,
    pub pages_reused: usize,
    pub games: usize,
    pub retrieved_at_unix_seconds: u64,
    pub cached_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingWiiIndexProgress {
    pub pages_complete: usize,
    pub pages_total: usize,
    pub page_number: Option<u32>,
    pub downloaded: bool,
    pub games_collected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GameHackingWiiMatchStrength {
    ExactGameIdAndRevision,
    ExactGameIdAndRegion,
    ExactGameId,
    ExactGameIdRevisionUnverified,
}

impl GameHackingWiiMatchStrength {
    fn priority(self) -> u8 {
        match self {
            Self::ExactGameIdAndRevision => 1,
            Self::ExactGameIdAndRegion => 2,
            Self::ExactGameId => 3,
            Self::ExactGameIdRevisionUnverified => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ExactGameIdAndRevision => "exact Wii Game ID + verified revision",
            Self::ExactGameIdAndRegion => "exact Wii Game ID + compatible region",
            Self::ExactGameId => "exact Wii Game ID",
            Self::ExactGameIdRevisionUnverified => {
                "exact Wii Game ID; catalogue revision needs confirmation"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingWiiMatchCandidate {
    pub game: GameHackingWiiGame,
    pub strength: GameHackingWiiMatchStrength,
    pub requires_user_confirmation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameHackingWiiMatchStatus {
    Matched,
    Candidates,
    NoMatch,
    IdentityIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingWiiMatch {
    pub status: GameHackingWiiMatchStatus,
    pub game: Option<GameHackingWiiGame>,
    pub candidates: Vec<GameHackingWiiMatchCandidate>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingWiiMatchOutcome {
    pub result: GameHackingWiiMatch,
    /// Exactly one increment per catalogue row considered. Exposed for
    /// bounded-work diagnostics and regression tests, not timing guesses.
    pub catalogue_rows_examined: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WiiCodeFormat {
    ActionReplay,
    Gecko,
    RawUnknown,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WiiCheatSafety {
    Installable,
    UnresolvedPlaceholder,
    UnsupportedMasterCodeRequirement,
    MalformedCode,
    UnverifiedFormatLabel,
}

impl WiiCheatSafety {
    pub fn installable(self) -> bool {
        self == Self::Installable
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Installable => "explicit format label and strict raw code lines",
            Self::UnresolvedPlaceholder => "contains unresolved placeholder text",
            Self::UnsupportedMasterCodeRequirement => {
                "master/enable-code dependency is not resolved"
            }
            Self::MalformedCode => "code body is not strict XXXXXXXX YYYYYYYY pairs",
            Self::UnverifiedFormatLabel => "format label is absent or not fixture-verified",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingWiiCheat {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub code_format: WiiCodeFormat,
    pub safety: WiiCheatSafety,
    pub safety_warnings: Vec<String>,
    pub code_lines: Vec<String>,
    pub source_game_id: u64,
    pub source_url: String,
}

struct WiiCatalogueHooks;

impl GameHackingCatalogueHooks for WiiCatalogueHooks {
    type Record = GameHackingWiiIndexRecord;
    type Page = GameHackingWiiIndexPage;
    type Catalogue = GameHackingWiiCatalogue;

    fn discover_page_numbers(
        &self,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<u32>, GameHackingError> {
        discover_wii_index_page_numbers(bytes, charset)
    }

    fn parse_page(
        &self,
        source_url: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<Self::Record>, GameHackingError> {
        parse_wii_index_page(source_url, retrieved_at_unix_seconds, bytes, charset)
    }

    fn record_id(&self, record: &Self::Record) -> u64 {
        record.game_id
    }

    fn record_title<'a>(&self, record: &'a Self::Record) -> &'a str {
        &record.title
    }

    fn make_page(&self, metadata: GameHackingCataloguePageMetadata) -> Self::Page {
        GameHackingWiiIndexPage {
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
        GameHackingWiiCatalogue {
            schema_version: metadata.schema_version,
            provider: metadata.provider.to_string(),
            system: metadata.system.to_string(),
            source_url: metadata.source_url.to_string(),
            retrieved_at_unix_seconds: metadata.retrieved_at_unix_seconds,
            pages,
            games,
            coverage: WiiCatalogueCoverage::CompleteCrawl,
            browser_imports: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct GameHackingWiiProvider {
    client: GameHackingClient,
}

impl GameHackingWiiProvider {
    pub fn refresh_wii_index<F>(
        &self,
        options: &GameHackingWiiFetchOptions,
        mut progress: F,
    ) -> Result<GameHackingWiiIndexRefreshResult, GameHackingError>
    where
        F: FnMut(GameHackingWiiIndexProgress),
    {
        let retained_imports = load_wii_catalogue(&options.cache_root)
            .map(|catalogue| catalogue.browser_imports)
            .unwrap_or_default();
        let root_files = [WII_INDEX_ROOT_CACHE_FILE];
        let spec = GameHackingCatalogueSpec {
            schema_version: WII_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_WII_PROVIDER_ID,
            system: "Wii",
            index_url: WII_INDEX_URL,
            robots_path: "/system/wii/all",
            root_cache_files: &root_files,
            page_cache_prefix: WII_INDEX_PAGE_PREFIX,
            page_cache_suffix: ".html",
            catalogue_cache_file: WII_CATALOGUE_FILE,
            maximum_index_bytes: MAX_INDEX_BYTES,
            maximum_pages: MAX_WII_INDEX_PAGES,
            insert_root_page_zero: true,
            no_pages_error: "GameHacking.org Wii root index contained no numbered pages",
            page_count_error: "GameHacking.org Wii index page count is invalid",
            incomplete_pagination_error: "GameHacking.org Wii index pagination is incomplete",
            page_limit_error: "GameHacking.org Wii index exceeded the page limit",
        };
        let result = GameHackingCatalogueCrawler::new(&self.client).crawl(
            &spec,
            options,
            &WiiCatalogueHooks,
            |transport, url, maximum_bytes| transport.get(url, maximum_bytes),
            |event| {
                progress(GameHackingWiiIndexProgress {
                    pages_complete: event.pages_complete,
                    pages_total: event.pages_total,
                    page_number: event.page_number,
                    downloaded: event.downloaded,
                    games_collected: event.games_collected,
                });
            },
        )?;
        if !retained_imports.is_empty() {
            let mut catalogue = load_wii_catalogue(&options.cache_root)?;
            merge_browser_imports(&mut catalogue, retained_imports);
            let bytes = serde_json::to_vec_pretty(&catalogue).map_err(|failure| {
                wii_error(
                    GameHackingErrorKind::CacheUnavailable,
                    format!("merged Wii catalogue could not be serialized: {failure}"),
                )
            })?;
            atomic_write(&result.catalogue_path, &bytes)?;
        }
        Ok(GameHackingWiiIndexRefreshResult {
            catalogue_path: result.catalogue_path,
            pages_total: result.pages_total,
            pages_downloaded: result.pages_downloaded,
            pages_reused: result.pages_reused,
            games: result.games,
            retrieved_at_unix_seconds: result.retrieved_at_unix_seconds,
            cached_fallback: result.cached_fallback,
        })
    }

    pub fn match_game(
        &self,
        identity: &WiiGameIdentity,
        options: &GameHackingWiiFetchOptions,
    ) -> Result<GameHackingWiiMatch, GameHackingError> {
        Ok(self.match_game_with_metrics(identity, options)?.result)
    }

    pub fn match_game_with_metrics(
        &self,
        identity: &WiiGameIdentity,
        options: &GameHackingWiiFetchOptions,
    ) -> Result<GameHackingWiiMatchOutcome, GameHackingError> {
        if identity.verified_game_id().is_none() {
            return Ok(GameHackingWiiMatchOutcome {
                result: GameHackingWiiMatch {
                    status: GameHackingWiiMatchStatus::IdentityIncomplete,
                    game: None,
                    candidates: Vec::new(),
                    detail: "A verified Wii disc-header Game ID is required before matching."
                        .to_string(),
                },
                catalogue_rows_examined: 0,
            });
        }
        let catalogue = load_wii_catalogue(&options.cache_root)?;
        match_wii_catalogue(identity, &catalogue, options.cancellation.as_deref())
    }

    /// Reads only an exact imported/cached Wii game page. It never invokes
    /// the shared transport, refreshes metadata, or falls through to a live
    /// request when the file is absent.
    pub fn load_cached_game_page_cheats(
        &self,
        identity: &WiiGameIdentity,
        game: &GameHackingWiiGame,
        options: &GameHackingWiiFetchOptions,
    ) -> Result<Option<Vec<GameHackingWiiCheat>>, GameHackingError> {
        if options
            .cancellation
            .as_deref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(wii_error(
                GameHackingErrorKind::Cancelled,
                "GameHacking.org Wii cached match was cancelled",
            ));
        }
        let path = options
            .cache_root
            .join(format!("wii-game-{}.html", game.game_id));
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(wii_error(
                    GameHackingErrorKind::CacheUnavailable,
                    format!("cached Wii game page could not be inspected: {error}"),
                ));
            }
        }
        let bytes = bounded_read(&path, MAX_GAME_PAGE_BYTES)
            .map_err(|failure| wii_error(failure.kind, failure.detail))?;
        let cheats = parse_wii_game_page(identity, game, &bytes)?;
        Ok(Some(cheats))
    }

    pub fn fetch_game_page_cheats(
        &self,
        identity: &WiiGameIdentity,
        game: &GameHackingWiiGame,
        user_confirmed: bool,
        options: &GameHackingWiiFetchOptions,
    ) -> Result<Vec<GameHackingWiiCheat>, GameHackingError> {
        Ok(self
            .fetch_game_page_cheats_with_status(identity, game, user_confirmed, options)?
            .data)
    }

    pub fn fetch_game_page_cheats_with_status(
        &self,
        identity: &WiiGameIdentity,
        game: &GameHackingWiiGame,
        user_confirmed: bool,
        options: &GameHackingWiiFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingWiiCheat>>, GameHackingError> {
        let Some((_strength, requires_confirmation)) = classify_wii_match(identity, game) else {
            return Err(wii_error(
                GameHackingErrorKind::IdentityConflict,
                "selected GameHacking.org Wii game does not match the verified local Game ID",
            ));
        };
        if requires_confirmation && !user_confirmed {
            return Err(wii_error(
                GameHackingErrorKind::IdentityConflict,
                "this Wii catalogue candidate requires explicit revision confirmation",
            ));
        }
        self.client.check_robots(options, &["/game/"])?;
        let cache_name = format!("wii-game-{}.html", game.game_id);
        let response = self.client.cached_request(
            GameHackingRequestSpec {
                cache_file: &cache_name,
                url: &game.source_url,
                maximum_bytes: MAX_GAME_PAGE_BYTES,
            },
            options,
            |transport| transport.get(&game.source_url, MAX_GAME_PAGE_BYTES),
        )?;
        Ok(GameHackingFetchOutcome {
            data: parse_wii_game_page(identity, game, &response.bytes)?,
            cached_fallback: response.cached_fallback,
            retrieved_at_unix_seconds: response.retrieved_at_unix_seconds,
        })
    }
}

fn match_wii_catalogue(
    identity: &WiiGameIdentity,
    catalogue: &GameHackingWiiCatalogue,
    cancellation: Option<&AtomicBool>,
) -> Result<GameHackingWiiMatchOutcome, GameHackingError> {
    let coverage_note = match catalogue.coverage {
        WiiCatalogueCoverage::BrowserAssistedPartial => format!(
            " Wii cache available. Coverage: browser-imported entries only ({} game{} imported).",
            catalogue.browser_imports.len(),
            if catalogue.browser_imports.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        WiiCatalogueCoverage::CompleteCrawl => String::new(),
    };
    let mut rows_examined = 0_usize;
    let mut candidates = Vec::new();
    for record in &catalogue.games {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(wii_error(
                GameHackingErrorKind::Cancelled,
                "GameHacking.org Wii cached match was cancelled",
            ));
        }
        rows_examined = rows_examined.saturating_add(1);
        let game = record.as_game();
        let Some((strength, requires_user_confirmation)) = classify_wii_match(identity, &game)
        else {
            continue;
        };
        candidates.push(GameHackingWiiMatchCandidate {
            game,
            strength,
            requires_user_confirmation,
        });
    }
    if candidates.is_empty() {
        return Ok(GameHackingWiiMatchOutcome {
            result: GameHackingWiiMatch {
                status: GameHackingWiiMatchStatus::NoMatch,
                game: None,
                candidates: Vec::new(),
                detail: format!(
                    "No exact six-character Wii Game ID match exists in the cached catalogue.{coverage_note}"
                ),
            },
            catalogue_rows_examined: rows_examined,
        });
    }
    candidates.sort_by(|left, right| {
        left.strength
            .priority()
            .cmp(&right.strength.priority())
            .then_with(|| left.game.title.cmp(&right.game.title))
            .then_with(|| left.game.game_id.cmp(&right.game.game_id))
    });
    let best = candidates[0].strength.priority();
    candidates.retain(|candidate| candidate.strength.priority() == best);
    if candidates.len() == 1 && !candidates[0].requires_user_confirmation {
        let selected = candidates.remove(0);
        return Ok(GameHackingWiiMatchOutcome {
            result: GameHackingWiiMatch {
                status: GameHackingWiiMatchStatus::Matched,
                detail: format!(
                    "Matched {} by {} from the cached Wii catalogue.{coverage_note}",
                    selected.game.title,
                    selected.strength.label()
                ),
                game: Some(selected.game),
                candidates: Vec::new(),
            },
            catalogue_rows_examined: rows_examined,
        });
    }
    Ok(GameHackingWiiMatchOutcome {
        result: GameHackingWiiMatch {
            status: GameHackingWiiMatchStatus::Candidates,
            game: None,
            candidates,
            detail: format!(
                "The Wii Game ID matches more than one revision, or the catalogue revision cannot be verified locally. Select the exact candidate explicitly.{coverage_note}"
            ),
        },
        catalogue_rows_examined: rows_examined,
    })
}

fn classify_wii_match(
    identity: &WiiGameIdentity,
    game: &GameHackingWiiGame,
) -> Option<(GameHackingWiiMatchStrength, bool)> {
    let local = identity
        .verified_game_id()
        .and_then(normalize_wii_game_id)?;
    let remote = game
        .dolphin_game_id
        .as_deref()
        .and_then(normalize_wii_game_id)?;
    if local != remote || !game.system.eq_ignore_ascii_case("Wii") {
        return None;
    }
    let local_region = identity.region.as_deref().and_then(region_family_from_code);
    let remote_region = game.region.as_deref().and_then(remote_region_family);
    if let (Some(local_region), Some(remote_region)) = (local_region, remote_region)
        && local_region != remote_region
    {
        return None;
    }
    if let Some(remote_revision) = game.revision {
        if identity.verified_revision == Some(remote_revision) {
            return Some((GameHackingWiiMatchStrength::ExactGameIdAndRevision, false));
        }
        return Some((
            GameHackingWiiMatchStrength::ExactGameIdRevisionUnverified,
            true,
        ));
    }
    if local_region.is_some() && local_region == remote_region {
        Some((GameHackingWiiMatchStrength::ExactGameIdAndRegion, false))
    } else {
        Some((GameHackingWiiMatchStrength::ExactGameId, false))
    }
}

fn region_family_from_code(value: &str) -> Option<&'static str> {
    match value.trim().chars().next()?.to_ascii_uppercase() {
        'E' => Some("usa"),
        'P' | 'D' | 'F' | 'I' | 'S' | 'H' | 'X' | 'Y' | 'Z' => Some("europe"),
        'J' => Some("japan"),
        'K' | 'Q' | 'T' => Some("korea"),
        _ => None,
    }
}

fn remote_region_family(value: &str) -> Option<&'static str> {
    let value = value.to_ascii_lowercase();
    if value.contains("usa") || value.contains("ntsc-u") || value.contains("north america") {
        Some("usa")
    } else if value.contains("europe") || value.contains("pal") {
        Some("europe")
    } else if value.contains("japan") || value.contains("ntsc-j") {
        Some("japan")
    } else if value.contains("korea") {
        Some("korea")
    } else {
        None
    }
}

fn wii_page_number_from_href(href: &str) -> Option<u32> {
    let base = Url::parse(BASE_URL).ok()?;
    let resolved = base.join(href).ok()?;
    if resolved.host_str() != Some("gamehacking.org") {
        return None;
    }
    let suffix = resolved.path().strip_prefix("/system/wii/all/")?;
    let suffix = suffix.trim_end_matches('/');
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    suffix.parse().ok()
}

fn discover_wii_index_page_numbers(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<u32>, GameHackingError> {
    reject_challenge(bytes)?;
    let document = Html::parse_document(&decode_provider_text(bytes, charset));
    let selector = Selector::parse("a[href]").expect("static selector");
    Ok(document
        .select(&selector)
        .filter_map(|link| link.value().attr("href"))
        .filter_map(wii_page_number_from_href)
        .collect())
}

pub fn parse_gamehacking_wii_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
) -> Result<Vec<GameHackingWiiIndexRecord>, GameHackingError> {
    parse_wii_index_page(source_url, retrieved_at_unix_seconds, bytes, None)
}

fn parse_wii_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<GameHackingWiiIndexRecord>, GameHackingError> {
    reject_challenge(bytes)?;
    let document = Html::parse_document(&decode_provider_text(bytes, charset));
    let row_selector = Selector::parse("tr").expect("static selector");
    let cell_selector = Selector::parse("th, td").expect("static selector");
    let game_selector = Selector::parse("a[href^='/game/']").expect("static selector");
    let mut current_title = None::<String>;
    let mut games = BTreeMap::new();
    for row in document.select(&row_selector) {
        let cells = row
            .select(&cell_selector)
            .map(element_text)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let game_link = row.select(&game_selector).find_map(|node| {
            let href = node.value().attr("href")?;
            let game_id = href
                .trim_start_matches("/game/")
                .split('/')
                .next()?
                .parse::<u64>()
                .ok()?;
            Some((game_id, href.to_string(), element_text(node)))
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
        // The linked version/region label is the first cell in the common
        // table. A six-letter word such as "Europe" happens to satisfy the
        // Game-ID character shape, but it is not the Serial column.
        let dolphin_game_id = cells
            .iter()
            .filter(|cell| !cell.eq_ignore_ascii_case(&link_label))
            .find_map(|cell| normalize_wii_game_id(cell));
        let revision = cells.iter().find_map(|cell| parse_revision(cell));
        let disc_number = cells.iter().find_map(|cell| parse_disc_number(cell));
        let crc32 = cells.iter().find_map(|cell| normalize_crc32(cell));
        let source = if href.starts_with("https://") {
            href
        } else {
            format!("{BASE_URL}{href}")
        };
        games.entry(game_id).or_insert(GameHackingWiiIndexRecord {
            game_id,
            title,
            dolphin_game_id,
            region: (!link_label.is_empty()).then_some(link_label),
            revision,
            disc_number,
            crc32,
            source_url: source,
            index_source_url: source_url.to_string(),
            retrieved_at_unix_seconds,
        });
    }
    if games.is_empty() {
        return Err(wii_error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org Wii index page contained no game rows: {source_url}"),
        ));
    }
    Ok(games.into_values().collect())
}

fn parse_revision(value: &str) -> Option<u16> {
    let lower = value.to_ascii_lowercase();
    for marker in ["revision", "rev"] {
        if let Some((_, tail)) = lower.split_once(marker) {
            let digits = tail
                .trim_start_matches(|character: char| !character.is_ascii_digit())
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if !digits.is_empty() {
                return digits.parse().ok();
            }
        }
    }
    None
}

fn parse_disc_number(value: &str) -> Option<u8> {
    let lower = value.to_ascii_lowercase();
    let (_, tail) = lower.split_once("disc")?;
    tail.trim_start_matches(|character: char| !character.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn normalize_crc32(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    (value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

fn element_text(element: scraper::ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn reject_challenge(bytes: &[u8]) -> Result<(), GameHackingError> {
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(wii_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
    Ok(())
}

pub fn load_wii_catalogue(root: &Path) -> Result<GameHackingWiiCatalogue, GameHackingError> {
    let path = root.join(WII_CATALOGUE_FILE);
    let bytes = bounded_read(&path, 32 * 1024 * 1024).map_err(|failure| {
        wii_error(
            failure.kind,
            format!(
                "GameHacking.org Wii catalogue is unavailable; run `archivefs-cli gamehacking-wii-index-refresh` first: {}",
                failure.detail
            ),
        )
    })?;
    let catalogue: GameHackingWiiCatalogue = serde_json::from_slice(&bytes).map_err(|failure| {
        wii_error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org Wii catalogue is invalid: {failure}"),
        )
    })?;
    if catalogue.schema_version != WII_CATALOGUE_SCHEMA_VERSION
        || catalogue.provider != GAMEHACKING_WII_PROVIDER_ID
        || !catalogue.system.eq_ignore_ascii_case("Wii")
    {
        return Err(wii_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org Wii catalogue metadata is unsupported",
        ));
    }
    Ok(catalogue)
}

fn merge_browser_imports(
    catalogue: &mut GameHackingWiiCatalogue,
    imports: Vec<WiiBrowserImportProvenance>,
) {
    for imported in imports {
        if let Some(existing) = catalogue
            .games
            .iter_mut()
            .find(|record| record.game_id == imported.game_id)
        {
            if existing.dolphin_game_id.is_none()
                || existing.dolphin_game_id != Some(imported.dolphin_game_id.clone())
            {
                *existing = imported.game.clone();
            }
        } else {
            catalogue.games.push(imported.game.clone());
        }
        if let Some(existing) = catalogue
            .browser_imports
            .iter_mut()
            .find(|entry| entry.game_id == imported.game_id)
        {
            *existing = imported;
        } else {
            catalogue.browser_imports.push(imported);
        }
    }
    catalogue.games.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then_with(|| left.game_id.cmp(&right.game_id))
    });
    catalogue.browser_imports.sort_by_key(|entry| entry.game_id);
}

fn page_table_metadata(document: &Html) -> BTreeMap<String, String> {
    let table_selector = Selector::parse("table").expect("static selector");
    let row_selector = Selector::parse("tr").expect("static selector");
    let cell_selector = Selector::parse("th, td").expect("static selector");
    let mut metadata = BTreeMap::new();
    for table in document.select(&table_selector) {
        let rows = table
            .select(&row_selector)
            .map(|row| {
                row.select(&cell_selector)
                    .map(element_text)
                    .collect::<Vec<_>>()
            })
            .filter(|cells| !cells.is_empty())
            .collect::<Vec<_>>();
        for cells in &rows {
            if cells.len() == 2 {
                metadata.insert(
                    cells[0].trim().to_ascii_lowercase(),
                    cells[1].trim().to_string(),
                );
            }
        }
        for pair in rows.windows(2) {
            if pair[0].len() == pair[1].len() {
                for (label, value) in pair[0].iter().zip(&pair[1]) {
                    if ["system", "region", "serial", "game id", "crc32", "revision"]
                        .iter()
                        .any(|known| label.trim().eq_ignore_ascii_case(known))
                    {
                        metadata
                            .insert(label.trim().to_ascii_lowercase(), value.trim().to_string());
                    }
                }
            }
        }
    }
    metadata
}

fn game_from_imported_wii_page(
    identity: &WiiGameIdentity,
    game_id: u64,
    bytes: &[u8],
) -> Result<GameHackingWiiGame, WiiManualImportError> {
    if bytes.len() > MAX_GAME_PAGE_BYTES || cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(import_error(
            if bytes.len() > MAX_GAME_PAGE_BYTES {
                WiiManualImportErrorKind::InputTooLarge
            } else {
                WiiManualImportErrorKind::ChallengeContent
            },
            "saved content is not a bounded completed Wii game page",
        ));
    }
    let text = String::from_utf8_lossy(bytes);
    let document = Html::parse_document(&text);
    let title_selector = Selector::parse("title").expect("static selector");
    let title = document
        .select(&title_selector)
        .next()
        .map(element_text)
        .map(|value| {
            value
                .strip_prefix("GameHacking.org | ")
                .unwrap_or(&value)
                .trim()
                .to_string()
        })
        .filter(|value: &String| !value.is_empty())
        .ok_or_else(|| {
            import_error(
                WiiManualImportErrorKind::InvalidPage,
                "saved Wii game page has no game title",
            )
        })?;
    let local_game_id = identity
        .verified_game_id()
        .and_then(normalize_wii_game_id)
        .ok_or_else(|| {
            import_error(
                WiiManualImportErrorKind::IdentityConflict,
                "a verified local Wii Dolphin Game ID is required for import",
            )
        })?;
    let metadata = page_table_metadata(&document);
    let game = GameHackingWiiGame {
        game_id,
        title,
        system: "Wii".to_string(),
        region: metadata.get("region").cloned(),
        dolphin_game_id: Some(local_game_id),
        revision: metadata
            .get("revision")
            .and_then(|value| parse_revision(value)),
        disc_number: None,
        crc32: metadata
            .get("crc32")
            .and_then(|value| normalize_crc32(value)),
        source_url: format!("{BASE_URL}/game/{game_id}"),
    };
    validate_wii_page_identity(&document, identity, &game).map_err(|failure| {
        import_error(WiiManualImportErrorKind::IdentityConflict, failure.detail)
    })?;
    Ok(game)
}

pub fn parse_wii_game_page(
    identity: &WiiGameIdentity,
    game: &GameHackingWiiGame,
    bytes: &[u8],
) -> Result<Vec<GameHackingWiiCheat>, GameHackingError> {
    reject_challenge(bytes)?;
    if bytes.len() > MAX_GAME_PAGE_BYTES {
        return Err(wii_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org Wii game page exceeded the byte limit",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        wii_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org Wii game page is not UTF-8",
        )
    })?;
    let document = Html::parse_document(text);
    validate_wii_page_identity(&document, identity, game)?;
    let entry_selector = Selector::parse(".codID").expect("static selector");
    let label_selector = Selector::parse(".col-sm-3 small").expect("static selector");
    let code_selector = Selector::parse(".col-sm-4.col-md-3 pre").expect("static selector");
    let title_selector = Selector::parse("label").expect("static selector");
    let input_selector = Selector::parse("input[name='codID[]']").expect("static selector");
    let author_selector = Selector::parse("a[href^='/hackers/']").expect("static selector");
    let mut cheats = Vec::new();
    for entry in document.select(&entry_selector) {
        let title = entry
            .select(&title_selector)
            .next()
            .map(element_text)
            .filter(|value| !value.is_empty());
        let Some(name) = title else {
            continue;
        };
        let id = entry
            .select(&input_selector)
            .next()
            .and_then(|node| node.value().attr("value"))
            .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("page-{}-{}", game.game_id, cheats.len() + 1));
        let author = entry
            .select(&author_selector)
            .next()
            .map(element_text)
            .filter(|value| !value.is_empty());
        let Some(row) = entry.parent_element() else {
            continue;
        };
        let label = row
            .select(&label_selector)
            .next()
            .map(element_text)
            .filter(|value| !value.is_empty());
        let raw_lines = row
            .select(&code_selector)
            .next()
            .map(|pre| {
                pre.text()
                    .collect::<String>()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let code_format = label
            .as_deref()
            .map(map_wii_label)
            .unwrap_or(WiiCodeFormat::RawUnknown);
        let (safety, safety_warnings) = classify_wii_cheat_safety(&name, code_format, &raw_lines);
        cheats.push(GameHackingWiiCheat {
            id,
            name,
            author,
            description: label.map(|label| format!("GameHacking.org format: {label}")),
            code_format,
            safety,
            safety_warnings,
            code_lines: raw_lines,
            source_game_id: game.game_id,
            source_url: game.source_url.clone(),
        });
    }
    if cheats.is_empty() {
        return Err(wii_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org Wii game page contained no named cheat entries",
        ));
    }
    Ok(cheats)
}

fn validate_wii_page_identity(
    document: &Html,
    identity: &WiiGameIdentity,
    game: &GameHackingWiiGame,
) -> Result<(), GameHackingError> {
    let input_selector = Selector::parse("input").expect("static selector");
    let mut hidden_game_id = None;
    let mut hidden_system_id = None;
    for input in document.select(&input_selector) {
        let name = input.value().attr("name").unwrap_or_default();
        let value = input.value().attr("value").unwrap_or_default();
        if name.eq_ignore_ascii_case("gamID") {
            hidden_game_id = value.parse::<u64>().ok();
        } else if name.eq_ignore_ascii_case("sysID") {
            hidden_system_id = value.parse::<u16>().ok();
        }
    }
    if hidden_game_id != Some(game.game_id) {
        return Err(wii_error(
            GameHackingErrorKind::IdentityConflict,
            "imported Wii page does not carry the selected GameHacking game ID",
        ));
    }
    if hidden_system_id != Some(VERIFIED_WII_SYS_ID) {
        return Err(wii_error(
            GameHackingErrorKind::IdentityConflict,
            "imported game page does not carry the verified Wii sysID 22",
        ));
    }
    let metadata = page_table_metadata(document);
    let serial = metadata
        .get("serial")
        .or_else(|| metadata.get("game id"))
        .and_then(|value| normalize_wii_game_id(value));
    let system_link_selector = Selector::parse("a[href^='/system/wii']").expect("static selector");
    let system_is_wii = metadata.get("system").is_some_and(|value| {
        value.trim().eq_ignore_ascii_case("wii")
            || value.to_ascii_lowercase().contains("nintendo wii")
    }) || document.select(&system_link_selector).next().is_some();
    let local = identity.verified_game_id().and_then(normalize_wii_game_id);
    if serial.is_none() || serial != local || serial != game.dolphin_game_id {
        return Err(wii_error(
            GameHackingErrorKind::IdentityConflict,
            "imported Wii page Serial does not match the verified local Dolphin Game ID",
        ));
    }
    if !system_is_wii {
        return Err(wii_error(
            GameHackingErrorKind::IdentityConflict,
            "imported game page does not identify its system as Wii",
        ));
    }
    Ok(())
}

fn map_wii_label(label: &str) -> WiiCodeFormat {
    match label.trim().to_ascii_lowercase().as_str() {
        "gecko" => WiiCodeFormat::Gecko,
        "action replay" | "armax" | "action replay max" => WiiCodeFormat::ActionReplay,
        // The audit explicitly says WiiRD terminology must not be treated as
        // interchangeable with a page's Gecko label without a real fixture.
        _ => WiiCodeFormat::RawUnknown,
    }
}

fn classify_wii_cheat_safety(
    name: &str,
    format: WiiCodeFormat,
    lines: &[String],
) -> (WiiCheatSafety, Vec<String>) {
    let lower_name = name.to_ascii_lowercase();
    if lower_name.contains("master code") || lower_name.contains("enable code") {
        return (
            WiiCheatSafety::UnsupportedMasterCodeRequirement,
            vec!["EmuWiz cannot prove master/enable-code dependencies from this page.".into()],
        );
    }
    if lines.iter().any(|line| contains_placeholder(line)) {
        return (
            WiiCheatSafety::UnresolvedPlaceholder,
            vec!["Replaceable X/Y/Z/? fields are never installed automatically.".into()],
        );
    }
    if !matches!(format, WiiCodeFormat::Gecko | WiiCodeFormat::ActionReplay) {
        return (
            WiiCheatSafety::UnverifiedFormatLabel,
            vec!["Only explicit Gecko or Action Replay labels are installable.".into()],
        );
    }
    if lines.is_empty() || !lines.iter().all(|line| strict_code_line(line)) {
        return (
            WiiCheatSafety::MalformedCode,
            vec!["Dolphin requires two eight-hex-digit words per line.".into()],
        );
    }
    let mut warnings = Vec::new();
    if lower_name.contains("button")
        || lower_name.contains("activator")
        || lower_name.contains("joker")
    {
        warnings.push(
            "Button activator is controller-specific; EmuWiz preserves it without rewriting the mask."
                .into(),
        );
    }
    if lines.len() >= 8 {
        warnings.push(
            "Multi-line injection is preserved exactly; review its region and revision assumptions."
                .into(),
        );
    }
    (WiiCheatSafety::Installable, warnings)
}

fn contains_placeholder(line: &str) -> bool {
    let compact = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    compact
        .chars()
        .any(|character| matches!(character.to_ascii_uppercase(), '?' | 'X' | 'Y' | 'Z'))
}

fn strict_code_line(line: &str) -> bool {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    tokens.len() == 2
        && tokens
            .iter()
            .all(|token| token.len() == 8 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn to_gamecube_cheat(cheat: &GameHackingWiiCheat) -> GameHackingGameCubeCheat {
    GameHackingGameCubeCheat {
        id: format!("wii:{}", cheat.id),
        name: cheat.name.clone(),
        author: cheat.author.clone(),
        description: cheat.description.clone(),
        code_format: match cheat.code_format {
            WiiCodeFormat::ActionReplay if cheat.safety.installable() => {
                GameCubeCodeFormat::ActionReplay
            }
            WiiCodeFormat::Gecko if cheat.safety.installable() => GameCubeCodeFormat::Gecko,
            WiiCodeFormat::Unsupported => GameCubeCodeFormat::Unsupported,
            _ => GameCubeCodeFormat::RawUnknown,
        },
        code_lines: cheat.code_lines.clone(),
        source_game_id: cheat.source_game_id,
        source_url: cheat.source_url.clone(),
    }
}

pub fn stage_wii_gamehacking_install(
    staging_root: &Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    cheats: &[GameHackingWiiCheat],
    selected_indices: &[usize],
) -> Result<StagedGameCubeIni, super::gamehacking_gamecube_install_plan::GameCubeInstallPlanError> {
    let converted = cheats.iter().map(to_gamecube_cheat).collect::<Vec<_>>();
    let mut selection = GameCubeCheatSelection::from_cheats(&converted, destination);
    for index in selected_indices {
        selection.set_selected(*index, true);
    }
    stage_gamecube_gamehacking_install(
        staging_root,
        file_name,
        destination,
        destination_existed,
        &converted,
        &selection,
    )
}

pub fn stage_wii_gamehacking_removal(
    staging_root: &Path,
    file_name: &str,
    destination: &DolphinIniDocument,
    destination_existed: bool,
    remove_dolphin_names: &[String],
) -> Result<StagedGameCubeIni, super::gamehacking_gamecube_install_plan::GameCubeInstallPlanError> {
    stage_gamecube_gamehacking_removal(
        staging_root,
        file_name,
        destination,
        destination_existed,
        remove_dolphin_names,
    )
}

pub fn build_wii_gamehacking_install_preview(
    request: &GameCubeGameHackingInstallPreviewRequest,
) -> Result<GameCubeGameHackingInstallPreview, GameCubeInstallPlanError> {
    build_dolphin_gamehacking_install_preview(request, "Wii")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiiManualImportErrorKind {
    InputTooLarge,
    SourceUnreadable,
    ChallengeContent,
    IdentityConflict,
    InvalidPage,
    TextExportShapeUnverified,
    CacheWriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiiManualImportError {
    pub kind: WiiManualImportErrorKind,
    pub detail: String,
}

impl std::fmt::Display for WiiManualImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for WiiManualImportError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WiiManualImportOutcome {
    pub cache_path: PathBuf,
    pub catalogue_path: PathBuf,
    pub retrieved_at_unix_seconds: u64,
    pub game_id: u64,
    pub dolphin_game_id: String,
    pub game_title: String,
    pub cheats: Vec<GameHackingWiiCheat>,
    pub supported_cheat_count: usize,
    pub blocked_or_unknown_count: usize,
    pub coverage: WiiCatalogueCoverage,
    pub content_sha256: String,
    pub provenance: String,
    pub network_used: bool,
}

fn import_error(kind: WiiManualImportErrorKind, detail: impl Into<String>) -> WiiManualImportError {
    WiiManualImportError {
        kind,
        detail: detail.into(),
    }
}

pub fn import_wii_game_page_file(
    cache_root: &Path,
    identity: &WiiGameIdentity,
    game: &GameHackingWiiGame,
    source: &Path,
) -> Result<WiiManualImportOutcome, WiiManualImportError> {
    let bytes = bounded_read(source, MAX_GAME_PAGE_BYTES).map_err(|failure| {
        import_error(
            if failure.detail.contains("exceeded") {
                WiiManualImportErrorKind::InputTooLarge
            } else {
                WiiManualImportErrorKind::SourceUnreadable
            },
            failure.detail,
        )
    })?;
    import_wii_game_page_bytes(cache_root, identity, game, &bytes)
}

pub fn import_wii_game_page_bootstrap_file(
    cache_root: &Path,
    identity: &WiiGameIdentity,
    game_id: u64,
    source: &Path,
) -> Result<WiiManualImportOutcome, WiiManualImportError> {
    let bytes = bounded_read(source, MAX_GAME_PAGE_BYTES).map_err(|failure| {
        import_error(
            if failure.detail.contains("oversized") {
                WiiManualImportErrorKind::InputTooLarge
            } else {
                WiiManualImportErrorKind::SourceUnreadable
            },
            failure.detail,
        )
    })?;
    import_wii_game_page_bootstrap_bytes(cache_root, identity, game_id, &bytes)
}

pub fn import_wii_game_page_bootstrap_bytes(
    cache_root: &Path,
    identity: &WiiGameIdentity,
    game_id: u64,
    bytes: &[u8],
) -> Result<WiiManualImportOutcome, WiiManualImportError> {
    let catalogue_path = cache_root.join(WII_CATALOGUE_FILE);
    let existing_catalogue = match std::fs::symlink_metadata(&catalogue_path) {
        Ok(_) => Some(load_wii_catalogue(cache_root).map_err(|failure| {
            import_error(WiiManualImportErrorKind::InvalidPage, failure.detail)
        })?),
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => None,
        Err(failure) => {
            return Err(import_error(
                WiiManualImportErrorKind::CacheWriteFailed,
                format!("Wii catalogue could not be inspected: {failure}"),
            ));
        }
    };
    let game = if let Some(catalogue) = &existing_catalogue {
        catalogue
            .games
            .iter()
            .find(|record| record.game_id == game_id)
            .ok_or_else(|| {
                import_error(
                    WiiManualImportErrorKind::IdentityConflict,
                    format!("game {game_id} is not in the cached Wii catalogue"),
                )
            })?
            .as_game()
    } else {
        game_from_imported_wii_page(identity, game_id, bytes)?
    };

    // Validate completely before creating the cache directory or changing a
    // cache file. This rejects wrong IDs, systems, serials and challenges.
    let normalized = String::from_utf8_lossy(bytes);
    parse_wii_game_page(identity, &game, normalized.as_bytes()).map_err(|failure| {
        import_error(
            if failure.kind == GameHackingErrorKind::IdentityConflict {
                WiiManualImportErrorKind::IdentityConflict
            } else {
                WiiManualImportErrorKind::InvalidPage
            },
            failure.detail,
        )
    })?;

    let cache_path = cache_root.join(format!("wii-game-{game_id}.html"));
    let affected_paths = [
        cache_path.clone(),
        charset_cache_path(&cache_path),
        retrieved_cache_path(&cache_path),
        catalogue_path.clone(),
    ];
    let snapshots = affected_paths
        .iter()
        .map(|path| match std::fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 32 * 1024 * 1024 =>
            {
                std::fs::read(path).map(Some).map_err(|failure| {
                    import_error(
                        WiiManualImportErrorKind::CacheWriteFailed,
                        format!("existing Wii cache file could not be backed up: {failure}"),
                    )
                })
            }
            Ok(_) => Err(import_error(
                WiiManualImportErrorKind::CacheWriteFailed,
                "existing Wii cache file is unsafe or oversized",
            )),
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(failure) => Err(import_error(
                WiiManualImportErrorKind::CacheWriteFailed,
                format!("existing Wii cache file could not be inspected: {failure}"),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let result = (|| {
        let mut outcome = import_wii_game_page_bytes(cache_root, identity, &game, bytes)?;
        let stored = bounded_read(&outcome.cache_path, MAX_GAME_PAGE_BYTES).map_err(|failure| {
            import_error(WiiManualImportErrorKind::CacheWriteFailed, failure.detail)
        })?;
        let content_sha256 = hex_sha256(&stored);
        let record = GameHackingWiiIndexRecord {
            game_id: game.game_id,
            title: game.title.clone(),
            dolphin_game_id: game.dolphin_game_id.clone(),
            region: game.region.clone(),
            revision: game.revision,
            disc_number: game.disc_number,
            crc32: game.crc32.clone(),
            source_url: game.source_url.clone(),
            index_source_url: "browser-assisted-import".to_string(),
            retrieved_at_unix_seconds: outcome.retrieved_at_unix_seconds,
        };
        let provenance = WiiBrowserImportProvenance {
            parser_schema_version: WII_CATALOGUE_SCHEMA_VERSION,
            game_id,
            dolphin_game_id: identity.verified_game_id().unwrap_or_default().to_string(),
            imported_at_unix_seconds: outcome.retrieved_at_unix_seconds,
            content_sha256: content_sha256.clone(),
            cache_file: cache_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            game: record.clone(),
        };
        let mut catalogue = existing_catalogue.unwrap_or_else(|| GameHackingWiiCatalogue {
            schema_version: WII_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_WII_PROVIDER_ID.to_string(),
            system: "Wii".to_string(),
            source_url: "browser-assisted-import".to_string(),
            retrieved_at_unix_seconds: outcome.retrieved_at_unix_seconds,
            pages: Vec::new(),
            games: Vec::new(),
            coverage: WiiCatalogueCoverage::BrowserAssistedPartial,
            browser_imports: Vec::new(),
        });
        merge_browser_imports(&mut catalogue, vec![provenance]);
        let catalogue_bytes = serde_json::to_vec_pretty(&catalogue).map_err(|failure| {
            import_error(
                WiiManualImportErrorKind::CacheWriteFailed,
                format!("Wii partial catalogue could not be serialized: {failure}"),
            )
        })?;
        atomic_write(&catalogue_path, &catalogue_bytes).map_err(|failure| {
            import_error(WiiManualImportErrorKind::CacheWriteFailed, failure.detail)
        })?;
        outcome.catalogue_path = catalogue_path.clone();
        outcome.coverage = catalogue.coverage;
        outcome.content_sha256 = content_sha256;
        Ok(outcome)
    })();
    if result.is_err() {
        for (path, snapshot) in affected_paths.iter().zip(snapshots) {
            match snapshot {
                Some(bytes) => {
                    let _ = atomic_write(path, &bytes);
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
    result
}

pub fn import_wii_game_page_bytes(
    cache_root: &Path,
    identity: &WiiGameIdentity,
    game: &GameHackingWiiGame,
    bytes: &[u8],
) -> Result<WiiManualImportOutcome, WiiManualImportError> {
    if bytes.len() > MAX_GAME_PAGE_BYTES {
        return Err(import_error(
            WiiManualImportErrorKind::InputTooLarge,
            "saved Wii page exceeded the 8 MiB import limit",
        ));
    }
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(import_error(
            WiiManualImportErrorKind::ChallengeContent,
            "that is a Cloudflare challenge page, not the completed Wii game page",
        ));
    }
    let decoded = String::from_utf8_lossy(bytes);
    let normalized_bytes = decoded.as_bytes();
    let cheats = parse_wii_game_page(identity, game, normalized_bytes).map_err(|failure| {
        import_error(
            if failure.kind == GameHackingErrorKind::IdentityConflict {
                WiiManualImportErrorKind::IdentityConflict
            } else {
                WiiManualImportErrorKind::InvalidPage
            },
            failure.detail,
        )
    })?;
    let sanitized = sanitize_imported_html(&decoded);
    // Re-parse the bytes that will actually be cached. Sanitization must not
    // be capable of removing identity or cheat evidence.
    parse_wii_game_page(identity, game, sanitized.as_bytes())
        .map_err(|failure| import_error(WiiManualImportErrorKind::InvalidPage, failure.detail))?;
    if !cache_root.is_absolute() || cache_root.parent().is_none() {
        return Err(import_error(
            WiiManualImportErrorKind::CacheWriteFailed,
            "GameHacking cache root must be an absolute non-root path",
        ));
    }
    std::fs::create_dir_all(cache_root).map_err(|failure| {
        import_error(
            WiiManualImportErrorKind::CacheWriteFailed,
            format!("GameHacking cache could not be created: {failure}"),
        )
    })?;
    let cache_path = cache_root.join(format!("wii-game-{}.html", game.game_id));
    atomic_write(&cache_path, sanitized.as_bytes()).map_err(|failure| {
        import_error(WiiManualImportErrorKind::CacheWriteFailed, failure.detail)
    })?;
    atomic_write(&charset_cache_path(&cache_path), b"utf-8").map_err(|failure| {
        import_error(WiiManualImportErrorKind::CacheWriteFailed, failure.detail)
    })?;
    let retrieved_at_unix_seconds = unix_seconds_now();
    atomic_write(
        &retrieved_cache_path(&cache_path),
        retrieved_at_unix_seconds.to_string().as_bytes(),
    )
    .map_err(|failure| import_error(WiiManualImportErrorKind::CacheWriteFailed, failure.detail))?;
    Ok(WiiManualImportOutcome {
        cache_path,
        catalogue_path: cache_root.join(WII_CATALOGUE_FILE),
        retrieved_at_unix_seconds,
        game_id: game.game_id,
        dolphin_game_id: identity.verified_game_id().unwrap_or_default().to_string(),
        game_title: game.title.clone(),
        supported_cheat_count: cheats
            .iter()
            .filter(|cheat| cheat.safety == WiiCheatSafety::Installable)
            .count(),
        blocked_or_unknown_count: cheats
            .iter()
            .filter(|cheat| cheat.safety != WiiCheatSafety::Installable)
            .count(),
        cheats,
        coverage: WiiCatalogueCoverage::CompleteCrawl,
        content_sha256: String::new(),
        provenance: "browser-assisted saved Wii game page import".to_string(),
        network_used: false,
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

/// Explicitly blocked until a real Wii Text response fixture verifies its
/// identity header and block grammar. The imported bytes are never cached.
pub fn import_wii_text_export_unverified(
    _bytes: &[u8],
) -> Result<WiiManualImportOutcome, WiiManualImportError> {
    Err(import_error(
        WiiManualImportErrorKind::TextExportShapeUnverified,
        "Wii Text export import is disabled until a sanitized real response fixture verifies its identity and cheat-block shape",
    ))
}

const STRIPPED_ELEMENTS: [&str; 7] = [
    "script", "style", "noscript", "iframe", "frame", "object", "embed",
];

fn sanitize_imported_html(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..open]);
        let tail = &rest[open..];
        if let Some(after) = tail.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let lowered = tail
            .chars()
            .take(16)
            .collect::<String>()
            .to_ascii_lowercase();
        for element in STRIPPED_ELEMENTS {
            let marker = format!("<{element}");
            if lowered.starts_with(&marker) {
                let closing = format!("</{element}");
                rest = find_ignore_ascii_case(tail, &closing)
                    .and_then(|position| tail[position..].find('>').map(|end| position + end + 1))
                    .map_or("", |after| &tail[after..]);
                continue 'outer;
            }
        }
        match tail.find('>') {
            Some(end) => {
                output.push_str(&tail[..=end]);
                rest = &tail[end + 1..];
            }
            None => break,
        }
    }
    output
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests;
