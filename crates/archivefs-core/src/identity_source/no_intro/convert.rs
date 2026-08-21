//! Converts [`DatIndex`] lookup results into lineage-aware
//! [`EvidenceObservation`]s (sections 15-21). Kept separate from
//! [`super::import`] (parsing/hashing) so conversion logic is testable
//! without touching the filesystem at all.

use crate::dat::index::{DatIndex, DatRomRef};
use crate::dat::model::ChecksumAlgorithm;
use crate::platform_evidence_fusion::evidence_lineage::{
    ClaimStrength, ClaimType, EvidenceChannel, EvidenceObservation, IdentityScope, LineageRelation,
    Provenance, Representation, SourceArtifactIdentity, SourceFamily,
};

use super::import::ImportedNoIntroSource;

/// Representation-appropriate exact-match claim (section 15/18) - the same
/// mapping shape as the Hasheous adapter's own `exact_claim_for`, so the two
/// adapters cannot silently diverge on what "exact match" means for a given
/// representation. A representation with no defined exact-match claim falls
/// back to [`ClaimType::PlatformCandidate`] rather than a fabricated one.
pub fn claim_for_representation(representation: Representation) -> ClaimType {
    match representation {
        Representation::PhysicalFile => ClaimType::ExactBytesMatch,
        Representation::NormalizedRom => ClaimType::ExactNormalizedMatch,
        Representation::DiscTrack => ClaimType::ExactTrackMatch,
        Representation::LogicalChd => ClaimType::ExactLogicalDiscMatch,
        Representation::WHDLoadSlave => ClaimType::ExactSlaveMatch,
        _ => ClaimType::PlatformCandidate,
    }
}

fn source_artifact(source: &ImportedNoIntroSource) -> SourceArtifactIdentity {
    SourceArtifactIdentity {
        source_family: SourceFamily::NoIntro,
        upstream_version: source.upstream_version.clone(),
        artifact_sha256: Some(source.artifact_sha256.clone()),
        artifact_name: Some(source.artifact_name.clone()),
    }
}

/// Whether a matched entry carries a cryptographic (SHA1/MD5) identity, vs.
/// only a weak CRC32-only identity (section 36) - a CRC-only row is
/// queryable, but must never be reported as [`ClaimStrength::Strong`]
/// cryptographic exact identity.
fn strength_for(rom: &DatRomRef, matched_algorithm: ChecksumAlgorithm) -> ClaimStrength {
    match matched_algorithm {
        ChecksumAlgorithm::Sha1 | ChecksumAlgorithm::Sha256 => ClaimStrength::Strong,
        ChecksumAlgorithm::Md5 => ClaimStrength::Strong,
        ChecksumAlgorithm::Crc32 => {
            // A CRC-only match without any stronger hash on the same entry
            // stays Corroborated - a real hit, not a cryptographic exact
            // identity claim.
            if rom.checksums.iter().any(|c| {
                matches!(
                    c.algorithm,
                    ChecksumAlgorithm::Sha1 | ChecksumAlgorithm::Md5 | ChecksumAlgorithm::Sha256
                )
            }) {
                ClaimStrength::Strong
            } else {
                ClaimStrength::Corroborated
            }
        }
    }
}

fn observation_for_match(
    source: &ImportedNoIntroSource,
    rom: &DatRomRef,
    representation: Representation,
    matched_algorithm: ChecksumAlgorithm,
    hash_value: &str,
) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::LocalNoIntro,
            upstream_source: SourceFamily::NoIntro,
            upstream_version: source.upstream_version.clone(),
            source_artifact: Some(source_artifact(source)),
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Independent,
            representation,
        },
        claim: claim_for_representation(representation),
        claim_strength: strength_for(rom, matched_algorithm),
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value: Some(hash_value.to_string()),
        platform_candidate: Some(source.system_name.clone()),
        release_candidate: Some(rom.game_name.clone()),
        notes: Some(format!(
            "No-Intro exact match in {} ({})",
            source.system_name, source.artifact_name
        )),
    }
}

/// One title/description observation for a matched entry (section 15/21/26):
/// always [`ClaimType::DisplayMetadata`], never folded into the exact
/// identity claim above.
fn display_observation(source: &ImportedNoIntroSource, rom: &DatRomRef) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::LocalNoIntro,
            upstream_source: SourceFamily::NoIntro,
            upstream_version: source.upstream_version.clone(),
            source_artifact: Some(source_artifact(source)),
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::MetadataOnly,
            representation: Representation::Unknown,
        },
        claim: ClaimType::DisplayMetadata,
        claim_strength: ClaimStrength::DisplayOnly,
        identity_scope: IdentityScope::ReleaseIdentity,
        hash_or_value: None,
        platform_candidate: None,
        release_candidate: Some(rom.game_name.clone()),
        notes: None,
    }
}

/// Looks up one hash against one imported source's index (section 12/14).
/// `representation` is always caller-supplied (section 14/23) - this
/// function never guesses it. Preserves full multiplicity: a CRC collision
/// or a SHA1 shared by multiple rows returns every candidate (sections
/// 37-38), never the first hit.
pub fn lookup_no_intro<'a>(
    index: &'a DatIndex,
    algorithm: ChecksumAlgorithm,
    hash_value: &str,
) -> &'a [DatRomRef] {
    let table = match algorithm {
        ChecksumAlgorithm::Sha1 => &index.by_sha1,
        ChecksumAlgorithm::Md5 => &index.by_md5,
        ChecksumAlgorithm::Crc32 => &index.by_crc32,
        ChecksumAlgorithm::Sha256 => &index.by_sha256,
    };
    table.get(hash_value).map(|v| v.as_slice()).unwrap_or(&[])
}

/// Converts every match for one hash lookup into lineage-aware observations
/// (sections 18/22/38): one exact-identity observation plus one display
/// observation per matched row, never collapsed to the first hit.
pub fn observations_from_no_intro_matches(
    source: &ImportedNoIntroSource,
    algorithm: ChecksumAlgorithm,
    hash_value: &str,
    representation: Representation,
) -> Vec<EvidenceObservation> {
    let matches = lookup_no_intro(&source.index, algorithm, hash_value);
    let mut out = Vec::with_capacity(matches.len() * 2);
    for rom in matches {
        out.push(observation_for_match(
            source,
            rom,
            representation,
            algorithm,
            hash_value,
        ));
        out.push(display_observation(source, rom));
    }
    out
}
