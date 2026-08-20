//! Batch 9: the read-only presentation/view model (milestone sections
//! 12-15) - the single place business logic ends and prose rendering
//! begins. [`present_identity`] is pure data transformation over an
//! existing [`IdentityResult`]; [`render_identity_text`] is the only
//! function in this module allowed to produce prose, and it does nothing
//! but format [`IdentityPresentation`]'s own fields - no decision is made
//! inside the renderer that is not already visible on the presentation
//! struct.
//!
//! # Status vocabulary (milestone section 14)
//!
//! [`IdentityStatus`] is deliberately conservative: `Strong` internal
//! content evidence is never called `Verified` - that word is reserved for
//! [`IdentityStatus::VerifiedByDat`], which only fires when a confident
//! (cryptographic-hash) DAT verdict exists, exactly matching
//! [`crate::dat::audit::AuditVerdict::is_confident`]'s own existing
//! distinction between a hash match and a weaker CRC32/filename guess.

use crate::dat::audit::AuditVerdict;
use crate::dat::identity::DatPlatformIdentity;
use crate::platform::platform_by_id;

use super::FusionOutcome;
use super::archive_set_identity::ArchiveSetIdentity;
use super::combined_identity::IdentityRelationship;
use super::dat_hash_representation::RepresentationMatchOutcome;
use super::identity_orchestrator::IdentityResult;

/// The headline identity status - milestone section 14's vocabulary,
/// computed in a fixed, documented priority order (never HashMap-iteration
/// dependent - see `tests::status_priority_is_deterministic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityStatus {
    /// A genuine, fail-closed contradiction - content vs. DAT, physical vs.
    /// normalized hash, or a multi-platform archive. Highest priority:
    /// nothing else matters once a conflict exists.
    Conflict,
    /// Content evidence or DAT identity (or both) is `Ambiguous` and
    /// neither resolved to a confident single platform.
    Ambiguous,
    /// A confident (`Exact`/`ExactMultipleCandidates`) DAT hash verdict
    /// exists - cryptographic identity, independent of whether content
    /// fusion also resolved anything.
    VerifiedByDat,
    /// Content fusion resolved a platform and a confident DAT verdict for
    /// the *same* platform also exists - the strongest combined state.
    ContentAndDatAgree,
    /// Content fusion resolved a platform; no DAT evidence was available or
    /// confident.
    ContentOnly,
    /// A DAT identity resolved a platform (non-confident tier, or a
    /// DAT-source identity without a per-file hash verdict); content
    /// fusion had no opinion.
    DatOnly,
    /// Neither lane resolved anything.
    Unknown,
}

impl IdentityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Conflict => "Conflict",
            Self::Ambiguous => "Ambiguous",
            Self::VerifiedByDat => "Verified by DAT",
            Self::ContentAndDatAgree => "Content and DAT agree",
            Self::ContentOnly => "Strong content evidence",
            Self::DatOnly => "DAT only",
            Self::Unknown => "Unknown",
        }
    }
}

/// One structured provenance row - `(label, value)`, never a formatted
/// prose sentence at this layer.
pub type ProvenanceRow = (&'static str, String);

/// The reusable read-only view model - milestone section 13. Every field
/// is either already-structured data borrowed/cloned from
/// [`IdentityResult`], or a short derived summary string; no field ever
/// authorizes an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityPresentation {
    /// The resolved/settled platform name, when any lane names one -
    /// prefers content's own resolution, falling back to DAT's.
    pub headline: Option<&'static str>,
    pub platform: Option<&'static str>,
    pub status: IdentityStatus,
    pub content_summary: String,
    pub dat_summary: String,
    pub representation_summary: String,
    pub set_summary: String,
    pub provenance_rows: Vec<ProvenanceRow>,
    pub conflict_rows: Vec<String>,
    pub caveats: Vec<&'static str>,
    /// Always `false` in this milestone - no code path in this crate's
    /// identity stack ever mutates a source file; retained as an explicit
    /// field so a renderer never has to assume it (milestone section 15's
    /// own "Source modified: No" example line).
    pub source_modified: bool,
}

/// Builds an [`IdentityPresentation`] from an already-computed
/// [`IdentityResult`]. Pure: no I/O, no re-analysis, no mutation.
pub fn present_identity(result: &IdentityResult) -> IdentityPresentation {
    let platform = result.content.resolved_platform.or_else(|| {
        result
            .dat
            .as_ref()
            .and_then(DatPlatformIdentity::platform)
            .and_then(platform_by_id)
            .map(|p| p.id)
    });

    let status = compute_status(result);

    IdentityPresentation {
        headline: platform,
        platform,
        status,
        content_summary: content_summary(result),
        dat_summary: dat_summary(result),
        representation_summary: representation_summary(result),
        set_summary: set_summary(result),
        provenance_rows: provenance_rows(result),
        conflict_rows: conflict_rows(result),
        caveats: result.caveats.clone(),
        source_modified: false,
    }
}

fn compute_status(result: &IdentityResult) -> IdentityStatus {
    if result.has_conflict() {
        return IdentityStatus::Conflict;
    }
    let content_ambiguous = result.content.outcome == FusionOutcome::Ambiguous;
    let dat_ambiguous = result
        .dat
        .as_ref()
        .is_some_and(DatPlatformIdentity::is_ambiguous);
    if content_ambiguous || dat_ambiguous {
        return IdentityStatus::Ambiguous;
    }

    let confident_dat_verdict = result.representation_match.as_ref().and_then(|m| match m {
        RepresentationMatchOutcome::PhysicalOnly { verdict }
        | RepresentationMatchOutcome::NormalizedOnly { verdict }
        | RepresentationMatchOutcome::BothAgree { verdict, .. } => Some(verdict),
        RepresentationMatchOutcome::Disagree { .. } | RepresentationMatchOutcome::NoMatch => None,
    });
    let dat_is_confident = confident_dat_verdict.is_some_and(AuditVerdict::is_confident);

    let content_resolved = result.content.outcome == FusionOutcome::Resolved;

    let combined_agrees = result
        .combined
        .as_ref()
        .is_some_and(|view| matches!(view.relationship, IdentityRelationship::Agree { .. }));

    match () {
        () if combined_agrees => IdentityStatus::ContentAndDatAgree,
        () if dat_is_confident => IdentityStatus::VerifiedByDat,
        _ if content_resolved => IdentityStatus::ContentOnly,
        _ if result
            .dat
            .as_ref()
            .is_some_and(|dat| dat.platform().is_some()) =>
        {
            IdentityStatus::DatOnly
        }
        _ => IdentityStatus::Unknown,
    }
}

fn content_summary(result: &IdentityResult) -> String {
    match result.content.outcome {
        FusionOutcome::Resolved => format!(
            "Resolved: {}",
            result.content.resolved_platform.unwrap_or("?")
        ),
        FusionOutcome::Ambiguous => {
            let candidates: Vec<&str> = result
                .content
                .fired_candidates
                .iter()
                .map(|c| c.platform)
                .collect();
            format!("Ambiguous (candidates: {})", candidates.join(", "))
        }
        FusionOutcome::Conflict => format!(
            "Conflict among: {}",
            result.content.conflicting_platforms.join(", ")
        ),
        FusionOutcome::Unknown => "No content evidence resolved a platform".to_string(),
    }
}

fn dat_summary(result: &IdentityResult) -> String {
    match &result.dat {
        None => "No DAT consulted".to_string(),
        Some(DatPlatformIdentity::Unknown) => "DAT: no opinion".to_string(),
        Some(DatPlatformIdentity::Resolved { platform, .. }) => format!("DAT: {platform}"),
        Some(DatPlatformIdentity::Ambiguous { candidates }) => {
            let names: Vec<&str> = candidates.iter().map(|c| c.platform.as_str()).collect();
            format!("DAT: ambiguous ({})", names.join(", "))
        }
    }
}

fn representation_summary(result: &IdentityResult) -> String {
    match &result.representation_match {
        None => "No hash representation compared".to_string(),
        Some(RepresentationMatchOutcome::NoMatch) => "No DAT hash match".to_string(),
        Some(RepresentationMatchOutcome::PhysicalOnly { verdict }) => {
            format!("Matched physical bytes ({})", verdict.label())
        }
        Some(RepresentationMatchOutcome::NormalizedOnly { verdict }) => {
            format!("Matched after normalization ({})", verdict.label())
        }
        Some(RepresentationMatchOutcome::BothAgree {
            verdict,
            identical_bytes,
        }) => {
            if *identical_bytes {
                format!(
                    "Matched physical bytes (already canonical, {})",
                    verdict.label()
                )
            } else {
                format!(
                    "Matched both physical and normalized representations ({})",
                    verdict.label()
                )
            }
        }
        Some(RepresentationMatchOutcome::Disagree { .. }) => {
            "Physical and normalized representations matched different DAT identities".to_string()
        }
    }
}

fn set_summary(result: &IdentityResult) -> String {
    match &result.set_identity {
        None => "Not an archive".to_string(),
        Some(ArchiveSetIdentity::Unknown) => "No member resolved a platform".to_string(),
        Some(ArchiveSetIdentity::SingleMember { platform, .. }) => {
            format!("Single member ({platform})")
        }
        Some(ArchiveSetIdentity::MultiMemberSamePlatform {
            platform,
            member_indices,
        }) => {
            format!(
                "Multi-member, same platform ({platform}, {} members)",
                member_indices.len()
            )
        }
        Some(ArchiveSetIdentity::MultiPlatform { platforms, .. }) => {
            format!("Multi-platform archive ({})", platforms.join(", "))
        }
        Some(ArchiveSetIdentity::StructuredSet { platform, .. }) => {
            format!("Structured set ({platform})")
        }
    }
}

fn provenance_rows(result: &IdentityResult) -> Vec<ProvenanceRow> {
    let mut rows = Vec::new();
    for fact in &result.content.input_evidence {
        rows.push(("content", format!("{:?} = {}", fact.kind, fact.value)));
    }
    if let Some(dat) = &result.dat {
        rows.push(("dat", format!("{dat:?}")));
    }
    rows
}

fn conflict_rows(result: &IdentityResult) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(view) = &result.combined
        && let IdentityRelationship::Disagree {
            content_platform,
            dat_platform,
        } = view.relationship
    {
        rows.push(format!(
            "Content says {content_platform}, DAT says {dat_platform}"
        ));
    }
    if let Some(RepresentationMatchOutcome::Disagree {
        physical_verdict,
        normalized_verdict,
    }) = &result.representation_match
    {
        rows.push(format!(
            "Physical hash matched {:?}, normalized hash matched {:?}",
            physical_verdict, normalized_verdict
        ));
    }
    if let Some(ArchiveSetIdentity::MultiPlatform { platforms, .. }) = &result.set_identity {
        rows.push(format!("Archive contains: {}", platforms.join(", ")));
    }
    rows
}

/// Plain human-readable rendering (milestone section 15). The only prose
/// this module ever produces; every fact it prints already exists on
/// `presentation`.
pub fn render_identity_text(presentation: &IdentityPresentation) -> String {
    let mut out = String::new();
    out.push_str(presentation.platform.unwrap_or("Unknown platform"));
    out.push('\n');
    out.push_str(presentation.status.label());
    out.push_str("\n\n");

    out.push_str("Content:\n  ");
    out.push_str(&presentation.content_summary);
    out.push('\n');

    out.push_str("DAT:\n  ");
    out.push_str(&presentation.dat_summary);
    out.push('\n');

    out.push_str("Representation:\n  ");
    out.push_str(&presentation.representation_summary);
    out.push('\n');

    out.push_str("Set identity:\n  ");
    out.push_str(&presentation.set_summary);
    out.push('\n');

    if !presentation.conflict_rows.is_empty() {
        out.push_str("\nConflicts:\n");
        for row in &presentation.conflict_rows {
            out.push_str("  ");
            out.push_str(row);
            out.push('\n');
        }
        out.push_str("EmuWiz did not choose a winner.\n");
    }

    if !presentation.caveats.is_empty() {
        out.push_str("\nCaveats:\n");
        for caveat in &presentation.caveats {
            out.push_str("  ");
            out.push_str(caveat);
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "\nSource modified: {}\n",
        if presentation.source_modified {
            "Yes"
        } else {
            "No"
        }
    ));
    out
}

#[cfg(test)]
mod tests;
