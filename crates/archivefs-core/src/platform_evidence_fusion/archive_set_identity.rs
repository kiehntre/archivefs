//! Batch 8: archive **set** identity - a separate axis from platform
//! identity (milestone sections 15, 17-19).
//!
//! # Why this is not [`crate::archive_member_content_evidence::ArchiveContentClassification`]
//!
//! That type (Batch 5) classifies members by whether their raw
//! [`ContentEvidence`] *fact signatures* agree, conflict, or diverge -
//! useful, and reused here as the per-member evidence source, but it never
//! resolves a platform. This module answers a different question: after
//! each member's own evidence is independently run through
//! [`fuse_platform_evidence`], how many *distinct resolved platforms* does
//! the archive actually contain? That is a genuinely different axis
//! (`ArchiveContentClassification::MultiFileSet`, for example, says nothing
//! about whether the members that produced different fact *signatures* also
//! resolved to different *platforms* - they might be the same platform's
//! disc 1/disc 2).
//!
//! Platform identity and set identity are kept as two separate results on
//! purpose (milestone section 15): a caller can legitimately have "all
//! strong members are SNES" (one settled platform) while the *set* is
//! still `MultiMemberSamePlatform`, not a single collapsed game identity.

use std::collections::BTreeSet;

use crate::content_evidence::ContentEvidence;

use super::{fuse_platform_evidence, group_by_equivalence};

/// The archive-content structure axis - milestone section 17. Deliberately
/// separate from any single member's own resolved platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveSetIdentity {
    /// No member's evidence resolved a platform at all.
    Unknown,
    /// Exactly one member resolved a platform - the milestone's "game ROM +
    /// README + cover" case (section 18A): non-game support members never
    /// count here, since they never fuse to a resolved platform in the
    /// first place.
    SingleMember {
        member_index: usize,
        platform: &'static str,
    },
    /// Two or more members resolved platforms, and every one of them is the
    /// same (or an equivalent) canonical platform - milestone section 18B.
    /// The archive's *platform* is settled; its *game/set* identity is not
    /// - see the module documentation.
    MultiMemberSamePlatform {
        member_indices: Vec<usize>,
        platform: &'static str,
    },
    /// Two or more members resolved to genuinely different, non-equivalent
    /// platforms - milestone section 18C. Never picked a winner.
    MultiPlatform {
        member_indices: Vec<usize>,
        platforms: Vec<&'static str>,
    },
    /// A determinable multi-disc/multi-part structure - milestone section
    /// 18D. No detector in this crate produces this variant yet (no
    /// reviewed multi-disc/set-structure signal exists to build it from);
    /// it exists in the type now so a future batch has somewhere to land
    /// one without another enum redesign. Never fabricated - see
    /// `tests::structured_set_is_never_produced_without_a_real_detector`.
    StructuredSet {
        member_indices: Vec<usize>,
        platform: &'static str,
    },
}

impl ArchiveSetIdentity {
    pub fn is_multi_member(&self) -> bool {
        matches!(
            self,
            Self::MultiMemberSamePlatform { .. }
                | Self::MultiPlatform { .. }
                | Self::StructuredSet { .. }
        )
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::MultiPlatform { .. })
    }
}

/// Classifies an archive's set identity from each member's own evidence -
/// `members` is `(member_index, evidence)` pairs, exactly the shape
/// [`crate::archive_member_content_evidence::ArchiveMemberContentResult`]
/// already carries. Each member's evidence is fused **independently**
/// (never pooled across members - pooling belongs to the platform-only
/// question [`fuse_platform_evidence`] already answers when a caller wants
/// "does this archive contain any evidence of platform X at all," a
/// different question from "how many distinct games/platforms does this
/// archive's member structure actually contain").
///
/// Pure and read-only: never opens a file, never mutates `members`, never
/// picks a "winning" member.
pub fn classify_archive_set(members: &[(usize, Vec<ContentEvidence>)]) -> ArchiveSetIdentity {
    let resolved: Vec<(usize, &'static str)> = members
        .iter()
        .filter_map(|(index, evidence)| {
            let explanation = fuse_platform_evidence(evidence.iter().cloned());
            explanation
                .resolved_platform
                .map(|platform| (*index, platform))
        })
        .collect();

    match resolved.len() {
        0 => ArchiveSetIdentity::Unknown,
        1 => ArchiveSetIdentity::SingleMember {
            member_index: resolved[0].0,
            platform: resolved[0].1,
        },
        _ => {
            let platforms: Vec<&'static str> = resolved.iter().map(|(_, p)| *p).collect();
            let groups = group_by_equivalence(&platforms);
            // Sorted so the result never depends on the order `members` was
            // handed in - the member's own index (not its position in the
            // input slice) is the only thing that should decide ordering
            // here (Batch 9 determinism fix).
            let mut member_indices: Vec<usize> = resolved.iter().map(|(i, _)| *i).collect();
            member_indices.sort_unstable();
            if groups.len() == 1 {
                ArchiveSetIdentity::MultiMemberSamePlatform {
                    member_indices,
                    platform: groups[0][0],
                }
            } else {
                let mut distinct: Vec<&'static str> = groups.iter().map(|g| g[0]).collect();
                distinct.sort_unstable();
                ArchiveSetIdentity::MultiPlatform {
                    member_indices,
                    platforms: distinct,
                }
            }
        }
    }
}

/// Every distinct member index referenced anywhere on `identity` - a small
/// convenience so a caller does not need to match on every variant just to
/// know which members participated in the decision.
pub fn participating_members(identity: &ArchiveSetIdentity) -> BTreeSet<usize> {
    match identity {
        ArchiveSetIdentity::Unknown => BTreeSet::new(),
        ArchiveSetIdentity::SingleMember { member_index, .. } => BTreeSet::from([*member_index]),
        ArchiveSetIdentity::MultiMemberSamePlatform { member_indices, .. }
        | ArchiveSetIdentity::MultiPlatform { member_indices, .. }
        | ArchiveSetIdentity::StructuredSet { member_indices, .. } => {
            member_indices.iter().copied().collect()
        }
    }
}

#[cfg(test)]
mod tests;
