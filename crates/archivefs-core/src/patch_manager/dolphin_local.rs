//! Bounded, read-only discovery and inspection of local Dolphin profiles.
//!
//! The adapter never starts Dolphin and has no write or network capability.
//! It accepts documented native/Flatpak roots and exact roots supplied by a
//! trusted caller, rejects symlinked roots, and opens regular GameSettings INI
//! files with `O_NOFOLLOW` on Unix.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::emulator_environment::EncodedPath;

use super::destination_safety::{
    DestinationRootState, DestinationSafetyFailureReason, validate_destination_root,
};

pub const DOLPHIN_MAX_PROFILES: usize = 16;
pub const DOLPHIN_MAX_ENTRIES_VISITED: usize = 10_000;
pub const DOLPHIN_MAX_GAME_INI_FILES: usize = 2_048;
pub const DOLPHIN_MAX_GAME_INI_BYTES: u64 = 256 * 1024;
pub const DOLPHIN_MAX_TOTAL_GAME_INI_BYTES: u64 = 16 * 1024 * 1024;
pub const DOLPHIN_MAX_LINES_PER_FILE: usize = 8_192;
pub const DOLPHIN_MAX_LINE_BYTES: usize = 8 * 1024;

const FLATPAK_APP_ID: &str = "org.DolphinEmu.dolphin-emu";
const MAX_RETAINED_NAMES_PER_KIND: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinInstallationType {
    Native,
    FlatpakUser,
    FlatpakSystem,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinProfileScope {
    User,
    SystemInstallationUserProfile,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinProfileBlockerKind {
    PathNotAbsolute,
    FilesystemRoot,
    MissingConfiguration,
    UnsafePath,
    NotDirectory,
    Unreadable,
    MissingDolphinEvidence,
    ProfileLimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DolphinProfileBlocker {
    pub kind: DolphinProfileBlockerKind,
    pub path: EncodedPath,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinSettingsDirectoryState {
    Available,
    Missing,
    UnsafePath,
    NotDirectory,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DolphinDirectoryIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinProfile {
    pub profile_id: String,
    pub installation_type: DolphinInstallationType,
    pub scope: DolphinProfileScope,
    pub configuration_path: PathBuf,
    pub provenance: &'static str,
    pub eligible: bool,
    pub blockers: Vec<DolphinProfileBlocker>,
    pub game_settings_path: PathBuf,
    pub game_settings_state: DolphinSettingsDirectoryState,
    pub game_settings_warning: Option<String>,
    pub configuration_identity: Option<DolphinDirectoryIdentity>,
    pub game_settings_identity: Option<DolphinDirectoryIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinProfileDiscovery {
    pub profiles: Vec<DolphinProfile>,
    pub warnings: Vec<DolphinProfileBlocker>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinProfileDiscoveryRoots {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub xdg_data_home: PathBuf,
    pub flatpak_system_root: PathBuf,
    /// Exact, already-known Dolphin user directories; never search for these.
    pub explicit_configuration_roots: Vec<PathBuf>,
}

impl DolphinProfileDiscoveryRoots {
    pub fn from_environment() -> Result<Self, DolphinDiscoveryError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(DolphinDiscoveryError::HomeUnavailable)?;
        Ok(Self {
            xdg_config_home: env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
            xdg_data_home: env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share")),
            home,
            flatpak_system_root: PathBuf::from("/var/lib/flatpak"),
            explicit_configuration_roots: Vec::new(),
        })
    }
}

#[derive(Debug)]
pub enum DolphinDiscoveryError {
    HomeUnavailable,
}

impl std::fmt::Display for DolphinDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HomeUnavailable => f.write_str("HOME is not set"),
        }
    }
}

impl std::error::Error for DolphinDiscoveryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinCodeKind {
    FramePatch,
    ActionReplay,
    Gecko,
    Riivolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinInspectionWarningKind {
    UnsafePath,
    UnreadablePath,
    SymlinkSkipped,
    SpecialFileSkipped,
    EntryLimitReached,
    FileCountLimitReached,
    FileTooLarge,
    TotalBytesLimitReached,
    LineCountLimitReached,
    LineTooLong,
    MalformedIni,
    InvalidUtf8,
    InvalidGameId,
    DuplicateGameIdentity,
    DuplicateFilename,
    DuplicateContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinInspectionWarning {
    pub kind: DolphinInspectionWarningKind,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinGameIniFile {
    pub path: PathBuf,
    pub filename_stem: OsString,
    pub game_id_candidate: Option<String>,
    pub revision_candidate: Option<u16>,
    pub region_candidate: Option<String>,
    pub frame_patch_names: Vec<String>,
    pub action_replay_names: Vec<String>,
    pub gecko_names: Vec<String>,
    pub riivolution_names: Vec<String>,
    pub enabled_frame_patch_names: Vec<String>,
    pub enabled_action_replay_names: Vec<String>,
    pub enabled_gecko_names: Vec<String>,
    pub enabled_riivolution_names: Vec<String>,
    pub size_bytes: u64,
    pub sha256: String,
    pub duplicate_game_identity: bool,
    pub duplicate_filename: bool,
    pub duplicate_content: bool,
    pub warnings: Vec<DolphinInspectionWarningKind>,
}

impl DolphinGameIniFile {
    pub fn definition_count(&self) -> usize {
        self.frame_patch_names.len()
            + self.action_replay_names.len()
            + self.gecko_names.len()
            + self.riivolution_names.len()
    }

    pub fn enabled_count(&self) -> usize {
        self.enabled_frame_patch_names.len()
            + self.enabled_action_replay_names.len()
            + self.enabled_gecko_names.len()
            + self.enabled_riivolution_names.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinGameIniInventory {
    pub profile_id: String,
    pub files: Vec<DolphinGameIniFile>,
    pub warnings: Vec<DolphinInspectionWarning>,
    pub entries_visited: usize,
    pub bytes_inspected: u64,
    pub complete: bool,
}

#[derive(Debug)]
pub enum DolphinInspectionError {
    IneligibleProfile { profile_id: String },
    ProfileChanged { path: PathBuf },
    UnsafeProfile { path: PathBuf },
}

impl std::fmt::Display for DolphinInspectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IneligibleProfile { profile_id } => {
                write!(f, "Dolphin profile {profile_id} is not eligible")
            }
            Self::ProfileChanged { path } => {
                write!(
                    f,
                    "Dolphin profile changed before inspection: {}",
                    path.display()
                )
            }
            Self::UnsafeProfile { path } => {
                write!(f, "Dolphin profile path is unsafe: {}", path.display())
            }
        }
    }
}

impl std::error::Error for DolphinInspectionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinMatchState {
    ExactGameIdMatch,
    ExactGameIdAndRevisionMatch,
    MultipleIniFilesForGame,
    NoVerifiedGameIdAvailable,
    NoMatchingIniFound,
    InvalidVerifiedGameId,
    RevisionMismatch,
    IdentityExtractionDeferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DolphinMatchResult {
    pub state: DolphinMatchState,
    pub verified_game_id: Option<String>,
    pub verified_revision: Option<u16>,
    pub matching_files: Vec<PathBuf>,
    pub reason: String,
}

#[derive(Debug, Clone)]
struct ProfileCandidate {
    installation_type: DolphinInstallationType,
    scope: DolphinProfileScope,
    path: PathBuf,
    provenance: &'static str,
    report_missing: bool,
}

/// Discovers documented paths and exact caller-supplied roots only.
pub fn discover_dolphin_profiles(
    roots: &DolphinProfileDiscoveryRoots,
) -> Result<DolphinProfileDiscovery, DolphinDiscoveryError> {
    let flatpak_path = roots
        .home
        .join(".var/app")
        .join(FLATPAK_APP_ID)
        .join("config/dolphin-emu");
    let user_install = roots.xdg_data_home.join("flatpak/app").join(FLATPAK_APP_ID);
    let system_install = roots.flatpak_system_root.join("app").join(FLATPAK_APP_ID);
    let system_only = real_directory(&system_install) && !real_directory(&user_install);
    let (flatpak_kind, flatpak_scope) = if system_only {
        (
            DolphinInstallationType::FlatpakSystem,
            DolphinProfileScope::SystemInstallationUserProfile,
        )
    } else {
        (
            DolphinInstallationType::FlatpakUser,
            DolphinProfileScope::User,
        )
    };
    let mut candidates = vec![
        ProfileCandidate {
            installation_type: DolphinInstallationType::Native,
            scope: DolphinProfileScope::User,
            path: roots.xdg_config_home.join("dolphin-emu"),
            provenance: "XDG Dolphin user directory",
            report_missing: false,
        },
        ProfileCandidate {
            installation_type: flatpak_kind,
            scope: flatpak_scope,
            path: flatpak_path,
            provenance: "Flatpak org.DolphinEmu.dolphin-emu user directory",
            report_missing: false,
        },
    ];
    candidates.extend(
        roots
            .explicit_configuration_roots
            .iter()
            .cloned()
            .map(|path| ProfileCandidate {
                installation_type: DolphinInstallationType::Explicit,
                scope: DolphinProfileScope::Explicit,
                path,
                provenance: "Explicitly supplied Dolphin user directory",
                report_missing: true,
            }),
    );
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    candidates.dedup_by(|a, b| a.path == b.path);

    let mut profiles = Vec::new();
    let mut warnings = Vec::new();
    for candidate in candidates {
        if profiles.len() >= DOLPHIN_MAX_PROFILES {
            warnings.push(blocker(
                DolphinProfileBlockerKind::ProfileLimitReached,
                &candidate.path,
                format!("profile discovery stopped at the {DOLPHIN_MAX_PROFILES}-profile limit"),
            ));
            break;
        }
        if !candidate.path.is_absolute() {
            profiles.push(blocked(
                candidate,
                DolphinProfileBlockerKind::PathNotAbsolute,
                "configuration path is not absolute",
            ));
            continue;
        }
        if candidate.path.parent().is_none() {
            profiles.push(blocked(
                candidate,
                DolphinProfileBlockerKind::FilesystemRoot,
                "a filesystem root cannot be a Dolphin profile",
            ));
            continue;
        }
        let validated = match validate_destination_root(&candidate.path) {
            Ok(value) => value,
            Err(error) => {
                let kind = match error.reason {
                    DestinationSafetyFailureReason::RootNotDirectory
                    | DestinationSafetyFailureReason::NonDirectoryParent => {
                        DolphinProfileBlockerKind::NotDirectory
                    }
                    DestinationSafetyFailureReason::InspectionFailed => {
                        DolphinProfileBlockerKind::Unreadable
                    }
                    _ => DolphinProfileBlockerKind::UnsafePath,
                };
                profiles.push(blocked(
                    candidate,
                    kind,
                    format!("configuration path rejected: {:?}", error.reason),
                ));
                continue;
            }
        };
        if validated.state() == DestinationRootState::Absent {
            if candidate.report_missing {
                profiles.push(blocked(
                    candidate,
                    DolphinProfileBlockerKind::MissingConfiguration,
                    "configuration directory does not exist",
                ));
            }
            continue;
        }
        if let Err((kind, detail)) = inspect_marker(&candidate.path) {
            profiles.push(blocked(candidate, kind, detail));
            continue;
        }
        let settings_path = candidate.path.join("GameSettings");
        let (settings_state, settings_warning, settings_identity) =
            inspect_settings(&settings_path);
        let identity = fs::symlink_metadata(&candidate.path)
            .ok()
            .and_then(|m| directory_identity(&m));
        profiles.push(DolphinProfile {
            profile_id: profile_id(candidate.installation_type, &candidate.path),
            installation_type: candidate.installation_type,
            scope: candidate.scope,
            configuration_path: candidate.path,
            provenance: candidate.provenance,
            eligible: true,
            blockers: Vec::new(),
            game_settings_path: settings_path,
            game_settings_state: settings_state,
            game_settings_warning: settings_warning,
            configuration_identity: identity,
            game_settings_identity: settings_identity,
        });
    }
    profiles.sort_by(|a, b| {
        a.installation_type
            .cmp(&b.installation_type)
            .then_with(|| a.configuration_path.cmp(&b.configuration_path))
    });
    Ok(DolphinProfileDiscovery {
        complete: warnings.is_empty(),
        profiles,
        warnings,
    })
}

fn inspect_marker(root: &Path) -> Result<(), (DolphinProfileBlockerKind, &'static str)> {
    let marker = root.join("Dolphin.ini");
    match fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err((
            DolphinProfileBlockerKind::UnsafePath,
            "Dolphin.ini is a symlink",
        )),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err((
            DolphinProfileBlockerKind::MissingDolphinEvidence,
            "Dolphin.ini is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err((
            DolphinProfileBlockerKind::MissingDolphinEvidence,
            "Dolphin.ini was not found",
        )),
        Err(_) => Err((
            DolphinProfileBlockerKind::Unreadable,
            "Dolphin.ini is unreadable",
        )),
    }
}

fn inspect_settings(
    path: &Path,
) -> (
    DolphinSettingsDirectoryState,
    Option<String>,
    Option<DolphinDirectoryIdentity>,
) {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => (
            DolphinSettingsDirectoryState::UnsafePath,
            Some("GameSettings is a symlink and will not be followed".into()),
            None,
        ),
        Ok(m) if m.is_dir() => (
            DolphinSettingsDirectoryState::Available,
            None,
            directory_identity(&m),
        ),
        Ok(_) => (
            DolphinSettingsDirectoryState::NotDirectory,
            Some("GameSettings is not a directory".into()),
            None,
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            (DolphinSettingsDirectoryState::Missing, None, None)
        }
        Err(e) => (
            DolphinSettingsDirectoryState::Unreadable,
            Some(format!("GameSettings cannot be inspected: {e}")),
            None,
        ),
    }
}

fn blocked(
    candidate: ProfileCandidate,
    kind: DolphinProfileBlockerKind,
    detail: impl Into<String>,
) -> DolphinProfile {
    let settings = candidate.path.join("GameSettings");
    DolphinProfile {
        profile_id: profile_id(candidate.installation_type, &candidate.path),
        installation_type: candidate.installation_type,
        scope: candidate.scope,
        configuration_path: candidate.path.clone(),
        provenance: candidate.provenance,
        eligible: false,
        blockers: vec![blocker(kind, &candidate.path, detail)],
        game_settings_path: settings,
        game_settings_state: DolphinSettingsDirectoryState::Missing,
        game_settings_warning: None,
        configuration_identity: None,
        game_settings_identity: None,
    }
}

fn blocker(
    kind: DolphinProfileBlockerKind,
    path: &Path,
    detail: impl Into<String>,
) -> DolphinProfileBlocker {
    DolphinProfileBlocker {
        kind,
        path: EncodedPath::from_path(path),
        detail: detail.into(),
    }
}

fn real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn profile_id(kind: DolphinInstallationType, path: &Path) -> String {
    let mut digest = Sha256::new();
    #[cfg(unix)]
    digest.update(path.as_os_str().as_bytes());
    #[cfg(not(unix))]
    digest.update(path.as_os_str().to_string_lossy().as_bytes());
    let kind = match kind {
        DolphinInstallationType::Native => "native",
        DolphinInstallationType::FlatpakUser => "flatpak-user",
        DolphinInstallationType::FlatpakSystem => "flatpak-system",
        DolphinInstallationType::Explicit => "explicit",
    };
    format!(
        "dolphin-{kind}-{:016x}",
        u64::from_be_bytes(digest.finalize()[..8].try_into().unwrap())
    )
}

pub fn inspect_dolphin_profile(
    profile: &DolphinProfile,
) -> Result<DolphinGameIniInventory, DolphinInspectionError> {
    inspect_dolphin_profile_with_limit(profile, DOLPHIN_MAX_GAME_INI_FILES)
}

fn inspect_dolphin_profile_with_limit(
    profile: &DolphinProfile,
    file_limit: usize,
) -> Result<DolphinGameIniInventory, DolphinInspectionError> {
    if !profile.eligible {
        return Err(DolphinInspectionError::IneligibleProfile {
            profile_id: profile.profile_id.clone(),
        });
    }
    let validated = validate_destination_root(&profile.configuration_path).map_err(|_| {
        DolphinInspectionError::UnsafeProfile {
            path: profile.configuration_path.clone(),
        }
    })?;
    if validated.state() != DestinationRootState::ExistingDirectory
        || inspect_marker(&profile.configuration_path).is_err()
    {
        return Err(DolphinInspectionError::ProfileChanged {
            path: profile.configuration_path.clone(),
        });
    }
    if profile.configuration_identity.is_some()
        && fs::symlink_metadata(&profile.configuration_path)
            .ok()
            .and_then(|m| directory_identity(&m))
            != profile.configuration_identity
    {
        return Err(DolphinInspectionError::ProfileChanged {
            path: profile.configuration_path.clone(),
        });
    }
    let mut inventory = DolphinGameIniInventory {
        profile_id: profile.profile_id.clone(),
        files: Vec::new(),
        warnings: Vec::new(),
        entries_visited: 0,
        bytes_inspected: 0,
        complete: true,
    };
    if profile.game_settings_state != DolphinSettingsDirectoryState::Available {
        return Ok(inventory);
    }
    if !matches!(validate_destination_root(&profile.game_settings_path), Ok(root) if root.state() == DestinationRootState::ExistingDirectory)
        || (profile.game_settings_identity.is_some()
            && fs::symlink_metadata(&profile.game_settings_path)
                .ok()
                .and_then(|m| directory_identity(&m))
                != profile.game_settings_identity)
    {
        warn(
            &mut inventory,
            DolphinInspectionWarningKind::UnsafePath,
            &profile.game_settings_path,
            "GameSettings path or identity changed after discovery",
        );
        return Ok(inventory);
    }
    let entries = match fs::read_dir(&profile.game_settings_path) {
        Ok(entries) => entries,
        Err(error) => {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::UnreadablePath,
                &profile.game_settings_path,
                format!("GameSettings cannot be read: {error}"),
            );
            return Ok(inventory);
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        if inventory.entries_visited >= DOLPHIN_MAX_ENTRIES_VISITED {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::EntryLimitReached,
                &profile.game_settings_path,
                format!("entry inspection stopped at {DOLPHIN_MAX_ENTRIES_VISITED}"),
            );
            break;
        }
        inventory.entries_visited += 1;
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => warn(
                &mut inventory,
                DolphinInspectionWarningKind::UnreadablePath,
                &profile.game_settings_path,
                format!("directory entry cannot be read: {error}"),
            ),
        }
    }
    paths.sort();
    for path in paths {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(error) => {
                warn(
                    &mut inventory,
                    DolphinInspectionWarningKind::UnreadablePath,
                    &path,
                    format!("entry cannot be inspected: {error}"),
                );
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::SymlinkSkipped,
                &path,
                "symlink was not followed",
            );
            continue;
        }
        if !metadata.is_file() {
            if is_ini(&path) {
                warn(
                    &mut inventory,
                    DolphinInspectionWarningKind::SpecialFileSkipped,
                    &path,
                    "non-regular INI entry was skipped",
                );
            }
            continue;
        }
        if !is_ini(&path) {
            continue;
        }
        if inventory.files.len() >= file_limit {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::FileCountLimitReached,
                &path,
                format!("INI parsing stopped at {file_limit} files"),
            );
            break;
        }
        if metadata.len() > DOLPHIN_MAX_GAME_INI_BYTES {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::FileTooLarge,
                &path,
                format!("INI exceeds {DOLPHIN_MAX_GAME_INI_BYTES} bytes"),
            );
            continue;
        }
        if inventory.bytes_inspected.saturating_add(metadata.len())
            > DOLPHIN_MAX_TOTAL_GAME_INI_BYTES
        {
            warn(
                &mut inventory,
                DolphinInspectionWarningKind::TotalBytesLimitReached,
                &path,
                format!("total INI input would exceed {DOLPHIN_MAX_TOTAL_GAME_INI_BYTES} bytes"),
            );
            break;
        }
        if let Some(file) = inspect_ini(&path, metadata.len(), &mut inventory) {
            inventory.files.push(file);
        }
    }
    mark_duplicates(&mut inventory);
    inventory.files.sort_by(|a, b| a.path.cmp(&b.path));
    inventory
        .warnings
        .sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
    Ok(inventory)
}

fn inspect_ini(
    path: &Path,
    expected_size: u64,
    inventory: &mut DolphinGameIniInventory,
) -> Option<DolphinGameIniFile> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            warn(
                inventory,
                DolphinInspectionWarningKind::UnreadablePath,
                path,
                format!("INI cannot be opened safely: {error}"),
            );
            return None;
        }
    };
    let metadata = match file.metadata() {
        Ok(m) if m.is_file() && m.len() == expected_size => m,
        Ok(_) => {
            warn(
                inventory,
                DolphinInspectionWarningKind::UnsafePath,
                path,
                "INI identity or size changed before reading",
            );
            return None;
        }
        Err(error) => {
            warn(
                inventory,
                DolphinInspectionWarningKind::UnreadablePath,
                path,
                format!("opened INI cannot be inspected: {error}"),
            );
            return None;
        }
    };
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .by_ref()
        .take(DOLPHIN_MAX_GAME_INI_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 != metadata.len()
    {
        warn(
            inventory,
            DolphinInspectionWarningKind::UnreadablePath,
            path,
            "INI could not be read completely",
        );
        return None;
    }
    let mut local_warnings = Vec::new();
    if bytes.split(|b| *b == b'\n').count() > DOLPHIN_MAX_LINES_PER_FILE {
        warn(
            inventory,
            DolphinInspectionWarningKind::LineCountLimitReached,
            path,
            format!("INI exceeds {DOLPHIN_MAX_LINES_PER_FILE} lines"),
        );
        return None;
    }
    if bytes
        .split(|b| *b == b'\n')
        .any(|line| line.len() > DOLPHIN_MAX_LINE_BYTES)
    {
        warn(
            inventory,
            DolphinInspectionWarningKind::LineTooLong,
            path,
            format!("INI contains a line over {DOLPHIN_MAX_LINE_BYTES} bytes"),
        );
        return None;
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            local_warnings.push(DolphinInspectionWarningKind::InvalidUtf8);
            warn(
                inventory,
                DolphinInspectionWarningKind::InvalidUtf8,
                path,
                "INI is not valid UTF-8; invalid bytes were replaced for structural parsing",
            );
            String::from_utf8_lossy(&bytes).into_owned()
        }
    };
    let (game_id, revision, region) = parse_game_identity(path.file_stem().unwrap_or_default());
    if game_id.is_none() {
        local_warnings.push(DolphinInspectionWarningKind::InvalidGameId);
        warn(
            inventory,
            DolphinInspectionWarningKind::InvalidGameId,
            path,
            "filename is not a supported Dolphin game ID with optional revision",
        );
    }
    let mut parsed = ParsedIni::default();
    parse_ini_text(&text, &mut parsed, &mut local_warnings);
    if local_warnings.contains(&DolphinInspectionWarningKind::MalformedIni) {
        warn(
            inventory,
            DolphinInspectionWarningKind::MalformedIni,
            path,
            "INI contains malformed section or code-name syntax",
        );
    }
    inventory.bytes_inspected += bytes.len() as u64;
    Some(DolphinGameIniFile {
        path: path.to_path_buf(),
        filename_stem: path.file_stem().unwrap_or_default().to_os_string(),
        game_id_candidate: game_id,
        revision_candidate: revision,
        region_candidate: region,
        frame_patch_names: parsed.frame,
        action_replay_names: parsed.ar,
        gecko_names: parsed.gecko,
        riivolution_names: parsed.riivolution,
        enabled_frame_patch_names: parsed.frame_enabled,
        enabled_action_replay_names: parsed.ar_enabled,
        enabled_gecko_names: parsed.gecko_enabled,
        enabled_riivolution_names: parsed.riivolution_enabled,
        size_bytes: bytes.len() as u64,
        sha256: Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        duplicate_game_identity: false,
        duplicate_filename: false,
        duplicate_content: false,
        warnings: local_warnings,
    })
}

#[derive(Default)]
struct ParsedIni {
    frame: Vec<String>,
    ar: Vec<String>,
    gecko: Vec<String>,
    riivolution: Vec<String>,
    frame_enabled: Vec<String>,
    ar_enabled: Vec<String>,
    gecko_enabled: Vec<String>,
    riivolution_enabled: Vec<String>,
}

fn parse_ini_text(
    text: &str,
    parsed: &mut ParsedIni,
    warnings: &mut Vec<DolphinInspectionWarningKind>,
) {
    let mut section = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                push_unique_warning(warnings, DolphinInspectionWarningKind::MalformedIni);
                section = None;
                continue;
            }
            section = section_kind(&line[1..line.len() - 1]);
            continue;
        }
        let Some((kind, enabled)) = section else {
            continue;
        };
        if !line.starts_with('$') {
            continue;
        }
        let name = line[1..]
            .split(['=', '\t'])
            .next()
            .unwrap_or_default()
            .trim();
        if name.is_empty() {
            push_unique_warning(warnings, DolphinInspectionWarningKind::MalformedIni);
            continue;
        }
        let target = match (kind, enabled) {
            (DolphinCodeKind::FramePatch, false) => &mut parsed.frame,
            (DolphinCodeKind::FramePatch, true) => &mut parsed.frame_enabled,
            (DolphinCodeKind::ActionReplay, false) => &mut parsed.ar,
            (DolphinCodeKind::ActionReplay, true) => &mut parsed.ar_enabled,
            (DolphinCodeKind::Gecko, false) => &mut parsed.gecko,
            (DolphinCodeKind::Gecko, true) => &mut parsed.gecko_enabled,
            (DolphinCodeKind::Riivolution, false) => &mut parsed.riivolution,
            (DolphinCodeKind::Riivolution, true) => &mut parsed.riivolution_enabled,
        };
        if target.len() < MAX_RETAINED_NAMES_PER_KIND && !target.iter().any(|value| value == name) {
            target.push(name.to_string());
        }
    }
}

fn section_kind(value: &str) -> Option<(DolphinCodeKind, bool)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "onframe" => Some((DolphinCodeKind::FramePatch, false)),
        "onframe_enabled" => Some((DolphinCodeKind::FramePatch, true)),
        "actionreplay" => Some((DolphinCodeKind::ActionReplay, false)),
        "actionreplay_enabled" => Some((DolphinCodeKind::ActionReplay, true)),
        "gecko" => Some((DolphinCodeKind::Gecko, false)),
        "gecko_enabled" => Some((DolphinCodeKind::Gecko, true)),
        "riivolution" => Some((DolphinCodeKind::Riivolution, false)),
        "riivolution_enabled" => Some((DolphinCodeKind::Riivolution, true)),
        _ => None,
    }
}

fn parse_game_identity(stem: &std::ffi::OsStr) -> (Option<String>, Option<u16>, Option<String>) {
    let Some(stem) = stem.to_str() else {
        return (None, None, None);
    };
    let (id, revision) = match stem.rsplit_once('r') {
        Some((id, revision))
            if !revision.is_empty() && revision.bytes().all(|b| b.is_ascii_digit()) =>
        {
            let Ok(revision) = revision.parse::<u16>() else {
                return (None, None, None);
            };
            (id, Some(revision))
        }
        _ => (stem, None),
    };
    if !(3..=6).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return (None, None, None);
    }
    let id = id.to_ascii_uppercase();
    let region = (id.len() >= 4).then(|| {
        match id.as_bytes()[3] as char {
            'E' => "NTSC-U",
            'J' => "NTSC-J",
            'K' | 'Q' | 'T' => "NTSC-K",
            'P' | 'D' | 'F' | 'H' | 'I' | 'L' | 'M' | 'R' | 'S' | 'U' | 'V' | 'X' | 'Y' | 'Z' => {
                "PAL"
            }
            _ => "Unknown",
        }
        .to_string()
    });
    (Some(id), revision, region)
}

fn normalize_verified_game_id(value: &str) -> Option<String> {
    let value = value.trim();
    ((3..=6).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| value.to_ascii_uppercase())
}

pub fn match_dolphin_inventory(
    inventory: &DolphinGameIniInventory,
    verified_game_id: Option<&str>,
    verified_revision: Option<u16>,
) -> DolphinMatchResult {
    let Some(value) = verified_game_id else {
        return DolphinMatchResult {
            state: DolphinMatchState::NoVerifiedGameIdAvailable,
            verified_game_id: None,
            verified_revision,
            matching_files: Vec::new(),
            reason: "ArchiveFS has no separately verified Dolphin game ID for this archive".into(),
        };
    };
    let Some(game_id) = normalize_verified_game_id(value) else {
        return DolphinMatchResult {
            state: DolphinMatchState::InvalidVerifiedGameId,
            verified_game_id: None,
            verified_revision,
            matching_files: Vec::new(),
            reason: "the supplied verified game ID is not three to six ASCII letters or digits"
                .into(),
        };
    };
    let game_matches: Vec<&DolphinGameIniFile> = inventory
        .files
        .iter()
        .filter(|f| f.game_id_candidate.as_deref() == Some(&game_id))
        .collect();
    if game_matches.is_empty() {
        return DolphinMatchResult {
            state: DolphinMatchState::NoMatchingIniFound,
            verified_game_id: Some(game_id),
            verified_revision,
            matching_files: Vec::new(),
            reason: "no inspected GameSettings filename matches the verified game ID".into(),
        };
    }
    let selected: Vec<&DolphinGameIniFile> = match verified_revision {
        Some(revision) => game_matches
            .iter()
            .copied()
            .filter(|f| f.revision_candidate == Some(revision))
            .collect(),
        None => game_matches.clone(),
    };
    if verified_revision.is_some() && selected.is_empty() {
        return DolphinMatchResult {
            state: DolphinMatchState::RevisionMismatch,
            verified_game_id: Some(game_id),
            verified_revision,
            matching_files: game_matches.into_iter().map(|f| f.path.clone()).collect(),
            reason: "game ID matched, but no INI matched the verified revision".into(),
        };
    }
    let paths = selected
        .into_iter()
        .map(|f| f.path.clone())
        .collect::<Vec<_>>();
    let state = if paths.len() > 1 {
        DolphinMatchState::MultipleIniFilesForGame
    } else if verified_revision.is_some() {
        DolphinMatchState::ExactGameIdAndRevisionMatch
    } else {
        DolphinMatchState::ExactGameIdMatch
    };
    let reason = match state {
        DolphinMatchState::MultipleIniFilesForGame => {
            "multiple GameSettings INI files match the verified identity"
        }
        DolphinMatchState::ExactGameIdAndRevisionMatch => {
            "one GameSettings INI matches the verified game ID and revision"
        }
        _ => "one GameSettings INI matches the verified game ID",
    };
    DolphinMatchResult {
        state,
        verified_game_id: Some(game_id),
        verified_revision,
        matching_files: paths,
        reason: reason.into(),
    }
}

fn mark_duplicates(inventory: &mut DolphinGameIniInventory) {
    let mut identities: BTreeMap<(String, Option<u16>), usize> = BTreeMap::new();
    let mut filenames: BTreeMap<OsString, usize> = BTreeMap::new();
    let mut hashes: BTreeMap<String, usize> = BTreeMap::new();
    for file in &inventory.files {
        if let Some(id) = &file.game_id_candidate {
            *identities
                .entry((id.clone(), file.revision_candidate))
                .or_default() += 1;
        }
        *filenames
            .entry(file.path.file_name().unwrap_or_default().to_os_string())
            .or_default() += 1;
        *hashes.entry(file.sha256.clone()).or_default() += 1;
    }
    for file in &mut inventory.files {
        file.duplicate_game_identity = file.game_id_candidate.as_ref().is_some_and(|id| {
            identities
                .get(&(id.clone(), file.revision_candidate))
                .copied()
                .unwrap_or_default()
                > 1
        });
        file.duplicate_filename = filenames
            .get(file.path.file_name().unwrap_or_default())
            .copied()
            .unwrap_or_default()
            > 1;
        file.duplicate_content = hashes.get(&file.sha256).copied().unwrap_or_default() > 1;
        if file.duplicate_game_identity {
            push_unique_warning(
                &mut file.warnings,
                DolphinInspectionWarningKind::DuplicateGameIdentity,
            );
        }
        if file.duplicate_filename {
            push_unique_warning(
                &mut file.warnings,
                DolphinInspectionWarningKind::DuplicateFilename,
            );
        }
        if file.duplicate_content {
            push_unique_warning(
                &mut file.warnings,
                DolphinInspectionWarningKind::DuplicateContent,
            );
        }
    }
}

fn is_ini(path: &Path) -> bool {
    path.extension().is_some_and(|value| value == "ini")
}

fn warn(
    inventory: &mut DolphinGameIniInventory,
    kind: DolphinInspectionWarningKind,
    path: &Path,
    detail: impl Into<String>,
) {
    inventory.complete = false;
    inventory.warnings.push(DolphinInspectionWarning {
        kind,
        path: path.to_path_buf(),
        detail: detail.into(),
    });
}

fn push_unique_warning(
    warnings: &mut Vec<DolphinInspectionWarningKind>,
    kind: DolphinInspectionWarningKind,
) {
    if !warnings.contains(&kind) {
        warnings.push(kind);
    }
}

#[cfg(unix)]
fn directory_identity(metadata: &fs::Metadata) -> Option<DolphinDirectoryIdentity> {
    metadata.is_dir().then(|| DolphinDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn directory_identity(_metadata: &fs::Metadata) -> Option<DolphinDirectoryIdentity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!(
            "archivefs-dolphin-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn roots(root: &Path) -> DolphinProfileDiscoveryRoots {
        DolphinProfileDiscoveryRoots {
            home: root.join("home"),
            xdg_config_home: root.join("config"),
            xdg_data_home: root.join("data"),
            flatpak_system_root: root.join("system"),
            explicit_configuration_roots: Vec::new(),
        }
    }

    fn make_profile(path: &Path) -> PathBuf {
        fs::create_dir_all(path.join("GameSettings")).unwrap();
        fs::write(path.join("Dolphin.ini"), b"[Core]\n").unwrap();
        path.to_path_buf()
    }

    fn eligible(path: &Path) -> DolphinProfile {
        let mut discovery_roots = roots(path.parent().unwrap());
        discovery_roots
            .explicit_configuration_roots
            .push(path.to_path_buf());
        discover_dolphin_profiles(&discovery_roots)
            .unwrap()
            .profiles
            .into_iter()
            .find(|p| p.configuration_path == path)
            .unwrap()
    }

    #[test]
    fn discovers_native_flatpak_and_exact_profiles() {
        let root = fixture("discovery");
        make_profile(&root.join("config/dolphin-emu"));
        make_profile(&root.join("home/.var/app/org.DolphinEmu.dolphin-emu/config/dolphin-emu"));
        let explicit = make_profile(&root.join("portable"));
        let mut discovery_roots = roots(&root);
        discovery_roots.explicit_configuration_roots.push(explicit);
        let discovery = discover_dolphin_profiles(&discovery_roots).unwrap();
        assert_eq!(discovery.profiles.len(), 3);
        assert!(discovery.profiles.iter().all(|p| p.eligible));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_supported_sections_without_modifying_files() {
        let root = fixture("parse");
        let profile = make_profile(&root.join("portable"));
        let ini = profile.join("GameSettings/GALE01r2.ini");
        let body = b"[OnFrame]\n$60 FPS\n0x0=1\n[ActionReplay]\n$Infinite Lives\n[ActionReplay_Enabled]\n$Infinite Lives\n[Gecko]\n$Widescreen\n[Gecko_Enabled]\n$Widescreen\n[Riivolution]\n$Texture Pack\n";
        fs::write(&ini, body).unwrap();
        let inventory = inspect_dolphin_profile(&eligible(&profile)).unwrap();
        assert_eq!(inventory.files.len(), 1);
        let file = &inventory.files[0];
        assert_eq!(file.game_id_candidate.as_deref(), Some("GALE01"));
        assert_eq!(file.revision_candidate, Some(2));
        assert_eq!(file.region_candidate.as_deref(), Some("NTSC-U"));
        assert_eq!(file.definition_count(), 4);
        assert_eq!(file.enabled_count(), 2);
        assert_eq!(fs::read(ini).unwrap(), body);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matching_requires_verified_identity() {
        let root = fixture("match");
        let profile = make_profile(&root.join("portable"));
        fs::write(
            profile.join("GameSettings/GALE01r2.ini"),
            b"[Gecko]\n$Code\n",
        )
        .unwrap();
        let inventory = inspect_dolphin_profile(&eligible(&profile)).unwrap();
        assert_eq!(
            match_dolphin_inventory(&inventory, None, None).state,
            DolphinMatchState::NoVerifiedGameIdAvailable
        );
        assert_eq!(
            match_dolphin_inventory(&inventory, Some("gale01"), Some(2)).state,
            DolphinMatchState::ExactGameIdAndRevisionMatch
        );
        assert_eq!(
            match_dolphin_inventory(&inventory, Some("GALE01"), Some(1)).state,
            DolphinMatchState::RevisionMismatch
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_profiles_and_ini_files() {
        use std::os::unix::fs::symlink;
        let root = fixture("symlink");
        let real = make_profile(&root.join("real"));
        fs::create_dir_all(root.join("container")).unwrap();
        symlink(&real, root.join("container/profile")).unwrap();
        let mut discovery_roots = roots(&root);
        discovery_roots
            .explicit_configuration_roots
            .push(root.join("container/profile"));
        assert!(
            !discover_dolphin_profiles(&discovery_roots)
                .unwrap()
                .profiles[0]
                .eligible
        );
        fs::write(root.join("outside.ini"), b"[Gecko]\n$Code\n").unwrap();
        symlink(
            root.join("outside.ini"),
            real.join("GameSettings/GALE01.ini"),
        )
        .unwrap();
        let inventory = inspect_dolphin_profile(&eligible(&real)).unwrap();
        assert!(inventory.files.is_empty());
        assert!(
            inventory
                .warnings
                .iter()
                .any(|w| w.kind == DolphinInspectionWarningKind::SymlinkSkipped)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_and_unsafe_exact_roots_are_blocked() {
        let root = fixture("blocked");
        fs::create_dir_all(&root).unwrap();
        let mut discovery_roots = roots(&root);
        discovery_roots.explicit_configuration_roots = vec![
            root.join("missing"),
            PathBuf::from("relative"),
            PathBuf::from("/"),
        ];
        let discovery = discover_dolphin_profiles(&discovery_roots).unwrap();
        assert_eq!(discovery.profiles.len(), 3);
        assert!(discovery.profiles.iter().all(|p| !p.eligible));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_ids_and_resource_limits_are_explicit() {
        let root = fixture("limits");
        let profile = make_profile(&root.join("portable"));
        fs::write(
            profile.join("GameSettings/not-an-id.ini"),
            b"[Gecko]\n$Code\n",
        )
        .unwrap();
        fs::write(
            profile.join("GameSettings/GALE01.ini"),
            vec![b'x'; DOLPHIN_MAX_LINE_BYTES + 1],
        )
        .unwrap();
        let inventory = inspect_dolphin_profile(&eligible(&profile)).unwrap();
        assert_eq!(inventory.files.len(), 1);
        assert!(
            inventory
                .warnings
                .iter()
                .any(|w| w.kind == DolphinInspectionWarningKind::InvalidGameId)
        );
        assert!(
            inventory
                .warnings
                .iter()
                .any(|w| w.kind == DolphinInspectionWarningKind::LineTooLong)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
