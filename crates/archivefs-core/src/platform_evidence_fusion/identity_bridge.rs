//! The Batch 6 fusion → platform identity bridge.
//!
//! ```text
//! ContentEvidence -> fuse_platform_evidence -> ResolutionExplanation
//!     -> to_identity_evidence (this module)
//!     -> PlatformIdentityEvidence (crate::platform::identity)
//!     -> resolve_platform_identity (unchanged, existing)
//!     -> PlatformIdentityResolution
//! ```
//!
//! # Why this is a bridge, not a merge
//!
//! [`crate::platform::identity`] already has a complete, reviewed
//! provenance/confidence vocabulary (`PlatformIdentitySource`,
//! `PlatformIdentityConfidence`) built for RomM/DAT/manual providers. This
//! module does **not** invent a parallel vocabulary for fusion - it adapts
//! [`ResolutionExplanation`] into that *existing* vocabulary, at the
//! weakest, most conservative tier that vocabulary already has
//! ([`PlatformIdentitySource::Inference`]), so a caller who already
//! collects [`PlatformIdentityEvidence`] from RomM/DAT/manual sources can
//! add fusion's own opinion to the same pool without learning a second
//! model.
//!
//! # The outcome mapping
//!
//! - [`FusionOutcome::Resolved`] -> exactly one
//!   [`PlatformIdentityEvidence`] at [`PlatformIdentitySource::Inference`] /
//!   [`PlatformIdentityConfidence::Strong`]. `Strong`, not `Verified` or
//!   `High`: those tiers are reserved for cryptographic DAT proof and
//!   matched RomM records respectively (see
//!   [`PlatformIdentityEvidence::from_verified_dat`]/`from_romm`) - reusing
//!   them here would make a content-fusion inference indistinguishable
//!   from actual DAT/RomM proof, which is exactly what this milestone's
//!   "never make fusion indistinguishable from DAT proof" rule forbids.
//!   `Strong` is nonetheless justified (not the weaker `Inferred`) because
//!   [`fuse_platform_evidence`] only ever reaches `Resolved` when a
//!   reviewed, platform-specific `Strong`-tier leg fired - never from
//!   Corroborated/Weak facts alone (see [`FusionRule::has_strong_leg`]).
//! - [`FusionOutcome::Unknown`] -> no evidence at all. There is nothing
//!   honest to assert.
//! - [`FusionOutcome::Ambiguous`] -> no evidence at all, deliberately -
//!   see "Why Ambiguous produces nothing" below.
//! - [`FusionOutcome::Conflict`] -> one [`PlatformIdentityEvidence`] per
//!   conflicting platform, all still at `Inference`/`Strong` (never
//!   silently downgraded - both platforms really did each fire a genuine
//!   `Strong`-eligible rule). This does not need a new "conflict" concept
//!   at the identity layer: [`crate::platform::identity::resolve_platform_identity`]'s
//!   existing `settle_tier` already turns more than one distinct platform
//!   value within one source tier into
//!   [`crate::platform::identity::PlatformIdentityResolution::Conflict`] on
//!   its own - reusing that existing mechanism is exactly "do not create
//!   redundant parallel enums unless unavoidable."
//!
//! # Why `Ambiguous` produces nothing
//!
//! [`PlatformIdentityEvidence`] asserts exactly one platform per item; it
//! has no "candidate list" shape. Two temptations exist for `Ambiguous`,
//! and both are wrong:
//!
//! 1. Emit one item per candidate at `Inference`. This is actively
//!    dangerous: `resolve_platform_identity`'s `settle_tier` would see
//!    multiple distinct platform values in the same tier and report
//!    [`crate::platform::identity::PlatformIdentityResolution::Conflict`]
//!    - fabricating a conflict between candidates that were never actually
//!    contradictory, only individually insufficient. `Ambiguous` and
//!    `Conflict` are different fusion outcomes for a reason; the bridge
//!    must not collapse them into the same identity-layer shape.
//! 2. Pick fusion's "best" candidate and emit it anyway. This is exactly
//!    the "chase a prettier scoreboard" the milestone's own final rule
//!    forbids - `Ambiguous` means the resolver itself declined to pick a
//!    winner; the bridge has no additional information that would make
//!    that choice safe.
//!
//! The honest answer is silence: a caller sees no fusion-sourced identity
//! evidence for this generation, exactly as if fusion had not run at all.
//! [`FiredCandidate`] information is still available on
//! [`ResolutionExplanation`] itself for a developer probe to display -
//! see [`crate::platform_evidence_fusion`]'s own `print_fusion` usage in
//! `examples/disc_probe.rs` - it is just never promoted to an identity
//! assertion.
//!
//! # Content vs. DAT: a separate lane, on purpose
//!
//! [`crate::platform_evidence_fusion::compare_content_and_dat`] (Batch 5)
//! remains the dedicated content-vs-DAT comparison - this module
//! deliberately does **not** route that comparison through
//! `resolve_platform_identity`'s own multi-provider tier system.
//! `resolve_platform_identity` treats `VerifiedDat` as authoritative and
//! returns as soon as authoritative evidence exists, **without ever
//! consulting `Inference`-tier evidence at all** (see that function's own
//! early return once `authoritative` is non-empty) - which is correct
//! default behavior for RomM/manual/existing-identity callers, but would
//! silently swallow a genuine content-fusion/DAT disagreement if fusion's
//! output were fed in as ordinary `Inference` evidence and DAT evidence
//! happened to also be present: DAT would win, and the disagreement would
//! never be visible. That is precisely what section 6 of this milestone
//! forbids ("DAT must never override strong contradictory internal
//! evidence... both trails survive"). Comparing fusion's own
//! [`ResolutionExplanation`] against a DAT platform id directly, via
//! `compare_content_and_dat`, keeps that comparison visible regardless of
//! which `resolve_platform_identity` tier would otherwise have won.
//!
//! A caller that wants the full picture calls both: `to_identity_evidence`
//! feeds fusion's own opinion into the ordinary multi-provider pool (where
//! it only matters when no RomM/DAT/manual evidence exists at all -
//! exactly the existing, reviewed precedence order), and
//! `compare_content_and_dat` separately reports whether fusion and DAT
//! agree, disagree, or only one of them has an opinion at all - see
//! [`ContentDatIdentityView`].

use crate::platform::identity::{
    PlatformIdentityConfidence, PlatformIdentityEvidence, PlatformIdentitySource,
};

use super::{DatContentComparison, FusionOutcome, ResolutionExplanation, compare_content_and_dat};

/// Adapts one [`ResolutionExplanation`] into zero or more
/// [`PlatformIdentityEvidence`] items for `generation` - see the module
/// documentation for the exact outcome mapping and why `Ambiguous`
/// produces nothing. Never touches a filesystem, database, or RomM/DAT
/// provider; pure data transformation only.
pub fn to_identity_evidence(
    explanation: &ResolutionExplanation,
    generation: u64,
) -> Vec<PlatformIdentityEvidence> {
    match explanation.outcome {
        FusionOutcome::Resolved => {
            let Some(platform) = explanation.resolved_platform else {
                // Structurally should not happen (fuse_platform_evidence
                // only sets Resolved alongside Some(platform)), but this
                // bridge fails closed rather than panicking on a future
                // fusion-side change that violates that invariant.
                return Vec::new();
            };
            let rule_ids: Vec<&str> = explanation
                .fired_candidates
                .iter()
                .filter(|candidate| candidate.platform == platform && candidate.has_strong_leg)
                .map(|candidate| candidate.rule_id)
                .collect();
            let detail = format!(
                "content evidence fusion resolved this platform from a reviewed strong rule ({}); evidence trail retained on the originating ResolutionExplanation",
                rule_ids.join(", ")
            );
            PlatformIdentityEvidence::canonical(
                platform,
                PlatformIdentitySource::Inference,
                PlatformIdentityConfidence::Strong,
                generation,
                detail,
            )
            .into_iter()
            .collect()
        }
        FusionOutcome::Conflict => explanation
            .conflicting_platforms
            .iter()
            .filter_map(|platform| {
                PlatformIdentityEvidence::canonical(
                    platform,
                    PlatformIdentitySource::Inference,
                    PlatformIdentityConfidence::Strong,
                    generation,
                    format!(
                        "content evidence fusion found conflicting reviewed strong evidence naming {platform} among other, non-equivalent platforms - never silently downgraded to a single winner"
                    ),
                )
            })
            .collect(),
        FusionOutcome::Unknown | FusionOutcome::Ambiguous => Vec::new(),
    }
}

/// Both evidence trails for one generation's fusion-vs-DAT question, kept
/// visibly separate - never merged into one opaque verdict. See the module
/// documentation's "Content vs. DAT: a separate lane, on purpose" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDatIdentityView {
    /// What [`to_identity_evidence`] would feed into the ordinary
    /// multi-provider [`crate::platform::identity::resolve_platform_identity`]
    /// pool - empty for `Unknown`/`Ambiguous`, exactly as documented above.
    pub content_identity: Vec<PlatformIdentityEvidence>,
    /// The explicit content-vs-DAT comparison, independent of provider
    /// tier precedence.
    pub dat_comparison: DatContentComparison,
}

/// Builds a [`ContentDatIdentityView`] from one fusion result and a
/// separately obtained DAT platform id (already vetted by the caller
/// against [`crate::dat::audit::AuditVerdict`], exactly as
/// `compare_content_and_dat` itself already requires).
pub fn content_and_dat_identity_view(
    explanation: &ResolutionExplanation,
    generation: u64,
    dat_platform: Option<&'static str>,
) -> ContentDatIdentityView {
    ContentDatIdentityView {
        content_identity: to_identity_evidence(explanation, generation),
        dat_comparison: compare_content_and_dat(explanation, dat_platform),
    }
}

#[cfg(test)]
mod tests;
