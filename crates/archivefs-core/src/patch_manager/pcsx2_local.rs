//! Bounded, read-only discovery and inspection of local PCSX2 profiles.
//!
//! This module has no write, process-execution, or network capability. It
//! validates configuration and patch roots without following symlinks, opens
//! PNACH files read-only (with `O_NOFOLLOW` on Unix), and applies fixed limits
//! before retaining any parsed metadata.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::emulator_environment::EncodedPath;

use super::destination_safety::{
    DestinationRootState, DestinationSafetyFailureReason, validate_destination_root,
};
use super::pcsx2::{normalize_crc, parse_patch_identity};

pub const PCSX2_MAX_PROFILES: usize = 16;
pub const PCSX2_MAX_PATCH_DIRECTORIES_PER_PROFILE: usize = 4;
pub const PCSX2_MAX_DIRECTORIES_TRAVERSED: usize = 256;
pub const PCSX2_MAX_ENTRIES_VISITED: usize = 10_000;
pub const PCSX2_MAX_DIRECTORY_DEPTH: usize = 4;
pub const PCSX2_MAX_PNACH_FILES: usize = 2_048;
pub const PCSX2_MAX_PNACH_FILE_BYTES: u64 = 256 * 1024;
pub const PCSX2_MAX_TOTAL_PNACH_BYTES: u64 = 16 * 1024 * 1024;
pub const PCSX2_MAX_LINES_PER_FILE: usize = 8_192;
pub const PCSX2_MAX_LINE_BYTES: usize = 8 * 1024;

const FLATPAK_APP_ID: &str = "net.pcsx2.PCSX2";
const PCSX2_MAX_RETAINED_COMMENTS_PER_FILE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2InstallationType {
    Native,
    FlatpakUser,
    FlatpakSystem,
    Portable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2ProfileScope {
    User,
    SystemInstallationUserProfile,
    Portable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2ProfileBlockerKind {
    PathNotAbsolute,
    FilesystemRoot,
    MissingConfiguration,
    UnsafePath,
    NotDirectory,
    Unreadable,
    MissingPcsx2Evidence,
    ProfileLimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pcsx2ProfileBlocker {
    pub kind: Pcsx2ProfileBlockerKind,
    pub path: EncodedPath,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2PatchCategory {
    Cheats,
    WidescreenPatches,
    OtherPatches,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2PatchDirectoryState {
    Available,
    Missing,
    UnsafePath,
    NotDirectory,
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2PatchDirectory {
    pub path: PathBuf,
    pub category: Pcsx2PatchCategory,
    pub state: Pcsx2PatchDirectoryState,
    pub warning: Option<String>,
    pub identity: Option<Pcsx2DirectoryIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pcsx2DirectoryIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2Profile {
    pub profile_id: String,
    pub installation_type: Pcsx2InstallationType,
    pub scope: Pcsx2ProfileScope,
    pub configuration_path: PathBuf,
    pub provenance: &'static str,
    pub eligible: bool,
    pub blockers: Vec<Pcsx2ProfileBlocker>,
    pub patch_directories: Vec<Pcsx2PatchDirectory>,
    pub configuration_identity: Option<Pcsx2DirectoryIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2ProfileDiscovery {
    pub profiles: Vec<Pcsx2Profile>,
    pub warnings: Vec<Pcsx2ProfileBlocker>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2ProfileDiscoveryRoots {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub xdg_data_home: PathBuf,
    pub flatpak_system_root: PathBuf,
    /// Portable roots must come from an already known PCSX2 configuration,
    /// never from blind filesystem searching.
    pub portable_configuration_roots: Vec<PathBuf>,
}

impl Pcsx2ProfileDiscoveryRoots {
    pub fn from_environment() -> Result<Self, Pcsx2DiscoveryError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(Pcsx2DiscoveryError::HomeUnavailable)?;
        let xdg_config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let xdg_data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Ok(Self {
            home,
            xdg_config_home,
            xdg_data_home,
            flatpak_system_root: PathBuf::from("/var/lib/flatpak"),
            portable_configuration_roots: Vec::new(),
        })
    }
}

#[derive(Debug)]
pub enum Pcsx2DiscoveryError {
    HomeUnavailable,
    Inspection { path: PathBuf, source: io::Error },
}

impl std::fmt::Display for Pcsx2DiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HomeUnavailable => formatter.write_str("HOME is not set"),
            Self::Inspection { path, source } => {
                write!(formatter, "failed to inspect {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Pcsx2DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Inspection { source, .. } => Some(source),
            Self::HomeUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2InspectionWarningKind {
    UnsafePath,
    UnreadablePath,
    SymlinkSkipped,
    SpecialFileSkipped,
    EntryLimitReached,
    DirectoryLimitReached,
    DepthLimitReached,
    FileCountLimitReached,
    FileTooLarge,
    TotalBytesLimitReached,
    LineCountLimitReached,
    LineTooLong,
    MalformedPnach,
    InvalidUtf8,
    DuplicateCrc,
    DuplicateFilename,
    DuplicateContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2InspectionWarning {
    pub kind: Pcsx2InspectionWarningKind,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2PnachFile {
    pub path: PathBuf,
    pub filename_stem: OsString,
    pub category: Pcsx2PatchCategory,
    pub crc_candidate: Option<String>,
    pub serial_candidate: Option<String>,
    pub title_candidates: Vec<String>,
    pub region_candidates: Vec<String>,
    pub comments: Vec<String>,
    pub patch_entry_count: usize,
    pub enabled_patch_count: usize,
    pub disabled_patch_count: usize,
    pub unknown_patch_count: usize,
    pub size_bytes: u64,
    pub sha256: String,
    pub duplicate_crc: bool,
    pub duplicate_filename: bool,
    pub duplicate_content: bool,
    pub warnings: Vec<Pcsx2InspectionWarningKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2PnachInventory {
    pub profile_id: String,
    pub files: Vec<Pcsx2PnachFile>,
    pub warnings: Vec<Pcsx2InspectionWarning>,
    pub directories_traversed: usize,
    pub entries_visited: usize,
    pub bytes_inspected: u64,
    pub complete: bool,
}

#[derive(Debug)]
pub enum Pcsx2InspectionError {
    IneligibleProfile { profile_id: String },
    ProfileChanged { path: PathBuf },
    UnsafeProfile { path: PathBuf },
}

impl std::fmt::Display for Pcsx2InspectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IneligibleProfile { profile_id } => {
                write!(formatter, "PCSX2 profile {profile_id} is not eligible")
            }
            Self::ProfileChanged { path } => {
                write!(
                    formatter,
                    "PCSX2 profile changed before inspection: {}",
                    path.display()
                )
            }
            Self::UnsafeProfile { path } => {
                write!(
                    formatter,
                    "PCSX2 profile path is unsafe: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for Pcsx2InspectionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2MatchState {
    ExactCrcMatch,
    MultiplePnachFilesForSameCrc,
    CandidateByFilenameOrTitleOnly,
    NoVerifiedGameCrcAvailable,
    NoMatchingPnachFound,
    InvalidVerifiedGameCrc,
    IdentityExtractionDeferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2MatchResult {
    pub state: Pcsx2MatchState,
    pub verified_crc: Option<String>,
    pub matching_files: Vec<PathBuf>,
    pub reason: String,
}

#[derive(Debug, Clone)]
struct ProfileCandidate {
    installation_type: Pcsx2InstallationType,
    scope: Pcsx2ProfileScope,
    configuration_path: PathBuf,
    provenance: &'static str,
    report_missing: bool,
}

/// Discovers only documented paths plus explicitly supplied portable roots.
/// Missing standard paths are ignored; existing but unsafe or unproven paths
/// remain visible as blocked profiles.
pub fn discover_pcsx2_profiles(
    roots: &Pcsx2ProfileDiscoveryRoots,
) -> Result<Pcsx2ProfileDiscovery, Pcsx2DiscoveryError> {
    let flatpak_config = roots
        .home
        .join(".var/app")
        .join(FLATPAK_APP_ID)
        .join("config/PCSX2");
    let user_flatpak_install = roots.xdg_data_home.join("flatpak/app").join(FLATPAK_APP_ID);
    let system_flatpak_install = roots.flatpak_system_root.join("app").join(FLATPAK_APP_ID);
    let user_installed = is_real_directory_no_follow(&user_flatpak_install).unwrap_or(false);
    let system_installed = is_real_directory_no_follow(&system_flatpak_install).unwrap_or(false);
    let flatpak_kind = if system_installed && !user_installed {
        Pcsx2InstallationType::FlatpakSystem
    } else {
        Pcsx2InstallationType::FlatpakUser
    };
    let flatpak_scope = if flatpak_kind == Pcsx2InstallationType::FlatpakSystem {
        Pcsx2ProfileScope::SystemInstallationUserProfile
    } else {
        Pcsx2ProfileScope::User
    };
    let mut candidates = vec![
        ProfileCandidate {
            installation_type: Pcsx2InstallationType::Native,
            scope: Pcsx2ProfileScope::User,
            configuration_path: roots.xdg_config_home.join("PCSX2"),
            provenance: "XDG PCSX2 configuration directory",
            report_missing: false,
        },
        ProfileCandidate {
            installation_type: flatpak_kind,
            scope: flatpak_scope,
            configuration_path: flatpak_config,
            provenance: "Flatpak net.pcsx2.PCSX2 user configuration directory",
            report_missing: false,
        },
    ];
    candidates.extend(
        roots
            .portable_configuration_roots
            .iter()
            .cloned()
            .map(|path| ProfileCandidate {
                installation_type: Pcsx2InstallationType::Portable,
                scope: Pcsx2ProfileScope::Portable,
                configuration_path: path,
                provenance: "Explicitly known PCSX2 portable configuration directory",
                report_missing: true,
            }),
    );
    candidates.sort_by(|left, right| left.configuration_path.cmp(&right.configuration_path));
    candidates.dedup_by(|left, right| left.configuration_path == right.configuration_path);

    let mut profiles = Vec::new();
    let mut warnings = Vec::new();
    for candidate in candidates {
        if profiles.len() >= PCSX2_MAX_PROFILES {
            warnings.push(blocker(
                Pcsx2ProfileBlockerKind::ProfileLimitReached,
                &candidate.configuration_path,
                format!("profile discovery stopped at the {PCSX2_MAX_PROFILES}-profile limit"),
            ));
            break;
        }
        if !candidate.configuration_path.is_absolute() {
            profiles.push(blocked_profile(
                candidate,
                Pcsx2ProfileBlockerKind::PathNotAbsolute,
                "configuration path is not absolute",
            ));
            continue;
        }
        if candidate.configuration_path.parent().is_none() {
            profiles.push(blocked_profile(
                candidate,
                Pcsx2ProfileBlockerKind::FilesystemRoot,
                "a filesystem root cannot be a PCSX2 profile",
            ));
            continue;
        }
        let validated = match validate_destination_root(&candidate.configuration_path) {
            Ok(validated) => validated,
            Err(error) => {
                let kind = match error.reason {
                    DestinationSafetyFailureReason::RootNotDirectory
                    | DestinationSafetyFailureReason::NonDirectoryParent => {
                        Pcsx2ProfileBlockerKind::NotDirectory
                    }
                    DestinationSafetyFailureReason::InspectionFailed => {
                        Pcsx2ProfileBlockerKind::Unreadable
                    }
                    _ => Pcsx2ProfileBlockerKind::UnsafePath,
                };
                profiles.push(blocked_profile(
                    candidate,
                    kind,
                    format!("configuration path rejected: {:?}", error.reason),
                ));
                continue;
            }
        };
        if validated.state() == DestinationRootState::Absent {
            if candidate.report_missing {
                profiles.push(blocked_profile(
                    candidate,
                    Pcsx2ProfileBlockerKind::MissingConfiguration,
                    "configuration directory does not exist",
                ));
            }
            continue;
        }
        let marker_state = inspect_pcsx2_marker(&candidate.configuration_path);
        if let Err((kind, detail)) = marker_state {
            profiles.push(blocked_profile(candidate, kind, detail));
            continue;
        }
        let configuration_identity = fs::symlink_metadata(&candidate.configuration_path)
            .ok()
            .and_then(|metadata| directory_identity(&metadata));
        let patch_directories = known_patch_directories(&candidate.configuration_path);
        profiles.push(Pcsx2Profile {
            profile_id: profile_id(candidate.installation_type, &candidate.configuration_path),
            installation_type: candidate.installation_type,
            scope: candidate.scope,
            configuration_path: candidate.configuration_path,
            provenance: candidate.provenance,
            eligible: true,
            blockers: Vec::new(),
            patch_directories,
            configuration_identity,
        });
    }
    profiles.sort_by(|left, right| {
        left.installation_type
            .cmp(&right.installation_type)
            .then_with(|| left.configuration_path.cmp(&right.configuration_path))
    });
    let complete = warnings.is_empty();
    Ok(Pcsx2ProfileDiscovery {
        profiles,
        warnings,
        complete,
    })
}

fn inspect_pcsx2_marker(root: &Path) -> Result<(), (Pcsx2ProfileBlockerKind, &'static str)> {
    let markers = [root.join("inis"), root.join("PCSX2.ini")];
    for marker in markers {
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err((
                    Pcsx2ProfileBlockerKind::UnsafePath,
                    "PCSX2 evidence path is a symlink",
                ));
            }
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => return Ok(()),
            Ok(_) => {
                return Err((
                    Pcsx2ProfileBlockerKind::MissingPcsx2Evidence,
                    "PCSX2 evidence path has an unsupported file type",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err((
                    Pcsx2ProfileBlockerKind::Unreadable,
                    "PCSX2 evidence path is unreadable",
                ));
            }
        }
    }
    Err((
        Pcsx2ProfileBlockerKind::MissingPcsx2Evidence,
        "no PCSX2.ini file or inis directory was found",
    ))
}

fn known_patch_directories(root: &Path) -> Vec<Pcsx2PatchDirectory> {
    [
        ("cheats", Pcsx2PatchCategory::Cheats, true),
        ("cheats_ws", Pcsx2PatchCategory::WidescreenPatches, true),
        ("patches", Pcsx2PatchCategory::OtherPatches, false),
    ]
    .into_iter()
    .filter_map(|(name, category, report_missing)| {
        let path = root.join(name);
        let (state, warning, identity) = inspect_patch_directory(&path);
        (report_missing || state != Pcsx2PatchDirectoryState::Missing).then_some(
            Pcsx2PatchDirectory {
                path,
                category,
                state,
                warning,
                identity,
            },
        )
    })
    .take(PCSX2_MAX_PATCH_DIRECTORIES_PER_PROFILE)
    .collect()
}

fn inspect_patch_directory(
    path: &Path,
) -> (
    Pcsx2PatchDirectoryState,
    Option<String>,
    Option<Pcsx2DirectoryIdentity>,
) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => (
            Pcsx2PatchDirectoryState::UnsafePath,
            Some("directory is a symlink and will not be followed".to_string()),
            None,
        ),
        Ok(metadata) if metadata.is_dir() => (
            Pcsx2PatchDirectoryState::Available,
            None,
            directory_identity(&metadata),
        ),
        Ok(_) => (
            Pcsx2PatchDirectoryState::NotDirectory,
            Some("path is not a directory".to_string()),
            None,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (Pcsx2PatchDirectoryState::Missing, None, None)
        }
        Err(error) => (
            Pcsx2PatchDirectoryState::Unreadable,
            Some(format!("directory cannot be inspected: {error}")),
            None,
        ),
    }
}

fn blocked_profile(
    candidate: ProfileCandidate,
    kind: Pcsx2ProfileBlockerKind,
    detail: impl Into<String>,
) -> Pcsx2Profile {
    Pcsx2Profile {
        profile_id: profile_id(candidate.installation_type, &candidate.configuration_path),
        installation_type: candidate.installation_type,
        scope: candidate.scope,
        blockers: vec![blocker(kind, &candidate.configuration_path, detail)],
        configuration_path: candidate.configuration_path,
        provenance: candidate.provenance,
        eligible: false,
        patch_directories: Vec::new(),
        configuration_identity: None,
    }
}

fn blocker(
    kind: Pcsx2ProfileBlockerKind,
    path: &Path,
    detail: impl Into<String>,
) -> Pcsx2ProfileBlocker {
    Pcsx2ProfileBlocker {
        kind,
        path: EncodedPath::from_path(path),
        detail: detail.into(),
    }
}

fn is_real_directory_no_follow(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn profile_id(kind: Pcsx2InstallationType, path: &Path) -> String {
    let mut digest = Sha256::new();
    #[cfg(unix)]
    digest.update(path.as_os_str().as_bytes());
    #[cfg(not(unix))]
    digest.update(path.as_os_str().to_string_lossy().as_bytes());
    let kind = match kind {
        Pcsx2InstallationType::Native => "native",
        Pcsx2InstallationType::FlatpakUser => "flatpak-user",
        Pcsx2InstallationType::FlatpakSystem => "flatpak-system",
        Pcsx2InstallationType::Portable => "portable",
    };
    format!(
        "pcsx2-{kind}-{:016x}",
        u64::from_be_bytes(digest.finalize()[..8].try_into().unwrap())
    )
}

/// Inspects the currently discovered directories again. This rejects a
/// profile whose root became unsafe or disappeared after discovery.
pub fn inspect_pcsx2_profile(
    profile: &Pcsx2Profile,
) -> Result<Pcsx2PnachInventory, Pcsx2InspectionError> {
    inspect_pcsx2_profile_with_limits(profile, PCSX2_MAX_PNACH_FILES, PCSX2_MAX_DIRECTORY_DEPTH)
}

fn inspect_pcsx2_profile_with_limits(
    profile: &Pcsx2Profile,
    max_pnach_files: usize,
    max_directory_depth: usize,
) -> Result<Pcsx2PnachInventory, Pcsx2InspectionError> {
    if !profile.eligible {
        return Err(Pcsx2InspectionError::IneligibleProfile {
            profile_id: profile.profile_id.clone(),
        });
    }
    let validated = validate_destination_root(&profile.configuration_path).map_err(|_| {
        Pcsx2InspectionError::UnsafeProfile {
            path: profile.configuration_path.clone(),
        }
    })?;
    if validated.state() != DestinationRootState::ExistingDirectory
        || inspect_pcsx2_marker(&profile.configuration_path).is_err()
    {
        return Err(Pcsx2InspectionError::ProfileChanged {
            path: profile.configuration_path.clone(),
        });
    }
    let current_identity = fs::symlink_metadata(&profile.configuration_path)
        .ok()
        .and_then(|metadata| directory_identity(&metadata));
    if profile.configuration_identity.is_some()
        && current_identity != profile.configuration_identity
    {
        return Err(Pcsx2InspectionError::ProfileChanged {
            path: profile.configuration_path.clone(),
        });
    }

    let mut inventory = Pcsx2PnachInventory {
        profile_id: profile.profile_id.clone(),
        files: Vec::new(),
        warnings: Vec::new(),
        directories_traversed: 0,
        entries_visited: 0,
        bytes_inspected: 0,
        complete: true,
    };
    for directory in profile
        .patch_directories
        .iter()
        .filter(|directory| directory.state == Pcsx2PatchDirectoryState::Available)
    {
        if inventory.directories_traversed >= PCSX2_MAX_DIRECTORIES_TRAVERSED {
            limit_warning(
                &mut inventory,
                Pcsx2InspectionWarningKind::DirectoryLimitReached,
                &directory.path,
                format!("directory traversal stopped at {PCSX2_MAX_DIRECTORIES_TRAVERSED}"),
            );
            break;
        }
        if inventory.entries_visited >= PCSX2_MAX_ENTRIES_VISITED {
            limit_warning(
                &mut inventory,
                Pcsx2InspectionWarningKind::EntryLimitReached,
                &directory.path,
                format!("entry inspection stopped at {PCSX2_MAX_ENTRIES_VISITED}"),
            );
            break;
        }
        if inventory.files.len() >= max_pnach_files {
            limit_warning(
                &mut inventory,
                Pcsx2InspectionWarningKind::FileCountLimitReached,
                &directory.path,
                format!("PNACH parsing stopped at {max_pnach_files} files"),
            );
            break;
        }
        if inventory.bytes_inspected >= PCSX2_MAX_TOTAL_PNACH_BYTES {
            limit_warning(
                &mut inventory,
                Pcsx2InspectionWarningKind::TotalBytesLimitReached,
                &directory.path,
                format!("total input reached {PCSX2_MAX_TOTAL_PNACH_BYTES} bytes"),
            );
            break;
        }
        inspect_patch_tree(
            directory,
            &mut inventory,
            max_pnach_files,
            max_directory_depth,
        )?;
    }
    mark_duplicates(&mut inventory);
    inventory
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    inventory.warnings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
    });
    Ok(inventory)
}

fn inspect_patch_tree(
    directory: &Pcsx2PatchDirectory,
    inventory: &mut Pcsx2PnachInventory,
    max_pnach_files: usize,
    max_directory_depth: usize,
) -> Result<(), Pcsx2InspectionError> {
    match validate_destination_root(&directory.path) {
        Ok(root) if root.state() == DestinationRootState::ExistingDirectory => {}
        Ok(_) => return Ok(()),
        Err(_) => {
            inventory.complete = false;
            inventory.warnings.push(warning(
                Pcsx2InspectionWarningKind::UnsafePath,
                &directory.path,
                "patch directory became unsafe after profile discovery",
            ));
            return Ok(());
        }
    }
    let current_identity = fs::symlink_metadata(&directory.path)
        .ok()
        .and_then(|metadata| directory_identity(&metadata));
    if directory.identity.is_some() && current_identity != directory.identity {
        inventory.complete = false;
        inventory.warnings.push(warning(
            Pcsx2InspectionWarningKind::UnsafePath,
            &directory.path,
            "patch directory identity changed after profile discovery",
        ));
        return Ok(());
    }
    let mut pending = VecDeque::from([(directory.path.clone(), 0_usize, directory.identity)]);
    while let Some((path, depth, expected_identity)) = pending.pop_front() {
        if inventory.directories_traversed >= PCSX2_MAX_DIRECTORIES_TRAVERSED {
            limit_warning(
                inventory,
                Pcsx2InspectionWarningKind::DirectoryLimitReached,
                &path,
                format!("directory traversal stopped at {PCSX2_MAX_DIRECTORIES_TRAVERSED}"),
            );
            break;
        }
        inventory.directories_traversed += 1;
        let validated = validate_destination_root(&path);
        let current_identity = fs::symlink_metadata(&path)
            .ok()
            .and_then(|metadata| directory_identity(&metadata));
        if !matches!(validated, Ok(root) if root.state() == DestinationRootState::ExistingDirectory)
            || (expected_identity.is_some() && current_identity != expected_identity)
        {
            inventory.complete = false;
            inventory.warnings.push(warning(
                Pcsx2InspectionWarningKind::UnsafePath,
                &path,
                "directory path or identity changed before traversal",
            ));
            continue;
        }
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                inventory.complete = false;
                inventory.warnings.push(warning(
                    Pcsx2InspectionWarningKind::UnreadablePath,
                    &path,
                    format!("directory cannot be read: {error}"),
                ));
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            if inventory.entries_visited >= PCSX2_MAX_ENTRIES_VISITED {
                limit_warning(
                    inventory,
                    Pcsx2InspectionWarningKind::EntryLimitReached,
                    &path,
                    format!("entry inspection stopped at {PCSX2_MAX_ENTRIES_VISITED}"),
                );
                return Ok(());
            }
            inventory.entries_visited += 1;
            match entry {
                Ok(entry) => children.push(entry.path()),
                Err(error) => {
                    inventory.complete = false;
                    inventory.warnings.push(warning(
                        Pcsx2InspectionWarningKind::UnreadablePath,
                        &path,
                        format!("directory entry cannot be read: {error}"),
                    ));
                }
            }
        }
        children.sort();
        for child in children {
            let metadata = match fs::symlink_metadata(&child) {
                Ok(metadata) => metadata,
                Err(error) => {
                    inventory.complete = false;
                    inventory.warnings.push(warning(
                        Pcsx2InspectionWarningKind::UnreadablePath,
                        &child,
                        format!("entry metadata cannot be read: {error}"),
                    ));
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                inventory.complete = false;
                inventory.warnings.push(warning(
                    Pcsx2InspectionWarningKind::SymlinkSkipped,
                    &child,
                    "symlink entry was not followed",
                ));
            } else if metadata.is_dir() {
                if depth >= max_directory_depth {
                    inventory.complete = false;
                    inventory.warnings.push(warning(
                        Pcsx2InspectionWarningKind::DepthLimitReached,
                        &child,
                        format!("directory depth exceeds {max_directory_depth}"),
                    ));
                } else {
                    pending.push_back((child, depth + 1, directory_identity(&metadata)));
                }
            } else if metadata.is_file() {
                if is_pnach_path(&child) {
                    inspect_pnach_file(
                        &child,
                        directory.category,
                        inventory,
                        max_pnach_files,
                        current_identity,
                    );
                }
            } else {
                inventory.complete = false;
                inventory.warnings.push(warning(
                    Pcsx2InspectionWarningKind::SpecialFileSkipped,
                    &child,
                    "special filesystem entry was not opened",
                ));
            }
        }
    }
    Ok(())
}

fn inspect_pnach_file(
    path: &Path,
    category: Pcsx2PatchCategory,
    inventory: &mut Pcsx2PnachInventory,
    max_pnach_files: usize,
    expected_parent_identity: Option<Pcsx2DirectoryIdentity>,
) {
    if inventory.files.len() >= max_pnach_files {
        limit_warning(
            inventory,
            Pcsx2InspectionWarningKind::FileCountLimitReached,
            path,
            format!("PNACH parsing stopped at {max_pnach_files} files"),
        );
        return;
    }
    let parent_is_stable = path.parent().is_some_and(|parent| {
        matches!(
            validate_destination_root(parent),
            Ok(root) if root.state() == DestinationRootState::ExistingDirectory
        ) && (expected_parent_identity.is_none()
            || fs::symlink_metadata(parent)
                .ok()
                .and_then(|metadata| directory_identity(&metadata))
                == expected_parent_identity)
    });
    if !parent_is_stable {
        inventory.complete = false;
        inventory.warnings.push(warning(
            Pcsx2InspectionWarningKind::UnsafePath,
            path,
            "PNACH parent path or identity changed before file open",
        ));
        return;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            inventory.complete = false;
            inventory.warnings.push(warning(
                Pcsx2InspectionWarningKind::UnreadablePath,
                path,
                format!("PNACH file cannot be opened safely: {error}"),
            ));
            return;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            inventory.complete = false;
            inventory.warnings.push(warning(
                Pcsx2InspectionWarningKind::SpecialFileSkipped,
                path,
                "opened entry is not a regular file",
            ));
            return;
        }
        Err(error) => {
            inventory.complete = false;
            inventory.warnings.push(warning(
                Pcsx2InspectionWarningKind::UnreadablePath,
                path,
                format!("opened PNACH metadata cannot be read: {error}"),
            ));
            return;
        }
    };
    if metadata.len() > PCSX2_MAX_PNACH_FILE_BYTES {
        inventory.complete = false;
        inventory.warnings.push(warning(
            Pcsx2InspectionWarningKind::FileTooLarge,
            path,
            format!("file exceeds the {PCSX2_MAX_PNACH_FILE_BYTES}-byte limit"),
        ));
        return;
    }
    if inventory.bytes_inspected.saturating_add(metadata.len()) > PCSX2_MAX_TOTAL_PNACH_BYTES {
        limit_warning(
            inventory,
            Pcsx2InspectionWarningKind::TotalBytesLimitReached,
            path,
            format!("total input exceeds the {PCSX2_MAX_TOTAL_PNACH_BYTES}-byte limit"),
        );
        return;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if let Err(error) = file
        .by_ref()
        .take(PCSX2_MAX_PNACH_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        inventory.complete = false;
        inventory.warnings.push(warning(
            Pcsx2InspectionWarningKind::UnreadablePath,
            path,
            format!("PNACH file cannot be read: {error}"),
        ));
        return;
    }
    if bytes.len() as u64 > PCSX2_MAX_PNACH_FILE_BYTES {
        inventory.complete = false;
        inventory.warnings.push(warning(
            Pcsx2InspectionWarningKind::FileTooLarge,
            path,
            "file grew beyond the per-file limit while being read",
        ));
        return;
    }
    let parsed = match parse_pnach(path, category, &bytes) {
        Ok(parsed) => parsed,
        Err((kind, detail)) => {
            inventory.complete = false;
            inventory.warnings.push(warning(kind, path, detail));
            return;
        }
    };
    inventory.bytes_inspected = inventory.bytes_inspected.saturating_add(bytes.len() as u64);
    for kind in &parsed.warnings {
        inventory.warnings.push(warning(
            *kind,
            path,
            match kind {
                Pcsx2InspectionWarningKind::InvalidUtf8 => {
                    "PNACH contains invalid UTF-8; metadata was decoded lossily"
                }
                Pcsx2InspectionWarningKind::MalformedPnach => {
                    "PNACH contains unrecognized patch syntax"
                }
                _ => "PNACH metadata warning",
            },
        ));
    }
    inventory.files.push(parsed);
}

fn parse_pnach(
    path: &Path,
    category: Pcsx2PatchCategory,
    bytes: &[u8],
) -> Result<Pcsx2PnachFile, (Pcsx2InspectionWarningKind, String)> {
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if lines.len() > PCSX2_MAX_LINES_PER_FILE {
        return Err((
            Pcsx2InspectionWarningKind::LineCountLimitReached,
            format!("file exceeds the {PCSX2_MAX_LINES_PER_FILE}-line limit"),
        ));
    }
    if lines.iter().any(|line| line.len() > PCSX2_MAX_LINE_BYTES) {
        return Err((
            Pcsx2InspectionWarningKind::LineTooLong,
            format!("line exceeds the {PCSX2_MAX_LINE_BYTES}-byte limit"),
        ));
    }
    let mut file_warnings = Vec::new();
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            file_warnings.push(Pcsx2InspectionWarningKind::InvalidUtf8);
            String::from_utf8_lossy(bytes).into_owned()
        }
    };
    let mut title_candidates = BTreeSet::new();
    let mut region_candidates = BTreeSet::new();
    let mut comments = Vec::new();
    let mut patch_entry_count = 0_usize;
    let mut enabled_patch_count = 0_usize;
    let mut disabled_patch_count = 0_usize;
    let mut unknown_patch_count = 0_usize;
    for line in text.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        if let Some(value) = line_value(line, &lower, "gametitle=") {
            if !value.is_empty() {
                title_candidates.insert(value.to_string());
            }
        } else if let Some(value) = line_value(line, &lower, "region=") {
            if !value.is_empty() {
                region_candidates.insert(value.to_string());
            }
        } else if let Some(value) = line_value(line, &lower, "comment=") {
            if !value.is_empty() && comments.len() < PCSX2_MAX_RETAINED_COMMENTS_PER_FILE {
                comments.push(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("//").or_else(|| line.strip_prefix('#')) {
            let value = value.trim();
            if !value.is_empty() && comments.len() < PCSX2_MAX_RETAINED_COMMENTS_PER_FILE {
                comments.push(value.to_string());
            }
        } else if let Some(rest) = lower.strip_prefix("patch=") {
            patch_entry_count += 1;
            match rest.split(',').next().map(str::trim) {
                Some("1") => enabled_patch_count += 1,
                Some("0") => disabled_patch_count += 1,
                _ => {
                    unknown_patch_count += 1;
                    file_warnings.push(Pcsx2InspectionWarningKind::MalformedPnach);
                }
            }
        }
    }
    file_warnings.sort_by_key(|warning| format!("{warning:?}"));
    file_warnings.dedup();
    let stem = path.file_stem().unwrap_or_default().to_os_string();
    let (serial_candidate, crc_candidate) = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(parse_patch_identity)
        .unwrap_or((None, None));
    let sha256 = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Pcsx2PnachFile {
        path: path.to_path_buf(),
        filename_stem: stem,
        category,
        crc_candidate,
        serial_candidate,
        title_candidates: title_candidates.into_iter().collect(),
        region_candidates: region_candidates.into_iter().collect(),
        comments,
        patch_entry_count,
        enabled_patch_count,
        disabled_patch_count,
        unknown_patch_count,
        size_bytes: bytes.len() as u64,
        sha256,
        duplicate_crc: false,
        duplicate_filename: false,
        duplicate_content: false,
        warnings: file_warnings,
    })
}

fn line_value<'a>(line: &'a str, lower: &str, prefix: &str) -> Option<&'a str> {
    lower
        .strip_prefix(prefix)
        .map(|suffix| &line[line.len() - suffix.len()..])
}

fn mark_duplicates(inventory: &mut Pcsx2PnachInventory) {
    let mut crcs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut names: BTreeMap<OsString, Vec<usize>> = BTreeMap::new();
    let mut digests: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, file) in inventory.files.iter().enumerate() {
        if let Some(crc) = &file.crc_candidate {
            crcs.entry(crc.clone()).or_default().push(index);
        }
        names
            .entry(file.path.file_name().unwrap_or_default().to_os_string())
            .or_default()
            .push(index);
        digests.entry(file.sha256.clone()).or_default().push(index);
    }
    for indices in crcs.values().filter(|indices| indices.len() > 1) {
        for index in indices {
            inventory.files[*index].duplicate_crc = true;
            inventory.files[*index]
                .warnings
                .push(Pcsx2InspectionWarningKind::DuplicateCrc);
        }
    }
    for indices in names.values().filter(|indices| indices.len() > 1) {
        for index in indices {
            inventory.files[*index].duplicate_filename = true;
            inventory.files[*index]
                .warnings
                .push(Pcsx2InspectionWarningKind::DuplicateFilename);
        }
    }
    for indices in digests.values().filter(|indices| indices.len() > 1) {
        for index in indices {
            inventory.files[*index].duplicate_content = true;
            inventory.files[*index]
                .warnings
                .push(Pcsx2InspectionWarningKind::DuplicateContent);
        }
    }
}

fn is_pnach_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pnach"))
}

fn warning(
    kind: Pcsx2InspectionWarningKind,
    path: &Path,
    detail: impl Into<String>,
) -> Pcsx2InspectionWarning {
    Pcsx2InspectionWarning {
        kind,
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn limit_warning(
    inventory: &mut Pcsx2PnachInventory,
    kind: Pcsx2InspectionWarningKind,
    path: &Path,
    detail: impl Into<String>,
) {
    inventory.complete = false;
    inventory.warnings.push(warning(kind, path, detail));
}

/// Matches only against a caller-supplied verified executable CRC. Filename
/// and comment evidence can produce a candidate state, never an exact match.
pub fn match_pcsx2_inventory(
    inventory: &Pcsx2PnachInventory,
    verified_crc: Option<&str>,
    archive_title: Option<&str>,
) -> Pcsx2MatchResult {
    if let Some(value) = verified_crc {
        let Some(crc) = normalize_crc(value) else {
            return Pcsx2MatchResult {
                state: Pcsx2MatchState::InvalidVerifiedGameCrc,
                verified_crc: None,
                matching_files: Vec::new(),
                reason: "the supplied verified game CRC is not eight hexadecimal digits".into(),
            };
        };
        let matching_files: Vec<PathBuf> = inventory
            .files
            .iter()
            .filter(|file| file.crc_candidate.as_deref() == Some(crc.as_str()))
            .map(|file| file.path.clone())
            .collect();
        let (state, reason) = match matching_files.len() {
            0 => (
                Pcsx2MatchState::NoMatchingPnachFound,
                "no inspected PNACH filename contains the verified game CRC",
            ),
            1 => (
                Pcsx2MatchState::ExactCrcMatch,
                "one PNACH filename matches the verified game CRC",
            ),
            _ => (
                Pcsx2MatchState::MultiplePnachFilesForSameCrc,
                "multiple PNACH files match the verified game CRC",
            ),
        };
        return Pcsx2MatchResult {
            state,
            verified_crc: Some(crc),
            matching_files,
            reason: reason.into(),
        };
    }
    let normalized_title = archive_title
        .map(normalize_title)
        .filter(|title| !title.is_empty());
    let matching_files: Vec<PathBuf> = normalized_title
        .as_deref()
        .map(|wanted| {
            inventory
                .files
                .iter()
                .filter(|file| {
                    file.title_candidates
                        .iter()
                        .any(|title| normalize_title(title) == wanted)
                        || file
                            .filename_stem
                            .to_str()
                            .is_some_and(|stem| normalize_title(stem) == wanted)
                })
                .map(|file| file.path.clone())
                .collect()
        })
        .unwrap_or_default();
    if !matching_files.is_empty() {
        Pcsx2MatchResult {
            state: Pcsx2MatchState::CandidateByFilenameOrTitleOnly,
            verified_crc: None,
            matching_files,
            reason:
                "filename or comment-title matches are unverified candidates, not exact identity"
                    .into(),
        }
    } else {
        Pcsx2MatchResult {
            state: Pcsx2MatchState::NoVerifiedGameCrcAvailable,
            verified_crc: None,
            matching_files: Vec::new(),
            reason: "ArchiveFS has no verified PCSX2 executable CRC for this archive".into(),
        }
    }
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(unix)]
fn directory_identity(metadata: &fs::Metadata) -> Option<Pcsx2DirectoryIdentity> {
    metadata.is_dir().then(|| Pcsx2DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn directory_identity(_metadata: &fs::Metadata) -> Option<Pcsx2DirectoryIdentity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!(
            "archivefs-pcsx2-local-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn roots(root: &Path) -> Pcsx2ProfileDiscoveryRoots {
        Pcsx2ProfileDiscoveryRoots {
            home: root.join("home"),
            xdg_config_home: root.join("config"),
            xdg_data_home: root.join("data"),
            flatpak_system_root: root.join("system-flatpak"),
            portable_configuration_roots: Vec::new(),
        }
    }

    fn make_profile(root: &Path) -> PathBuf {
        fs::create_dir_all(root.join("inis")).unwrap();
        fs::create_dir_all(root.join("cheats")).unwrap();
        root.to_path_buf()
    }

    fn eligible_profile(root: &Path) -> Pcsx2Profile {
        let mut discovery_roots = roots(root.parent().unwrap());
        discovery_roots.portable_configuration_roots = vec![root.to_path_buf()];
        discover_pcsx2_profiles(&discovery_roots)
            .unwrap()
            .profiles
            .into_iter()
            .find(|profile| profile.configuration_path == root)
            .unwrap()
    }

    #[test]
    fn discovers_native_and_flatpak_user_profiles() {
        let root = fixture_root("discovery");
        make_profile(&root.join("config/PCSX2"));
        make_profile(&root.join("home/.var/app/net.pcsx2.PCSX2/config/PCSX2"));
        fs::create_dir_all(root.join("data/flatpak/app/net.pcsx2.PCSX2")).unwrap();
        let discovery = discover_pcsx2_profiles(&roots(&root)).unwrap();
        assert_eq!(discovery.profiles.len(), 2);
        assert!(discovery.profiles.iter().all(|profile| profile.eligible));
        assert!(
            discovery
                .profiles
                .iter()
                .any(|profile| profile.installation_type == Pcsx2InstallationType::Native)
        );
        assert!(
            discovery
                .profiles
                .iter()
                .any(|profile| { profile.installation_type == Pcsx2InstallationType::FlatpakUser })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_system_flatpak_installation_scope() {
        let root = fixture_root("system-flatpak");
        make_profile(&root.join("home/.var/app/net.pcsx2.PCSX2/config/PCSX2"));
        fs::create_dir_all(root.join("system-flatpak/app/net.pcsx2.PCSX2")).unwrap();
        let profile = discover_pcsx2_profiles(&roots(&root))
            .unwrap()
            .profiles
            .pop()
            .unwrap();
        assert_eq!(
            profile.installation_type,
            Pcsx2InstallationType::FlatpakSystem
        );
        assert_eq!(
            profile.scope,
            Pcsx2ProfileScope::SystemInstallationUserProfile
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_directories_are_not_created_and_missing_portable_is_blocked() {
        let root = fixture_root("missing");
        fs::create_dir_all(&root).unwrap();
        let missing = root.join("portable");
        let mut discovery_roots = roots(&root);
        discovery_roots
            .portable_configuration_roots
            .push(missing.clone());
        let discovery = discover_pcsx2_profiles(&discovery_roots).unwrap();
        assert_eq!(discovery.profiles.len(), 1);
        assert!(!discovery.profiles[0].eligible);
        assert_eq!(
            discovery.profiles[0].blockers[0].kind,
            Pcsx2ProfileBlockerKind::MissingConfiguration
        );
        assert!(!missing.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_profile_and_cheat_directory_are_refused() {
        use std::os::unix::fs::symlink;
        let root = fixture_root("symlink");
        make_profile(&root.join("real"));
        fs::create_dir_all(root.join("container")).unwrap();
        symlink(root.join("real"), root.join("container/profile")).unwrap();
        let mut discovery_roots = roots(&root);
        discovery_roots.portable_configuration_roots = vec![root.join("container/profile")];
        let discovery = discover_pcsx2_profiles(&discovery_roots).unwrap();
        assert!(!discovery.profiles[0].eligible);

        let second = make_profile(&root.join("second"));
        fs::remove_dir_all(second.join("cheats")).unwrap();
        symlink(root.join("real/cheats"), second.join("cheats")).unwrap();
        discovery_roots.portable_configuration_roots = vec![second.clone()];
        let profile = discover_pcsx2_profiles(&discovery_roots)
            .unwrap()
            .profiles
            .pop()
            .unwrap();
        assert!(profile.eligible);
        assert_eq!(
            profile.patch_directories[0].state,
            Pcsx2PatchDirectoryState::UnsafePath
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_crc_metadata_and_categories_without_writing() {
        let root = fixture_root("parse");
        let profile_root = make_profile(&root.join("portable"));
        fs::create_dir_all(profile_root.join("cheats_ws")).unwrap();
        fs::write(
            profile_root.join("cheats/DEADBEEF.pnach"),
            b"gametitle=Example Game\nregion=PAL\ncomment=Owner note\npatch=1,EE,00100000,word,00000001\npatch=0,EE,00100004,word,00000002\n",
        )
        .unwrap();
        fs::write(
            profile_root.join("cheats_ws/CAFEBABE.pnach"),
            b"patch=1,EE,00100000,word,00000001\n",
        )
        .unwrap();
        let before = fs::read(profile_root.join("cheats/DEADBEEF.pnach")).unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert_eq!(inventory.files.len(), 2);
        assert_eq!(
            inventory.files[0].crc_candidate.as_deref(),
            Some("DEADBEEF")
        );
        assert_eq!(inventory.files[0].enabled_patch_count, 1);
        assert_eq!(inventory.files[0].disabled_patch_count, 1);
        assert_eq!(inventory.files[0].comments, vec!["Owner note"]);
        assert!(
            inventory
                .files
                .iter()
                .any(|file| { file.category == Pcsx2PatchCategory::WidescreenPatches })
        );
        assert_eq!(
            before,
            fs::read(profile_root.join("cheats/DEADBEEF.pnach")).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_and_oversized_pnach_files_are_reported() {
        let root = fixture_root("limits");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(profile_root.join("cheats/BAD.pnach"), b"patch=maybe\n").unwrap();
        let mut oversized = File::create(profile_root.join("cheats/TOOBIG.pnach")).unwrap();
        oversized
            .write_all(&vec![b'x'; PCSX2_MAX_PNACH_FILE_BYTES as usize + 1])
            .unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert_eq!(inventory.files.len(), 1);
        assert!(
            inventory.files[0]
                .warnings
                .contains(&Pcsx2InspectionWarningKind::MalformedPnach)
        );
        assert!(
            inventory
                .warnings
                .iter()
                .any(|warning| { warning.kind == Pcsx2InspectionWarningKind::FileTooLarge })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicates_and_match_confidence_are_explicit() {
        let root = fixture_root("matches");
        let profile_root = make_profile(&root.join("portable"));
        fs::create_dir_all(profile_root.join("cheats_ws")).unwrap();
        let body = b"gametitle=Example Game\npatch=1,EE,0,word,1\n";
        fs::write(profile_root.join("cheats/DEADBEEF.pnach"), body).unwrap();
        fs::write(profile_root.join("cheats_ws/DEADBEEF.pnach"), body).unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert!(inventory.files.iter().all(|file| file.duplicate_crc));
        assert!(inventory.files.iter().all(|file| file.duplicate_filename));
        assert!(inventory.files.iter().all(|file| file.duplicate_content));
        assert_eq!(
            match_pcsx2_inventory(&inventory, Some("DEADBEEF"), None).state,
            Pcsx2MatchState::MultiplePnachFilesForSameCrc
        );
        assert_eq!(
            match_pcsx2_inventory(&inventory, None, Some("Example Game")).state,
            Pcsx2MatchState::CandidateByFilenameOrTitleOnly
        );
        assert_eq!(
            match_pcsx2_inventory(&inventory, None, Some("Different")).state,
            Pcsx2MatchState::NoVerifiedGameCrcAvailable
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_crc_filename_is_not_identity_evidence() {
        let root = fixture_root("invalid-crc");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(profile_root.join("cheats/NOT-A-CRC.pnach"), b"patch=1,x\n").unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert_eq!(inventory.files[0].crc_candidate, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relative_and_filesystem_root_profiles_are_blocked_without_inspection() {
        let root = fixture_root("unsafe-roots");
        fs::create_dir_all(&root).unwrap();
        let mut discovery_roots = roots(&root);
        discovery_roots.portable_configuration_roots =
            vec![PathBuf::from("relative"), PathBuf::from("/")];
        let discovery = discover_pcsx2_profiles(&discovery_roots).unwrap();
        assert_eq!(discovery.profiles.len(), 2);
        assert!(discovery.profiles.iter().all(|profile| !profile.eligible));
        assert!(discovery.profiles.iter().any(|profile| {
            profile.blockers[0].kind == Pcsx2ProfileBlockerKind::PathNotAbsolute
        }));
        assert!(discovery.profiles.iter().any(|profile| {
            profile.blockers[0].kind == Pcsx2ProfileBlockerKind::FilesystemRoot
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_count_and_depth_limits_are_deterministic() {
        let root = fixture_root("bounded");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(profile_root.join("cheats/00000001.pnach"), b"patch=1,x\n").unwrap();
        fs::write(profile_root.join("cheats/00000002.pnach"), b"patch=1,x\n").unwrap();
        fs::write(profile_root.join("cheats/00000003.pnach"), b"patch=1,x\n").unwrap();
        fs::create_dir_all(profile_root.join("cheats/a/b")).unwrap();
        fs::write(
            profile_root.join("cheats/a/b/00000004.pnach"),
            b"patch=1,x\n",
        )
        .unwrap();
        let profile = eligible_profile(&profile_root);
        let inventory = inspect_pcsx2_profile_with_limits(&profile, 2, 1).unwrap();
        assert_eq!(inventory.files.len(), 2);
        assert!(!inventory.complete);
        assert!(
            inventory.warnings.iter().any(|warning| {
                warning.kind == Pcsx2InspectionWarningKind::FileCountLimitReached
            })
        );
        assert!(
            inventory
                .warnings
                .iter()
                .any(|warning| { warning.kind == Pcsx2InspectionWarningKind::DepthLimitReached })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn changed_profile_identity_is_rejected_before_file_inspection() {
        let root = fixture_root("identity-change");
        let profile_root = make_profile(&root.join("portable"));
        let profile = eligible_profile(&profile_root);
        fs::rename(&profile_root, root.join("old-profile")).unwrap();
        make_profile(&profile_root);
        let error = inspect_pcsx2_profile(&profile).unwrap_err();
        assert!(matches!(error, Pcsx2InspectionError::ProfileChanged { .. }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_no_match_and_invalid_crc_states_require_verified_input() {
        let root = fixture_root("exact");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(profile_root.join("cheats/DEADBEEF.pnach"), b"patch=1,x\n").unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert_eq!(
            match_pcsx2_inventory(&inventory, Some("deadbeef"), None).state,
            Pcsx2MatchState::ExactCrcMatch
        );
        assert_eq!(
            match_pcsx2_inventory(&inventory, Some("CAFEBABE"), None).state,
            Pcsx2MatchState::NoMatchingPnachFound
        );
        assert_eq!(
            match_pcsx2_inventory(&inventory, Some("not-a-crc"), None).state,
            Pcsx2MatchState::InvalidVerifiedGameCrc
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_profile_and_pnach_paths_remain_inspectable_by_exact_os_identity() {
        use std::os::unix::ffi::OsStringExt;
        let root = fixture_root("non-utf8");
        let profile_name = OsString::from_vec(b"PCSX2-\xff".to_vec());
        let profile_root = make_profile(&root.join(profile_name));
        let pnach_name = OsString::from_vec(b"DEADBEEF-\xfe.pnach".to_vec());
        fs::write(
            profile_root.join("cheats").join(&pnach_name),
            b"patch=1,x\n",
        )
        .unwrap();
        let profile = eligible_profile(&profile_root);
        let inventory = inspect_pcsx2_profile(&profile).unwrap();
        assert_eq!(inventory.files.len(), 1);
        assert_eq!(
            inventory.files[0].path.file_name(),
            Some(pnach_name.as_os_str())
        );
        assert!(profile.profile_id.starts_with("pcsx2-portable-"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_special_pnach_entries_are_reported_without_being_opened() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;
        let root = fixture_root("special");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(root.join("outside.pnach"), b"patch=1,x\n").unwrap();
        symlink(
            root.join("outside.pnach"),
            profile_root.join("cheats/link.pnach"),
        )
        .unwrap();
        let socket_path = profile_root.join("cheats/socket.pnach");
        let _listener = UnixListener::bind(&socket_path).unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert!(inventory.files.is_empty());
        assert!(
            inventory
                .warnings
                .iter()
                .any(|warning| { warning.kind == Pcsx2InspectionWarningKind::SymlinkSkipped })
        );
        assert!(
            inventory
                .warnings
                .iter()
                .any(|warning| { warning.kind == Pcsx2InspectionWarningKind::SpecialFileSkipped })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn line_count_and_line_length_limits_refuse_unbounded_metadata() {
        let root = fixture_root("line-limits");
        let profile_root = make_profile(&root.join("portable"));
        fs::write(
            profile_root.join("cheats/LONGLINE.pnach"),
            vec![b'x'; PCSX2_MAX_LINE_BYTES + 1],
        )
        .unwrap();
        let many_lines = "\n".repeat(PCSX2_MAX_LINES_PER_FILE + 1);
        fs::write(profile_root.join("cheats/MANYLINES.pnach"), many_lines).unwrap();
        let inventory = inspect_pcsx2_profile(&eligible_profile(&profile_root)).unwrap();
        assert!(inventory.files.is_empty());
        assert!(
            inventory
                .warnings
                .iter()
                .any(|warning| { warning.kind == Pcsx2InspectionWarningKind::LineTooLong })
        );
        assert!(
            inventory.warnings.iter().any(|warning| {
                warning.kind == Pcsx2InspectionWarningKind::LineCountLimitReached
            })
        );
        fs::remove_dir_all(root).unwrap();
    }
}
