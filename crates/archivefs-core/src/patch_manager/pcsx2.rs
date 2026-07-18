use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{PatchManagerError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Pcsx2CandidateKind {
    Native,
    Flatpak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Pcsx2DiscoveryConfidence {
    StandardPathCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Pcsx2InstallationCandidate {
    pub kind: Pcsx2CandidateKind,
    pub data_root: PathBuf,
    pub provenance: &'static str,
    pub discovery_confidence: Pcsx2DiscoveryConfidence,
    pub detected_version: Option<String>,
    pub mutation_readiness: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2DiscoveryRoots {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
}

impl Pcsx2DiscoveryRoots {
    pub fn from_environment() -> Result<Self> {
        Self::from_values(
            env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")),
            env::var_os("XDG_CONFIG_HOME"),
        )
    }

    fn from_values(home: Option<OsString>, xdg_config_home: Option<OsString>) -> Result<Self> {
        let home = home
            .map(PathBuf::from)
            .ok_or_else(|| PatchManagerError::Discovery("HOME is not set".to_string()))?;
        let xdg_config_home = xdg_config_home
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            home,
            xdg_config_home,
        })
    }
}

/// Read-only filesystem capability used by PCSX2 discovery. It deliberately
/// has no directory-listing, creation, writability-probe, or mutation method.
pub trait ReadOnlyFilesystem {
    fn is_directory_no_follow(&self, path: &Path) -> io::Result<bool>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HostReadOnlyFilesystem;

impl ReadOnlyFilesystem for HostReadOnlyFilesystem {
    fn is_directory_no_follow(&self, path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(metadata.file_type().is_dir()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Phase 1 PCSX2 adapter. It probes only two documented configuration/data
/// roots and never examines `cheats`, `patches`, or their contents.
#[derive(Debug, Clone)]
pub struct ReadOnlyPcsx2Adapter<F = HostReadOnlyFilesystem> {
    filesystem: F,
    roots: Pcsx2DiscoveryRoots,
}

impl ReadOnlyPcsx2Adapter<HostReadOnlyFilesystem> {
    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots::from_environment()?,
        ))
    }
}

impl<F: ReadOnlyFilesystem> ReadOnlyPcsx2Adapter<F> {
    pub fn new(filesystem: F, roots: Pcsx2DiscoveryRoots) -> Self {
        Self { filesystem, roots }
    }

    pub fn discover(&self) -> Result<Vec<Pcsx2InstallationCandidate>> {
        let candidates = [
            (
                Pcsx2CandidateKind::Native,
                self.roots.xdg_config_home.join("PCSX2"),
                "XDG PCSX2 configuration directory",
            ),
            (
                Pcsx2CandidateKind::Flatpak,
                self.roots
                    .home
                    .join(".var/app/net.pcsx2.PCSX2/config/PCSX2"),
                "Flatpak net.pcsx2.PCSX2 configuration directory",
            ),
        ];
        let mut seen = BTreeSet::new();
        let mut candidates_found = Vec::new();
        for (kind, path, provenance) in candidates {
            if !seen.insert(path.clone()) {
                continue;
            }
            let exists = self
                .filesystem
                .is_directory_no_follow(&path)
                .map_err(|error| {
                    PatchManagerError::Discovery(format!(
                        "failed to inspect PCSX2 candidate {}: {error}",
                        path.display()
                    ))
                })?;
            if exists {
                candidates_found.push(Pcsx2InstallationCandidate {
                    kind,
                    data_root: path,
                    provenance,
                    discovery_confidence: Pcsx2DiscoveryConfidence::StandardPathCandidate,
                    detected_version: None,
                    mutation_readiness: "NotEvaluated",
                });
            }
        }
        candidates_found.sort();
        Ok(candidates_found)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone)]
    struct RecordingFilesystem {
        directories: BTreeSet<PathBuf>,
        probes: Rc<RefCell<Vec<PathBuf>>>,
    }

    impl ReadOnlyFilesystem for RecordingFilesystem {
        fn is_directory_no_follow(&self, path: &Path) -> io::Result<bool> {
            self.probes.borrow_mut().push(path.to_path_buf());
            Ok(self.directories.contains(path))
        }
    }

    #[test]
    fn native_and_flatpak_discovery_probe_only_their_roots() {
        let home = PathBuf::from("/home/tester");
        let native = home.join(".config/PCSX2");
        let flatpak = home.join(".var/app/net.pcsx2.PCSX2/config/PCSX2");
        let probes = Rc::new(RefCell::new(Vec::new()));
        let filesystem = RecordingFilesystem {
            directories: BTreeSet::from([native.clone(), flatpak.clone()]),
            probes: Rc::clone(&probes),
        };
        let adapter = ReadOnlyPcsx2Adapter::new(
            filesystem,
            Pcsx2DiscoveryRoots {
                home: home.clone(),
                xdg_config_home: home.join(".config"),
            },
        );

        let candidates = adapter.discover().unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(&*probes.borrow(), &[native, flatpak]);
        assert!(
            probes
                .borrow()
                .iter()
                .all(|path| !path.components().any(|part| part.as_os_str() == "cheats"))
        );
    }

    #[test]
    fn discovery_does_not_create_missing_native_or_flatpak_directories() {
        let root =
            env::temp_dir().join(format!("archivefs-pcsx2-discovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: root.clone(),
                xdg_config_home: root.join("config"),
            },
        );

        assert!(adapter.discover().unwrap().is_empty());
        assert!(!root.join("config").exists());
        assert!(!root.join(".var").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovery_ignores_final_component_symlink_candidates() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let root =
                env::temp_dir().join(format!("archivefs-pcsx2-symlink-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("real")).unwrap();
            fs::create_dir_all(root.join("config")).unwrap();
            symlink(root.join("real"), root.join("config/PCSX2")).unwrap();
            let adapter = ReadOnlyPcsx2Adapter::new(
                HostReadOnlyFilesystem,
                Pcsx2DiscoveryRoots {
                    home: root.clone(),
                    xdg_config_home: root.join("config"),
                },
            );

            assert!(adapter.discover().unwrap().is_empty());
            let _ = fs::remove_dir_all(root);
        }
    }
}
