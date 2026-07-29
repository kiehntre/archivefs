//! Provider-neutral PCSX2 cheat catalogue and strict compatibility model.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::pcsx2::normalize_crc;
use super::pcsx2_identity::Pcsx2GameIdentity;
use super::pcsx2_pnach::{ManagedPnachCheat, PnachPatchLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2ProviderTrust {
    Approved,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2CheatCategory {
    OrdinaryCheat,
    Widescreen,
    EncryptedUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2CheatConfidence {
    VerifiedCrcAndConstraints,
    VerifiedCrcOnly,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pcsx2CheatProviderRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub source_game_id: Option<String>,
    pub source_url: Option<String>,
    pub game_crc: String,
    pub serial_constraint: Option<String>,
    pub region_constraint: Option<String>,
    pub patch_lines: Vec<String>,
    pub category: Pcsx2CheatCategory,
    pub confidence: Pcsx2CheatConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pcsx2CheatProviderCatalogue {
    pub provider_id: String,
    pub provider_name: String,
    pub source: String,
    pub trust: Pcsx2ProviderTrust,
    pub records: Vec<Pcsx2CheatProviderRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2CandidateBlockedReason {
    ProviderUnverified,
    RecordUnverified,
    IdentityIncomplete,
    InvalidCatalogueCrc,
    CrcMismatch,
    SerialUnverified,
    SerialMismatch,
    RegionUnverified,
    RegionMismatch,
    MalformedPatchLine,
    UnsupportedEncryptedFormat,
    WidescreenKeptSeparate,
    DuplicateRecordId,
}

impl Pcsx2CandidateBlockedReason {
    pub const fn plain_reason(self) -> &'static str {
        match self {
            Self::ProviderUnverified => "This cheat source has not been approved.",
            Self::RecordUnverified => "This cheat record has not been verified.",
            Self::IdentityIncomplete => "The game CRC could not be verified.",
            Self::InvalidCatalogueCrc => "The cheat source has an invalid game CRC.",
            Self::CrcMismatch => "This cheat is for a different game CRC.",
            Self::SerialUnverified => {
                "This cheat requires a game serial that could not be verified."
            }
            Self::SerialMismatch => "This cheat is for a different game serial.",
            Self::RegionUnverified => {
                "This cheat requires a game region that could not be verified."
            }
            Self::RegionMismatch => "This cheat is for a different game region.",
            Self::MalformedPatchLine => {
                "This cheat contains a malformed or unsupported PNACH line."
            }
            Self::UnsupportedEncryptedFormat => "Encrypted cheat formats are not supported.",
            Self::WidescreenKeptSeparate => {
                "Widescreen patches are kept separate from ordinary cheats."
            }
            Self::DuplicateRecordId => "The cheat source contains a duplicate cheat ID.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pcsx2CheatCompatibility {
    Compatible,
    Blocked(Pcsx2CandidateBlockedReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2CheatCandidate {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub source_game_id: Option<String>,
    pub source_url: Option<String>,
    pub provider_id: String,
    pub provider_name: String,
    pub source: String,
    pub game_crc: String,
    pub serial_constraint: Option<String>,
    pub region_constraint: Option<String>,
    pub patch_lines: Vec<PnachPatchLine>,
    pub confidence: Pcsx2CheatConfidence,
    pub compatibility: Pcsx2CheatCompatibility,
}

impl Pcsx2CheatCandidate {
    pub fn selectable(&self) -> bool {
        self.compatibility == Pcsx2CheatCompatibility::Compatible && !self.patch_lines.is_empty()
    }
}

pub fn build_pcsx2_cheat_candidates(
    catalogue: &Pcsx2CheatProviderCatalogue,
    identity: &Pcsx2GameIdentity,
) -> Vec<Pcsx2CheatCandidate> {
    let mut ids = BTreeSet::new();
    catalogue
        .records
        .iter()
        .map(|record| {
            let duplicate = !ids.insert(record.id.clone());
            let parsed_lines = record
                .patch_lines
                .iter()
                .map(|line| PnachPatchLine::parse(line))
                .collect::<Result<Vec<_>, _>>();
            let compatibility = classify(catalogue, record, identity, duplicate, &parsed_lines);
            Pcsx2CheatCandidate {
                id: record.id.clone(),
                name: record.name.clone(),
                description: record.description.clone(),
                author: record.author.clone(),
                source_game_id: record.source_game_id.clone(),
                source_url: record.source_url.clone(),
                provider_id: catalogue.provider_id.clone(),
                provider_name: catalogue.provider_name.clone(),
                source: catalogue.source.clone(),
                game_crc: normalize_crc(&record.game_crc)
                    .unwrap_or_else(|| record.game_crc.clone()),
                serial_constraint: record.serial_constraint.clone(),
                region_constraint: record.region_constraint.clone(),
                patch_lines: parsed_lines.unwrap_or_default(),
                confidence: record.confidence,
                compatibility,
            }
        })
        .collect()
}

fn classify(
    catalogue: &Pcsx2CheatProviderCatalogue,
    record: &Pcsx2CheatProviderRecord,
    identity: &Pcsx2GameIdentity,
    duplicate: bool,
    parsed_lines: &Result<Vec<PnachPatchLine>, super::pcsx2_pnach::PnachDocumentError>,
) -> Pcsx2CheatCompatibility {
    let blocked = if catalogue.trust != Pcsx2ProviderTrust::Approved {
        Some(Pcsx2CandidateBlockedReason::ProviderUnverified)
    } else if record.confidence == Pcsx2CheatConfidence::Unverified {
        Some(Pcsx2CandidateBlockedReason::RecordUnverified)
    } else if duplicate {
        Some(Pcsx2CandidateBlockedReason::DuplicateRecordId)
    } else if record.category == Pcsx2CheatCategory::EncryptedUnsupported {
        Some(Pcsx2CandidateBlockedReason::UnsupportedEncryptedFormat)
    } else if record.category == Pcsx2CheatCategory::Widescreen {
        Some(Pcsx2CandidateBlockedReason::WidescreenKeptSeparate)
    } else if parsed_lines.is_err() || record.patch_lines.is_empty() {
        Some(Pcsx2CandidateBlockedReason::MalformedPatchLine)
    } else if normalize_crc(&record.game_crc).is_none() {
        Some(Pcsx2CandidateBlockedReason::InvalidCatalogueCrc)
    } else if identity.verified_crc().is_none() {
        Some(Pcsx2CandidateBlockedReason::IdentityIncomplete)
    } else if normalize_crc(&record.game_crc).as_deref() != identity.verified_crc() {
        Some(Pcsx2CandidateBlockedReason::CrcMismatch)
    } else if let Some(required) = record.serial_constraint.as_deref() {
        match identity.serial.as_deref() {
            None => Some(Pcsx2CandidateBlockedReason::SerialUnverified),
            Some(actual) if !actual.eq_ignore_ascii_case(required) => {
                Some(Pcsx2CandidateBlockedReason::SerialMismatch)
            }
            Some(_) => None,
        }
    } else {
        None
    };
    let blocked = blocked.or_else(|| {
        record
            .region_constraint
            .as_deref()
            .and_then(|required| match identity.region.as_deref() {
                None => Some(Pcsx2CandidateBlockedReason::RegionUnverified),
                Some(actual) if !actual.eq_ignore_ascii_case(required) => {
                    Some(Pcsx2CandidateBlockedReason::RegionMismatch)
                }
                Some(_) => None,
            })
    });
    blocked.map_or(
        Pcsx2CheatCompatibility::Compatible,
        Pcsx2CheatCompatibility::Blocked,
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pcsx2CheatSelection {
    pub selected_ids: BTreeSet<String>,
}

pub fn selected_pcsx2_managed_cheats(
    candidates: &[Pcsx2CheatCandidate],
    selection: &Pcsx2CheatSelection,
) -> Result<Vec<ManagedPnachCheat>, String> {
    if selection.selected_ids.is_empty() {
        return Err("select at least one compatible PCSX2 cheat".to_string());
    }
    let by_id = candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    selection
        .selected_ids
        .iter()
        .map(|id| {
            let candidate = by_id
                .get(id.as_str())
                .ok_or_else(|| format!("selected PCSX2 cheat is stale or missing: {id}"))?;
            if !candidate.selectable() {
                return Err(format!("selected PCSX2 cheat is blocked: {id}"));
            }
            Ok(ManagedPnachCheat {
                id: candidate.id.clone(),
                name: candidate.name.clone(),
                description: Some(
                    [
                        candidate.description.clone(),
                        candidate
                            .author
                            .as_ref()
                            .map(|author| format!("Author: {author}")),
                        candidate
                            .source_game_id
                            .as_ref()
                            .map(|game_id| format!("GameHacking game ID: {game_id}")),
                        candidate
                            .source_url
                            .as_ref()
                            .map(|source| format!("Source: {source}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" | "),
                )
                .filter(|description| !description.is_empty()),
                patch_lines: candidate.patch_lines.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::Pcsx2IdentityState;
    use super::*;

    fn identity() -> Pcsx2GameIdentity {
        Pcsx2GameIdentity {
            archive_path: std::path::PathBuf::from("/games/game.iso"),
            title: "Game".to_string(),
            region: Some("NTSC-U".to_string()),
            serial: Some("SLUS-20312".to_string()),
            executable_crc: Some("A1B2C3D4".to_string()),
            state: Pcsx2IdentityState::Verified,
            evidence: Vec::new(),
            plain_failure_reason: None,
        }
    }

    fn record() -> Pcsx2CheatProviderRecord {
        Pcsx2CheatProviderRecord {
            id: "infinite-health".to_string(),
            name: "Infinite health".to_string(),
            description: None,
            author: None,
            source_game_id: None,
            source_url: None,
            game_crc: "A1B2C3D4".to_string(),
            serial_constraint: Some("SLUS-20312".to_string()),
            region_constraint: Some("NTSC-U".to_string()),
            patch_lines: vec!["patch=1,EE,20123456,word,00000001".to_string()],
            category: Pcsx2CheatCategory::OrdinaryCheat,
            confidence: Pcsx2CheatConfidence::VerifiedCrcAndConstraints,
        }
    }

    fn catalogue(record: Pcsx2CheatProviderRecord) -> Pcsx2CheatProviderCatalogue {
        Pcsx2CheatProviderCatalogue {
            provider_id: "fixture".to_string(),
            provider_name: "Fixture provider".to_string(),
            source: "local test fixture".to_string(),
            trust: Pcsx2ProviderTrust::Approved,
            records: vec![record],
        }
    }

    #[test]
    fn exact_crc_serial_and_region_is_selectable() {
        let candidates = build_pcsx2_cheat_candidates(&catalogue(record()), &identity());
        assert!(candidates[0].selectable());
    }

    #[test]
    fn wrong_or_unverified_region_is_blocked() {
        let mut wrong = identity();
        wrong.region = Some("PAL".to_string());
        let candidates = build_pcsx2_cheat_candidates(&catalogue(record()), &wrong);
        assert_eq!(
            candidates[0].compatibility,
            Pcsx2CheatCompatibility::Blocked(Pcsx2CandidateBlockedReason::RegionMismatch)
        );
        wrong.region = None;
        let candidates = build_pcsx2_cheat_candidates(&catalogue(record()), &wrong);
        assert_eq!(
            candidates[0].compatibility,
            Pcsx2CheatCompatibility::Blocked(Pcsx2CandidateBlockedReason::RegionUnverified)
        );
    }

    #[test]
    fn malformed_and_encrypted_codes_are_never_selectable() {
        let mut malformed = record();
        malformed.patch_lines = vec!["DEADBEEF encrypted".to_string()];
        assert!(!build_pcsx2_cheat_candidates(&catalogue(malformed), &identity())[0].selectable());
        let mut encrypted = record();
        encrypted.category = Pcsx2CheatCategory::EncryptedUnsupported;
        assert!(!build_pcsx2_cheat_candidates(&catalogue(encrypted), &identity())[0].selectable());
    }

    #[test]
    fn unverified_provider_or_record_is_never_selectable() {
        let mut unverified_source = catalogue(record());
        unverified_source.trust = Pcsx2ProviderTrust::Unverified;
        assert_eq!(
            build_pcsx2_cheat_candidates(&unverified_source, &identity())[0].compatibility,
            Pcsx2CheatCompatibility::Blocked(Pcsx2CandidateBlockedReason::ProviderUnverified)
        );

        let mut unverified_record = record();
        unverified_record.confidence = Pcsx2CheatConfidence::Unverified;
        assert_eq!(
            build_pcsx2_cheat_candidates(&catalogue(unverified_record), &identity())[0]
                .compatibility,
            Pcsx2CheatCompatibility::Blocked(Pcsx2CandidateBlockedReason::RecordUnverified)
        );
    }

    #[test]
    fn widescreen_is_kept_out_of_normal_cheat_selection() {
        let mut widescreen = record();
        widescreen.category = Pcsx2CheatCategory::Widescreen;
        let candidate = &build_pcsx2_cheat_candidates(&catalogue(widescreen), &identity())[0];
        assert_eq!(
            candidate.compatibility,
            Pcsx2CheatCompatibility::Blocked(Pcsx2CandidateBlockedReason::WidescreenKeptSeparate)
        );
    }

    #[test]
    fn selection_materializes_only_exact_selected_ids_and_rejects_stale_ids() {
        let candidates = build_pcsx2_cheat_candidates(&catalogue(record()), &identity());
        let selected = Pcsx2CheatSelection {
            selected_ids: BTreeSet::from(["infinite-health".to_string()]),
        };
        assert_eq!(
            selected_pcsx2_managed_cheats(&candidates, &selected)
                .unwrap()
                .len(),
            1
        );
        let stale = Pcsx2CheatSelection {
            selected_ids: BTreeSet::from(["game-b-selection".to_string()]),
        };
        assert!(selected_pcsx2_managed_cheats(&candidates, &stale).is_err());
    }
}
