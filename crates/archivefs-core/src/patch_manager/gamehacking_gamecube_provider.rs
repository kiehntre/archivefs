//! Cached GameCube catalogue and per-library-game access to GameHacking.org.
//!
//! GameCube only, mirroring the shape of the PS2 provider
//! (`gamehacking_provider.rs`) without touching it: retrieval, HTML
//! parsing, identity matching, and cheat-export parsing remain separate.
//! The explicit index command walks only the numbered public GameCube
//! table pages. Runtime matching is local, and only one selected game's
//! export is requested after an automatic match or user confirmation.
//! This milestone is preview-only: there is no install/apply path here at
//! all, unlike the PS2 provider.
//!
//! GameHacking.org's system slug for GameCube is confirmed to be `ngc`,
//! not `gamecube` - the catalogue lives at
//! `https://gamehacking.org/system/ngc/all` (see `GAMECUBE_INDEX_URL`).
//! ArchiveFS's own user-facing platform name stays "GameCube"
//! everywhere else (CLI command name, cache file names, the catalogue's
//! `system` field, GUI labels) - only the GameHacking URL path and
//! robots.txt check use the `ngc` slug.
//!
//! The numeric GameHacking.org system ID used for per-game cheat exports
//! (see `GameCubeGameHackingAdapter::system_id`) is still not confirmed:
//! this sandbox's network egress to `gamehacking.org` answers every
//! request (both `/system/ps2/all` and `/system/ngc/all`) with a
//! Cloudflare challenge page (HTTP 403), confirming an environment-wide
//! block rather than anything GameCube-specific. Catalogue crawling,
//! matching, and preview never need this constant (they only use
//! `index_url()`); only `fetch_cheats`/`fetch_cheats_for_confirmed_candidate`
//! do, and both fail loudly with `GameHackingErrorKind::UnsupportedSystem`
//! until `GameCubeGameHackingAdapter::system_id` is set to a confirmed value.

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

use super::gamehacking_provider::{GameHackingError, GameHackingErrorKind, gamehacking_cache_root};
use crate::game_identity::{GameIdentityReport, IdentityKind, IdentityPlatform, IdentityStatus};

pub const GAMEHACKING_GAMECUBE_PROVIDER_ID: &str = "gamehacking.org";
const BASE_URL: &str = "https://gamehacking.org";
/// Confirmed GameHacking.org system slug for GameCube: `ngc`, not
/// `gamecube`. Do not change this without re-confirming against a real
/// request - see the module doc comment.
const GAMECUBE_INDEX_URL: &str = "https://gamehacking.org/system/ngc/all";
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
const GAMECUBE_CATALOGUE_SCHEMA_VERSION: u32 = 1;
const GAMECUBE_CATALOGUE_FILE: &str = "gamecube-catalogue.json";
const GAMECUBE_INDEX_ROOT_CACHE_FILE: &str = "gamecube-index-root.html";
const MAX_GAMECUBE_INDEX_PAGES: usize = 512;

// --- Identity -----------------------------------------------------------

/// A verified-only local GameCube identity, adapted from the shared,
/// already-implemented Dolphin disc-header evidence in `game_identity.rs`
/// (see `GameIdentityReport::verified_dolphin_game_id`/
/// `verified_dolphin_revision`), exactly parallel to how
/// `Pcsx2GameIdentity` adapts PS2 evidence. Never promotes a candidate
/// value to verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameCubeIdentityState {
    Verified,
    MissingGameId,
    Deferred,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameCubeGameIdentity {
    pub archive_path: PathBuf,
    pub title: String,
    pub dolphin_game_id: Option<String>,
    /// The raw single-character Dolphin region code (the Game ID's 4th
    /// byte, e.g. `"E"`, `"P"`, `"J"`) - no locale name is inferred here,
    /// exactly like the underlying evidence it comes from.
    pub region: Option<String>,
    pub revision: Option<u16>,
    /// A generic loose-file SHA-256, when this GameCube image was
    /// identified as a loose (non-disc-header) file. Used only for the
    /// "exact hash + region" fallback match tier.
    pub loose_rom_sha256: Option<String>,
    pub state: GameCubeIdentityState,
    pub evidence: Vec<String>,
    pub plain_failure_reason: Option<String>,
}

impl GameCubeGameIdentity {
    pub fn from_report(title: impl Into<String>, report: &GameIdentityReport) -> Self {
        let title = title.into();
        let dolphin_game_id = report
            .verified_dolphin_game_id()
            .and_then(normalize_gamecube_game_id);
        let region = report
            .verified_value(IdentityKind::DolphinRegion)
            .map(str::to_owned);
        let revision = report.verified_dolphin_revision();
        let loose_rom_sha256 = report.verified_loose_rom_sha256().map(str::to_owned);
        let game_id_evidence = report
            .evidence
            .iter()
            .find(|item| item.kind == IdentityKind::DolphinGameId);
        let state = if report.platform != IdentityPlatform::GameCube {
            GameCubeIdentityState::Unsupported
        } else if dolphin_game_id.is_some() {
            GameCubeIdentityState::Verified
        } else {
            match game_id_evidence.map(|item| item.status) {
                Some(IdentityStatus::Deferred) => GameCubeIdentityState::Deferred,
                Some(IdentityStatus::Ambiguous | IdentityStatus::ResourceLimitReached) => {
                    GameCubeIdentityState::Ambiguous
                }
                Some(IdentityStatus::Unsupported | IdentityStatus::Invalid) => {
                    GameCubeIdentityState::Unsupported
                }
                _ => GameCubeIdentityState::MissingGameId,
            }
        };
        let plain_failure_reason = match state {
            GameCubeIdentityState::Verified => None,
            GameCubeIdentityState::MissingGameId => Some(
                "ArchiveFS could not prove the GameCube Game ID required for GameHacking.org matching."
                    .to_string(),
            ),
            GameCubeIdentityState::Deferred => Some(
                "Game identification is not available for this image format yet.".to_string(),
            ),
            GameCubeIdentityState::Ambiguous => Some(
                "ArchiveFS found ambiguous game identity evidence and will not guess.".to_string(),
            ),
            GameCubeIdentityState::Unsupported => {
                Some("This selection is not a supported GameCube game image.".to_string())
            }
        };
        let evidence = report
            .evidence
            .iter()
            .filter(|item| {
                matches!(
                    item.kind,
                    IdentityKind::DolphinGameId
                        | IdentityKind::DolphinRevision
                        | IdentityKind::DolphinRegion
                )
            })
            .map(|item| format!("{}: {} ({})", item.kind, item.status, item.diagnostic))
            .collect();
        Self {
            archive_path: report.archive_path.clone(),
            title,
            dolphin_game_id,
            region,
            revision,
            loose_rom_sha256,
            state,
            evidence,
            plain_failure_reason,
        }
    }

    pub fn verified_game_id(&self) -> Option<&str> {
        (self.state == GameCubeIdentityState::Verified)
            .then_some(self.dolphin_game_id.as_deref())
            .flatten()
    }
}

/// Normalizes a Dolphin Game ID to its exact 6-character uppercase
/// alphanumeric shape. `None` if `value` does not match.
pub fn normalize_gamecube_game_id(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    (value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(value)
}

/// Folds a raw single-character Dolphin region code into one of a small
/// set of region families, mirroring the same byte convention already
/// used by `dolphin_gecko_provider::region_for_game_id` for the 4th
/// Game-ID byte.
fn region_family_from_code(value: &str) -> Option<&'static str> {
    let byte = value.trim().chars().next()?.to_ascii_uppercase();
    match byte {
        'E' => Some("usa"),
        'P' | 'D' | 'F' | 'I' | 'S' | 'H' | 'X' | 'Y' | 'Z' => Some("europe"),
        'J' => Some("japan"),
        'K' | 'Q' | 'T' => Some("korea"),
        _ => None,
    }
}

/// Folds GameHacking.org's free-text region string into the same family
/// buckets as `region_family_from_code`, so a local raw region byte and a
/// remote free-text region string can be compared meaningfully.
fn gamehacking_region_family(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("usa") || lower.contains("ntsc-u") || lower.contains("north america") {
        Some("usa")
    } else if lower.contains("pal") || lower.contains("europe") {
        Some("europe")
    } else if lower.contains("japan") || lower.contains("ntsc-j") {
        Some("japan")
    } else if lower.contains("korea") {
        Some("korea")
    } else {
        None
    }
}

fn normalized_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

// --- Catalogue ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeGame {
    pub game_id: u64,
    pub title: String,
    pub system: String,
    pub region: Option<String>,
    pub dolphin_game_id: Option<String>,
    /// A disc revision, only when the catalogue listing happens to expose
    /// one (not confirmed to exist in practice - see the module doc
    /// comment). Never fabricated.
    pub revision: Option<u16>,
    /// A hash-like token scraped from the catalogue listing, if present
    /// (GameHacking.org's GameCube listing may or may not expose one -
    /// this is compared, never assumed).
    pub hash: Option<String>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeCheat {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub code_format: GameCubeCodeFormat,
    pub code_lines: Vec<String>,
    pub source_game_id: u64,
    pub source_url: String,
}

/// A returned cheat's identified raw code format. Never inferred from the
/// hex shape alone - only an explicit label in the exported text (an
/// `Encryption:`/`Format:` field) promotes a cheat to `ActionReplay` or
/// `Gecko`; ArchiveFS never speculatively converts between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameCubeCodeFormat {
    ActionReplay,
    Gecko,
    /// Well-formed 8-hex/8-hex code lines with no explicit format label.
    RawUnknown,
    /// Present but does not parse as a well-formed GameCube code line.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameHackingGameCubeMatchStatus {
    Matched,
    Candidates,
    NoMatch,
    IdentityConflict,
    IdentityIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingGameCubeMatch {
    pub status: GameHackingGameCubeMatchStatus,
    pub game: Option<GameHackingGameCubeGame>,
    pub candidates: Vec<GameHackingGameCubeMatchCandidate>,
    pub detail: String,
}

/// Match tiers in strict priority order, exactly mirroring the sequence
/// required for GameCube matching: exact Game ID with revision, exact Game
/// ID with region, exact Game ID alone, exact hash with region, and
/// finally a normalized-title-with-region candidate that always requires
/// explicit user confirmation. A bare title-only match (no region
/// agreement) is never produced at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GameHackingGameCubeMatchStrength {
    ExactGameIdAndRevision,
    ExactGameIdAndRegion,
    ExactGameId,
    ExactHashAndRegion,
    NormalizedTitleAndRegion,
}

impl GameHackingGameCubeMatchStrength {
    fn priority(self) -> u8 {
        match self {
            Self::ExactGameIdAndRevision => 1,
            Self::ExactGameIdAndRegion => 2,
            Self::ExactGameId => 3,
            Self::ExactHashAndRegion => 4,
            Self::NormalizedTitleAndRegion => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ExactGameIdAndRevision => "exact Game ID + revision",
            Self::ExactGameIdAndRegion => "exact Game ID + compatible region",
            Self::ExactGameId => "exact Game ID",
            Self::ExactHashAndRegion => "exact hash + compatible region",
            Self::NormalizedTitleAndRegion => "normalized title + compatible region",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeMatchCandidate {
    pub game: GameHackingGameCubeGame,
    pub strength: GameHackingGameCubeMatchStrength,
    pub requires_user_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeIndexPage {
    pub page_number: u32,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub sha256: String,
    pub game_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeIndexRecord {
    pub game_id: u64,
    pub title: String,
    pub dolphin_game_id: Option<String>,
    pub region: Option<String>,
    pub revision: Option<u16>,
    pub hash: Option<String>,
    pub source_url: String,
    pub index_source_url: String,
    pub retrieved_at_unix_seconds: u64,
}

impl GameHackingGameCubeIndexRecord {
    fn as_game(&self) -> GameHackingGameCubeGame {
        GameHackingGameCubeGame {
            game_id: self.game_id,
            title: self.title.clone(),
            system: "GameCube".to_string(),
            region: self.region.clone(),
            dolphin_game_id: self.dolphin_game_id.clone(),
            revision: self.revision,
            hash: self.hash.clone(),
            source_url: self.source_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameHackingGameCubeCatalogue {
    pub schema_version: u32,
    pub provider: String,
    pub system: String,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub pages: Vec<GameHackingGameCubeIndexPage>,
    pub games: Vec<GameHackingGameCubeIndexRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GameHackingGameCubeIndexRefreshResult {
    pub catalogue_path: PathBuf,
    pub pages_total: usize,
    pub pages_downloaded: usize,
    pub pages_reused: usize,
    pub games: usize,
    pub retrieved_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHackingGameCubeIndexProgress {
    pub pages_complete: usize,
    pub pages_total: usize,
    pub page_number: Option<u32>,
    pub downloaded: bool,
    pub games_collected: usize,
}

#[derive(Debug, Clone)]
pub struct GameHackingGameCubeFetchOptions {
    pub cache_root: PathBuf,
    pub force_refresh: bool,
    pub delay: Duration,
    pub cancellation: Option<std::sync::Arc<AtomicBool>>,
}

impl GameHackingGameCubeFetchOptions {
    pub fn defaults() -> Result<Self, GameHackingError> {
        Ok(Self {
            cache_root: gamehacking_cache_root()?,
            force_refresh: false,
            delay: Duration::from_secs(3),
            cancellation: None,
        })
    }
}

/// GameHacking.org's system adapter for GameCube. `system_id` is the
/// numeric `sysID` form field required only for per-game cheat exports
/// (see the module doc comment for why it is not yet confirmed).
#[derive(Debug, Clone, Copy, Default)]
pub struct GameCubeGameHackingAdapter;

impl GameCubeGameHackingAdapter {
    pub fn system_name(&self) -> &'static str {
        "GameCube"
    }

    /// `None` until confirmed via a real request from an unrestricted
    /// network. Catalogue crawling and matching never call this.
    pub fn system_id(&self) -> Option<u16> {
        None
    }

    pub fn index_url(&self) -> &'static str {
        GAMECUBE_INDEX_URL
    }

    pub fn export_format(&self) -> &'static str {
        "Dolphin"
    }

    pub fn supports(&self, identity: &GameCubeGameIdentity) -> bool {
        identity.verified_game_id().is_some()
    }
}

// --- Transport --------------------------------------------------------

trait GameCubeGameHackingTransport {
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
struct UreqGameCubeGameHackingTransport {
    agent: ureq::Agent,
}

impl UreqGameCubeGameHackingTransport {
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
            return Err(gamecube_error(
                GameHackingErrorKind::AccessDenied,
                format!("GameHacking.org denied access (HTTP {status})"),
            ));
        }
        if status == 429 {
            return Err(gamecube_error(
                GameHackingErrorKind::RateLimited,
                "GameHacking.org asked ArchiveFS to slow down (HTTP 429)",
            ));
        }
        if (500..600).contains(&status) {
            return Err(gamecube_error(
                GameHackingErrorKind::TemporaryFailure,
                format!("GameHacking.org is temporarily unavailable (HTTP {status})"),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(gamecube_error(
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
                gamecube_error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org response could not be read: {failure}"),
                )
            })?;
        if bytes.len() > maximum_bytes {
            return Err(gamecube_error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org response exceeded the bounded size limit",
            ));
        }
        Ok(ProviderResponse { bytes, charset })
    }
}

impl GameCubeGameHackingTransport for UreqGameCubeGameHackingTransport {
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

fn gamecube_error(kind: GameHackingErrorKind, detail: impl Into<String>) -> GameHackingError {
    GameHackingError {
        kind,
        detail: detail.into(),
    }
}

fn validate_provider_url(value: &str) -> Result<(), GameHackingError> {
    let url = Url::parse(value).map_err(|_| {
        gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is invalid",
        )
    })?;
    if url.scheme() != "https" || url.host_str() != Some("gamehacking.org") {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "provider URL is outside the fixed GameHacking.org HTTPS origin",
        ));
    }
    Ok(())
}

fn classify_transport_error(failure: ureq::Error) -> GameHackingError {
    gamecube_error(
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

pub struct GameHackingGameCubeProvider {
    adapter: GameCubeGameHackingAdapter,
    transport: UreqGameCubeGameHackingTransport,
}

impl Default for GameHackingGameCubeProvider {
    fn default() -> Self {
        Self {
            adapter: GameCubeGameHackingAdapter,
            transport: UreqGameCubeGameHackingTransport::new(),
        }
    }
}

impl GameHackingGameCubeProvider {
    pub fn match_game(
        &self,
        identity: &GameCubeGameIdentity,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<GameHackingGameCubeMatch, GameHackingError> {
        if !self.adapter.supports(identity) {
            return Ok(GameHackingGameCubeMatch {
                status: GameHackingGameCubeMatchStatus::IdentityIncomplete,
                game: None,
                candidates: Vec::new(),
                detail: "A verified local Dolphin Game ID is required before checking the cached GameHacking.org GameCube catalogue.".to_string(),
            });
        }
        let catalogue = load_gamecube_catalogue(&options.cache_root)?;
        let mut candidates = match_gamecube_catalogue(identity, &catalogue);
        if candidates.is_empty() {
            return Ok(GameHackingGameCubeMatch {
                status: GameHackingGameCubeMatchStatus::NoMatch,
                game: None,
                candidates: Vec::new(),
                detail: "No Game ID, hash, or normalized-title+region match exists in the cached GameHacking.org GameCube catalogue.".to_string(),
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
            return Ok(GameHackingGameCubeMatch {
                status: GameHackingGameCubeMatchStatus::Matched,
                detail: format!(
                    "Matched {} by {} from the cached GameCube catalogue.",
                    selected.game.title,
                    selected.strength.label()
                ),
                game: Some(selected.game),
                candidates: Vec::new(),
            });
        }
        Ok(GameHackingGameCubeMatch {
            status: GameHackingGameCubeMatchStatus::Candidates,
            game: None,
            detail: if best_priority
                == GameHackingGameCubeMatchStrength::NormalizedTitleAndRegion.priority()
            {
                "Only normalized-title candidates were found. Confirm the correct GameHacking.org game before requesting its export.".to_string()
            } else {
                "More than one equally strong identity match was found. Confirm the correct GameHacking.org game before requesting its export.".to_string()
            },
            candidates,
        })
    }

    pub fn refresh_gamecube_index<F>(
        &self,
        options: &GameHackingGameCubeFetchOptions,
        mut progress: F,
    ) -> Result<GameHackingGameCubeIndexRefreshResult, GameHackingError>
    where
        F: FnMut(GameHackingGameCubeIndexProgress),
    {
        self.check_robots(options, &["/system/ngc/all"])?;
        prepare_cache(&options.cache_root)?;
        let root_path = options.cache_root.join(GAMECUBE_INDEX_ROOT_CACHE_FILE);
        let root_was_cached = root_path.is_file();
        let resume_options = GameHackingGameCubeFetchOptions {
            cache_root: options.cache_root.clone(),
            force_refresh: false,
            delay: Duration::from_secs(2),
            cancellation: options.cancellation.clone(),
        };
        let root = self.cached_request(
            GAMECUBE_INDEX_ROOT_CACHE_FILE,
            MAX_INDEX_BYTES,
            &resume_options,
            |transport| transport.get(self.adapter.index_url(), MAX_INDEX_BYTES),
        )?;
        let page_numbers = parse_gamecube_index_page_numbers(&root.bytes, root.charset.as_deref())?;
        if page_numbers.len() > MAX_GAMECUBE_INDEX_PAGES {
            return Err(gamecube_error(
                GameHackingErrorKind::InvalidResponse,
                "GameHacking.org GameCube index exceeded the page limit",
            ));
        }
        let mut pages = Vec::with_capacity(page_numbers.len());
        let mut games_by_id = BTreeMap::<u64, GameHackingGameCubeIndexRecord>::new();
        let mut downloaded = 0usize;
        let mut reused = 0usize;
        for (position, page_number) in page_numbers.iter().copied().enumerate() {
            check_cancelled(&resume_options)?;
            let url = format!("{}/{}", self.adapter.index_url(), page_number);
            let cache_name = format!("gamecube-index-page-{page_number}.html");
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
            let mut page_games = parse_gamecube_index_page(
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
            pages.push(GameHackingGameCubeIndexPage {
                page_number,
                source_url: page_source_url,
                retrieved_at_unix_seconds: retrieved_at,
                sha256: sha256_hex(&response.bytes),
                game_count: page_games.len(),
            });
            progress(GameHackingGameCubeIndexProgress {
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
        let catalogue = GameHackingGameCubeCatalogue {
            schema_version: GAMECUBE_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_GAMECUBE_PROVIDER_ID.to_string(),
            system: self.adapter.system_name().to_string(),
            source_url: self.adapter.index_url().to_string(),
            retrieved_at_unix_seconds: retrieved_at,
            pages,
            games,
        };
        let catalogue_path = options.cache_root.join(GAMECUBE_CATALOGUE_FILE);
        let mut bytes = serde_json::to_vec_pretty(&catalogue).map_err(|failure| {
            gamecube_error(
                GameHackingErrorKind::CacheUnavailable,
                format!("GameHacking.org catalogue could not be serialized: {failure}"),
            )
        })?;
        bytes.push(b'\n');
        atomic_write(&catalogue_path, &bytes)?;
        Ok(GameHackingGameCubeIndexRefreshResult {
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
        identity: &GameCubeGameIdentity,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
        self.check_robots(options, &["/inc/sub.exportCodes.php"])?;
        authorize_gamecube_catalogue_match(identity, game, false)?;
        self.fetch_export(game, identity, options)
    }

    pub fn fetch_cheats_for_confirmed_candidate(
        &self,
        identity: &GameCubeGameIdentity,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
        authorize_gamecube_catalogue_match(identity, game, true)?;
        self.fetch_export(game, identity, options)
    }

    fn fetch_export(
        &self,
        game: &GameHackingGameCubeGame,
        identity: &GameCubeGameIdentity,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
        let Some(system_id) = self.adapter.system_id() else {
            return Err(gamecube_error(
                GameHackingErrorKind::UnsupportedSystem,
                "GameHacking.org's numeric GameCube system ID has not been confirmed yet; cheat export is disabled until GameCubeGameHackingAdapter::system_id is set from a real request.",
            ));
        };
        self.check_robots(options, &["/inc/sub.exportCodes.php"])?;
        let filename = game
            .dolphin_game_id
            .as_deref()
            .or(identity.dolphin_game_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&identity.title);
        let form = [
            ("format", self.adapter.export_format().to_string()),
            ("codID", String::new()),
            ("filename", filename.to_string()),
            ("sysID", system_id.to_string()),
            ("gamID", game.game_id.to_string()),
            ("download", "true".to_string()),
        ];
        let cache_name = format!("export-{}.txt", game.game_id);
        let bytes = self.cached_request(&cache_name, MAX_EXPORT_BYTES, options, |transport| {
            transport.post_form(EXPORT_URL, &form, MAX_EXPORT_BYTES)
        })?;
        parse_gamehacking_gamecube_export(game, &bytes.bytes)
    }

    fn cached_request<F>(
        &self,
        file_name: &str,
        maximum_bytes: usize,
        options: &GameHackingGameCubeFetchOptions,
        request: F,
    ) -> Result<ProviderResponse, GameHackingError>
    where
        F: Fn(&UreqGameCubeGameHackingTransport) -> Result<ProviderResponse, GameHackingError>,
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
            gamecube_error(
                GameHackingErrorKind::TemporaryFailure,
                "GameHacking.org retry limit reached",
            )
        }))
    }

    fn check_robots(
        &self,
        options: &GameHackingGameCubeFetchOptions,
        paths: &[&str],
    ) -> Result<(), GameHackingError> {
        let robots = self.cached_request("robots.txt", 256 * 1024, options, |transport| {
            transport.get(ROBOTS_URL, 256 * 1024)
        })?;
        let text = decode_provider_text(&robots.bytes, robots.charset.as_deref());
        for path in paths {
            if robots_disallows_archivefs(&text, path) {
                return Err(gamecube_error(
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

fn authorize_gamecube_catalogue_match(
    identity: &GameCubeGameIdentity,
    game: &GameHackingGameCubeGame,
    user_confirmed: bool,
) -> Result<GameHackingGameCubeMatchStrength, GameHackingError> {
    let strength = classify_gamecube_catalogue_match(identity, game).ok_or_else(|| {
        gamecube_error(
            GameHackingErrorKind::IdentityConflict,
            "selected GameHacking.org game no longer matches local Game ID, region, hash, or title",
        )
    })?;
    if strength == GameHackingGameCubeMatchStrength::NormalizedTitleAndRegion && !user_confirmed {
        return Err(gamecube_error(
            GameHackingErrorKind::IdentityConflict,
            "normalized-title-only GameHacking.org candidate requires explicit user confirmation",
        ));
    }
    Ok(strength)
}

fn match_gamecube_catalogue(
    identity: &GameCubeGameIdentity,
    catalogue: &GameHackingGameCubeCatalogue,
) -> Vec<GameHackingGameCubeMatchCandidate> {
    catalogue
        .games
        .iter()
        .filter_map(|record| {
            let game = record.as_game();
            let strength = classify_gamecube_catalogue_match(identity, &game)?;
            Some(GameHackingGameCubeMatchCandidate {
                game,
                strength,
                requires_user_confirmation: strength
                    == GameHackingGameCubeMatchStrength::NormalizedTitleAndRegion,
            })
        })
        .collect()
}

/// Classifies a candidate match in the exact required priority order:
/// exact Game ID + revision, exact Game ID + region, exact Game ID alone,
/// exact hash + region, then normalized title + region (always requiring
/// confirmation). A title match without an agreeing region is never
/// returned - fuzzy title-only matches are never silently accepted.
fn classify_gamecube_catalogue_match(
    identity: &GameCubeGameIdentity,
    game: &GameHackingGameCubeGame,
) -> Option<GameHackingGameCubeMatchStrength> {
    let local_id = identity
        .verified_game_id()
        .and_then(normalize_gamecube_game_id);
    let remote_id = game
        .dolphin_game_id
        .as_deref()
        .and_then(normalize_gamecube_game_id);
    let id_matches = local_id.is_some() && local_id == remote_id;
    let regions_match = identity
        .region
        .as_deref()
        .and_then(region_family_from_code)
        .zip(game.region.as_deref().and_then(gamehacking_region_family))
        .is_some_and(|(local, remote)| local == remote);
    if id_matches {
        // GameHacking.org's per-system listing has never been confirmed to
        // expose a per-game revision at all; this tier only fires if a
        // future catalogue actually carries one - it is never fabricated.
        let revisions_match = identity
            .revision
            .zip(game.revision)
            .is_some_and(|(local, remote)| local == remote);
        if revisions_match {
            return Some(GameHackingGameCubeMatchStrength::ExactGameIdAndRevision);
        }
        if regions_match {
            return Some(GameHackingGameCubeMatchStrength::ExactGameIdAndRegion);
        }
        return Some(GameHackingGameCubeMatchStrength::ExactGameId);
    }
    let hash_matches = identity
        .loose_rom_sha256
        .as_deref()
        .zip(game.hash.as_deref())
        .is_some_and(|(local, remote)| local.eq_ignore_ascii_case(remote));
    if hash_matches && regions_match {
        return Some(GameHackingGameCubeMatchStrength::ExactHashAndRegion);
    }
    if regions_match && normalized_title(&identity.title) == normalized_title(&game.title) {
        return Some(GameHackingGameCubeMatchStrength::NormalizedTitleAndRegion);
    }
    None
}

fn decode_provider_text<'a>(bytes: &'a [u8], charset: Option<&str>) -> std::borrow::Cow<'a, str> {
    if charset.is_none_or(|value| value.eq_ignore_ascii_case("utf-8")) {
        return String::from_utf8_lossy(bytes);
    }
    String::from_utf8_lossy(bytes)
}

fn parse_gamecube_index_page_numbers(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<u32>, GameHackingError> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let selector = Selector::parse("a[href^='/system/ngc/all/']").expect("static selector");
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
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube root index contained no numbered pages",
        ));
    }
    let pages = pages.into_iter().collect::<Vec<_>>();
    let expected_len = u32::try_from(pages.len()).map_err(|_| {
        gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube index page count is invalid",
        )
    })?;
    if pages.first() != Some(&0) || pages.iter().copied().ne(0..expected_len) {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube index pagination is incomplete",
        ));
    }
    Ok(pages)
}

pub fn parse_gamehacking_gamecube_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
) -> Result<Vec<GameHackingGameCubeIndexRecord>, GameHackingError> {
    parse_gamecube_index_page(source_url, retrieved_at_unix_seconds, bytes, None)
}

fn parse_gamecube_index_page(
    source_url: &str,
    retrieved_at_unix_seconds: u64,
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<GameHackingGameCubeIndexRecord>, GameHackingError> {
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let row_selector = Selector::parse("tr").expect("static selector");
    let cell_selector = Selector::parse("th, td").expect("static selector");
    let game_selector = Selector::parse("a[href^='/game/']").expect("static selector");
    let mut current_title = None::<String>;
    let mut games = BTreeMap::<u64, GameHackingGameCubeIndexRecord>::new();
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
        let dolphin_game_id = cells
            .iter()
            .find(|cell| normalize_gamecube_game_id(cell).is_some())
            .cloned();
        let hash = cells.iter().find(|cell| is_hash_like(cell)).cloned();
        let revision = cells.iter().find_map(|cell| parse_revision_cell(cell));
        let region = (!link_label.is_empty()).then_some(link_label);
        let source = if href.starts_with("https://") {
            href
        } else {
            format!("{BASE_URL}{href}")
        };
        games
            .entry(game_id)
            .or_insert(GameHackingGameCubeIndexRecord {
                game_id,
                title,
                dolphin_game_id,
                region,
                revision,
                hash,
                source_url: source,
                index_source_url: source_url.to_string(),
                retrieved_at_unix_seconds,
            });
    }
    if games.is_empty() {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org GameCube index page contained no game rows: {source_url}"),
        ));
    }
    Ok(games.into_values().collect())
}

fn is_hash_like(value: &str) -> bool {
    let value = value.trim();
    matches!(value.len(), 32 | 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Opportunistically reads a `Rev N`/`Revision N` cell, if the catalogue
/// listing happens to carry one (not confirmed to exist in practice - see
/// the module doc comment). Never guessed from a bare number alone.
fn parse_revision_cell(value: &str) -> Option<u16> {
    let lower = value.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("revision")
        .or_else(|| lower.strip_prefix("rev"))?;
    rest.trim_start_matches('.').trim().parse::<u16>().ok()
}

// --- Cheat export parsing -----------------------------------------------

pub fn parse_gamehacking_gamecube_export(
    game: &GameHackingGameCubeGame,
    bytes: &[u8],
) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube export is not UTF-8",
        )
    })?;
    let mut cheats = Vec::new();
    let mut pending = PendingGameCubeCheat::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if let Some(section) = gamecube_section_title(trimmed) {
            flush_pending_gamecube_cheat(game, &mut pending, &mut cheats);
            pending.name = Some(section);
            continue;
        }
        if let Some(value) = strip_assignment(trimmed, "encryption")
            .or_else(|| strip_assignment(trimmed, "format"))
            .or_else(|| strip_label(trimmed, "encryption"))
            .or_else(|| strip_label(trimmed, "format"))
        {
            pending.declared_format = Some(classify_declared_format(value));
            continue;
        }
        if let Some(value) = strip_assignment(trimmed, "author") {
            pending.author = nonempty_decoded(value);
            continue;
        }
        if let Some(value) = strip_assignment(trimmed, "description")
            .or_else(|| strip_assignment(trimmed, "note"))
            .or_else(|| strip_assignment(trimmed, "notes"))
        {
            if let Some(value) = nonempty_decoded(value) {
                pending.description.push(value);
            }
            continue;
        }
        if looks_like_gamecube_code_line(trimmed) {
            pending.code_lines.push(trimmed.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if !pending.code_lines.is_empty() {
                flush_pending_gamecube_cheat(game, &mut pending, &mut cheats);
            }
            continue;
        }
        if !trimmed.starts_with("//") {
            pending.description.push(decode_html_text(trimmed));
        }
    }
    flush_pending_gamecube_cheat(game, &mut pending, &mut cheats);
    if cheats.is_empty() {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org export contained no recognisable GameCube code lines",
        ));
    }
    Ok(cheats)
}

#[derive(Debug, Default)]
struct PendingGameCubeCheat {
    name: Option<String>,
    author: Option<String>,
    description: Vec<String>,
    code_lines: Vec<String>,
    declared_format: Option<GameCubeCodeFormat>,
}

fn flush_pending_gamecube_cheat(
    game: &GameHackingGameCubeGame,
    pending: &mut PendingGameCubeCheat,
    cheats: &mut Vec<GameHackingGameCubeCheat>,
) {
    if pending.code_lines.is_empty() {
        *pending = PendingGameCubeCheat::default();
        return;
    }
    let all_lines_well_formed = pending
        .code_lines
        .iter()
        .all(|line| valid_gamecube_code_line(line));
    let code_format = if !all_lines_well_formed {
        GameCubeCodeFormat::Unsupported
    } else {
        pending
            .declared_format
            .unwrap_or(GameCubeCodeFormat::RawUnknown)
    };
    let index = cheats.len() + 1;
    let name = pending
        .name
        .take()
        .unwrap_or_else(|| format!("Cheat {index}"));
    cheats.push(GameHackingGameCubeCheat {
        id: format!("gh-gc-{}-{index}", game.game_id),
        name,
        author: pending.author.take(),
        description: normalized_description(std::mem::take(&mut pending.description)),
        code_format,
        code_lines: std::mem::take(&mut pending.code_lines),
        source_game_id: game.game_id,
        source_url: game.source_url.clone(),
    });
    *pending = PendingGameCubeCheat::default();
}

fn classify_declared_format(value: &str) -> GameCubeCodeFormat {
    let lower = value.trim().to_ascii_lowercase();
    if lower.contains("action replay") || lower == "ar" || lower.contains("actionreplay") {
        GameCubeCodeFormat::ActionReplay
    } else if lower.contains("gecko") {
        GameCubeCodeFormat::Gecko
    } else {
        GameCubeCodeFormat::RawUnknown
    }
}

/// A GameCube Action Replay/Gecko code line: exactly two whitespace
/// separated 8-hex-digit groups. This shape is shared by both formats -
/// only an explicit label (see `classify_declared_format`) distinguishes
/// them; the hex shape alone is never used to guess.
fn valid_gamecube_code_line(value: &str) -> bool {
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    tokens.len() == 2
        && tokens
            .iter()
            .all(|token| token.len() == 8 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A looser shape check used only to decide whether a line was *attempted*
/// as a code line (so a malformed one still becomes an `Unsupported`
/// cheat instead of being silently swallowed as description text): two
/// whitespace-separated non-empty hex tokens, regardless of length.
fn looks_like_gamecube_code_line(value: &str) -> bool {
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    tokens.len() == 2
        && tokens
            .iter()
            .all(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn gamecube_section_title(line: &str) -> Option<String> {
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

fn strip_label<'a>(value: &'a str, label: &str) -> Option<&'a str> {
    let (head, tail) = value.split_once(':')?;
    head.trim()
        .eq_ignore_ascii_case(label)
        .then_some(tail.trim())
        .filter(|tail| !tail.is_empty())
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

// --- Cache helpers -------------------------------------------------------

fn prepare_cache(root: &Path) -> Result<(), GameHackingError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(gamecube_error(
            GameHackingErrorKind::CacheUnavailable,
            "GameHacking.org cache root must be an absolute non-root path",
        ));
    }
    fs::create_dir_all(root).map_err(|failure| {
        gamecube_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("GameHacking.org cache could not be created: {failure}"),
        )
    })
}

pub fn load_gamecube_catalogue(
    root: &Path,
) -> Result<GameHackingGameCubeCatalogue, GameHackingError> {
    let path = root.join(GAMECUBE_CATALOGUE_FILE);
    let bytes = bounded_read(&path, 32 * 1024 * 1024).map_err(|failure| {
        gamecube_error(
            failure.kind,
            format!(
                "GameHacking.org GameCube catalogue is unavailable; run `archivefs-cli gamehacking-gamecube-index-refresh` first: {}",
                failure.detail
            ),
        )
    })?;
    let catalogue: GameHackingGameCubeCatalogue =
        serde_json::from_slice(&bytes).map_err(|failure| {
            gamecube_error(
                GameHackingErrorKind::InvalidResponse,
                format!("GameHacking.org GameCube catalogue is invalid: {failure}"),
            )
        })?;
    if catalogue.schema_version != GAMECUBE_CATALOGUE_SCHEMA_VERSION
        || catalogue.provider != GAMEHACKING_GAMECUBE_PROVIDER_ID
        || !catalogue.system.eq_ignore_ascii_case("GameCube")
    {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube catalogue metadata is unsupported",
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
            gamecube_error(
                GameHackingErrorKind::CacheUnavailable,
                format!("cached retrieval date is unavailable: {}", path.display()),
            )
        })
}

fn bounded_read(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, GameHackingError> {
    let metadata = path.symlink_metadata().map_err(|failure| {
        gamecube_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("cached provider response could not be inspected: {failure}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum_bytes as u64
    {
        return Err(gamecube_error(
            GameHackingErrorKind::CacheUnavailable,
            "cached provider response is unsafe or oversized",
        ));
    }
    fs::read(path).map_err(|failure| {
        gamecube_error(
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
        gamecube_error(
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
        return Err(gamecube_error(
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
    let timestamp = unix_seconds_now();
    fs::write(root.join("last-request"), timestamp.to_string()).map_err(|failure| {
        gamecube_error(
            GameHackingErrorKind::CacheUnavailable,
            format!("provider rate-limit marker could not be written: {failure}"),
        )
    })
}

fn check_cancelled(options: &GameHackingGameCubeFetchOptions) -> Result<(), GameHackingError> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return Err(gamecube_error(
            GameHackingErrorKind::Cancelled,
            "GameHacking.org request was cancelled",
        ));
    }
    Ok(())
}

fn cancellable_delay(
    options: &GameHackingGameCubeFetchOptions,
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
    use crate::game_identity::{
        IdentityConfidence, IdentityEvidence, IdentityImageFormat, IdentityProvenance,
    };

    fn evidence(
        kind: IdentityKind,
        status: IdentityStatus,
        value: Option<&str>,
    ) -> IdentityEvidence {
        IdentityEvidence {
            kind,
            status,
            value: value.map(str::to_string),
            confidence: IdentityConfidence::ExactBytes,
            provenance: IdentityProvenance {
                archive_path: PathBuf::from("/games/game.iso"),
                member_path: None,
                member_index: None,
                method: "test fixture".to_string(),
            },
            diagnostic: "test fixture".to_string(),
        }
    }

    fn report(game_id: &str, region: &str, revision: u16) -> GameIdentityReport {
        GameIdentityReport {
            archive_path: PathBuf::from("/games/game.iso"),
            platform: IdentityPlatform::GameCube,
            format: IdentityImageFormat::Iso,
            evidence: vec![
                evidence(
                    IdentityKind::DolphinGameId,
                    IdentityStatus::Verified,
                    Some(game_id),
                ),
                evidence(
                    IdentityKind::DolphinRegion,
                    IdentityStatus::Verified,
                    Some(region),
                ),
                evidence(
                    IdentityKind::DolphinRevision,
                    IdentityStatus::Verified,
                    Some(&revision.to_string()),
                ),
            ],
            warnings: Vec::new(),
            bytes_read: 32,
            archive_members_inspected: 0,
            metadata_paths_inspected: 0,
            nested_container_depth: 0,
            complete: true,
        }
    }

    fn identity(game_id: &str, region: &str, revision: u16) -> GameCubeGameIdentity {
        GameCubeGameIdentity::from_report("Fixture Game", &report(game_id, region, revision))
    }

    fn game(game_id: u64, dolphin_id: &str, region: &str, title: &str) -> GameHackingGameCubeGame {
        GameHackingGameCubeGame {
            game_id,
            title: title.to_string(),
            system: "GameCube".to_string(),
            region: Some(region.to_string()),
            dolphin_game_id: Some(dolphin_id.to_string()),
            revision: None,
            hash: None,
            source_url: format!("https://gamehacking.org/game/{game_id}"),
        }
    }

    #[test]
    fn verified_game_id_requires_gamecube_platform_and_verified_status() {
        let verified = identity("GM8E01", "E", 0);
        assert_eq!(verified.verified_game_id(), Some("GM8E01"));
        assert_eq!(verified.state, GameCubeIdentityState::Verified);
    }

    /// GameHacking.org's confirmed system slug for GameCube is `ngc`, not
    /// `gamecube` - a wrong slug here silently 404s (or matches the wrong
    /// system) instead of failing loudly, so this is pinned exactly.
    #[test]
    fn gamecube_adapter_index_url_uses_the_confirmed_ngc_slug() {
        let adapter = GameCubeGameHackingAdapter;
        assert_eq!(
            adapter.index_url(),
            "https://gamehacking.org/system/ngc/all"
        );
    }

    #[test]
    fn normalize_gamecube_game_id_requires_exact_six_char_alnum_shape() {
        assert_eq!(
            normalize_gamecube_game_id("gm8e01"),
            Some("GM8E01".to_string())
        );
        assert!(normalize_gamecube_game_id("GM8E0").is_none());
        assert!(normalize_gamecube_game_id("GM8E-1").is_none());
    }

    #[test]
    fn exact_game_id_and_region_outranks_bare_game_id() {
        let local = identity("GM8E01", "E", 0);
        let remote = game(1, "GM8E01", "USA", "Fixture Game");
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::ExactGameIdAndRegion)
        );
    }

    #[test]
    fn exact_game_id_and_revision_outranks_game_id_and_region() {
        let local = identity("GM8E01", "E", 2);
        let mut remote = game(1, "GM8E01", "USA", "Fixture Game");
        remote.revision = Some(2);
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::ExactGameIdAndRevision)
        );
    }

    #[test]
    fn revision_mismatch_falls_back_to_game_id_and_region() {
        let local = identity("GM8E01", "E", 1);
        let mut remote = game(1, "GM8E01", "USA", "Fixture Game");
        remote.revision = Some(2);
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::ExactGameIdAndRegion)
        );
    }

    #[test]
    fn region_mismatch_still_matches_on_game_id_alone() {
        let local = identity("GM8E01", "E", 0);
        let remote = game(1, "GM8E01", "Japan", "Fixture Game");
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::ExactGameId)
        );
    }

    #[test]
    fn different_game_id_falls_back_to_hash_and_region() {
        let mut local = identity("GM8E01", "E", 0);
        local.dolphin_game_id = None;
        local.state = GameCubeIdentityState::MissingGameId;
        local.loose_rom_sha256 = Some("a".repeat(64));
        let mut remote = game(1, "ZZZZZZ", "USA", "Fixture Game");
        remote.hash = Some("A".repeat(64));
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::ExactHashAndRegion)
        );
    }

    #[test]
    fn ambiguous_title_candidate_requires_region_agreement_and_confirmation() {
        let mut local = identity("GM8E01", "E", 0);
        local.dolphin_game_id = None;
        local.state = GameCubeIdentityState::MissingGameId;
        let remote = game(1, "ZZZZZZ", "USA", "Fixture Game");
        assert_eq!(
            classify_gamecube_catalogue_match(&local, &remote),
            Some(GameHackingGameCubeMatchStrength::NormalizedTitleAndRegion)
        );
        let candidates = match_gamecube_catalogue(
            &local,
            &GameHackingGameCubeCatalogue {
                schema_version: GAMECUBE_CATALOGUE_SCHEMA_VERSION,
                provider: GAMEHACKING_GAMECUBE_PROVIDER_ID.to_string(),
                system: "GameCube".to_string(),
                source_url: GAMECUBE_INDEX_URL.to_string(),
                retrieved_at_unix_seconds: 0,
                pages: Vec::new(),
                games: vec![GameHackingGameCubeIndexRecord {
                    game_id: 1,
                    title: "Fixture Game".to_string(),
                    dolphin_game_id: Some("ZZZZZZ".to_string()),
                    region: Some("USA".to_string()),
                    revision: None,
                    hash: None,
                    source_url: "https://gamehacking.org/game/1".to_string(),
                    index_source_url: GAMECUBE_INDEX_URL.to_string(),
                    retrieved_at_unix_seconds: 0,
                }],
            },
        );
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].requires_user_confirmation);
        assert!(
            authorize_gamecube_catalogue_match(&local, &remote, false).is_err(),
            "an unconfirmed title-only candidate must never authorize an export request"
        );
        assert!(authorize_gamecube_catalogue_match(&local, &remote, true).is_ok());
    }

    #[test]
    fn title_only_without_region_agreement_never_matches() {
        let mut local = identity("GM8E01", "E", 0);
        local.dolphin_game_id = None;
        local.state = GameCubeIdentityState::MissingGameId;
        let remote = game(1, "ZZZZZZ", "Japan", "Fixture Game");
        assert_eq!(classify_gamecube_catalogue_match(&local, &remote), None);
    }

    #[test]
    fn index_page_parses_game_id_region_and_title() {
        let html = format!(
            r#"<table>
<tr><td>Test Racer</td></tr>
<tr><td><a href="/game/501/test-racer">USA</a></td><td>GTRE01</td><td>15</td></tr>
</table>
<a href="{GAMECUBE_INDEX_URL}/0">0</a>"#
        );
        let records = parse_gamehacking_gamecube_index_page(
            GAMECUBE_INDEX_URL,
            1_700_000_000,
            html.as_bytes(),
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.game_id, 501);
        assert_eq!(record.title, "Test Racer");
        assert_eq!(record.dolphin_game_id.as_deref(), Some("GTRE01"));
        assert_eq!(record.region.as_deref(), Some("USA"));
        assert_eq!(
            record.source_url,
            "https://gamehacking.org/game/501/test-racer"
        );
    }

    #[test]
    fn index_page_numbers_require_a_complete_zero_based_run() {
        let html = r#"<a href="/system/ngc/all/0">0</a><a href="/system/ngc/all/1">1</a><a href="/system/ngc/all/2">2</a>"#;
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages, vec![0, 1, 2]);

        let incomplete = r#"<a href="/system/ngc/all/0">0</a><a href="/system/ngc/all/2">2</a>"#;
        assert!(parse_gamecube_index_page_numbers(incomplete.as_bytes(), None).is_err());
    }

    #[test]
    fn named_cheat_export_preserves_author_and_description() {
        let fixture_game = game(501, "GTRE01", "USA", "Test Racer");
        let export = b"[Codes\\Infinite Boost]\nauthor=Ada\ndescription=Boost never runs out\nEncryption: Action Replay\n04001234 00000001\n\n[Codes\\Unlock All Tracks]\nEncryption: Gecko\nC21F8B51 00000004\n60000000 00000000\n\n";
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert_eq!(cheats.len(), 2);
        assert_eq!(cheats[0].name, "Codes › Infinite Boost");
        assert_eq!(cheats[0].author.as_deref(), Some("Ada"));
        assert_eq!(
            cheats[0].description.as_deref(),
            Some("Boost never runs out")
        );
        assert_eq!(cheats[0].code_format, GameCubeCodeFormat::ActionReplay);
        assert_eq!(cheats[0].code_lines, vec!["04001234 00000001".to_string()]);
        assert_eq!(cheats[1].name, "Codes › Unlock All Tracks");
        assert_eq!(cheats[1].code_format, GameCubeCodeFormat::Gecko);
        assert_eq!(cheats[1].code_lines.len(), 2);
    }

    #[test]
    fn undeclared_format_is_raw_unknown_not_guessed() {
        let fixture_game = game(501, "GTRE01", "USA", "Test Racer");
        let export = b"[Codes\\Mystery Code]\n04001234 00000001\n";
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert_eq!(cheats.len(), 1);
        assert_eq!(cheats[0].code_format, GameCubeCodeFormat::RawUnknown);
    }

    #[test]
    fn malformed_code_line_is_unsupported() {
        let fixture_game = game(501, "GTRE01", "USA", "Test Racer");
        let export = b"[Codes\\Broken Code]\nEncryption: Gecko\n04001234 0001\n";
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert_eq!(cheats.len(), 1);
        assert_eq!(cheats[0].code_format, GameCubeCodeFormat::Unsupported);
    }

    #[test]
    fn resume_reuses_cached_pages_with_no_further_network_activity() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamecube-gamehacking-resume-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let index_html = r#"<a href="/system/ngc/all/0">0</a><table>
<tr><td>Test Racer</td></tr>
<tr><td><a href="/game/501/test-racer">USA</a></td><td>GTRE01</td></tr>
</table>"#;
        fs::write(
            root.join(GAMECUBE_INDEX_ROOT_CACHE_FILE),
            index_html.as_bytes(),
        )
        .unwrap();
        fs::write(root.join("gamecube-index-root.retrieved"), b"1700000000").unwrap();
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: false,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        // "robots.txt" is not cached, so a real crawl would need the
        // network here too; instead, pre-seed an allow-all robots.txt to
        // prove the whole refresh completes from cache alone with zero
        // real network calls (the fake transport is never exercised
        // because `cached_request` short-circuits on the cache hit before
        // ever calling `request`).
        fs::write(root.join("robots.txt"), b"User-agent: *\nAllow: /\n").unwrap();
        let result = provider
            .refresh_gamecube_index(&options, |_| {})
            .expect("a fully cached crawl must succeed without any network access");
        assert_eq!(result.pages_downloaded, 0);
        assert_eq!(result.pages_reused, 1);
        assert_eq!(result.games, 1);
        let catalogue = load_gamecube_catalogue(&root).unwrap();
        assert_eq!(
            catalogue.games[0].dolphin_game_id.as_deref(),
            Some("GTRE01")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn catalogue_output_is_deterministically_sorted() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamecube-gamehacking-sorted-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let index_html = r#"<a href="/system/ngc/all/0">0</a><table>
<tr><td>Zeta Game</td></tr>
<tr><td><a href="/game/900/zeta">USA</a></td><td>GZAE01</td></tr>
<tr><td>Alpha Game</td></tr>
<tr><td><a href="/game/100/alpha">USA</a></td><td>GALE01</td></tr>
</table>"#;
        fs::write(
            root.join(GAMECUBE_INDEX_ROOT_CACHE_FILE),
            index_html.as_bytes(),
        )
        .unwrap();
        fs::write(root.join("gamecube-index-root.retrieved"), b"1700000000").unwrap();
        fs::write(root.join("robots.txt"), b"User-agent: *\nAllow: /\n").unwrap();
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: false,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        provider.refresh_gamecube_index(&options, |_| {}).unwrap();
        let catalogue = load_gamecube_catalogue(&root).unwrap();
        let ids: Vec<u64> = catalogue.games.iter().map(|game| game.game_id).collect();
        assert_eq!(
            ids,
            vec![100, 900],
            "games must be sorted by (game_id, title)"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
