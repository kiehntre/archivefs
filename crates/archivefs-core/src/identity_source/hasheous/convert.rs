//! Converts raw [`HashLookupResponse`] DTOs into lineage-aware
//! [`EvidenceObservation`]s (Batch 20, sections 18-21, 49). This is the
//! adapter's entire evidence-shaping logic, kept deliberately separate from
//! [`super::client`] (pure HTTP) so it can be tested with zero network
//! access, per this batch's own privacy/offline requirements.

use crate::platform_evidence_fusion::evidence_lineage::{
    ClaimStrength, ClaimType, EvidenceChannel, EvidenceObservation, IdentityScope, LineageRelation,
    Provenance, Representation, SourceFamily, hasheous_upstream_for_tag,
};

use super::dto::HashLookupResponse;

/// Representation-appropriate exact-match claim (section 18). A
/// representation this adapter cannot map to an existing exact-match claim
/// falls back to [`ClaimType::PlatformCandidate`] - the closest existing
/// claim type "without lying" (never a fabricated exact-match claim for a
/// representation with no defined one).
fn exact_claim_for(representation: Representation) -> ClaimType {
    match representation {
        Representation::PhysicalFile => ClaimType::ExactBytesMatch,
        Representation::NormalizedRom => ClaimType::ExactNormalizedMatch,
        Representation::DiscTrack => ClaimType::ExactTrackMatch,
        Representation::LogicalChd => ClaimType::ExactLogicalDiscMatch,
        Representation::WHDLoadSlave => ClaimType::ExactSlaveMatch,
        _ => ClaimType::PlatformCandidate,
    }
}

/// The lineage relation for one matched upstream family (section 12): a
/// recognised family is always a [`LineageRelation::Relay`] (Hasheous only
/// ever relays another source's own fact); an unrecognised/unknown tag
/// stays [`LineageRelation::Unknown`] rather than being guessed as a relay
/// of something we could not identify.
fn lineage_for(upstream_source: SourceFamily) -> LineageRelation {
    if upstream_source == SourceFamily::Unknown {
        LineageRelation::Unknown
    } else {
        LineageRelation::Relay
    }
}

fn exact_match_observation(
    tag: &str,
    representation: Representation,
    hash_value: &str,
    release_candidate: Option<String>,
) -> EvidenceObservation {
    let upstream_source = hasheous_upstream_for_tag(tag);
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::Hasheous,
            upstream_source,
            upstream_version: None, // never fabricated from a response timestamp (section 29)
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: lineage_for(upstream_source),
            representation,
        },
        claim: exact_claim_for(representation),
        // A cryptographic exact-hash match is Strong identity evidence
        // regardless of which family it is attributed to - the *lineage*
        // question (independent vs. relay vs. derived) is answered by
        // upstream_source/lineage, never by weakening the claim itself
        // (section 19).
        claim_strength: ClaimStrength::Strong,
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value: Some(hash_value.to_string()),
        platform_candidate: None,
        release_candidate,
        notes: Some(format!("Hasheous relay of upstream tag `{tag}`")),
    }
}

/// Hasheous's own aggregated `platform`/`publisher` fields are top-level on
/// the response, not attributed to one specific upstream DAT family - so,
/// matching [`crate::platform_evidence_fusion::evidence_lineage::romm_display_observation`]'s
/// established convention for a provider's own consolidated metadata, these
/// use [`SourceFamily::GenericMetadata`], never one of the exact-match
/// families found in `signatures` (section 20/21).
fn platform_metadata_observation(platform_name: &str) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::Hasheous,
            upstream_source: SourceFamily::GenericMetadata,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::MetadataOnly,
            representation: Representation::Unknown,
        },
        claim: ClaimType::PlatformCandidate,
        claim_strength: ClaimStrength::Corroborated,
        identity_scope: IdentityScope::PlatformIdentity,
        hash_or_value: None,
        platform_candidate: Some(platform_name.to_string()),
        release_candidate: None,
        notes: Some("Hasheous's own aggregated platform metadata".to_string()),
    }
}

fn display_metadata_observation(claim: ClaimType, value: &str) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::Hasheous,
            upstream_source: SourceFamily::GenericMetadata,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::MetadataOnly,
            representation: Representation::Unknown,
        },
        claim,
        claim_strength: ClaimStrength::DisplayOnly,
        identity_scope: IdentityScope::ReleaseIdentity,
        hash_or_value: None,
        platform_candidate: None,
        release_candidate: Some(value.to_string()),
        notes: None,
    }
}

/// Converts one [`HashLookupResponse`] into a full set of
/// [`EvidenceObservation`]s. `hash_value`/`representation` describe the
/// hash actually searched for - the response never repeats which one that
/// was, so the caller (which made the request) supplies it back for exact
/// attribution.
///
/// Every source in `signatures` is preserved (section 22) - never reduced
/// to the first one - and every game/rom entry under a source is preserved
/// too (section 23), so genuinely distinct releases are never dropped;
/// exact structural duplicates are left for [`super::super::evidence_lineage::dedup_mirror_artifacts`]
/// / [`super::super::evidence_lineage::merge_evidence`]'s own deterministic
/// dedup rather than being collapsed twice.
///
/// `retrieved_at_unix` is stamped onto every returned observation's
/// [`Provenance::retrieved_at_unix`] (section 30) - `imported_at_unix`
/// stays `None`, since this adapter reads a live network response, not a
/// persistent local cache. Passed in rather than read from the clock here
/// so this function stays pure and deterministically testable (section
/// 51); a caller reading a real response should stamp it with the current
/// time at the point of receipt.
pub fn observations_from_hash_lookup(
    response: &HashLookupResponse,
    representation: Representation,
    hash_value: &str,
    retrieved_at_unix: Option<u64>,
) -> Vec<EvidenceObservation> {
    let mut out = Vec::new();

    if let Some(signatures) = &response.signatures {
        // `signatures` is a `BTreeMap`, so iteration order is already the
        // source-tag's own deterministic lexical order (section 39/53).
        for (tag, results) in signatures {
            for result in results {
                let release_candidate = result.game.as_ref().and_then(|game| game.name.clone());
                out.push(exact_match_observation(
                    tag,
                    representation,
                    hash_value,
                    release_candidate,
                ));
            }
        }
    } else if let Some(single) = &response.signature {
        // Fallback for a hypothetical `returnAllSources=false` response
        // shape - this adapter always requests `true`, but a defensive,
        // honestly-labelled fallback is safer than silently dropping a
        // match if a future server default ever changed (section 24).
        let tag = single
            .rom
            .as_ref()
            .and_then(|rom| rom.signature_source.clone())
            .unwrap_or_else(|| "Unknown".to_string());
        let release_candidate = single.game.as_ref().and_then(|game| game.name.clone());
        out.push(exact_match_observation(
            &tag,
            representation,
            hash_value,
            release_candidate,
        ));
    }

    if let Some(platform) = &response.platform
        && let Some(name) = &platform.name
    {
        out.push(platform_metadata_observation(name));
    }
    if let Some(publisher) = &response.publisher
        && let Some(name) = &publisher.name
    {
        out.push(display_metadata_observation(
            ClaimType::DisplayMetadata,
            name,
        ));
    }

    for observation in &mut out {
        observation.provenance.retrieved_at_unix = retrieved_at_unix;
    }
    out
}
