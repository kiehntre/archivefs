//! Provider-neutral platform identity enrichment.
//!
//! This layer resolves identity facts that have already been established by
//! their owning subsystem. It never reads or writes a ROM, never performs DAT
//! matching, and never guesses a provider platform string. RomM records enter
//! only after the RomM adapter mapped a slug through the canonical registry and
//! matching made the record usable; DAT facts enter only from cryptographic
//! exact audit verdicts whose source has a canonical platform assignment.
//!
//! Manual assignment is absolute. Verified DAT and usable RomM evidence are
//! otherwise treated as authoritative peers for conflict purposes: agreement
//! can enrich identity, but disagreement requires review instead of allowing
//! provider arrival order to pick a winner.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dat::audit::AuditVerdict;
use crate::dat::sources::audit_run::DatAuditOutcome;
use crate::identity_source::model::{ExternalIdentityRecord, IdentityProvider};

use super::{display_name_for, platform_by_id, platform_for_alias};

/// Stable provenance categories, ordered from weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformIdentitySource {
    Inference,
    ExistingStrongIdentity,
    Romm,
    VerifiedDat,
    Manual,
}

impl PlatformIdentitySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inference => "Existing inference",
            Self::ExistingStrongIdentity => "Existing game identity",
            Self::Romm => "RomM",
            Self::VerifiedDat => "Verified DAT",
            Self::Manual => "Manual assignment",
        }
    }
}

/// Confidence supported by the winning evidence, without pretending that
/// provider confidence and a user decision are the same kind of claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformIdentityConfidence {
    Inferred,
    Strong,
    High,
    Verified,
    UserSelected,
}

impl PlatformIdentityConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inferred => "Inferred",
            Self::Strong => "Strong",
            Self::High => "High",
            Self::Verified => "Verified",
            Self::UserSelected => "User selected",
        }
    }
}

/// One canonical platform fact and the generation it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIdentityEvidence {
    pub platform: String,
    pub source: PlatformIdentitySource,
    pub confidence: PlatformIdentityConfidence,
    pub generation: u64,
    pub detail: String,
}

impl PlatformIdentityEvidence {
    /// Builds evidence from a canonical id or exact registry alias. This is the
    /// general adapter for existing local identity; unknown values are refused.
    pub fn canonical(
        value: &str,
        source: PlatformIdentitySource,
        confidence: PlatformIdentityConfidence,
        generation: u64,
        detail: impl Into<String>,
    ) -> Option<Self> {
        let platform = canonical_platform(value)?;
        Some(Self {
            platform: platform.to_string(),
            source,
            confidence,
            generation,
            detail: detail.into(),
        })
    }

    /// A manual value is deliberately preserved even when it is custom text:
    /// the existing UI/CLI allow custom manual platforms and provider
    /// enrichment must never erase that choice.
    pub fn manual(value: &str, generation: u64) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        let platform = canonical_platform(trimmed).unwrap_or(trimmed);
        Some(Self {
            platform: platform.to_string(),
            source: PlatformIdentitySource::Manual,
            confidence: PlatformIdentityConfidence::UserSelected,
            generation,
            detail: "the user explicitly assigned this platform".to_string(),
        })
    }

    /// Adapts an already-matched RomM record. The provider's raw name/id is
    /// never considered: only the canonical candidate produced by the strict
    /// RomM slug adapter is accepted.
    pub fn from_romm(record: &ExternalIdentityRecord, generation: u64) -> Option<Self> {
        if record.provider != IdentityProvider::Romm
            || !record.verification.is_usable()
            || record.has_conflicts()
        {
            return None;
        }
        let candidate = record.platform_candidate.as_deref()?;
        let platform = platform_by_id(candidate)?;
        Some(Self {
            platform: platform.id.to_string(),
            source: PlatformIdentitySource::Romm,
            confidence: PlatformIdentityConfidence::High,
            generation,
            detail: format!(
                "RomM record {} resolved through the canonical platform registry ({})",
                record.provider_game_id,
                record.verification.label()
            ),
        })
    }

    /// Adapts one file from a DAT audit. Merely loading or parsing a DAT is not
    /// evidence: the local path must have a cryptographic exact verdict and the
    /// audited source must carry an exact canonical platform id.
    pub fn from_verified_dat(
        outcome: &DatAuditOutcome,
        local_path: &Path,
        generation: u64,
    ) -> Option<Self> {
        let candidate = outcome.platform.as_deref()?;
        let platform = platform_by_id(candidate)?;
        let entry = outcome.report.entries.iter().find(|entry| {
            Path::new(&entry.local_path) == local_path && entry.verdict.is_confident()
        })?;
        let algorithm = match &entry.verdict {
            AuditVerdict::Exact { algorithm, .. }
            | AuditVerdict::ExactMultipleCandidates { algorithm, .. } => *algorithm,
            _ => return None,
        };
        Some(Self {
            platform: platform.id.to_string(),
            source: PlatformIdentitySource::VerifiedDat,
            confidence: PlatformIdentityConfidence::Verified,
            generation,
            detail: format!(
                "{} verified this file with {algorithm} ({})",
                outcome.source_display_name,
                entry.verdict.label()
            ),
        })
    }
}

/// The deterministic answer for one game/library generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PlatformIdentityResolution {
    Unknown {
        generation: u64,
    },
    Resolved {
        generation: u64,
        platform: String,
        display_name: String,
        confidence: PlatformIdentityConfidence,
        evidence: Vec<PlatformIdentityEvidence>,
    },
    Conflict {
        generation: u64,
        evidence: Vec<PlatformIdentityEvidence>,
    },
}

impl PlatformIdentityResolution {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Unknown { generation }
            | Self::Resolved { generation, .. }
            | Self::Conflict { generation, .. } => *generation,
        }
    }

    pub fn platform(&self) -> Option<&str> {
        match self {
            Self::Resolved { platform, .. } => Some(platform),
            Self::Unknown { .. } | Self::Conflict { .. } => None,
        }
    }

    pub fn evidence(&self) -> &[PlatformIdentityEvidence] {
        match self {
            Self::Resolved { evidence, .. } | Self::Conflict { evidence, .. } => evidence,
            Self::Unknown { .. } => &[],
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }
}

/// Resolves all current-generation evidence. Stale evidence is ignored rather
/// than allowed to overwrite a newer game/library generation.
pub fn resolve_platform_identity(
    generation: u64,
    evidence: impl IntoIterator<Item = PlatformIdentityEvidence>,
) -> PlatformIdentityResolution {
    let mut evidence: Vec<_> = evidence
        .into_iter()
        .filter(|item| item.generation == generation)
        .collect();
    evidence.sort_by(|left, right| {
        right
            .source
            .cmp(&left.source)
            .then_with(|| left.platform.cmp(&right.platform))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    evidence.dedup();

    let manual: Vec<_> = evidence
        .iter()
        .filter(|item| item.source == PlatformIdentitySource::Manual)
        .cloned()
        .collect();
    if !manual.is_empty() {
        return settle_tier(generation, manual);
    }

    // DAT and RomM are considered together specifically so contradictory
    // authoritative providers cannot be resolved by whichever arrived first.
    let authoritative: Vec<_> = evidence
        .iter()
        .filter(|item| {
            matches!(
                item.source,
                PlatformIdentitySource::VerifiedDat | PlatformIdentitySource::Romm
            )
        })
        .cloned()
        .collect();
    if !authoritative.is_empty() {
        let authoritative_resolution = settle_tier(generation, authoritative);
        let authoritative_platform = match &authoritative_resolution {
            PlatformIdentityResolution::Conflict { .. } => return authoritative_resolution,
            PlatformIdentityResolution::Resolved { platform, .. } => platform,
            PlatformIdentityResolution::Unknown { .. } => unreachable!("non-empty tier"),
        };
        let existing_strong: Vec<_> = evidence
            .iter()
            .filter(|item| item.source == PlatformIdentitySource::ExistingStrongIdentity)
            .cloned()
            .collect();
        if existing_strong
            .iter()
            .any(|item| item.platform != *authoritative_platform)
        {
            let mut conflicting_evidence = authoritative_resolution.evidence().to_vec();
            conflicting_evidence.extend(existing_strong);
            return PlatformIdentityResolution::Conflict {
                generation,
                evidence: conflicting_evidence,
            };
        }
        return authoritative_resolution;
    }

    for source in [
        PlatformIdentitySource::ExistingStrongIdentity,
        PlatformIdentitySource::Inference,
    ] {
        let tier: Vec<_> = evidence
            .iter()
            .filter(|item| item.source == source)
            .cloned()
            .collect();
        if !tier.is_empty() {
            return settle_tier(generation, tier);
        }
    }

    PlatformIdentityResolution::Unknown { generation }
}

fn settle_tier(
    generation: u64,
    evidence: Vec<PlatformIdentityEvidence>,
) -> PlatformIdentityResolution {
    let platforms: BTreeSet<&str> = evidence.iter().map(|item| item.platform.as_str()).collect();
    if platforms.len() != 1 {
        return PlatformIdentityResolution::Conflict {
            generation,
            evidence,
        };
    }
    let platform = platforms.iter().next().expect("one platform").to_string();
    let confidence = evidence
        .iter()
        .map(|item| item.confidence)
        .max()
        .unwrap_or(PlatformIdentityConfidence::Inferred);
    PlatformIdentityResolution::Resolved {
        generation,
        display_name: display_name_for(&platform).to_string(),
        platform,
        confidence,
        evidence,
    }
}

fn canonical_platform(value: &str) -> Option<&'static str> {
    platform_by_id(value)
        .or_else(|| platform_for_alias(value))
        .map(|platform| platform.id)
}

#[cfg(test)]
mod tests;
