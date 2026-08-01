//! Bounded, read-only discovery of local Xenia Canary installations.
//!
//! Xenia Canary has no single standard install location: real installs on
//! Linux are typically portable (a folder holding `xenia_canary.exe`,
//! `xenia-canary.config.toml`, and its own `patches` directory), often run
//! under Wine/Proton rather than natively packaged. No native path is
//! guessed here without real evidence for one - discovery is deliberately
//! limited to caller-supplied explicit directories, each validated the
//! same way as every other emulator adapter's profile roots.
//!
//! This module never starts Xenia and has no write or network capability.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::emulator_environment::EncodedPath;

use super::destination_safety::{
    DestinationRootState, DestinationSafetyFailureReason, validate_destination_root,
};

pub const XENIA_MAX_PROFILES: usize = 16;

/// The exact config filename Xenia Canary itself uses
/// (`xe::config::config_name` upstream) - the marker that distinguishes a
/// real Xenia Canary directory from an arbitrary folder.
const XENIA_CONFIG_MARKER: &str = "xenia-canary.config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaInstallationType {
    /// A caller-supplied directory - the only kind ArchiveFS currently
    /// discovers, since Xenia Canary has no single documented native path.
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaProfileScope {
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaProfileBlockerKind {
    PathNotAbsolute,
    FilesystemRoot,
    MissingConfiguration,
    UnsafePath,
    NotDirectory,
    Unreadable,
    MissingXeniaEvidence,
    ProfileLimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XeniaProfileBlocker {
    pub kind: XeniaProfileBlockerKind,
    pub path: EncodedPath,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaPatchesDirectoryState {
    Available,
    Missing,
    UnsafePath,
    NotDirectory,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XeniaDirectoryIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XeniaProfile {
    pub profile_id: String,
    pub installation_type: XeniaInstallationType,
    pub scope: XeniaProfileScope,
    /// The Xenia Canary root directory - alongside `xenia_canary.exe` and
    /// `xenia-canary.config.toml` in a real install.
    pub configuration_path: PathBuf,
    pub provenance: &'static str,
    pub eligible: bool,
    pub blockers: Vec<XeniaProfileBlocker>,
    /// `configuration_path/patches` - the only directory ArchiveFS ever
    /// manages for this adapter.
    pub patches_path: PathBuf,
    pub patches_state: XeniaPatchesDirectoryState,
    pub patches_warning: Option<String>,
    pub configuration_identity: Option<XeniaDirectoryIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XeniaProfileDiscovery {
    pub profiles: Vec<XeniaProfile>,
    pub warnings: Vec<XeniaProfileBlocker>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XeniaProfileDiscoveryRoots {
    /// Exact, already-known Xenia Canary directories; never searched for.
    pub explicit_configuration_roots: Vec<PathBuf>,
}

/// Discovers only caller-supplied explicit Xenia Canary directories - see
/// the module documentation for why no native path is guessed.
pub fn discover_xenia_profiles(roots: &XeniaProfileDiscoveryRoots) -> XeniaProfileDiscovery {
    let mut candidates: Vec<PathBuf> = roots.explicit_configuration_roots.clone();
    candidates.sort();
    candidates.dedup();

    let mut profiles = Vec::new();
    let mut warnings = Vec::new();
    for path in candidates {
        if profiles.len() >= XENIA_MAX_PROFILES {
            warnings.push(blocker(
                XeniaProfileBlockerKind::ProfileLimitReached,
                &path,
                format!("profile discovery stopped at the {XENIA_MAX_PROFILES}-profile limit"),
            ));
            break;
        }
        if !path.is_absolute() {
            profiles.push(blocked(
                path,
                XeniaProfileBlockerKind::PathNotAbsolute,
                "configuration path is not absolute",
            ));
            continue;
        }
        if path.parent().is_none() {
            profiles.push(blocked(
                path,
                XeniaProfileBlockerKind::FilesystemRoot,
                "a filesystem root cannot be a Xenia profile",
            ));
            continue;
        }
        let validated = match validate_destination_root(&path) {
            Ok(value) => value,
            Err(error) => {
                let kind = match error.reason {
                    DestinationSafetyFailureReason::RootNotDirectory
                    | DestinationSafetyFailureReason::NonDirectoryParent => {
                        XeniaProfileBlockerKind::NotDirectory
                    }
                    DestinationSafetyFailureReason::InspectionFailed => {
                        XeniaProfileBlockerKind::Unreadable
                    }
                    _ => XeniaProfileBlockerKind::UnsafePath,
                };
                profiles.push(blocked(
                    path,
                    kind,
                    format!("configuration path rejected: {:?}", error.reason),
                ));
                continue;
            }
        };
        if validated.state() == DestinationRootState::Absent {
            profiles.push(blocked(
                path,
                XeniaProfileBlockerKind::MissingConfiguration,
                "configuration directory does not exist",
            ));
            continue;
        }
        if let Err((kind, detail)) = inspect_marker(&path) {
            profiles.push(blocked(path, kind, detail));
            continue;
        }
        let patches_path = path.join("patches");
        let (patches_state, patches_warning, _) = inspect_patches_directory(&patches_path);
        let identity = fs::symlink_metadata(&path)
            .ok()
            .and_then(|metadata| directory_identity(&metadata));
        profiles.push(XeniaProfile {
            profile_id: profile_id(&path),
            installation_type: XeniaInstallationType::Explicit,
            scope: XeniaProfileScope::Explicit,
            configuration_path: path,
            provenance: "Explicitly supplied Xenia Canary directory",
            eligible: true,
            blockers: Vec::new(),
            patches_path,
            patches_state,
            patches_warning,
            configuration_identity: identity,
        });
    }
    profiles.sort_by(|a, b| a.configuration_path.cmp(&b.configuration_path));
    XeniaProfileDiscovery {
        complete: warnings.is_empty(),
        profiles,
        warnings,
    }
}

fn inspect_marker(root: &Path) -> Result<(), (XeniaProfileBlockerKind, &'static str)> {
    let marker = root.join(XENIA_CONFIG_MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err((
            XeniaProfileBlockerKind::UnsafePath,
            "xenia-canary.config.toml is a symlink",
        )),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err((
            XeniaProfileBlockerKind::MissingXeniaEvidence,
            "xenia-canary.config.toml is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err((
            XeniaProfileBlockerKind::MissingXeniaEvidence,
            "xenia-canary.config.toml was not found in this directory",
        )),
        Err(_) => Err((
            XeniaProfileBlockerKind::Unreadable,
            "xenia-canary.config.toml is unreadable",
        )),
    }
}

fn inspect_patches_directory(
    path: &Path,
) -> (
    XeniaPatchesDirectoryState,
    Option<String>,
    Option<XeniaDirectoryIdentity>,
) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => (
            XeniaPatchesDirectoryState::UnsafePath,
            Some("patches is a symlink and will not be followed".into()),
            None,
        ),
        Ok(metadata) if metadata.is_dir() => (
            XeniaPatchesDirectoryState::Available,
            None,
            directory_identity(&metadata),
        ),
        Ok(_) => (
            XeniaPatchesDirectoryState::NotDirectory,
            Some("patches is not a directory".into()),
            None,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (XeniaPatchesDirectoryState::Missing, None, None)
        }
        Err(error) => (
            XeniaPatchesDirectoryState::Unreadable,
            Some(format!("patches cannot be inspected: {error}")),
            None,
        ),
    }
}

fn blocked(
    path: PathBuf,
    kind: XeniaProfileBlockerKind,
    detail: impl Into<String>,
) -> XeniaProfile {
    let patches_path = path.join("patches");
    XeniaProfile {
        profile_id: profile_id(&path),
        installation_type: XeniaInstallationType::Explicit,
        scope: XeniaProfileScope::Explicit,
        configuration_path: path.clone(),
        provenance: "Explicitly supplied Xenia Canary directory",
        eligible: false,
        blockers: vec![blocker(kind, &path, detail)],
        patches_path,
        patches_state: XeniaPatchesDirectoryState::Missing,
        patches_warning: None,
        configuration_identity: None,
    }
}

fn blocker(
    kind: XeniaProfileBlockerKind,
    path: &Path,
    detail: impl Into<String>,
) -> XeniaProfileBlocker {
    XeniaProfileBlocker {
        kind,
        path: EncodedPath::from_path(path),
        detail: detail.into(),
    }
}

fn profile_id(path: &Path) -> String {
    let mut digest = Sha256::new();
    #[cfg(unix)]
    digest.update(path.as_os_str().as_bytes());
    #[cfg(not(unix))]
    digest.update(path.as_os_str().to_string_lossy().as_bytes());
    format!(
        "xenia-explicit-{:016x}",
        u64::from_be_bytes(digest.finalize()[..8].try_into().unwrap())
    )
}

#[cfg(unix)]
fn directory_identity(metadata: &fs::Metadata) -> Option<XeniaDirectoryIdentity> {
    metadata.is_dir().then(|| XeniaDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn directory_identity(_metadata: &fs::Metadata) -> Option<XeniaDirectoryIdentity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct FixtureDir(PathBuf);

    impl FixtureDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-xenia-local-{label}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_real_xenia_directory_with_the_marker_file_is_eligible() {
        let root = FixtureDir::new("real");
        fs::write(root.0.join(XENIA_CONFIG_MARKER), "").unwrap();
        fs::create_dir_all(root.0.join("patches")).unwrap();
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
            explicit_configuration_roots: vec![root.0.clone()],
        });
        assert_eq!(discovery.profiles.len(), 1);
        let profile = &discovery.profiles[0];
        assert!(profile.eligible);
        assert_eq!(profile.patches_state, XeniaPatchesDirectoryState::Available);
    }

    #[test]
    fn a_directory_missing_the_marker_file_is_blocked_not_guessed() {
        let root = FixtureDir::new("no-marker");
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
            explicit_configuration_roots: vec![root.0.clone()],
        });
        assert_eq!(discovery.profiles.len(), 1);
        let profile = &discovery.profiles[0];
        assert!(!profile.eligible);
        assert_eq!(
            profile.blockers[0].kind,
            XeniaProfileBlockerKind::MissingXeniaEvidence
        );
    }

    #[test]
    fn a_missing_patches_directory_is_not_an_eligibility_blocker() {
        let root = FixtureDir::new("no-patches-dir");
        fs::write(root.0.join(XENIA_CONFIG_MARKER), "").unwrap();
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
            explicit_configuration_roots: vec![root.0.clone()],
        });
        let profile = &discovery.profiles[0];
        assert!(
            profile.eligible,
            "a game with no patches installed yet must still work"
        );
        assert_eq!(profile.patches_state, XeniaPatchesDirectoryState::Missing);
    }

    #[test]
    fn a_nonexistent_explicit_directory_is_reported_as_missing() {
        let root = FixtureDir::new("missing");
        let missing = root.0.join("does-not-exist");
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
            explicit_configuration_roots: vec![missing],
        });
        assert_eq!(discovery.profiles.len(), 1);
        assert!(!discovery.profiles[0].eligible);
        assert_eq!(
            discovery.profiles[0].blockers[0].kind,
            XeniaProfileBlockerKind::MissingConfiguration
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_patches_directory_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = FixtureDir::new("symlinked-patches");
        fs::write(root.0.join(XENIA_CONFIG_MARKER), "").unwrap();
        let real = root.0.join("real-patches");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, root.0.join("patches")).unwrap();
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
            explicit_configuration_roots: vec![root.0.clone()],
        });
        let profile = &discovery.profiles[0];
        assert!(profile.eligible);
        assert_eq!(
            profile.patches_state,
            XeniaPatchesDirectoryState::UnsafePath
        );
    }

    #[test]
    fn no_explicit_roots_means_no_profiles_and_no_fabricated_native_guess() {
        let discovery = discover_xenia_profiles(&XeniaProfileDiscoveryRoots::default());
        assert!(discovery.profiles.is_empty());
        assert!(discovery.complete);
    }
}
