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

use serde::Serialize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Native,
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
}

fn finalize_diagnostics(mut raw: Vec<RawDiagnostic>) -> Vec<Diagnostic> {
    raw.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.profile.cmp(&b.profile))
            .then_with(|| a.purpose.cmp(&b.purpose))
            .then_with(|| a.path.cmp(&b.path))
    });
    raw.into_iter()
        .map(|diagnostic| Diagnostic {
            code: diagnostic.code,
            severity: diagnostic.severity,
            detail_kind: diagnostic.detail_kind,
            profile: diagnostic.profile,
            purpose: diagnostic.purpose,
            path: diagnostic.path.as_deref().map(EncodedPath::from_path),
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

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchProfile {
    pub profile_kind: ProfileKind,
    pub scope: ProfileScope,
    pub evidence: Evidence,
    pub config_directory: DirectoryProbeFinding,
    pub config_file: ConfigFileFinding,
    pub paths: Vec<PathFinding>,
    pub cores: Vec<CoreFinding>,
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
}

impl DiscoveryEnvironment {
    pub fn from_process_environment() -> Self {
        let home = std::env::var_os("HOME");
        let user_flatpak_root = home
            .as_ref()
            .map(|home| PathBuf::from(home).join(".local/share/flatpak"))
            .unwrap_or_else(|| PathBuf::from(".local/share/flatpak"));
        Self {
            home,
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            path: std::env::var_os("PATH"),
            user_flatpak_root,
            system_flatpak_root: PathBuf::from("/var/lib/flatpak"),
        }
    }
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

    let mut profiles = vec![
        discover_profile(
            filesystem,
            ProfileKind::Native,
            ProfileScope::User,
            &native_config_dir,
            &home_dir,
            environment.path.as_deref(),
            None,
        ),
        discover_profile(
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
        ),
        discover_profile(
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
        ),
    ];

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
        format_version: 1,
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

        assert_eq!(report.format_version, 1);
        assert_eq!(report.profiles.len(), 3);
        for profile in &report.profiles {
            assert!(profile.evidence.executables.is_empty());
            assert!(!profile.evidence.config_directory_found);
            assert!(!profile.evidence.config_file_found);
            assert!(profile.cores.is_empty());
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
            &fixture.native_config_body(),
        );
        fixture.mkdir("opt/retroarch/cores");
        fixture.write("opt/retroarch/cores/test_libretro.so", "stub");

        let filesystem = HostReadOnlyFilesystem;
        let report = discover_retroarch_environment(&filesystem, &fixture.env()).unwrap();
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
                "config_directory",
                "config_file",
                "cores",
                "diagnostics",
                "evidence",
                "paths",
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

        assert_eq!(json["format_version"], 1);
        assert_eq!(profile["profile_kind"], "native");
        assert_eq!(profile["scope"], "user");
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
}
