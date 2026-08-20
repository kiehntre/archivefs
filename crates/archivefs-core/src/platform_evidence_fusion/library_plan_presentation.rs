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
    out
}

#[cfg(test)]
mod tests;
