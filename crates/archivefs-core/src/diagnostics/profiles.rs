//! Read-only writability assessment for discovered emulator profiles.
//!
//! Doctor already knows which profiles exist: `discover_dolphin_profiles`,
//! `discover_pcsx2_profiles` and `discover_xenia_profiles` do that, and each
//! already reports its own blockers. This module answers the one question none
//! of them answers: **could ArchiveFS write an install into that profile?**
//!
//! It answers it from metadata only. Nothing here creates a directory, a
//! `GameSettings` file, a `.pnach`, a `.patch.toml`, a config file or a probe
//! file, and nothing changes a permission bit. The strongest verdict available
//! is therefore [`WritabilityAssessment::AppearsWritable`] - see that type for
//! why "writable: yes" is never claimed.
//!
//! ## Scope
//!
//! Only the three adapters ArchiveFS already supports, through their existing
//! discovery abstractions. No emulator gains support here. Flatpak profiles
//! are assessed exactly like native ones, which is honest but incomplete: a
//! portal can still refuse a write that the bits allow, so the narrowed
//! deferred entry in `DEFERRED_CHECKS` says so.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::environment::{
    MountEntry, MountMode, PathPermissions, WritabilityAssessment, assess_permissions,
    assess_writability, mount_entry_for_path,
};
use super::managed::{ManagedFormat, ManagedScanTarget};
use super::{
    DoctorCategory, DoctorSeverity, DoctorSubsystem, Finding, Measurement, NotCheckedCheck,
};
use crate::emulator_environment::EncodedPath;
use crate::patch_manager::{
    DolphinProfileDiscovery, DolphinProfileDiscoveryRoots, Pcsx2ProfileDiscovery,
    Pcsx2ProfileDiscoveryRoots, XeniaProfileDiscovery, XeniaProfileDiscoveryRoots,
    discover_dolphin_profiles, discover_pcsx2_profiles, discover_xenia_profiles,
};

/// At most this many profiles are reported individually; beyond it Doctor
/// summarises. A machine with dozens of Flatpak and portable installs must not
/// bury every other result.
pub const MAX_INDIVIDUAL_PROFILE_FINDINGS: usize = 12;

/// Which emulator a profile belongs to. Only adapters ArchiveFS already
/// supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatorKind {
    Dolphin,
    Pcsx2,
    Xenia,
}

impl EmulatorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dolphin => "Dolphin",
            Self::Pcsx2 => "PCSX2",
            Self::Xenia => "Xenia Canary",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Dolphin => "dolphin",
            Self::Pcsx2 => "pcsx2",
            Self::Xenia => "xenia",
        }
    }

    /// The ambiguity finding is per emulator. A shared id would let two
    /// emulators' results merge into one, because neither names a single
    /// affected path - which is exactly what a live scan showed happening.
    fn ambiguous_profile_finding_id(self) -> &'static str {
        match self {
            Self::Dolphin => "emulator_profile.ambiguous_preferred_dolphin_profile",
            Self::Pcsx2 => "emulator_profile.ambiguous_preferred_pcsx2_profile",
            Self::Xenia => "emulator_profile.ambiguous_preferred_xenia_profile",
        }
    }
}

/// One profile's assessment. Everything the GUI and the CLI need, with the
/// evidence that produced each conclusion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileAssessment {
    pub emulator: EmulatorKind,
    pub profile_id: String,
    /// The adapter's own installation-type label - native, Flatpak, AppImage,
    /// portable - taken verbatim rather than reclassified here.
    pub profile_kind: String,
    pub scope: String,
    /// The adapter's own provenance string: how this profile was discovered.
    pub discovery_confidence: &'static str,
    /// Whether the adapter itself considers this profile usable.
    pub eligible: bool,
    /// The adapter's own blockers, if any.
    pub blockers: Vec<String>,
    pub root_path: EncodedPath,
    /// The one directory ArchiveFS would write into for this adapter.
    pub destination_path: EncodedPath,
    pub destination_exists: bool,
    pub destination_is_directory: bool,
    pub destination_is_symlink: bool,
    pub mount_mode: MountMode,
    pub permissions: Option<PathPermissions>,
    pub writability: WritabilityAssessment,
    /// Set when ArchiveFS already knows this is the selected/remembered
    /// profile for this adapter.
    pub preferred: Option<bool>,
}

impl ProfileAssessment {
    fn finding_id(&self) -> &'static str {
        match self.writability {
            WritabilityAssessment::MissingDestination => "emulator_profile.missing_destination",
            WritabilityAssessment::ReadOnlyFilesystem => "emulator_profile.read_only_filesystem",
            WritabilityAssessment::PermissionDenied => "emulator_profile.permission_denied",
            WritabilityAssessment::UnsafeDestination => "emulator_profile.unsafe_destination",
            WritabilityAssessment::NotProven => "emulator_profile.writability_not_proven",
            WritabilityAssessment::AppearsWritable => "emulator_profile.appears_writable",
        }
    }

    fn severity(&self) -> Option<DoctorSeverity> {
        match self.writability {
            // Nothing to report: this is the ordinary healthy case.
            WritabilityAssessment::AppearsWritable => None,
            // ArchiveFS creates the destination during install, so its absence
            // is informational, not a fault.
            WritabilityAssessment::MissingDestination => Some(DoctorSeverity::Info),
            // An install into this profile would fail.
            WritabilityAssessment::ReadOnlyFilesystem
            | WritabilityAssessment::PermissionDenied
            | WritabilityAssessment::UnsafeDestination => Some(DoctorSeverity::Warning),
            // Honest uncertainty, not a problem.
            WritabilityAssessment::NotProven => Some(DoctorSeverity::Info),
        }
    }

    fn evidence(&self) -> Vec<String> {
        let mut evidence = vec![
            format!("Emulator: {}", self.emulator.label()),
            format!("Profile type: {}", self.profile_kind),
            format!("Scope: {}", self.scope),
            format!("How it was found: {}", self.discovery_confidence),
            format!("Profile root: {}", self.root_path.display),
            format!("Destination: {}", self.destination_path.display),
            format!("Destination exists: {}", self.destination_exists),
            format!("Mount state: {}", self.mount_mode.label()),
            format!("Assessment: {}", self.writability.label()),
        ];
        if let Some(preferred) = self.preferred {
            evidence.push(format!("Currently selected profile: {preferred}"));
        }
        evidence.push(format!("Adapter considers it usable: {}", self.eligible));
        match &self.permissions {
            Some(permissions) => {
                if let Some(mode) = permissions.mode {
                    evidence.push(format!("Permissions: {:o}", mode & 0o7777));
                }
                if let Some(owned) = permissions.owned_by_current_user {
                    evidence.push(format!("Owned by the current user: {owned}"));
                }
                if let Some(may_write) = permissions.current_user_may_write {
                    evidence.push(format!(
                        "Permission bits allow this user to write: {may_write}"
                    ));
                }
            }
            None => evidence.push(
                "Permissions: not readable, so they were not used in this assessment".to_string(),
            ),
        }
        for blocker in &self.blockers {
            evidence.push(format!("Adapter blocker: {blocker}"));
        }
        evidence
    }
}

/// Everything Doctor could work out about emulator profiles.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileAssessmentReport {
    pub profiles: Vec<ProfileAssessment>,
    /// Adapters whose discovery failed or was not run, and why. Never
    /// presented as "no profiles".
    pub unavailable: Vec<(EmulatorKind, String)>,
    /// `true` when an adapter reported its own discovery as incomplete.
    pub discovery_incomplete: bool,
}

/// The already-completed discoveries Doctor assesses. Borrowed, and each
/// optional, so one adapter failing never stops the others.
#[derive(Debug, Default)]
pub struct ProfileDiscoveries<'a> {
    pub dolphin: Option<&'a DolphinProfileDiscovery>,
    pub dolphin_error: Option<String>,
    pub pcsx2: Option<&'a Pcsx2ProfileDiscovery>,
    pub pcsx2_error: Option<String>,
    pub xenia: Option<&'a XeniaProfileDiscovery>,
    pub xenia_error: Option<String>,
    /// The profile ids ArchiveFS currently prefers, when known.
    pub preferred_dolphin: Option<&'a str>,
    pub preferred_pcsx2: Option<&'a str>,
    pub preferred_xenia: Option<&'a str>,
}

/// Assesses every discovered profile's write destination.
///
/// Read-only: `symlink_metadata` for existence and permissions, one already
/// read mount table for mount mode. Nothing is created and no permission is
/// changed.
pub fn assess_emulator_profiles(
    discoveries: &ProfileDiscoveries<'_>,
    mount_table: Option<&[MountEntry]>,
) -> ProfileAssessmentReport {
    let mut profiles = Vec::new();
    let mut unavailable = Vec::new();
    let mut discovery_incomplete = false;

    if let Some(discovery) = discoveries.dolphin {
        discovery_incomplete |= !discovery.complete;
        for profile in &discovery.profiles {
            profiles.push(assess_one(
                EmulatorKind::Dolphin,
                profile.profile_id.clone(),
                format!("{:?}", profile.installation_type),
                format!("{:?}", profile.scope),
                profile.provenance,
                profile.eligible,
                profile
                    .blockers
                    .iter()
                    .map(|blocker| format!("{:?}: {}", blocker.kind, blocker.detail))
                    .collect(),
                &profile.configuration_path,
                // The one directory ArchiveFS writes into for Dolphin.
                &profile.game_settings_path,
                discoveries.preferred_dolphin,
                mount_table,
            ));
        }
    } else if let Some(error) = &discoveries.dolphin_error {
        unavailable.push((EmulatorKind::Dolphin, error.clone()));
    }

    if let Some(discovery) = discoveries.pcsx2 {
        discovery_incomplete |= !discovery.complete;
        for profile in &discovery.profiles {
            // PCSX2 can expose several patch directories; assess each, since
            // an install can target any of them.
            for directory in &profile.patch_directories {
                profiles.push(assess_one(
                    EmulatorKind::Pcsx2,
                    profile.profile_id.clone(),
                    format!("{:?}", profile.installation_type),
                    format!("{:?}", profile.scope),
                    profile.provenance,
                    profile.eligible,
                    profile
                        .blockers
                        .iter()
                        .map(|blocker| format!("{:?}: {}", blocker.kind, blocker.detail))
                        .collect(),
                    &profile.configuration_path,
                    &directory.path,
                    discoveries.preferred_pcsx2,
                    mount_table,
                ));
            }
            if profile.patch_directories.is_empty() {
                profiles.push(assess_one(
                    EmulatorKind::Pcsx2,
                    profile.profile_id.clone(),
                    format!("{:?}", profile.installation_type),
                    format!("{:?}", profile.scope),
                    profile.provenance,
                    profile.eligible,
                    profile
                        .blockers
                        .iter()
                        .map(|blocker| format!("{:?}: {}", blocker.kind, blocker.detail))
                        .collect(),
                    &profile.configuration_path,
                    // No patch directory was discovered; report the profile
                    // root so the finding still names something real.
                    &profile.configuration_path,
                    discoveries.preferred_pcsx2,
                    mount_table,
                ));
            }
        }
    } else if let Some(error) = &discoveries.pcsx2_error {
        unavailable.push((EmulatorKind::Pcsx2, error.clone()));
    }

    if let Some(discovery) = discoveries.xenia {
        discovery_incomplete |= !discovery.complete;
        for profile in &discovery.profiles {
            profiles.push(assess_one(
                EmulatorKind::Xenia,
                profile.profile_id.clone(),
                format!("{:?}", profile.installation_type),
                format!("{:?}", profile.scope),
                profile.provenance,
                profile.eligible,
                profile
                    .blockers
                    .iter()
                    .map(|blocker| format!("{:?}: {}", blocker.kind, blocker.detail))
                    .collect(),
                &profile.configuration_path,
                &profile.patches_path,
                discoveries.preferred_xenia,
                mount_table,
            ));
        }
    } else if let Some(error) = &discoveries.xenia_error {
        unavailable.push((EmulatorKind::Xenia, error.clone()));
    }

    profiles.sort_by(|left, right| {
        (
            left.emulator,
            &left.profile_id,
            &left.destination_path.display,
        )
            .cmp(&(
                right.emulator,
                &right.profile_id,
                &right.destination_path.display,
            ))
    });
    ProfileAssessmentReport {
        profiles,
        unavailable,
        discovery_incomplete,
    }
}

#[allow(clippy::too_many_arguments)]
fn assess_one(
    emulator: EmulatorKind,
    profile_id: String,
    profile_kind: String,
    scope: String,
    discovery_confidence: &'static str,
    eligible: bool,
    blockers: Vec<String>,
    root_path: &Path,
    destination_path: &PathBuf,
    preferred_profile_id: Option<&str>,
    mount_table: Option<&[MountEntry]>,
) -> ProfileAssessment {
    // `symlink_metadata`, never `metadata`: a symlinked destination must be
    // reported as unsafe rather than silently followed.
    let metadata = fs::symlink_metadata(destination_path).ok();
    let destination_exists = metadata.is_some();
    let destination_is_symlink = metadata
        .as_ref()
        .is_some_and(|value| value.file_type().is_symlink());
    let destination_is_directory = metadata.as_ref().is_some_and(fs::Metadata::is_dir);
    let mount_mode = match mount_table {
        Some(table) => mount_entry_for_path(table, destination_path)
            .map_or(MountMode::Unknown, MountEntry::mode),
        None => MountMode::Unknown,
    };
    let permissions = destination_exists
        .then(|| assess_permissions(destination_path))
        .flatten();
    let writability = assess_writability(
        destination_exists,
        destination_is_directory,
        destination_is_symlink,
        mount_mode,
        permissions,
    );
    ProfileAssessment {
        emulator,
        preferred: preferred_profile_id.map(|preferred| preferred == profile_id),
        profile_id,
        profile_kind,
        scope,
        discovery_confidence,
        eligible,
        blockers,
        root_path: EncodedPath::from_path(root_path),
        destination_path: EncodedPath::from_path(destination_path),
        destination_exists,
        destination_is_directory,
        destination_is_symlink,
        mount_mode,
        permissions,
        writability,
    }
}

/// Profile findings: one per profile that has something to report, plus a
/// summary when there are too many, plus an ambiguity finding when several
/// eligible profiles compete and none is selected.
///
/// A profile that appears writable produces no finding at all - Doctor reports
/// problems, not an inventory.
pub fn findings_from_emulator_profiles(report: &ProfileAssessmentReport) -> Vec<Finding> {
    let mut findings = Vec::new();
    let notable: Vec<&ProfileAssessment> = report
        .profiles
        .iter()
        .filter(|profile| profile.severity().is_some())
        .collect();

    if notable.len() <= MAX_INDIVIDUAL_PROFILE_FINDINGS {
        for profile in &notable {
            let severity = profile.severity().expect("filtered above");
            findings.push(
                Finding::new(
                    profile.finding_id(),
                    DoctorCategory::EmulatorProfiles,
                    DoctorSubsystem::EmulatorProfiles,
                    severity,
                    format!("{} profile: {}", profile.emulator.label(), profile.writability.label()),
                    format!(
                        "{} ({}) would install into {}. {}",
                        profile.emulator.label(),
                        profile.profile_kind,
                        profile.destination_path.display,
                        profile.writability.label()
                    ),
                )
                .with_affected(profile.destination_path.clone())
                .with_evidence(profile.evidence())
                .with_measurements([
                    ("emulator", Measurement::text(profile.emulator.label())),
                    ("profile_kind", Measurement::text(&profile.profile_kind)),
                    (
                        "discovery_confidence",
                        Measurement::text(profile.discovery_confidence),
                    ),
                    (
                        "writability_assessment",
                        Measurement::text(profile.writability.label()),
                    ),
                    (
                        "filesystem_read_only",
                        Measurement::Flag(
                            profile.writability == WritabilityAssessment::ReadOnlyFilesystem,
                        ),
                    ),
                    (
                        "destination_exists",
                        Measurement::Flag(profile.destination_exists),
                    ),
                    ("eligible", Measurement::Flag(profile.eligible)),
                ])
                .with_guidance(
                    match profile.writability {
                        WritabilityAssessment::ReadOnlyFilesystem =>
                            "Installing a cheat or patch into this profile would fail, because the filesystem itself is read-only.",
                        WritabilityAssessment::PermissionDenied =>
                            "Installing a cheat or patch into this profile would fail with a permission error.",
                        WritabilityAssessment::MissingDestination =>
                            "ArchiveFS creates this directory during an install, so this is only worth knowing about in advance.",
                        WritabilityAssessment::UnsafeDestination =>
                            "ArchiveFS refuses to write through a symlink or into a non-directory, so this profile cannot be used as-is.",
                        WritabilityAssessment::NotProven | WritabilityAssessment::AppearsWritable =>
                            "ArchiveFS cannot confirm from metadata alone whether a write here would succeed, and will not write a test file to find out.",
                    },
                    match profile.writability {
                        WritabilityAssessment::ReadOnlyFilesystem =>
                            "Remount that filesystem read-write, or pick a different profile before installing.",
                        WritabilityAssessment::PermissionDenied =>
                            "Give the current user write access to that directory, or pick a different profile.",
                        WritabilityAssessment::MissingDestination =>
                            "Nothing to do now. Launch the emulator once, or let ArchiveFS create it during an install.",
                        WritabilityAssessment::UnsafeDestination =>
                            "Replace the symlink with a real directory, or point ArchiveFS at a different profile.",
                        WritabilityAssessment::NotProven | WritabilityAssessment::AppearsWritable =>
                            "No action needed. An install will report the real outcome.",
                    },
                ),
            );
        }
    } else {
        let mut evidence: Vec<String> = notable
            .iter()
            .take(MAX_INDIVIDUAL_PROFILE_FINDINGS)
            .map(|profile| {
                format!(
                    "{} ({}): {} - {}",
                    profile.emulator.label(),
                    profile.profile_kind,
                    profile.destination_path.display,
                    profile.writability.label()
                )
            })
            .collect();
        evidence.push(format!(
            "... and {} more",
            notable.len() - MAX_INDIVIDUAL_PROFILE_FINDINGS
        ));
        evidence.push(format!(
            "Individual profiles are not listed separately above {MAX_INDIVIDUAL_PROFILE_FINDINGS}, so this one result covers them all."
        ));
        findings.push(
            Finding::new(
                "emulator_profile.multiple_need_attention",
                DoctorCategory::EmulatorProfiles,
                DoctorSubsystem::EmulatorProfiles,
                notable
                    .iter()
                    .filter_map(|profile| profile.severity())
                    .min_by_key(|severity| severity.rank())
                    .unwrap_or(DoctorSeverity::Info),
                "Several emulator profiles need attention",
                format!(
                    "{} discovered emulator profiles have a destination ArchiveFS could not confirm it can write to.",
                    notable.len()
                ),
            )
            .with_evidence(evidence)
            .with_measurements([
                ("profiles_needing_attention", Measurement::Integer(notable.len() as u64)),
                (
                    "individual_findings_truncated",
                    Measurement::Flag(true),
                ),
                (
                    "individual_finding_limit",
                    Measurement::Integer(MAX_INDIVIDUAL_PROFILE_FINDINGS as u64),
                ),
            ]),
        );
    }

    // Several eligible, apparently writable profiles with none selected is
    // genuinely ambiguous: an install would have to ask.
    for emulator in [
        EmulatorKind::Dolphin,
        EmulatorKind::Pcsx2,
        EmulatorKind::Xenia,
    ] {
        let candidates: Vec<&ProfileAssessment> = report
            .profiles
            .iter()
            .filter(|profile| {
                profile.emulator == emulator
                    && profile.eligible
                    && profile.writability == WritabilityAssessment::AppearsWritable
            })
            .collect();
        let none_selected = candidates
            .iter()
            .all(|profile| profile.preferred != Some(true));
        if candidates.len() > 1 && none_selected {
            findings.push(
                Finding::new(
                    emulator.ambiguous_profile_finding_id(),
                    DoctorCategory::EmulatorProfiles,
                    DoctorSubsystem::EmulatorProfiles,
                    DoctorSeverity::Info,
                    format!("More than one {} profile could be used", emulator.label()),
                    format!(
                        "{} usable {} profiles were found and none is selected, so ArchiveFS will ask before installing.",
                        candidates.len(),
                        emulator.label()
                    ),
                )
                .with_evidence(
                    candidates
                        .iter()
                        .map(|profile| {
                            format!(
                                "{} ({}): {}",
                                profile.profile_id,
                                profile.profile_kind,
                                profile.destination_path.display
                            )
                        })
                        .collect::<Vec<_>>(),
                )
                .with_guidance(
                    "This is not a fault. ArchiveFS never picks an emulator profile for you when there is real ambiguity.",
                    "Choose a profile in the Cheats and Mods workflow to have it remembered.",
                ),
            );
        }
    }
    findings
}

/// Adapters whose profiles could not be assessed, and the sandbox caveat that
/// applies even when they could.
pub fn not_checked_from_emulator_profiles(
    report: &ProfileAssessmentReport,
) -> Vec<NotCheckedCheck> {
    let mut items: Vec<NotCheckedCheck> = report
        .unavailable
        .iter()
        .map(|(emulator, reason)| NotCheckedCheck {
            name: format!("{} profiles", emulator.label()),
            reason: reason.clone(),
            next_step:
                "Rescan profiles from the Cheats and Mods page, or set an explicit directory."
                    .to_string(),
        })
        .collect();
    if report.discovery_incomplete {
        items.push(NotCheckedCheck {
            name: "Complete emulator profile discovery".to_string(),
            reason: "One adapter reported its own discovery as incomplete, so some profiles may be missing from this result.".to_string(),
            next_step: "Rescan profiles from the Cheats and Mods page.".to_string(),
        });
    }
    // Stated on every run: this is the honest limit of a metadata-only check.
    if report.profiles.iter().any(|profile| {
        profile
            .profile_kind
            .to_ascii_lowercase()
            .contains("flatpak")
    }) {
        items.push(NotCheckedCheck {
            name: "Flatpak sandbox write permission".to_string(),
            reason: "A Flatpak profile's real writability depends on portal and sandbox permissions, which cannot be read from file metadata. ArchiveFS reports what the permissions and mount state say, and no more.".to_string(),
            next_step: "No action needed. An install will report the real outcome.".to_string(),
        });
    }
    items
}

/// A stable, machine-readable form of one profile for `--json`, so a script
/// gets values rather than prose.
pub fn profile_json_summary(profile: &ProfileAssessment) -> serde_json::Value {
    serde_json::json!({
        "emulator": profile.emulator,
        "profile_id": profile.profile_id,
        "profile_kind": profile.profile_kind,
        "scope": profile.scope,
        "discovery_confidence": profile.discovery_confidence,
        "eligible": profile.eligible,
        "root_path": profile.root_path,
        "destination_path": profile.destination_path,
        "destination_exists": profile.destination_exists,
        "filesystem_read_only": profile.mount_mode == MountMode::ReadOnly,
        "mount_mode": profile.mount_mode,
        "writability_assessment": profile.writability,
        "preferred": profile.preferred,
        "finding_id": profile.finding_id(),
    })
}

impl EmulatorKind {
    /// Namespace fragment for ids, kept next to the labels so the two cannot
    /// drift.
    pub fn id_fragment(self) -> &'static str {
        self.slug()
    }
}

// --- Discovery gatherer ---------------------------------------------------

/// Owned profile discoveries, so a caller can gather once and then borrow for
/// the pure runner.
///
/// Each adapter is a separate `Result`: one emulator's discovery failing must
/// never hide another's, and "not discoverable" is reported rather than being
/// silently treated as "no profiles".
#[derive(Debug)]
pub struct DiscoveredProfiles {
    pub dolphin: Result<DolphinProfileDiscovery, String>,
    pub pcsx2: Result<Pcsx2ProfileDiscovery, String>,
    /// Xenia has no documented native configuration path, so it is only ever
    /// discovered from roots the user has already pointed ArchiveFS at. With
    /// none supplied there is nothing to assess - not a failure.
    pub xenia: Option<XeniaProfileDiscovery>,
}

impl DiscoveredProfiles {
    /// Discovers Dolphin and PCSX2 profiles from the documented paths, plus
    /// Xenia from the supplied explicit roots only.
    ///
    /// Read-only: each adapter's discovery inspects metadata of documented
    /// paths and never creates a directory or a profile.
    pub fn from_environment(explicit_xenia_roots: Vec<PathBuf>) -> Self {
        let dolphin = DolphinProfileDiscoveryRoots::from_environment()
            .map_err(|error| format!("Dolphin profiles could not be discovered: {error}"))
            .and_then(|roots| {
                discover_dolphin_profiles(&roots)
                    .map_err(|error| format!("Dolphin profiles could not be discovered: {error}"))
            });
        let pcsx2 = Pcsx2ProfileDiscoveryRoots::from_environment()
            .map_err(|error| format!("PCSX2 profiles could not be discovered: {error}"))
            .and_then(|roots| {
                discover_pcsx2_profiles(&roots)
                    .map_err(|error| format!("PCSX2 profiles could not be discovered: {error}"))
            });
        let xenia = if explicit_xenia_roots.is_empty() {
            None
        } else {
            Some(discover_xenia_profiles(&XeniaProfileDiscoveryRoots {
                explicit_configuration_roots: explicit_xenia_roots,
            }))
        };
        Self {
            dolphin,
            pcsx2,
            xenia,
        }
    }

    pub fn borrowed(&self) -> ProfileDiscoveries<'_> {
        ProfileDiscoveries {
            dolphin: self.dolphin.as_ref().ok(),
            dolphin_error: self.dolphin.as_ref().err().cloned(),
            pcsx2: self.pcsx2.as_ref().ok(),
            pcsx2_error: self.pcsx2.as_ref().err().cloned(),
            xenia: self.xenia.as_ref(),
            xenia_error: None,
            preferred_dolphin: None,
            preferred_pcsx2: None,
            preferred_xenia: None,
        }
    }
}

/// Every directory a discovered profile would be written into, for the
/// free-space and read-only checks. Deduplicated, deterministic order.
pub fn profile_destination_directories(report: &ProfileAssessmentReport) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = report
        .profiles
        .iter()
        .map(|profile| {
            let path = PathBuf::from(&profile.destination_path.display);
            // A destination that is a file (a .pnach, a GameSettings .ini)
            // shares its parent's filesystem, which is what capacity and
            // mount mode are actually about.
            if profile.destination_is_directory {
                path
            } else {
                path.parent().map(Path::to_path_buf).unwrap_or(path)
            }
        })
        .collect();
    directories.sort();
    directories.dedup();
    directories
}

/// The managed-file scan targets implied by discovered profiles.
///
/// Only formats with an in-file ArchiveFS marker are worth returning: for the
/// others there is nothing in the file to recognise, so walking the directory
/// would read a user's own files for no diagnostic gain.
pub fn managed_scan_targets(report: &ProfileAssessmentReport) -> Vec<ManagedScanTarget> {
    let mut targets: Vec<ManagedScanTarget> = report
        .profiles
        .iter()
        .filter(|profile| profile.emulator == EmulatorKind::Pcsx2 && profile.destination_exists)
        .map(|profile| {
            let path = PathBuf::from(&profile.destination_path.display);
            ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: if profile.destination_is_directory {
                    path
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or(path)
                },
            }
        })
        .collect();
    targets.sort_by(|left, right| left.destination_root.cmp(&right.destination_root));
    targets.dedup();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::tests::{TempTree, snapshot_tree};
    use std::os::unix::fs::symlink;

    /// One assessment of a real path, through the same code path the runner
    /// uses. Nothing is created here except the fixture itself.
    fn assess(destination: &Path, mount_table: Option<&[MountEntry]>) -> ProfileAssessment {
        assess_one(
            EmulatorKind::Pcsx2,
            "pcsx2-native".to_string(),
            "Native".to_string(),
            "User".to_string(),
            "documented native path",
            true,
            Vec::new(),
            destination.parent().unwrap_or(destination),
            &destination.to_path_buf(),
            None,
            mount_table,
        )
    }

    fn report(profiles: Vec<ProfileAssessment>) -> ProfileAssessmentReport {
        ProfileAssessmentReport {
            profiles,
            unavailable: Vec::new(),
            discovery_incomplete: false,
        }
    }

    /// A read-write mount covering every path a fixture can use, so a test
    /// exercises the permissions branch rather than the unknown-mount branch.
    fn rw_table() -> Vec<MountEntry> {
        vec![MountEntry {
            mount_point: PathBuf::from("/"),
            read_only_mount: false,
            read_only_superblock: false,
            filesystem_type: Some("ext4".to_string()),
        }]
    }

    fn read_only_table(mount_point: &Path) -> Vec<MountEntry> {
        vec![MountEntry {
            mount_point: mount_point.to_path_buf(),
            read_only_mount: true,
            read_only_superblock: false,
            filesystem_type: Some("ext4".to_string()),
        }]
    }

    /// Test 35
    #[test]
    fn an_existing_writable_profile_directory_appears_writable() {
        let tree = TempTree::new("profiles-writable");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let assessed = assess(&destination, Some(&rw_table()));
        assert!(assessed.destination_exists);
        assert!(assessed.destination_is_directory);
        assert_eq!(assessed.writability, WritabilityAssessment::AppearsWritable);
    }

    /// Test 36
    #[test]
    fn a_writable_profile_is_information_not_a_problem() {
        let tree = TempTree::new("profiles-healthy");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let assessed = assess(&destination, Some(&rw_table()));
        assert_eq!(
            assessed.severity(),
            None,
            "a profile ArchiveFS can write to must produce no finding at all"
        );
        assert!(findings_from_emulator_profiles(&report(vec![assessed])).is_empty());
    }

    /// Test 37
    #[test]
    fn a_missing_profile_directory_is_reported_but_never_created() {
        let tree = TempTree::new("profiles-missing");
        let destination = tree.path().join("patches");
        let before = snapshot_tree(tree.path());
        let assessed = assess(&destination, Some(&rw_table()));
        assert_eq!(
            assessed.writability,
            WritabilityAssessment::MissingDestination
        );
        let findings = findings_from_emulator_profiles(&report(vec![assessed]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "emulator_profile.missing_destination");
        assert_eq!(findings[0].severity, DoctorSeverity::Info);
        assert_eq!(
            snapshot_tree(tree.path()),
            before,
            "Doctor must never create a profile directory to make a check pass"
        );
    }

    /// Test 38
    #[test]
    fn a_read_only_filesystem_beats_permissions_that_look_writable() {
        let tree = TempTree::new("profiles-read-only");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let table = read_only_table(tree.path());
        let assessed = assess(&destination, Some(&table));
        assert_eq!(
            assessed.writability,
            WritabilityAssessment::ReadOnlyFilesystem
        );
        let findings = findings_from_emulator_profiles(&report(vec![assessed]));
        assert_eq!(findings[0].id, "emulator_profile.read_only_filesystem");
        assert!(
            findings[0]
                .next_step
                .as_deref()
                .expect("guidance")
                .contains("Remount"),
            "the advice must address the mount, not the permissions"
        );
    }

    /// Test 39
    #[test]
    fn a_symlinked_profile_destination_is_unsafe_and_is_not_followed() {
        let tree = TempTree::new("profiles-symlink");
        let real = tree.path().join("real-patches");
        fs::create_dir_all(&real).expect("fixture");
        let link = tree.path().join("patches");
        symlink(&real, &link).expect("fixture");
        let assessed = assess(&link, Some(&rw_table()));
        assert!(assessed.destination_is_symlink);
        assert_eq!(
            assessed.writability,
            WritabilityAssessment::UnsafeDestination
        );
        let findings = findings_from_emulator_profiles(&report(vec![assessed]));
        assert_eq!(findings[0].id, "emulator_profile.unsafe_destination");
        assert_eq!(findings[0].severity, DoctorSeverity::Warning);
    }

    /// Test 40
    #[test]
    fn a_file_where_a_profile_directory_belongs_is_unsafe() {
        let tree = TempTree::new("profiles-file");
        let destination = tree.path().join("patches");
        fs::write(&destination, b"not a directory").expect("fixture");
        let assessed = assess(&destination, Some(&rw_table()));
        assert!(!assessed.destination_is_directory);
        assert_eq!(
            assessed.writability,
            WritabilityAssessment::UnsafeDestination
        );
    }

    /// Test 41
    #[test]
    fn an_unknown_mount_state_leaves_writability_unproven_rather_than_claimed() {
        let tree = TempTree::new("profiles-unknown-mount");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let assessed = assess(&destination, None);
        assert_eq!(assessed.mount_mode, MountMode::Unknown);
        assert_eq!(assessed.writability, WritabilityAssessment::NotProven);
        let findings = findings_from_emulator_profiles(&report(vec![assessed]));
        assert_eq!(findings[0].id, "emulator_profile.writability_not_proven");
        assert!(
            findings[0]
                .why_it_matters
                .as_deref()
                .expect("guidance")
                .contains("will not write a test file"),
            "the reason for the uncertainty must be stated plainly"
        );
    }

    /// Test 42
    #[test]
    fn a_profile_finding_carries_machine_readable_values() {
        let tree = TempTree::new("profiles-measurements");
        let destination = tree.path().join("patches");
        let assessed = assess(&destination, Some(&rw_table()));
        let findings = findings_from_emulator_profiles(&report(vec![assessed]));
        let measurements = &findings[0].measurements;
        assert_eq!(
            measurements.get("profile_kind"),
            Some(&Measurement::Text("Native".to_string()))
        );
        assert_eq!(
            measurements.get("discovery_confidence"),
            Some(&Measurement::Text("documented native path".to_string()))
        );
        assert_eq!(
            measurements.get("writability_assessment"),
            Some(&Measurement::Text(
                WritabilityAssessment::MissingDestination
                    .label()
                    .to_string()
            ))
        );
        assert_eq!(
            measurements.get("filesystem_read_only"),
            Some(&Measurement::Flag(false))
        );
    }

    /// Test 43
    #[test]
    fn many_profiles_needing_attention_collapse_into_one_result() {
        let tree = TempTree::new("profiles-flood");
        let profiles: Vec<ProfileAssessment> = (0..MAX_INDIVIDUAL_PROFILE_FINDINGS + 5)
            .map(|index| {
                assess(
                    &tree.path().join(format!("patches-{index}")),
                    Some(&rw_table()),
                )
            })
            .collect();
        let findings = findings_from_emulator_profiles(&report(profiles));
        assert_eq!(
            findings.len(),
            1,
            "a dashboard must never be flooded with one result per profile"
        );
        assert_eq!(findings[0].id, "emulator_profile.multiple_need_attention");
        assert_eq!(
            findings[0].measurements.get("profiles_needing_attention"),
            Some(&Measurement::Integer(
                (MAX_INDIVIDUAL_PROFILE_FINDINGS + 5) as u64
            ))
        );
        assert!(
            findings[0]
                .evidence
                .iter()
                .any(|item| item.contains("and 5 more")),
            "the count that was left out must still be visible"
        );
    }

    /// Test 44
    #[test]
    fn exactly_the_limit_is_still_listed_individually() {
        let tree = TempTree::new("profiles-at-limit");
        let profiles: Vec<ProfileAssessment> = (0..MAX_INDIVIDUAL_PROFILE_FINDINGS)
            .map(|index| {
                assess(
                    &tree.path().join(format!("patches-{index}")),
                    Some(&rw_table()),
                )
            })
            .collect();
        assert_eq!(
            findings_from_emulator_profiles(&report(profiles)).len(),
            MAX_INDIVIDUAL_PROFILE_FINDINGS
        );
    }

    /// Test 45
    #[test]
    fn several_usable_profiles_with_none_selected_is_reported_as_ambiguous() {
        let tree = TempTree::new("profiles-ambiguous");
        let mut profiles = Vec::new();
        for index in 0..2 {
            let destination = tree.path().join(format!("patches-{index}"));
            fs::create_dir_all(&destination).expect("fixture");
            profiles.push(assess(&destination, Some(&rw_table())));
        }
        let findings = findings_from_emulator_profiles(&report(profiles));
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].id, "emulator_profile.ambiguous_preferred_pcsx2_profile",
            "the id names the emulator, so two emulators' results cannot merge"
        );
        assert_eq!(findings[0].severity, DoctorSeverity::Info);
    }

    /// Test 46
    #[test]
    fn a_selected_profile_removes_the_ambiguity() {
        let tree = TempTree::new("profiles-selected");
        let mut profiles = Vec::new();
        for index in 0..2 {
            let destination = tree.path().join(format!("patches-{index}"));
            fs::create_dir_all(&destination).expect("fixture");
            profiles.push(assess(&destination, Some(&rw_table())));
        }
        profiles[0].preferred = Some(true);
        assert!(
            findings_from_emulator_profiles(&report(profiles)).is_empty(),
            "once a profile is remembered there is nothing to ask about"
        );
    }

    /// Test 47
    #[test]
    fn a_failed_discovery_is_reported_as_unavailable_not_as_no_profiles() {
        let discoveries = ProfileDiscoveries {
            dolphin: None,
            dolphin_error: Some("HOME is not set".to_string()),
            pcsx2: None,
            pcsx2_error: None,
            xenia: None,
            xenia_error: None,
            preferred_dolphin: None,
            preferred_pcsx2: None,
            preferred_xenia: None,
        };
        let assessed = assess_emulator_profiles(&discoveries, None);
        assert!(assessed.profiles.is_empty());
        assert_eq!(assessed.unavailable.len(), 1);
        assert_eq!(assessed.unavailable[0].0, EmulatorKind::Dolphin);
        let not_checked = not_checked_from_emulator_profiles(&assessed);
        assert!(
            not_checked
                .iter()
                .any(|item| item.name.contains("Dolphin") && item.reason.contains("HOME")),
            "a failed discovery must be stated, never presented as a clean result"
        );
    }

    /// Test 48
    #[test]
    fn a_flatpak_profile_always_states_the_sandbox_caveat() {
        let tree = TempTree::new("profiles-flatpak");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let mut assessed = assess(&destination, Some(&rw_table()));
        assessed.profile_kind = "Flatpak".to_string();
        let not_checked = not_checked_from_emulator_profiles(&report(vec![assessed]));
        assert!(
            not_checked
                .iter()
                .any(|item| item.name == "Flatpak sandbox write permission"),
            "metadata cannot see a portal, and that limit must be admitted"
        );
    }

    /// Test 49
    #[test]
    fn incomplete_discovery_is_declared_rather_than_hidden() {
        let mut assessed = report(Vec::new());
        assessed.discovery_incomplete = true;
        assert!(
            not_checked_from_emulator_profiles(&assessed)
                .iter()
                .any(|item| item.name.contains("Complete emulator profile discovery"))
        );
    }

    /// Test 50
    #[test]
    fn the_json_summary_exposes_the_values_a_script_needs() {
        let tree = TempTree::new("profiles-json");
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let summary = profile_json_summary(&assess(&destination, Some(&rw_table())));
        for key in [
            "profile_kind",
            "discovery_confidence",
            "writability_assessment",
            "filesystem_read_only",
            "destination_exists",
            "finding_id",
        ] {
            assert!(summary.get(key).is_some(), "`{key}` is missing from --json");
        }
        assert_eq!(summary["filesystem_read_only"], serde_json::json!(false));
    }

    /// Test 51
    #[test]
    fn a_destination_file_contributes_its_parent_directory_to_the_storage_check() {
        let tree = TempTree::new("profiles-destinations");
        let file = tree.path().join("GameSettings/GALE01.ini");
        fs::create_dir_all(file.parent().expect("parent")).expect("fixture");
        fs::write(&file, b"[Gecko]\n").expect("fixture");
        let assessed = assess(&file, Some(&rw_table()));
        let directories = profile_destination_directories(&report(vec![assessed]));
        assert_eq!(directories, vec![tree.path().join("GameSettings")]);
    }

    /// Test 52
    #[test]
    fn only_pcsx2_profiles_become_managed_scan_targets() {
        let tree = TempTree::new("profiles-targets");
        let pcsx2 = tree.path().join("patches");
        fs::create_dir_all(&pcsx2).expect("fixture");
        let mut dolphin = assess(&pcsx2, Some(&rw_table()));
        dolphin.emulator = EmulatorKind::Dolphin;
        let targets =
            managed_scan_targets(&report(vec![assess(&pcsx2, Some(&rw_table())), dolphin]));
        assert_eq!(
            targets.len(),
            1,
            "only a format with an in-file ArchiveFS marker is worth walking"
        );
        assert_eq!(targets[0].format, ManagedFormat::Pcsx2Pnach);
        assert_eq!(targets[0].destination_root, pcsx2);
    }

    /// Read-only proof 4: assessing every profile in a real tree changes
    /// nothing - no profile, no GameSettings file, no cheat, no test file.
    #[test]
    fn assessing_profiles_leaves_the_tree_byte_for_byte_unchanged() {
        let tree = TempTree::new("profiles-read-only-proof");
        let existing = tree.path().join("patches");
        fs::create_dir_all(&existing).expect("fixture");
        fs::write(existing.join("SLUS-20946.pnach"), b"// user's own file\n").expect("fixture");
        let missing = tree.path().join("absent-patches");
        let before = snapshot_tree(tree.path());

        let assessed = report(vec![
            assess(&existing, Some(&rw_table())),
            assess(&missing, Some(&rw_table())),
        ]);
        let _ = findings_from_emulator_profiles(&assessed);
        let _ = not_checked_from_emulator_profiles(&assessed);
        let _ = profile_destination_directories(&assessed);
        let _ = managed_scan_targets(&assessed);

        assert_eq!(
            snapshot_tree(tree.path()),
            before,
            "a profile assessment must not create a profile, a settings file or a probe"
        );
    }

    /// Read-only proof 5: this module contains no write, permission-changing
    /// or process-spawning call.
    #[test]
    fn this_module_contains_no_mutating_call() {
        let whole = include_str!("profiles.rs");
        let source = whole
            .split_once("#[cfg(test)]")
            .expect("this file ends with its own test module")
            .0;
        for forbidden in [
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "fs::set_permissions",
            "File::create",
            "OpenOptions",
            "libc::access",
            "libc::chmod",
            "Command",
            "ureq",
            "install_cheat",
            "apply_shared",
        ] {
            assert!(
                !source.contains(forbidden),
                "`{forbidden}` must never appear in a read-only diagnostic module"
            );
        }
    }

    /// Read-only proof 6: no finding from this family offers a repair, so no
    /// Delete, Clean, Repair or Fix button can appear for one.
    #[test]
    fn no_profile_finding_offers_a_repair() {
        let tree = TempTree::new("profiles-no-repair");
        let table = read_only_table(tree.path());
        let destination = tree.path().join("patches");
        fs::create_dir_all(&destination).expect("fixture");
        let assessed = report(vec![
            assess(&destination, Some(&table)),
            assess(&tree.path().join("absent"), Some(&rw_table())),
        ]);
        for finding in findings_from_emulator_profiles(&assessed) {
            assert!(
                finding.repair.is_none(),
                "{} must not offer a repair in a diagnostic-only milestone",
                finding.id
            );
            assert!(
                finding.recovery.is_none(),
                "{} must not advertise a repair elsewhere either",
                finding.id
            );
        }
    }

    /// Test 97
    #[test]
    fn two_emulators_with_ambiguous_profiles_stay_two_separate_results() {
        let tree = TempTree::new("profiles-two-emulators");
        let mut profiles = Vec::new();
        for emulator in [EmulatorKind::Dolphin, EmulatorKind::Pcsx2] {
            for index in 0..2 {
                let destination = tree
                    .path()
                    .join(format!("{}-{index}", emulator.label().to_lowercase()));
                fs::create_dir_all(&destination).expect("fixture");
                let mut assessed = assess(&destination, Some(&rw_table()));
                assessed.emulator = emulator;
                profiles.push(assessed);
            }
        }
        let ids: Vec<String> = findings_from_emulator_profiles(&report(profiles))
            .into_iter()
            .map(|finding| finding.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "emulator_profile.ambiguous_preferred_dolphin_profile".to_string(),
                "emulator_profile.ambiguous_preferred_pcsx2_profile".to_string(),
            ],
            "a live scan merged these into one result when they shared an id"
        );
    }
}
