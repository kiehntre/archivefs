//! Batch 8: the thin end-to-end identity orchestrator (milestone sections
//! 22-23) - composes existing pieces, adds no new decision logic of its
//! own. Every field on [`IdentityResult`] is produced by a function this
//! crate already has and already tests independently
//! ([`super::fuse_platform_evidence`], [`super::combined_identity::combine_identity`],
//! [`super::archive_set_identity::classify_archive_set`],
//! [`super::dat_hash_representation::compare_representations`]) - this
//! module only assembles the call graph and the result shape a caller
//! (CLI/probe/GUI) actually wants in one place.

use crate::content_evidence::ContentEvidence;
use crate::dat::identity::DatPlatformIdentity;

use super::archive_set_identity::{ArchiveSetIdentity, classify_archive_set};
use super::combined_identity::{CombinedIdentityView, combine_identity};
use super::dat_hash_representation::RepresentationMatchOutcome;
use super::{ResolutionExplanation, fuse_platform_evidence};

/// One end-to-end identity result - milestone section 23. Read-only data;
/// no field or method here can mutate a filesystem, database, or RomM
/// record (see `tests::identity_result_carries_no_action_bearing_fields`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResult {
    /// The content-evidence fusion result, pooling every member/view's
    /// evidence the caller supplied - unchanged from
    /// [`fuse_platform_evidence`]'s own output.
    pub content: ResolutionExplanation,
    /// The DAT-source identity, when the caller supplied one (`None` when
    /// no DAT was consulted at all - distinct from
    /// [`DatPlatformIdentity::Unknown`], which means a DAT *was* consulted
    /// and had no opinion).
    pub dat: Option<DatPlatformIdentity>,
    /// Content lane vs. DAT-source lane, when both are available.
    pub combined: Option<CombinedIdentityView>,
    /// The physical-vs-normalized DAT hash match outcome, when the caller
    /// computed one (a separate, per-file hash question from `combined`,
    /// which is about DAT-*source* identity - see
    /// [`super::dat_hash_representation`]'s own module documentation for
    /// why these stay separate).
    pub representation_match: Option<RepresentationMatchOutcome>,
    /// The archive set-identity axis, when the input was an archive with
    /// more than one candidate member (`None` for a single bare file).
    pub set_identity: Option<ArchiveSetIdentity>,
    /// Caveats a renderer should surface verbatim - never used internally
    /// to change any decision, only accumulated for display (milestone
    /// section 23's "caveats" field).
    pub caveats: Vec<&'static str>,
}

impl IdentityResult {
    /// Whether any lane found a genuine conflict - content/DAT disagreement
    /// ([`CombinedIdentityView`]), a raw/normalized hash disagreement
    /// ([`RepresentationMatchOutcome::Disagree`]), or a multi-platform
    /// archive ([`ArchiveSetIdentity::MultiPlatform`]). A caller checking
    /// "is this trustworthy" should check this before anything else.
    pub fn has_conflict(&self) -> bool {
        self.combined
            .as_ref()
            .is_some_and(|c| c.relationship.is_conflict())
            || self
                .representation_match
                .as_ref()
                .is_some_and(RepresentationMatchOutcome::is_conflict)
            || self
                .set_identity
                .as_ref()
                .is_some_and(ArchiveSetIdentity::is_conflict)
    }
}

/// Inputs to [`inspect_identity`] - deliberately all optional beyond
/// `content_evidence`, so a caller with only a bare file (no DAT, no
/// archive) still gets a real, honest [`IdentityResult`].
#[derive(Debug, Clone, Default)]
pub struct IdentityInspectionInput {
    /// Content evidence already gathered by a caller (disc_probe/
    /// cartridge_probe/archive-member observation) - this orchestrator
    /// never parses bytes itself.
    pub content_evidence: Vec<ContentEvidence>,
    pub dat: Option<DatPlatformIdentity>,
    pub representation_match: Option<RepresentationMatchOutcome>,
    /// Per-member evidence, when the input was an archive - `(member_index,
    /// evidence)` pairs, the same shape
    /// [`super::archive_set_identity::classify_archive_set`] takes.
    pub archive_members: Option<Vec<(usize, Vec<ContentEvidence>)>>,
}

/// Composes existing pieces into one [`IdentityResult`] - the thin
/// orchestrator milestone section 22 asks for. Pure and read-only: no I/O,
/// no mutation, no action authority of any kind.
pub fn inspect_identity(input: IdentityInspectionInput) -> IdentityResult {
    let content = fuse_platform_evidence(input.content_evidence);
    let combined = input
        .dat
        .as_ref()
        .map(|dat| combine_identity(&content, dat));
    let set_identity = input
        .archive_members
        .as_ref()
        .map(|members| classify_archive_set(members));

    let mut caveats = Vec::new();
    if combined
        .as_ref()
        .is_some_and(|c| c.relationship.is_conflict())
    {
        caveats.push("content and DAT-source identity disagree on the platform");
    }
    if input
        .representation_match
        .as_ref()
        .is_some_and(RepresentationMatchOutcome::is_conflict)
    {
        caveats
            .push("physical and normalized byte representations matched different DAT identities");
    }
    if set_identity
        .as_ref()
        .is_some_and(ArchiveSetIdentity::is_conflict)
    {
        caveats.push("archive contains members resolving to different, non-equivalent platforms");
    }
    if content.outcome == super::FusionOutcome::Ambiguous {
        caveats.push("content evidence is ambiguous - no platform-specific strong leg fired");
    }

    IdentityResult {
        content,
        dat: input.dat,
        combined,
        representation_match: input.representation_match,
        set_identity,
        caveats,
    }
}

#[cfg(test)]
mod tests;
