//! Batch 19: source-lineage / provenance foundation.
//!
//! # The thesis every future adapter must obey
//!
//! **Observation != Channel != UpstreamSource.**
//!
//! - An [`EvidenceObservation`] is one fact returned/read at one time.
//! - An [`EvidenceChannel`] is *how EmuWiz received* that fact (Hasheous,
//!   RomM, a locally parsed DAT, a locally parsed byte header, ...).
//! - A [`SourceFamily`] is the underlying preservation corpus the fact is
//!   *actually about* (No-Intro, TOSEC, Redump, ...).
//!
//! A channel is never itself a source. Hasheous and RomM are relays: they
//! forward facts that are really about No-Intro, Redump, TOSEC, WHDLoad,
//! MAME, etc. Six channels repeating one upstream source must remain
//! representable as six observations grouped under **one** upstream
//! lineage - never inflated into six independent votes. See
//! [`merge_evidence`] and [`AgreementStatus::SameSourceAgreement`].
//!
//! Genuinely independent evidence lanes (a structural byte-header detector
//! plus a real No-Intro hash match) must still be classifiable as real
//! independent agreement ([`AgreementStatus::IndependentAgreement`]).
//! Derived/relay relationships (MAMERedump derived from Redump) are
//! distinguished from independence, so a disagreement between a direct
//! source and its own derivative reads as "stale derivative / mapping
//! drift" ([`AgreementStatus::DerivedSourceConflict`]), not "two
//! preservation authorities disagree"
//! ([`AgreementStatus::IndependentSourceConflict`]).
//!
//! # What this module is not
//!
//! This is architecture, not integration. Nothing here calls a network,
//! fabricates a DAT match, changes [`crate::platform_evidence_fusion`]'s
//! existing content-fusion behavior, or touches library planning /
//! transaction execution in any way. It exists *alongside*
//! [`crate::platform_evidence_fusion::combined_identity`] and
//! [`crate::dat::identity`], which remain fully unchanged; a handful of
//! pure `observation_from_*` bridge functions turn their already-existing
//! output into lineage-aware observations without altering how those
//! modules compute anything.
//!
//! # No numeric voting
//!
//! There is deliberately no `observations.len()`-as-confidence anywhere in
//! this module's public API. [`independent_source_group_count`] exists
//! specifically so "how many observations exist" and "how many
//! *independent upstream families* agree" are never the same question -
//! see `tests::independent_source_group_count_is_not_observation_count`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence};
use crate::dat::identity::{DatPlatformConfidence, DatPlatformEvidence};

// ---------------------------------------------------------------------
// Vocabulary (sections 3-10)
// ---------------------------------------------------------------------

/// How EmuWiz received one fact. Provider-agnostic and semantic: a channel
/// is never treated as an [`SourceFamily`] anywhere in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceChannel {
    LocalDat,
    LocalMame,
    LocalRedump,
    LocalTosec,
    LocalNoIntro,
    LocalWHDLoad,
    /// This crate's own byte-level structural detectors
    /// ([`crate::content_evidence`] and friends) - a local fact, never an
    /// external preservation source (section 32).
    LocalStructural,
    Hasheous,
    RomM,
    GeneratedIndex,
    DirectMetadataProvider,
    Unknown,
}

/// The underlying preservation corpus a fact is actually about.
/// `Unknown` always exists and is the honest default when lineage cannot
/// be determined - never fabricated (section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFamily {
    NoIntro,
    TOSEC,
    Redump,
    MAMEArcade,
    MAMESoftwareList,
    MAMERedump,
    WHDLoad,
    Retroplay,
    PureDOS,
    TotalDOSCollection,
    FBNeo,
    RetroAchievements,
    ScreenScraper,
    GenericMetadata,
    Unknown,
}

/// A provenance relationship - not a confidence score (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageRelation {
    /// A genuinely separate evidence lane with no shared upstream root.
    Independent,
    /// The same upstream source, observed through a different channel
    /// (Hasheous relaying No-Intro; RomM relaying No-Intro).
    SameSourceDifferentChannel,
    /// A derivative corpus built from another (MAMERedump from Redump).
    DerivedFrom,
    /// A channel that only relays another source's own facts without
    /// adding independent authority of its own.
    Relay,
    /// Display-only metadata with no preservation-authority claim.
    MetadataOnly,
    Unknown,
}

/// Which bytes/artifact an observation is actually about. Never merged
/// implicitly - a physical file and its normalized form are related but
/// distinct observations (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    PhysicalFile,
    NormalizedRom,
    ArchiveMember,
    DiscTrack,
    LogicalChd,
    RawDisc,
    SoftwareListMember,
    WHDLoadSlave,
    WHDLoadInstallFile,
    WholeArchive,
    WholeHdf,
    StructuralMetadata,
    Unknown,
}

/// A domain-specific claim category - deliberately never a generic
/// `verified = true` (section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    ExactBytesMatch,
    ExactNormalizedMatch,
    ExactTrackMatch,
    ExactLogicalDiscMatch,
    ExactSlaveMatch,
    PlatformCandidate,
    ReleaseCandidate,
    RevisionCandidate,
    RegionMetadata,
    LanguageMetadata,
    VariantStatus,
    HardwareCompatibility,
    DisplayMetadata,
    CrosswalkCandidate,
    VettedCrosswalk,
    EquivalentCanonical,
    RelatedPlatform,
}

/// Categorical claim strength. Deliberately **not** numeric - source
/// quality (`SourceFamily`/`LineageRelation`) and claim strength stay
/// separate concepts (section 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrength {
    DisplayOnly,
    Weak,
    Corroborated,
    Strong,
}

/// The scope at which two observations can be said to agree (section 28).
/// A TOSEC crack, a No-Intro original, and a WHDLoad install can agree at
/// [`Self::GameIdentity`] while differing at [`Self::DumpIdentity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityScope {
    ByteIdentity,
    DumpIdentity,
    ReleaseIdentity,
    GameIdentity,
    PlatformIdentity,
}

// ---------------------------------------------------------------------
// Provenance / observation objects (sections 11-14)
// ---------------------------------------------------------------------

/// Deterministic identity of the delivered artifact itself (a DAT file, a
/// generated index, ...) - not the ROM the artifact describes. Two mirrors
/// with the same `artifact_sha256` are the same delivery (section 13/29).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceArtifactIdentity {
    pub source_family: SourceFamily,
    pub upstream_version: Option<String>,
    pub artifact_sha256: Option<String>,
    pub artifact_name: Option<String>,
}

/// Compact provenance for one [`EvidenceObservation`] (section 11). No URL
/// field: this batch avoids network-specific churn entirely.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Provenance {
    pub channel: EvidenceChannel,
    pub upstream_source: SourceFamily,
    pub upstream_version: Option<String>,
    pub source_artifact: Option<SourceArtifactIdentity>,
    pub imported_at_unix: Option<u64>,
    pub retrieved_at_unix: Option<u64>,
    pub generator_version: Option<String>,
    pub lineage: LineageRelation,
    pub representation: Representation,
}

impl Provenance {
    /// Whether an upstream version is on record at all - the section 41
    /// "version known / unknown" helper. No freshness *policy* is
    /// implemented here.
    pub fn version_known(&self) -> bool {
        self.upstream_version.is_some()
    }
}

/// One observed fact, fully attributed (section 12). Every future adapter
/// must be able to construct this - see [`observation_declares_provenance`]
/// for the minimum contract check (section 56).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceObservation {
    pub provenance: Provenance,
    pub claim: ClaimType,
    pub claim_strength: ClaimStrength,
    pub identity_scope: IdentityScope,
    /// A hash (physical or normalized), a release/track/slave identifier,
    /// or any other exact matched value - `None` for structural claims
    /// that carry no comparable value of their own.
    pub hash_or_value: Option<String>,
    pub platform_candidate: Option<String>,
    pub release_candidate: Option<String>,
    pub notes: Option<String>,
}

/// The section 56 adapter contract, made checkable rather than merely
/// documented: an observation whose channel *and* upstream source are both
/// [`EvidenceChannel::Unknown`]/[`SourceFamily::Unknown`] is a bare,
/// provenance-free claim - still representable (never rejected/dropped),
/// but easy for a caller or a test to flag rather than silently trust.
pub fn observation_declares_provenance(observation: &EvidenceObservation) -> bool {
    observation.provenance.channel != EvidenceChannel::Unknown
        || observation.provenance.upstream_source != SourceFamily::Unknown
}

/// The asserted value a claim is actually about, for comparison purposes -
/// whichever of `hash_or_value`/`release_candidate`/`platform_candidate` is
/// present, in that priority order (an exact hash is more specific than a
/// release id, which is more specific than a bare platform string).
fn asserted_value(observation: &EvidenceObservation) -> Option<&str> {
    observation
        .hash_or_value
        .as_deref()
        .or(observation.release_candidate.as_deref())
        .or(observation.platform_candidate.as_deref())
}

// ---------------------------------------------------------------------
// Deterministic ordering / dedup keys (sections 14-15, 39)
// ---------------------------------------------------------------------

/// A fully-ordered, deterministic sort key for one observation - collection
/// order never affects merge/render output (section 39).
type SortKey<'a> = (
    ClaimType,
    EvidenceChannel,
    SourceFamily,
    Option<&'a str>,
    Representation,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

fn sort_key(observation: &EvidenceObservation) -> SortKey<'_> {
    (
        observation.claim,
        observation.provenance.channel,
        observation.provenance.upstream_source,
        observation.provenance.upstream_version.as_deref(),
        observation.provenance.representation,
        observation.hash_or_value.as_deref(),
        observation.platform_candidate.as_deref(),
        observation.release_candidate.as_deref(),
    )
}

fn sorted(observations: &[EvidenceObservation]) -> Vec<EvidenceObservation> {
    let mut out = observations.to_vec();
    out.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    out
}

/// The deterministic same-source dedup key (section 15). Two observations
/// with an equal key are the same upstream fact seen twice - never two
/// different representations collapsed together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SameSourceDedupKey {
    pub source_family: SourceFamily,
    pub upstream_version: Option<String>,
    pub artifact_sha256: Option<String>,
    pub representation: Representation,
    pub claim: ClaimType,
    pub value: Option<String>,
    pub release_candidate: Option<String>,
}

pub fn dedup_key(observation: &EvidenceObservation) -> SameSourceDedupKey {
    SameSourceDedupKey {
        source_family: observation.provenance.upstream_source,
        upstream_version: observation.provenance.upstream_version.clone(),
        artifact_sha256: observation
            .provenance
            .source_artifact
            .as_ref()
            .and_then(|artifact| artifact.artifact_sha256.clone()),
        representation: observation.provenance.representation,
        claim: observation.claim,
        value: observation.hash_or_value.clone(),
        release_candidate: observation.release_candidate.clone(),
    }
}

/// Drops mirror duplicates: observations whose [`SourceArtifactIdentity`]
/// carries an `artifact_sha256` already seen are the same delivery and are
/// removed (section 29). Observations with no artifact hash, or a distinct
/// one, are always preserved.
pub fn dedup_mirror_artifacts(observations: &[EvidenceObservation]) -> Vec<EvidenceObservation> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for observation in sorted(observations) {
        if let Some(hash) = observation
            .provenance
            .source_artifact
            .as_ref()
            .and_then(|artifact| artifact.artifact_sha256.clone())
            && !seen.insert(hash)
        {
            continue;
        }
        out.push(observation);
    }
    out
}

// ---------------------------------------------------------------------
// Lineage grouping (sections 16-19, 22)
// ---------------------------------------------------------------------

/// Every observation grouped under one upstream lineage. `Unknown` lineage
/// observations are never merged with each other - each stays its own
/// singleton group, since two `Unknown` facts are not provably the *same*
/// unknown source (section 19).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageGroup {
    pub source_family: SourceFamily,
    pub observations: Vec<EvidenceObservation>,
}

pub fn group_by_lineage(observations: &[EvidenceObservation]) -> Vec<LineageGroup> {
    let mut known: BTreeMap<SourceFamily, Vec<EvidenceObservation>> = BTreeMap::new();
    let mut unknown_singles: Vec<EvidenceObservation> = Vec::new();
    for observation in sorted(observations) {
        if observation.provenance.upstream_source == SourceFamily::Unknown {
            unknown_singles.push(observation);
        } else {
            known
                .entry(observation.provenance.upstream_source)
                .or_default()
                .push(observation);
        }
    }
    let mut groups: Vec<LineageGroup> = known
        .into_iter()
        .map(|(source_family, observations)| LineageGroup {
            source_family,
            observations,
        })
        .collect();
    for observation in unknown_singles {
        groups.push(LineageGroup {
            source_family: SourceFamily::Unknown,
            observations: vec![observation],
        });
    }
    groups
}

/// How many *independent, non-`Unknown` upstream families* are represented,
/// deliberately never `observations.len()`. This is the only "count" this
/// module exposes, and it means something specific: it is not a vote tally
/// (section 22).
pub fn independent_source_group_count(observations: &[EvidenceObservation]) -> usize {
    observations
        .iter()
        .map(|observation| observation.provenance.upstream_source)
        .filter(|family| *family != SourceFamily::Unknown)
        .collect::<BTreeSet<_>>()
        .len()
}

// ---------------------------------------------------------------------
// Source dependency registry (section 20-21)
// ---------------------------------------------------------------------

/// Known, reviewed derivation relationships only - never a speculative
/// graph (section 21). `None` means "not a known derivative," not "proven
/// independent."
pub fn known_derivation(family: SourceFamily) -> Option<SourceFamily> {
    match family {
        SourceFamily::MAMERedump => Some(SourceFamily::Redump),
        _ => None,
    }
}

/// Maps a Hasheous-reported source tag to the upstream family it actually
/// names. Hasheous itself is never returned - it is a channel, never a
/// source (section 20/57). Unrecognized tags conservatively resolve to
/// [`SourceFamily::Unknown`] rather than a guess.
///
/// Batch 20 extended this with the exact
/// `RomSignatureObject_Game_Rom_SignatureSourceType` tag strings verified
/// against the live `https://hasheous.org/swagger/v1/swagger.json` document
/// (`NoIntros`, `PureDOSDAT`, `TotalDOSCollection`, `ScreenScraper`,
/// `Generic`) alongside the original researched variants, which are kept
/// for backward compatibility with any caller already using them. Two live
/// tags - `Pleasuredome` and `eXo` - have no corresponding [`SourceFamily`]
/// variant and are deliberately left unmapped (falling through to
/// `Unknown`) rather than overclaiming a relationship to an existing
/// variant that isn't actually the same corpus (section 21).
pub fn hasheous_upstream_for_tag(tag: &str) -> SourceFamily {
    match tag {
        "NoIntro" | "nointro" | "No-Intro" | "NoIntros" => SourceFamily::NoIntro,
        "TOSEC" | "tosec" => SourceFamily::TOSEC,
        "Redump" | "redump" => SourceFamily::Redump,
        "MAMERedump" | "mameredump" => SourceFamily::MAMERedump,
        "MAMEArcade" | "MAME" | "mame" => SourceFamily::MAMEArcade,
        "MAMESoftwareList" | "MAMEMess" | "mamemess" => SourceFamily::MAMESoftwareList,
        "WHDLoad" | "whdload" => SourceFamily::WHDLoad,
        "Retroplay" | "retroplay" => SourceFamily::Retroplay,
        "FBNeo" | "fbneo" => SourceFamily::FBNeo,
        "PureDOS" | "PureDOSDAT" => SourceFamily::PureDOS,
        "TotalDOSCollection" => SourceFamily::TotalDOSCollection,
        "RetroAchievements" => SourceFamily::RetroAchievements,
        "ScreenScraper" => SourceFamily::ScreenScraper,
        "Generic" | "GenericMetadata" => SourceFamily::GenericMetadata,
        _ => SourceFamily::Unknown,
    }
}

/// Maps a RomM boolean match-flag field name to the upstream family it
/// relays. RomM itself is never returned as a source (section 20/58).
pub fn romm_upstream_for_flag(flag: &str) -> SourceFamily {
    match flag {
        "nointro_match" => SourceFamily::NoIntro,
        "redump_match" => SourceFamily::Redump,
        "tosec_match" => SourceFamily::TOSEC,
        "mame_redump_match" => SourceFamily::MAMERedump,
        "whdload_match" => SourceFamily::WHDLoad,
        _ => SourceFamily::Unknown,
    }
}

// ---------------------------------------------------------------------
// Merge / agreement-conflict model (sections 23-27)
// ---------------------------------------------------------------------

/// A claim-scoped merge outcome (section 23) - never one global status for
/// a whole observation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgreementStatus {
    SameSourceAgreement,
    IndependentAgreement,
    DerivedAgreement,
    CrossRepresentationAgreement,
    WeakAgreement,
    SameSourceVersionConflict,
    DerivedSourceConflict,
    IndependentSourceConflict,
    RepresentationConflict,
    MetadataConflict,
}

impl AgreementStatus {
    pub fn is_conflict(self) -> bool {
        matches!(
            self,
            Self::SameSourceVersionConflict
                | Self::DerivedSourceConflict
                | Self::IndependentSourceConflict
                | Self::RepresentationConflict
                | Self::MetadataConflict
        )
    }
}

/// One claim's merged observations plus the classified relationship
/// between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSummary {
    pub claim: ClaimType,
    pub status: AgreementStatus,
    pub observations: Vec<EvidenceObservation>,
}

fn metadata_only_claim(claim: ClaimType) -> bool {
    matches!(
        claim,
        ClaimType::RegionMetadata
            | ClaimType::LanguageMetadata
            | ClaimType::DisplayMetadata
            | ClaimType::VariantStatus
            | ClaimType::HardwareCompatibility
    )
}

/// Classifies one already claim-scoped, non-empty group of observations
/// into a single [`AgreementStatus`] via explicit rules - never a numeric
/// score (matching this crate's established rule-based-fusion
/// convention). See the module-level test matrix for the exhaustive
/// worked examples this ladder is built from.
fn classify_group(claim: ClaimType, group: &[EvidenceObservation]) -> AgreementStatus {
    if group.len() < 2 {
        return AgreementStatus::WeakAgreement;
    }

    let values: BTreeSet<&str> = group.iter().filter_map(asserted_value).collect();
    let representations: BTreeSet<Representation> = group
        .iter()
        .map(|observation| observation.provenance.representation)
        .collect();
    let families: BTreeSet<SourceFamily> = group
        .iter()
        .map(|observation| observation.provenance.upstream_source)
        .collect();
    let non_unknown_families: BTreeSet<SourceFamily> = families
        .iter()
        .copied()
        .filter(|family| *family != SourceFamily::Unknown)
        .collect();
    let any_unknown_lineage = group.iter().any(|observation| {
        observation.provenance.upstream_source == SourceFamily::Unknown
            || observation.provenance.lineage == LineageRelation::Unknown
    });
    let any_derived = group
        .iter()
        .any(|observation| observation.provenance.lineage == LineageRelation::DerivedFrom);

    let agree = values.len() <= 1;

    if agree {
        if non_unknown_families.len() <= 1 {
            // One shared upstream family (possibly via several channels),
            // or nothing but Unknown observations agreeing by coincidence:
            // either way there is at most one lineage to trust, never
            // independent corroboration.
            return if any_unknown_lineage && non_unknown_families.is_empty() {
                AgreementStatus::WeakAgreement
            } else {
                AgreementStatus::SameSourceAgreement
            };
        }
        if any_derived {
            return AgreementStatus::DerivedAgreement;
        }
        if representations.len() > 1 {
            return AgreementStatus::CrossRepresentationAgreement;
        }
        AgreementStatus::IndependentAgreement
    } else {
        if non_unknown_families.len() <= 1 && !any_unknown_lineage {
            return AgreementStatus::SameSourceVersionConflict;
        }
        if any_derived {
            return AgreementStatus::DerivedSourceConflict;
        }
        if metadata_only_claim(claim) {
            return AgreementStatus::MetadataConflict;
        }
        if representations.len() > 1 {
            return AgreementStatus::RepresentationConflict;
        }
        AgreementStatus::IndependentSourceConflict
    }
}

/// Groups `observations` by [`ClaimType`] (claim-scoped, never one global
/// verdict) and classifies each group. Deterministic regardless of input
/// order (section 39): observations are sorted before grouping and before
/// classification, and exact duplicates collapse.
pub fn merge_evidence(observations: &[EvidenceObservation]) -> Vec<ClaimSummary> {
    let mut by_claim: BTreeMap<ClaimType, Vec<EvidenceObservation>> = BTreeMap::new();
    for observation in sorted(observations) {
        by_claim
            .entry(observation.claim)
            .or_default()
            .push(observation);
    }
    by_claim
        .into_iter()
        .map(|(claim, mut group)| {
            group.dedup();
            let status = classify_group(claim, &group);
            ClaimSummary {
                claim,
                status,
                observations: group,
            }
        })
        .collect()
}

/// Convenience wrapper for a caller that already has one claim-scoped,
/// non-empty group and just wants the classification (used directly by
/// several of this module's own tests, and available to a future caller
/// that has already partitioned observations itself).
pub fn classify_agreement(claim: ClaimType, group: &[EvidenceObservation]) -> AgreementStatus {
    classify_group(claim, group)
}

// ---------------------------------------------------------------------
// Explanation renderer (sections 36-37)
// ---------------------------------------------------------------------

fn describe(observation: &EvidenceObservation) -> String {
    format!(
        "{:?} via {:?}: {}",
        observation.provenance.upstream_source,
        observation.provenance.channel,
        asserted_value(observation).unwrap_or("(no comparable value)")
    )
}

/// A minimal, deterministic, human-readable text explanation of merged
/// evidence - not a GUI, just a stable rendering test helper (section 36).
pub fn render_evidence_summary(observations: &[EvidenceObservation]) -> String {
    let summaries = merge_evidence(observations);
    let mut out = String::from("Identity evidence:\n");
    for summary in &summaries {
        let heading = if summary.status.is_conflict() {
            "Conflict"
        } else {
            "Agreement"
        };
        out.push_str(&format!(
            "\n{heading} ({:?}, {:?}):\n",
            summary.claim, summary.status
        ));
        for observation in &summary.observations {
            out.push_str(&format!("  - {}\n", describe(observation)));
        }
    }
    out
}

/// Renders one already-classified conflict summary on its own (section
/// 37's worked example shape).
pub fn render_conflict_explanation(summary: &ClaimSummary) -> String {
    let mut out = format!("Conflict:\n  status: {:?}\n", summary.status);
    for observation in &summary.observations {
        out.push_str(&format!("  {}\n", describe(observation)));
    }
    out
}

// ---------------------------------------------------------------------
// Bridges from existing evidence models (sections 31-33)
// ---------------------------------------------------------------------

/// Bridges one already-computed [`DatPlatformEvidence`] fact (from
/// [`crate::dat::identity`], unmodified) into a lineage-aware observation.
/// The DAT parser is never rewritten or re-invoked here. Current DAT
/// metadata cannot establish a preservation-corpus family on its own, so
/// `upstream_source` is honestly [`SourceFamily::Unknown`] unless a caller
/// separately knows which corpus this DAT actually is (section 31).
pub fn observation_from_dat_platform_evidence(
    evidence: &DatPlatformEvidence,
    representation: Representation,
) -> EvidenceObservation {
    let claim_strength = match evidence.confidence {
        DatPlatformConfidence::Strong => ClaimStrength::Strong,
        DatPlatformConfidence::Corroborated => ClaimStrength::Corroborated,
        DatPlatformConfidence::Weak => ClaimStrength::Weak,
    };
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::LocalDat,
            upstream_source: SourceFamily::Unknown,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Unknown,
            representation,
        },
        claim: ClaimType::PlatformCandidate,
        claim_strength,
        identity_scope: IdentityScope::PlatformIdentity,
        hash_or_value: None,
        platform_candidate: Some(evidence.platform.clone()),
        release_candidate: None,
        notes: Some(evidence.detail.clone()),
    }
}

/// Bridges one already-computed [`ContentEvidence`] structural fact into a
/// lineage-aware observation, tagged [`EvidenceChannel::LocalStructural`] -
/// never forced into a preservation [`SourceFamily`], since a byte-level
/// detector is not an external preservation source (section 32).
pub fn observation_from_content_evidence(fact: &ContentEvidence) -> EvidenceObservation {
    let claim_strength = match fact.confidence {
        ContentEvidenceConfidence::Strong => ClaimStrength::Strong,
        ContentEvidenceConfidence::Corroborated => ClaimStrength::Corroborated,
        ContentEvidenceConfidence::Weak => ClaimStrength::Weak,
    };
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::LocalStructural,
            upstream_source: SourceFamily::Unknown,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Independent,
            representation: Representation::StructuralMetadata,
        },
        claim: ClaimType::PlatformCandidate,
        claim_strength,
        identity_scope: IdentityScope::PlatformIdentity,
        hash_or_value: None,
        platform_candidate: Some(fact.value.clone()),
        release_candidate: None,
        notes: Some(fact.detail.clone()),
    }
}

/// Builds a physical-vs-normalized pair of observations for the same exact
/// hash claim, demonstrating representation separation (section 33) -
/// conceptual only, no real ROM access. `physical_hash`/`normalized_hash`
/// are independent optional values; when both are `Some` the two returned
/// observations never collapse into one even if the two hash strings
/// happen to be identical.
pub fn observations_from_physical_and_normalized(
    channel: EvidenceChannel,
    upstream_source: SourceFamily,
    physical_hash: Option<String>,
    normalized_hash: Option<String>,
) -> Vec<EvidenceObservation> {
    let mut out = Vec::new();
    let base_provenance = |representation: Representation| Provenance {
        channel,
        upstream_source,
        upstream_version: None,
        source_artifact: None,
        imported_at_unix: None,
        retrieved_at_unix: None,
        generator_version: None,
        lineage: if upstream_source == SourceFamily::Unknown {
            LineageRelation::Unknown
        } else {
            LineageRelation::Independent
        },
        representation,
    };
    if let Some(hash) = physical_hash {
        out.push(EvidenceObservation {
            provenance: base_provenance(Representation::PhysicalFile),
            claim: ClaimType::ExactBytesMatch,
            claim_strength: ClaimStrength::Strong,
            identity_scope: IdentityScope::DumpIdentity,
            hash_or_value: Some(hash),
            platform_candidate: None,
            release_candidate: None,
            notes: None,
        });
    }
    if let Some(hash) = normalized_hash {
        out.push(EvidenceObservation {
            provenance: base_provenance(Representation::NormalizedRom),
            claim: ClaimType::ExactNormalizedMatch,
            claim_strength: ClaimStrength::Strong,
            identity_scope: IdentityScope::DumpIdentity,
            hash_or_value: Some(hash),
            platform_candidate: None,
            release_candidate: None,
            notes: None,
        });
    }
    out
}

// ---------------------------------------------------------------------
// Generated-index provenance (section 30)
// ---------------------------------------------------------------------

/// Builds an observation for a generated index, keeping the *original*
/// upstream family visible rather than letting the generator become a new
/// source (section 30). `generator_version` should always be supplied when
/// known; the generated index's own artifact hash is optional.
pub fn observation_from_generated_index(
    upstream_source: SourceFamily,
    generator_version: Option<String>,
    artifact_sha256: Option<String>,
    claim: ClaimType,
    hash_or_value: Option<String>,
) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::GeneratedIndex,
            upstream_source,
            upstream_version: None,
            source_artifact: Some(SourceArtifactIdentity {
                source_family: upstream_source,
                upstream_version: None,
                artifact_sha256,
                artifact_name: None,
            }),
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version,
            lineage: LineageRelation::SameSourceDifferentChannel,
            representation: Representation::Unknown,
        },
        claim,
        claim_strength: ClaimStrength::Corroborated,
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value,
        platform_candidate: None,
        release_candidate: None,
        notes: None,
    }
}

// ---------------------------------------------------------------------
// Synthetic adapter-contract demonstrations (sections 57-60)
// ---------------------------------------------------------------------

/// Demonstrates the Hasheous contract (section 57): the response's own
/// `source` tag decides `upstream_source` - Hasheous itself never becomes
/// one.
pub fn hasheous_observation(
    source_tag: &str,
    representation: Representation,
    claim: ClaimType,
    hash_or_value: Option<String>,
    platform_candidate: Option<String>,
) -> EvidenceObservation {
    let upstream_source = hasheous_upstream_for_tag(source_tag);
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::Hasheous,
            upstream_source,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: if upstream_source == SourceFamily::Unknown {
                LineageRelation::Unknown
            } else {
                LineageRelation::Relay
            },
            representation,
        },
        claim,
        claim_strength: if upstream_source == SourceFamily::Unknown {
            ClaimStrength::Weak
        } else {
            ClaimStrength::Strong
        },
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value,
        platform_candidate,
        release_candidate: None,
        notes: Some(format!("Hasheous relay of upstream tag `{source_tag}`")),
    }
}

/// Demonstrates the RomM contract (section 58): a boolean match-flag field
/// name decides `upstream_source` via [`romm_upstream_for_flag`] - RomM
/// itself never becomes one. RomM's own title/slug should always be
/// carried as [`ClaimType::DisplayMetadata`] via [`romm_display_observation`],
/// never folded into this match claim.
pub fn romm_match_observation(
    flag: &str,
    representation: Representation,
    claim: ClaimType,
    hash_or_value: Option<String>,
) -> EvidenceObservation {
    let upstream_source = romm_upstream_for_flag(flag);
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::RomM,
            upstream_source,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: if upstream_source == SourceFamily::Unknown {
                LineageRelation::Unknown
            } else {
                LineageRelation::Relay
            },
            representation,
        },
        claim,
        claim_strength: if upstream_source == SourceFamily::Unknown {
            ClaimStrength::Weak
        } else {
            ClaimStrength::Strong
        },
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value,
        platform_candidate: None,
        release_candidate: None,
        notes: Some(format!("RomM relay of match flag `{flag}`")),
    }
}

/// RomM's own title/slug: always [`ClaimType::DisplayMetadata`], never a
/// preservation-source claim (section 58).
pub fn romm_display_observation(title_or_slug: String) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::RomM,
            upstream_source: SourceFamily::GenericMetadata,
            upstream_version: None,
            source_artifact: None,
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
        release_candidate: Some(title_or_slug),
        notes: None,
    }
}

#[cfg(test)]
mod tests;
