//! Batch 11: the complete read-only planning report - milestone section 0's
//! "one complete READ-ONLY planning report" and section 33's structured
//! counts. Extended in Batch 13 (milestone section 13) with the
//! support/set-destination axes.
//!
//! Deliberately a *composition* over [`super::library_planning::plan_library`],
//! [`super::duplicate_taxonomy::group_duplicates`],
//! [`super::library_grouping::group_multidisc_sets`], and
//! [`super::set_destination::plan_set_destinations`] - never a second
//! planning pipeline. [`super::library_planning::LibraryPlanningReport`]
//! itself is left untouched; this module only adds the cross-cutting
//! statistics milestone section 33/13 asks for on top of it.

use serde::Serialize;

use super::duplicate_taxonomy::{DuplicateClass, DuplicateGroup, group_duplicates};
use super::library_grouping::{MultiDiscSet, group_multidisc_sets};
use super::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, LibraryPlanningReport, plan_library,
};
use super::set_destination::{
    SetDestinationPlan, SupportCandidate, SupportPlanItem, plan_set_destinations,
};
use super::support_attachment::SupportAssociation;

/// The complete, read-only report - everything a caller needs to show a
/// user the full planned picture in one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FullLibraryReport {
    pub plan: LibraryPlanningReport,
    pub duplicate_groups: Vec<DuplicateGroup>,
    pub multidisc_sets: Vec<MultiDiscSet>,
    pub set_destinations: Vec<SetDestinationPlan>,
    pub support_items: Vec<SupportPlanItem>,
    pub counts: FullLibraryCounts,
}

/// Milestone section 33/13's structured counts, entirely derived from the
/// data above - never a separately-tracked source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct FullLibraryCounts {
    pub total_inputs: usize,
    pub primary_items: usize,
    pub support_files: usize,
    pub support_attached: usize,
    pub support_candidate: usize,
    pub support_unassociated: usize,
    pub support_unsafe: usize,
    pub game_groups: usize,
    pub release_groups: usize,
    pub set_groups: usize,
    pub exact_physical_duplicates: usize,
    pub normalized_duplicates: usize,
    pub same_dat_release_groups: usize,
    pub different_revision_groups: usize,
    pub possible_duplicate_groups: usize,
    pub ready: usize,
    pub needs_review: usize,
    pub ambiguous: usize,
    pub conflict: usize,
    pub unknown: usize,
    pub unsupported: usize,
    pub romm_mapped: usize,
    pub romm_unmapped: usize,
}

impl FullLibraryCounts {
    /// Milestone section 14's arithmetic invariants, checked in one place
    /// so both this module's own tests and any caller can call the same
    /// check. Returns the first violation found, if any.
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        if self.support_attached
            + self.support_candidate
            + self.support_unassociated
            + self.support_unsafe
            != self.support_files
        {
            return Err("support role totals do not reconcile with support_files");
        }
        if self.ready
            + self.needs_review
            + self.ambiguous
            + self.conflict
            + self.unknown
            + self.unsupported
            != self.primary_items
        {
            return Err("plan status totals do not reconcile with primary_items");
        }
        if self.romm_mapped + self.romm_unmapped > self.primary_items {
            return Err("romm mapped+unmapped exceeds primary_items");
        }
        if self.primary_items + self.support_files != self.total_inputs {
            return Err("primary_items + support_files does not reconcile with total_inputs");
        }
        Ok(())
    }
}

/// Builds the complete report for one batch of primary inputs plus any
/// already-classified support candidates - milestone section 45's real
/// bounded corpus run is exactly one call to this.
pub fn build_full_report(
    inputs: &[LibraryPlanInput],
    context: &LibraryPlanningContext<'_>,
    support_candidates: &[SupportCandidate<'_>],
) -> FullLibraryReport {
    let plan = plan_library(inputs, context);
    let duplicate_groups = group_duplicates(inputs);
    let multidisc_sets = group_multidisc_sets(inputs);
    let set_plan = plan_set_destinations(&plan, &multidisc_sets, support_candidates);

    // `inputs` and `support_candidates` are the caller's own two disjoint
    // lists (primary items go through identity via `plan_library`; support
    // files never do) - `primary_items` is simply `inputs.len()`, not a
    // second, potentially-disagreeing re-classification by extension.
    let primary_items = inputs.len();
    let support_files = support_candidates.len();

    let mut support_attached = 0usize;
    let mut support_candidate = 0usize;
    let mut support_unassociated = 0usize;
    let mut support_unsafe = 0usize;
    for item in &set_plan.support_items {
        match &item.association {
            SupportAssociation::Attached { .. } => support_attached += 1,
            SupportAssociation::Candidate { .. } => support_candidate += 1,
            SupportAssociation::Unassociated => support_unassociated += 1,
            SupportAssociation::UnsafeReference { .. } => support_unsafe += 1,
        }
    }

    let mut counts = FullLibraryCounts {
        total_inputs: inputs.len() + support_files,
        primary_items,
        support_files,
        support_attached,
        support_candidate,
        support_unassociated,
        support_unsafe,
        // No confirmed clone_of/game-identity grouping into a distinct
        // "game group" concept exists yet - disclosed gap, left at 0
        // rather than conflated with set_groups (a different axis).
        game_groups: 0,
        release_groups: 0,
        set_groups: set_plan.sets.len(),
        exact_physical_duplicates: 0,
        normalized_duplicates: 0,
        same_dat_release_groups: 0,
        different_revision_groups: 0,
        possible_duplicate_groups: 0,
        ready: plan.ready,
        needs_review: plan.needs_review,
        ambiguous: plan.ambiguous,
        conflict: plan.conflict,
        unknown: plan.unknown,
        unsupported: plan.unsupported,
        romm_mapped: plan.romm_mapped,
        romm_unmapped: plan.romm_unmapped,
    };
    for group in &duplicate_groups {
        match group.classification {
            DuplicateClass::ExactPhysicalDuplicate => counts.exact_physical_duplicates += 1,
            DuplicateClass::ExactNormalizedDuplicate => counts.normalized_duplicates += 1,
            DuplicateClass::SameDatRelease | DuplicateClass::SameGameDifferentDump => {
                counts.same_dat_release_groups += 1
            }
            DuplicateClass::SameGameDifferentRevision => counts.different_revision_groups += 1,
            DuplicateClass::PossibleDuplicate => counts.possible_duplicate_groups += 1,
            DuplicateClass::NotDuplicate => {}
        }
    }
    counts.release_groups = counts.different_revision_groups;

    FullLibraryReport {
        plan,
        duplicate_groups,
        multidisc_sets,
        set_destinations: set_plan.sets,
        support_items: set_plan.support_items,
        counts,
    }
}

#[cfg(test)]
mod tests;
