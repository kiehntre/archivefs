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

use scraper::{Element, Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::gamehacking_provider::{
    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE, GameHackingError, GameHackingErrorKind,
    GameHackingFetchOutcome, GameHackingHttpClassification, blocked_without_cache_message,
    cached_bytes_are_cloudflare_challenge, classify_gamehacking_http_response,
    classify_gamehacking_transport_error, clear_cloudflare_marker, cloudflare_cooldown_remaining,
    gamehacking_cache_root, mark_cloudflare_blocked,
};
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

/// The result of inspecting one real game page's cheat-export form: what
/// the CLI diagnostic command prints, and the only path by which
/// `GameCubeGameHackingAdapter::system_id` is ever set - never guessed.
#[derive(Debug, Clone, Serialize)]
pub struct GameCubeSysIdDiagnostics {
    pub game_id: u64,
    pub title: String,
    pub game_page_url: String,
    pub export_form_action: String,
    pub hidden_fields: Vec<(String, String)>,
    /// `None` if the page's export form has no `sysID` hidden field, or
    /// its value does not parse as a `u16`.
    pub sys_id: Option<u16>,
}

/// A returned cheat's identified raw code format. Never inferred from the
/// hex shape alone - only an explicit label in the exported text (an
/// `Encryption:`/`Format:` field) promotes a cheat to `ActionReplay` or
/// `Gecko`; ArchiveFS never speculatively converts between the two.
///
/// ## Why the hex shape alone can never decide this (code-format audit)
///
/// Dolphin's own source (`Source/Core/Core/ActionReplay.cpp`,
/// `GeckoCode.cpp`, upstream `dolphin-emu/dolphin`) was inspected as the
/// authority, per the audit this type's design is based on:
///
/// - Dolphin's `GameSettings/<ID>.ini` distinguishes the two formats
///   *only* by which INI section a code was declared under - `[Gecko]`
///   or `[ActionReplay]` - never by inspecting a code's own bytes. Both
///   sections use the identical `XXXXXXXX YYYYYYYY` hex-pair line shape
///   (confirmed in `gecko_document.rs`'s own `GeckoCode`/
///   `MalformedLine` check, which applies the same shape rule Dolphin
///   itself expects).
/// - `ActionReplay.cpp`'s `ARAddr` bit-packs the first word as
///   `subtype:2 | type:3 | size:2 | gcaddr:25` and interprets *every*
///   32-bit value fed to it under that scheme - there is no byte pattern
///   AR's own decoder rejects outright. A first word like `0xC0000000`
///   (which some GameCube cheat archives use for Gecko-specific
///   conditional/loop constructs) decodes under AR's own bit layout to a
///   *defined, non-error* AR subtype (`SUB_MASTER_CODE`) - the same bytes
///   are not exclusively one format or the other; they are simply
///   interpreted differently depending on which engine runs them.
/// - `GeckoCode.cpp` never semantically decodes a Gecko code's opcode at
///   all; Dolphin only copies the raw bytes into memory and installs a
///   separate, non-Dolphin-source "codehandler" that interprets them on
///   the (emulated) CPU at runtime. Dolphin's own codebase therefore
///   contains zero opcode-based Gecko-vs-AR disambiguation logic to
///   mirror here.
///
/// Sampling ten real GameHacking.org GameCube exports (Luigi's Mansion,
/// Eternal Darkness, and eight more) found zero instances of an explicit
/// `Encryption:`/`Format:` label anywhere - the live "Text" export format
/// is consistently a flat, unlabeled `<name> [<author>]` + code-lines
/// list. Given Dolphin itself cannot and does not distinguish the two
/// formats from content, and the exact same bytes are independently
/// valid (with different real effects) under each engine, classifying
/// by opcode shape here would be an unsafe guess, not a deterministic
/// read - exactly what this type must never do. `RawUnknown` is
/// therefore the correct, final answer for this export format, not a
/// placeholder awaiting a smarter classifier.
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

/// One cheat's code-format audit detail - what
/// `gamehacking-gamecube-code-format-audit` prints per cheat. Recomputed
/// from an already-parsed `GameHackingGameCubeCheat`; never a second
/// source of truth for the classification itself.
#[derive(Debug, Clone, Serialize)]
pub struct GameCubeCheatCodeDiagnostic {
    pub name: String,
    pub author: Option<String>,
    pub code_lines: Vec<String>,
    pub line_count: usize,
    /// The first two hex characters of each code line's first 32-bit
    /// word. Shown for human review only - never itself used to decide
    /// `code_format` (see `GameCubeCodeFormat`'s doc comment for why
    /// that would be unsafe).
    pub opcode_prefixes: Vec<String>,
    pub code_format: GameCubeCodeFormat,
    pub classification_reason: String,
}

/// Explains, in the same terms `GameCubeCodeFormat`'s doc comment
/// documents, exactly why a cheat received its classification.
pub fn diagnose_gamecube_cheat_code_format(
    cheat: &GameHackingGameCubeCheat,
) -> GameCubeCheatCodeDiagnostic {
    let opcode_prefixes = cheat
        .code_lines
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .map(|token| token.get(0..2).unwrap_or(token).to_ascii_uppercase())
        .collect();
    let classification_reason = match cheat.code_format {
        GameCubeCodeFormat::ActionReplay => {
            "an explicit label declared Action Replay - either an Encryption:/Format: \
             field in the export text itself, or (far more commonly, since the Text \
             export never carries one) the individual game page's own per-cheat label, \
             matched to this cheat by its exact code body or by normalized title+author"
                .to_string()
        }
        GameCubeCodeFormat::Gecko => {
            "an explicit label declared Gecko - either an Encryption:/Format: field in \
             the export text itself, or (far more commonly, since the Text export never \
             carries one) the individual game page's own per-cheat label, matched to \
             this cheat by its exact code body or by normalized title+author"
                .to_string()
        }
        GameCubeCodeFormat::RawUnknown => {
            "no explicit format label was present (from the export text or a matched \
             game-page entry), and Dolphin's own source \
             (ActionReplay.cpp / GeckoCode.cpp) confirms the identical raw code-line \
             shape has independently valid but different decodes under each engine's \
             own addressing scheme - never distinguishable from content alone"
                .to_string()
        }
        GameCubeCodeFormat::Unsupported => {
            "one or more lines did not match the well-formed 8-hex/8-hex code line shape"
                .to_string()
        }
    };
    GameCubeCheatCodeDiagnostic {
        name: cheat.name.clone(),
        author: cheat.author.clone(),
        code_lines: cheat.code_lines.clone(),
        line_count: cheat.code_lines.len(),
        opcode_prefixes,
        code_format: cheat.code_format,
        classification_reason,
    }
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
    pub fn as_game(&self) -> GameHackingGameCubeGame {
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
    pub cached_fallback: bool,
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
/// numeric `sysID` form field required only for per-game cheat exports.
#[derive(Debug, Clone, Copy, Default)]
pub struct GameCubeGameHackingAdapter;

impl GameCubeGameHackingAdapter {
    pub fn system_name(&self) -> &'static str {
        "GameCube"
    }

    /// Confirmed by fetching a real game page's cheat-export form
    /// (`gamehacking-gamecube-sysid-diagnostic`, see
    /// `parse_gamecube_sysid_diagnostics`) and reading its hidden `sysID`
    /// field directly - Luigi's Mansion (GameHacking game 54172)
    /// confirmed `sysID = 13`. Never guessed.
    pub fn system_id(&self) -> Option<u16> {
        Some(13)
    }

    pub fn index_url(&self) -> &'static str {
        GAMECUBE_INDEX_URL
    }

    /// Confirmed from the same real export form's `format` `<select>`:
    /// the only options are `Gecko` (.gct, a binary Dolphin container -
    /// unusable for named-cheat text parsing), `Mednafen` (.cht), and
    /// `Text` (.txt, "Plain Text"). `Text` is the only option whose
    /// export is actually parseable readable text with names, so it is
    /// the one this adapter requests.
    pub fn export_format(&self) -> &'static str {
        "Text"
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
    cached_fallback: bool,
    retrieved_at_unix_seconds: Option<u64>,
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
                gamecube_error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org response could not be read: {failure}"),
                )
            })?;
        match classify_gamehacking_http_response(status, server.as_deref(), &bytes) {
            GameHackingHttpClassification::Success => {}
            GameHackingHttpClassification::CloudflareBlocked => {
                return Err(gamecube_error(
                    GameHackingErrorKind::CloudflareBlocked,
                    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                ));
            }
            GameHackingHttpClassification::AccessDenied => {
                return Err(gamecube_error(
                    GameHackingErrorKind::AccessDenied,
                    format!("GameHacking.org denied access (HTTP {status})"),
                ));
            }
            GameHackingHttpClassification::RateLimited => {
                return Err(gamecube_error(
                    GameHackingErrorKind::RateLimited,
                    "GameHacking.org asked ArchiveFS to slow down (HTTP 429)",
                ));
            }
            GameHackingHttpClassification::ServerError => {
                return Err(gamecube_error(
                    GameHackingErrorKind::TemporaryFailure,
                    format!("GameHacking.org is temporarily unavailable (HTTP {status})"),
                ));
            }
            GameHackingHttpClassification::OtherHttpError => {
                return Err(gamecube_error(
                    GameHackingErrorKind::PermanentHttpFailure,
                    format!("GameHacking.org returned HTTP {status}"),
                ));
            }
        }
        if bytes.len() > maximum_bytes {
            return Err(gamecube_error(
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
            .map_err(classify_gamehacking_transport_error)?;
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
            .map_err(classify_gamehacking_transport_error)?;
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
        let root_options = GameHackingGameCubeFetchOptions {
            cache_root: options.cache_root.clone(),
            force_refresh: options.force_refresh,
            delay: Duration::from_secs(2),
            cancellation: options.cancellation.clone(),
        };
        let root = self.cached_request(
            GAMECUBE_INDEX_ROOT_CACHE_FILE,
            self.adapter.index_url(),
            MAX_INDEX_BYTES,
            &root_options,
            |transport| transport.get(self.adapter.index_url(), MAX_INDEX_BYTES),
        )?;
        let cached_fallback = root.cached_fallback;
        let resume_options = GameHackingGameCubeFetchOptions {
            force_refresh: false,
            ..root_options
        };
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
            cached_fallback,
        })
    }

    pub fn fetch_cheats(
        &self,
        identity: &GameCubeGameIdentity,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
        self.fetch_cheats_with_status(identity, game, options)
            .map(|outcome| outcome.data)
    }

    pub fn fetch_cheats_with_status(
        &self,
        identity: &GameCubeGameIdentity,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingGameCubeCheat>>, GameHackingError> {
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
        self.fetch_cheats_for_confirmed_candidate_with_status(identity, game, options)
            .map(|outcome| outcome.data)
    }

    pub fn fetch_cheats_for_confirmed_candidate_with_status(
        &self,
        identity: &GameCubeGameIdentity,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingGameCubeCheat>>, GameHackingError> {
        authorize_gamecube_catalogue_match(identity, game, true)?;
        self.fetch_export(game, identity, options)
    }

    fn fetch_export(
        &self,
        game: &GameHackingGameCubeGame,
        identity: &GameCubeGameIdentity,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<GameHackingFetchOutcome<Vec<GameHackingGameCubeCheat>>, GameHackingError> {
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
        let bytes = self.cached_request(
            &cache_name,
            EXPORT_URL,
            MAX_EXPORT_BYTES,
            options,
            |transport| transport.post_form(EXPORT_URL, &form, MAX_EXPORT_BYTES),
        )?;
        let mut cheats = parse_gamehacking_gamecube_export(game, &bytes.bytes)?;
        // Best-effort only: the flat Text export never carries a format
        // label at all (see the module doc comment), but the individual
        // game page does, per cheat. If the page can't be fetched for any
        // reason, the export itself has already succeeded - cheats simply
        // stay RawUnknown, exactly as if this enhancement didn't run.
        if let Ok(page_bytes) = self.fetch_game_page(game, options) {
            apply_gamecube_page_format_labels(&mut cheats, &page_bytes);
        }
        Ok(GameHackingFetchOutcome {
            data: cheats,
            cached_fallback: bytes.cached_fallback,
            retrieved_at_unix_seconds: bytes.retrieved_at_unix_seconds,
        })
    }

    /// Fetches one real game's page, purely to discover its cheat-export
    /// form's action and hidden fields (the only way `sysID` is ever
    /// confirmed - never guessed). Not part of the matching/preview flow.
    fn fetch_game_page(
        &self,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<u8>, GameHackingError> {
        self.check_robots(options, &["/game/"])?;
        validate_provider_url(&game.source_url)?;
        let source_url = game.source_url.clone();
        let cache_name = format!("game-{}.html", game.game_id);
        let response = self.cached_request(
            &cache_name,
            &source_url,
            MAX_INDEX_BYTES,
            options,
            |transport| transport.get(&source_url, MAX_INDEX_BYTES),
        )?;
        Ok(response.bytes)
    }

    /// Fetches one real game's page and parses its cheat-export form's
    /// action and hidden fields, including `sysID` if present. This is
    /// the CLI diagnostic command's entire implementation.
    pub fn diagnose_export_form(
        &self,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<GameCubeSysIdDiagnostics, GameHackingError> {
        let bytes = self.fetch_game_page(game, options)?;
        parse_gamecube_sysid_diagnostics(game.game_id, &game.title, &game.source_url, &bytes)
    }

    /// Diagnostic-only: fetches this exact catalogue game's cheat export
    /// directly. There is no identity-match ambiguity to authorize
    /// against here - the caller already chose this exact `game_id` from
    /// the cached catalogue - so this bypasses
    /// `authorize_gamecube_catalogue_match` entirely and is never used by
    /// the real preview/match flow (see `fetch_cheats`/
    /// `fetch_cheats_for_confirmed_candidate`, which both require it).
    pub fn fetch_cheats_for_diagnostic(
        &self,
        game: &GameHackingGameCubeGame,
        options: &GameHackingGameCubeFetchOptions,
    ) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
        let identity = GameCubeGameIdentity {
            archive_path: PathBuf::new(),
            title: game.title.clone(),
            dolphin_game_id: game.dolphin_game_id.clone(),
            region: game.region.clone(),
            revision: game.revision,
            loose_rom_sha256: None,
            state: GameCubeIdentityState::Verified,
            evidence: Vec::new(),
            plain_failure_reason: None,
        };
        self.fetch_export(game, &identity, options)
            .map(|outcome| outcome.data)
    }

    fn cached_request<F>(
        &self,
        file_name: &str,
        url: &str,
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
            let bytes = bounded_read(&path, maximum_bytes)?;
            if cached_bytes_are_cloudflare_challenge(&bytes) {
                return Err(gamecube_error(
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
                url,
                age
            );
            return Ok(response);
        }
        if cloudflare_cooldown_remaining(&options.cache_root).is_some() {
            if let Some(response) = cached_fallback_response(&path, maximum_bytes)? {
                log_cached_fallback(url, &path, &response);
                return Ok(response);
            }
            return Err(gamecube_error(
                GameHackingErrorKind::CloudflareBlocked,
                blocked_without_cache_message(&options.cache_root, file_name),
            ));
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
                    clear_cloudflare_marker(&options.cache_root);
                    log::info!(
                        "gamehacking request_url={} classification=success cache_fallback=false cache_write=completed",
                        url
                    );
                    return Ok(response);
                }
                Err(failure) if failure.kind == GameHackingErrorKind::CloudflareBlocked => {
                    mark_cloudflare_blocked(&options.cache_root)?;
                    log::warn!(
                        "gamehacking request_url={} status=blocked classification=cloudflare cache_write=skipped",
                        url
                    );
                    if let Some(response) = cached_fallback_response(&path, maximum_bytes)? {
                        log_cached_fallback(url, &path, &response);
                        return Ok(response);
                    }
                    return Err(gamecube_error(
                        failure.kind,
                        blocked_without_cache_message(&options.cache_root, file_name),
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
                        url,
                        failure.kind,
                        attempt + 1
                    );
                    last_error = Some(failure);
                }
                Err(failure) => {
                    log::warn!(
                        "gamehacking request_url={} classification={:?} cache_fallback=false",
                        url,
                        failure.kind
                    );
                    return Err(failure);
                }
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
        let robots =
            self.cached_request("robots.txt", ROBOTS_URL, 256 * 1024, options, |transport| {
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

/// Extracts a page number from an `<a>` element's `href`, resolved
/// against `BASE_URL` so both root-relative (`/system/ngc/all/3`) and
/// fully-qualified (`https://gamehacking.org/system/ngc/all/3`) hrefs are
/// recognised identically - the live pagination widget's range labels
/// (`"00 - An"`, `"An - Ba"`, ... `"Ze - Zo"`) carry no page number at
/// all, only the numeric href does, so page numbers are never inferred
/// from anchor text. Rejects anything that isn't exactly
/// `/system/ngc/all/<non-negative integer>` (an optional single trailing
/// slash is tolerated): a different host, an unrelated `ngc` path (e.g.
/// `/system/ngc/game/123`), a deeper path, or a non-numeric suffix all
/// return `None` rather than a guessed value.
fn gamecube_page_number_from_href(href: &str) -> Option<u32> {
    let base = Url::parse(BASE_URL).ok()?;
    let resolved = base.join(href).ok()?;
    if resolved.host_str() != Some("gamehacking.org") {
        return None;
    }
    let suffix = resolved.path().strip_prefix("/system/ngc/all/")?;
    let suffix = suffix.trim_end_matches('/');
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    suffix.parse::<u32>().ok()
}

fn parse_gamecube_index_page_numbers(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<Vec<u32>, GameHackingError> {
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(gamecube_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
    let text = decode_provider_text(bytes, charset);
    let document = Html::parse_document(&text);
    let selector = Selector::parse("a[href]").expect("static selector");
    let mut pages = BTreeSet::new();
    for node in document.select(&selector) {
        if let Some(page) = node
            .value()
            .attr("href")
            .and_then(gamecube_page_number_from_href)
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
    // The document just parsed is the root page itself, i.e. page 0,
    // whether or not its own pagination widget links back to the
    // currently-displayed range (the live page's first range, "00 - An",
    // is not always a link to itself). Inserted only after confirming at
    // least one other real numbered link exists above, so a response
    // with no real pagination links still fails loudly instead of
    // silently reporting a single fabricated page. `BTreeSet` naturally
    // deduplicates if `/system/ngc/all/0` was also linked explicitly.
    pages.insert(0);
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
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(gamecube_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
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

// --- sysID diagnostics ----------------------------------------------------

/// Parses a real game page's cheat-export `<form>` (identified by its
/// `action` attribute referencing `sub.exportCodes.php`, the same
/// endpoint the PS2 provider already posts to) and every `<input
/// type="hidden">` inside it. The numeric GameCube `sysID` is read
/// straight from that form - it is never guessed or derived any other
/// way.
pub fn parse_gamecube_sysid_diagnostics(
    game_id: u64,
    title: &str,
    game_page_url: &str,
    bytes: &[u8],
) -> Result<GameCubeSysIdDiagnostics, GameHackingError> {
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(gamecube_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
    let text = decode_provider_text(bytes, None);
    let document = Html::parse_document(&text);
    let form_selector = Selector::parse("form").expect("static selector");
    let input_selector = Selector::parse("input").expect("static selector");
    let export_form = document.select(&form_selector).find(|form| {
        form.value()
            .attr("action")
            .is_some_and(|action| action.to_ascii_lowercase().contains("exportcodes.php"))
    });
    let Some(export_form) = export_form else {
        return Err(gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            format!("GameHacking.org game page {game_id} has no cheat-export form"),
        ));
    };
    let export_form_action = export_form
        .value()
        .attr("action")
        .unwrap_or_default()
        .to_string();
    let hidden_fields: Vec<(String, String)> = export_form
        .select(&input_selector)
        .filter(|input| {
            input
                .value()
                .attr("type")
                .is_some_and(|input_type| input_type.eq_ignore_ascii_case("hidden"))
        })
        .filter_map(|input| {
            let name = input.value().attr("name")?;
            let value = input.value().attr("value").unwrap_or_default();
            Some((name.to_string(), value.to_string()))
        })
        .collect();
    let sys_id = hidden_fields
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("sysID"))
        .and_then(|(_, value)| value.trim().parse::<u16>().ok());
    Ok(GameCubeSysIdDiagnostics {
        game_id,
        title: title.to_string(),
        game_page_url: game_page_url.to_string(),
        export_form_action,
        hidden_fields,
        sys_id,
    })
}

// --- Game page format labels ---------------------------------------------

/// One cheat entry as it appears on the individual GameHacking.org game
/// page - unlike the flat Text export (which never carries a format
/// label at all), the page visibly labels every entry `ARMax` (the
/// GameCube/Wii Action Replay MAX format) or `Gecko`. A single game
/// commonly mixes both across its cheats (confirmed live: Luigi's
/// Mansion's 14 listed entries are 11 `ARMax` and 3 `Gecko`).
///
/// The page's own code text is *not* always the same encoding as the
/// Text export's: for a `Gecko`-labelled entry it is the identical raw
/// `XXXXXXXX YYYYYYYY` hex-pair lines (confirmed byte-for-byte equal to
/// the Text export's own lines for the same cheat); for an
/// `ARMax`-labelled entry it is the separately-encrypted AR-MAX text
/// notation (`XXXX-XXXX-XXXXX`-shaped groups), not the raw hex at all.
/// This is exactly why matching falls back to normalized title+author
/// when the code body doesn't line up - see
/// `apply_gamecube_page_format_labels`.
struct GameCubePageCheat {
    title: String,
    author: Option<String>,
    /// The label exactly as scraped (`"ARMax"`, `"Gecko"`, or whatever
    /// else the page ever shows) - mapped to `GameCubeCodeFormat` only by
    /// `map_gamecube_page_label`, never assumed here.
    format_label: Option<String>,
    /// Only populated for entries whose `<pre>` body looks like the same
    /// `XXXXXXXX YYYYYYYY` raw hex-pair lines the Text export uses (an
    /// ARMax entry's encrypted text never does) - used for the
    /// higher-confidence exact code-body match.
    code_lines: Vec<String>,
}

/// Scrapes every cheat entry from a real game page. Infallible and
/// best-effort: an unexpected page shape simply yields zero entries
/// (the export cheats then just stay `RawUnknown`, exactly as if this
/// enhancement were never attempted) rather than failing the cheat fetch
/// that already succeeded.
fn parse_gamecube_game_page_cheats(bytes: &[u8]) -> Vec<GameCubePageCheat> {
    let text = decode_provider_text(bytes, None);
    let document = Html::parse_document(&text);
    // `.codID` is the one class name the real page uses only for a
    // cheat's own title/checkbox/author block - specific enough to
    // anchor each entry without also matching the filter form, the game
    // info table, or the export form.
    let Ok(entry_selector) = Selector::parse(".codID") else {
        return Vec::new();
    };
    let Ok(label_selector) = Selector::parse(".col-sm-3 small") else {
        return Vec::new();
    };
    let Ok(code_selector) = Selector::parse(".col-sm-4.col-md-3 pre") else {
        return Vec::new();
    };
    let Ok(label_element_selector) = Selector::parse("label") else {
        return Vec::new();
    };
    let Ok(author_selector) = Selector::parse("a[href^='/hackers/']") else {
        return Vec::new();
    };
    let mut cheats = Vec::new();
    for entry in document.select(&entry_selector) {
        let title = entry
            .select(&label_element_selector)
            .next()
            .map(|label| {
                label
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map(|value| decode_html_text(&value));
        let Some(title) = title.filter(|value| !value.is_empty()) else {
            continue;
        };
        let author = entry
            .select(&author_selector)
            .next()
            .map(|node| node.text().collect::<String>().trim().to_string())
            .map(|value| decode_html_text(&value))
            .filter(|value| !value.is_empty());
        // The label and code body live in sibling divs under the same
        // `.row` as `.codID`, not inside `.codID` itself.
        let Some(row) = entry.parent_element() else {
            cheats.push(GameCubePageCheat {
                title,
                author,
                format_label: None,
                code_lines: Vec::new(),
            });
            continue;
        };
        let format_label = row
            .select(&label_selector)
            .next()
            .map(|node| node.text().collect::<String>().trim().to_string())
            .filter(|value| !value.is_empty());
        let code_lines = row
            .select(&code_selector)
            .next()
            .map(|pre| {
                pre.text()
                    .collect::<String>()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        cheats.push(GameCubePageCheat {
            title,
            author,
            format_label,
            code_lines,
        });
    }
    cheats
}

/// Maps the page's own label text to a format - the only place a cheat
/// is ever promoted out of `RawUnknown`. Anything not recognised exactly
/// (including no label at all) maps to `None`, never a guess.
fn map_gamecube_page_label(label: &str) -> Option<GameCubeCodeFormat> {
    match label.trim().to_ascii_lowercase().as_str() {
        "gecko" => Some(GameCubeCodeFormat::Gecko),
        "armax" | "action replay" | "action replay max" => Some(GameCubeCodeFormat::ActionReplay),
        _ => None,
    }
}

/// Upgrades already-parsed export cheats from `RawUnknown` to
/// `ActionReplay`/`Gecko` using only the individual game page's explicit
/// per-cheat labels - the Text export itself never carries one. Matches
/// each export cheat to at most one page entry, in priority order:
///
/// 1. An exact code-body match (the export cheat's own lines equal to a
///    single page entry's raw hex lines) - the higher-confidence path,
///    but only ever populated for `Gecko`-labelled page entries (an
///    `ARMax` entry's on-page text is separately encrypted, never equal
///    to the export's raw hex).
/// 2. Otherwise, a normalized-title-and-author match against a single
///    page entry.
///
/// If either step would match more than one page entry ambiguously, or
/// no page entry at all, the cheat is left exactly as it was (never
/// guessed). Returns how many cheats were actually upgraded.
pub fn apply_gamecube_page_format_labels(
    cheats: &mut [GameHackingGameCubeCheat],
    page_bytes: &[u8],
) -> usize {
    let page_cheats = parse_gamecube_game_page_cheats(page_bytes);
    let mut upgraded = 0;
    for cheat in cheats.iter_mut() {
        if cheat.code_format != GameCubeCodeFormat::RawUnknown {
            continue;
        }
        let code_matches: Vec<&GameCubePageCheat> = page_cheats
            .iter()
            .filter(|page| !page.code_lines.is_empty() && page.code_lines == cheat.code_lines)
            .collect();
        let matched_label = if code_matches.len() == 1 {
            code_matches[0].format_label.as_deref()
        } else {
            let title_norm = normalized_title(&cheat.name);
            let author_norm = cheat.author.as_deref().map(normalized_title);
            let title_matches: Vec<&GameCubePageCheat> = page_cheats
                .iter()
                .filter(|page| {
                    normalized_title(&page.title) == title_norm
                        && page.author.as_deref().map(normalized_title) == author_norm
                })
                .collect();
            if title_matches.len() == 1 {
                title_matches[0].format_label.as_deref()
            } else {
                None
            }
        };
        if let Some(format) = matched_label.and_then(map_gamecube_page_label) {
            cheat.code_format = format;
            upgraded += 1;
        }
    }
    upgraded
}

// --- Cheat export parsing -----------------------------------------------

/// The confirmed real "Text" export shape (`export_format() == "Text"`,
/// verified live against GameHacking game 54172, Luigi's Mansion) is a
/// two-line header (Dolphin Game ID, then `"<title> (<region>)"`), a
/// blank line, then repeated cheat blocks of a `<name> [<author>]` title
/// line (author may be empty: `[]`) followed by one or more code lines,
/// separated by blank lines. There is no `[Category\Title]` bracket
/// section syntax and no `Encryption:`/`author=` field in this format at
/// all - those are kept as a secondary, unambiguously-distinguishable
/// fallback (a `[Category\Title]` line starts with `[` as its very first
/// character, which a `<name> [<author>]` line never does) in case a
/// differently-shaped export is ever returned, never guessed from this
/// format's actual shape.
pub fn parse_gamehacking_gamecube_export(
    game: &GameHackingGameCubeGame,
    bytes: &[u8],
) -> Result<Vec<GameHackingGameCubeCheat>, GameHackingError> {
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(gamecube_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        gamecube_error(
            GameHackingErrorKind::InvalidResponse,
            "GameHacking.org GameCube export is not UTF-8",
        )
    })?;
    let mut lines = text.lines().peekable();
    // The confirmed real header is exactly a bare Dolphin Game ID line
    // followed by a "<title> (<region>)" line - skip both (plus one
    // blank separator line, if present) only when the very first line
    // actually normalizes as a Game ID. This is a specific, unambiguous
    // signal, never a generic "skip until the first blank line"
    // heuristic that could eat real cheat content in other export
    // shapes (e.g. the bracket-section fixtures covered below).
    if lines
        .peek()
        .is_some_and(|first| normalize_gamecube_game_id(first.trim()).is_some())
    {
        lines.next();
        lines.next();
        if lines.peek().is_some_and(|line| line.trim().is_empty()) {
            lines.next();
        }
    }
    let mut cheats = Vec::new();
    let mut pending = PendingGameCubeCheat::default();
    for raw in lines {
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
            flush_pending_gamecube_cheat(game, &mut pending, &mut cheats);
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if pending.name.is_none() {
            // The confirmed real title-line shape: "<name> [<author>]",
            // with an optionally empty author ("[]").
            let (name, author) = parse_gamecube_title_and_author(trimmed);
            pending.name = Some(name);
            if pending.author.is_none() {
                pending.author = author;
            }
            continue;
        }
        pending.description.push(decode_html_text(trimmed));
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

/// Splits the confirmed real title-line shape `"<name> [<author>]"` into
/// its name and (optionally empty) author. Requires the line to actually
/// end with `]` and contain a matching `[`; otherwise the whole line is
/// treated as the name with no author, rather than guessing a split
/// point.
fn parse_gamecube_title_and_author(line: &str) -> (String, Option<String>) {
    if line.ends_with(']')
        && let Some(open) = line.rfind('[')
    {
        let name = line[..open].trim();
        let author = line[open + 1..line.len() - 1].trim();
        return (
            decode_html_text(name),
            (!author.is_empty()).then(|| decode_html_text(author)),
        );
    }
    (decode_html_text(line.trim()), None)
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

    /// GameHacking.org's numeric GameCube sysID was confirmed by fetching
    /// a real game page (Luigi's Mansion, GameHacking game 54172) and
    /// reading its cheat-export form's hidden `sysID` field directly -
    /// never guessed. Pinned exactly so a future edit can't silently
    /// regress it back to an unconfirmed placeholder.
    #[test]
    fn gamecube_adapter_system_id_is_the_confirmed_value() {
        let adapter = GameCubeGameHackingAdapter;
        assert_eq!(adapter.system_id(), Some(13));
    }

    /// A sanitized excerpt of the real Luigi's Mansion (GameHacking game
    /// 54172) game page, trimmed to the cheat-export form plus an
    /// unrelated distractor form, proving the parser picks the form by
    /// its `action` (referencing `sub.exportCodes.php`) and reads the
    /// `sysID` hidden field directly rather than guessing it.
    #[test]
    fn sysid_diagnostics_parse_the_real_export_form_fixture() {
        let html =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-game-page-export-form.html");
        let diagnostics = parse_gamecube_sysid_diagnostics(
            54172,
            "Luigi's Mansion",
            "https://gamehacking.org/game/54172",
            html,
        )
        .unwrap();
        assert_eq!(diagnostics.export_form_action, "/inc/sub.exportCodes.php");
        assert_eq!(diagnostics.sys_id, Some(13));
        assert!(
            diagnostics
                .hidden_fields
                .iter()
                .any(|(name, value)| name == "gamID" && value == "54172")
        );
        assert!(
            diagnostics
                .hidden_fields
                .iter()
                .any(|(name, value)| name == "download" && value == "true")
        );
        // The distractor search form's unrelated hidden field must never
        // be picked up as if it belonged to the export form.
        assert!(
            !diagnostics
                .hidden_fields
                .iter()
                .any(|(name, _)| name == "unrelated")
        );
    }

    #[test]
    fn sysid_diagnostics_error_when_no_export_form_is_present() {
        let html = b"<html><body><form action=\"/search.php\"></form></body></html>";
        let error = parse_gamecube_sysid_diagnostics(
            1,
            "No Form Game",
            "https://gamehacking.org/game/1",
            html,
        )
        .unwrap_err();
        assert_eq!(error.kind, GameHackingErrorKind::InvalidResponse);
    }

    /// A sanitized real export (Luigi's Mansion, GameHacking game 54172,
    /// `format=Text`), trimmed to its header plus four representative
    /// cheats including one with an empty author (`[]`) and one whose
    /// title contains a period. Proves named cheats, real authors, and
    /// the header skip all work against genuine live data, not just a
    /// hand-written approximation of the format.
    #[test]
    fn real_gamecube_text_export_fixture_parses_named_cheats_and_authors() {
        let export = include_bytes!("../../tests/fixtures/gamehacking/gamecube-real-export.txt");
        let fixture_game = game(54172, "GLME01", "USA", "Luigi's Mansion");
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert_eq!(cheats.len(), 5);
        assert_eq!(cheats[0].name, "99 of Some Treasures");
        assert_eq!(cheats[0].author.as_deref(), Some("Codejunkies"));
        assert_eq!(cheats[0].code_lines.len(), 7);
        assert_eq!(cheats[0].code_format, GameCubeCodeFormat::RawUnknown);
        assert_eq!(cheats[1].name, "999 Cash");
        assert_eq!(cheats[2].name, "Element Modifier");
        assert_eq!(cheats[2].code_lines.len(), 20);
        assert_eq!(cheats[3].name, "End Boss Has No HP");
        assert_eq!(cheats[3].code_lines, vec!["04126E6C 60000000".to_string()]);
        assert_eq!(cheats[4].name, "Matrix Look");
        assert_eq!(cheats[4].author.as_deref(), Some("Dosha"));
        // "Note []" has no code lines at all and must be silently
        // dropped, not counted as a sixth cheat with a blank name.
        assert!(cheats.iter().all(|cheat| cheat.name != "Note"));
    }

    /// A second real, independently-fetched export (Eternal Darkness -
    /// Sanity's Requiem, GameHacking game 54189) - proves the parser
    /// isn't overfit to Luigi's Mansion's specific export. Every one of
    /// its 16 real cheats has no explicit format label either, exactly
    /// like Luigi's Mansion.
    #[test]
    fn real_eternal_darkness_export_fixture_parses_all_named_cheats_as_raw_unknown() {
        let export =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-eternal-darkness-export.txt");
        let fixture_game = game(
            54189,
            "GEDE01",
            "USA",
            "Eternal Darkness - Sanity's Requiem",
        );
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert_eq!(cheats.len(), 16);
        assert!(
            cheats
                .iter()
                .all(|cheat| cheat.code_format == GameCubeCodeFormat::RawUnknown),
            "no real sampled export ever carries an explicit format label; every cheat here must stay RawUnknown"
        );
        let unlock_all = cheats
            .iter()
            .find(|cheat| cheat.name == "Unlock All Extras")
            .expect("named cheat present");
        assert_eq!(unlock_all.author.as_deref(), Some("donny2112"));
        assert_eq!(unlock_all.code_lines.len(), 2);
    }

    /// A real export for a game with zero published cheats (Pokemon Box -
    /// Ruby & Sapphire, GameHacking game 54198) - just the two-line
    /// header and nothing else. Must fail the same "no recognisable
    /// code lines" way a genuinely malformed export would, not panic or
    /// silently report zero cheats as success.
    #[test]
    fn real_export_with_no_cheats_at_all_fails_loudly_not_silently() {
        let export =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-no-cheats-export.txt");
        let fixture_game = game(54198, "GPXP01", "Europe", "Pokemon Box - Ruby & Sapphire");
        let error = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap_err();
        assert_eq!(error.kind, GameHackingErrorKind::InvalidResponse);
    }

    /// The code-format audit's classification reasons must cite the
    /// actual Dolphin-source-grounded justification, not merely repeat
    /// the enum variant name - this is what
    /// `gamehacking-gamecube-code-format-audit` prints per cheat.
    #[test]
    fn code_format_diagnostic_explains_why_raw_unknown_cheats_are_not_guessed() {
        let export = include_bytes!("../../tests/fixtures/gamehacking/gamecube-real-export.txt");
        let fixture_game = game(54172, "GLME01", "USA", "Luigi's Mansion");
        let cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        let diagnostic = diagnose_gamecube_cheat_code_format(&cheats[0]);
        assert_eq!(diagnostic.name, "99 of Some Treasures");
        assert_eq!(diagnostic.line_count, 7);
        assert_eq!(diagnostic.opcode_prefixes, vec!["04"; 7]);
        assert_eq!(diagnostic.code_format, GameCubeCodeFormat::RawUnknown);
        assert!(
            diagnostic
                .classification_reason
                .contains("ActionReplay.cpp")
        );
        assert!(diagnostic.classification_reason.contains("GeckoCode.cpp"));
    }

    /// A sanitized real excerpt of Luigi's Mansion's own game page cheat
    /// listing (GameHacking game 54172), containing a genuine mixture of
    /// `ARMax` and `Gecko` labelled entries - proving the label vocabulary
    /// really is per-cheat, not per-game, and that both values are
    /// scraped correctly from the real markup shape (`.codID` block plus
    /// sibling `.col-sm-3`/`.col-sm-4.col-md-3` divs under the same
    /// `.row`).
    #[test]
    fn page_cheats_parse_a_real_mixture_of_armax_and_gecko_labels() {
        let html =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-game-page-mixed-labels.html");
        let page_cheats = parse_gamecube_game_page_cheats(html);
        assert!(page_cheats.len() >= 12, "{}", page_cheats.len());
        let armax_count = page_cheats
            .iter()
            .filter(|cheat| cheat.format_label.as_deref() == Some("ARMax"))
            .count();
        let gecko_count = page_cheats
            .iter()
            .filter(|cheat| cheat.format_label.as_deref() == Some("Gecko"))
            .count();
        assert!(armax_count >= 9, "ARMax count: {armax_count}");
        assert_eq!(gecko_count, 3, "Gecko count: {gecko_count}");
        let element_modifier = page_cheats
            .iter()
            .find(|cheat| cheat.title == "Element Modifier")
            .expect("Element Modifier present on the real page");
        assert_eq!(element_modifier.author.as_deref(), Some("Link Master"));
        assert_eq!(element_modifier.format_label.as_deref(), Some("Gecko"));
        assert_eq!(element_modifier.code_lines.len(), 20);
        assert_eq!(element_modifier.code_lines[0], "284CAFD0 00000008");
        let treasures = page_cheats
            .iter()
            .find(|cheat| cheat.title == "99 of Some Treasures")
            .expect("real ARMax-labelled entry present");
        assert_eq!(treasures.format_label.as_deref(), Some("ARMax"));
        // An ARMax entry's on-page code is separately-encrypted AR-MAX
        // text, never the raw hex the Text export uses for the same
        // cheat - confirmed here so the matcher's fallback to
        // title+author (rather than code-body) for ActionReplay entries
        // is exercised against real data, not an assumption.
        assert_ne!(treasures.code_lines, vec!["040AE518 63180063".to_string()]);
    }

    #[test]
    fn map_gamecube_page_label_only_recognises_the_confirmed_real_vocabulary() {
        assert_eq!(
            map_gamecube_page_label("Gecko"),
            Some(GameCubeCodeFormat::Gecko)
        );
        assert_eq!(
            map_gamecube_page_label("ARMax"),
            Some(GameCubeCodeFormat::ActionReplay)
        );
        assert_eq!(
            map_gamecube_page_label("armax"),
            Some(GameCubeCodeFormat::ActionReplay)
        );
        assert_eq!(map_gamecube_page_label("Mednafen"), None);
        assert_eq!(map_gamecube_page_label(""), None);
    }

    /// The end-to-end enhancement: real export cheats (initially all
    /// `RawUnknown`, per `parse_gamehacking_gamecube_export`'s own
    /// design) get upgraded using the real mixed-label page fixture -
    /// `Element Modifier` via its exact code body (a Gecko entry, whose
    /// on-page code equals the export's raw hex lines), and the ARMax
    /// entries via normalized title+author (since their on-page code is
    /// a different, encrypted encoding that can never equal the export's
    /// raw hex).
    #[test]
    fn page_labels_upgrade_matching_export_cheats_by_code_body_or_title_and_author() {
        let export = include_bytes!("../../tests/fixtures/gamehacking/gamecube-real-export.txt");
        let fixture_game = game(54172, "GLME01", "USA", "Luigi's Mansion");
        let mut cheats = parse_gamehacking_gamecube_export(&fixture_game, export).unwrap();
        assert!(
            cheats
                .iter()
                .all(|cheat| cheat.code_format == GameCubeCodeFormat::RawUnknown)
        );
        let page_html =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-game-page-mixed-labels.html");
        let upgraded = apply_gamecube_page_format_labels(&mut cheats, page_html);
        assert_eq!(
            upgraded, 5,
            "all 5 export cheats have a matching page entry"
        );
        let by_name = |name: &str| cheats.iter().find(|cheat| cheat.name == name).unwrap();
        assert_eq!(
            by_name("Element Modifier").code_format,
            GameCubeCodeFormat::Gecko,
            "matched by exact code body"
        );
        assert_eq!(
            by_name("99 of Some Treasures").code_format,
            GameCubeCodeFormat::ActionReplay,
            "matched by normalized title+author (ARMax on-page code never equals raw hex)"
        );
        assert_eq!(
            by_name("999 Cash").code_format,
            GameCubeCodeFormat::ActionReplay
        );
        assert_eq!(
            by_name("End Boss Has No HP").code_format,
            GameCubeCodeFormat::ActionReplay
        );
        assert_eq!(
            by_name("Matrix Look").code_format,
            GameCubeCodeFormat::ActionReplay
        );
    }

    #[test]
    fn unmatched_or_unlabelled_page_entries_never_change_the_classification() {
        let fixture_game = game(999, "ZZZZZZ", "USA", "No Such Game");
        let mut cheats = vec![GameHackingGameCubeCheat {
            id: "gh-gc-999-1".to_string(),
            name: "Not On The Page".to_string(),
            author: None,
            description: None,
            code_format: GameCubeCodeFormat::RawUnknown,
            code_lines: vec!["DEADBEEF 00000000".to_string()],
            source_game_id: fixture_game.game_id,
            source_url: fixture_game.source_url.clone(),
        }];
        let page_html =
            include_bytes!("../../tests/fixtures/gamehacking/gamecube-game-page-mixed-labels.html");
        let upgraded = apply_gamecube_page_format_labels(&mut cheats, page_html);
        assert_eq!(upgraded, 0);
        assert_eq!(cheats[0].code_format, GameCubeCodeFormat::RawUnknown);
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

    /// The live pagination widget's visible labels are alphabetic ranges
    /// ("00 - An", "An - Ba", ... "Ze - Zo"), never a page number - only
    /// the numeric hrefs (`/system/ngc/all/0`, `/system/ngc/all/1`, ...)
    /// identify pages. Mirrors the real root page's shape closely enough
    /// to catch a regression back to label-based parsing.
    fn range_labelled_root_page_html(self_links_page_zero: bool) -> String {
        // Real ranges are contiguous with no gaps; only the first
        // ("00 - An") and last ("Ze - Zo") carry recognisable labels in
        // this fixture, the rest use a generic label - the parser must
        // not care either way, since it never reads anchor text at all.
        let labels: [&str; 26] = [
            "00 - An", "range 1", "range 2", "range 3", "range 4", "range 5", "range 6", "range 7",
            "range 8", "range 9", "range 10", "range 11", "range 12", "range 13", "range 14",
            "range 15", "range 16", "range 17", "range 18", "range 19", "range 20", "range 21",
            "range 22", "range 23", "range 24", "Ze - Zo",
        ];
        let mut html = String::from("<nav>");
        for (page, label) in labels.iter().enumerate() {
            if page == 0 && !self_links_page_zero {
                html.push_str(&format!("<span class=\"current\">{label}</span>"));
            } else {
                html.push_str(&format!("<a href=\"/system/ngc/all/{page}\">{label}</a>"));
            }
        }
        html.push_str("</nav>");
        html
    }

    #[test]
    fn href_based_page_discovery_ignores_range_labels() {
        let html = range_labelled_root_page_html(true);
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages.first(), Some(&0));
        assert_eq!(pages.last(), Some(&25));
        assert!(pages.contains(&1));
        assert!(pages.contains(&3));
    }

    #[test]
    fn root_page_without_a_self_link_to_page_zero_still_yields_page_zero() {
        // The real root page's first range ("00 - An") is the
        // currently-displayed page and is not always a clickable link -
        // page 0 must still be accepted without erroring or duplicating.
        let html = range_labelled_root_page_html(false);
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages.first(), Some(&0));
        assert_eq!(pages.iter().filter(|page| **page == 0).count(), 1);
    }

    #[test]
    fn duplicate_page_zero_href_is_deduplicated_not_double_counted() {
        let html = r#"<a href="/system/ngc/all/0">00 - An</a><a href="/system/ngc/all/0">00 - An</a><a href="/system/ngc/all/1">An - Ba</a>"#;
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages, vec![0, 1]);
    }

    #[test]
    fn absolute_and_relative_hrefs_are_recognised_identically() {
        let html = r#"<a href="/system/ngc/all/0">00 - An</a><a href="https://gamehacking.org/system/ngc/all/1">An - Ba</a><a href="/system/ngc/all/2">Ba - Bi</a>"#;
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages, vec![0, 1, 2]);
    }

    #[test]
    fn unrelated_ngc_links_and_malformed_suffixes_are_rejected() {
        let html = r#"<a href="/system/ngc/all/0">00 - An</a>
<a href="/system/ngc/all/1">An - Ba</a>
<a href="/system/ngc/game/501">Some Game</a>
<a href="/system/ngc/all/abc">Not a page number</a>
<a href="/system/ngc/all/1/extra">Deeper path</a>
<a href="https://example.com/system/ngc/all/9">Wrong host</a>"#;
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages, vec![0, 1]);
    }

    #[test]
    fn index_page_numbers_require_a_complete_zero_based_run() {
        let html = r#"<a href="/system/ngc/all/0">00 - An</a><a href="/system/ngc/all/1">An - Ba</a><a href="/system/ngc/all/2">Ba - Bi</a>"#;
        let pages = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap();
        assert_eq!(pages, vec![0, 1, 2]);

        let incomplete =
            r#"<a href="/system/ngc/all/0">00 - An</a><a href="/system/ngc/all/2">Ba - Bi</a>"#;
        assert!(parse_gamecube_index_page_numbers(incomplete.as_bytes(), None).is_err());
    }

    #[test]
    fn no_page_error_only_when_no_valid_hrefs_exist_at_all() {
        let html =
            r#"<a href="/other/page">Unrelated</a><a href="/system/ngc/game/501">Some Game</a>"#;
        let error = parse_gamecube_index_page_numbers(html.as_bytes(), None).unwrap_err();
        assert_eq!(error.kind, GameHackingErrorKind::InvalidResponse);
        assert!(error.detail.contains("no numbered pages"));
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

    fn cloudflare_cache_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamecube-gamehacking-cloudflare-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    const CLOUDFLARE_CHALLENGE_BODY: &str = "<html><head><title>Just a moment...</title></head><body>Enable JavaScript and cookies to continue<div id=\"footer\">Cloudflare Ray ID: 89abc123def456</div></body></html>";

    #[test]
    fn cloudflare_challenge_html_returned_with_status_200_is_rejected_before_parsing() {
        assert!(
            parse_gamecube_index_page_numbers(CLOUDFLARE_CHALLENGE_BODY.as_bytes(), None).is_err()
        );
        let error = parse_gamecube_index_page_numbers(CLOUDFLARE_CHALLENGE_BODY.as_bytes(), None)
            .unwrap_err();
        assert_eq!(error.kind, GameHackingErrorKind::CloudflareBlocked);
        assert_eq!(error.detail, GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE);

        let export_error = parse_gamehacking_gamecube_export(
            &game(1, "GALE01", "USA", "Fixture"),
            CLOUDFLARE_CHALLENGE_BODY.as_bytes(),
        )
        .unwrap_err();
        assert_eq!(export_error.kind, GameHackingErrorKind::CloudflareBlocked);
    }

    #[test]
    fn ordinary_index_html_is_not_misclassified_as_a_cloudflare_challenge() {
        let html = r#"<a href="/system/ngc/all/0">0</a><table>
<tr><td>Test Racer</td></tr>
<tr><td><a href="/game/501/test-racer">USA</a></td><td>GTRE01</td></tr>
</table>"#;
        assert!(!cached_bytes_are_cloudflare_challenge(html.as_bytes()));
    }

    #[test]
    fn a_cached_challenge_page_is_rejected_on_replay_without_touching_the_network() {
        let root = cloudflare_cache_root("cached-replay");
        fs::write(
            root.join(GAMECUBE_INDEX_ROOT_CACHE_FILE),
            CLOUDFLARE_CHALLENGE_BODY.as_bytes(),
        )
        .unwrap();
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: false,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        let result = provider.cached_request(
            GAMECUBE_INDEX_ROOT_CACHE_FILE,
            GAMECUBE_INDEX_URL,
            MAX_INDEX_BYTES,
            &options,
            |_transport| unreachable!("a cache hit must never reach the network"),
        );
        let error = result.unwrap_err();
        assert_eq!(error.kind, GameHackingErrorKind::CloudflareBlocked);
        assert_eq!(error.detail, GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_live_cloudflare_block_never_overwrites_the_existing_valid_cache_entry() {
        let root = cloudflare_cache_root("no-overwrite");
        let cache_name = "export-42.txt";
        let original = b"GALE01\nFixture Game (USA)\n\n[Gecko]\n$Real Cheat\n04000000 00000001\n";
        fs::write(root.join(cache_name), original).unwrap();
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        let result = provider
            .cached_request(cache_name, EXPORT_URL, MAX_EXPORT_BYTES, &options, |_| {
                Err(gamecube_error(
                    GameHackingErrorKind::CloudflareBlocked,
                    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                ))
            })
            .unwrap();
        assert!(result.cached_fallback);
        assert_eq!(result.bytes, original);
        let on_disk = fs::read(root.join(cache_name)).unwrap();
        assert_eq!(
            on_disk, original,
            "a Cloudflare-blocked live fetch must never overwrite the existing cache entry"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cloudflare_block_starts_a_cooldown_that_skips_the_network_on_the_next_call() {
        let root = cloudflare_cache_root("cooldown");
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: true,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        let first = provider.cached_request(
            "export-1.txt",
            EXPORT_URL,
            MAX_EXPORT_BYTES,
            &options,
            |_| {
                Err(gamecube_error(
                    GameHackingErrorKind::CloudflareBlocked,
                    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
                ))
            },
        );
        assert_eq!(
            first.unwrap_err().kind,
            GameHackingErrorKind::CloudflareBlocked
        );
        // A second call, for a different cache entry, must be rejected purely
        // from the cooldown marker - never reaching the network closure -
        // exactly the "do not retry repeatedly" behaviour required after a
        // confirmed block.
        let second = provider.cached_request(
            "export-2.txt",
            EXPORT_URL,
            MAX_EXPORT_BYTES,
            &options,
            |_transport| unreachable!("the cooldown must skip the network entirely"),
        );
        assert_eq!(
            second.unwrap_err().kind,
            GameHackingErrorKind::CloudflareBlocked
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cached_gamecube_catalogue_stays_usable_for_matching_while_live_requests_are_blocked() {
        let root = cloudflare_cache_root("catalogue-usable");
        // A confirmed-blocked cooldown is active for live requests...
        mark_cloudflare_blocked(&root).unwrap();
        assert!(cloudflare_cooldown_remaining(&root).is_some());
        // ...but the already-downloaded catalogue never touches the network
        // at all, so matching against it must keep working regardless.
        let catalogue = GameHackingGameCubeCatalogue {
            schema_version: GAMECUBE_CATALOGUE_SCHEMA_VERSION,
            provider: GAMEHACKING_GAMECUBE_PROVIDER_ID.to_string(),
            system: "GameCube".to_string(),
            source_url: GAMECUBE_INDEX_URL.to_string(),
            retrieved_at_unix_seconds: 1_700_000_000,
            pages: Vec::new(),
            games: vec![GameHackingGameCubeIndexRecord {
                game_id: 501,
                title: "Test Racer".to_string(),
                dolphin_game_id: Some("GTRE01".to_string()),
                region: Some("USA".to_string()),
                revision: None,
                hash: None,
                source_url: "https://gamehacking.org/game/501/test-racer".to_string(),
                index_source_url: GAMECUBE_INDEX_URL.to_string(),
                retrieved_at_unix_seconds: 1_700_000_000,
            }],
        };
        let mut bytes = serde_json::to_vec_pretty(&catalogue).unwrap();
        bytes.push(b'\n');
        fs::write(root.join(GAMECUBE_CATALOGUE_FILE), bytes).unwrap();
        let loaded = load_gamecube_catalogue(&root).unwrap();
        assert_eq!(loaded.games.len(), 1);
        assert_eq!(loaded.games[0].dolphin_game_id.as_deref(), Some("GTRE01"));
        let provider = GameHackingGameCubeProvider::default();
        let options = GameHackingGameCubeFetchOptions {
            cache_root: root.clone(),
            force_refresh: false,
            delay: Duration::from_millis(1),
            cancellation: None,
        };
        let matched = provider
            .match_game(&identity("GTRE01", "E", 0), &options)
            .unwrap();
        assert_eq!(matched.status, GameHackingGameCubeMatchStatus::Matched);
        assert_eq!(matched.game.unwrap().game_id, 501);
        let _ = fs::remove_dir_all(&root);
    }
}
