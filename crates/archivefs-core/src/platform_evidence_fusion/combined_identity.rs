//! Batch 7: the content-fusion / DAT-identity convergence layer.
//!
//! ```text
//! ContentEvidence -> fuse_platform_evidence -> ResolutionExplanation  (content lane)
//! ParsedDat -> gather_dat_platform_evidence -> resolve_dat_platform_identity -> DatPlatformIdentity  (DAT lane)
//!     both ->  combine_identity (this module)  -> CombinedIdentityView
//! ```
//!
//! # Which "DAT identity" this is
//!
//! This module combines the content-fusion lane with
//! [`crate::dat::identity::DatPlatformIdentity`] - "what platform does this
//! whole DAT *catalogue* describe" - not
//! [`crate::platform::identity::PlatformIdentitySource::VerifiedDat`]
//! ("this specific file's hash matched a DAT entry"). Both are legitimate,
//! already-reviewed DAT-adjacent lanes; this module picks
//! `dat::identity` deliberately because it is the one that already shares
//! content fusion's own three-outcome shape (`Unknown`/`Resolved`/
//! `Ambiguous`, `Weak`/`Corroborated`/`Strong` confidence) - the milestone's
//! own relationship states (`Agree`/`Disagree`/`ContentAmbiguous`/
//! `DatAmbiguous`/`BothAmbiguous`, ...) only make sense against a lane that
//! can itself be ambiguous, which a bare verified-hash match cannot be. A
//! caller who has *both* a DAT-source identity and a verified-hash match for
//! one specific file already has
//! [`crate::platform_evidence_fusion::identity_bridge::content_and_dat_identity_view`]
//! (Batch 6) for the latter; this module does not replace or duplicate that
//! - see its own module documentation for why the two "DAT" concepts are
//! kept apart rather than merged into one framework.
//!
//! # Why this is not a third copy of Agree/Disagree
//!
//! [`crate::platform_evidence_fusion::DatContentComparison`] (Batch 5/6)
//! already models `Agree`/`Disagree`/`ContentOnly`/`DatOnly`/`Neither`
//! against a bare `Option<&str>` DAT platform - it is still exactly right
//! for a caller who only has a single already-decided DAT platform string
//! (e.g. from a verified hash match, which cannot itself be ambiguous).
//! [`IdentityRelationship`] here is a strict superset needed only because
//! [`DatPlatformIdentity`] can *also* be `Ambiguous` - a state
//! `DatContentComparison` has no way to represent. Both types stay; this
//! one is used only where the caller actually has a full
//! `DatPlatformIdentity`, not just a platform string.

use crate::dat::identity::{DatPlatformConfidence, DatPlatformEvidence, DatPlatformIdentity};
use crate::platform::platform_by_id;

use super::{FusionOutcome, ResolutionExplanation, group_by_equivalence};

/// How the content lane and the DAT-source lane relate for one generation -
/// never collapsed into a single opaque verdict. See the module
/// documentation for the full outcome table (milestone section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityRelationship {
    /// Content resolved X, DAT resolved X (or an equivalent canonical id) -
    /// two independent lanes agreeing raises confidence, but neither
    /// source fact is rewritten.
    Agree { platform: &'static str },
    /// Content resolved X, DAT resolved Y, and X/Y are not equivalent -
    /// fails closed. Never "DAT wins" or "content wins."
    Disagree {
        content_platform: &'static str,
        dat_platform: &'static str,
    },
    /// Content resolved X; the DAT lane has no opinion at all
    /// ([`DatPlatformIdentity::Unknown`]).
    ContentOnly { platform: &'static str },
    /// The DAT lane resolved X; content has no opinion at all
    /// ([`FusionOutcome::Unknown`]).
    DatOnly { platform: &'static str },
    /// Neither lane resolved anything.
    Neither,
    /// Content is [`FusionOutcome::Ambiguous`] and DAT resolved a
    /// candidate. Per milestone section 5E, this is **never** automatically
    /// promoted to `Agree`/`ContentOnly` - an ambiguous content lane stays
    /// visibly ambiguous even when a DAT candidate happens to match one of
    /// its fired candidates; see
    /// `tests::dat_never_silently_promotes_an_ambiguous_content_candidate`.
    ContentAmbiguous { dat_platform: Option<&'static str> },
    /// The DAT lane is [`DatPlatformIdentity::Ambiguous`] and content
    /// resolved X - both facts are retained (milestone section 5F: "retain
    /// DAT ambiguity and content resolution"), never silently narrowed to
    /// content's answer alone.
    DatAmbiguous {
        content_platform: Option<&'static str>,
    },
    /// Both lanes are ambiguous.
    BothAmbiguous,
}

impl IdentityRelationship {
    /// Whether this relationship names one settled platform both a caller
    /// and a later, separately reviewed layer could safely treat as
    /// strengthened - only [`Self::Agree`].
    pub fn is_agreement(&self) -> bool {
        matches!(self, Self::Agree { .. })
    }

    /// Whether this relationship represents a genuine, fail-closed
    /// contradiction between two independently strong lanes.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Disagree { .. })
    }
}

/// The combined content+DAT identity view for one generation - both source
/// trails always retained in full (`content` / `dat`), never summarized
/// away into `relationship` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedIdentityView {
    /// The content lane's own outcome, verbatim.
    pub content_outcome: FusionOutcome,
    /// The DAT lane's own resolved/candidate platform id(s) - kept
    /// separately from `content_outcome`'s platform so a caller can always
    /// tell which lane said what.
    pub content_platform: Option<&'static str>,
    /// The DAT lane's own outcome shape, summarized (`Unknown`/`Resolved`/
    /// `Ambiguous`) without re-deriving it - see [`DatOutcome`].
    pub dat_outcome: DatOutcome,
    pub dat_platform: Option<&'static str>,
    pub relationship: IdentityRelationship,
}

/// A compact mirror of [`DatPlatformIdentity`]'s own three states - kept as
/// its own small enum (not a raw clone of `DatPlatformIdentity`, which
/// carries full evidence vectors this view does not need to duplicate)
/// purely so [`CombinedIdentityView`] itself stays `PartialEq`/cheap to
/// compare in tests without dragging `DatPlatformEvidence` equality along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatOutcome {
    Unknown,
    Resolved,
    Ambiguous,
}

/// Combines one content-fusion [`ResolutionExplanation`] with one DAT-source
/// [`DatPlatformIdentity`] into a [`CombinedIdentityView`]. Pure and
/// read-only: never opens a file, never mutates either input, never
/// authorizes a rename/move/delete/RomM/transaction-apply action - matching
/// every prior batch's own "no action authority" rule (see
/// `tests::combined_view_carries_no_action_bearing_fields`).
pub fn combine_identity(
    content: &ResolutionExplanation,
    dat: &DatPlatformIdentity,
) -> CombinedIdentityView {
    let content_platform = content.resolved_platform;
    let dat_platform = dat.platform().and_then(platform_by_id).map(|p| p.id);
    let dat_outcome = match dat {
        DatPlatformIdentity::Unknown => DatOutcome::Unknown,
        DatPlatformIdentity::Resolved { .. } => DatOutcome::Resolved,
        DatPlatformIdentity::Ambiguous { .. } => DatOutcome::Ambiguous,
    };

    let relationship = match (content.outcome, dat) {
        (FusionOutcome::Resolved, DatPlatformIdentity::Resolved { .. }) => {
            let content_platform = content_platform.expect("Resolved always carries a platform");
            let dat_platform = dat_platform.expect("Resolved always carries a platform");
            let groups = group_by_equivalence(&[content_platform, dat_platform]);
            if groups.len() == 1 {
                IdentityRelationship::Agree {
                    platform: groups[0][0],
                }
            } else {
                IdentityRelationship::Disagree {
                    content_platform,
                    dat_platform,
                }
            }
        }
        (FusionOutcome::Resolved, DatPlatformIdentity::Unknown) => {
            IdentityRelationship::ContentOnly {
                platform: content_platform.expect("Resolved always carries a platform"),
            }
        }
        (FusionOutcome::Resolved, DatPlatformIdentity::Ambiguous { .. }) => {
            IdentityRelationship::DatAmbiguous { content_platform }
        }
        (
            FusionOutcome::Unknown | FusionOutcome::Conflict,
            DatPlatformIdentity::Resolved { .. },
        ) => IdentityRelationship::DatOnly {
            platform: dat_platform.expect("Resolved always carries a platform"),
        },
        (FusionOutcome::Unknown | FusionOutcome::Conflict, DatPlatformIdentity::Unknown) => {
            IdentityRelationship::Neither
        }
        (
            FusionOutcome::Unknown | FusionOutcome::Conflict,
            DatPlatformIdentity::Ambiguous { .. },
        ) => {
            // Content has no single platform to report here (Unknown has
            // none; Conflict's own conflicting_platforms is a different,
            // already-explicit shape this view does not re-narrate) - both
            // lanes fail closed independently, reported as DatAmbiguous
            // with no content platform.
            IdentityRelationship::DatAmbiguous {
                content_platform: None,
            }
        }
        (FusionOutcome::Ambiguous, DatPlatformIdentity::Resolved { .. }) => {
            IdentityRelationship::ContentAmbiguous { dat_platform }
        }
        (FusionOutcome::Ambiguous, DatPlatformIdentity::Unknown) => {
            IdentityRelationship::ContentAmbiguous { dat_platform: None }
        }
        (FusionOutcome::Ambiguous, DatPlatformIdentity::Ambiguous { .. }) => {
            IdentityRelationship::BothAmbiguous
        }
    };

    CombinedIdentityView {
        content_outcome: content.outcome,
        content_platform,
        dat_outcome,
        dat_platform,
        relationship,
    }
}

/// Compact, structured DAT-source provenance for display - milestone
/// section 16 ("structured compact provenance only," never a giant DAT
/// blob). Built from a real [`DatPlatformIdentity::Resolved`]/`Ambiguous`'s
/// own evidence, never invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatSourceProvenance {
    pub platform: &'static str,
    pub confidence: DatPlatformConfidence,
    /// The kind label of the strongest evidence that decided this platform
    /// (e.g. `"DAT header name"`) - see
    /// [`crate::dat::identity::DatPlatformEvidenceKind::label`].
    pub deciding_kind_label: &'static str,
    pub machine_key: Option<String>,
}

/// Extracts [`DatSourceProvenance`] from a `Resolved` [`DatPlatformIdentity`],
/// returning `None` for `Unknown`/`Ambiguous`, which have no single decided
/// platform to summarize this way (their own `evidence()`/`candidates` stay
/// available on the original value for a caller that wants those instead).
pub fn dat_source_provenance(dat: &DatPlatformIdentity) -> Option<DatSourceProvenance> {
    match dat {
        DatPlatformIdentity::Resolved {
            platform,
            machine_key,
            confidence,
            evidence,
        } => {
            let platform = platform_by_id(platform)?.id;
            // `evidence` is already sorted strongest-first (confidence
            // descending, then kind ascending - see
            // resolve_dat_platform_identity's own sort) - the first entry
            // at this outcome's own confidence is exactly the deciding
            // fact, no re-ranking needed.
            let deciding: &DatPlatformEvidence = evidence
                .iter()
                .find(|item| item.confidence == *confidence)?;
            Some(DatSourceProvenance {
                platform,
                confidence: *confidence,
                deciding_kind_label: deciding.kind.label(),
                machine_key: machine_key.clone(),
            })
        }
        DatPlatformIdentity::Unknown | DatPlatformIdentity::Ambiguous { .. } => None,
    }
}

#[cfg(test)]
mod tests;
