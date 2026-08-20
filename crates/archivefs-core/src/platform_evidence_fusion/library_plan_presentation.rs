//! Batch 10: the read-only presentation model for one
//! [`super::library_planning::LibraryItemPlan`] - milestone section 37.
//! Kept deliberately separate from [`super::identity_presentation`]
//! (identity confidence is a different responsibility from a destination
//! proposal, per that section's own "do not mix with identity presentation
//! if that would muddy responsibilities" instruction).

use super::identity_presentation::{IdentityPresentation, present_identity};
use super::library_planning::{LibraryItemPlan, PlanStatus, RenameBasis, RommMappingStatus};

/// One structured row - `(label, value)`, matching
/// [`super::identity_presentation::ProvenanceRow`]'s own shape.
pub type PlanRow = (&'static str, String);

/// The reusable read-only planning view model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryPlanPresentation {
    pub status: PlanStatus,
    pub source: String,
    pub identity: IdentityPresentation,
    pub platform_library: Option<String>,
    pub set_summary: String,
    pub destination_preview: Option<String>,
    pub rename_summary: String,
    pub romm_summary: String,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
    pub provenance_rows: Vec<PlanRow>,
    pub source_modified: bool,
    /// Batch 12 (milestone section 30/32): the duplicate group this item
    /// belongs to, when the caller already computed one via
    /// [`super::duplicate_taxonomy::group_duplicates`]. `None` from
    /// [`present_library_plan`] itself; only
    /// [`present_library_plan_with_context`] ever populates this.
    pub duplicate_relationship: Option<super::duplicate_taxonomy::DuplicateGroup>,
    /// Batch 12 (milestone section 30/33): this item's DAT-declared
    /// lineage, when supplied.
    pub revision_relationship: Option<super::release_relationship::ReleaseRelationship>,
    /// Batch 12 (milestone section 30): a human summary of this item's
    /// multi-disc set membership, when supplied.
    pub multidisc_state: Option<String>,
    /// Batch 12 (milestone section 30/34): this item's support-file role
    /// and association, when this item is itself a support file.
    pub support_role: Option<SupportRolePresentation>,
}

/// Builds a [`LibraryPlanPresentation`] from an already-computed
/// [`LibraryItemPlan`] and the [`super::identity_orchestrator::IdentityResult`]
/// it was built from (needed only to render the identity sub-presentation -
/// no re-analysis happens here).
pub fn present_library_plan(
    plan: &LibraryItemPlan,
    identity: &super::identity_orchestrator::IdentityResult,
) -> LibraryPlanPresentation {
    let identity_presentation = present_identity(identity);
    let entry = &plan.organisation;

    let destination_preview =
        (plan.status == PlanStatus::Ready).then(|| entry.destination_path.display().to_string());

    let set_summary = match &plan.set_identity {
        None => "Single file (not part of an inspected archive)".to_string(),
        Some(super::archive_set_identity::ArchiveSetIdentity::Unknown) => {
            "Archive member; no member resolved a platform".to_string()
        }
        Some(super::archive_set_identity::ArchiveSetIdentity::SingleMember {
            platform, ..
        }) => {
            format!("Single game-like member ({platform})")
        }
        Some(super::archive_set_identity::ArchiveSetIdentity::MultiMemberSamePlatform {
            platform,
            member_indices,
        }) => format!(
            "Multi-member set, same platform ({platform}, {} members) - exact game/set identity not collapsed",
            member_indices.len()
        ),
        Some(super::archive_set_identity::ArchiveSetIdentity::MultiPlatform {
            platforms, ..
        }) => {
            format!("Multi-platform archive ({})", platforms.join(", "))
        }
        Some(super::archive_set_identity::ArchiveSetIdentity::StructuredSet {
            platform, ..
        }) => {
            format!("Structured set ({platform})")
        }
    };

    let rename_summary = match plan.rename.basis {
        RenameBasis::AuthoritativeDatRelease => format!(
            "Suggested (from verified DAT release): {} -- NOT AUTHORIZED",
            plan.rename.proposed_name.as_deref().unwrap_or("?")
        ),
        RenameBasis::OriginalNamePreserved => format!(
            "Suggested (original name preserved): {} -- NOT AUTHORIZED",
            plan.rename.proposed_name.as_deref().unwrap_or("?")
        ),
        RenameBasis::Unavailable => "No rename suggestion available".to_string(),
    };

    let romm_summary = match plan.romm.status {
        RommMappingStatus::Mapped => format!(
            "Proposed mapping: {} ({})",
            plan.romm.slug.clone().unwrap_or_default(),
            plan.romm.canonical_platform.clone().unwrap_or_default()
        ),
        RommMappingStatus::Unmapped => "No RomM slug mapping exists yet".to_string(),
        RommMappingStatus::Ambiguous => {
            "RomM mapping ambiguous - platform itself is unresolved".to_string()
        }
        RommMappingStatus::Unsupported => "No canonical platform to map".to_string(),
    };

    let mut blockers: Vec<String> = plan.rename.blockers.clone();
    if let Some(reason) = &entry.reason {
        blockers.push(reason.clone());
    }
    blockers.dedup();

    let mut warnings: Vec<String> = plan.romm.warnings.iter().map(|w| w.to_string()).collect();
    warnings.extend(identity_presentation.caveats.iter().map(|c| c.to_string()));

    let mut provenance_rows: Vec<PlanRow> = vec![
        ("platform_source", entry.platform_source.clone()),
        ("mode", entry.mode.label().to_string()),
    ];
    if let Some(set) = &plan.set_identity {
        provenance_rows.push(("set_identity", format!("{set:?}")));
    }

    LibraryPlanPresentation {
        status: plan.status,
        source: entry.source_path.display().to_string(),
        identity: identity_presentation,
        platform_library: entry.platform.clone(),
        set_summary,
        destination_preview,
        rename_summary,
        romm_summary,
        warnings,
        blockers,
        provenance_rows,
        source_modified: false,
        duplicate_relationship: None,
        revision_relationship: None,
        multidisc_state: None,
        support_role: None,
    }
}

/// Plain human-readable rendering (milestone sections 35-36). The only
/// prose function in this module; every fact it prints already exists on
/// `presentation`.
pub fn render_library_plan_text(presentation: &LibraryPlanPresentation) -> String {
    let mut out = String::new();
    out.push_str(presentation.status.label());
    out.push_str("\n\n");

    out.push_str("Source:\n  ");
    out.push_str(&presentation.source);
    out.push_str("\n\n");

    out.push_str("Identity:\n  ");
    out.push_str(presentation.identity.platform.unwrap_or("Unknown"));
    out.push_str(" - ");
    out.push_str(presentation.identity.status.label());
    out.push('\n');

    out.push_str("\nProposed library:\n  ");
    out.push_str(
        presentation
            .platform_library
            .as_deref()
            .unwrap_or("(none - unresolved identity)"),
    );
    out.push('\n');

    out.push_str("\nProposed destination:\n  ");
    out.push_str(
        presentation
            .destination_preview
            .as_deref()
            .unwrap_or("(preview only - not ready)"),
    );
    out.push('\n');

    out.push_str("\nSet identity:\n  ");
    out.push_str(&presentation.set_summary);
    out.push('\n');

    out.push_str("\nRename suggestion:\n  ");
    out.push_str(&presentation.rename_summary);
    out.push('\n');

    out.push_str("\nRomM:\n  ");
    out.push_str(&presentation.romm_summary);
    out.push('\n');

    if !presentation.blockers.is_empty() {
        out.push_str("\nBlockers:\n");
        for blocker in &presentation.blockers {
            out.push_str("  ");
            out.push_str(blocker);
            out.push('\n');
        }
    }
    if !presentation.warnings.is_empty() {
        out.push_str("\nWarnings:\n");
        for warning in &presentation.warnings {
            out.push_str("  ");
            out.push_str(warning);
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "\nSource modified:\n  {}\n",
        if presentation.source_modified {
            "YES"
        } else {
            "NO"
        }
    ));
    if let Some(duplicate) = &presentation.duplicate_relationship {
        out.push_str("\nDuplicate relationship:\n  ");
        out.push_str(duplicate.classification.label());
        out.push('\n');
        out.push_str("  ");
        out.push_str(&duplicate.basis);
        out.push('\n');
        out.push_str("  Delete:\n    NOT AUTHORIZED\n");
    }
    if let Some(revision) = &presentation.revision_relationship {
        out.push_str("\nRelease relationship:\n  ");
        out.push_str(&revision.label());
        out.push('\n');
    }
    if let Some(multidisc) = &presentation.multidisc_state {
        out.push_str("\nMulti-disc set:\n  ");
        out.push_str(multidisc);
        out.push('\n');
    }
    if let Some(support) = &presentation.support_role {
        out.push_str("\nSupport file role:\n  ");
        out.push_str(support.role.label());
        out.push_str(" - ");
        out.push_str(&support.association_label);
        out.push('\n');
    }
    out
}

/// Batch 12: additional, entirely optional cross-batch context a caller
/// may have already computed (a duplicate group this item belongs to, its
/// release lineage, a multi-disc set summary, its support-file
/// attachment) - milestone section 30. Kept as a *separate* function from
/// [`present_library_plan`] rather than widening that function's own
/// signature, so every one of its existing callers/tests is untouched;
/// this function composes on top of it.
pub fn present_library_plan_with_context(
    plan: &LibraryItemPlan,
    identity: &super::identity_orchestrator::IdentityResult,
    duplicate_relationship: Option<&super::duplicate_taxonomy::DuplicateGroup>,
    revision_relationship: Option<&super::release_relationship::ReleaseRelationship>,
    multidisc_state: Option<&str>,
    support: Option<&super::support_attachment::SupportFileAttachment>,
) -> LibraryPlanPresentation {
    let mut presentation = present_library_plan(plan, identity);
    presentation.duplicate_relationship = duplicate_relationship.cloned();
    presentation.revision_relationship = revision_relationship.cloned();
    presentation.multidisc_state = multidisc_state.map(str::to_string);
    presentation.support_role = support.map(|attachment| SupportRolePresentation {
        role: attachment.role,
        association_label: match &attachment.association {
            super::support_attachment::SupportAssociation::Attached { set_label } => {
                format!("Attached ({set_label})")
            }
            super::support_attachment::SupportAssociation::Candidate { reason } => {
                format!("Candidate ({reason})")
            }
            super::support_attachment::SupportAssociation::Unassociated => {
                "Unassociated".to_string()
            }
            super::support_attachment::SupportAssociation::UnsafeReference { detail } => {
                format!("UnsafeReference ({detail})")
            }
        },
    });
    presentation
}

/// A support file's role plus a human-readable association label -
/// milestone section 34's example shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportRolePresentation {
    pub role: super::side_file_classification::SideFileRole,
    pub association_label: String,
}

impl super::release_relationship::ReleaseRelationship {
    /// Human label for presentation - milestone section 33.
    pub fn label(&self) -> String {
        match self {
            Self::Unknown => "Unknown (no DAT match)".to_string(),
            Self::Canonical { .. } => "Canonical release (no declared parent)".to_string(),
            Self::CloneOf { parent, .. } => {
                format!("Same game, different revision (parent: {parent})")
            }
        }
    }
}

#[cfg(test)]
mod tests;
