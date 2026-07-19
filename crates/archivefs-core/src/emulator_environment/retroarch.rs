//! Read-only RetroArch environment discovery.
//!
//! Discovers a native and (user-scope and system-scope) Flatpak RetroArch
//! profile, locates and parses `retroarch.cfg` for a fixed, small set of
//! path purposes, and inventories installed Linux cores (`*_libretro.so`)
//! plus their optional `.info` metadata. Nothing here downloads, installs,
//! executes, or modifies anything - see the module-level doc comment on
//! [`super`] and `docs/RETROARCH_ENVIRONMENT.md` for the full design
//! record, including the primary RetroArch/Flatpak source citations this
//! implementation is based on.
//!
//! This module owns no shared state with `crate::patch_manager` and is not
//! imported by it.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    BoundedListResult, BoundedReadResult, EncodedPath, ExecutableProbe, FsProbe,
    ReadOnlyHostFilesystem, os_str_bytes,
};

/// Official Flatpak application ID for RetroArch, confirmed against the
/// official Flathub manifest (`flathub/org.libretro.RetroArch`).
const FLATPAK_APP_ID: &str = "org.libretro.RetroArch";
const CORE_SUFFIX: &str = "_libretro.so";

/// Bounded read/listing limits. Exceeding one produces a structured
/// diagnostic rather than a partially-trusted read.
pub const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_INFO_BYTES: usize = 128 * 1024;
pub const MAX_CORE_DIR_ENTRIES: usize = 4096;
pub const MAX_PLAYLIST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PLAYLISTS_PER_PROFILE: usize = 1024;
pub const MAX_ENTRIES_PER_PLAYLIST: usize = 16384;
pub const MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE: usize = 65536;
const PLAYLIST_SUFFIX: &str = ".lpl";
pub const MAX_APPIMAGE_SEARCH_ROOT_ENTRIES: usize = 4096;
pub const MAX_DESKTOP_FILE_BYTES: usize = 256 * 1024;
pub const MAX_DESKTOP_FILES_PER_DIRECTORY: usize = 4096;
const APPIMAGE_SUFFIX: &str = ".appimage";
const DESKTOP_SUFFIX: &str = ".desktop";
/// Fixed, documented default AppImage search roots (non-recursive) - see
/// `docs/RETROARCH_APPIMAGE.md`'s "Detection sources". Deliberately not
/// `$HOME` itself or any other broad directory; scanning the entire home
/// directory is explicitly out of scope for this milestone.
const DEFAULT_APPIMAGE_SEARCH_ROOT_RELATIVE_PATHS: [&str; 5] = [
    "Applications",
    ".local/bin",
    ".local/share/applications",
    "AppImages",
    "bin",
];

/// The only RetroArch path purposes this milestone reports. Declared
/// order is the fixed emission order for `RetroArchProfile::paths` - it
/// is never derived from a map or filesystem listing. Assets, filters,
/// remaps, recording, logs, cache, screenshots, content history, and
/// favourites are deliberately out of scope for v1 (see
/// `docs/RETROARCH_ENVIRONMENT.md`'s non-goals) and can be added later
/// without breaking this order (new purposes are appended, not inserted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathPurpose {
    System,
    Cores,
    CoreInfo,
    Saves,
    SaveStates,
    Playlists,
    Shaders,
    Overlays,
    Thumbnails,
    JoypadAutoconfig,
    Database,
    Cheats,
}

/// `(PathPurpose, retroarch.cfg key)` pairs, confirmed against the
/// official RetroArch source (`configuration.c`'s `SETTING_PATH`
/// registrations). Declared order doubles as `PathPurpose`'s emission
/// order.
const PATH_PURPOSE_SPECS: [(PathPurpose, &str); 12] = [
    (PathPurpose::System, "system_directory"),
    (PathPurpose::Cores, "libretro_directory"),
    (PathPurpose::CoreInfo, "libretro_info_path"),
    (PathPurpose::Saves, "savefile_directory"),
    (PathPurpose::SaveStates, "savestate_directory"),
    (PathPurpose::Playlists, "playlist_directory"),
    (PathPurpose::Shaders, "video_shader_dir"),
    (PathPurpose::Overlays, "overlay_directory"),
    (PathPurpose::Thumbnails, "thumbnails_directory"),
    (PathPurpose::JoypadAutoconfig, "joypad_autoconfig_dir"),
    (PathPurpose::Database, "content_database_path"),
    (PathPurpose::Cheats, "cheat_database_path"),
];

fn path_purpose_keys() -> [&'static str; 12] {
    let mut keys = [""; 12];
    for (index, (_, key)) in PATH_PURPOSE_SPECS.iter().enumerate() {
        keys[index] = key;
    }
    keys
}

const INFO_KEYS: [&str; 4] = [
    "display_name",
    "display_version",
    "systemname",
    "supported_extensions",
];

/// Declaration order is deliberately `Native`, `AppImage`, `Flatpak`: this
/// is also the derived `Ord` used to sort `profiles[]`, and matches the
/// fixed logical order this milestone documents (native/shared user
/// profile, then any distinct-config AppImage profiles, then Flatpak
/// user, then Flatpak system) - see `docs/RETROARCH_APPIMAGE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Native,
    AppImage,
    Flatpak,
}

/// Flatpak install scope. Native profiles are always [`ProfileScope::User`],
/// because RetroArch's own native default-path derivation always reads the
/// invoking user's `$HOME`/`$XDG_CONFIG_HOME`; there is no system-wide
/// native RetroArch configuration concept in the source reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope {
    User,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProfileRef {
    pub profile_kind: ProfileKind,
    pub scope: ProfileScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCategory {
    Discovery,
    ConfigParse,
    PathResolution,
    CoreInventory,
    Filesystem,
    /// Playlist directory listing, per-file parsing, and per-entry
    /// findings - see [`RetroArchPlaylistInventory`].
    PlaylistInventory,
    /// AppImage candidate discovery, desktop-file parsing, and config
    /// association findings - see [`AppImageCandidate`].
    AppImageInventory,
}

/// A structured, machine-readable finding. `code` is the stable
/// fine-grained identifier; `detail_kind` is a coarser category for
/// consumers that want to group without knowing every code. Deliberately
/// no free-text `message` field - human wording belongs only in the CLI
/// formatter, never in the stable JSON contract.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: DiagnosticSeverity,
    pub detail_kind: DiagnosticCategory,
    pub profile: Option<ProfileRef>,
    pub purpose: Option<PathPurpose>,
    pub path: Option<EncodedPath>,
    /// The zero-based playlist entry index this finding is about, if any -
    /// `None` for every diagnostic that is not entry-specific (directory-
    /// or playlist-file-level findings). Added for playlist diagnostics;
    /// no pre-existing diagnostic ever sets it.
    pub entry_index: Option<u32>,
}

/// Internal, pre-sort representation carrying a real `PathBuf` (for
/// deterministic byte-order sorting) instead of the lossy-safe
/// `EncodedPath` used in the public [`Diagnostic`]. Converted via
/// [`finalize_diagnostics`] after sorting.
struct RawDiagnostic {
    code: &'static str,
    severity: DiagnosticSeverity,
    detail_kind: DiagnosticCategory,
    profile: Option<ProfileRef>,
    purpose: Option<PathPurpose>,
    path: Option<PathBuf>,
    entry_index: Option<u32>,
}

impl RawDiagnostic {
    /// Convenience constructor for the pre-existing (non-playlist)
    /// diagnostic call sites, which never set `entry_index`.
    fn new(
        code: &'static str,
        severity: DiagnosticSeverity,
        detail_kind: DiagnosticCategory,
        profile: Option<ProfileRef>,
        purpose: Option<PathPurpose>,
        path: Option<PathBuf>,
    ) -> Self {
        Self {
            code,
            severity,
            detail_kind,
            profile,
            purpose,
            path,
            entry_index: None,
        }
    }
}

fn finalize_diagnostics(mut raw: Vec<RawDiagnostic>) -> Vec<Diagnostic> {
    raw.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.profile.cmp(&b.profile))
            .then_with(|| a.purpose.cmp(&b.purpose))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.entry_index.cmp(&b.entry_index))
    });
    raw.into_iter()
        .map(|diagnostic| Diagnostic {
            code: diagnostic.code,
            severity: diagnostic.severity,
            detail_kind: diagnostic.detail_kind,
            profile: diagnostic.profile,
            purpose: diagnostic.purpose,
            path: diagnostic.path.as_deref().map(EncodedPath::from_path),
            entry_index: diagnostic.entry_index,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    /// Native-only: deterministic (`PATH` order), first-occurrence-deduped
    /// list of regular, executable files literally named `retroarch`.
    /// Always empty for Flatpak profiles.
    pub executables: Vec<EncodedPath>,
    /// Flatpak-only: whether this scope's Flatpak app directory for
    /// `org.libretro.RetroArch` exists. Always `false` for Native
    /// profiles. This is evidence the app is *installed*, not that it has
    /// ever been launched or has a config file - see `config_file_found`.
    pub flatpak_metadata_found: bool,
    pub config_directory_found: bool,
    pub config_file_found: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryProbeFinding {
    pub path: EncodedPath,
    pub probe: FsProbe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigReadOutcome {
    /// The config file's own [`FsProbe`] (on `ConfigFileFinding::probe`)
    /// was anything other than `PresentFile` - missing, a symlink (not
    /// followed), the wrong type, inaccessible, or another I/O error.
    NotAttempted,
    Parsed {
        /// One-based line numbers of lines that were not blank, not a
        /// comment, not an `#include`, and did not parse as `key = value`.
        /// Sorted ascending. Parsing continues past every malformed line.
        malformed_lines: Vec<u32>,
        /// An `#include "..."` directive was found. Never followed in
        /// this milestone.
        include_detected: bool,
        /// `!include_detected` - kept as an explicit field so a JSON
        /// consumer does not need to know that any include implies an
        /// incomplete read.
        complete: bool,
    },
    TooLarge {
        limit_bytes: u64,
    },
    InvalidUtf8,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigFileFinding {
    pub path: EncodedPath,
    pub probe: FsProbe,
    pub read: ConfigReadOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionState {
    ConfiguredResolved,
    /// A non-empty value was configured but ArchiveFS declines to
    /// resolve it (a `:`-application-directory alias, or a plain
    /// relative value) - see the `colon_alias_unresolved`/
    /// `relative_path_unresolved` diagnostics for why.
    ConfiguredUnresolved,
    /// The key was absent, or present with an empty value. RetroArch
    /// applies its own runtime default in this case; this milestone does
    /// not attempt to reproduce it (see `docs/RETROARCH_ENVIRONMENT.md`).
    /// Never described as "not configured" - an empty value is a real,
    /// distinct configured state, not merely a missing key.
    RuntimeDefaultUnknown,
    /// The config file itself could not be read (missing, unreadable,
    /// too large, or invalid UTF-8), so no key could be checked.
    NoReadableConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathFinding {
    pub purpose: PathPurpose,
    pub config_key: &'static str,
    pub configured_value: Option<String>,
    pub resolution: ResolutionState,
    pub resolved_path: Option<EncodedPath>,
    pub probe: Option<FsProbe>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreFinding {
    pub file_name: EncodedPath,
    pub full_path: EncodedPath,
    pub core_stem: String,
    pub info: CoreInfoFinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreInfoFinding {
    Found {
        display_name: Option<String>,
        display_version: Option<String>,
        system_name: Option<String>,
        supported_extensions: Vec<String>,
    },
    Missing,
    DirectoryUnavailable,
    Symlink,
    WrongType,
    TooLarge,
    InvalidUtf8,
    Inaccessible,
    IoError,
}

/// How one playlist entry's `path` value was classified. Verified against
/// `libretro/RetroArch`'s `playlist.c` (`playlist_path_id_init`) and
/// `libretro-common/file/file_path.c` (`path_get_archive_delim`,
/// `path_is_compressed_file`) - see `docs/RETROARCH_PLAYLISTS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPathKind {
    /// An absolute filesystem path with no recognized archive-member
    /// delimiter.
    Filesystem,
    /// Contains a `#` immediately after a `.7z`, `.zip`, `.zst`, or `.apk`
    /// extension (case-insensitive) - verified as the *only* condition
    /// under which RetroArch itself treats `#` as an archive-member
    /// delimiter (`path_get_archive_delim`). A `#` anywhere else in the
    /// path (including after `.rar`, which RetroArch's own
    /// `path_is_compressed_file` does not recognize as compressed at all)
    /// is just a literal character, not a delimiter.
    ArchiveMember,
    /// A non-empty value that is not an absolute path (does not start
    /// with `/`). This milestone does not invent a resolution base for
    /// it, mirroring the same policy already applied to `retroarch.cfg`
    /// path values.
    Relative,
    /// The `path` key was present with an empty string value.
    Empty,
    /// The `path` key was absent from this entry entirely.
    Missing,
}

/// A playlist entry's content path, preserved exactly as written plus its
/// classification. `raw` is always a real, already-UTF-8-validated
/// `String` (it came from parsed JSON text, never from a probed
/// filesystem path), so no lossy encoding is needed here - contrast with
/// [`RetroArchPlaylist::file_path`], which is a real filesystem path and
/// does use [`EncodedPath`].
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistContentPath {
    /// `None` only when [`ContentPathKind::Missing`].
    pub raw: Option<String>,
    pub kind: ContentPathKind,
    /// The portion before the archive-member delimiter. `Some` only when
    /// `kind == ArchiveMember`.
    pub archive_path: Option<String>,
    /// The portion after the archive-member delimiter (the inner member's
    /// own path, never opened or resolved by this milestone). `Some` only
    /// when `kind == ArchiveMember`.
    pub archive_member_path: Option<String>,
}

/// A playlist entry's `crc32` field. Verified format (`tasks/task_database.c`,
/// `manual_content_scan.c`): an 8-hex-digit, uppercase CRC32 followed by a
/// literal `|crc` suffix (e.g. `"A1B2C3D4|crc"`); the literal placeholder
/// `"00000000|crc"` is RetroArch's own "not computed" sentinel, written
/// whenever a manual scan does not hash content. Never silently
/// normalized into a different shape - a value that does not match this
/// exact grammar is `Malformed`, not coerced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlaylistCrc {
    /// Exactly 8 hex digits (canonicalized uppercase) followed by `|crc`,
    /// and not the all-zero placeholder.
    Verified { value: String },
    /// The field was absent or an empty string.
    Missing,
    /// The literal `"00000000|crc"` placeholder.
    Placeholder,
    /// Present, non-empty, but does not match the verified grammar.
    Malformed { raw: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchPlaylistEntry {
    /// Zero-based index into this playlist's own JSON `items` array - the
    /// natural, least-surprising convention, and the one used throughout
    /// this module's own diagnostics.
    pub entry_index: u32,
    pub content_path: PlaylistContentPath,
    pub label: Option<String>,
    pub core_path: Option<String>,
    pub core_name: Option<String>,
    pub crc: PlaylistCrc,
    /// Exactly the JSON `db_name` value when present and non-empty;
    /// `None` otherwise. This milestone does **not** reproduce RetroArch's
    /// own runtime fallback (playlist basename, then the loaded core's
    /// declared databases - see `playlist_get_db_name`) - identity
    /// evidence here is only ever what the file itself actually states.
    pub database_name: Option<String>,
    pub subsystem_ident: Option<String>,
    pub subsystem_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchPlaylist {
    pub file_path: EncodedPath,
    /// The playlist filename with its `.lpl` extension removed - a
    /// convenience identity label. Deliberately not a reproduction of
    /// `playlist_get_db_name`'s own fallback (which keeps the `.lpl`
    /// suffix and special-cases `content_history.lpl`/
    /// `content_favorites.lpl`); see `docs/RETROARCH_PLAYLISTS.md`.
    pub playlist_name: String,
    /// The raw JSON `version` field, if present. Never used to accept or
    /// reject a file - confirmed from `playlist.c`'s own JSON object
    /// member handler, which has no case for `"version"` at all on read;
    /// it is write-only metadata upstream itself never validates.
    pub version: Option<String>,
    pub default_core_path: Option<String>,
    pub default_core_name: Option<String>,
    pub entries: Vec<RetroArchPlaylistEntry>,
    pub diagnostics: Vec<Diagnostic>,
    /// `false` if [`MAX_ENTRIES_PER_PLAYLIST`] was reached - `entries`
    /// then holds only the first-parsed entries up to that limit, never a
    /// silently-truncated-without-notice list.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchPlaylistInventory {
    /// The resolved `Playlists` directory, or `None` if it was never
    /// resolved (unconfigured, empty, a colon alias, a relative value, or
    /// runtime-default-unknown - see [`ResolutionState`]). This milestone
    /// never guesses a fallback directory.
    pub directory: Option<EncodedPath>,
    /// Sorted by encoded playlist path bytes - never filesystem
    /// enumeration order.
    pub playlists: Vec<RetroArchPlaylist>,
    pub diagnostics: Vec<Diagnostic>,
    /// `false` if the directory listing exceeded
    /// [`MAX_PLAYLISTS_PER_PROFILE`] or the running entry total across
    /// playlists reached [`MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE`] -
    /// `playlists` then holds only what was actually processed before
    /// stopping, never a silently-truncated-without-notice list.
    pub complete: bool,
}

/// Confidence that one AppImage candidate is actually a RetroArch
/// AppImage - deliberately its own vocabulary (not PCSX2's
/// `MatchConfidence` or the playlist milestone's `PlaylistMatchConfidence`):
/// the evidence categories here (filename text, desktop-entry text, exec
/// resolution) are genuinely different from either. See
/// `docs/RETROARCH_APPIMAGE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppImageIdentificationConfidence {
    /// A desktop entry's own `Exec` resolves to exactly this candidate
    /// *and* the same entry's `Name`/`GenericName`/`StartupWMClass`/`Icon`
    /// identifies RetroArch.
    Exact,
    /// A desktop entry's `Name`/`GenericName`/`StartupWMClass`/`Icon`
    /// identifies RetroArch, but its `Exec` does not fully resolve to
    /// this exact candidate (unresolved, a shell wrapper, or a missing
    /// target) - one strong signal (the name), not two agreeing ones.
    /// An `Exec` match alone, with no independent name/icon evidence, is
    /// deliberately *not* enough for this tier - see
    /// `docs/RETROARCH_APPIMAGE.md` for why (virtually any AppImage with
    /// an ordinary, unrelated desktop file would otherwise qualify,
    /// since a desktop entry's `Exec` almost always points at itself).
    Strong,
    /// Only the filename itself contains `retroarch` (case-insensitive) -
    /// never promoted to `Exact`/`Strong` by itself.
    Weak,
    /// Multiple, mutually inconsistent pieces of evidence (e.g. two
    /// different desktop entries both claim this exact file but disagree
    /// on identity) - never silently resolved to one confidence level.
    Ambiguous,
    /// No usable evidence at all (present only structurally; a candidate
    /// with zero evidence is not reported as a finding in the first
    /// place, so this value is not expected to appear in practice).
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableState {
    Executable,
    NotExecutable,
}

/// Result of resolving one desktop entry's own `Exec` value against one
/// AppImage candidate's path - see [`parse_exec_executable_token`] for the
/// verified freedesktop.org Desktop Entry Specification grammar this is
/// based on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecResolution {
    /// The `Exec` key's own executable token resolves to exactly this
    /// candidate's path (matched either by the full path or, when `Exec`
    /// names a bare filename, by basename).
    MatchesCandidate,
    /// `Exec` resolved to a real, well-formed executable token, but it
    /// names a *different* path than this candidate.
    MismatchedTarget { target: EncodedPath },
    /// The executable token names a known shell/wrapper command (`sh`,
    /// `bash`, `env`, ...) whose real target cannot be determined without
    /// executing a shell, which this milestone never does.
    ShellWrapperUnresolved,
    /// The token resolved to a plausible path, but no such file exists.
    TargetMissing,
    /// `Exec` was empty, absent, or could not be tokenized at all.
    Unresolved,
}

/// One desktop entry's evidence about one AppImage candidate.
#[derive(Debug, Clone, Serialize)]
pub struct DesktopEntryMatch {
    pub desktop_file: EncodedPath,
    /// Whether `Name`, `GenericName`, `Icon`, or `StartupWMClass`
    /// mentions RetroArch (case-insensitive substring match on
    /// `"retroarch"`).
    pub name_evidence: bool,
    pub exec_resolution: ExecResolution,
}

/// How this AppImage's RetroArch configuration relates to the native
/// profile's own resolved configuration - see
/// `docs/RETROARCH_APPIMAGE.md`'s "Configuration association" for the
/// verified basis of each variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigAssociation {
    /// No evidence of a distinct configuration root was found - this
    /// AppImage would resolve `HOME`/`XDG_CONFIG_HOME` exactly like the
    /// native profile (verified: RetroArch's own Linux config resolution
    /// has no AppImage-specific logic at all).
    SharesNativeProfile,
    /// A writable directory named `<AppImage-path>.home` and/or
    /// `<AppImage-path>.config` exists next to the AppImage - the
    /// official AppImage `type2-runtime`'s own portable-mode convention,
    /// which overrides `$HOME`/`$XDG_CONFIG_HOME` before RetroArch ever
    /// starts, yielding a genuinely distinct resolved config directory.
    PortableConfigDetected { config_directory: EncodedPath },
    /// A desktop entry's `Exec` includes `-c`/`--config <path>` (a real,
    /// verified RetroArch CLI option) naming an explicit config file
    /// whose parent directory differs from the native profile's own.
    ExplicitConfig { config_directory: EncodedPath },
    /// Evidence exists but does not resolve to a specific directory
    /// (e.g. `--config` present but its value could not be determined).
    Unknown,
    /// Two or more pieces of evidence disagree on the resolved
    /// configuration directory - never silently resolved to one.
    Ambiguous,
}

/// One discovered AppImage candidate. A candidate is only ever reported
/// when at least one piece of evidence (filename or desktop entry)
/// suggests it might be RetroArch - see [`AppImageIdentificationConfidence`].
#[derive(Debug, Clone, Serialize)]
pub struct AppImageCandidate {
    pub path: EncodedPath,
    pub probe: FsProbe,
    /// `None` only when `probe != FsProbe::PresentFile` (a symlink,
    /// missing, wrong type, or inaccessible candidate has no meaningful
    /// executable-bit state).
    pub executable: Option<ExecutableState>,
    /// Whether the candidate's own filename contains `retroarch`
    /// case-insensitively.
    pub filename_evidence: bool,
    /// Sorted by encoded desktop-file path bytes.
    pub desktop_evidence: Vec<DesktopEntryMatch>,
    pub confidence: AppImageIdentificationConfidence,
    pub config_association: ConfigAssociation,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchProfile {
    pub profile_kind: ProfileKind,
    pub scope: ProfileScope,
    pub evidence: Evidence,
    pub config_directory: DirectoryProbeFinding,
    pub config_file: ConfigFileFinding,
    pub paths: Vec<PathFinding>,
    pub cores: Vec<CoreFinding>,
    pub playlists: RetroArchPlaylistInventory,
    /// Every AppImage candidate attributed to this logical profile -
    /// candidates sharing this profile's own configuration (`Native`) or
    /// promoted here because they evidence a distinct configuration
    /// (`AppImage`). Always empty for `Flatpak` profiles: AppImage
    /// evidence never merges with a Flatpak sandbox's own path roots.
    /// Sorted by encoded AppImage path bytes.
    pub app_images: Vec<AppImageCandidate>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchEnvironmentReport {
    pub format_version: u32,
    pub profiles: Vec<RetroArchProfile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Injected discovery inputs. Production code uses
/// [`DiscoveryEnvironment::from_process_environment`]; tests construct
/// this directly so discovery never depends on the developer's real
/// `HOME`, `PATH`, or Flatpak installation.
#[derive(Debug, Clone)]
pub struct DiscoveryEnvironment {
    pub home: Option<std::ffi::OsString>,
    pub xdg_config_home: Option<std::ffi::OsString>,
    pub path: Option<std::ffi::OsString>,
    pub user_flatpak_root: PathBuf,
    pub system_flatpak_root: PathBuf,
    /// Bounded, non-recursive AppImage search roots - see
    /// `docs/RETROARCH_APPIMAGE.md`'s "Detection sources". Production
    /// code populates this with a small, fixed, documented set of
    /// directories under `$HOME`; tests inject their own so discovery
    /// never depends on the real machine's home directory contents.
    pub app_image_search_roots: Vec<PathBuf>,
    /// Bounded desktop-file search roots (each is scanned non-recursively
    /// for `*.desktop` files) - `$XDG_DATA_HOME/applications` plus each
    /// `$XDG_DATA_DIRS` entry's own `applications` subdirectory.
    pub desktop_file_roots: Vec<PathBuf>,
}

impl DiscoveryEnvironment {
    pub fn from_process_environment() -> Self {
        let home = std::env::var_os("HOME");
        let home_path = home.as_ref().map(PathBuf::from);
        let user_flatpak_root = home_path
            .as_ref()
            .map(|home| home.join(".local/share/flatpak"))
            .unwrap_or_else(|| PathBuf::from(".local/share/flatpak"));
        let app_image_search_roots = home_path
            .as_ref()
            .map(|home| {
                DEFAULT_APPIMAGE_SEARCH_ROOT_RELATIVE_PATHS
                    .iter()
                    .map(|relative| home.join(relative))
                    .collect()
            })
            .unwrap_or_default();
        let desktop_file_roots = resolve_desktop_file_roots(
            home_path.as_deref(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("XDG_DATA_DIRS").as_deref(),
        );
        Self {
            home,
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            path: std::env::var_os("PATH"),
            user_flatpak_root,
            system_flatpak_root: PathBuf::from("/var/lib/flatpak"),
            app_image_search_roots,
            desktop_file_roots,
        }
    }
}

/// `$XDG_DATA_HOME/applications` plus each `$XDG_DATA_DIRS` entry's own
/// `applications` subdirectory, per the XDG Base Directory Specification's
/// documented defaults (`$HOME/.local/share` and
/// `/usr/local/share:/usr/share` respectively) - applied the same way
/// this module already applies `XDG_CONFIG_HOME`'s own defaulting rule:
/// unset, empty, or (for `XDG_DATA_HOME`) relative values fall back to
/// the default rather than being resolved against an invented base.
fn resolve_desktop_file_roots(
    home_dir: Option<&Path>,
    xdg_data_home: Option<&OsStr>,
    xdg_data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let data_home = match xdg_data_home {
        Some(value) if !value.is_empty() => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                Some(candidate)
            } else {
                home_dir.map(|home| home.join(".local/share"))
            }
        }
        _ => home_dir.map(|home| home.join(".local/share")),
    };
    if let Some(data_home) = data_home {
        roots.push(data_home.join("applications"));
    }

    let data_dirs_value = match xdg_data_dirs {
        Some(value) if !value.is_empty() => value.as_bytes().to_vec(),
        _ => b"/usr/local/share:/usr/share".to_vec(),
    };
    for directory_bytes in data_dirs_value.split(|&byte| byte == b':') {
        if directory_bytes.is_empty() {
            continue;
        }
        let directory = PathBuf::from(OsStr::from_bytes(directory_bytes));
        if directory.is_absolute() {
            roots.push(directory.join("applications"));
        }
    }
    roots
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    /// `HOME` is unset or empty. This is the only condition under which
    /// no discovery roots can be constructed at all - mirrors
    /// `patch_manager::pcsx2::Pcsx2DiscoveryRoots::from_environment`'s
    /// existing precedent exactly.
    NoHome,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => write!(
                formatter,
                "HOME is not set; cannot determine any RetroArch discovery roots"
            ),
        }
    }
}

impl std::error::Error for DiscoveryError {}

pub fn discover_retroarch_environment(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &DiscoveryEnvironment,
) -> Result<RetroArchEnvironmentReport, DiscoveryError> {
    let home = environment
        .home
        .as_ref()
        .filter(|value| !value.is_empty())
        .ok_or(DiscoveryError::NoHome)?;
    let home_dir = PathBuf::from(home);

    let mut report_diagnostics: Vec<RawDiagnostic> = Vec::new();
    let xdg_config_home_dir =
        resolve_xdg_config_home(environment, &home_dir, &mut report_diagnostics);
    let native_config_dir = xdg_config_home_dir.join("retroarch");

    // Flatpak's own environment setup (distinct from generic XDG
    // defaulting) sets XDG_CONFIG_HOME to `$HOME/.var/app/<app-id>/config`
    // inside the sandbox - not `.config`. Confirmed against the official
    // Flathub manifest and RetroArch's own Flatpak-bundled seed config.
    let flatpak_sandbox_home = home_dir.join(".var/app").join(FLATPAK_APP_ID);
    let flatpak_config_dir = flatpak_sandbox_home.join("config").join("retroarch");

    let (native_profile, native_sort_path, native_diagnostics) = discover_profile(
        filesystem,
        ProfileKind::Native,
        ProfileScope::User,
        &native_config_dir,
        &home_dir,
        environment.path.as_deref(),
        None,
    );

    // AppImage/desktop-file discovery is profile-independent scanning
    // (the same search roots and desktop entries exist regardless of
    // which RetroArch profile ends up owning the resulting evidence), so
    // its own diagnostics are report-level, not attributed to one
    // profile.
    let desktop_entries = discover_desktop_entries(
        filesystem,
        &environment.desktop_file_roots,
        &mut report_diagnostics,
    );
    let appimage_paths = discover_app_image_files(
        filesystem,
        &environment.app_image_search_roots,
        &mut report_diagnostics,
    );
    let mut appimage_candidates: Vec<AppImageCandidate> = appimage_paths
        .iter()
        .filter_map(|path| {
            build_app_image_candidate(filesystem, path, &desktop_entries, &native_config_dir)
        })
        .collect();
    appimage_candidates.sort_by(|left, right| left.path.display.cmp(&right.path.display));
    appimage_candidates.dedup_by(|left, right| left.path.display == right.path.display);

    let partition = partition_by_config_association(appimage_candidates);
    let mut native_profile = native_profile;
    native_profile.app_images = partition.shared;
    if partition.ambiguous_merge_diagnostic {
        report_diagnostics.push(RawDiagnostic::new(
            "duplicate_logical_profile_prevented",
            DiagnosticSeverity::Warning,
            DiagnosticCategory::AppImageInventory,
            Some(ProfileRef {
                profile_kind: ProfileKind::Native,
                scope: ProfileScope::User,
            }),
            None,
            None,
        ));
    }

    let mut profiles = vec![(native_profile, native_sort_path, native_diagnostics)];

    if let Some((distinct_config_dir, distinct_candidates)) = partition.distinct {
        let (mut distinct_profile, distinct_sort_path, distinct_diagnostics) = discover_profile(
            filesystem,
            ProfileKind::AppImage,
            ProfileScope::User,
            &distinct_config_dir,
            &home_dir,
            None,
            None,
        );
        distinct_profile.app_images = distinct_candidates;
        profiles.push((distinct_profile, distinct_sort_path, distinct_diagnostics));
    }

    profiles.push(discover_profile(
        filesystem,
        ProfileKind::Flatpak,
        ProfileScope::User,
        &flatpak_config_dir,
        &flatpak_sandbox_home,
        None,
        Some(flatpak_metadata_found(
            filesystem,
            &environment.user_flatpak_root,
        )),
    ));
    profiles.push(discover_profile(
        filesystem,
        ProfileKind::Flatpak,
        ProfileScope::System,
        &flatpak_config_dir,
        &flatpak_sandbox_home,
        None,
        Some(flatpak_metadata_found(
            filesystem,
            &environment.system_flatpak_root,
        )),
    ));

    profiles.sort_by(|left, right| {
        (left.0.profile_kind, left.0.scope, &left.1).cmp(&(
            right.0.profile_kind,
            right.0.scope,
            &right.1,
        ))
    });

    let profiles = profiles
        .into_iter()
        .map(|(mut profile, _sort_path, diagnostics)| {
            profile.diagnostics = finalize_diagnostics(diagnostics);
            profile
        })
        .collect();

    Ok(RetroArchEnvironmentReport {
        // Bumped from 1: `profiles[]` can now contain a 4th element (a
        // distinct AppImage profile) positioned *between* the native and
        // Flatpak profiles, which shifts the array index of the
        // Flatpak/user and Flatpak/system profiles whenever it is
        // present - a genuine positional-contract change for any
        // consumer that indexed `profiles[]` by position rather than by
        // `profile_kind`/`scope`. See `docs/RETROARCH_APPIMAGE.md`'s
        // "JSON compatibility decision".
        format_version: 2,
        profiles,
        diagnostics: finalize_diagnostics(report_diagnostics),
    })
}

fn flatpak_metadata_found(filesystem: &dyn ReadOnlyHostFilesystem, root: &Path) -> bool {
    filesystem.probe(&root.join("app").join(FLATPAK_APP_ID)) == FsProbe::PresentDirectory
}

/// Per the XDG Base Directory Specification: an unset or empty
/// `XDG_CONFIG_HOME` falls back to `$HOME/.config`, and any relative
/// value must be ignored (treated the same as unset).
fn resolve_xdg_config_home(
    environment: &DiscoveryEnvironment,
    home_dir: &Path,
    diagnostics: &mut Vec<RawDiagnostic>,
) -> PathBuf {
    match environment.xdg_config_home.as_ref() {
        Some(value) if !value.is_empty() => {
            let candidate = PathBuf::from(value);
            if candidate.is_absolute() {
                candidate
            } else {
                diagnostics.push(RawDiagnostic {
                    code: "xdg_config_home_relative_ignored",
                    severity: DiagnosticSeverity::Info,
                    detail_kind: DiagnosticCategory::Discovery,
                    profile: None,
                    purpose: None,
                    path: Some(candidate),
                    entry_index: None,
                });
                home_dir.join(".config")
            }
        }
        _ => home_dir.join(".config"),
    }
}

/// Returns `(profile, sort_key_path, raw_diagnostics)`. The sort key is
/// the profile's own config-file path, kept as a real `PathBuf` so
/// top-level sorting can use its native (component-wise, deterministic)
/// `Ord` rather than comparing lossy display text.
#[allow(clippy::too_many_arguments)]
fn discover_profile(
    filesystem: &dyn ReadOnlyHostFilesystem,
    profile_kind: ProfileKind,
    scope: ProfileScope,
    config_dir: &Path,
    tilde_home: &Path,
    executable_search_path: Option<&OsStr>,
    flatpak_metadata_found: Option<bool>,
) -> (RetroArchProfile, PathBuf, Vec<RawDiagnostic>) {
    let mut diagnostics: Vec<RawDiagnostic> = Vec::new();
    let profile_ref = ProfileRef {
        profile_kind,
        scope,
    };

    let executables = match executable_search_path {
        Some(path_value) => discover_native_executables(filesystem, path_value),
        None => Vec::new(),
    };

    let config_directory_probe = filesystem.probe(config_dir);
    let config_directory_found = config_directory_probe == FsProbe::PresentDirectory;

    let config_file_path = config_dir.join("retroarch.cfg");
    let (config_probe, config_outcome, parsed) = read_config_file(
        filesystem,
        &config_file_path,
        MAX_CONFIG_BYTES,
        &path_purpose_keys(),
    );
    record_config_diagnostics(
        &config_probe,
        &config_outcome,
        &config_file_path,
        profile_ref,
        &mut diagnostics,
    );
    let config_file_found = matches!(config_outcome, ConfigReadOutcome::Parsed { .. });

    let path_results = build_path_findings(
        filesystem,
        parsed.as_ref(),
        tilde_home,
        profile_ref,
        &mut diagnostics,
    );

    let cores = discover_cores(
        filesystem,
        &path_results.resolved_dirs,
        profile_ref,
        &mut diagnostics,
    );

    // Playlist diagnostics are deliberately *not* threaded into this
    // profile's own shared `diagnostics` accumulator (unlike every other
    // finding above): they already live fully nested under
    // `playlists`/`playlists.playlists[]`, and duplicating them into the
    // flat `profile.diagnostics` list too would mean every playlist
    // finding appears twice in JSON for no benefit.
    let playlists = discover_playlists(filesystem, &path_results.resolved_dirs, profile_ref);

    let evidence = Evidence {
        executables: executables.clone(),
        flatpak_metadata_found: flatpak_metadata_found.unwrap_or(false),
        config_directory_found,
        config_file_found,
    };

    let profile = RetroArchProfile {
        profile_kind,
        scope,
        evidence,
        config_directory: DirectoryProbeFinding {
            path: EncodedPath::from_path(config_dir),
            probe: config_directory_probe,
        },
        config_file: ConfigFileFinding {
            path: EncodedPath::from_path(&config_file_path),
            probe: config_probe,
            read: config_outcome,
        },
        paths: path_results.findings,
        cores,
        playlists,
        app_images: Vec::new(), // filled in by discover_retroarch_environment, native/AppImage profiles only
        diagnostics: Vec::new(), // filled in by the caller after global sort
    };

    (profile, config_file_path, diagnostics)
}

fn record_config_diagnostics(
    probe: &FsProbe,
    outcome: &ConfigReadOutcome,
    config_file_path: &Path,
    profile: ProfileRef,
    diagnostics: &mut Vec<RawDiagnostic>,
) {
    let push =
        |diagnostics: &mut Vec<RawDiagnostic>, code: &'static str, severity: DiagnosticSeverity| {
            diagnostics.push(RawDiagnostic {
                code,
                severity,
                detail_kind: DiagnosticCategory::ConfigParse,
                profile: Some(profile),
                purpose: None,
                path: Some(config_file_path.to_path_buf()),
                entry_index: None,
            });
        };
    match (probe, outcome) {
        (FsProbe::Missing, _) => {}
        (FsProbe::Symlink, ConfigReadOutcome::NotAttempted) => {
            push(
                diagnostics,
                "config_file_symlink_not_followed",
                DiagnosticSeverity::Warning,
            );
        }
        (FsProbe::WrongType, ConfigReadOutcome::NotAttempted) => {
            push(
                diagnostics,
                "config_file_wrong_type",
                DiagnosticSeverity::Warning,
            );
        }
        (FsProbe::Inaccessible, ConfigReadOutcome::NotAttempted) => {
            push(
                diagnostics,
                "config_file_inaccessible",
                DiagnosticSeverity::Warning,
            );
        }
        (FsProbe::IoError, ConfigReadOutcome::NotAttempted) => {
            push(
                diagnostics,
                "config_file_io_error",
                DiagnosticSeverity::Warning,
            );
        }
        (FsProbe::PresentFile, ConfigReadOutcome::TooLarge { .. }) => {
            push(
                diagnostics,
                "config_file_too_large",
                DiagnosticSeverity::Warning,
            );
        }
        (FsProbe::PresentFile, ConfigReadOutcome::InvalidUtf8) => {
            push(
                diagnostics,
                "config_file_invalid_utf8",
                DiagnosticSeverity::Warning,
            );
        }
        (
            FsProbe::PresentFile,
            ConfigReadOutcome::Parsed {
                include_detected: true,
                ..
            },
        ) => {
            push(
                diagnostics,
                "include_directive_not_followed",
                DiagnosticSeverity::Warning,
            );
        }
        _ => {}
    }
}

fn discover_native_executables(
    filesystem: &dyn ReadOnlyHostFilesystem,
    path_value: &OsStr,
) -> Vec<EncodedPath> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for directory_bytes in path_value.as_bytes().split(|&byte| byte == b':') {
        if directory_bytes.is_empty() {
            continue;
        }
        let directory = PathBuf::from(OsStr::from_bytes(directory_bytes));
        let candidate = directory.join("retroarch");
        if seen.contains(&candidate) {
            continue;
        }
        if filesystem.probe_executable(&candidate) == ExecutableProbe::RegularExecutable {
            seen.push(candidate);
        }
    }
    seen.iter()
        .map(|path| EncodedPath::from_path(path))
        .collect()
}

struct ParsedConfig {
    values: BTreeMap<&'static str, String>,
    malformed_lines: Vec<u32>,
    include_detected: bool,
}

fn read_config_file(
    filesystem: &dyn ReadOnlyHostFilesystem,
    path: &Path,
    max_bytes: usize,
    recognized_keys: &[&'static str],
) -> (FsProbe, ConfigReadOutcome, Option<ParsedConfig>) {
    let probe = filesystem.probe(path);
    if probe != FsProbe::PresentFile {
        return (probe, ConfigReadOutcome::NotAttempted, None);
    }
    match filesystem.read_bounded(path, max_bytes) {
        BoundedReadResult::Ok(bytes) => {
            let bytes = strip_utf8_bom(&bytes);
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    let parsed = parse_config(text, recognized_keys);
                    let outcome = ConfigReadOutcome::Parsed {
                        malformed_lines: parsed.malformed_lines.clone(),
                        include_detected: parsed.include_detected,
                        complete: !parsed.include_detected,
                    };
                    (probe, outcome, Some(parsed))
                }
                Err(_) => (probe, ConfigReadOutcome::InvalidUtf8, None),
            }
        }
        BoundedReadResult::TooLarge => (
            probe,
            ConfigReadOutcome::TooLarge {
                limit_bytes: max_bytes as u64,
            },
            None,
        ),
        _ => (probe, ConfigReadOutcome::NotAttempted, None),
    }
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

enum LineKind<'a> {
    Blank,
    Include,
    WholeLineComment,
    KeyValue { key: &'a str, value_region: &'a str },
    Malformed,
}

/// Classifies one line according to RetroArch's own `config_file.c`
/// grammar: comments start at the first unquoted `#`; a line whose first
/// non-whitespace character is `#` is a whole-line comment or an
/// `#include` directive; a `#` inside a quoted string literal is just
/// data. As a deliberate, documented simplification, the "first
/// character" check here is on the left-trimmed line, not the raw line -
/// this only differs from upstream for a comment/include line with
/// leading whitespace, which real RetroArch configs do not produce.
fn classify_line(raw_line: &str) -> LineKind<'_> {
    let line = raw_line.trim_end_matches('\r');
    let left_trimmed = line.trim_start();
    if left_trimmed.is_empty() {
        return LineKind::Blank;
    }
    if let Some(after_hash) = left_trimmed.strip_prefix('#') {
        if after_hash.trim_start().starts_with("include") {
            return LineKind::Include;
        }
        return LineKind::WholeLineComment;
    }
    let content = strip_trailing_comment(left_trimmed);
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return LineKind::Blank;
    }
    match trimmed.split_once('=') {
        Some((key, value_region)) => {
            let key = key.trim();
            if key.is_empty() {
                LineKind::Malformed
            } else {
                LineKind::KeyValue { key, value_region }
            }
        }
        None => LineKind::Malformed,
    }
}

fn strip_trailing_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Matches `config_file_extract_value`: leading whitespace is skipped; a
/// quoted value reads to the next `"` (or the end of the region if
/// unterminated, matching upstream's own lenient behavior); an unquoted
/// value reads to the next whitespace character, which means it
/// truncates at the first space - exactly like real RetroArch, not a gap
/// in this parser.
fn extract_value(value_region: &str) -> String {
    let trimmed_start = value_region.trim_start_matches([' ', '\t']);
    if let Some(rest) = trimmed_start.strip_prefix('"') {
        match rest.find('"') {
            Some(end) => rest[..end].to_string(),
            None => rest.to_string(),
        }
    } else {
        let end = trimmed_start
            .find([' ', '\t'])
            .unwrap_or(trimmed_start.len());
        trimmed_start[..end].to_string()
    }
}

fn parse_config(text: &str, recognized_keys: &[&'static str]) -> ParsedConfig {
    let mut values: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut malformed_lines = Vec::new();
    let mut include_detected = false;
    for (index, raw_line) in text.split('\n').enumerate() {
        let line_number = (index + 1) as u32;
        match classify_line(raw_line) {
            LineKind::Blank | LineKind::WholeLineComment => {}
            LineKind::Include => include_detected = true,
            LineKind::Malformed => malformed_lines.push(line_number),
            LineKind::KeyValue { key, value_region } => {
                if let Some(&recognized) =
                    recognized_keys.iter().find(|candidate| **candidate == key)
                {
                    values
                        .entry(recognized)
                        .or_insert_with(|| extract_value(value_region));
                }
            }
        }
    }
    ParsedConfig {
        values,
        malformed_lines,
        include_detected,
    }
}

/// Resolves a non-empty configured value to a real path, or `None` if
/// ArchiveFS declines to resolve it (colon alias, or a plain relative
/// value with no config-relative anchor RetroArch itself would use -
/// confirmed via `fill_pathname_expand_special` in the primary source).
fn resolve_configured_value(raw: &str, tilde_home: &Path) -> Option<PathBuf> {
    if raw.starts_with('/') {
        Some(PathBuf::from(raw))
    } else if raw == "~" {
        Some(tilde_home.to_path_buf())
    } else {
        raw.strip_prefix("~/").map(|rest| tilde_home.join(rest))
    }
}

fn unresolved_diagnostic_code(raw: &str) -> &'static str {
    if raw.starts_with(':') {
        "colon_alias_unresolved"
    } else {
        "relative_path_unresolved"
    }
}

struct PathFindingsResult {
    findings: Vec<PathFinding>,
    resolved_dirs: BTreeMap<PathPurpose, PathBuf>,
}

fn build_path_findings(
    filesystem: &dyn ReadOnlyHostFilesystem,
    parsed: Option<&ParsedConfig>,
    tilde_home: &Path,
    profile: ProfileRef,
    diagnostics: &mut Vec<RawDiagnostic>,
) -> PathFindingsResult {
    let mut findings = Vec::with_capacity(PATH_PURPOSE_SPECS.len());
    let mut resolved_dirs = BTreeMap::new();

    for &(purpose, config_key) in PATH_PURPOSE_SPECS.iter() {
        let (configured_value, resolution, resolved) = match parsed {
            None => (None, ResolutionState::NoReadableConfig, None),
            Some(parsed) => match parsed.values.get(config_key) {
                None => (None, ResolutionState::RuntimeDefaultUnknown, None),
                Some(raw) if raw.is_empty() => (
                    Some(raw.clone()),
                    ResolutionState::RuntimeDefaultUnknown,
                    None,
                ),
                Some(raw) => match resolve_configured_value(raw, tilde_home) {
                    Some(resolved_path) => (
                        Some(raw.clone()),
                        ResolutionState::ConfiguredResolved,
                        Some(resolved_path),
                    ),
                    None => {
                        diagnostics.push(RawDiagnostic {
                            code: unresolved_diagnostic_code(raw),
                            severity: DiagnosticSeverity::Warning,
                            detail_kind: DiagnosticCategory::PathResolution,
                            profile: Some(profile),
                            purpose: Some(purpose),
                            path: None,
                            entry_index: None,
                        });
                        (
                            Some(raw.clone()),
                            ResolutionState::ConfiguredUnresolved,
                            None,
                        )
                    }
                },
            },
        };

        let probe = resolved.as_ref().map(|path| filesystem.probe(path));
        if let (Some(path), Some(probe_result)) = (&resolved, probe) {
            if probe_result == FsProbe::PresentDirectory {
                resolved_dirs.insert(purpose, path.clone());
            } else {
                let code = match probe_result {
                    FsProbe::Missing => "configured_directory_missing",
                    FsProbe::Symlink => "configured_directory_symlink",
                    FsProbe::PresentFile | FsProbe::WrongType => "configured_path_wrong_type",
                    FsProbe::Inaccessible => "configured_directory_inaccessible",
                    FsProbe::IoError => "configured_directory_io_error",
                    FsProbe::PresentDirectory => unreachable!(),
                };
                diagnostics.push(RawDiagnostic {
                    code,
                    severity: DiagnosticSeverity::Warning,
                    detail_kind: DiagnosticCategory::PathResolution,
                    profile: Some(profile),
                    purpose: Some(purpose),
                    path: Some(path.clone()),
                    entry_index: None,
                });
            }
        }

        findings.push(PathFinding {
            purpose,
            config_key,
            configured_value,
            resolution,
            resolved_path: resolved.as_deref().map(EncodedPath::from_path),
            probe,
        });
    }

    PathFindingsResult {
        findings,
        resolved_dirs,
    }
}

fn discover_cores(
    filesystem: &dyn ReadOnlyHostFilesystem,
    resolved_dirs: &BTreeMap<PathPurpose, PathBuf>,
    profile: ProfileRef,
    diagnostics: &mut Vec<RawDiagnostic>,
) -> Vec<CoreFinding> {
    let Some(cores_dir) = resolved_dirs.get(&PathPurpose::Cores) else {
        return Vec::new();
    };
    let entries = match filesystem.list_dir_bounded(cores_dir, MAX_CORE_DIR_ENTRIES) {
        BoundedListResult::Ok(entries) => entries,
        BoundedListResult::TooLarge => {
            diagnostics.push(RawDiagnostic {
                code: "core_directory_listing_too_large",
                severity: DiagnosticSeverity::Warning,
                detail_kind: DiagnosticCategory::CoreInventory,
                profile: Some(profile),
                purpose: Some(PathPurpose::Cores),
                path: Some(cores_dir.clone()),
                entry_index: None,
            });
            return Vec::new();
        }
        _ => return Vec::new(),
    };

    let core_info_dir = resolved_dirs.get(&PathPurpose::CoreInfo);

    let mut cores: Vec<(Vec<u8>, CoreFinding)> = Vec::new();
    for entry in entries {
        let name_string = entry.file_name.to_string_lossy();
        if !name_string.ends_with(CORE_SUFFIX) {
            continue;
        }
        match entry.probe {
            FsProbe::PresentFile => {}
            FsProbe::Symlink => {
                diagnostics.push(RawDiagnostic {
                    code: "core_symlink_skipped",
                    severity: DiagnosticSeverity::Warning,
                    detail_kind: DiagnosticCategory::CoreInventory,
                    profile: Some(profile),
                    purpose: Some(PathPurpose::Cores),
                    path: Some(cores_dir.join(&entry.file_name)),
                    entry_index: None,
                });
                continue;
            }
            _ => continue,
        }

        let stem = name_string
            .strip_suffix(CORE_SUFFIX)
            .unwrap_or(&name_string)
            .to_string();
        let full_path = cores_dir.join(&entry.file_name);
        let info = match core_info_dir {
            None => CoreInfoFinding::DirectoryUnavailable,
            Some(directory) => resolve_core_info(filesystem, directory, &stem),
        };
        let core = CoreFinding {
            file_name: EncodedPath::from_os_string(&entry.file_name),
            full_path: EncodedPath::from_path(&full_path),
            core_stem: stem,
            info,
        };
        cores.push((os_str_bytes(&entry.file_name).to_vec(), core));
    }

    cores.sort_by(|(left, _), (right, _)| left.cmp(right));
    cores.into_iter().map(|(_, core)| core).collect()
}

fn resolve_core_info(
    filesystem: &dyn ReadOnlyHostFilesystem,
    info_dir: &Path,
    stem: &str,
) -> CoreInfoFinding {
    let info_path = info_dir.join(format!("{stem}.info"));
    match filesystem.probe(&info_path) {
        FsProbe::Missing => CoreInfoFinding::Missing,
        FsProbe::Symlink => CoreInfoFinding::Symlink,
        FsProbe::WrongType | FsProbe::PresentDirectory => CoreInfoFinding::WrongType,
        FsProbe::Inaccessible => CoreInfoFinding::Inaccessible,
        FsProbe::IoError => CoreInfoFinding::IoError,
        FsProbe::PresentFile => match filesystem.read_bounded(&info_path, MAX_INFO_BYTES) {
            BoundedReadResult::Ok(bytes) => {
                let bytes = strip_utf8_bom(&bytes);
                match std::str::from_utf8(bytes) {
                    Ok(text) => {
                        let parsed = parse_config(text, &INFO_KEYS);
                        CoreInfoFinding::Found {
                            display_name: parsed.values.get("display_name").cloned(),
                            display_version: parsed.values.get("display_version").cloned(),
                            system_name: parsed.values.get("systemname").cloned(),
                            supported_extensions: parsed
                                .values
                                .get("supported_extensions")
                                .map(|value| split_supported_extensions(value))
                                .unwrap_or_default(),
                        }
                    }
                    Err(_) => CoreInfoFinding::InvalidUtf8,
                }
            }
            BoundedReadResult::TooLarge => CoreInfoFinding::TooLarge,
            _ => CoreInfoFinding::IoError,
        },
    }
}

fn split_supported_extensions(raw: &str) -> Vec<String> {
    raw.split('|')
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

/// Archive-container extensions RetroArch itself recognizes for the
/// purpose of splitting a playlist `path` at a `#` archive-member
/// delimiter - verified exactly (case-insensitively) against
/// `libretro-common/file/file_path.c`'s `path_get_archive_delim`/
/// `path_is_compressed_file`. Deliberately does **not** include `.rar`:
/// RetroArch's own `path_is_compressed_file` does not recognize it as a
/// compressed extension at all, so `path_get_archive_delim` never treats
/// a `#` after `.rar` as a delimiter either.
const ARCHIVE_CONTAINER_EXTENSIONS: [&str; 4] = ["7z", "zip", "zst", "apk"];

#[derive(Debug, Deserialize)]
struct RawPlaylistFile {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    default_core_path: Option<String>,
    #[serde(default)]
    default_core_name: Option<String>,
    #[serde(default)]
    items: Vec<RawPlaylistItem>,
}

#[derive(Debug, Deserialize)]
struct RawPlaylistItem {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    core_path: Option<String>,
    #[serde(default)]
    core_name: Option<String>,
    #[serde(default)]
    crc32: Option<String>,
    #[serde(default)]
    db_name: Option<String>,
    #[serde(default)]
    subsystem_ident: Option<String>,
    #[serde(default)]
    subsystem_name: Option<String>,
}

/// Splits `path` into `(archive_path, archive_member_path)` if and only if
/// it contains a `#` immediately after one of [`ARCHIVE_CONTAINER_EXTENSIONS`]
/// (case-insensitive) - mirroring `path_get_archive_delim` exactly,
/// including its "only the first qualifying `#`" rule and its requirement
/// that the extension be immediately before the `#` (a `#` elsewhere in
/// the filename, or after an unrecognized extension such as `.rar`, is
/// left as a literal character, never split).
fn split_archive_member_path(path: &str) -> Option<(&str, &str)> {
    let bytes = path.as_bytes();
    let mut search_from = 0usize;
    while let Some(relative_index) = path[search_from..].find('#') {
        let hash_index = search_from + relative_index;
        if hash_index >= 4 {
            let before = &path[..hash_index];
            if ARCHIVE_CONTAINER_EXTENSIONS.iter().any(|extension| {
                before.len() > extension.len()
                    && before.as_bytes()[before.len() - extension.len() - 1] == b'.'
                    && before[before.len() - extension.len()..].eq_ignore_ascii_case(extension)
            }) {
                return Some((before, &path[hash_index + 1..]));
            }
        }
        search_from = hash_index + 1;
        if search_from >= bytes.len() {
            break;
        }
    }
    None
}

fn classify_content_path(raw: Option<String>) -> PlaylistContentPath {
    match raw {
        None => PlaylistContentPath {
            raw: None,
            kind: ContentPathKind::Missing,
            archive_path: None,
            archive_member_path: None,
        },
        Some(value) if value.is_empty() => PlaylistContentPath {
            raw: Some(value),
            kind: ContentPathKind::Empty,
            archive_path: None,
            archive_member_path: None,
        },
        Some(value) => {
            if let Some((archive_path, member_path)) = split_archive_member_path(&value) {
                let archive_path = archive_path.to_string();
                let member_path = member_path.to_string();
                PlaylistContentPath {
                    raw: Some(value),
                    kind: ContentPathKind::ArchiveMember,
                    archive_path: Some(archive_path),
                    archive_member_path: Some(member_path),
                }
            } else if value.starts_with('/') {
                PlaylistContentPath {
                    raw: Some(value),
                    kind: ContentPathKind::Filesystem,
                    archive_path: None,
                    archive_member_path: None,
                }
            } else {
                PlaylistContentPath {
                    raw: Some(value),
                    kind: ContentPathKind::Relative,
                    archive_path: None,
                    archive_member_path: None,
                }
            }
        }
    }
}

/// Classifies a playlist entry's `crc32` field - see [`PlaylistCrc`] for
/// the verified grammar this checks against. Never mutates a malformed
/// value into a well-formed one; only a value that is *already* well
/// formed has its hex digits canonicalized to uppercase (matching
/// upstream's own `%08lX` output), the same lossless canonicalization
/// `patch_manager::pcsx2`'s `normalize_crc` already applies to PCSX2
/// executable CRCs.
fn classify_crc(raw: Option<&str>) -> PlaylistCrc {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return PlaylistCrc::Missing;
    };
    if raw == "00000000|crc" {
        return PlaylistCrc::Placeholder;
    }
    let Some(hex_part) = raw.strip_suffix("|crc") else {
        return PlaylistCrc::Malformed {
            raw: raw.to_string(),
        };
    };
    if hex_part.len() == 8
        && hex_part
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        let canonical = hex_part.to_ascii_uppercase();
        if canonical == "00000000" {
            PlaylistCrc::Placeholder
        } else {
            PlaylistCrc::Verified { value: canonical }
        }
    } else {
        PlaylistCrc::Malformed {
            raw: raw.to_string(),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

/// Reads and parses one `.lpl` file. Bounded at every step: the read
/// itself is capped at [`MAX_PLAYLIST_BYTES`], and parsed entries beyond
/// [`MAX_ENTRIES_PER_PLAYLIST`] are dropped (with a diagnostic and
/// `complete: false`) rather than exposed. The already-bounded input size
/// is what actually keeps this safe from unbounded work/memory - see the
/// module-level note in `docs/RETROARCH_PLAYLISTS.md` on why a
/// straightforward bounded-then-parse approach does not need a streaming
/// JSON reader here: JSON has no separate declared-length field to
/// (mis)trust ahead of the bytes themselves, so bounding the byte count
/// before parsing already bounds the worst case.
fn read_playlist_file(
    filesystem: &dyn ReadOnlyHostFilesystem,
    file_path: &Path,
    profile: ProfileRef,
) -> RetroArchPlaylist {
    let mut diagnostics: Vec<RawDiagnostic> = Vec::new();
    let playlist_name = file_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let empty = |complete: bool, diagnostics: Vec<RawDiagnostic>| RetroArchPlaylist {
        file_path: EncodedPath::from_path(file_path),
        playlist_name: playlist_name.clone(),
        version: None,
        default_core_path: None,
        default_core_name: None,
        entries: Vec::new(),
        diagnostics: finalize_diagnostics(diagnostics),
        complete,
    };

    let bytes = match filesystem.read_bounded(file_path, MAX_PLAYLIST_BYTES) {
        BoundedReadResult::Ok(bytes) => bytes,
        BoundedReadResult::TooLarge => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_too_large",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            ));
            return empty(false, diagnostics);
        }
        _ => return empty(true, diagnostics),
    };
    let bytes = strip_utf8_bom(&bytes);
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_invalid_utf8",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            ));
            return empty(true, diagnostics);
        }
    };

    let raw_file: RawPlaylistFile = match serde_json::from_str(text) {
        Ok(parsed) => parsed,
        Err(_) => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_malformed_json",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            ));
            return empty(true, diagnostics);
        }
    };

    if let Some(version) = &raw_file.version
        && version != "1.0"
        && version != "1.5"
    {
        diagnostics.push(RawDiagnostic::new(
            "playlist_unsupported_version",
            DiagnosticSeverity::Info,
            DiagnosticCategory::PlaylistInventory,
            Some(profile),
            Some(PathPurpose::Playlists),
            Some(file_path.to_path_buf()),
        ));
    }

    let mut complete = true;
    let mut entries = Vec::new();
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (index, item) in raw_file.items.into_iter().enumerate() {
        if entries.len() >= MAX_ENTRIES_PER_PLAYLIST {
            diagnostics.push(RawDiagnostic::new(
                "playlist_entry_count_limit_reached",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            ));
            complete = false;
            break;
        }
        let entry_index = index as u32;
        let content_path = classify_content_path(item.path);
        if let Some(raw_path) = &content_path.raw
            && !seen_paths.insert(raw_path.clone())
        {
            let mut diagnostic = RawDiagnostic::new(
                "duplicate_playlist_entry",
                DiagnosticSeverity::Info,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            );
            diagnostic.entry_index = Some(entry_index);
            diagnostics.push(diagnostic);
        }
        let crc = classify_crc(item.crc32.as_deref());
        if matches!(crc, PlaylistCrc::Malformed { .. }) {
            let mut diagnostic = RawDiagnostic::new(
                "playlist_malformed_crc",
                DiagnosticSeverity::Info,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(file_path.to_path_buf()),
            );
            diagnostic.entry_index = Some(entry_index);
            diagnostics.push(diagnostic);
        }
        entries.push(RetroArchPlaylistEntry {
            entry_index,
            content_path,
            label: non_empty(item.label),
            core_path: non_empty(item.core_path),
            core_name: non_empty(item.core_name),
            crc,
            database_name: non_empty(item.db_name),
            subsystem_ident: non_empty(item.subsystem_ident),
            subsystem_name: non_empty(item.subsystem_name),
        });
    }

    RetroArchPlaylist {
        file_path: EncodedPath::from_path(file_path),
        playlist_name,
        version: raw_file.version,
        default_core_path: non_empty(raw_file.default_core_path),
        default_core_name: non_empty(raw_file.default_core_name),
        entries,
        diagnostics: finalize_diagnostics(diagnostics),
        complete,
    }
}

fn discover_playlists(
    filesystem: &dyn ReadOnlyHostFilesystem,
    resolved_dirs: &BTreeMap<PathPurpose, PathBuf>,
    profile: ProfileRef,
) -> RetroArchPlaylistInventory {
    let Some(playlists_dir) = resolved_dirs.get(&PathPurpose::Playlists) else {
        return RetroArchPlaylistInventory {
            directory: None,
            playlists: Vec::new(),
            diagnostics: Vec::new(),
            complete: true,
        };
    };

    let mut diagnostics: Vec<RawDiagnostic> = Vec::new();
    let entries = match filesystem.list_dir_bounded(playlists_dir, MAX_PLAYLISTS_PER_PROFILE) {
        BoundedListResult::Ok(entries) => entries,
        BoundedListResult::TooLarge => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_listing_too_large",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: false,
            };
        }
        BoundedListResult::NotFound => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_missing",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: true,
            };
        }
        BoundedListResult::Symlink => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_symlink",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: true,
            };
        }
        BoundedListResult::WrongType => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_wrong_type",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: true,
            };
        }
        BoundedListResult::Inaccessible => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_inaccessible",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: true,
            };
        }
        BoundedListResult::IoError => {
            diagnostics.push(RawDiagnostic::new(
                "playlist_directory_io_error",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            return RetroArchPlaylistInventory {
                directory: Some(EncodedPath::from_path(playlists_dir)),
                playlists: Vec::new(),
                diagnostics: finalize_diagnostics(diagnostics),
                complete: true,
            };
        }
    };

    let mut candidate_files: Vec<(Vec<u8>, PathBuf)> = Vec::new();
    for entry in entries {
        let name_string = entry.file_name.to_string_lossy();
        if !name_string.to_ascii_lowercase().ends_with(PLAYLIST_SUFFIX) {
            continue;
        }
        match entry.probe {
            FsProbe::PresentFile => {}
            FsProbe::Symlink => {
                diagnostics.push(RawDiagnostic::new(
                    "playlist_file_symlink_skipped",
                    DiagnosticSeverity::Warning,
                    DiagnosticCategory::PlaylistInventory,
                    Some(profile),
                    Some(PathPurpose::Playlists),
                    Some(playlists_dir.join(&entry.file_name)),
                ));
                continue;
            }
            _ => continue,
        }
        let full_path = playlists_dir.join(&entry.file_name);
        candidate_files.push((os_str_bytes(full_path.as_os_str()).to_vec(), full_path));
    }
    candidate_files.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut playlists = Vec::new();
    let mut total_entries = 0usize;
    let mut complete = true;
    for (_, full_path) in candidate_files {
        if total_entries >= MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE {
            diagnostics.push(RawDiagnostic::new(
                "playlist_total_entry_limit_reached",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::PlaylistInventory,
                Some(profile),
                Some(PathPurpose::Playlists),
                Some(playlists_dir.clone()),
            ));
            complete = false;
            break;
        }
        let playlist = read_playlist_file(filesystem, &full_path, profile);
        total_entries += playlist.entries.len();
        if !playlist.complete {
            complete = false;
        }
        playlists.push(playlist);
    }

    RetroArchPlaylistInventory {
        directory: Some(EncodedPath::from_path(playlists_dir)),
        playlists,
        diagnostics: finalize_diagnostics(diagnostics),
        complete,
    }
}

// ---- AppImage / desktop-file detection ----

struct RawDesktopEntry {
    entry_type: Option<String>,
    name: Option<String>,
    generic_name: Option<String>,
    exec: Option<String>,
    try_exec: Option<String>,
    icon: Option<String>,
    startup_wm_class: Option<String>,
    hidden: bool,
    no_display: bool,
}

/// Parses only the `[Desktop Entry]` section of a `.desktop` file -
/// verified against the freedesktop.org Desktop Entry Specification.
/// Other sections (e.g. `[Desktop Action ...]`) are ignored entirely;
/// unrecognized keys within `[Desktop Entry]` are also ignored, never
/// rejected. A non-`Key=Value`, non-comment, non-blank, non-section-header
/// line inside `[Desktop Entry]` is recorded by one-based line number and
/// skipped; parsing continues past it. Matches this module's own
/// first-occurrence-wins duplicate-key convention (already used for
/// `retroarch.cfg` and `.info` parsing).
fn parse_desktop_entry(text: &str) -> (RawDesktopEntry, Vec<u32>) {
    let mut entry = RawDesktopEntry {
        entry_type: None,
        name: None,
        generic_name: None,
        exec: None,
        try_exec: None,
        icon: None,
        startup_wm_class: None,
        hidden: false,
        no_display: false,
    };
    let mut malformed_lines = Vec::new();
    let mut in_desktop_entry_section = false;

    for (index, raw_line) in text.split('\n').enumerate() {
        let line_number = (index + 1) as u32;
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_desktop_entry_section = trimmed == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry_section {
            continue;
        }
        match trimmed.split_once('=') {
            Some((key, value)) => {
                let key = key.trim();
                let value = value.trim();
                match key {
                    "Type" => {
                        entry.entry_type.get_or_insert_with(|| value.to_string());
                    }
                    "Name" => {
                        entry.name.get_or_insert_with(|| value.to_string());
                    }
                    "GenericName" => {
                        entry.generic_name.get_or_insert_with(|| value.to_string());
                    }
                    "Exec" => {
                        entry.exec.get_or_insert_with(|| value.to_string());
                    }
                    "TryExec" => {
                        entry.try_exec.get_or_insert_with(|| value.to_string());
                    }
                    "Icon" => {
                        entry.icon.get_or_insert_with(|| value.to_string());
                    }
                    "StartupWMClass" => {
                        entry
                            .startup_wm_class
                            .get_or_insert_with(|| value.to_string());
                    }
                    "Hidden" => entry.hidden = entry.hidden || value == "true",
                    "NoDisplay" => entry.no_display = entry.no_display || value == "true",
                    _ => {}
                }
            }
            None => malformed_lines.push(line_number),
        }
    }
    (entry, malformed_lines)
}

/// `Type` absent or exactly `"Application"`, and `Hidden` not `true` - the
/// only state this milestone treats as a real, evidence-bearing desktop
/// entry. `NoDisplay=true` does *not* affect this: per spec it only means
/// "not shown in menus", not "inactive" - it may still be valid evidence.
fn desktop_entry_is_active(entry: &RawDesktopEntry) -> bool {
    !entry.hidden
        && entry
            .entry_type
            .as_deref()
            .is_none_or(|value| value == "Application")
}

/// Case-insensitive substring check for `"retroarch"` - the one filename/
/// text evidence signal this milestone uses everywhere identity text is
/// inspected (AppImage filename, desktop `Name`/`GenericName`/`Icon`/
/// `StartupWMClass`).
fn mentions_retroarch(value: &str) -> bool {
    value.to_ascii_lowercase().contains("retroarch")
}

fn desktop_entry_name_evidence(entry: &RawDesktopEntry) -> bool {
    [
        entry.name.as_deref(),
        entry.generic_name.as_deref(),
        entry.icon.as_deref(),
        entry.startup_wm_class.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(mentions_retroarch)
}

/// Tokenizes an `Exec` value per the freedesktop.org Desktop Entry
/// Specification's quoting rules: unquoted tokens are whitespace-
/// separated; a double-quoted token may contain escaped `"`, `` ` ``,
/// `$`, and `\` (each unescaped to the literal character); any other
/// backslash sequence inside quotes is left as-is (matching the spec's
/// own note that a literal `\\` must appear as `\\\\` to survive both the
/// general string-escaping rule and this quoting rule - this milestone
/// does not attempt that second unescaping layer, which only matters for
/// literal backslashes, never for identifying the executable token).
/// Returns `None` for an unterminated quote.
fn tokenize_exec(value: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = value.chars().peekable();
    loop {
        while chars
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut token = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    None => return None,
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(character @ ('"' | '`' | '$' | '\\')) => token.push(character),
                        Some(other) => {
                            token.push('\\');
                            token.push(other);
                        }
                        None => return None,
                    },
                    Some(character) => token.push(character),
                }
            }
        } else {
            while let Some(&character) = chars.peek() {
                if character.is_whitespace() {
                    break;
                }
                token.push(character);
                chars.next();
            }
        }
        tokens.push(token);
    }
    Some(tokens)
}

/// Known shell/wrapper commands whose real target this milestone never
/// attempts to resolve - doing so correctly would require a real shell,
/// which this milestone never invokes.
const SHELL_WRAPPER_COMMANDS: [&str; 5] = ["sh", "bash", "dash", "zsh", "flatpak-spawn"];

enum ExecTarget {
    Token(String),
    ShellWrapper,
    Empty,
}

/// Resolves an `Exec` token list down to the single executable text this
/// milestone will compare against AppImage candidates - conservatively
/// unwrapping a leading `env NAME=value ...` prefix (a well-defined,
/// purely textual transformation, never executed), and refusing to look
/// further into any other recognized shell wrapper.
fn resolve_exec_target(tokens: &[String]) -> ExecTarget {
    let mut index = 0;
    if tokens.first().map(String::as_str) == Some("env") {
        index = 1;
        while tokens
            .get(index)
            .is_some_and(|token| looks_like_env_assignment(token))
        {
            index += 1;
        }
    }
    match tokens.get(index) {
        None => ExecTarget::Empty,
        Some(token) => {
            let basename = Path::new(token)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(token.as_str());
            if SHELL_WRAPPER_COMMANDS.contains(&basename) {
                ExecTarget::ShellWrapper
            } else {
                ExecTarget::Token(token.clone())
            }
        }
    }
}

fn looks_like_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        }
        None => false,
    }
}

/// Compares one already-tokenized `Exec` value's resolved executable
/// target against one AppImage candidate's real path. A bare (non-
/// absolute, no path separator) token is compared only by basename - this
/// milestone does not perform a full `$PATH` lookup for desktop `Exec`
/// resolution.
fn exec_resolution_for_candidate(
    filesystem: &dyn ReadOnlyHostFilesystem,
    target: &ExecTarget,
    candidate_path: &Path,
) -> ExecResolution {
    match target {
        ExecTarget::Empty => ExecResolution::Unresolved,
        ExecTarget::ShellWrapper => ExecResolution::ShellWrapperUnresolved,
        ExecTarget::Token(text) => {
            if text.starts_with('%') {
                return ExecResolution::Unresolved;
            }
            let token_path = Path::new(text);
            let is_bare_name = !text.contains('/');
            let matches = if is_bare_name {
                token_path.file_name() == candidate_path.file_name()
            } else {
                token_path == candidate_path
            };
            if matches {
                return ExecResolution::MatchesCandidate;
            }
            if !is_bare_name && filesystem.probe(token_path) == FsProbe::Missing {
                return ExecResolution::TargetMissing;
            }
            ExecResolution::MismatchedTarget {
                target: EncodedPath::from_path(token_path),
            }
        }
    }
}

/// Scans an already-tokenized `Exec` value for `-c`/`--config` and
/// returns the explicit config file path it names, if any - verified
/// against `retroarch.c`'s own argument parser (`-c`/`--config=FILE` sets
/// `path_config_file`). `Some(None)` means the flag is present but its
/// value could not be resolved to a real path (e.g. a field code);
/// `None` means the flag is simply absent.
fn exec_explicit_config_value(tokens: &[String]) -> Option<Option<String>> {
    let mut found: Option<Option<String>> = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let value = if let Some(rest) = token.strip_prefix("--config=") {
            Some(rest.to_string())
        } else if token == "--config" || token == "-c" {
            tokens.get(index + 1).cloned()
        } else {
            None
        };
        if let Some(value) = value {
            found = Some(if value.is_empty() || value.starts_with('%') {
                None
            } else {
                Some(value)
            });
        }
        index += 1;
    }
    found
}

fn read_desktop_file(
    filesystem: &dyn ReadOnlyHostFilesystem,
    path: &Path,
) -> (Option<RawDesktopEntry>, Vec<RawDiagnostic>) {
    let mut diagnostics = Vec::new();
    let entry = match filesystem.read_bounded(path, MAX_DESKTOP_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) => {
            let bytes = strip_utf8_bom(&bytes);
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    let (entry, malformed_lines) = parse_desktop_entry(text);
                    for line in malformed_lines {
                        let mut diagnostic = RawDiagnostic::new(
                            "desktop_file_malformed_line",
                            DiagnosticSeverity::Info,
                            DiagnosticCategory::AppImageInventory,
                            None,
                            None,
                            Some(path.to_path_buf()),
                        );
                        diagnostic.entry_index = Some(line);
                        diagnostics.push(diagnostic);
                    }
                    if !desktop_entry_is_active(&entry) {
                        diagnostics.push(RawDiagnostic::new(
                            "desktop_entry_inactive",
                            DiagnosticSeverity::Info,
                            DiagnosticCategory::AppImageInventory,
                            None,
                            None,
                            Some(path.to_path_buf()),
                        ));
                    }
                    Some(entry)
                }
                Err(_) => {
                    diagnostics.push(RawDiagnostic::new(
                        "desktop_file_invalid_utf8",
                        DiagnosticSeverity::Warning,
                        DiagnosticCategory::AppImageInventory,
                        None,
                        None,
                        Some(path.to_path_buf()),
                    ));
                    None
                }
            }
        }
        BoundedReadResult::TooLarge => {
            diagnostics.push(RawDiagnostic::new(
                "desktop_file_too_large",
                DiagnosticSeverity::Warning,
                DiagnosticCategory::AppImageInventory,
                None,
                None,
                Some(path.to_path_buf()),
            ));
            None
        }
        _ => None,
    };
    (entry, diagnostics)
}

/// One successfully-parsed, active desktop entry, kept internally with
/// its own real `PathBuf` (for deterministic sorting and exec-target
/// comparison) alongside the already-tokenized `Exec` resolution.
struct DiscoveredDesktopEntry {
    path: PathBuf,
    name_evidence: bool,
    exec_target: Option<ExecTarget>,
    explicit_config: Option<Option<String>>,
}

fn discover_desktop_entries(
    filesystem: &dyn ReadOnlyHostFilesystem,
    roots: &[PathBuf],
    diagnostics: &mut Vec<RawDiagnostic>,
) -> Vec<DiscoveredDesktopEntry> {
    let mut discovered = Vec::new();
    for root in roots {
        let entries = match filesystem.list_dir_bounded(root, MAX_DESKTOP_FILES_PER_DIRECTORY) {
            BoundedListResult::Ok(entries) => entries,
            BoundedListResult::TooLarge => {
                diagnostics.push(RawDiagnostic::new(
                    "appimage_directory_listing_too_large",
                    DiagnosticSeverity::Warning,
                    DiagnosticCategory::AppImageInventory,
                    None,
                    None,
                    Some(root.clone()),
                ));
                continue;
            }
            _ => continue,
        };
        for entry in entries {
            let name_string = entry.file_name.to_string_lossy();
            if !name_string.to_ascii_lowercase().ends_with(DESKTOP_SUFFIX) {
                continue;
            }
            if entry.probe != FsProbe::PresentFile {
                continue;
            }
            let full_path = root.join(&entry.file_name);
            let (parsed, mut file_diagnostics) = read_desktop_file(filesystem, &full_path);
            diagnostics.append(&mut file_diagnostics);
            let Some(parsed) = parsed else {
                continue;
            };
            if !desktop_entry_is_active(&parsed) {
                continue;
            }
            let name_evidence = desktop_entry_name_evidence(&parsed);
            let exec_source = parsed.exec.as_deref().or(parsed.try_exec.as_deref());
            let (exec_target, explicit_config) = match exec_source.map(tokenize_exec) {
                Some(Some(tokens)) => (
                    Some(resolve_exec_target(&tokens)),
                    exec_explicit_config_value(&tokens),
                ),
                _ => (None, None),
            };
            discovered.push(DiscoveredDesktopEntry {
                path: full_path,
                name_evidence,
                exec_target,
                explicit_config,
            });
        }
    }
    discovered.sort_by(|left, right| {
        os_str_bytes(left.path.as_os_str()).cmp(os_str_bytes(right.path.as_os_str()))
    });
    discovered
}

fn discover_app_image_files(
    filesystem: &dyn ReadOnlyHostFilesystem,
    roots: &[PathBuf],
    diagnostics: &mut Vec<RawDiagnostic>,
) -> Vec<PathBuf> {
    let mut candidate_paths = Vec::new();
    for root in roots {
        let entries = match filesystem.list_dir_bounded(root, MAX_APPIMAGE_SEARCH_ROOT_ENTRIES) {
            BoundedListResult::Ok(entries) => entries,
            BoundedListResult::TooLarge => {
                diagnostics.push(RawDiagnostic::new(
                    "appimage_directory_listing_too_large",
                    DiagnosticSeverity::Warning,
                    DiagnosticCategory::AppImageInventory,
                    None,
                    None,
                    Some(root.clone()),
                ));
                continue;
            }
            BoundedListResult::NotFound => {
                // Info, not Warning: most of the fixed default search
                // roots do not exist for a given user (e.g. `~/AppImages`
                // is uncommon), so this is expected, routine information,
                // not something to alarm about - but it is still recorded
                // so a consumer can see exactly which roots were actually
                // scanned.
                diagnostics.push(RawDiagnostic::new(
                    "appimage_search_root_missing",
                    DiagnosticSeverity::Info,
                    DiagnosticCategory::AppImageInventory,
                    None,
                    None,
                    Some(root.clone()),
                ));
                continue;
            }
            BoundedListResult::Inaccessible => {
                diagnostics.push(RawDiagnostic::new(
                    "appimage_search_root_inaccessible",
                    DiagnosticSeverity::Warning,
                    DiagnosticCategory::AppImageInventory,
                    None,
                    None,
                    Some(root.clone()),
                ));
                continue;
            }
            _ => continue,
        };
        for entry in entries {
            let name_string = entry.file_name.to_string_lossy();
            if !name_string.to_ascii_lowercase().ends_with(APPIMAGE_SUFFIX) {
                continue;
            }
            let full_path = root.join(&entry.file_name);
            if entry.probe == FsProbe::Symlink {
                diagnostics.push(RawDiagnostic::new(
                    "appimage_candidate_symlink",
                    DiagnosticSeverity::Warning,
                    DiagnosticCategory::AppImageInventory,
                    None,
                    None,
                    Some(full_path),
                ));
                continue;
            }
            candidate_paths.push(full_path);
        }
    }
    candidate_paths
        .sort_by(|left, right| os_str_bytes(left.as_os_str()).cmp(os_str_bytes(right.as_os_str())));
    candidate_paths.dedup();
    candidate_paths
}

/// `<appimage-path>.home`/`<appimage-path>.config` - the official
/// AppImage `type2-runtime`'s own portable-mode convention (verified:
/// `set_portable_home_and_config` in `AppImage/type2-runtime`'s
/// `runtime.c`), which overrides `$HOME`/`$XDG_CONFIG_HOME` for the
/// launched process before RetroArch itself ever starts. Returns the
/// resulting effective RetroArch config directory when either sibling
/// directory is found, using the exact same XDG resolution the native
/// profile already uses.
fn portable_config_directory(
    filesystem: &dyn ReadOnlyHostFilesystem,
    appimage_path: &Path,
) -> Option<PathBuf> {
    let mut portable_home = appimage_path.as_os_str().to_os_string();
    portable_home.push(".home");
    let portable_home_path = PathBuf::from(portable_home);
    let portable_home_found = filesystem.probe(&portable_home_path) == FsProbe::PresentDirectory;

    let mut portable_config = appimage_path.as_os_str().to_os_string();
    portable_config.push(".config");
    let portable_config_path = PathBuf::from(portable_config);
    let portable_config_found =
        filesystem.probe(&portable_config_path) == FsProbe::PresentDirectory;

    if !portable_home_found && !portable_config_found {
        return None;
    }
    let effective_config_home = if portable_config_found {
        portable_config_path
    } else {
        portable_home_path.join(".config")
    };
    Some(effective_config_home.join("retroarch"))
}

#[allow(clippy::too_many_arguments)]
fn build_app_image_candidate(
    filesystem: &dyn ReadOnlyHostFilesystem,
    path: &Path,
    desktop_entries: &[DiscoveredDesktopEntry],
    native_config_dir: &Path,
) -> Option<AppImageCandidate> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let filename_evidence = mentions_retroarch(&file_name);

    let mut desktop_evidence = Vec::new();
    let mut explicit_config_values: Vec<Option<String>> = Vec::new();
    for desktop_entry in desktop_entries {
        if !desktop_entry.name_evidence {
            // A desktop entry that never mentions RetroArch in its own
            // Name/GenericName/Icon/StartupWMClass is not evidence that
            // *this* AppImage is RetroArch, even when its `Exec` happens
            // to resolve to this exact file - every properly-installed
            // AppImage typically has its own desktop entry whose `Exec`
            // points at itself, so an exec-match-only rule here would
            // flag virtually any AppImage that has any desktop file at
            // all (verified against real installed AppImages during the
            // real-world smoke test: ES-DE, Heroic, LosslessCut, Vita3K,
            // and Eden were all falsely flagged before this guard was
            // added).
            continue;
        }
        let resolution = match &desktop_entry.exec_target {
            Some(target) => exec_resolution_for_candidate(filesystem, target, path),
            None => ExecResolution::Unresolved,
        };
        if matches!(
            resolution,
            ExecResolution::MismatchedTarget { .. } | ExecResolution::TargetMissing
        ) {
            // This entry's `Exec` names a specific, different (or
            // nonexistent) path, so it is not evidence for *this*
            // existing candidate specifically - it may still be
            // independently evidence for whichever candidate it does
            // resolve to.
            continue;
        }
        if resolution == ExecResolution::MatchesCandidate
            && let Some(explicit_config) = &desktop_entry.explicit_config
        {
            explicit_config_values.push(explicit_config.clone());
        }
        desktop_evidence.push(DesktopEntryMatch {
            desktop_file: EncodedPath::from_path(&desktop_entry.path),
            name_evidence: desktop_entry.name_evidence,
            exec_resolution: resolution,
        });
    }

    if !filename_evidence && desktop_evidence.is_empty() {
        return None;
    }

    let confidence = if desktop_evidence.is_empty() {
        AppImageIdentificationConfidence::Weak
    } else if desktop_evidence
        .iter()
        .any(|entry| entry.exec_resolution == ExecResolution::MatchesCandidate)
    {
        AppImageIdentificationConfidence::Exact
    } else {
        AppImageIdentificationConfidence::Strong
    };

    let probe = filesystem.probe(path);
    let executable = if probe == FsProbe::PresentFile {
        filesystem
            .probe_regular_file_executable_bit(path)
            .map(|bit| {
                if bit {
                    ExecutableState::Executable
                } else {
                    ExecutableState::NotExecutable
                }
            })
    } else {
        None
    };

    let config_association =
        resolve_config_association(filesystem, path, native_config_dir, &explicit_config_values);

    Some(AppImageCandidate {
        path: EncodedPath::from_path(path),
        probe,
        executable,
        filename_evidence,
        desktop_evidence,
        confidence,
        config_association,
    })
}

fn resolve_config_association(
    filesystem: &dyn ReadOnlyHostFilesystem,
    appimage_path: &Path,
    native_config_dir: &Path,
    explicit_config_values: &[Option<String>],
) -> ConfigAssociation {
    // Explicit `--config`/`-c` evidence, when present, is verified
    // RetroArch-level behavior and takes precedence over the AppImage
    // runtime's own portable-mode convention (which would be overridden
    // by RetroArch's own `-c` handling in practice).
    let mut resolved_explicit: Vec<PathBuf> = Vec::new();
    let mut unresolved_explicit = false;
    for value in explicit_config_values {
        match value {
            Some(path_text) => {
                let config_file = PathBuf::from(path_text);
                if let Some(parent) = config_file.parent() {
                    resolved_explicit.push(parent.to_path_buf());
                } else {
                    unresolved_explicit = true;
                }
            }
            None => unresolved_explicit = true,
        }
    }
    if !resolved_explicit.is_empty() {
        resolved_explicit.sort();
        resolved_explicit.dedup();
        return match resolved_explicit.len() {
            1 => {
                let directory = resolved_explicit.into_iter().next().unwrap();
                if directory == native_config_dir {
                    ConfigAssociation::SharesNativeProfile
                } else {
                    ConfigAssociation::ExplicitConfig {
                        config_directory: EncodedPath::from_path(&directory),
                    }
                }
            }
            _ => ConfigAssociation::Ambiguous,
        };
    }
    if unresolved_explicit {
        return ConfigAssociation::Unknown;
    }

    match portable_config_directory(filesystem, appimage_path) {
        Some(directory) if directory == native_config_dir => ConfigAssociation::SharesNativeProfile,
        Some(directory) => ConfigAssociation::PortableConfigDetected {
            config_directory: EncodedPath::from_path(&directory),
        },
        None => ConfigAssociation::SharesNativeProfile,
    }
}

/// Distinct-config candidates found, grouped by resolved directory. When
/// exactly one distinct directory is agreed upon, that becomes the one
/// distinct AppImage profile this milestone supports; when candidates
/// disagree on 2+ distinct directories, no profile is promoted at all
/// (every such candidate is instead attached to the native profile with
/// its association downgraded to `Ambiguous`, and a diagnostic recorded) -
/// a conservative, explicit "do not guess" outcome rather than an
/// unbounded multi-profile model.
struct DistinctConfigPartition {
    shared: Vec<AppImageCandidate>,
    distinct: Option<(PathBuf, Vec<AppImageCandidate>)>,
    ambiguous_merge_diagnostic: bool,
}

fn partition_by_config_association(candidates: Vec<AppImageCandidate>) -> DistinctConfigPartition {
    let mut shared = Vec::new();
    let mut distinct_by_directory: BTreeMap<PathBuf, Vec<AppImageCandidate>> = BTreeMap::new();
    for candidate in candidates {
        match &candidate.config_association {
            ConfigAssociation::PortableConfigDetected { config_directory }
            | ConfigAssociation::ExplicitConfig { config_directory } => {
                let directory = PathBuf::from(&config_directory.display);
                distinct_by_directory
                    .entry(directory)
                    .or_default()
                    .push(candidate);
            }
            _ => shared.push(candidate),
        }
    }

    match distinct_by_directory.len() {
        0 => DistinctConfigPartition {
            shared,
            distinct: None,
            ambiguous_merge_diagnostic: false,
        },
        1 => {
            let (directory, distinct_candidates) =
                distinct_by_directory.into_iter().next().unwrap();
            DistinctConfigPartition {
                shared,
                distinct: Some((directory, distinct_candidates)),
                ambiguous_merge_diagnostic: false,
            }
        }
        _ => {
            for (_, mut candidates) in distinct_by_directory {
                for candidate in &mut candidates {
                    candidate.config_association = ConfigAssociation::Ambiguous;
                }
                shared.append(&mut candidates);
            }
            DistinctConfigPartition {
                shared,
                distinct: None,
                ambiguous_merge_diagnostic: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator_environment::HostReadOnlyFilesystem;
    use std::fs;

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "archivefs-retroarch-env-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn mkdir(&self, relative: &str) {
            fs::create_dir_all(self.path(relative)).unwrap();
        }

        fn env(&self) -> DiscoveryEnvironment {
            DiscoveryEnvironment {
                home: Some(self.root.clone().into_os_string()),
                xdg_config_home: None,
                path: None,
                user_flatpak_root: self.path("user-flatpak"),
                system_flatpak_root: self.path("system-flatpak"),
                app_image_search_roots: Vec::new(),
                desktop_file_roots: Vec::new(),
            }
        }

        /// A `retroarch.cfg` body whose configured directories are real,
        /// absolute paths under this fixture's own tempdir root (never a
        /// literal `/opt/...`-style path, which would not actually exist
        /// on the machine running the test).
        fn native_config_body(&self) -> String {
            format!(
                "system_directory = \"{}\"\nlibretro_directory = \"{}\"\nlibretro_info_path = \"{}\"\n",
                self.path("opt/retroarch/system").display(),
                self.path("opt/retroarch/cores").display(),
                self.path("opt/retroarch/info").display(),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn discovery_fails_only_when_home_is_unset() {
        let filesystem = HostReadOnlyFilesystem;
        let env = DiscoveryEnvironment {
            home: None,
            xdg_config_home: None,
            path: None,
            user_flatpak_root: PathBuf::from("/nonexistent"),
            system_flatpak_root: PathBuf::from("/nonexistent"),
            app_image_search_roots: Vec::new(),
            desktop_file_roots: Vec::new(),
        };
        assert_eq!(
            discover_retroarch_environment(&filesystem, &env).unwrap_err(),
            DiscoveryError::NoHome
        );
    }

    #[test]
    fn no_evidence_produces_three_not_detected_profiles() {
        let fixture = Fixture::new("no-evidence");
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();

        assert_eq!(report.format_version, 2);
        assert_eq!(report.profiles.len(), 3);
        for profile in &report.profiles {
            assert!(profile.evidence.executables.is_empty());
            assert!(!profile.evidence.config_directory_found);
            assert!(!profile.evidence.config_file_found);
            assert!(profile.cores.is_empty());
            assert!(profile.app_images.is_empty());
        }
        assert_eq!(report.profiles[0].profile_kind, ProfileKind::Native);
        assert_eq!(report.profiles[1].profile_kind, ProfileKind::Flatpak);
        assert_eq!(report.profiles[1].scope, ProfileScope::User);
        assert_eq!(report.profiles[2].profile_kind, ProfileKind::Flatpak);
        assert_eq!(report.profiles[2].scope, ProfileScope::System);
    }

    #[test]
    fn native_config_file_is_parsed_and_resolved() {
        let fixture = Fixture::new("native-config");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.native_config_body(),
        );
        fixture.mkdir("opt/retroarch/system");

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let native = &report.profiles[0];

        assert!(native.evidence.config_file_found);
        let system = native
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.resolution, ResolutionState::ConfiguredResolved);
        assert_eq!(
            system.resolved_path.as_ref().unwrap().display,
            fixture.path("opt/retroarch/system").to_string_lossy()
        );
        assert_eq!(system.probe, Some(FsProbe::PresentDirectory));
    }

    #[test]
    fn missing_key_is_runtime_default_unknown_not_missing() {
        let fixture = Fixture::new("missing-key");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/x\"\n",
        );

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let native = &report.profiles[0];
        let saves = native
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::Saves)
            .unwrap();
        assert_eq!(saves.resolution, ResolutionState::RuntimeDefaultUnknown);
        assert_eq!(saves.configured_value, None);
    }

    #[test]
    fn empty_value_is_runtime_default_unknown_with_configured_value_present() {
        let fixture = Fixture::new("empty-value");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "savefile_directory = \"\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let saves = report.profiles[0]
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::Saves)
            .unwrap();
        assert_eq!(saves.resolution, ResolutionState::RuntimeDefaultUnknown);
        assert_eq!(saves.configured_value.as_deref(), Some(""));
    }

    #[test]
    fn config_missing_marks_every_purpose_no_readable_config() {
        let fixture = Fixture::new("config-missing");
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        for finding in &report.profiles[0].paths {
            assert_eq!(finding.resolution, ResolutionState::NoReadableConfig);
        }
        assert!(matches!(
            report.profiles[0].config_file.read,
            ConfigReadOutcome::NotAttempted
        ));
    }

    #[test]
    fn malformed_lines_are_reported_with_one_based_line_numbers_and_parsing_continues() {
        let fixture = Fixture::new("malformed-lines");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "this line has no equals\nsystem_directory = \"/ok\"\nalso broken\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        match &report.profiles[0].config_file.read {
            ConfigReadOutcome::Parsed {
                malformed_lines, ..
            } => {
                assert_eq!(malformed_lines, &[1, 3]);
            }
            other => panic!("expected Parsed, got {other:?}"),
        }
        let system = report.profiles[0]
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.configured_value.as_deref(), Some("/ok"));
    }

    #[test]
    fn duplicate_keys_use_first_occurrence() {
        let fixture = Fixture::new("duplicate-keys");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/first\"\nsystem_directory = \"/second\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let system = report.profiles[0]
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.configured_value.as_deref(), Some("/first"));
    }

    #[test]
    fn comments_and_trailing_comments_and_hashes_in_quotes_are_handled() {
        let fixture = Fixture::new("comments");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "# whole line comment\n\
             system_directory = \"/ok\" # trailing comment\n\
             cheat_database_path = \"/has#hash/inside\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let paths = &report.profiles[0].paths;
        let system = paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.configured_value.as_deref(), Some("/ok"));
        let cheats = paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::Cheats)
            .unwrap();
        assert_eq!(cheats.configured_value.as_deref(), Some("/has#hash/inside"));
        match &report.profiles[0].config_file.read {
            ConfigReadOutcome::Parsed {
                malformed_lines, ..
            } => assert!(malformed_lines.is_empty()),
            other => panic!("expected Parsed, got {other:?}"),
        }
    }

    #[test]
    fn crlf_and_utf8_bom_are_handled() {
        let fixture = Fixture::new("crlf-bom");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(
            b"system_directory = \"/ok\"\r\ncheat_database_path = \"/also-ok\"\r\n",
        );
        fs::create_dir_all(fixture.path(".config/retroarch")).unwrap();
        fs::write(fixture.path(".config/retroarch/retroarch.cfg"), bytes).unwrap();

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let paths = &report.profiles[0].paths;
        assert_eq!(
            paths
                .iter()
                .find(|finding| finding.purpose == PathPurpose::System)
                .unwrap()
                .configured_value
                .as_deref(),
            Some("/ok")
        );
        assert_eq!(
            paths
                .iter()
                .find(|finding| finding.purpose == PathPurpose::Cheats)
                .unwrap()
                .configured_value
                .as_deref(),
            Some("/also-ok")
        );
    }

    #[test]
    fn include_directive_is_detected_not_followed_and_marks_result_incomplete() {
        let fixture = Fixture::new("include");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/ok\"\n#include \"other.cfg\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        match &report.profiles[0].config_file.read {
            ConfigReadOutcome::Parsed {
                include_detected,
                complete,
                ..
            } => {
                assert!(*include_detected);
                assert!(!*complete);
            }
            other => panic!("expected Parsed, got {other:?}"),
        }
        assert!(
            report.profiles[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "include_directive_not_followed")
        );
    }

    #[test]
    fn invalid_utf8_config_is_reported_and_blocks_every_path() {
        let fixture = Fixture::new("invalid-utf8");
        fs::create_dir_all(fixture.path(".config/retroarch")).unwrap();
        fs::write(
            fixture.path(".config/retroarch/retroarch.cfg"),
            [0xFF, 0xFE, 0x00, 0x01],
        )
        .unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert!(matches!(
            report.profiles[0].config_file.read,
            ConfigReadOutcome::InvalidUtf8
        ));
        for finding in &report.profiles[0].paths {
            assert_eq!(finding.resolution, ResolutionState::NoReadableConfig);
        }
        assert!(
            report.profiles[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "config_file_invalid_utf8")
        );
    }

    #[test]
    fn oversized_config_is_reported_as_too_large() {
        let fixture = Fixture::new("oversized-config");
        let big = "x".repeat(MAX_CONFIG_BYTES + 1);
        fixture.write(".config/retroarch/retroarch.cfg", &big);
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert!(matches!(
            report.profiles[0].config_file.read,
            ConfigReadOutcome::TooLarge { limit_bytes } if limit_bytes == MAX_CONFIG_BYTES as u64
        ));
    }

    #[test]
    fn absolute_tilde_colon_and_relative_paths_resolve_as_documented() {
        let fixture = Fixture::new("path-forms");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/absolute/path\"\n\
             libretro_directory = \"~/cores\"\n\
             libretro_info_path = \":\\some\\app\\dir\\info\"\n\
             savefile_directory = \"relative/saves\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let paths = &report.profiles[0].paths;

        let system = paths
            .iter()
            .find(|f| f.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.resolution, ResolutionState::ConfiguredResolved);
        assert_eq!(
            system.resolved_path.as_ref().unwrap().display,
            "/absolute/path"
        );

        let cores = paths
            .iter()
            .find(|f| f.purpose == PathPurpose::Cores)
            .unwrap();
        assert_eq!(cores.resolution, ResolutionState::ConfiguredResolved);
        assert_eq!(
            cores.resolved_path.as_ref().unwrap().display,
            fixture.path("cores").to_string_lossy()
        );

        let info = paths
            .iter()
            .find(|f| f.purpose == PathPurpose::CoreInfo)
            .unwrap();
        assert_eq!(info.resolution, ResolutionState::ConfiguredUnresolved);
        assert_eq!(info.resolved_path, None);

        let saves = paths
            .iter()
            .find(|f| f.purpose == PathPurpose::Saves)
            .unwrap();
        assert_eq!(saves.resolution, ResolutionState::ConfiguredUnresolved);

        let codes: Vec<_> = report.profiles[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect();
        assert!(codes.contains(&"colon_alias_unresolved"));
        assert!(codes.contains(&"relative_path_unresolved"));
    }

    #[test]
    fn configured_directory_missing_produces_a_diagnostic() {
        let fixture = Fixture::new("configured-missing");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/does/not/exist\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let system = report.profiles[0]
            .paths
            .iter()
            .find(|f| f.purpose == PathPurpose::System)
            .unwrap();
        assert_eq!(system.probe, Some(FsProbe::Missing));
        assert!(
            report.profiles[0]
                .diagnostics
                .iter()
                .any(|d| d.code == "configured_directory_missing")
        );
    }

    #[test]
    fn cores_are_discovered_sorted_and_filtered() {
        let fixture = Fixture::new("cores");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\n",
                fixture.path("cores").display()
            ),
        );
        fixture.mkdir("cores");
        fixture.write("cores/zzz_libretro.so", "stub");
        fixture.write("cores/aaa_libretro.so", "stub");
        fixture.write("cores/unrelated.txt", "stub");
        fixture.write("cores/aaa_libretro.so.bak", "stub");

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let cores = &report.profiles[0].cores;
        assert_eq!(cores.len(), 2);
        assert_eq!(cores[0].core_stem, "aaa");
        assert_eq!(cores[1].core_stem, "zzz");
        assert!(matches!(
            cores[0].info,
            CoreInfoFinding::DirectoryUnavailable
        ));
    }

    #[test]
    fn core_symlink_is_reported_and_not_followed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("core-symlink");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\n",
                fixture.path("cores").display()
            ),
        );
        fixture.mkdir("cores");
        fixture.write("real_libretro.so", "stub");
        symlink(
            fixture.path("real_libretro.so"),
            fixture.path("cores/link_libretro.so"),
        )
        .unwrap();

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert!(report.profiles[0].cores.is_empty());
        assert!(
            report.profiles[0]
                .diagnostics
                .iter()
                .any(|d| d.code == "core_symlink_skipped")
        );
    }

    #[test]
    fn matching_info_file_is_parsed() {
        let fixture = Fixture::new("core-info");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\nlibretro_info_path = \"{}\"\n",
                fixture.path("cores").display(),
                fixture.path("info").display()
            ),
        );
        fixture.mkdir("cores");
        fixture.mkdir("info");
        fixture.write("cores/snes9x_libretro.so", "stub");
        fixture.write(
            "info/snes9x.info",
            "display_name = \"Nintendo - SNES / SFC (Snes9x)\"\n\
             display_version = \"1.62.3\"\n\
             systemname = \"Nintendo - SNES / SFC\"\n\
             supported_extensions = \"smc|sfc|swc||fig\"\n",
        );

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let core = &report.profiles[0].cores[0];
        match &core.info {
            CoreInfoFinding::Found {
                display_name,
                display_version,
                system_name,
                supported_extensions,
            } => {
                assert_eq!(
                    display_name.as_deref(),
                    Some("Nintendo - SNES / SFC (Snes9x)")
                );
                assert_eq!(display_version.as_deref(), Some("1.62.3"));
                assert_eq!(system_name.as_deref(), Some("Nintendo - SNES / SFC"));
                assert_eq!(supported_extensions, &vec!["smc", "sfc", "swc", "fig"]);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn missing_info_file_is_a_normal_finding_not_a_diagnostic() {
        let fixture = Fixture::new("core-info-missing");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\nlibretro_info_path = \"{}\"\n",
                fixture.path("cores").display(),
                fixture.path("info").display()
            ),
        );
        fixture.mkdir("cores");
        fixture.mkdir("info");
        fixture.write("cores/nes_libretro.so", "stub");

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert!(matches!(
            report.profiles[0].cores[0].info,
            CoreInfoFinding::Missing
        ));
        assert!(report.profiles[0].diagnostics.is_empty());
    }

    #[test]
    fn oversized_info_file_is_reported() {
        let fixture = Fixture::new("core-info-toolarge");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\nlibretro_info_path = \"{}\"\n",
                fixture.path("cores").display(),
                fixture.path("info").display()
            ),
        );
        fixture.mkdir("cores");
        fixture.mkdir("info");
        fixture.write("cores/big_libretro.so", "stub");
        fixture.write("info/big.info", &"x".repeat(MAX_INFO_BYTES + 1));

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert!(matches!(
            report.profiles[0].cores[0].info,
            CoreInfoFinding::TooLarge
        ));
    }

    #[test]
    fn flatpak_evidence_is_recorded_independently_per_scope() {
        let fixture = Fixture::new("flatpak-evidence");
        fixture.mkdir("user-flatpak/app/org.libretro.RetroArch");

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let user_flatpak = &report.profiles[1];
        let system_flatpak = &report.profiles[2];
        assert_eq!(user_flatpak.scope, ProfileScope::User);
        assert!(user_flatpak.evidence.flatpak_metadata_found);
        assert_eq!(system_flatpak.scope, ProfileScope::System);
        assert!(!system_flatpak.evidence.flatpak_metadata_found);
    }

    #[test]
    fn flatpak_config_never_falls_back_to_a_config_directory() {
        // Flatpak's own environment sets XDG_CONFIG_HOME to
        // `.var/app/<id>/config` (no dot), never `.config`.
        let fixture = Fixture::new("flatpak-config-path");
        fixture.write(
            ".var/app/org.libretro.RetroArch/config/retroarch/retroarch.cfg",
            "system_directory = \"/flatpak/system\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let user_flatpak = &report.profiles[1];
        assert!(user_flatpak.evidence.config_file_found);
        assert!(
            user_flatpak
                .config_file
                .path
                .display
                .ends_with(".var/app/org.libretro.RetroArch/config/retroarch/retroarch.cfg")
        );
    }

    #[test]
    fn flatpak_tilde_resolves_against_sandbox_home_not_host_home() {
        let fixture = Fixture::new("flatpak-tilde");
        fixture.write(
            ".var/app/org.libretro.RetroArch/config/retroarch/retroarch.cfg",
            "libretro_directory = \"~/cores\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let user_flatpak = &report.profiles[1];
        let cores = user_flatpak
            .paths
            .iter()
            .find(|f| f.purpose == PathPurpose::Cores)
            .unwrap();
        assert_eq!(
            cores.resolved_path.as_ref().unwrap().display,
            fixture
                .path(".var/app/org.libretro.RetroArch/cores")
                .to_string_lossy()
        );
    }

    #[test]
    fn xdg_config_home_relative_value_is_ignored_per_xdg_spec() {
        let fixture = Fixture::new("xdg-relative");
        let mut env = fixture.env();
        env.xdg_config_home = Some(std::ffi::OsString::from("relative/path"));
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/ok\"\n",
        );

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].evidence.config_file_found);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "xdg_config_home_relative_ignored")
        );
    }

    #[test]
    fn xdg_config_home_absolute_value_is_used_directly() {
        let fixture = Fixture::new("xdg-absolute");
        let custom = fixture.path("custom-xdg");
        fs::create_dir_all(custom.join("retroarch")).unwrap();
        fs::write(
            custom.join("retroarch/retroarch.cfg"),
            "system_directory = \"/ok\"\n",
        )
        .unwrap();
        let mut env = fixture.env();
        env.xdg_config_home = Some(custom.into_os_string());

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].evidence.config_file_found);
    }

    #[test]
    fn multiple_path_candidates_are_deduplicated_and_ordered() {
        let fixture = Fixture::new("path-candidates");
        fixture.mkdir("bin1");
        fixture.mkdir("bin2");
        fixture.write("bin1/retroarch", "#!/bin/sh\n");
        fixture.write("bin2/retroarch", "#!/bin/sh\n");
        make_executable(&fixture.path("bin1/retroarch"));
        make_executable(&fixture.path("bin2/retroarch"));

        let mut env = fixture.env();
        let path_value = format!(
            "{}:{}:{}",
            fixture.path("bin1").display(),
            fixture.path("bin1").display(), // duplicate entry
            fixture.path("bin2").display()
        );
        env.path = Some(std::ffi::OsString::from(path_value));

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        let executables = &report.profiles[0].evidence.executables;
        assert_eq!(executables.len(), 2);
        assert!(
            executables[0]
                .display
                .starts_with(&fixture.path("bin1").to_string_lossy().to_string())
        );
        assert!(
            executables[1]
                .display
                .starts_with(&fixture.path("bin2").to_string_lossy().to_string())
        );
    }

    #[test]
    fn non_executable_path_candidate_is_ignored() {
        let fixture = Fixture::new("non-executable");
        fixture.mkdir("bin");
        fixture.write("bin/retroarch", "not executable");
        let mut env = fixture.env();
        env.path = Some(fixture.path("bin").into_os_string());

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].evidence.executables.is_empty());
    }

    #[test]
    fn path_directory_candidate_is_ignored() {
        let fixture = Fixture::new("path-is-dir");
        fixture.mkdir("bin/retroarch");
        let mut env = fixture.env();
        env.path = Some(fixture.path("bin").into_os_string());

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].evidence.executables.is_empty());
    }

    #[test]
    fn empty_path_produces_no_candidates() {
        let fixture = Fixture::new("empty-path");
        let mut env = fixture.env();
        env.path = Some(std::ffi::OsString::from(""));
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].evidence.executables.is_empty());
    }

    #[test]
    fn json_report_key_sets_are_stable() {
        let fixture = Fixture::new("json-keys");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "{}playlist_directory = \"{}\"\n",
                fixture.native_config_body(),
                fixture.path("opt/retroarch/playlists").display(),
            ),
        );
        fixture.mkdir("opt/retroarch/cores");
        fixture.write("opt/retroarch/cores/test_libretro.so", "stub");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Nintendo - Super Nintendo Entertainment System.lpl",
            r#"{
                "version": "1.5",
                "default_core_path": "",
                "default_core_name": "",
                "items": [
                    {
                        "path": "/roms/snes/game.sfc",
                        "label": "Game",
                        "core_path": "DETECT",
                        "core_name": "DETECT",
                        "crc32": "00000000|crc",
                        "db_name": "Nintendo - Super Nintendo Entertainment System.lpl"
                    }
                ]
            }"#,
        );
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");

        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        let json = serde_json::to_value(&report).unwrap();

        let mut top_keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        top_keys.sort();
        assert_eq!(top_keys, vec!["diagnostics", "format_version", "profiles"]);

        let profile = &json["profiles"][0];
        let mut profile_keys: Vec<_> = profile.as_object().unwrap().keys().cloned().collect();
        profile_keys.sort();
        assert_eq!(
            profile_keys,
            vec![
                "app_images",
                "config_directory",
                "config_file",
                "cores",
                "diagnostics",
                "evidence",
                "paths",
                "playlists",
                "profile_kind",
                "scope",
            ]
        );

        let path_finding = &profile["paths"][0];
        let mut path_keys: Vec<_> = path_finding.as_object().unwrap().keys().cloned().collect();
        path_keys.sort();
        assert_eq!(
            path_keys,
            vec![
                "config_key",
                "configured_value",
                "probe",
                "purpose",
                "resolution",
                "resolved_path",
            ]
        );

        let core = &profile["cores"][0];
        let mut core_keys: Vec<_> = core.as_object().unwrap().keys().cloned().collect();
        core_keys.sort();
        assert_eq!(
            core_keys,
            vec!["core_stem", "file_name", "full_path", "info"]
        );

        let inventory = &profile["playlists"];
        let mut inventory_keys: Vec<_> = inventory.as_object().unwrap().keys().cloned().collect();
        inventory_keys.sort();
        assert_eq!(
            inventory_keys,
            vec!["complete", "diagnostics", "directory", "playlists"]
        );

        let playlist = &inventory["playlists"][0];
        let mut playlist_keys: Vec<_> = playlist.as_object().unwrap().keys().cloned().collect();
        playlist_keys.sort();
        assert_eq!(
            playlist_keys,
            vec![
                "complete",
                "default_core_name",
                "default_core_path",
                "diagnostics",
                "entries",
                "file_path",
                "playlist_name",
                "version",
            ]
        );

        let playlist_entry = &playlist["entries"][0];
        let mut entry_keys: Vec<_> = playlist_entry
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        entry_keys.sort();
        assert_eq!(
            entry_keys,
            vec![
                "content_path",
                "core_name",
                "core_path",
                "crc",
                "database_name",
                "entry_index",
                "label",
                "subsystem_ident",
                "subsystem_name",
            ]
        );

        let content_path = &playlist_entry["content_path"];
        let mut content_path_keys: Vec<_> =
            content_path.as_object().unwrap().keys().cloned().collect();
        content_path_keys.sort();
        assert_eq!(
            content_path_keys,
            vec!["archive_member_path", "archive_path", "kind", "raw"]
        );

        let app_image = &profile["app_images"][0];
        let mut app_image_keys: Vec<_> = app_image.as_object().unwrap().keys().cloned().collect();
        app_image_keys.sort();
        assert_eq!(
            app_image_keys,
            vec![
                "confidence",
                "config_association",
                "desktop_evidence",
                "executable",
                "filename_evidence",
                "path",
                "probe",
            ]
        );
        assert_eq!(app_image["confidence"], "weak");
        assert_eq!(
            app_image["config_association"]["type"],
            "shares_native_profile"
        );

        assert_eq!(json["format_version"], 2);
        assert_eq!(profile["profile_kind"], "native");
        assert_eq!(profile["scope"], "user");
        assert_eq!(playlist_entry["content_path"]["kind"], "filesystem");
        assert_eq!(playlist_entry["crc"]["type"], "placeholder");
        assert_eq!(playlist_entry["core_path"], "DETECT");
    }

    #[test]
    fn config_file_finding_uses_internally_tagged_read_outcome() {
        let fixture = Fixture::new("tagged-enum");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            "system_directory = \"/ok\"\n",
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        let read = &json["profiles"][0]["config_file"]["read"];
        assert_eq!(read["type"], "parsed");
        assert!(read["malformed_lines"].is_array());
        assert_eq!(read["include_detected"], false);
        assert_eq!(read["complete"], true);
    }

    #[test]
    fn non_utf8_core_filename_serializes_lossily_without_failing() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new("non-utf8-core");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\n",
                fixture.path("cores").display()
            ),
        );
        fixture.mkdir("cores");
        let mut raw_name = b"bad-\x80".to_vec();
        raw_name.extend_from_slice(CORE_SUFFIX.as_bytes());
        fs::write(
            fixture.path("cores").join(OsString::from_vec(raw_name)),
            "stub",
        )
        .unwrap();

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert_eq!(report.profiles[0].cores.len(), 1);
        assert!(report.profiles[0].cores[0].file_name.lossy);
        // Must not panic or error when serialized.
        let _ = serde_json::to_string(&report).unwrap();
    }

    #[test]
    fn deterministic_ordering_is_independent_of_filesystem_enumeration_order() {
        let fixture = Fixture::new("deterministic-order");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "libretro_directory = \"{}\"\n",
                fixture.path("cores").display()
            ),
        );
        fixture.mkdir("cores");
        // Create in reverse-alphabetical order to prove sorting, not
        // creation/readdir order, determines the final sequence.
        for name in ["zeta_libretro.so", "mid_libretro.so", "alpha_libretro.so"] {
            fixture.write(&format!("cores/{name}"), "stub");
        }

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let stems: Vec<_> = report.profiles[0]
            .cores
            .iter()
            .map(|core| core.core_stem.as_str())
            .collect();
        assert_eq!(stems, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn report_is_deterministic_across_repeated_calls() {
        let fixture = Fixture::new("deterministic-repeat");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.native_config_body(),
        );
        let filesystem = HostReadOnlyFilesystem;
        let first = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let second = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
    }

    #[test]
    fn discovery_makes_no_filesystem_writes() {
        let fixture = Fixture::new("no-writes");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.native_config_body(),
        );

        fn tree_entries(root: &Path) -> Vec<PathBuf> {
            fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
                let mut children: Vec<_> = fs::read_dir(current)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                children.sort();
                for child in children {
                    entries.push(child.strip_prefix(root).unwrap().to_path_buf());
                    if child.is_dir() {
                        visit(root, &child, entries);
                    }
                }
            }
            let mut entries = Vec::new();
            visit(root, root, &mut entries);
            entries
        }

        let before = tree_entries(&fixture.root);
        let filesystem = HostReadOnlyFilesystem;
        let _ = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let after = tree_entries(&fixture.root);
        assert_eq!(before, after);
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    // ---- Playlist discovery and parsing ----

    impl Fixture {
        fn playlists_config_body(&self) -> String {
            format!(
                "playlist_directory = \"{}\"\n",
                self.path("opt/retroarch/playlists").display()
            )
        }

        fn discover_playlists_only(&self) -> RetroArchPlaylistInventory {
            self.mkdir("opt/retroarch/playlists");
            let filesystem = HostReadOnlyFilesystem;
            let report = discover_retroarch_environment(&filesystem, &self.env()).unwrap();
            report.profiles[0].playlists.clone()
        }
    }

    #[test]
    fn playlist_directory_unconfigured_yields_no_directory_and_no_playlists() {
        let fixture = Fixture::new("playlists-unconfigured");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.native_config_body(),
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let inventory = &report.profiles[0].playlists;
        assert!(inventory.directory.is_none());
        assert!(inventory.playlists.is_empty());
        assert!(inventory.complete);
    }

    /// A missing configured `Playlists` directory never even reaches
    /// `discover_playlists`: `build_path_findings` only ever inserts a
    /// purpose into `resolved_dirs` when its own probe already found
    /// `FsProbe::PresentDirectory`, and it already emits
    /// `configured_directory_missing` for exactly this case (the same
    /// pre-existing mechanism `Cores`/`CoreInfo`/every other purpose
    /// already relies on - this is not new behavior, just the same
    /// invariant applied to `Playlists`). `discover_playlists` therefore
    /// correctly reports `directory: None` here, matching
    /// `discover_cores`'s own precedent, and does *not* duplicate a
    /// second, playlist-specific "missing" diagnostic for the same fact.
    #[test]
    fn playlist_directory_missing_is_diagnosed_upstream_not_duplicated() {
        let fixture = Fixture::new("playlists-missing-dir");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &format!(
                "playlist_directory = \"{}\"\n",
                fixture.path("does-not-exist").display()
            ),
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let profile = &report.profiles[0];
        assert!(profile.playlists.directory.is_none());
        assert!(profile.playlists.playlists.is_empty());
        assert!(profile.playlists.complete);
        assert!(
            profile
                .diagnostics
                .iter()
                .any(|d| d.code == "configured_directory_missing"
                    && d.purpose == Some(PathPurpose::Playlists))
        );
    }

    /// Same reasoning as the missing-directory case above: a symlinked
    /// `Playlists` directory never enters `resolved_dirs` either
    /// (`FsProbe::Symlink != PresentDirectory`), so it is already reported
    /// upstream by `build_path_findings` as `configured_directory_symlink`.
    #[test]
    fn playlist_directory_final_component_symlink_is_diagnosed_upstream_not_followed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("playlists-dir-symlink");
        fixture.mkdir("real-playlists");
        fixture.mkdir("opt/retroarch");
        symlink(
            fixture.path("real-playlists"),
            fixture.path("opt/retroarch/playlists"),
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let profile = &report.profiles[0];
        assert!(profile.playlists.directory.is_none());
        assert!(profile.playlists.playlists.is_empty());
        assert!(profile.playlists.complete);
        assert!(
            profile
                .diagnostics
                .iter()
                .any(|d| d.code == "configured_directory_symlink"
                    && d.purpose == Some(PathPurpose::Playlists))
        );
    }

    #[test]
    fn playlist_file_final_component_symlink_is_skipped_and_reported() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("playlist-file-symlink");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write("real.lpl", r#"{"version":"1.5","items":[]}"#);
        symlink(
            fixture.path("real.lpl"),
            fixture.path("opt/retroarch/playlists/link.lpl"),
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let inventory = &report.profiles[0].playlists;
        assert!(inventory.playlists.is_empty());
        assert!(
            inventory
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_file_symlink_skipped")
        );
    }

    #[test]
    fn valid_modern_playlist_parses_every_recognized_field() {
        let fixture = Fixture::new("playlist-valid");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Test.lpl",
            r#"{
                "version": "1.5",
                "default_core_path": "/cores/snes9x_libretro.so",
                "default_core_name": "Snes9x",
                "items": [
                    {
                        "path": "/roms/Chrono Trigger (USA).sfc",
                        "label": "Chrono Trigger (USA)",
                        "core_path": "/cores/snes9x_libretro.so",
                        "core_name": "Snes9x",
                        "crc32": "A1B2C3D4|crc",
                        "db_name": "Nintendo - Super Nintendo Entertainment System.lpl",
                        "subsystem_ident": "ident",
                        "subsystem_name": "name"
                    }
                ]
            }"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
        let inventory = &report.profiles[0].playlists;
        assert_eq!(inventory.playlists.len(), 1);
        let playlist = &inventory.playlists[0];
        assert_eq!(playlist.playlist_name, "Test");
        assert_eq!(playlist.version.as_deref(), Some("1.5"));
        assert_eq!(
            playlist.default_core_path.as_deref(),
            Some("/cores/snes9x_libretro.so")
        );
        assert_eq!(playlist.default_core_name.as_deref(), Some("Snes9x"));
        assert!(playlist.complete);
        assert_eq!(playlist.entries.len(), 1);
        let entry = &playlist.entries[0];
        assert_eq!(entry.entry_index, 0);
        assert_eq!(
            entry.content_path.raw.as_deref(),
            Some("/roms/Chrono Trigger (USA).sfc")
        );
        assert_eq!(entry.content_path.kind, ContentPathKind::Filesystem);
        assert_eq!(entry.label.as_deref(), Some("Chrono Trigger (USA)"));
        assert_eq!(
            entry.core_path.as_deref(),
            Some("/cores/snes9x_libretro.so")
        );
        assert_eq!(entry.core_name.as_deref(), Some("Snes9x"));
        assert_eq!(
            entry.crc,
            PlaylistCrc::Verified {
                value: "A1B2C3D4".to_string()
            }
        );
        assert_eq!(
            entry.database_name.as_deref(),
            Some("Nintendo - Super Nintendo Entertainment System.lpl")
        );
        assert_eq!(entry.subsystem_ident.as_deref(), Some("ident"));
        assert_eq!(entry.subsystem_name.as_deref(), Some("name"));
    }

    #[test]
    fn empty_playlist_parses_to_zero_entries() {
        let fixture = Fixture::new("playlist-empty");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Empty.lpl",
            r#"{"version":"1.5","items":[]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(inventory.playlists.len(), 1);
        assert!(inventory.playlists[0].entries.is_empty());
        assert!(inventory.playlists[0].complete);
    }

    #[test]
    fn detect_core_is_preserved_verbatim_not_treated_as_missing() {
        let fixture = Fixture::new("playlist-detect");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Detect.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/game.zip","core_path":"DETECT","core_name":"DETECT"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let entry = &inventory.playlists[0].entries[0];
        assert_eq!(entry.core_path.as_deref(), Some("DETECT"));
        assert_eq!(entry.core_name.as_deref(), Some("DETECT"));
    }

    #[test]
    fn path_with_spaces_is_preserved_exactly() {
        let fixture = Fixture::new("playlist-spaces");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Spaces.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/My Cool Game (USA) [!].zip"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(
            inventory.playlists[0].entries[0]
                .content_path
                .raw
                .as_deref(),
            Some("/roms/My Cool Game (USA) [!].zip")
        );
    }

    #[test]
    fn archive_member_path_is_split_only_after_a_recognized_container_extension() {
        let fixture = Fixture::new("playlist-archive-member");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Archive.lpl",
            r#"{"version":"1.5","items":[
                {"path":"/roms/game.zip#game.sfc"},
                {"path":"/roms/game.rar#game.sfc"},
                {"path":"/roms/weird#name.zip"}
            ]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let entries = &inventory.playlists[0].entries;

        assert_eq!(entries[0].content_path.kind, ContentPathKind::ArchiveMember);
        assert_eq!(
            entries[0].content_path.archive_path.as_deref(),
            Some("/roms/game.zip")
        );
        assert_eq!(
            entries[0].content_path.archive_member_path.as_deref(),
            Some("game.sfc")
        );

        // `.rar` is not a recognized RetroArch archive container extension
        // (verified: `path_is_compressed_file` does not recognize it), so
        // the `#` here is just a literal character.
        assert_eq!(entries[1].content_path.kind, ContentPathKind::Filesystem);
        assert_eq!(
            entries[1].content_path.raw.as_deref(),
            Some("/roms/game.rar#game.sfc")
        );

        // A `#` not immediately after a recognized extension is also just
        // a literal character.
        assert_eq!(entries[2].content_path.kind, ContentPathKind::Filesystem);
    }

    #[test]
    fn relative_content_path_is_classified_and_never_resolved() {
        let fixture = Fixture::new("playlist-relative");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Relative.lpl",
            r#"{"version":"1.5","items":[{"path":"roms/game.zip"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(
            inventory.playlists[0].entries[0].content_path.kind,
            ContentPathKind::Relative
        );
    }

    #[test]
    fn missing_optional_fields_are_none_not_defaulted_to_empty_string() {
        let fixture = Fixture::new("playlist-missing-fields");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Minimal.lpl",
            r#"{"version":"1.5","items":[{}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let entry = &inventory.playlists[0].entries[0];
        assert_eq!(entry.content_path.kind, ContentPathKind::Missing);
        assert!(entry.content_path.raw.is_none());
        assert!(entry.label.is_none());
        assert!(entry.core_path.is_none());
        assert!(entry.core_name.is_none());
        assert!(entry.database_name.is_none());
        assert_eq!(entry.crc, PlaylistCrc::Missing);
    }

    #[test]
    fn unknown_extra_fields_are_ignored_not_rejected() {
        let fixture = Fixture::new("playlist-unknown-fields");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Extra.lpl",
            r#"{
                "version": "1.5",
                "some_future_field": {"nested": true},
                "items": [{"path": "/roms/game.zip", "entry_slot": 3, "future_entry_field": [1,2,3]}]
            }"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(inventory.playlists.len(), 1);
        assert!(inventory.playlists[0].complete);
        assert_eq!(inventory.playlists[0].entries.len(), 1);
    }

    #[test]
    fn malformed_json_is_reported_and_yields_an_incomplete_empty_playlist() {
        let fixture = Fixture::new("playlist-malformed-json");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write("opt/retroarch/playlists/Bad.lpl", "{ not json ][");
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(inventory.playlists.len(), 1);
        let playlist = &inventory.playlists[0];
        assert!(playlist.entries.is_empty());
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_malformed_json")
        );
    }

    #[test]
    fn invalid_utf8_playlist_is_reported() {
        let fixture = Fixture::new("playlist-invalid-utf8");
        fixture.mkdir("opt/retroarch/playlists");
        fs::write(
            fixture.path("opt/retroarch/playlists/Bad.lpl"),
            [b'{', 0xFF, 0xFE, b'}'],
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert!(playlist.entries.is_empty());
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_invalid_utf8")
        );
    }

    #[test]
    fn utf8_bom_is_stripped_before_parsing() {
        let fixture = Fixture::new("playlist-bom");
        fixture.mkdir("opt/retroarch/playlists");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"version":"1.5","items":[{"path":"/roms/game.zip"}]}"#);
        fs::write(fixture.path("opt/retroarch/playlists/Bom.lpl"), bytes).unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(inventory.playlists[0].entries.len(), 1);
        assert!(inventory.playlists[0].complete);
    }

    #[test]
    fn oversized_playlist_is_reported_as_too_large() {
        let fixture = Fixture::new("playlist-oversized");
        fixture.mkdir("opt/retroarch/playlists");
        let padding = "x".repeat(MAX_PLAYLIST_BYTES + 1);
        fs::write(
            fixture.path("opt/retroarch/playlists/Big.lpl"),
            format!(r#"{{"version":"1.5","padding":"{padding}","items":[]}}"#),
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert!(!playlist.complete);
        assert!(playlist.entries.is_empty());
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_too_large")
        );
    }

    #[test]
    fn entry_count_limit_truncates_and_marks_incomplete() {
        let fixture = Fixture::new("playlist-entry-limit");
        fixture.mkdir("opt/retroarch/playlists");
        let items = (0..(MAX_ENTRIES_PER_PLAYLIST + 5))
            .map(|index| format!(r#"{{"path":"/roms/game{index}.zip"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            fixture.path("opt/retroarch/playlists/Many.lpl"),
            format!(r#"{{"version":"1.5","items":[{items}]}}"#),
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert_eq!(playlist.entries.len(), MAX_ENTRIES_PER_PLAYLIST);
        assert!(!playlist.complete);
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_entry_count_limit_reached")
        );
    }

    /// Each playlist stays well under [`MAX_ENTRIES_PER_PLAYLIST`] on its
    /// own (so no per-playlist truncation happens), but there are enough
    /// playlists that the running total across the profile exceeds
    /// [`MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE`] partway through -
    /// proving the two limits are independent and that the total limit
    /// stops processing *further playlists*, not just further entries
    /// within one.
    #[test]
    fn total_entry_limit_stops_processing_further_playlists() {
        let fixture = Fixture::new("playlist-total-limit");
        fixture.mkdir("opt/retroarch/playlists");
        const ENTRIES_PER_PLAYLIST: usize = 1000;
        let items = (0..ENTRIES_PER_PLAYLIST)
            .map(|index| format!(r#"{{"path":"/roms/a{index}.zip"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(r#"{{"version":"1.5","items":[{items}]}}"#);
        // ceil(MAX_TOTAL / ENTRIES_PER_PLAYLIST) + a few extra playlists,
        // so the total is guaranteed to be exceeded partway through.
        let playlist_count =
            MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE.div_ceil(ENTRIES_PER_PLAYLIST) + 5;
        for index in 0..playlist_count {
            fixture.write(&format!("opt/retroarch/playlists/p{index:04}.lpl"), &body);
        }
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );

        let inventory = fixture.discover_playlists_only();

        assert!(!inventory.complete);
        assert!(
            inventory
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_total_entry_limit_reached")
        );
        // The limit is checked *before* starting each new playlist, not by
        // truncating the one that crosses it - so the running total may
        // exceed the cap by up to one playlist's worth of entries, but
        // never reaches a second playlist beyond that, and never fails to
        // stop at all.
        assert!(inventory.playlists.len() < playlist_count);
        let total_entries: usize = inventory
            .playlists
            .iter()
            .map(|playlist| playlist.entries.len())
            .sum();
        assert!(total_entries < MAX_TOTAL_PLAYLIST_ENTRIES_PER_PROFILE + ENTRIES_PER_PLAYLIST);
        assert!(
            inventory
                .playlists
                .iter()
                .all(|playlist| playlist.entries.len() == ENTRIES_PER_PLAYLIST && playlist.complete)
        );
    }

    #[test]
    fn duplicate_entries_are_reported_but_both_are_kept() {
        let fixture = Fixture::new("playlist-duplicates");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Dupes.lpl",
            r#"{"version":"1.5","items":[
                {"path":"/roms/game.zip"},
                {"path":"/roms/game.zip"}
            ]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert_eq!(playlist.entries.len(), 2);
        let duplicate_diagnostics: Vec<_> = playlist
            .diagnostics
            .iter()
            .filter(|d| d.code == "duplicate_playlist_entry")
            .collect();
        assert_eq!(duplicate_diagnostics.len(), 1);
        assert_eq!(duplicate_diagnostics[0].entry_index, Some(1));
    }

    #[test]
    fn malformed_crc_is_reported_and_not_normalized() {
        let fixture = Fixture::new("playlist-malformed-crc");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Crc.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/game.zip","crc32":"not-a-crc"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert_eq!(
            playlist.entries[0].crc,
            PlaylistCrc::Malformed {
                raw: "not-a-crc".to_string()
            }
        );
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_malformed_crc" && d.entry_index == Some(0))
        );
    }

    #[test]
    fn valid_crc_is_canonicalized_to_uppercase() {
        let fixture = Fixture::new("playlist-valid-crc");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Crc.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/game.zip","crc32":"a1b2c3d4|crc"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(
            inventory.playlists[0].entries[0].crc,
            PlaylistCrc::Verified {
                value: "A1B2C3D4".to_string()
            }
        );
    }

    #[test]
    fn placeholder_crc_is_distinguished_from_missing_and_verified() {
        let fixture = Fixture::new("playlist-placeholder-crc");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Crc.lpl",
            r#"{"version":"1.5","items":[
                {"path":"/roms/a.zip","crc32":"00000000|crc"},
                {"path":"/roms/b.zip"}
            ]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let entries = &inventory.playlists[0].entries;
        assert_eq!(entries[0].crc, PlaylistCrc::Placeholder);
        assert_eq!(entries[1].crc, PlaylistCrc::Missing);
    }

    #[test]
    fn missing_database_name_is_none_not_empty_string() {
        let fixture = Fixture::new("playlist-missing-db-name");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/NoDb.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/game.zip","db_name":""}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert!(inventory.playlists[0].entries[0].database_name.is_none());
    }

    #[test]
    fn unsupported_playlist_version_is_an_informational_diagnostic_not_a_rejection() {
        let fixture = Fixture::new("playlist-unsupported-version");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Future.lpl",
            r#"{"version":"9.9","items":[{"path":"/roms/game.zip"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let playlist = &inventory.playlists[0];
        assert_eq!(playlist.entries.len(), 1);
        assert!(playlist.complete);
        assert!(
            playlist
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_unsupported_version"
                    && d.severity == DiagnosticSeverity::Info)
        );
    }

    #[test]
    fn top_level_default_core_is_parsed() {
        let fixture = Fixture::new("playlist-default-core");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Default.lpl",
            r#"{"version":"1.5","default_core_path":"/cores/x_libretro.so","default_core_name":"X","items":[]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(
            inventory.playlists[0].default_core_path.as_deref(),
            Some("/cores/x_libretro.so")
        );
        assert_eq!(
            inventory.playlists[0].default_core_name.as_deref(),
            Some("X")
        );
    }

    #[test]
    fn too_many_playlist_files_are_reported_as_too_large_a_listing() {
        let fixture = Fixture::new("playlists-too-many-files");
        fixture.mkdir("opt/retroarch/playlists");
        for index in 0..(MAX_PLAYLISTS_PER_PROFILE + 5) {
            fixture.write(
                &format!("opt/retroarch/playlists/p{index}.lpl"),
                r#"{"version":"1.5","items":[]}"#,
            );
        }
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert!(inventory.playlists.is_empty());
        assert!(!inventory.complete);
        assert!(
            inventory
                .diagnostics
                .iter()
                .any(|d| d.code == "playlist_directory_listing_too_large")
        );
    }

    #[test]
    fn playlists_are_sorted_deterministically_regardless_of_creation_order() {
        let fixture = Fixture::new("playlists-sorted");
        fixture.mkdir("opt/retroarch/playlists");
        for name in ["zeta.lpl", "mid.lpl", "alpha.lpl"] {
            fixture.write(
                &format!("opt/retroarch/playlists/{name}"),
                r#"{"version":"1.5","items":[]}"#,
            );
        }
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        let names: Vec<_> = inventory
            .playlists
            .iter()
            .map(|playlist| playlist.playlist_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn non_utf8_playlist_filename_is_still_discovered_and_serializes_lossily() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new("playlist-non-utf8-name");
        fixture.mkdir("opt/retroarch/playlists");
        let raw_name = std::ffi::OsString::from_vec(b"bad-\xFF-name.lpl".to_vec());
        fs::write(
            fixture.path("opt/retroarch/playlists").join(&raw_name),
            r#"{"version":"1.5","items":[]}"#,
        )
        .unwrap();
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );
        let inventory = fixture.discover_playlists_only();
        assert_eq!(inventory.playlists.len(), 1);
        assert!(inventory.playlists[0].file_path.lossy);
        let json = serde_json::to_string(&inventory).unwrap();
        assert!(json.contains("\"lossy\":true"));
    }

    #[test]
    fn playlist_discovery_makes_no_filesystem_writes() {
        let fixture = Fixture::new("playlist-no-writes");
        fixture.mkdir("opt/retroarch/playlists");
        fixture.write(
            "opt/retroarch/playlists/Test.lpl",
            r#"{"version":"1.5","items":[{"path":"/roms/game.zip"}]}"#,
        );
        fixture.write(
            ".config/retroarch/retroarch.cfg",
            &fixture.playlists_config_body(),
        );

        fn tree_entries(root: &Path) -> Vec<PathBuf> {
            fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
                let mut children: Vec<_> = fs::read_dir(current)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                children.sort();
                for child in children {
                    entries.push(child.strip_prefix(root).unwrap().to_path_buf());
                    if child.is_dir() {
                        visit(root, &child, entries);
                    }
                }
            }
            let mut entries = Vec::new();
            visit(root, root, &mut entries);
            entries
        }

        let before = tree_entries(&fixture.root);
        let _ = fixture.discover_playlists_only();
        let after = tree_entries(&fixture.root);
        assert_eq!(before, after);
    }

    // ---- AppImage discovery ----

    impl Fixture {
        fn discover_native_app_images(
            &self,
            env_mutator: impl FnOnce(&mut DiscoveryEnvironment),
        ) -> Vec<AppImageCandidate> {
            let filesystem = HostReadOnlyFilesystem;
            let mut env = self.env();
            env_mutator(&mut env);
            let report = discover_retroarch_environment(&filesystem, &env).unwrap();
            report.profiles[0].app_images.clone()
        }
    }

    #[test]
    fn appimage_in_applications_directory_is_discovered() {
        let fixture = Fixture::new("appimage-applications-dir");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].path.display.ends_with("RetroArch.AppImage"));
    }

    #[test]
    fn lowercase_appimage_suffix_is_recognized() {
        let fixture = Fixture::new("appimage-lowercase-suffix");
        fixture.mkdir("Applications");
        fixture.write("Applications/retroarch.appimage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn unrelated_appimage_is_ignored() {
        let fixture = Fixture::new("appimage-unrelated");
        fixture.mkdir("Applications");
        fixture.write("Applications/Blender.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert!(candidates.is_empty());
    }

    #[test]
    fn filename_only_retroarch_candidate_is_weak() {
        let fixture = Fixture::new("appimage-filename-weak");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch-Linux-x86_64.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].confidence,
            AppImageIdentificationConfidence::Weak
        );
        assert!(candidates[0].filename_evidence);
        assert!(candidates[0].desktop_evidence.is_empty());
    }

    #[test]
    fn executable_appimage_candidate_reports_executable_state() {
        let fixture = Fixture::new("appimage-executable");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        make_executable(&fixture.path("Applications/RetroArch.AppImage"));
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(candidates[0].executable, Some(ExecutableState::Executable));
    }

    #[test]
    fn non_executable_appimage_candidate_reports_not_executable() {
        let fixture = Fixture::new("appimage-non-executable");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(
            candidates[0].executable,
            Some(ExecutableState::NotExecutable)
        );
    }

    #[test]
    fn symlink_appimage_candidate_is_rejected_and_reported() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("appimage-symlink");
        fixture.mkdir("Applications");
        fixture.write("real-retroarch.AppImage", "stub");
        symlink(
            fixture.path("real-retroarch.AppImage"),
            fixture.path("Applications/RetroArch.AppImage"),
        )
        .unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].app_images.is_empty());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "appimage_candidate_symlink")
        );
    }

    #[test]
    fn non_utf8_appimage_filename_is_discovered_and_serializes_lossily() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new("appimage-non-utf8-name");
        fixture.mkdir("Applications");
        let raw_name = std::ffi::OsString::from_vec(b"retroarch-\xFF-bad.AppImage".to_vec());
        fs::write(fixture.path("Applications").join(&raw_name), "stub").unwrap();
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].path.lossy);
        let json = serde_json::to_string(&candidates[0]).unwrap();
        assert!(json.contains("\"lossy\":true"));
    }

    #[test]
    fn appimage_candidates_are_sorted_deterministically() {
        let fixture = Fixture::new("appimage-sorted");
        fixture.mkdir("Applications");
        for name in [
            "zeta-retroarch.AppImage",
            "mid-retroarch.AppImage",
            "alpha-retroarch.AppImage",
        ] {
            fixture.write(&format!("Applications/{name}"), "stub");
        }
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        let names: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.path.display.clone())
            .collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names);
        assert_eq!(candidates.len(), 3);
    }

    #[test]
    fn appimage_directory_listing_limit_is_enforced() {
        let fixture = Fixture::new("appimage-listing-limit");
        fixture.mkdir("Applications");
        for index in 0..(MAX_APPIMAGE_SEARCH_ROOT_ENTRIES + 5) {
            fixture.write(&format!("Applications/file{index}.txt"), "x");
        }
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].app_images.is_empty());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "appimage_directory_listing_too_large")
        );
    }

    #[test]
    fn missing_appimage_search_root_is_diagnosed_but_not_an_error() {
        let fixture = Fixture::new("appimage-missing-root");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("does-not-exist")];
        });
        assert!(candidates.is_empty());
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("does-not-exist")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "appimage_search_root_missing")
        );
    }

    #[test]
    fn no_recursive_scanning_of_appimage_search_roots() {
        let fixture = Fixture::new("appimage-no-recursion");
        fixture.mkdir("Applications/nested");
        fixture.write("Applications/nested/RetroArch.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert!(candidates.is_empty());
    }

    // ---- Desktop file parsing ----

    #[test]
    fn valid_retroarch_desktop_entry_is_matched_with_exact_confidence() {
        let fixture = Fixture::new("desktop-valid");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={} %f\nIcon=retroarch\n",
                appimage_path.display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].confidence,
            AppImageIdentificationConfidence::Exact
        );
        assert_eq!(candidates[0].desktop_evidence.len(), 1);
        assert!(candidates[0].desktop_evidence[0].name_evidence);
        assert_eq!(
            candidates[0].desktop_evidence[0].exec_resolution,
            ExecResolution::MatchesCandidate
        );
    }

    #[test]
    fn quoted_exec_path_is_tokenized_correctly() {
        let fixture = Fixture::new("desktop-quoted-exec");
        fixture.mkdir("Applications with spaces");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications with spaces/RetroArch.AppImage");
        fixture.write("Applications with spaces/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec=\"{}\" %f\n",
                appimage_path.display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications with spaces")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].desktop_evidence[0].exec_resolution,
            ExecResolution::MatchesCandidate
        );
    }

    #[test]
    fn exec_with_field_codes_still_resolves_to_the_executable_token() {
        let fixture = Fixture::new("desktop-field-codes");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={} %U\n",
                appimage_path.display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(
            candidates[0].desktop_evidence[0].exec_resolution,
            ExecResolution::MatchesCandidate
        );
    }

    #[test]
    fn hidden_desktop_entry_is_inactive_and_not_used_as_evidence() {
        let fixture = Fixture::new("desktop-hidden");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/game.AppImage");
        fixture.write("Applications/game.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nHidden=true\nExec={}\n",
                appimage_path.display()
            ),
        );
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(report.profiles[0].app_images.is_empty());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "desktop_entry_inactive")
        );
    }

    #[test]
    fn malformed_desktop_file_line_is_reported_and_parsing_continues() {
        let fixture = Fixture::new("desktop-malformed");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nnot a valid line\nName=RetroArch\nExec={}\n",
                appimage_path.display()
            ),
        );
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert_eq!(report.profiles[0].app_images.len(), 1);
        assert_eq!(
            report.profiles[0].app_images[0].confidence,
            AppImageIdentificationConfidence::Exact
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "desktop_file_malformed_line")
        );
    }

    #[test]
    fn invalid_utf8_desktop_file_is_reported_and_skipped() {
        let fixture = Fixture::new("desktop-invalid-utf8");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fs::write(
            fixture.path("desktop/retroarch.desktop"),
            [b'[', 0xFF, 0xFE, b']'],
        )
        .unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "desktop_file_invalid_utf8")
        );
    }

    #[test]
    fn oversized_desktop_file_is_reported() {
        let fixture = Fixture::new("desktop-oversized");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let padding = "#".repeat(MAX_DESKTOP_FILE_BYTES + 1);
        fixture.write("desktop/retroarch.desktop", &padding);
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "desktop_file_too_large")
        );
    }

    #[test]
    fn shell_wrapper_exec_is_unresolved_not_guessed() {
        let fixture = Fixture::new("desktop-shell-wrapper");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            "[Desktop Entry]\nType=Application\nName=RetroArch\nExec=sh -c \"/some/script.sh\"\n",
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        // The desktop entry's own Name identifies RetroArch, so it is
        // still evidence (Strong) even though its shell-wrapped Exec
        // cannot be resolved to confirm this exact candidate.
        assert_eq!(
            candidates[0].confidence,
            AppImageIdentificationConfidence::Strong
        );
        assert_eq!(candidates[0].desktop_evidence.len(), 1);
        assert_eq!(
            candidates[0].desktop_evidence[0].exec_resolution,
            ExecResolution::ShellWrapperUnresolved
        );
    }

    #[test]
    fn env_wrapper_exec_is_handled_conservatively() {
        let fixture = Fixture::new("desktop-env-wrapper");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec=env SDL_VIDEODRIVER=wayland {}\n",
                appimage_path.display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(
            candidates[0].desktop_evidence[0].exec_resolution,
            ExecResolution::MatchesCandidate
        );
    }

    #[test]
    fn desktop_exec_target_missing_is_reported() {
        let fixture = Fixture::new("desktop-exec-missing");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={}\n",
                fixture.path("Applications/DoesNotExist.AppImage").display()
            ),
        );
        // No candidate at all is produced for the (missing) exec target,
        // since it never resolves to the one real candidate that does
        // exist; the real candidate is still Weak from its own filename.
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].desktop_evidence.is_empty());
        assert_eq!(
            candidates[0].confidence,
            AppImageIdentificationConfidence::Weak
        );
    }

    #[test]
    fn desktop_exec_mismatch_does_not_attach_to_the_wrong_candidate() {
        let fixture = Fixture::new("desktop-exec-mismatch");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write("Applications/OtherRetroArchTool.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={}\n",
                fixture.path("Applications/RetroArch.AppImage").display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(candidates.len(), 2);
        let exact = candidates
            .iter()
            .find(|candidate| candidate.path.display.ends_with("RetroArch.AppImage"))
            .unwrap();
        let weak = candidates
            .iter()
            .find(|candidate| {
                candidate
                    .path
                    .display
                    .ends_with("OtherRetroArchTool.AppImage")
            })
            .unwrap();
        assert_eq!(exact.confidence, AppImageIdentificationConfidence::Exact);
        assert_eq!(weak.confidence, AppImageIdentificationConfidence::Weak);
        assert!(weak.desktop_evidence.is_empty());
    }

    #[test]
    fn unrelated_appimages_own_self_pointing_desktop_entry_is_never_evidence() {
        // Regression test for a real-world false positive found during the
        // Phase 18 smoke test: an ordinary, unrelated AppImage (e.g. a
        // video editor) ships its own desktop entry whose Exec naturally
        // points at itself. That coincidental Exec match must never, by
        // itself, be treated as evidence that the AppImage is RetroArch -
        // only a desktop entry that itself mentions RetroArch counts.
        let fixture = Fixture::new("desktop-unrelated-self-pointing");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/LosslessCut.AppImage");
        fixture.write("Applications/LosslessCut.AppImage", "stub");
        fixture.write(
            "desktop/losslesscut.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=LosslessCut\nComment=Lossless video/audio editor\nExec={} --no-sandbox %f\n",
                appimage_path.display()
            ),
        );
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert!(
            report.profiles[0].app_images.is_empty(),
            "an unrelated AppImage with only its own non-RetroArch desktop entry must not be reported as a candidate at all"
        );
    }

    // ---- Configuration association ----

    #[test]
    fn appimage_with_no_evidence_of_a_distinct_config_shares_the_native_profile() {
        let fixture = Fixture::new("config-shares-native");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
        });
        assert_eq!(
            candidates[0].config_association,
            ConfigAssociation::SharesNativeProfile
        );
    }

    #[test]
    fn portable_config_sibling_directory_is_detected_as_a_distinct_profile() {
        let fixture = Fixture::new("config-portable");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.mkdir("Applications/RetroArch.AppImage.config");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();

        // A genuinely distinct config directory was found, so this
        // becomes its own logical profile rather than staying attached to
        // native.
        assert_eq!(report.profiles.len(), 4);
        let appimage_profile = report
            .profiles
            .iter()
            .find(|profile| profile.profile_kind == ProfileKind::AppImage)
            .unwrap();
        assert_eq!(appimage_profile.app_images.len(), 1);
        assert!(matches!(
            appimage_profile.app_images[0].config_association,
            ConfigAssociation::PortableConfigDetected { .. }
        ));
        assert!(report.profiles[0].app_images.is_empty());
    }

    #[test]
    fn portable_home_sibling_directory_alone_also_yields_a_distinct_config() {
        let fixture = Fixture::new("config-portable-home-only");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.mkdir("Applications/RetroArch.AppImage.home");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert_eq!(report.profiles.len(), 4);
        let appimage_profile = report
            .profiles
            .iter()
            .find(|profile| profile.profile_kind == ProfileKind::AppImage)
            .unwrap();
        assert!(matches!(
            appimage_profile.app_images[0].config_association,
            ConfigAssociation::PortableConfigDetected { .. }
        ));
    }

    #[test]
    fn explicit_config_flag_creates_a_distinct_appimage_profile() {
        let fixture = Fixture::new("config-explicit");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.mkdir("custom-config-dir");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={} --config {}\n",
                appimage_path.display(),
                fixture.path("custom-config-dir/retroarch.cfg").display(),
            ),
        );
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        env.desktop_file_roots = vec![fixture.path("desktop")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();

        assert_eq!(report.profiles.len(), 4);
        let appimage_profile = report
            .profiles
            .iter()
            .find(|profile| profile.profile_kind == ProfileKind::AppImage)
            .unwrap();
        assert_eq!(
            appimage_profile.config_directory.path.display,
            fixture.path("custom-config-dir").display().to_string()
        );
        assert!(matches!(
            appimage_profile.app_images[0].config_association,
            ConfigAssociation::ExplicitConfig { .. }
        ));
    }

    #[test]
    fn unresolvable_explicit_config_value_is_unknown_not_a_guess() {
        let fixture = Fixture::new("config-unresolvable");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        let appimage_path = fixture.path("Applications/RetroArch.AppImage");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            &format!(
                "[Desktop Entry]\nType=Application\nName=RetroArch\nExec={} --config %f\n",
                appimage_path.display()
            ),
        );
        let candidates = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        assert_eq!(candidates[0].config_association, ConfigAssociation::Unknown);
    }

    #[test]
    fn two_appimages_sharing_native_config_produce_one_logical_profile() {
        let fixture = Fixture::new("config-two-share-native");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write("Applications/RetroArch2.AppImage", "stub");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert_eq!(report.profiles.len(), 3);
        assert_eq!(report.profiles[0].app_images.len(), 2);
    }

    #[test]
    fn two_appimages_with_the_same_distinct_config_produce_one_appimage_profile() {
        let fixture = Fixture::new("config-two-same-distinct");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write("Applications/RetroArch2.AppImage", "stub");
        fixture.mkdir("Applications/RetroArch.AppImage.config");
        fixture.mkdir("Applications/RetroArch2.AppImage.config");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        // Both resolve to *different* portable config directories (each
        // AppImage's own `.config` sibling), so this is the ambiguous
        // multi-distinct-directory case, not a single shared profile.
        assert_eq!(report.profiles.len(), 3);
        assert_eq!(report.profiles[0].app_images.len(), 2);
        assert!(
            report.profiles[0]
                .app_images
                .iter()
                .all(|candidate| candidate.config_association == ConfigAssociation::Ambiguous)
        );
    }

    #[test]
    fn ambiguous_distinct_configs_prevent_a_duplicate_profile_and_are_reported() {
        let fixture = Fixture::new("config-ambiguous-merge");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write("Applications/RetroArch2.AppImage", "stub");
        fixture.mkdir("Applications/RetroArch.AppImage.config");
        fixture.mkdir("Applications/RetroArch2.AppImage.config");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert_eq!(
            report.profiles.len(),
            3,
            "no duplicate AppImage profile should be created when evidence disagrees"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "duplicate_logical_profile_prevented")
        );
    }

    #[test]
    fn flatpak_profiles_never_receive_appimage_evidence() {
        let fixture = Fixture::new("config-flatpak-never-merges");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots = vec![fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        for profile in &report.profiles {
            if profile.profile_kind == ProfileKind::Flatpak {
                assert!(profile.app_images.is_empty());
            }
        }
    }

    #[test]
    fn deduplication_uses_byte_safe_paths_not_display_strings() {
        // Two candidates resolved from overlapping search roots (the same
        // real path reachable via two configured roots) must be
        // deduplicated to one entry, using the real path bytes - not
        // string equality on a lossy display form.
        let fixture = Fixture::new("dedup-byte-safe");
        fixture.mkdir("Applications");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        let filesystem = HostReadOnlyFilesystem;
        let mut env = fixture.env();
        env.app_image_search_roots =
            vec![fixture.path("Applications"), fixture.path("Applications")];
        let report = discover_retroarch_environment(&filesystem, &env).unwrap();
        assert_eq!(report.profiles[0].app_images.len(), 1);
    }

    #[test]
    fn no_appimage_or_desktop_file_is_ever_modified() {
        let fixture = Fixture::new("appimage-no-writes");
        fixture.mkdir("Applications");
        fixture.mkdir("desktop");
        fixture.write("Applications/RetroArch.AppImage", "stub");
        fixture.write(
            "desktop/retroarch.desktop",
            "[Desktop Entry]\nType=Application\nName=RetroArch\nExec=/nonexistent\n",
        );

        fn tree_entries(root: &Path) -> Vec<PathBuf> {
            fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
                let mut children: Vec<_> = fs::read_dir(current)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                children.sort();
                for child in children {
                    entries.push(child.strip_prefix(root).unwrap().to_path_buf());
                    if child.is_dir() {
                        visit(root, &child, entries);
                    }
                }
            }
            let mut entries = Vec::new();
            visit(root, root, &mut entries);
            entries
        }

        let before = tree_entries(&fixture.root);
        let _ = fixture.discover_native_app_images(|env| {
            env.app_image_search_roots = vec![fixture.path("Applications")];
            env.desktop_file_roots = vec![fixture.path("desktop")];
        });
        let after = tree_entries(&fixture.root);
        assert_eq!(before, after);
    }
}
