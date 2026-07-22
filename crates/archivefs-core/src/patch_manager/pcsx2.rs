use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::adapter::{
    AdapterCapabilities, AdapterId, AdapterIdentityEvidence, DiscoveryConfidence, EmulatorAdapter,
    InstallationCandidate,
};
use super::{CatalogueGameEvidence, PatchManagerError, PatchMetadataRecord, Result};

const ADAPTER_ID: AdapterId = "pcsx2";
const IDENTITY_NAMESPACE_SERIAL: &str = "ps2-serial";
const IDENTITY_NAMESPACE_EXECUTABLE_CRC: &str = "ps2-executable-crc";

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

fn candidate_kind_label(kind: Pcsx2CandidateKind) -> &'static str {
    match kind {
        Pcsx2CandidateKind::Native => "Native",
        Pcsx2CandidateKind::Flatpak => "Flatpak",
    }
}

impl<F: ReadOnlyFilesystem> EmulatorAdapter for ReadOnlyPcsx2Adapter<F> {
    fn id(&self) -> AdapterId {
        ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter_id: ADAPTER_ID,
            display_name: "PCSX2",
            identity_namespaces: &[IDENTITY_NAMESPACE_SERIAL, IDENTITY_NAMESPACE_EXECUTABLE_CRC],
            mutation_supported: false,
        }
    }

    fn discover_installations(&self) -> Result<Vec<InstallationCandidate>> {
        Ok(self
            .discover()?
            .into_iter()
            .map(|candidate| InstallationCandidate {
                adapter_id: ADAPTER_ID,
                kind: candidate_kind_label(candidate.kind).to_string(),
                data_root: candidate.data_root,
                provenance: candidate.provenance,
                discovery_confidence: match candidate.discovery_confidence {
                    Pcsx2DiscoveryConfidence::StandardPathCandidate => {
                        DiscoveryConfidence::StandardPathCandidate
                    }
                },
                detected_version: candidate.detected_version,
                mutation_readiness: candidate.mutation_readiness,
            })
            .collect())
    }

    fn identity_evidence_from_record(
        &self,
        record: &PatchMetadataRecord,
    ) -> Vec<AdapterIdentityEvidence> {
        let mut evidence = Vec::new();
        if let Some(serial) = &record.serial {
            evidence.push(AdapterIdentityEvidence {
                namespace: IDENTITY_NAMESPACE_SERIAL,
                value: serial.clone(),
                match_reason: "namespaced PCSX2 disc serial matches",
                conflict_reason: "catalogue disc serial conflicts with patch metadata",
            });
        }
        if let Some(crc) = &record.executable_crc {
            evidence.push(AdapterIdentityEvidence {
                namespace: IDENTITY_NAMESPACE_EXECUTABLE_CRC,
                value: crc.clone(),
                match_reason: "namespaced PCSX2 executable CRC matches",
                conflict_reason: "catalogue executable CRC conflicts with patch metadata",
            });
        }
        evidence
    }

    fn identity_evidence_from_catalogue(
        &self,
        game: &CatalogueGameEvidence,
    ) -> Vec<AdapterIdentityEvidence> {
        let mut evidence = Vec::new();
        if let Some(serial) = game.serial.as_deref().and_then(normalize_serial) {
            evidence.push(AdapterIdentityEvidence {
                namespace: IDENTITY_NAMESPACE_SERIAL,
                value: serial,
                match_reason: "namespaced PCSX2 disc serial matches",
                conflict_reason: "catalogue disc serial conflicts with patch metadata",
            });
        }
        if let Some(crc) = game.executable_crc.as_deref().and_then(normalize_crc) {
            evidence.push(AdapterIdentityEvidence {
                namespace: IDENTITY_NAMESPACE_EXECUTABLE_CRC,
                value: crc,
                match_reason: "namespaced PCSX2 executable CRC matches",
                conflict_reason: "catalogue executable CRC conflicts with patch metadata",
            });
        }
        evidence
    }

    fn hypothetical_relative_path(&self, record: &PatchMetadataRecord) -> Option<String> {
        validate_flat_pnach_repository_path(&record.repository_path)
    }
}

/// Accepts only the exact flat `patches/<filename>.pnach` shape PCSX2's own
/// repository actually uses, rejecting anything else as `None` rather than
/// reformatting or best-effort-repairing it. This function must never
/// assume a caller already validated its input the same way
/// `validate_repository_path` does at fetch time (in `patch_manager::mod`),
/// so it independently re-derives the same safety properties from
/// scratch: no traversal, no nested path, no absolute path, no backslash,
/// no empty filename, and the exact `.pnach` extension (case-sensitive,
/// matching the exact suffix check the metadata parser itself already
/// requires).
///
/// This only validates path *shape*. It deliberately does not reject or
/// escape shell metacharacters (`;`, `#`, `~`, spaces, ...) in an otherwise
/// well-shaped filename - nothing in this codebase ever passes a
/// hypothetical destination to a shell, a process, or a real filesystem
/// write, so there is no injection surface for those characters to exploit
/// here; treating them as unsafe would be a shape check this function does
/// not need.
fn validate_flat_pnach_repository_path(repository_path: &str) -> Option<String> {
    if repository_path.is_empty()
        || repository_path.contains('\0')
        || repository_path.starts_with('/')
        || repository_path.contains('\\')
    {
        return None;
    }
    let file_name = repository_path.strip_prefix("patches/")?;
    if file_name.is_empty()
        || file_name == ".pnach"
        || file_name == "."
        || file_name == ".."
        || file_name.contains('/')
        || file_name.contains('\\')
        || !file_name.ends_with(".pnach")
    {
        return None;
    }
    Some(format!("patches/{file_name}"))
}

/// Parses a `patches/<stem>.pnach` filename stem into a normalized PS2 disc
/// serial and/or executable CRC, exactly mirroring the upstream
/// `PCSX2/pcsx2_patches` naming convention (`<SERIAL>_<CRC>.pnach` or a
/// bare `<CRC>.pnach`). Moved here from `patch_manager::mod` - this is
/// PCSX2-only filename knowledge, not shared planning logic.
pub(super) fn parse_patch_identity(stem: &str) -> (Option<String>, Option<String>) {
    let mut parts = stem.rsplitn(2, '_');
    let possible_crc = parts.next().unwrap_or_default();
    let possible_serial = parts.next();
    let crc = normalize_crc(possible_crc);
    let serial = possible_serial.and_then(normalize_serial);
    match (serial, crc) {
        (Some(serial), Some(crc)) => (Some(serial), Some(crc)),
        (None, Some(crc)) if possible_serial.is_none() => (None, Some(crc)),
        _ => (None, None),
    }
}

/// Normalizes a PS2 disc serial to PCSX2's `XXXX-NNNNN` convention
/// (4 ASCII letters, a hyphen, 5 ASCII digits), uppercased. `None` if
/// `value` does not match that shape.
fn normalize_serial(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();
    let (prefix, digits) = value.split_once('-')?;
    if prefix.len() == 4
        && prefix
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        && digits.len() == 5
        && digits.chars().all(|character| character.is_ascii_digit())
    {
        Some(format!("{prefix}-{digits}"))
    } else {
        None
    }
}

/// Normalizes an 8-hex-digit PCSX2 executable CRC, uppercased. `None` if
/// `value` is not exactly 8 hex digits.
pub(super) fn normalize_crc(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 8 && value.chars().all(|character| character.is_ascii_hexdigit()))
        .then(|| value.to_ascii_uppercase())
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

    fn test_record(serial: Option<&str>, crc: Option<&str>) -> PatchMetadataRecord {
        PatchMetadataRecord {
            record_id: "patches/SLUS-20312_A1B2C3D4.pnach".to_string(),
            repository_path: "patches/SLUS-20312_A1B2C3D4.pnach".to_string(),
            patch_blob_id: "blob".to_string(),
            title: None,
            platform: "PS2".to_string(),
            region: None,
            serial: serial.map(str::to_string),
            executable_crc: crc.map(str::to_string),
            metadata_kind: "test fixture".to_string(),
        }
    }

    #[test]
    fn adapter_reports_a_stable_id_and_capabilities() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );

        assert_eq!(EmulatorAdapter::id(&adapter), "pcsx2");
        let capabilities = adapter.capabilities();
        assert_eq!(capabilities.adapter_id, "pcsx2");
        assert_eq!(capabilities.display_name, "PCSX2");
        assert_eq!(
            capabilities.identity_namespaces,
            &["ps2-serial", "ps2-executable-crc"]
        );
        assert!(!capabilities.mutation_supported);
    }

    #[test]
    fn discover_installations_matches_the_inherent_discover_method() {
        let home = PathBuf::from("/home/tester");
        let native = home.join(".config/PCSX2");
        let filesystem = RecordingFilesystem {
            directories: BTreeSet::from([native.clone()]),
            probes: Rc::new(RefCell::new(Vec::new())),
        };
        let adapter = ReadOnlyPcsx2Adapter::new(
            filesystem,
            Pcsx2DiscoveryRoots {
                home: home.clone(),
                xdg_config_home: home.join(".config"),
            },
        );

        let inherent = adapter.discover().unwrap();
        let neutral = adapter.discover_installations().unwrap();

        assert_eq!(inherent.len(), 1);
        assert_eq!(neutral.len(), 1);
        assert_eq!(neutral[0].adapter_id, "pcsx2");
        assert_eq!(neutral[0].kind, "Native");
        assert_eq!(neutral[0].data_root, inherent[0].data_root);
        assert_eq!(neutral[0].provenance, inherent[0].provenance);
        assert_eq!(
            neutral[0].discovery_confidence,
            DiscoveryConfidence::StandardPathCandidate
        );
        assert_eq!(neutral[0].detected_version, inherent[0].detected_version);
        assert_eq!(
            neutral[0].mutation_readiness,
            inherent[0].mutation_readiness
        );
    }

    /// End-to-end candidate ordering through the real discovery path: when
    /// both the native and Flatpak standard paths exist, the neutral trait
    /// method yields Native first, then Flatpak - the same order the
    /// pre-extraction `Pcsx2CandidateKind`'s derived `Ord` (`Native`
    /// declared before `Flatpak`) already produced via `.sort()`.
    #[test]
    fn discover_installations_yields_native_before_flatpak_when_both_exist() {
        let home = PathBuf::from("/home/tester");
        let native = home.join(".config/PCSX2");
        let flatpak = home.join(".var/app/net.pcsx2.PCSX2/config/PCSX2");
        let filesystem = RecordingFilesystem {
            directories: BTreeSet::from([native.clone(), flatpak.clone()]),
            probes: Rc::new(RefCell::new(Vec::new())),
        };
        let adapter = ReadOnlyPcsx2Adapter::new(
            filesystem,
            Pcsx2DiscoveryRoots {
                home: home.clone(),
                xdg_config_home: home.join(".config"),
            },
        );

        let candidates = adapter.discover_installations().unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].kind, "Native");
        assert_eq!(candidates[0].data_root, native);
        assert_eq!(candidates[1].kind, "Flatpak");
        assert_eq!(candidates[1].data_root, flatpak);
    }

    #[test]
    fn identity_evidence_from_record_preserves_pre_normalized_serial_and_crc() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );
        let record = test_record(Some("SLUS-20312"), Some("A1B2C3D4"));

        let evidence = adapter.identity_evidence_from_record(&record);

        assert_eq!(evidence.len(), 2);
        assert!(
            evidence
                .iter()
                .any(|item| item.namespace == "ps2-serial" && item.value == "SLUS-20312")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.namespace == "ps2-executable-crc" && item.value == "A1B2C3D4")
        );
    }

    #[test]
    fn identity_evidence_from_catalogue_normalizes_like_before_extraction() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );
        let game = CatalogueGameEvidence {
            archive_id: 1,
            is_present: true,
            display_name: "Example".to_string(),
            normalized_name: "example".to_string(),
            platform: Some("PS2".to_string()),
            region: None,
            serial: Some("slus-20312".to_string()),
            executable_crc: Some("a1b2c3d4".to_string()),
        };

        let evidence = adapter.identity_evidence_from_catalogue(&game);

        assert!(
            evidence
                .iter()
                .any(|item| item.namespace == "ps2-serial" && item.value == "SLUS-20312")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.namespace == "ps2-executable-crc" && item.value == "A1B2C3D4")
        );
    }

    #[test]
    fn hypothetical_relative_path_reproduces_the_original_patches_prefix_calculation() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );
        let record = test_record(Some("SLUS-20312"), Some("A1B2C3D4"));

        assert_eq!(
            adapter.hypothetical_relative_path(&record),
            Some("patches/SLUS-20312_A1B2C3D4.pnach".to_string())
        );
    }

    /// `hypothetical_relative_path` independently rejects (fail-closed,
    /// `None`) any `repository_path` that is not the exact flat
    /// `patches/<filename>.pnach` shape - traversal, nesting, absolute
    /// paths, backslashes, empty filenames, and wrong extensions are all
    /// rejected here even though `validate_repository_path` already
    /// rejects most of them at fetch time (see
    /// `unsafe_repository_paths_are_rejected_not_rendered` in
    /// `patch_manager::mod`). This function must never assume a caller
    /// already validated its input.
    #[test]
    fn hypothetical_relative_path_rejects_unsafe_or_malformed_repository_paths() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );
        for unsafe_path in [
            "patches/../../etc/passwd.pnach", // traversal
            "patches/../evil.pnach",          // traversal, shorter
            "patches/sub/evil.pnach",         // nested path
            "/patches/evil.pnach",            // absolute path
            "patches\\evil.pnach",            // backslash, not a real prefix match
            "patches/sub\\evil.pnach",        // backslash inside the filename
            "patches/.pnach",                 // empty filename (extension only)
            "patches/",                       // empty filename (nothing at all)
            "patches",                        // no trailing slash at all
            "patches/.",                      // filename is exactly "."
            "patches/..",                     // filename is exactly ".."
            "patches/evil.exe",               // wrong extension
            "patches/evil.PNACH",             // wrong extension (case-sensitive)
            "patches/evil.pnach/",            // trailing slash after the filename
            "not-under-patches-at-all.pnach", // not under patches/ at all
            "",                               // empty repository_path
        ] {
            let mut record = test_record(None, None);
            record.repository_path = unsafe_path.to_string();
            assert!(
                adapter.hypothetical_relative_path(&record).is_none(),
                "expected {unsafe_path:?} to be rejected, not silently reformatted"
            );
        }
    }

    /// The only path shape this function accepts is a flat
    /// `patches/<filename>.pnach` record path, returned unchanged. A
    /// filename containing shell metacharacters is still accepted - this
    /// function validates path *shape* only; it never shell-escapes
    /// content, since nothing in this codebase ever passes a hypothetical
    /// destination to a shell, a process, or a real filesystem write.
    #[test]
    fn hypothetical_relative_path_accepts_only_the_flat_patches_pnach_shape() {
        let adapter = ReadOnlyPcsx2Adapter::new(
            HostReadOnlyFilesystem,
            Pcsx2DiscoveryRoots {
                home: PathBuf::from("/unused"),
                xdg_config_home: PathBuf::from("/unused/.config"),
            },
        );

        let mut record = test_record(Some("SLUS-20312"), Some("A1B2C3D4"));
        record.repository_path = "patches/SLUS-20312_A1B2C3D4.pnach".to_string();
        assert_eq!(
            adapter.hypothetical_relative_path(&record),
            Some("patches/SLUS-20312_A1B2C3D4.pnach".to_string())
        );

        let mut weird = test_record(None, None);
        weird.repository_path = "patches/; rm -rf ~ #.pnach".to_string();
        assert_eq!(
            adapter.hypothetical_relative_path(&weird),
            Some("patches/; rm -rf ~ #.pnach".to_string())
        );
    }
}
