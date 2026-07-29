//! PCSX2-facing identity and confirmed-profile foundations.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::game_identity::{GameIdentityReport, IdentityKind, IdentityPlatform, IdentityStatus};

use super::pcsx2::normalize_crc;
use super::pcsx2_local::{
    Pcsx2PatchCategory, Pcsx2PatchDirectoryState, Pcsx2Profile, Pcsx2ProfileDiscovery,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2IdentityState {
    Verified,
    MissingCrc,
    Deferred,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pcsx2GameIdentity {
    pub archive_path: PathBuf,
    pub title: String,
    pub region: Option<String>,
    pub serial: Option<String>,
    pub executable_crc: Option<String>,
    pub state: Pcsx2IdentityState,
    pub evidence: Vec<String>,
    pub plain_failure_reason: Option<String>,
}

impl Pcsx2GameIdentity {
    /// Adapts the existing read-only identity report. It never promotes a
    /// candidate or filename-derived CRC to verified PNACH identity.
    pub fn from_report(title: impl Into<String>, report: &GameIdentityReport) -> Self {
        let title = title.into();
        let serial = report.verified_ps2_serial().map(str::to_owned);
        let executable_crc = report.verified_pcsx2_crc().and_then(normalize_crc);
        let crc_evidence = report
            .evidence
            .iter()
            .find(|item| item.kind == IdentityKind::Pcsx2ExecutableCrc);
        let state = if report.platform != IdentityPlatform::PlayStation2 {
            Pcsx2IdentityState::Unsupported
        } else if executable_crc.is_some() {
            Pcsx2IdentityState::Verified
        } else {
            match crc_evidence.map(|item| item.status) {
                Some(IdentityStatus::Deferred) => Pcsx2IdentityState::Deferred,
                Some(IdentityStatus::Ambiguous | IdentityStatus::ResourceLimitReached) => {
                    Pcsx2IdentityState::Ambiguous
                }
                Some(IdentityStatus::Unsupported | IdentityStatus::Invalid) => {
                    Pcsx2IdentityState::Unsupported
                }
                _ => Pcsx2IdentityState::MissingCrc,
            }
        };
        let plain_failure_reason = match state {
            Pcsx2IdentityState::Verified => None,
            Pcsx2IdentityState::MissingCrc => Some(
                "ArchiveFS could not prove the game CRC required for PCSX2 cheats.".to_string(),
            ),
            Pcsx2IdentityState::Deferred => {
                Some("Game identification is not available for this image format yet.".to_string())
            }
            Pcsx2IdentityState::Ambiguous => Some(
                "ArchiveFS found ambiguous game identity evidence and will not guess.".to_string(),
            ),
            Pcsx2IdentityState::Unsupported => {
                Some("This selection is not a supported PlayStation 2 game image.".to_string())
            }
        };
        let evidence = report
            .evidence
            .iter()
            .filter(|item| {
                matches!(
                    item.kind,
                    IdentityKind::Ps2Serial | IdentityKind::Pcsx2ExecutableCrc
                )
            })
            .map(|item| format!("{}: {} ({})", item.kind, item.status, item.diagnostic))
            .collect();
        Self {
            archive_path: report.archive_path.clone(),
            title,
            region: None,
            serial,
            executable_crc,
            state,
            evidence,
            plain_failure_reason,
        }
    }

    pub fn verified_crc(&self) -> Option<&str> {
        (self.state == Pcsx2IdentityState::Verified)
            .then_some(self.executable_crc.as_deref())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pcsx2ProfileChoiceError {
    NoEligibleProfile,
    ConfirmationRequired { eligible_profile_ids: Vec<String> },
    ConfirmedProfileUnavailable { profile_id: String },
}

impl std::fmt::Display for Pcsx2ProfileChoiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEligibleProfile => formatter.write_str("no eligible PCSX2 profile was found"),
            Self::ConfirmationRequired { .. } => {
                formatter.write_str("choose and confirm a PCSX2 profile")
            }
            Self::ConfirmedProfileUnavailable { profile_id } => {
                write!(
                    formatter,
                    "confirmed PCSX2 profile is unavailable: {profile_id}"
                )
            }
        }
    }
}

impl std::error::Error for Pcsx2ProfileChoiceError {}

/// Requires a confirmed ID whenever discovery has multiple eligible profiles.
/// A single eligible profile may be returned directly, matching the existing
/// remembered-profile selection policy.
pub fn confirmed_pcsx2_profile<'a>(
    discovery: &'a Pcsx2ProfileDiscovery,
    confirmed_profile_id: Option<&str>,
) -> Result<&'a Pcsx2Profile, Pcsx2ProfileChoiceError> {
    let eligible = discovery
        .profiles
        .iter()
        .filter(|profile| profile.eligible)
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return Err(Pcsx2ProfileChoiceError::NoEligibleProfile);
    }
    if let Some(profile_id) = confirmed_profile_id {
        return eligible
            .into_iter()
            .find(|profile| profile.profile_id == profile_id)
            .ok_or_else(|| Pcsx2ProfileChoiceError::ConfirmedProfileUnavailable {
                profile_id: profile_id.to_string(),
            });
    }
    if eligible.len() == 1 {
        Ok(eligible[0])
    } else {
        Err(Pcsx2ProfileChoiceError::ConfirmationRequired {
            eligible_profile_ids: eligible
                .into_iter()
                .map(|profile| profile.profile_id.clone())
                .collect(),
        })
    }
}

/// Resolves only the normal cheats directory. `cheats_ws` and other patch
/// categories are never accepted by this install adapter.
pub fn pcsx2_cheats_directory(profile: &Pcsx2Profile) -> Option<&Path> {
    profile
        .eligible
        .then(|| {
            profile.patch_directories.iter().find(|directory| {
                directory.category == Pcsx2PatchCategory::Cheats
                    && matches!(
                        directory.state,
                        Pcsx2PatchDirectoryState::Available | Pcsx2PatchDirectoryState::Missing
                    )
            })
        })
        .flatten()
        .map(|directory| directory.path.as_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_identity::{
        IdentityConfidence, IdentityEvidence, IdentityImageFormat, IdentityProvenance,
    };
    use crate::patch_manager::{Pcsx2InstallationType, Pcsx2PatchDirectory, Pcsx2ProfileScope};

    fn report(status: IdentityStatus, value: Option<&str>) -> GameIdentityReport {
        GameIdentityReport {
            archive_path: PathBuf::from("/games/game.iso"),
            platform: IdentityPlatform::PlayStation2,
            format: IdentityImageFormat::Iso,
            evidence: vec![IdentityEvidence {
                kind: IdentityKind::Pcsx2ExecutableCrc,
                status,
                value: value.map(str::to_string),
                confidence: IdentityConfidence::ExactBytes,
                provenance: IdentityProvenance {
                    archive_path: PathBuf::from("/games/game.iso"),
                    member_path: None,
                    member_index: None,
                    method: "test".to_string(),
                },
                diagnostic: "test evidence".to_string(),
            }],
            warnings: Vec::new(),
            bytes_read: 1,
            archive_members_inspected: 0,
            metadata_paths_inspected: 1,
            nested_container_depth: 0,
            complete: true,
        }
    }

    fn profile(id: &str, root: &str) -> Pcsx2Profile {
        Pcsx2Profile {
            profile_id: id.to_string(),
            installation_type: Pcsx2InstallationType::Portable,
            scope: Pcsx2ProfileScope::Portable,
            configuration_path: PathBuf::from(root),
            provenance: "test",
            eligible: true,
            blockers: Vec::new(),
            patch_directories: vec![Pcsx2PatchDirectory {
                path: PathBuf::from(root).join("cheats"),
                category: Pcsx2PatchCategory::Cheats,
                state: Pcsx2PatchDirectoryState::Missing,
                warning: None,
                identity: None,
            }],
            configuration_identity: None,
        }
    }

    #[test]
    fn only_verified_crc_becomes_install_identity() {
        let verified = Pcsx2GameIdentity::from_report(
            "Game",
            &report(IdentityStatus::Verified, Some("a1b2c3d4")),
        );
        assert_eq!(verified.verified_crc(), Some("A1B2C3D4"));
        let candidate = Pcsx2GameIdentity::from_report(
            "Game",
            &report(IdentityStatus::Candidate, Some("A1B2C3D4")),
        );
        assert_eq!(candidate.verified_crc(), None);
        assert_eq!(candidate.state, Pcsx2IdentityState::MissingCrc);
    }

    #[test]
    fn incomplete_identity_is_a_terminal_truthful_state() {
        let identity =
            Pcsx2GameIdentity::from_report("Game", &report(IdentityStatus::Deferred, None));
        assert_eq!(identity.state, Pcsx2IdentityState::Deferred);
        assert!(identity.plain_failure_reason.is_some());
    }

    #[test]
    fn multiple_profiles_require_confirmation_and_exact_choice() {
        let discovery = Pcsx2ProfileDiscovery {
            profiles: vec![profile("a", "/tmp/a"), profile("b", "/tmp/b")],
            warnings: Vec::new(),
            complete: true,
        };
        assert!(matches!(
            confirmed_pcsx2_profile(&discovery, None),
            Err(Pcsx2ProfileChoiceError::ConfirmationRequired { .. })
        ));
        assert_eq!(
            confirmed_pcsx2_profile(&discovery, Some("b"))
                .unwrap()
                .profile_id,
            "b"
        );
    }

    #[test]
    fn normal_cheats_directory_never_resolves_widescreen() {
        let mut selected = profile("a", "/tmp/a");
        selected.patch_directories.insert(
            0,
            Pcsx2PatchDirectory {
                path: PathBuf::from("/tmp/a/cheats_ws"),
                category: Pcsx2PatchCategory::WidescreenPatches,
                state: Pcsx2PatchDirectoryState::Available,
                warning: None,
                identity: None,
            },
        );
        assert_eq!(
            pcsx2_cheats_directory(&selected),
            Some(Path::new("/tmp/a/cheats"))
        );
    }
}
