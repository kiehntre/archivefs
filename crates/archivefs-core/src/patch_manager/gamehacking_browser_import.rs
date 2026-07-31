//! Browser-assisted import of GameHacking.org content that ArchiveFS is
//! not allowed to fetch itself.
//!
//! GameHacking.org works normally in a person's own web browser, but it
//! answers ArchiveFS (and command-line HTTP clients generally) with a
//! Cloudflare bot challenge after only a few requests - see
//! `gamehacking_provider::classify_gamehacking_http_response` and
//! `GameHackingErrorKind::CloudflareBlocked`. This module is the
//! deliberately *unclever* answer to that: the person opens the exact
//! game page in their own ordinary browser, saves or copies it, and hands
//! the resulting bytes to ArchiveFS.
//!
//! ## What this module explicitly does not do
//!
//! Nothing here attempts to defeat, weaken, or work around a challenge:
//!
//! - No browser fingerprint is spoofed and no user agent is rotated; the
//!   provider's single honest `USER_AGENT` is untouched.
//! - No browser cookie, session, header, profile, or history is read,
//!   requested, or stored. The provenance record written below has no
//!   field capable of holding one.
//! - No CAPTCHA is solved, no headless/embedded browser is launched, and
//!   no remote-debugging port is used. The only process ever started is
//!   the desktop's own default handler (`xdg-open` and friends) for a
//!   validated `https://gamehacking.org` URL, which the person themself
//!   asked for.
//! - No proxy is used, and the existing Cloudflare detection and cooldown
//!   behaviour in `gamehacking_provider` is not relaxed in any way. A
//!   challenge page is *rejected* here too, and can never overwrite a
//!   good cache.
//!
//! ## Trust model
//!
//! Imported bytes are inert data. HTML is never rendered - it is parsed
//! by `scraper` into text and attributes, exactly as the live provider
//! already parses a fetched page - and script/style/frame elements are
//! stripped before anything is stored (see `sanitize_imported_html`).
//! `import_safety::UNKNOWN_CODE_POLICY` applies unchanged: ArchiveFS may
//! inspect imported content, but never executes it.
//!
//! ## Destinations
//!
//! A successful import writes the *same* cache file the live provider
//! itself would have written, so every downstream stage (matching,
//! preview, classification, install, verification, History/Undo) keeps
//! working with no special case at all:
//!
//! | Platform | Import kind | Cache file |
//! |---|---|---|
//! | GameCube | game page HTML | `game-<gamID>.html` |
//! | GameCube | Text export | `export-<gamID>.txt` |
//! | PlayStation 2 | PCSX2 export | `export-<gamID>.pnach` |
//!
//! PlayStation 2 game-page HTML has no counterpart cache key: the PS2
//! provider never fetches a game page (it uses only the catalogue index
//! and the export endpoint), and it has no page-cheat parser to run one
//! through. Importing one is therefore refused with a specific,
//! actionable message rather than written to a file nothing reads - this
//! is the "where compatible with the existing provider" boundary.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::gamehacking_gamecube_provider::{
    GameCubeGameIdentity, GameHackingGameCubeCheat, GameHackingGameCubeGame,
    apply_gamecube_page_format_labels, normalize_gamecube_game_id,
    parse_gamehacking_gamecube_export, summarize_gamecube_game_page,
};
use super::gamehacking_provider::{
    GameHackingCheat, GameHackingGame, cached_bytes_are_cloudflare_challenge, normalize_ps2_serial,
    parse_gamehacking_pnach,
};
use super::pcsx2_identity::Pcsx2GameIdentity;

/// The exact provenance marker written for every manually imported cache
/// entry. Callers compare against this constant rather than matching
/// prose, exactly as they already do with
/// `GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE`.
pub const MANUAL_BROWSER_IMPORT_SOURCE: &str = "manual_browser_import";

/// Bumped only when [`BrowserImportProvenance`]'s own shape changes.
pub const BROWSER_IMPORT_PROVENANCE_SCHEMA_VERSION: u32 = 1;

/// The parser contract an import was validated against. Bumped when the
/// GameCube page/export or PS2 pnach parsing rules change in a way that
/// would make an older stored record's counts no longer reproducible.
pub const BROWSER_IMPORT_PARSER_SCHEMA_VERSION: u32 = 1;

/// The hard ceiling for any imported file or pasted string. A real saved
/// GameHacking game page is a few hundred kilobytes; a Text export is a
/// few kilobytes. 8 MiB is the same bound the provider already applies to
/// a fetched index page (`MAX_INDEX_BYTES`).
pub const MAX_BROWSER_IMPORT_BYTES: usize = 8 * 1024 * 1024;

/// The one origin any browser-import URL may ever have.
const GAMEHACKING_HOST: &str = "gamehacking.org";
const GAMEHACKING_BASE_URL: &str = "https://gamehacking.org";

/// Confirmed numeric GameHacking.org `sysID` values, read from real export
/// forms - see `GameCubeGameHackingAdapter::system_id` and
/// `Ps2GameHackingAdapter::system_id`. Used here only to *reject* a page
/// belonging to another system, never to build a request.
const GAMECUBE_SYS_ID: u16 = 13;
const PS2_SYS_ID: u16 = 16;

/// The exact banner wording shown when live access is blocked and the
/// browser-assisted route is offered instead. Kept as constants so the
/// GUI and its tests share one source of truth.
pub const GAMEHACKING_BROWSER_IMPORT_BLOCKED_TITLE: &str = "GameHacking.org access blocked";
pub const GAMEHACKING_BROWSER_IMPORT_BLOCKED_BODY: &str = "ArchiveFS cannot fetch this page automatically, but you can open it \
     in your browser and import the page or Text export.";

// --- Platform and kind ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserImportPlatform {
    GameCube,
    PlayStation2,
}

impl BrowserImportPlatform {
    /// The CLI's `--platform` value.
    pub fn slug(self) -> &'static str {
        match self {
            Self::GameCube => "gamecube",
            Self::PlayStation2 => "ps2",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::GameCube => "GameCube",
            Self::PlayStation2 => "PlayStation 2",
        }
    }

    /// GameHacking.org's own URL slug - `ngc` for GameCube, not
    /// `gamecube` (see `gamehacking_gamecube_provider`'s module doc).
    pub fn gamehacking_system_slug(self) -> &'static str {
        match self {
            Self::GameCube => "ngc",
            Self::PlayStation2 => "ps2",
        }
    }

    fn system_id(self) -> u16 {
        match self {
            Self::GameCube => GAMECUBE_SYS_ID,
            Self::PlayStation2 => PS2_SYS_ID,
        }
    }

    pub fn parse_slug(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gamecube" | "gc" | "ngc" => Some(Self::GameCube),
            "ps2" | "playstation2" | "playstation-2" => Some(Self::PlayStation2),
            _ => None,
        }
    }

    /// Every import kind this platform actually has a provider cache key
    /// for, in the order the dialog lists them.
    pub fn accepted_kinds(self) -> &'static [BrowserImportKind] {
        match self {
            Self::GameCube => &[
                BrowserImportKind::GamePageHtml,
                BrowserImportKind::TextExport,
            ],
            Self::PlayStation2 => &[BrowserImportKind::TextExport],
        }
    }

    /// Human-readable accepted formats, for the dialog and the CLI.
    pub fn accepted_formats(self) -> Vec<&'static str> {
        match self {
            Self::GameCube => vec![
                "Saved game page (.html, .htm)",
                "Copied game page source",
                "GameHacking Text export (.txt)",
                "Pasted Text export",
            ],
            Self::PlayStation2 => vec![
                "GameHacking PCSX2 export (.pnach, .txt)",
                "Pasted PCSX2 export",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserImportKind {
    /// An individual GameHacking.org game page, saved or copied as HTML.
    GamePageHtml,
    /// A GameHacking.org cheat export: the GameCube "Text" (`.txt`)
    /// format, or the PS2 "PCSX2" (`.pnach`) format.
    TextExport,
}

impl BrowserImportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GamePageHtml => "game page HTML",
            Self::TextExport => "cheat export",
        }
    }

    pub fn parse_slug(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "page" | "html" | "game-page" => Some(Self::GamePageHtml),
            "export" | "text" | "txt" | "pnach" => Some(Self::TextExport),
            _ => None,
        }
    }

    /// The exact provider cache file name this kind populates, or `None`
    /// when the platform's provider has no such cache key at all.
    pub fn cache_file_name(self, platform: BrowserImportPlatform, game_id: u64) -> Option<String> {
        match (platform, self) {
            (BrowserImportPlatform::GameCube, Self::GamePageHtml) => {
                Some(format!("game-{game_id}.html"))
            }
            (BrowserImportPlatform::GameCube, Self::TextExport) => {
                Some(format!("export-{game_id}.txt"))
            }
            (BrowserImportPlatform::PlayStation2, Self::TextExport) => {
                Some(format!("export-{game_id}.pnach"))
            }
            (BrowserImportPlatform::PlayStation2, Self::GamePageHtml) => None,
        }
    }
}

// --- Errors ---------------------------------------------------------------

/// Every way a *local* import can fail. Deliberately disjoint from
/// `GameHackingErrorKind`: a local import failure must never be reported
/// as an HTTP or network error, because no request was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserImportErrorKind {
    /// The selected local game has no verified identity to check against.
    IdentityIncomplete,
    /// Nothing was supplied at all (empty file, empty paste).
    EmptyInput,
    /// The clipboard held no text.
    ClipboardEmpty,
    /// The clipboard could not be read on this system at all.
    ClipboardUnavailable,
    InputTooLarge,
    /// The file could not be read, was a symlink, or was not a file.
    SourceUnreadable,
    /// Not UTF-8, or not decodable as text at all.
    NotText,
    /// A Cloudflare challenge/interstitial page, per the shared
    /// classifier. Never written to the cache.
    ChallengeContent,
    /// Recognisably not a GameHacking.org page/export at all.
    UnrelatedContent,
    /// GameHacking.org content, but for a different game.
    WrongGame,
    /// GameHacking.org content, but for a different system.
    WrongPlatform,
    /// GameCube: the content's Game ID is not the selected Dolphin Game ID.
    GameIdMismatch,
    /// PlayStation 2: the content's serial is not the verified local serial.
    SerialMismatch,
    /// Real GameHacking content whose shape this platform's provider has
    /// no parser for (e.g. a PS2 game page).
    UnsupportedPageShape,
    /// A cheat export that no cheat could be parsed out of.
    MalformedExport,
    /// GameHacking content with no evidence tying it to any game - a
    /// filename is never accepted as evidence.
    MissingIdentityEvidence,
    /// The validated content could not be written to the cache.
    CacheWriteFailed,
    /// Only ever produced by the browser-launch helper.
    BrowserLaunchFailed,
    /// A URL that is not a plain `https://gamehacking.org` URL.
    InvalidUrl,
}

impl BrowserImportErrorKind {
    /// A short, stable headline for the GUI. Never a generic HTTP phrase.
    pub fn headline(self) -> &'static str {
        match self {
            Self::IdentityIncomplete => "Local game identity incomplete",
            Self::EmptyInput => "Nothing to import",
            Self::ClipboardEmpty => "Clipboard is empty",
            Self::ClipboardUnavailable => "Clipboard unavailable",
            Self::InputTooLarge => "Import too large",
            Self::SourceUnreadable => "File could not be read",
            Self::NotText => "File is not text",
            Self::ChallengeContent => "Imported a Cloudflare challenge page",
            Self::UnrelatedContent => "Not a GameHacking.org page",
            Self::WrongGame => "Wrong game page",
            Self::WrongPlatform => "Wrong platform",
            Self::GameIdMismatch => "Game ID mismatch",
            Self::SerialMismatch => "PS2 serial mismatch",
            Self::UnsupportedPageShape => "Unsupported page shape",
            Self::MalformedExport => "Malformed cheat export",
            Self::MissingIdentityEvidence => "No identity evidence in import",
            Self::CacheWriteFailed => "Cache write failed",
            Self::BrowserLaunchFailed => "Browser could not be opened",
            Self::InvalidUrl => "Unsupported URL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserImportError {
    pub kind: BrowserImportErrorKind,
    pub detail: String,
}

impl std::fmt::Display for BrowserImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for BrowserImportError {}

fn import_error(kind: BrowserImportErrorKind, detail: impl Into<String>) -> BrowserImportError {
    BrowserImportError {
        kind,
        detail: detail.into(),
    }
}

// --- Local identity -------------------------------------------------------

/// The verified-only local identity an import is checked against. Built
/// solely from the existing verified identity adapters - it can never be
/// constructed from a candidate, a filename, or user-typed text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum BrowserImportLocalIdentity {
    GameCube {
        title: String,
        dolphin_game_id: String,
        region: Option<String>,
    },
    PlayStation2 {
        title: String,
        executable_crc: String,
        serial: Option<String>,
        region: Option<String>,
    },
}

impl BrowserImportLocalIdentity {
    pub fn from_gamecube(identity: &GameCubeGameIdentity) -> Result<Self, BrowserImportError> {
        let dolphin_game_id = identity.verified_game_id().ok_or_else(|| {
            import_error(
                BrowserImportErrorKind::IdentityIncomplete,
                "ArchiveFS needs a verified local Dolphin Game ID before importing a GameHacking.org page for this game.",
            )
        })?;
        Ok(Self::GameCube {
            title: identity.title.clone(),
            dolphin_game_id: dolphin_game_id.to_string(),
            region: identity.region.clone(),
        })
    }

    pub fn from_ps2(identity: &Pcsx2GameIdentity) -> Result<Self, BrowserImportError> {
        let executable_crc = identity.verified_crc().ok_or_else(|| {
            import_error(
                BrowserImportErrorKind::IdentityIncomplete,
                "ArchiveFS needs a verified local PCSX2 executable CRC before importing a GameHacking.org export for this game.",
            )
        })?;
        Ok(Self::PlayStation2 {
            title: identity.title.clone(),
            executable_crc: executable_crc.to_string(),
            serial: identity.serial.clone(),
            region: identity.region.clone(),
        })
    }

    pub fn platform(&self) -> BrowserImportPlatform {
        match self {
            Self::GameCube { .. } => BrowserImportPlatform::GameCube,
            Self::PlayStation2 { .. } => BrowserImportPlatform::PlayStation2,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::GameCube { title, .. } | Self::PlayStation2 { title, .. } => title,
        }
    }

    /// The one-line "verified local identity" the dialog must display.
    pub fn summary(&self) -> String {
        match self {
            Self::GameCube {
                dolphin_game_id,
                region,
                ..
            } => format!(
                "Verified Dolphin Game ID {dolphin_game_id}{}",
                region
                    .as_deref()
                    .map(|region| format!(" · region code {region}"))
                    .unwrap_or_default()
            ),
            Self::PlayStation2 {
                executable_crc,
                serial,
                region,
                ..
            } => format!(
                "Verified PCSX2 CRC {executable_crc}{}{}",
                serial
                    .as_deref()
                    .map(|serial| format!(" · serial {serial}"))
                    .unwrap_or_default(),
                region
                    .as_deref()
                    .map(|region| format!(" · region {region}"))
                    .unwrap_or_default()
            ),
        }
    }
}

// --- URL construction and browser launch ---------------------------------

/// The exact game-page URL a person should open. Query strings and
/// fragments are never part of it, so no tracking parameter can ride
/// along.
pub fn gamehacking_game_page_url(game_id: u64) -> String {
    format!("{GAMEHACKING_BASE_URL}/game/{game_id}")
}

/// Accepts only a plain `https://gamehacking.org` URL and returns it
/// normalized with any query string and fragment removed. Everything
/// else - another host, a `userinfo@` prefix, a non-default port, any
/// other scheme, a host that merely *ends with* `gamehacking.org` - is
/// refused. This is the single gate every launch and every displayed URL
/// passes through.
pub fn validate_gamehacking_browser_url(value: &str) -> Result<String, BrowserImportError> {
    let value = value.trim();
    // Refused before parsing: `Url::parse` would percent-encode embedded
    // whitespace and control characters into a "valid" URL, which is
    // never what the person meant and is exactly the shape a
    // command-injection attempt has.
    if value.chars().any(|character| {
        character.is_whitespace() || character.is_control() || character == '"' || character == '\''
    }) {
        return Err(import_error(
            BrowserImportErrorKind::InvalidUrl,
            "A GameHacking.org URL cannot contain spaces, quotes, or control characters.",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        import_error(
            BrowserImportErrorKind::InvalidUrl,
            format!("`{value}` is not a valid URL."),
        )
    })?;
    if url.scheme() != "https" {
        return Err(import_error(
            BrowserImportErrorKind::InvalidUrl,
            "Only https:// GameHacking.org URLs can be opened.",
        ));
    }
    if url.host_str() != Some(GAMEHACKING_HOST) {
        return Err(import_error(
            BrowserImportErrorKind::InvalidUrl,
            format!("Only {GAMEHACKING_HOST} URLs can be opened."),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(import_error(
            BrowserImportErrorKind::InvalidUrl,
            "Only a plain https://gamehacking.org URL with no credentials or port can be opened.",
        ));
    }
    let mut normalized = url.clone();
    normalized.set_query(None);
    normalized.set_fragment(None);
    Ok(normalized.to_string())
}

/// Extracts the GameHacking game ID from a `/game/<id>` URL or path,
/// ignoring any trailing slug segment (`/game/501/test-racer`).
pub fn gamehacking_game_id_from_url(value: &str) -> Option<u64> {
    let path = match Url::parse(value.trim()) {
        Ok(url) => {
            if url.host_str() != Some(GAMEHACKING_HOST) {
                return None;
            }
            url.path().to_string()
        }
        Err(_) => value.trim().to_string(),
    };
    let rest = path.trim_start_matches('/').strip_prefix("game/")?;
    rest.split('/').next()?.parse::<u64>().ok()
}

/// The exact program and argument list used to hand a URL to the
/// desktop's own default handler. Returned separately (never as one
/// shell string) so nothing in the URL can be interpreted by a shell -
/// there is no shell in the picture at all.
pub fn gamehacking_browser_launch_command(
    url: &str,
) -> Result<(&'static str, Vec<String>), BrowserImportError> {
    let url = validate_gamehacking_browser_url(url)?;
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Ok((program, vec![url]))
}

/// Starts the desktop handler. Kept behind a trait purely so tests can
/// observe the exact program and arguments, and simulate a failure,
/// without launching anything.
pub trait BrowserLauncher {
    fn launch(&self, program: &str, arguments: &[String]) -> Result<(), String>;
}

/// The real launcher: one process, separate arguments, no shell.
#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopBrowserLauncher;

impl BrowserLauncher for DesktopBrowserLauncher {
    fn launch(&self, program: &str, arguments: &[String]) -> Result<(), String> {
        let status = std::process::Command::new(program)
            .args(arguments)
            .status()
            .map_err(|failure| format!("{program} could not be started: {failure}"))?;
        if !status.success() {
            return Err(match status.code() {
                Some(code) => format!("{program} exited with status {code}"),
                None => format!("{program} was terminated by a signal"),
            });
        }
        Ok(())
    }
}

/// Opens a validated GameHacking.org URL in the person's ordinary
/// browser. Success here is *only* "the handler started" - it is never
/// treated as, or reported as, a successful import: no cache is touched
/// and no provenance is written.
pub fn open_gamehacking_url_in_browser(
    url: &str,
    launcher: &dyn BrowserLauncher,
) -> Result<String, BrowserImportError> {
    let (program, arguments) = gamehacking_browser_launch_command(url)?;
    launcher.launch(program, &arguments).map_err(|failure| {
        import_error(
            BrowserImportErrorKind::BrowserLaunchFailed,
            format!(
                "ArchiveFS could not open your browser ({failure}). Copy the URL and open it manually instead."
            ),
        )
    })?;
    Ok(format!(
        "Opened {} in your browser. Nothing has been imported yet.",
        arguments[0]
    ))
}

// --- Plan (what the dialog must show) ------------------------------------

/// A cache entry that already exists at a destination an import would
/// write, so the dialog can say exactly what would be replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExistingBrowserImportCache {
    pub retrieved_at_unix_seconds: Option<u64>,
    /// `manual_browser_import` when a provenance record is present,
    /// otherwise `live_fetch` - the live provider writes no provenance.
    pub source: String,
    pub imported_from_filename: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserImportDestination {
    pub kind: BrowserImportKind,
    pub cache_file_name: String,
    pub cache_path: PathBuf,
    pub existing: Option<ExistingBrowserImportCache>,
}

/// Everything the "Import through browser" dialog is required to display
/// before any content is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserImportPlan {
    pub platform: BrowserImportPlatform,
    pub platform_label: &'static str,
    pub local_game_title: String,
    pub local_identity_summary: String,
    pub gamehacking_game_id: u64,
    pub expected_source_url: String,
    pub accepted_formats: Vec<&'static str>,
    pub destinations: Vec<BrowserImportDestination>,
}

impl BrowserImportPlan {
    pub fn replaces_existing_cache(&self) -> bool {
        self.destinations
            .iter()
            .any(|destination| destination.existing.is_some())
    }
}

/// Builds the dialog's plan. `source_url` may be the catalogue record's
/// own `source_url` (which can carry a title slug); it is validated and
/// normalized, and falls back to the canonical `/game/<id>` form when it
/// does not name the selected game.
pub fn plan_gamehacking_browser_import(
    platform: BrowserImportPlatform,
    game_id: u64,
    source_url: Option<&str>,
    identity: &BrowserImportLocalIdentity,
    cache_root: &Path,
) -> Result<BrowserImportPlan, BrowserImportError> {
    if identity.platform() != platform {
        return Err(import_error(
            BrowserImportErrorKind::WrongPlatform,
            format!(
                "The verified local identity is {}, not {}.",
                identity.platform().label(),
                platform.label()
            ),
        ));
    }
    let expected_source_url = expected_source_url(game_id, source_url);
    let destinations = platform
        .accepted_kinds()
        .iter()
        .filter_map(|kind| {
            let cache_file_name = kind.cache_file_name(platform, game_id)?;
            let cache_path = cache_root.join(&cache_file_name);
            let existing = describe_existing_cache(&cache_path);
            Some(BrowserImportDestination {
                kind: *kind,
                cache_file_name,
                cache_path,
                existing,
            })
        })
        .collect();
    Ok(BrowserImportPlan {
        platform,
        platform_label: platform.label(),
        local_game_title: identity.title().to_string(),
        local_identity_summary: identity.summary(),
        gamehacking_game_id: game_id,
        expected_source_url,
        accepted_formats: platform.accepted_formats(),
        destinations,
    })
}

fn expected_source_url(game_id: u64, source_url: Option<&str>) -> String {
    source_url
        .and_then(|value| validate_gamehacking_browser_url(value).ok())
        .filter(|value| gamehacking_game_id_from_url(value) == Some(game_id))
        .unwrap_or_else(|| gamehacking_game_page_url(game_id))
}

fn describe_existing_cache(cache_path: &Path) -> Option<ExistingBrowserImportCache> {
    let metadata = cache_path.symlink_metadata().ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let provenance = read_provenance(cache_path);
    Some(ExistingBrowserImportCache {
        retrieved_at_unix_seconds: provenance
            .as_ref()
            .map(|record| record.imported_at_unix_seconds)
            .or_else(|| read_retrieved_at(cache_path)),
        source: provenance
            .as_ref()
            .map(|record| record.source.clone())
            .unwrap_or_else(|| "live_fetch".to_string()),
        imported_from_filename: provenance
            .as_ref()
            .and_then(|record| record.original_filename.clone()),
        sha256: provenance
            .as_ref()
            .map(|record| record.stored_sha256.clone()),
        size_bytes: metadata.len(),
    })
}

// --- Provenance -----------------------------------------------------------

/// What is recorded alongside a manually imported cache entry. Every
/// field is either an ArchiveFS-side fact or content the person
/// deliberately handed over; there is deliberately no field able to hold
/// a cookie, an authorization header, browser history, or a clipboard
/// log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserImportProvenance {
    pub schema_version: u32,
    /// Always [`MANUAL_BROWSER_IMPORT_SOURCE`].
    pub source: String,
    pub imported_at_unix_seconds: u64,
    pub expected_source_url: String,
    /// SHA-256 of exactly what was supplied, before any sanitization.
    pub supplied_sha256: String,
    /// SHA-256 of the bytes actually stored in the cache file. Differs
    /// from `supplied_sha256` only when script/style/frame elements were
    /// stripped from imported HTML.
    pub stored_sha256: String,
    pub platform: BrowserImportPlatform,
    pub gamehacking_game_id: u64,
    pub import_kind: BrowserImportKind,
    pub local_identity: BrowserImportLocalIdentity,
    /// The file's own name when imported from a file - never used as
    /// identity evidence, recorded only so a person can tell two imports
    /// apart.
    pub original_filename: Option<String>,
    pub parser_schema_version: u32,
    pub cache_file_name: String,
    /// The identity checks that actually found evidence and passed.
    pub verified_evidence: Vec<String>,
}

fn provenance_path(cache_path: &Path) -> PathBuf {
    sidecar_path(cache_path, "import.json")
}

fn replaced_backup_path(cache_path: &Path) -> PathBuf {
    sidecar_path(cache_path, "replaced")
}

fn charset_sidecar_path(cache_path: &Path) -> PathBuf {
    sidecar_path(cache_path, "charset")
}

/// The provider's own sidecar convention: `<file-name>.<suffix>` beside
/// the cache file, never a rename of it.
fn sidecar_path(cache_path: &Path, suffix: &str) -> PathBuf {
    let file_name = cache_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("response");
    cache_path.with_file_name(format!("{file_name}.{suffix}"))
}

/// Mirrors `gamehacking_gamecube_provider::retrieved_cache_path`, whose
/// suffix replaces the extension rather than appending to the file name.
fn retrieved_sidecar_path(cache_path: &Path) -> PathBuf {
    cache_path.with_extension(format!(
        "{}.retrieved",
        cache_path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("cache")
    ))
}

fn read_provenance(cache_path: &Path) -> Option<BrowserImportProvenance> {
    let bytes = bounded_import_read(&provenance_path(cache_path), 64 * 1024).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Reads an existing manual-import provenance record, if the cache entry
/// at `cache_path` has one. Used by callers that need to tell an imported
/// cache entry from a live-fetched one.
pub fn read_browser_import_provenance(cache_path: &Path) -> Option<BrowserImportProvenance> {
    read_provenance(cache_path)
}

fn read_retrieved_at(cache_path: &Path) -> Option<u64> {
    let bytes = bounded_import_read(&retrieved_sidecar_path(cache_path), 64).ok()?;
    String::from_utf8_lossy(&bytes).trim().parse::<u64>().ok()
}

// --- Request and outcome --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserImportTextOrigin {
    /// Read from the OS clipboard, only ever after an explicit click.
    Clipboard,
    /// Typed or pasted into ArchiveFS's own text area.
    PastedText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserImportSource {
    File(PathBuf),
    Text {
        text: String,
        origin: BrowserImportTextOrigin,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserImportRequest {
    pub platform: BrowserImportPlatform,
    pub game_id: u64,
    /// The selected candidate's own page URL, when known. Validated, and
    /// replaced by the canonical `/game/<id>` form if it does not name
    /// this game.
    pub source_url: Option<String>,
    /// The selected GameHacking candidate's title - the only identity
    /// evidence a PCSX2 pnach export carries at all.
    pub candidate_title: String,
    pub identity: BrowserImportLocalIdentity,
    pub cache_root: PathBuf,
    /// `None` detects the kind from the content itself. An explicit kind
    /// is honoured, and still fully validated.
    pub kind: Option<BrowserImportKind>,
    pub source: BrowserImportSource,
}

/// What a successful import produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserImportOutcome {
    pub platform: BrowserImportPlatform,
    pub gamehacking_game_id: u64,
    pub kind: BrowserImportKind,
    /// The title the imported content itself carried, when it carried one.
    pub imported_title: Option<String>,
    pub cache_path: PathBuf,
    pub provenance_path: PathBuf,
    pub provenance: BrowserImportProvenance,
    pub replaced_existing_cache: bool,
    /// What was at the destination before, when something was.
    pub replaced: Option<ExistingBrowserImportCache>,
    pub backup_path: Option<PathBuf>,
    pub cheat_count: usize,
    pub action_replay_count: usize,
    pub gecko_count: usize,
    pub raw_unknown_count: usize,
    pub unsupported_count: usize,
    /// Present when a GameCube import was enriched against the *other*
    /// already-imported artefact for the same game (page labels applied
    /// to an export, or an export re-counted against a new page).
    pub enriched_from_cache: Option<PathBuf>,
    pub verified_evidence: Vec<String>,
}

impl BrowserImportOutcome {
    /// The exact success headline the GUI shows.
    pub fn headline(&self) -> &'static str {
        "Browser import successful"
    }
}

// --- The import itself ----------------------------------------------------

/// Validates and imports one piece of browser-supplied GameHacking.org
/// content into the exact cache key the live provider would have written.
///
/// Ordering matters and is deliberate: size and emptiness first (cheapest
/// and safest), then the shared Cloudflare classifier, then "is this
/// GameHacking content at all", then "is it *this* game on *this*
/// system", then the real parser. Nothing is written until every one of
/// those has passed, so a bad import can never damage a good cache.
pub fn import_gamehacking_browser_content(
    request: &BrowserImportRequest,
) -> Result<BrowserImportOutcome, BrowserImportError> {
    if request.identity.platform() != request.platform {
        return Err(import_error(
            BrowserImportErrorKind::WrongPlatform,
            format!(
                "The verified local identity is {}, not {}.",
                request.identity.platform().label(),
                request.platform.label()
            ),
        ));
    }
    let (supplied, original_filename) = read_import_source(&request.source)?;
    if supplied.len() > MAX_BROWSER_IMPORT_BYTES {
        return Err(import_error(
            BrowserImportErrorKind::InputTooLarge,
            format!(
                "That import is {} bytes, over the {} byte limit. A real GameHacking.org page or export is far smaller.",
                supplied.len(),
                MAX_BROWSER_IMPORT_BYTES
            ),
        ));
    }
    // The same shared classifier the live provider uses, applied to a
    // body with no HTTP status of its own - exactly the HTTP-200 "Just a
    // moment" case. A challenge page is never GameHacking content.
    if cached_bytes_are_cloudflare_challenge(&supplied) {
        return Err(import_error(
            BrowserImportErrorKind::ChallengeContent,
            "That looks like the Cloudflare \"checking your browser\" page, not a GameHacking.org game page. Wait for the real page to finish loading in your browser, then save or copy it again.",
        ));
    }
    // Decoded once, and trimmed once, so a saved file and a paste of the
    // same page produce byte-identical stored content.
    let text = std::str::from_utf8(&supplied)
        .map(|text| strip_bom(text).trim())
        .map_err(|_| {
            import_error(
                BrowserImportErrorKind::NotText,
                "That import is not UTF-8 text. Save the page as HTML, or copy the Text export.",
            )
        })?;
    if text.is_empty() {
        return Err(match &request.source {
            BrowserImportSource::Text {
                origin: BrowserImportTextOrigin::Clipboard,
                ..
            } => import_error(
                BrowserImportErrorKind::ClipboardEmpty,
                "The clipboard held no text. Copy the game page or Text export first, then paste again.",
            ),
            _ => import_error(
                BrowserImportErrorKind::EmptyInput,
                "There was nothing to import. Save or copy the GameHacking.org page first.",
            ),
        });
    }
    let kind = request.kind.unwrap_or_else(|| detect_import_kind(text));
    let Some(cache_file_name) = kind.cache_file_name(request.platform, request.game_id) else {
        return Err(import_error(
            BrowserImportErrorKind::UnsupportedPageShape,
            format!(
                "ArchiveFS has no {} cache for {}: the {} provider reads only the cheat export. Use the page's own export button and import the downloaded file instead.",
                kind.label(),
                request.platform.label(),
                request.platform.label()
            ),
        ));
    };
    let expected_source_url = expected_source_url(request.game_id, request.source_url.as_deref());

    let validated = match (request.platform, kind) {
        (BrowserImportPlatform::GameCube, BrowserImportKind::GamePageHtml) => {
            validate_gamecube_page(request, text)?
        }
        (BrowserImportPlatform::GameCube, BrowserImportKind::TextExport) => {
            validate_gamecube_export(request, text, &expected_source_url)?
        }
        (BrowserImportPlatform::PlayStation2, BrowserImportKind::TextExport) => {
            validate_ps2_export(request, text, &expected_source_url)?
        }
        (BrowserImportPlatform::PlayStation2, BrowserImportKind::GamePageHtml) => {
            unreachable!("cache_file_name already refused this combination")
        }
    };

    write_validated_import(
        request,
        kind,
        &cache_file_name,
        &expected_source_url,
        &supplied,
        original_filename,
        validated,
    )
}

/// The parsed, identity-checked result of one import, plus the exact
/// bytes to store.
struct ValidatedImport {
    stored: Vec<u8>,
    imported_title: Option<String>,
    cheat_count: usize,
    action_replay_count: usize,
    gecko_count: usize,
    raw_unknown_count: usize,
    unsupported_count: usize,
    enriched_from_cache: Option<PathBuf>,
    verified_evidence: Vec<String>,
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Distinguishes HTML from a flat cheat export by the content itself, not
/// by the file's extension - a `.txt` holding a saved page is still a
/// page, which is exactly what the accepted-formats note promises.
fn detect_import_kind(text: &str) -> BrowserImportKind {
    let head: String = text
        .chars()
        .take(4096)
        .collect::<String>()
        .to_ascii_lowercase();
    let html_markers = [
        "<!doctype html",
        "<html",
        "<body",
        "<table",
        "<div",
        "<form",
        "<span",
    ];
    if html_markers.iter().any(|marker| head.contains(marker)) {
        BrowserImportKind::GamePageHtml
    } else {
        BrowserImportKind::TextExport
    }
}

// --- Reading the supplied bytes ------------------------------------------

fn read_import_source(
    source: &BrowserImportSource,
) -> Result<(Vec<u8>, Option<String>), BrowserImportError> {
    match source {
        BrowserImportSource::File(path) => {
            let bytes = bounded_import_read(path, MAX_BROWSER_IMPORT_BYTES).map_err(|failure| {
                // A file that is merely too big must say so, not read as
                // an unreadable file.
                if failure.kind == BrowserImportErrorKind::InputTooLarge {
                    failure
                } else {
                    import_error(
                        BrowserImportErrorKind::SourceUnreadable,
                        format!("{} could not be read: {}", path.display(), failure.detail),
                    )
                }
            })?;
            let original_filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            Ok((bytes, original_filename))
        }
        BrowserImportSource::Text { text, .. } => Ok((text.as_bytes().to_vec(), None)),
    }
}

/// Bounded, symlink-refusing read, matching the provider's own
/// `bounded_read` rules: a real file, never a symlink, never larger than
/// the caller's ceiling. Paths containing spaces are ordinary paths here -
/// nothing is ever interpolated into a command line.
fn bounded_import_read(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, BrowserImportError> {
    let metadata = path.symlink_metadata().map_err(|failure| {
        import_error(
            BrowserImportErrorKind::SourceUnreadable,
            format!("{failure}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(import_error(
            BrowserImportErrorKind::SourceUnreadable,
            "ArchiveFS does not follow symlinks when importing a file.",
        ));
    }
    if !metadata.is_file() {
        return Err(import_error(
            BrowserImportErrorKind::SourceUnreadable,
            "that path is not a regular file",
        ));
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(import_error(
            BrowserImportErrorKind::InputTooLarge,
            format!(
                "{} is {} bytes, over the {maximum_bytes} byte limit.",
                path.display(),
                metadata.len()
            ),
        ));
    }
    fs::read(path).map_err(|failure| {
        import_error(
            BrowserImportErrorKind::SourceUnreadable,
            format!("{failure}"),
        )
    })
}

// --- HTML sanitization ---------------------------------------------------

/// Elements removed, with their whole content, before anything is stored.
/// None of them can carry a cheat entry, a format label, a code body, or
/// an export-form field, so removing them cannot affect what the existing
/// page parser sees.
const STRIPPED_ELEMENTS: [&str; 7] = [
    "script", "style", "noscript", "iframe", "frame", "object", "embed",
];

/// Removes active-content and framing elements plus HTML comments from
/// imported page source.
///
/// This is defence in depth, not the primary protection: ArchiveFS never
/// renders imported HTML in a web view at all - `scraper` parses it into
/// text and attributes exactly as the live provider already parses a
/// fetched page - so an inline handler attribute is inert either way. It
/// matters because the *stored cache file* should not contain third-party
/// script or tracking markup that a person might later open in a browser
/// themselves.
pub fn sanitize_imported_html(text: &str) -> String {
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
            match after.find("-->") {
                Some(end) => {
                    rest = &after[end + 3..];
                    continue;
                }
                None => break,
            }
        }
        let lowered_tail: String = tail
            .chars()
            .take(16)
            .collect::<String>()
            .to_ascii_lowercase();
        for element in STRIPPED_ELEMENTS {
            let open_marker = format!("<{element}");
            if lowered_tail.starts_with(&open_marker)
                && tail[open_marker.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '-')
            {
                let closing = format!("</{element}");
                rest = match find_ignore_ascii_case(tail, &closing) {
                    Some(close_start) => {
                        let after_close = &tail[close_start..];
                        match after_close.find('>') {
                            Some(end) => &after_close[end + 1..],
                            None => "",
                        }
                    }
                    // Self-closing or unterminated: drop just this tag.
                    None => match tail.find('>') {
                        Some(end) => &tail[end + 1..],
                        None => "",
                    },
                };
                continue 'outer;
            }
        }
        // An ordinary tag: copy it through verbatim.
        match tail.find('>') {
            Some(end) => {
                output.push_str(&tail[..=end]);
                rest = &tail[end + 1..];
            }
            None => {
                output.push_str(tail);
                break;
            }
        }
    }
    output
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    let lowered = haystack.to_ascii_lowercase();
    lowered.find(&needle.to_ascii_lowercase())
}

// --- Evidence extraction -------------------------------------------------

/// Everything a saved GameHacking.org page can prove about itself. Every
/// field is either absent (no evidence, so nothing to contradict) or a
/// hard fact read from the markup - never inferred from the file name.
#[derive(Debug, Default)]
struct ImportedPageEvidence {
    canonical_game_id: Option<u64>,
    hidden_game_id: Option<u64>,
    hidden_system_id: Option<u16>,
    linked_game_ids: BTreeSet<u64>,
    system_slugs: BTreeSet<String>,
    /// Dolphin-Game-ID-shaped tokens, read only from the export form's own
    /// filename options and from a `Game ID` info row.
    dolphin_game_ids: BTreeSet<String>,
    ps2_serials: BTreeSet<String>,
    systems: BTreeSet<String>,
    title: Option<String>,
    has_export_form: bool,
    mentions_gamehacking: bool,
}

fn extract_page_evidence(html: &str) -> ImportedPageEvidence {
    let document = Html::parse_document(html);
    let mut evidence = ImportedPageEvidence::default();

    let lowered = html.to_ascii_lowercase();
    evidence.mentions_gamehacking = lowered.contains("gamehacking.org");

    if let Ok(selector) = Selector::parse("link[rel~=\"canonical\"], link[rel=\"canonical\"]") {
        evidence.canonical_game_id = document
            .select(&selector)
            .filter_map(|node| node.value().attr("href"))
            .find_map(gamehacking_game_id_from_url);
    }
    if evidence.canonical_game_id.is_none()
        && let Ok(selector) = Selector::parse("meta[property=\"og:url\"]")
    {
        evidence.canonical_game_id = document
            .select(&selector)
            .filter_map(|node| node.value().attr("content"))
            .find_map(gamehacking_game_id_from_url);
    }
    if let Ok(selector) = Selector::parse("a[href]") {
        for node in document.select(&selector) {
            let Some(href) = node.value().attr("href") else {
                continue;
            };
            if let Some(game_id) = gamehacking_game_id_from_url(href) {
                evidence.linked_game_ids.insert(game_id);
            }
            if let Some(slug) = system_slug_from_href(href) {
                evidence.system_slugs.insert(slug);
            }
        }
    }
    if let (Ok(form_selector), Ok(input_selector), Ok(option_selector)) = (
        Selector::parse("form"),
        Selector::parse("input"),
        Selector::parse("select[name=\"filename\"] option"),
    ) {
        for form in document.select(&form_selector) {
            let is_export_form = form
                .value()
                .attr("action")
                .is_some_and(|action| action.to_ascii_lowercase().contains("exportcodes.php"));
            if !is_export_form {
                continue;
            }
            evidence.has_export_form = true;
            for input in form.select(&input_selector) {
                let Some(name) = input.value().attr("name") else {
                    continue;
                };
                let value = input.value().attr("value").unwrap_or_default().trim();
                if name.eq_ignore_ascii_case("gamID") {
                    evidence.hidden_game_id = value.parse::<u64>().ok();
                } else if name.eq_ignore_ascii_case("sysID") {
                    evidence.hidden_system_id = value.parse::<u16>().ok();
                }
            }
            for option in form.select(&option_selector) {
                let value = option.value().attr("value").unwrap_or_default();
                if let Some(game_id) = normalize_gamecube_game_id(value) {
                    evidence.dolphin_game_ids.insert(game_id);
                }
                if let Some(serial) = normalize_ps2_serial(value) {
                    evidence.ps2_serials.insert(serial);
                }
            }
        }
    }
    if let Ok(selector) = Selector::parse("h1, h2.game-title, .game-title") {
        evidence.title = document
            .select(&selector)
            .map(|node| compact_text(&node.text().collect::<String>()))
            .find(|value| !value.is_empty());
    }
    if let Ok(selector) = Selector::parse("tr, dt, dd, .game-info-row, li") {
        for row in document.select(&selector) {
            let compact = compact_text(&row.text().collect::<Vec<_>>().join(" "));
            for (label, sink) in [
                ("Game ID", RowSink::DolphinGameId),
                ("ID", RowSink::DolphinGameId),
                ("Serial", RowSink::Ps2Serial),
                ("System", RowSink::System),
            ] {
                let Some(value) = strip_info_label(&compact, label) else {
                    continue;
                };
                match sink {
                    RowSink::DolphinGameId => {
                        if let Some(game_id) = normalize_gamecube_game_id(value) {
                            evidence.dolphin_game_ids.insert(game_id);
                        }
                    }
                    RowSink::Ps2Serial => {
                        if let Some(serial) = normalize_ps2_serial(value) {
                            evidence.ps2_serials.insert(serial);
                        }
                    }
                    RowSink::System => {
                        evidence.systems.insert(value.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    evidence
}

enum RowSink {
    DolphinGameId,
    Ps2Serial,
    System,
}

fn compact_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reads `"<Label>: <value>"` / `"<Label> <value>"` out of one compacted
/// info row, stopping at a `|` separator exactly as
/// `parse_gamehacking_game_page` already does.
fn strip_info_label<'a>(compact: &'a str, label: &str) -> Option<&'a str> {
    let rest = compact.get(..label.len()).and_then(|head| {
        head.eq_ignore_ascii_case(label)
            .then(|| &compact[label.len()..])
    })?;
    rest.trim_start_matches([':', ' '])
        .split('|')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn system_slug_from_href(href: &str) -> Option<String> {
    let path = match Url::parse(href) {
        Ok(url) if url.host_str() == Some(GAMEHACKING_HOST) => url.path().to_string(),
        Ok(_) => return None,
        Err(_) => href.to_string(),
    };
    let rest = path.trim_start_matches('/').strip_prefix("system/")?;
    let slug = rest.split('/').next()?.trim();
    (!slug.is_empty()).then(|| slug.to_ascii_lowercase())
}

// --- GameCube validation -------------------------------------------------

fn validate_gamecube_page(
    request: &BrowserImportRequest,
    text: &str,
) -> Result<ValidatedImport, BrowserImportError> {
    let sanitized = sanitize_imported_html(text);
    let evidence = extract_page_evidence(&sanitized);
    let mut verified = Vec::new();
    check_is_gamehacking_page(&evidence)?;
    verified.push(gamehacking_evidence_note(&evidence));
    // Platform first: "this is the wrong console" is more useful to a
    // person than "this is the wrong game", and a page from another
    // system is always also a different game ID.
    check_page_platform(&evidence, request.platform)?;
    verified.push(format!(
        "{} confirmed by the page's own system markers",
        request.platform.label()
    ));
    check_page_game_id(&evidence, request.game_id)?;
    verified.push(format!(
        "GameHacking game ID {} confirmed by the page itself",
        request.game_id
    ));

    let BrowserImportLocalIdentity::GameCube {
        dolphin_game_id, ..
    } = &request.identity
    else {
        unreachable!("platform agreement was checked before validation");
    };
    if !evidence.dolphin_game_ids.is_empty() {
        if !evidence.dolphin_game_ids.contains(dolphin_game_id) {
            return Err(import_error(
                BrowserImportErrorKind::GameIdMismatch,
                format!(
                    "That page is for Dolphin Game ID {}, but the selected local game is verified as {dolphin_game_id}.",
                    evidence
                        .dolphin_game_ids
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        verified.push(format!(
            "Dolphin Game ID {dolphin_game_id} confirmed by the page itself"
        ));
    }

    let summary = summarize_gamecube_game_page(sanitized.as_bytes());
    if summary.entry_count == 0 {
        return Err(import_error(
            BrowserImportErrorKind::UnsupportedPageShape,
            "That page has no GameHacking.org cheat entries ArchiveFS can read. Save the game's own \"Codes\" page, not a search result or a category listing.",
        ));
    }

    // Staged workflow: if this game's Text export was already imported,
    // re-apply the freshly imported page's labels to it so the reported
    // counts are the real, enriched ones.
    let export_path = BrowserImportKind::TextExport
        .cache_file_name(request.platform, request.game_id)
        .map(|name| request.cache_root.join(name));
    let mut enriched_from_cache = None;
    let mut format_counts = FormatCounts {
        action_replay: summary.action_replay_count,
        gecko: summary.gecko_count,
        raw_unknown: summary.unlabelled_count,
        unsupported: 0,
    };
    let mut cheat_count = summary.entry_count;
    if let Some(export_path) = export_path.filter(|path| path.is_file())
        && let Ok(export_bytes) = bounded_import_read(&export_path, MAX_BROWSER_IMPORT_BYTES)
        && !cached_bytes_are_cloudflare_challenge(&export_bytes)
        && let Ok(mut cheats) =
            parse_gamehacking_gamecube_export(&gamecube_game(request), &export_bytes)
    {
        apply_gamecube_page_format_labels(&mut cheats, sanitized.as_bytes());
        format_counts = count_gamecube_formats(&cheats);
        cheat_count = cheats.len();
        enriched_from_cache = Some(export_path);
    }

    Ok(ValidatedImport {
        stored: sanitized.into_bytes(),
        imported_title: evidence.title.clone(),
        cheat_count,
        action_replay_count: format_counts.action_replay,
        gecko_count: format_counts.gecko,
        raw_unknown_count: format_counts.raw_unknown,
        unsupported_count: format_counts.unsupported,
        enriched_from_cache,
        verified_evidence: verified,
    })
}

fn validate_gamecube_export(
    request: &BrowserImportRequest,
    text: &str,
    expected_source_url: &str,
) -> Result<ValidatedImport, BrowserImportError> {
    let BrowserImportLocalIdentity::GameCube {
        dolphin_game_id, ..
    } = &request.identity
    else {
        unreachable!("platform agreement was checked before validation");
    };
    let mut lines = text.lines();
    let header = lines
        .next()
        .map(str::trim)
        .and_then(normalize_gamecube_game_id)
        .ok_or_else(|| {
            import_error(
                BrowserImportErrorKind::MissingIdentityEvidence,
                "That export has no Dolphin Game ID header line, so ArchiveFS cannot tell which game it belongs to. Export it again with the \"Plain Text\" format from the game's own page.",
            )
        })?;
    if &header != dolphin_game_id {
        return Err(import_error(
            BrowserImportErrorKind::GameIdMismatch,
            format!(
                "That export is for Dolphin Game ID {header}, but the selected local game is verified as {dolphin_game_id}."
            ),
        ));
    }
    let imported_title = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let game = gamecube_game(request);
    let mut cheats =
        parse_gamehacking_gamecube_export(&game, text.as_bytes()).map_err(|failure| {
            import_error(
                BrowserImportErrorKind::MalformedExport,
                format!(
                    "ArchiveFS could not read any cheats out of that export: {}",
                    failure.detail
                ),
            )
        })?;

    let mut verified = vec![
        format!("Dolphin Game ID {dolphin_game_id} confirmed by the export's own header line"),
        format!("expected source {expected_source_url}"),
    ];

    // Staged workflow: enrich straight away from an already-imported (or
    // previously fetched) game page for the same game, so format labels
    // survive into the reported counts and the installable selection.
    let page_path = BrowserImportKind::GamePageHtml
        .cache_file_name(request.platform, request.game_id)
        .map(|name| request.cache_root.join(name));
    let mut enriched_from_cache = None;
    if let Some(page_path) = page_path.filter(|path| path.is_file())
        && let Ok(page_bytes) = bounded_import_read(&page_path, MAX_BROWSER_IMPORT_BYTES)
        && !cached_bytes_are_cloudflare_challenge(&page_bytes)
    {
        let upgraded = apply_gamecube_page_format_labels(&mut cheats, &page_bytes);
        if upgraded > 0 {
            verified.push(format!(
                "{upgraded} cheat format label(s) taken from the already-imported game page"
            ));
        }
        enriched_from_cache = Some(page_path);
    }

    let counts = count_gamecube_formats(&cheats);
    Ok(ValidatedImport {
        stored: text.as_bytes().to_vec(),
        imported_title,
        cheat_count: cheats.len(),
        action_replay_count: counts.action_replay,
        gecko_count: counts.gecko,
        raw_unknown_count: counts.raw_unknown,
        unsupported_count: counts.unsupported,
        enriched_from_cache,
        verified_evidence: verified,
    })
}

fn gamecube_game(request: &BrowserImportRequest) -> GameHackingGameCubeGame {
    let (dolphin_game_id, region) = match &request.identity {
        BrowserImportLocalIdentity::GameCube {
            dolphin_game_id,
            region,
            ..
        } => (Some(dolphin_game_id.clone()), region.clone()),
        BrowserImportLocalIdentity::PlayStation2 { .. } => (None, None),
    };
    GameHackingGameCubeGame {
        game_id: request.game_id,
        title: request.candidate_title.clone(),
        system: "GameCube".to_string(),
        region,
        dolphin_game_id,
        revision: None,
        hash: None,
        source_url: expected_source_url(request.game_id, request.source_url.as_deref()),
    }
}

struct FormatCounts {
    action_replay: usize,
    gecko: usize,
    raw_unknown: usize,
    unsupported: usize,
}

fn count_gamecube_formats(cheats: &[GameHackingGameCubeCheat]) -> FormatCounts {
    use super::gamehacking_gamecube_provider::GameCubeCodeFormat;
    let mut counts = FormatCounts {
        action_replay: 0,
        gecko: 0,
        raw_unknown: 0,
        unsupported: 0,
    };
    for cheat in cheats {
        match cheat.code_format {
            GameCubeCodeFormat::ActionReplay => counts.action_replay += 1,
            GameCubeCodeFormat::Gecko => counts.gecko += 1,
            GameCubeCodeFormat::RawUnknown => counts.raw_unknown += 1,
            GameCubeCodeFormat::Unsupported => counts.unsupported += 1,
        }
    }
    counts
}

// --- PlayStation 2 validation -------------------------------------------

/// A PCSX2 export carries exactly two things that can tie it to a game:
/// GameHacking's own generator comment, and a title comment. Neither the
/// serial nor the CRC appears in the format at all, so the tie is made to
/// the *selected candidate's* title (which the local verified identity
/// already had to match for that candidate to be selectable) - never to
/// the file name.
fn validate_ps2_export(
    request: &BrowserImportRequest,
    text: &str,
    expected_source_url: &str,
) -> Result<ValidatedImport, BrowserImportError> {
    let comments: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("//"))
        .map(|comment| comment.trim().to_string())
        .filter(|comment| !comment.is_empty())
        .collect();
    let generated_marker = comments.iter().any(|comment| {
        comment
            .to_ascii_lowercase()
            .starts_with("file generated by gamehacking.org")
    });
    let title_comment = comments
        .iter()
        .find(|comment| title_comment_matches(&request.candidate_title, comment))
        .cloned();
    if !generated_marker && title_comment.is_none() {
        return Err(import_error(
            BrowserImportErrorKind::MissingIdentityEvidence,
            format!(
                "That export carries no GameHacking.org marker and no `// {}` title comment, so ArchiveFS cannot tell which game it belongs to. Use the game page's own PCSX2 export.",
                request.candidate_title
            ),
        ));
    }
    if generated_marker && title_comment.is_none() {
        let other_title = comments
            .iter()
            .find(|comment| {
                !comment
                    .to_ascii_lowercase()
                    .starts_with("file generated by gamehacking.org")
            })
            .cloned();
        return Err(match other_title {
            Some(other) => import_error(
                BrowserImportErrorKind::WrongGame,
                format!(
                    "That is a GameHacking.org export for `{other}`, but the selected GameHacking game is `{}`.",
                    request.candidate_title
                ),
            ),
            None => import_error(
                BrowserImportErrorKind::MissingIdentityEvidence,
                format!(
                    "That GameHacking.org export has no title comment, so ArchiveFS cannot confirm it is `{}`.",
                    request.candidate_title
                ),
            ),
        });
    }

    let game = GameHackingGame {
        game_id: request.game_id,
        title: request.candidate_title.clone(),
        system: "PlayStation 2".to_string(),
        region: match &request.identity {
            BrowserImportLocalIdentity::PlayStation2 { region, .. } => region.clone(),
            BrowserImportLocalIdentity::GameCube { .. } => None,
        },
        serial: match &request.identity {
            BrowserImportLocalIdentity::PlayStation2 { serial, .. } => serial.clone(),
            BrowserImportLocalIdentity::GameCube { .. } => None,
        },
        crc: match &request.identity {
            BrowserImportLocalIdentity::PlayStation2 { executable_crc, .. } => {
                Some(executable_crc.clone())
            }
            BrowserImportLocalIdentity::GameCube { .. } => None,
        },
        source_url: expected_source_url.to_string(),
    };
    let cheats: Vec<GameHackingCheat> =
        parse_gamehacking_pnach(&game, text.as_bytes()).map_err(|failure| {
            import_error(
                BrowserImportErrorKind::MalformedExport,
                format!(
                    "ArchiveFS could not read any cheats out of that PCSX2 export: {}",
                    failure.detail
                ),
            )
        })?;
    if cheats.is_empty() {
        return Err(import_error(
            BrowserImportErrorKind::MalformedExport,
            "That PCSX2 export contained no `patch=` lines ArchiveFS could read.",
        ));
    }
    let mut verified = vec![format!(
        "GameHacking game `{}` confirmed by the export's own title comment",
        request.candidate_title
    )];
    if generated_marker {
        verified.push("GameHacking.org generator marker present".to_string());
    }
    verified.push(format!("expected source {expected_source_url}"));
    Ok(ValidatedImport {
        stored: text.as_bytes().to_vec(),
        imported_title: title_comment,
        cheat_count: cheats.len(),
        action_replay_count: 0,
        gecko_count: 0,
        raw_unknown_count: 0,
        unsupported_count: 0,
        enriched_from_cache: None,
        verified_evidence: verified,
    })
}

/// The same rule `gamehacking_provider::is_generated_pnach_comment`
/// applies to recognise its own generated title comment - `"<Title>"` or
/// `"<Title> (<Region>)"` - compared on normalized text so punctuation
/// and case cannot cause a false mismatch.
fn title_comment_matches(candidate_title: &str, comment: &str) -> bool {
    let expected = normalized_title(candidate_title);
    if expected.is_empty() {
        return false;
    }
    let actual = normalized_title(comment);
    actual == expected || actual.starts_with(&expected)
}

fn normalized_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

// --- Shared page checks --------------------------------------------------

fn check_is_gamehacking_page(evidence: &ImportedPageEvidence) -> Result<(), BrowserImportError> {
    let has_evidence = evidence.canonical_game_id.is_some()
        || evidence.hidden_game_id.is_some()
        || evidence.has_export_form
        || !evidence.linked_game_ids.is_empty()
        || !evidence.system_slugs.is_empty();
    if has_evidence {
        return Ok(());
    }
    Err(import_error(
        BrowserImportErrorKind::UnrelatedContent,
        if evidence.mentions_gamehacking {
            "That page mentions GameHacking.org but has no game link, export form, or canonical game URL, so it is not an individual game page."
        } else {
            "That is not a GameHacking.org page. Open the game's own page in your browser and save or copy that."
        },
    ))
}

fn gamehacking_evidence_note(evidence: &ImportedPageEvidence) -> String {
    let mut parts = Vec::new();
    if evidence.canonical_game_id.is_some() {
        parts.push("canonical game URL");
    }
    if evidence.hidden_game_id.is_some() {
        parts.push("export form gamID");
    }
    if evidence.has_export_form {
        parts.push("cheat-export form");
    }
    if !evidence.linked_game_ids.is_empty() {
        parts.push("game links");
    }
    format!("GameHacking.org page evidence: {}", parts.join(", "))
}

/// The page's *own* game ID, in decreasing order of authority. Only
/// falls back to plain `/game/<id>` links when the page names exactly one
/// game - a listing that links many games proves nothing about which one
/// it is, and is refused as an unrelated page instead.
fn check_page_game_id(
    evidence: &ImportedPageEvidence,
    expected: u64,
) -> Result<(), BrowserImportError> {
    let authoritative = evidence.canonical_game_id.or(evidence.hidden_game_id);
    if let Some(found) = authoritative {
        if found == expected {
            return Ok(());
        }
        return Err(import_error(
            BrowserImportErrorKind::WrongGame,
            format!(
                "That is GameHacking.org game {found}, but the selected candidate is game {expected}. Change the selected GameHacking candidate if you meant to import that game."
            ),
        ));
    }
    if evidence.linked_game_ids.contains(&expected) {
        return Ok(());
    }
    if evidence.linked_game_ids.is_empty() {
        return Err(import_error(
            BrowserImportErrorKind::MissingIdentityEvidence,
            format!(
                "That page carries no GameHacking game ID at all, so ArchiveFS cannot confirm it is game {expected}. Save the complete game page rather than a fragment."
            ),
        ));
    }
    Err(import_error(
        BrowserImportErrorKind::WrongGame,
        format!(
            "That page is for GameHacking.org game {}, not the selected game {expected}.",
            evidence
                .linked_game_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

fn check_page_platform(
    evidence: &ImportedPageEvidence,
    platform: BrowserImportPlatform,
) -> Result<(), BrowserImportError> {
    if let Some(found) = evidence.hidden_system_id
        && found != platform.system_id()
    {
        return Err(import_error(
            BrowserImportErrorKind::WrongPlatform,
            format!(
                "That page's export form is for GameHacking.org system {found}, not {} (system {}).",
                platform.label(),
                platform.system_id()
            ),
        ));
    }
    let expected_slug = platform.gamehacking_system_slug();
    if !evidence.system_slugs.is_empty() && !evidence.system_slugs.contains(expected_slug) {
        return Err(import_error(
            BrowserImportErrorKind::WrongPlatform,
            format!(
                "That page belongs to GameHacking.org system `{}`, not `{expected_slug}` ({}).",
                evidence
                    .system_slugs
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("`, `"),
                platform.label()
            ),
        ));
    }
    let expected_system_words: &[&str] = match platform {
        BrowserImportPlatform::GameCube => &["gamecube", "game cube", "nintendo gamecube"],
        BrowserImportPlatform::PlayStation2 => &["playstation 2", "playstation2", "ps2"],
    };
    if !evidence.systems.is_empty()
        && !evidence.systems.iter().any(|system| {
            expected_system_words
                .iter()
                .any(|word| system.contains(word))
        })
    {
        return Err(import_error(
            BrowserImportErrorKind::WrongPlatform,
            format!(
                "That page's own System field says `{}`, not {}.",
                evidence
                    .systems
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("`, `"),
                platform.label()
            ),
        ));
    }
    Ok(())
}

// --- Writing --------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn write_validated_import(
    request: &BrowserImportRequest,
    kind: BrowserImportKind,
    cache_file_name: &str,
    expected_source_url: &str,
    supplied: &[u8],
    original_filename: Option<String>,
    validated: ValidatedImport,
) -> Result<BrowserImportOutcome, BrowserImportError> {
    prepare_import_cache(&request.cache_root)?;
    let cache_path = request.cache_root.join(cache_file_name);
    let replaced = describe_existing_cache(&cache_path);
    let backup_path = if replaced.is_some() {
        let backup_path = replaced_backup_path(&cache_path);
        let existing = bounded_import_read(&cache_path, MAX_BROWSER_IMPORT_BYTES)?;
        atomic_import_write(&backup_path, &existing)?;
        Some(backup_path)
    } else {
        None
    };

    let provenance = BrowserImportProvenance {
        schema_version: BROWSER_IMPORT_PROVENANCE_SCHEMA_VERSION,
        source: MANUAL_BROWSER_IMPORT_SOURCE.to_string(),
        imported_at_unix_seconds: unix_seconds_now(),
        expected_source_url: expected_source_url.to_string(),
        supplied_sha256: sha256_hex(supplied),
        stored_sha256: sha256_hex(&validated.stored),
        platform: request.platform,
        gamehacking_game_id: request.game_id,
        import_kind: kind,
        local_identity: request.identity.clone(),
        original_filename,
        parser_schema_version: BROWSER_IMPORT_PARSER_SCHEMA_VERSION,
        cache_file_name: cache_file_name.to_string(),
        verified_evidence: validated.verified_evidence.clone(),
    };
    let provenance_bytes = serde_json::to_vec_pretty(&provenance).map_err(|failure| {
        import_error(
            BrowserImportErrorKind::CacheWriteFailed,
            format!("import provenance could not be encoded: {failure}"),
        )
    })?;

    // Content first, then the metadata that describes it: the cache file
    // is only ever replaced by bytes that already passed every check.
    atomic_import_write(&cache_path, &validated.stored)?;
    // Imported content is UTF-8 by construction (it was validated as
    // UTF-8 above), so the provider's charset sidecar records exactly
    // that rather than being left to guess.
    atomic_import_write(&charset_sidecar_path(&cache_path), b"utf-8")?;
    atomic_import_write(
        &retrieved_sidecar_path(&cache_path),
        provenance.imported_at_unix_seconds.to_string().as_bytes(),
    )?;
    let provenance_path = provenance_path(&cache_path);
    atomic_import_write(&provenance_path, &provenance_bytes)?;

    log::info!(
        "gamehacking browser_import platform={} game_id={} kind={:?} cache_path={} stored_sha256={} replaced={} cheats={}",
        request.platform.slug(),
        request.game_id,
        kind,
        cache_path.display(),
        provenance.stored_sha256,
        replaced.is_some(),
        validated.cheat_count
    );

    Ok(BrowserImportOutcome {
        platform: request.platform,
        gamehacking_game_id: request.game_id,
        kind,
        imported_title: validated.imported_title,
        cache_path,
        provenance_path,
        provenance,
        replaced_existing_cache: replaced.is_some(),
        replaced,
        backup_path,
        cheat_count: validated.cheat_count,
        action_replay_count: validated.action_replay_count,
        gecko_count: validated.gecko_count,
        raw_unknown_count: validated.raw_unknown_count,
        unsupported_count: validated.unsupported_count,
        enriched_from_cache: validated.enriched_from_cache,
        verified_evidence: validated.verified_evidence,
    })
}

/// The same rule `gamehacking_provider::prepare_cache` applies: an
/// absolute, non-root directory. The destination is never derived from
/// anything in the imported content - only from the validated platform
/// and the selected numeric game ID (see
/// `BrowserImportKind::cache_file_name`), so imported bytes cannot choose
/// where they land.
fn prepare_import_cache(root: &Path) -> Result<(), BrowserImportError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(import_error(
            BrowserImportErrorKind::CacheWriteFailed,
            "GameHacking.org cache root must be an absolute non-root path",
        ));
    }
    fs::create_dir_all(root).map_err(|failure| {
        import_error(
            BrowserImportErrorKind::CacheWriteFailed,
            format!("GameHacking.org cache could not be created: {failure}"),
        )
    })
}

/// Byte-for-byte the provider's own `atomic_write`: a fresh temporary
/// file, fully written and fsynced, then renamed over the destination -
/// so a reader never observes a half-written cache entry.
fn atomic_import_write(path: &Path, bytes: &[u8]) -> Result<(), BrowserImportError> {
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
        return Err(import_error(
            BrowserImportErrorKind::CacheWriteFailed,
            format!(
                "{} could not be written atomically: {failure}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
