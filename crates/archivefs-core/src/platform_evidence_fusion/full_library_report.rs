//! Batch 11: the complete read-only planning report - milestone section 0's
//! "one complete READ-ONLY planning report" and section 33's structured
//! counts.
//!
//! Deliberately a *composition* over [`super::library_planning::plan_library`],
//! [`super::duplicate_taxonomy::group_duplicates`], and
//! [`super::library_grouping::group_multidisc_sets`] - never a second
//! planning pipeline. [`super::library_planning::LibraryPlanningReport`]
//! itself is left untouched (its own 60+ existing tests keep passing
//! unchanged); this module only adds the cross-cutting statistics milestone
//! section 33 asks for on top of it.

use serde::Serialize;

use super::duplicate_taxonomy::{DuplicateClass, DuplicateGroup, group_duplicates};
use super::library_grouping::{MultiDiscSet, group_multidisc_sets};
use super::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, LibraryPlanningReport, plan_library,
};
use super::side_file_classification::{SideFileRole, classify_side_file};

/// The complete, read-only report - everything a caller needs to show a
/// user the full planned picture in one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FullLibraryReport {
    pub plan: LibraryPlanningReport,
    pub duplicate_groups: Vec<DuplicateGroup>,
    pub multidisc_sets: Vec<MultiDiscSet>,
    pub counts: FullLibraryCounts,
}

/// Milestone section 33's structured counts, entirely derived from the
/// three inputs above - never a fourth independently-tracked source of
/// truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct FullLibraryCounts {
    pub primary_items: usize,
    pub support_files: usize,
    pub game_groups: usize,
    pub set_groups: usize,
    pub exact_physical_duplicates: usize,
    pub normalized_duplicates: usize,
    pub same_dat_release_groups: usize,
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

/// Builds the complete report for one batch of inputs - milestone section
/// 45's real bounded corpus run is exactly one call to this.
pub fn build_full_report(
    inputs: &[LibraryPlanInput],
    context: &LibraryPlanningContext<'_>,
) -> FullLibraryReport {
    let plan = plan_library(inputs, context);
    let duplicate_groups = group_duplicates(inputs);
    let multidisc_sets = group_multidisc_sets(inputs);

    let mut primary_items = 0usize;
    let mut support_files = 0usize;
    for input in inputs {
        match classify_side_file(&input.source_path) {
            SideFileRole::PrimaryContent => primary_items += 1,
            _ => support_files += 1,
        }
    }

    let mut counts = FullLibraryCounts {
        primary_items,
        support_files,
        game_groups: 0, // no confirmed clone_of/game-identity grouping exists yet - see this
        // batch's disclosed gap in the final report; left at 0 rather than
        // conflated with multidisc_sets.len() (a different axis).
        set_groups: multidisc_sets.len(),
        exact_physical_duplicates: 0,
        normalized_duplicates: 0,
        same_dat_release_groups: 0,
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
            DuplicateClass::PossibleDuplicate => counts.possible_duplicate_groups += 1,
            DuplicateClass::SameGameDifferentRevision | DuplicateClass::NotDuplicate => {}
        }
    }

    FullLibraryReport {
        plan,
        duplicate_groups,
        multidisc_sets,
        counts,
    }
}

#[cfg(test)]
mod tests;
