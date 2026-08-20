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
    /// Batch 13 (milestone section 18): the DAT-declared release lineage
    /// label, when the caller supplied one - always an owned `String`
    /// (via [`super::release_relationship::ReleaseRelationship::label`]),
    /// never a borrow.
    pub revision_relationship: Option<String>,
    /// Batch 13: the set this item belongs to, and that set's own
    /// destination folder, when it belongs to one (from
    /// [`super::set_destination::plan_set_destinations`]).
    pub set_label: Option<String>,
    pub set_destination: Option<String>,
    /// Batch 13: this item's own support-file role/association, when this
    /// export item is itself a support file rather than a primary item.
    pub support_role: Option<String>,
    pub support_association: Option<String>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub source_modified: bool,
}

/// The full frozen export - milestone section 44/46.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryPlanExport {
    pub items: Vec<LibraryPlanExportItem>,
}

/// The set/support facts to fold into one export item, when the caller
/// already computed them via
/// [`super::set_destination::plan_set_destinations`] - kept as a separate
/// parameter struct rather than widening `export_item`'s positional
/// argument list further.
#[derive(Debug, Clone, Default)]
pub struct SetAndSupportContext {
    pub set_label: Option<String>,
    pub set_destination: Option<String>,
    pub support_role: Option<String>,
    pub support_association: Option<String>,
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
    export_item_with_context(
        plan,
        presentation,
        physical_hash,
        normalized_hash,
        &SetAndSupportContext::default(),
    )
}

/// Same as [`export_item`], additionally folding in set/support facts.
pub fn export_item_with_context(
    plan: &LibraryItemPlan,
    presentation: &LibraryPlanPresentation,
    physical_hash: Option<&str>,
    normalized_hash: Option<&str>,
    set_and_support: &SetAndSupportContext,
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
        revision_relationship: presentation
            .revision_relationship
            .as_ref()
            .map(|relationship| relationship.label()),
        set_label: set_and_support.set_label.clone(),
        set_destination: set_and_support.set_destination.clone(),
        support_role: set_and_support.support_role.clone(),
        support_association: set_and_support.support_association.clone(),
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
