//! Batch 12: the owned, frozen plan-export boundary - milestone sections
//! 44-46.
//!
//! [`LibraryPlanExport`] is a snapshot: every field is an owned `String`/
//! plain value, safe to serialize, persist, or hand to a future GUI without
//! that consumer re-running identity or holding a borrow into this
//! session's `IdentityResult`/`OrganisationPlanEntry`. It carries **no**
//! executable authority - no function pointers, no action enum with an
//! `apply()` method, nothing a future transaction system could invoke
//! directly. It only names *what a future transaction system would need to
//! validate before acting* (milestone section 46): the source path, its
//! best-known precondition facts (size/hashes, when the caller already had
//! them - never computed here), the proposed destination, an operation
//! intent label, blockers, and provenance. Turning this into a real
//! transaction is explicitly out of scope for this batch.

use serde::{Deserialize, Serialize};

use super::duplicate_taxonomy::DuplicateClass;
use super::library_plan_presentation::LibraryPlanPresentation;
use super::library_planning::{LibraryItemPlan, PlanStatus, RenameBasis, RommMappingStatus};
use crate::dat::rom_organisation::OrganisationMode;

/// The frozen precondition facts a future transaction system would need to
/// detect a stale plan (milestone section 47) - never computed here, only
/// carried forward from what the caller already knew when the plan was
/// built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePrecondition {
    pub source_path: String,
    pub physical_hash: Option<String>,
    pub normalized_hash: Option<String>,
}

/// The proposed operation's intent - a label only, never an executable
/// action (milestone section 45's "no function pointers/actions").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationIntent {
    MoveToLibraryFolder,
    RenameInPlace,
    OrganiseSymlinkOnly,
    /// No operation is proposed (not `Ready`).
    None,
}

/// One item's frozen, owned export - milestone section 42.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryPlanExportItem {
    pub status: PlanStatus,
    pub precondition: SourcePrecondition,
    pub proposed_destination: Option<String>,
    pub operation_intent: OperationIntent,
    pub platform_library: Option<String>,
    pub display_name: String,
    pub romm_status: RommMappingStatus,
    pub romm_slug: Option<String>,
    pub rename_basis: RenameBasis,
    pub proposed_name: Option<String>,
    pub duplicate_classification: Option<DuplicateClass>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub source_modified: bool,
}

/// The full frozen export - milestone section 44/46.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryPlanExport {
    pub items: Vec<LibraryPlanExportItem>,
}

/// Builds one item's export from its already-computed
/// [`LibraryItemPlan`]/[`LibraryPlanPresentation`] - pure data
/// transcription, no new analysis, no filesystem access.
pub fn export_item(
    plan: &LibraryItemPlan,
    presentation: &LibraryPlanPresentation,
    physical_hash: Option<&str>,
    normalized_hash: Option<&str>,
) -> LibraryPlanExportItem {
    let entry = &plan.organisation;
    let operation_intent = if plan.status == PlanStatus::Ready {
        match entry.mode {
            OrganisationMode::MoveRealFile => OperationIntent::MoveToLibraryFolder,
            OrganisationMode::RenameInPlace => OperationIntent::RenameInPlace,
            OrganisationMode::OrganiseSymlinkOnly => OperationIntent::OrganiseSymlinkOnly,
        }
    } else {
        OperationIntent::None
    };

    LibraryPlanExportItem {
        status: plan.status,
        precondition: SourcePrecondition {
            source_path: entry.source_path.display().to_string(),
            physical_hash: physical_hash.map(str::to_string),
            normalized_hash: normalized_hash.map(str::to_string),
        },
        proposed_destination: presentation.destination_preview.clone(),
        operation_intent,
        platform_library: presentation.platform_library.clone(),
        display_name: presentation
            .identity
            .platform
            .unwrap_or("Unknown")
            .to_string(),
        romm_status: plan.romm.status,
        romm_slug: plan.romm.slug.clone(),
        rename_basis: plan.rename.basis,
        proposed_name: plan.rename.proposed_name.clone(),
        duplicate_classification: presentation
            .duplicate_relationship
            .as_ref()
            .map(|group| group.classification),
        blockers: presentation.blockers.clone(),
        warnings: presentation.warnings.clone(),
        source_modified: presentation.source_modified,
    }
}

/// Builds the full export from a batch of already-computed plans/
/// presentations, in the caller's own supplied order (stable - milestone
/// section 48; the caller is expected to have already sorted its own
/// inputs deterministically, this function does not re-sort).
pub fn export_plan(
    items: &[(
        &LibraryItemPlan,
        &LibraryPlanPresentation,
        Option<&str>,
        Option<&str>,
    )],
) -> LibraryPlanExport {
    LibraryPlanExport {
        items: items
            .iter()
            .map(|(plan, presentation, physical, normalized)| {
                export_item(plan, presentation, *physical, *normalized)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests;
