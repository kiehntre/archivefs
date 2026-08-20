//! Batch 13: the missing set-folder destination shape - milestone
//! sections 3-9.
//!
//! Batch 10-12 left every primary item's destination flat:
//! `<root>/<slug>/<basename>`, regardless of whether it was a lone file or
//! one disc of a real multi-disc set. This module is a pure, read-only,
//! in-memory *post-processing* pass over an already-computed
//! [`LibraryPlanningReport`]: for items that are genuinely part of a set
//! (a [`MultiDiscSet`], or a cue/m3u-resolved reference group), it nests
//! their existing destination one level deeper -
//! `<root>/<slug>/<set-label>/<basename>` - and computes a matching
//! destination for any support file [`attach_support_file`] resolved as
//! `Attached` to that same set. A lone file's destination is left exactly
//! as `build_organisation_plan` already computed it - never forced into a
//! nested folder (milestone section 4's explicit rule).
//!
//! This deliberately does **not** touch [`crate::dat::rom_organisation`]
//! or [`super::library_planning::plan_library`] - both stay exactly as
//! Batch 10 built them. Nesting is computed here, over their already-final
//! output, and never fed back into a second identity/organisation pass.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::dat::rename_apply::preflight::is_safe_basename;

use super::library_grouping::MultiDiscSet;
use super::library_planning::{LibraryPlanningReport, PlanStatus};
use super::side_file_classification::SideFileRole;
use super::support_attachment::SupportAssociation;

/// One support file's planned destination - milestone section 3's
/// required fields, reusing [`PlanStatus`] rather than a new vocabulary
/// (milestone section 23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupportPlanItem {
    pub path: PathBuf,
    pub role: SideFileRole,
    pub association: SupportAssociation,
    /// The set this item attached to, when it did.
    pub attached_set: Option<String>,
    /// `Some` only when `association` is `Attached` *and* that set's own
    /// folder was actually computed (a matching set exists in this same
    /// report) - never fabricated from the association alone.
    pub proposed_destination: Option<PathBuf>,
    /// `Ready` when a destination was computed; `NeedsReview` for
    /// `Candidate`/`UnsafeReference` (never silently ignored - milestone
    /// section 5); `Unsupported` for `Unassociated` (nothing to attach to).
    pub status: PlanStatus,
}

/// One real set's computed folder and every member's nested destination -
/// milestone section 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetDestinationPlan {
    pub set_label: String,
    /// The set's own folder - the parent every member/support destination
    /// below is nested under.
    pub set_folder: PathBuf,
    /// `(source_path, nested_destination)` for every primary member.
    pub member_destinations: Vec<(PathBuf, PathBuf)>,
}

/// The full result - milestone sections 3-9.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct SetDestinationReport {
    pub sets: Vec<SetDestinationPlan>,
    pub support_items: Vec<SupportPlanItem>,
}

/// One support file candidate the caller already classified/attached -
/// this module never calls [`super::support_attachment::attach_support_file`]
/// itself (that requires cue/m3u file contents the caller already has),
/// only consumes its result.
pub struct SupportCandidate<'a> {
    pub path: &'a Path,
    pub role: SideFileRole,
    pub association: SupportAssociation,
    /// For `CueSheet`/`Playlist`: the *actual* resolved, safe reference
    /// paths this file names (from
    /// [`super::cue_m3u_parsing::parse_cue_file_references`]/
    /// [`super::cue_m3u_parsing::parse_m3u_references`]) - the real
    /// evidence [`infer_ad_hoc_set`] anchors on. Never inferred from
    /// directory proximity: two unrelated single-disc games sitting in the
    /// same directory must never be matched to each other's cue/m3u
    /// (milestone section 9's "do not collapse... into one file
    /// identity", generalised to "never merge two different sets either").
    /// Empty for every other role.
    pub referenced_members: Vec<PathBuf>,
}

/// Computes nested set-folder destinations for every real
/// [`MultiDiscSet`] in `report`, and resolves every `support_candidates`
/// entry against those (and single-primary-item) sets - milestone
/// sections 3-9. Pure computation over already-built data; no filesystem
/// access, no re-identification.
pub fn plan_set_destinations(
    report: &LibraryPlanningReport,
    multidisc_sets: &[MultiDiscSet],
    support_candidates: &[SupportCandidate<'_>],
) -> SetDestinationReport {
    let mut sets = Vec::new();

    for disc_set in multidisc_sets {
        let Some(plan) = nest_multidisc_set(report, disc_set) else {
            continue;
        };
        sets.push(plan);
    }

    // Every `Attached` support candidate whose `set_label` names a set not
    // already covered by a MultiDiscSet still deserves its own folder
    // (e.g. a cue+bin pair, or an m3u referencing real disc files) - built
    // from its own *actual resolved references*, never directory
    // proximity (see `referenced_members`'s own doc comment for why that
    // matters: two unrelated single-disc games can share a directory).
    for candidate in support_candidates {
        if let SupportAssociation::Attached { set_label } = &candidate.association
            && !sets.iter().any(|set| &set.set_label == set_label)
            && let Some(plan) = infer_ad_hoc_set(report, &candidate.referenced_members, set_label)
        {
            sets.push(plan);
        }
    }

    let support_items = support_candidates
        .iter()
        .map(|candidate| resolve_support_item(candidate, &sets))
        .collect();

    SetDestinationReport {
        sets,
        support_items,
    }
}

fn nest_multidisc_set(
    report: &LibraryPlanningReport,
    disc_set: &MultiDiscSet,
) -> Option<SetDestinationPlan> {
    if !is_safe_basename(&disc_set.base_title) {
        return None;
    }
    let mut member_destinations = Vec::new();
    let mut set_folder: Option<PathBuf> = None;
    for (_, source_path) in &disc_set.discs {
        let item = report
            .items
            .iter()
            .find(|item| &item.organisation.source_path == source_path)?;
        if item.status != PlanStatus::Ready {
            // A set is only planned once every member is itself Ready -
            // one blocked/conflicted disc means the whole set stays
            // unplanned rather than silently partial.
            return None;
        }
        let destination = &item.organisation.destination_path;
        let parent = destination.parent()?;
        let basename = destination.file_name()?;
        let folder = parent.join(&disc_set.base_title);
        set_folder.get_or_insert_with(|| folder.clone());
        member_destinations.push((source_path.clone(), folder.join(basename)));
    }
    Some(SetDestinationPlan {
        set_label: disc_set.base_title.clone(),
        set_folder: set_folder?,
        member_destinations,
    })
}

/// Builds a one-member ad hoc set folder for a support file `Attached` to
/// `set_label`, anchored on whichever `Ready` primary item shares the
/// support file's own parent directory - the only defensible "which set"
/// signal available when the support file's association didn't come from
/// a [`MultiDiscSet`] (e.g. a single cue+bin pair). Never invents a folder
/// when no such primary item exists in this report.
fn infer_ad_hoc_set(
    report: &LibraryPlanningReport,
    referenced_members: &[PathBuf],
    set_label: &str,
) -> Option<SetDestinationPlan> {
    if !is_safe_basename(set_label) || referenced_members.is_empty() {
        return None;
    }
    let mut member_destinations = Vec::new();
    let mut set_folder: Option<PathBuf> = None;
    for member_path in referenced_members {
        let item = report
            .items
            .iter()
            .find(|item| &item.organisation.source_path == member_path)?;
        if item.status != PlanStatus::Ready {
            return None;
        }
        let destination = &item.organisation.destination_path;
        let parent = destination.parent()?;
        let basename = destination.file_name()?;
        let folder = parent.join(set_label);
        set_folder.get_or_insert_with(|| folder.clone());
        member_destinations.push((member_path.clone(), folder.join(basename)));
    }
    Some(SetDestinationPlan {
        set_label: set_label.to_string(),
        set_folder: set_folder?,
        member_destinations,
    })
}

fn resolve_support_item(
    candidate: &SupportCandidate<'_>,
    sets: &[SetDestinationPlan],
) -> SupportPlanItem {
    match &candidate.association {
        SupportAssociation::Attached { set_label } => {
            match sets.iter().find(|set| &set.set_label == set_label) {
                Some(set) => {
                    let Some(basename) = candidate.path.file_name() else {
                        return SupportPlanItem {
                            path: candidate.path.to_path_buf(),
                            role: candidate.role,
                            association: candidate.association.clone(),
                            attached_set: Some(set_label.clone()),
                            proposed_destination: None,
                            status: PlanStatus::NeedsReview,
                        };
                    };
                    SupportPlanItem {
                        path: candidate.path.to_path_buf(),
                        role: candidate.role,
                        association: candidate.association.clone(),
                        attached_set: Some(set_label.clone()),
                        proposed_destination: Some(set.set_folder.join(basename)),
                        status: PlanStatus::Ready,
                    }
                }
                None => SupportPlanItem {
                    path: candidate.path.to_path_buf(),
                    role: candidate.role,
                    association: candidate.association.clone(),
                    attached_set: Some(set_label.clone()),
                    proposed_destination: None,
                    status: PlanStatus::NeedsReview,
                },
            }
        }
        SupportAssociation::Candidate { .. } | SupportAssociation::UnsafeReference { .. } => {
            SupportPlanItem {
                path: candidate.path.to_path_buf(),
                role: candidate.role,
                association: candidate.association.clone(),
                attached_set: None,
                proposed_destination: None,
                status: PlanStatus::NeedsReview,
            }
        }
        SupportAssociation::Unassociated => SupportPlanItem {
            path: candidate.path.to_path_buf(),
            role: candidate.role,
            association: candidate.association.clone(),
            attached_set: None,
            proposed_destination: None,
            status: PlanStatus::Unsupported,
        },
    }
}

#[cfg(test)]
mod tests;
