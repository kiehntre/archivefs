//! Bridges the real whole-library DAT rename plan into the duplicate
//! quarantine planner ([`super::quarantine`]).
//!
//! This module never re-implements duplicate detection, content proof, or
//! quarantine planning: it only groups the [`RenameProposal`]s a whole-library
//! scan ([`super::library::run_library_scan`]) already produced by their
//! verified DAT game/rom identity, bridges each member into
//! [`super::quarantine::KeeperEvidence`] via the existing
//! [`super::quarantine::keeper_evidence_from_rename_proposal`], and hands each
//! group to the existing [`super::quarantine::plan_duplicate_quarantine`].
//! Content proof, survivor selection, and Safe/NeedsReview classification all
//! remain exactly [`super::quarantine`]'s decisions.
//!
//! # Why grouping by (game, rom) is cheap and correct
//!
//! A [`RenamePlan`] contains at most one proposal per audited source path, so
//! grouping its proposals by their verified `(game_name, rom_name)` identity
//! is a single `O(n)` pass with no filesystem access and no hashing - the
//! DAT audit already proved that identity. It is also a *partition*: every
//! source path lands in exactly one group, so no path can ever silently
//! belong to two different duplicate groups. The actual content proof stays
//! exactly as cheap as [`super::quarantine::plan_duplicate_quarantine`]
//! already makes it: one proof per non-survivor member against the survivor
//! (never a full O(n^2) pairwise scan), and
//! [`super::duplicate::prove_duplicate_content`] itself refuses a
//! size-mismatched pair before reading a single byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::dat::rename_plan::{ProposalState, RenamePlan, RenameProposal};
use crate::safe_read::TrustedRoots;

use super::duplicate::DuplicateHashCache;
use super::proposal::RepairProposal;
use super::quarantine::{
    KeeperEvidence, QuarantinePlanRefusal, keeper_evidence_from_rename_proposal,
    plan_duplicate_quarantine,
};

/// Additive, explicit accounting for one duplicate-quarantine scan pass.
///
/// Deliberately separate from [`super::library::ReportCounts`]'s existing
/// DAT candidate/ancillary/unmatched fields: this never changes what those
/// mean, it only adds a second, orthogonal accounting of the same proposals
/// from the duplicate-quarantine angle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DuplicateScanAccounting {
    /// Candidate groups found by (game, rom) identity, before any content
    /// proof.
    pub groups_examined: usize,
    /// Groups where a unique objective survivor was found, so this module
    /// went on to content-prove the other members against it (whether or not
    /// that produced any Safe proposal).
    pub groups_content_proven: usize,
    /// Safe `MovePath` quarantine proposals produced (one per redundant
    /// member independently proven a distinct-object duplicate).
    pub quarantine_safe: usize,
    /// Groups where no unique objective survivor existed
    /// ([`QuarantinePlanRefusal::NeedsReview`]); never an executable Safe
    /// proposal for that group.
    pub quarantine_needs_review: usize,
    /// Members skipped because they are the same filesystem object as the
    /// survivor (hard-linked) - never a reclaimable duplicate.
    pub same_object_ignored: usize,
    /// Members skipped because content proof refused for any other reason
    /// (hash mismatch, size mismatch, unreadable, changed mid-proof, ...).
    pub content_mismatch_refused: usize,
}

/// One member of a group [`plan_duplicate_quarantine_from_rename_plan`]
/// could not turn into any Safe proposal because the group itself had no
/// unique objective survivor - never an executable candidate, but never
/// silently dropped either: a caller (the CLI, a future GUI) surfaces this
/// exactly like any other non-executable report row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateNeedsReviewMember {
    pub path: PathBuf,
    /// The verified DAT game/rom identity the group shares, when known.
    pub game_name: Option<String>,
    pub rom_name: Option<String>,
    /// Why [`super::quarantine::select_survivor`] found no unique winner.
    pub reason: String,
}

/// Groups whole-library rename-plan proposals that share one verified DAT
/// game/rom identity into duplicate-quarantine candidate groups.
///
/// Only a proposal state that already carries a confident, singular per-file
/// DAT identity contributes a member:
///
/// - [`ProposalState::Suggested`] and [`ProposalState::AlreadyCanonical`] -
///   an ordinary verified match, in or out of place;
/// - [`ProposalState::Conflict`] - two proposals that would collide on the
///   same canonical name are still two confidently-identified copies of the
///   same ROM; the rename-plan collision they share is a *different*
///   question (whether they can both be renamed in their own directories)
///   from whether they are duplicate-quarantine candidates.
///
/// [`ProposalState::Ambiguous`], [`ProposalState::Blocked`],
/// [`ProposalState::Unsupported`], [`ProposalState::ExcludedByContentPolicy`],
/// and [`ProposalState::UnclassifiedContent`] never contribute: none of them
/// carries a confident, singular DAT attribution a keeper decision could be
/// built from.
fn duplicate_candidate_groups(plan: &RenamePlan) -> Vec<Vec<&RenameProposal>> {
    let mut groups: BTreeMap<(&str, &str), Vec<&RenameProposal>> = BTreeMap::new();
    for proposal in &plan.proposals {
        if !matches!(
            proposal.state,
            ProposalState::Suggested | ProposalState::AlreadyCanonical | ProposalState::Conflict
        ) {
            continue;
        }
        let (Some(game), Some(rom)) = (proposal.game_name.as_deref(), proposal.rom_name.as_deref())
        else {
            continue;
        };
        groups.entry((game, rom)).or_default().push(proposal);
    }
    groups
        .into_values()
        .filter(|group| group.len() >= 2)
        .collect()
}

/// Turns a whole-library [`RenamePlan`] into additive duplicate-quarantine
/// [`RepairProposal`]s, planning only - no filesystem mutation, no
/// `.emuwiz-quarantine` directory, nothing executed.
///
/// `trusted_root` must be the real scan/library root (never an EmuWiz state
/// directory): it is used only as [`super::quarantine::plan_duplicate_quarantine`]'s
/// `trusted_root`, exactly as [`super::library::run_library_scan`] already
/// trusts it for the DAT rename plan itself.
///
/// One [`DuplicateHashCache`] is shared across every group in this scan, so a
/// path proven or refused in one group's content proof is never re-hashed
/// for another.
pub fn plan_duplicate_quarantine_from_rename_plan(
    plan: &RenamePlan,
    trusted_root: &Path,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> (
    Vec<RepairProposal>,
    DuplicateScanAccounting,
    Vec<DuplicateNeedsReviewMember>,
) {
    let mut cache = DuplicateHashCache::new();
    let mut accounting = DuplicateScanAccounting::default();
    let mut proposals = Vec::new();
    let mut needs_review = Vec::new();

    for group in duplicate_candidate_groups(plan) {
        accounting.groups_examined += 1;
        let evidence: Vec<KeeperEvidence> = group
            .iter()
            .map(|proposal| keeper_evidence_from_rename_proposal(&proposal.source_path, proposal))
            .collect();
        match plan_duplicate_quarantine(&evidence, trusted_root, trusted, &mut cache, cancel) {
            Ok(group_plan) => {
                accounting.groups_content_proven += 1;
                accounting.quarantine_safe += group_plan.proposals.len();
                for (_, reason) in &group_plan.skipped {
                    if reason.contains("hard-linked") {
                        accounting.same_object_ignored += 1;
                    } else {
                        accounting.content_mismatch_refused += 1;
                    }
                }
                proposals.extend(group_plan.proposals);
            }
            Err(QuarantinePlanRefusal::NeedsReview { reason }) => {
                accounting.quarantine_needs_review += 1;
                for proposal in &group {
                    needs_review.push(DuplicateNeedsReviewMember {
                        path: proposal.source_path.clone(),
                        game_name: proposal.game_name.clone(),
                        rom_name: proposal.rom_name.clone(),
                        reason: reason.clone(),
                    });
                }
            }
        }
    }

    (proposals, accounting, needs_review)
}

#[cfg(test)]
mod tests;
