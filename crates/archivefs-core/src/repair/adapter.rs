//! Adapter from the existing hardened DAT rename-plan output into the Repair
//! Center.
//!
//! The bridge is deliberately strict: only a `Suggested`, actionable,
//! collision-free regular-file [`RenameProposal`] becomes a [`RepairProposal`],
//! and it never loses its verification or provenance - the audited
//! filesystem identity, the DAT source identity, the game/ROM names, and the
//! verdict are all carried over. An ambiguous, blocked, unsupported, or
//! already-canonical rename result **never** becomes an executable Repair
//! proposal: it returns `None` and stays in the `rename_plan` layer, where the
//! GUI already knows how to explain it.
//!
//! The Repair Center must not weaken a rename proposal; it only re-presents the
//! strongest existing classification in the Repair vocabulary.

use std::path::PathBuf;

use crate::dat::rename_plan::{ProposalState, RenamePlan, RenameProposal, SourceObjectKind};

use super::plan::{RepairPlan, RepairPlanId, build_repair_plan};
use super::proposal::{
    RepairAction, RepairAuditRef, RepairEvidence, RepairEvidenceKind, RepairProposal,
    RepairProposalId, SafetyState,
};

/// Bridges one trusted (Suggested) DAT rename proposal into a Safe Repair
/// proposal. Returns `None` for anything that is not executable, so an
/// ambiguous or incomplete DAT result can never become a mutation candidate.
pub fn repair_proposal_from_suggested_rename(
    proposal: &RenameProposal,
    generation: u64,
) -> Option<RepairProposal> {
    if proposal.state != ProposalState::Suggested
        || !proposal.actionable
        || proposal.collision.is_some()
        || proposal.object_kind != SourceObjectKind::RegularFile
    {
        return None;
    }
    let proposed_basename = proposal.proposed_basename.as_ref()?;
    let destination = proposal.source_path.parent()?.join(proposed_basename);

    // A durable, path-safe proposal id derived from the source path.
    let id_raw = format!(
        "dat-{}",
        proposal
            .source_path
            .to_string_lossy()
            .replace(['/', '\\'], "_")
    );
    let id = RepairProposalId::new(id_raw)?;

    let mut evidence = Vec::new();
    evidence.push(RepairEvidence::new(
        RepairEvidenceKind::CanonicalDatName,
        format!(
            "canonical DAT name '{}' for {}",
            proposed_basename, proposal.source_display_name
        ),
    ));
    if proposal.is_outer_archive {
        evidence.push(RepairEvidence::new(
            RepairEvidenceKind::VerifiedWholeArchiveAttribution,
            "the outer archive was attributed to exactly one verified set".to_string(),
        ));
    } else if proposal.match_confident {
        evidence.push(RepairEvidence::new(
            RepairEvidenceKind::ExactDatMemberIdentity,
            format!(
                "{} matched by {}",
                proposal.rom_name.as_deref().unwrap_or("member"),
                proposal.verdict_label
            ),
        ));
    }

    // The audited identity, carried verbatim. Never auto-captured here: an
    // executable proposal without audited identity is refused by plan
    // validation, preflight, and execution rather than silently capturing
    // whatever currently exists at the path.
    let expected_source_identity = proposal.audited_identity.clone();

    Some(RepairProposal {
        id,
        action: RepairAction::RenamePath { destination },
        source_path: proposal.source_path.clone(),
        reason: proposal.explanations.join("; "),
        evidence,
        expected_source_identity,
        originating_audit: Some(RepairAuditRef {
            source_id: proposal.source_id.clone(),
            generation,
        }),
        safety: SafetyState::Safe,
        blockers: Vec::new(),
        warnings: proposal.sanitisation_notes.clone(),
        dat_source_id: Some(proposal.source_id.clone()),
        dat_source_display: Some(proposal.source_display_name.clone()),
        game_name: proposal.game_name.clone(),
        rom_name: proposal.rom_name.clone(),
        verdict_label: Some(proposal.verdict_label.clone()),
        match_confident: proposal.match_confident,
        is_outer_archive: proposal.is_outer_archive,
        is_outer_archive_verified: proposal.is_outer_archive,
    })
}

/// Bridges every trusted proposal of a DAT rename plan into a Repair plan.
///
/// The resulting plan carries the same generation, so the executor's stale
/// check applies exactly as it does to the original plan. Non-executable
/// rename results are dropped (they never become mutation candidates).
pub fn repair_plan_from_rename_plan(plan: &RenamePlan, now_unix: u64) -> RepairPlan {
    let mut proposals = Vec::new();
    for proposal in &plan.proposals {
        if let Some(repair) = repair_proposal_from_suggested_rename(proposal, plan.generation) {
            proposals.push(repair);
        }
    }

    let id = RepairPlanId::new(format!(
        "dat-{}-{}",
        plan.source_id.replace(['/', '\\'], "_"),
        plan.generation
    ))
    .unwrap_or_else(|| RepairPlanId::new("dat-rename").expect("static id is valid"));

    build_repair_plan(
        id,
        plan.generation,
        now_unix,
        Some(plan.scan_root.clone()),
        proposals,
    )
}

/// The destination a trusted DAT rename implies, for callers that only need
/// the path (never used for execution here).
pub fn suggested_rename_destination(proposal: &RenameProposal) -> Option<PathBuf> {
    let proposed = proposal.proposed_basename.as_ref()?;
    proposal
        .source_path
        .parent()
        .map(|parent| parent.join(proposed))
}
