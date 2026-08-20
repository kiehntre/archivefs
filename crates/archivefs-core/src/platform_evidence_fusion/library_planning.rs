//! Batch 10: read-only library planning - `IdentityResult` -> proposal.
//!
//! ```text
//! IdentityResult
//!     -> identity_result_to_resolution (this module)
//!     -> crate::platform::identity::PlatformIdentityResolution  (existing)
//!     -> build_organisation_candidate (this module)
//!     -> crate::dat::rom_organisation::OrganisationCandidate  (existing)
//!     -> crate::dat::rom_organisation::build_organisation_plan  (existing, unchanged)
//!     -> crate::dat::rom_organisation::OrganisationPlanEntry  (existing)
//!     -> plan_status / RommMappingPreview / RenameSuggestion / ArchiveSetIdentity (this module)
//!     -> LibraryItemPlan / LibraryPlanningReport (this module)
//! ```
//!
//! # Why this is a bridge, not a new planner
//!
//! [`crate::dat::rom_organisation`] is **already** a complete, reviewed,
//! read-only organisation-planning system: destination-path building,
//! RomM-slug-gated platform folder naming, name derivation/sanitization
//! (`crate::dat::rename_plan::derive_proposed_basename`,
//! `crate::dat::rename_apply::preflight::is_safe_basename`), collision
//! detection, and an explicit `Suggested`/`AlreadyOrganised`/`Conflict`/
//! `Blocked`/`Unsupported` status vocabulary - everything milestone
//! sections 4-9 and 13-21 ask for. Building a second, parallel model here
//! would violate this project's own "do not create parallel planning
//! systems" rule (repeated across every batch of this milestone). This
//! module's only real job is the one genuine gap:
//! [`IdentityResult`] (Batches 8-9's content+DAT+representation+archive
//! identity stack) has no existing path into
//! [`crate::platform::identity::PlatformIdentityResolution`], the type
//! `rom_organisation` actually consumes. [`identity_result_to_resolution`]
//! is that bridge, built entirely from already-reviewed pieces:
//! [`crate::platform_evidence_fusion::identity_bridge::to_identity_evidence`]
//! for the content lane, and
//! [`crate::platform::identity::PlatformIdentityEvidence::canonical`]
//! (unchanged, Batch 6) for the DAT lane - fed into the existing
//! [`crate::platform::identity::resolve_platform_identity`], never a new
//! authority model.
//!
//! # What genuinely is new here
//!
//! - The DAT-hash-confidence -> [`PlatformIdentitySource`] mapping
//!   (section 17): a confident cryptographic
//!   [`crate::dat::audit::AuditVerdict`] becomes `VerifiedDat`-tier
//!   evidence; a DAT-source-catalogue-only identity (no per-file hash)
//!   stays at the weaker `Inference` tier. This is the one real judgment
//!   call this module makes, and it is the existing `resolve_platform_identity`
//!   tier system - already reviewed - that decides what that evidence is
//!   worth, not new code here.
//! - [`ArchiveSetIdentity`] awareness (sections 10-12, 22): `rom_organisation`
//!   has no concept of "these N members are the same game's disc set" vs.
//!   "these N members are N different games" - [`LibraryItemPlan::set_identity`]
//!   carries that fact alongside the organisation entry, never folding it
//!   into one collapsed destination.
//! - [`RommMappingPreview`] (sections 24-29): a small, honest read of what
//!   `OrganisationPlanEntry.slug`/`platform_source` already computed,
//!   reshaped for a per-platform aggregate view. No real production
//!   canonical-platform-to-RomM-slug table exists anywhere in this crate
//!   (confirmed by inspection before writing this module) -
//!   [`no_slug_mapping`] is the honest default (`None` for everything);
//!   `build_organisation_plan`'s own existing `Unsupported` status already
//!   reports a missing mapping correctly without inventing fake data.
//! - [`PlanStatus`] (section 5): a presentation-level synthesis of the
//!   already-built [`IdentityStatus`] (Batch 9) and the existing
//!   [`OrganisationStatus`] - not a third independent decision, a naming
//!   layer over two decisions that already exist.
//! - [`RenameSuggestion`] (sections 16-17): wraps
//!   `OrganisationPlanEntry`'s own destination basename as a *suggestion*
//!   with an explicit `authorized: false` - this milestone stops at
//!   preview, exactly like every other batch's "no action authority" rule.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dat::audit::AuditVerdict;
use crate::dat::identity::DatPlatformIdentity;
use crate::dat::rom_organisation::{
    OrganisationCandidate, OrganisationMode, OrganisationPlan, OrganisationPlanEntry,
    OrganisationPlanRequest, OrganisationStatus, build_organisation_plan,
};
use crate::platform::identity::{
    PlatformIdentityConfidence, PlatformIdentityEvidence, PlatformIdentityResolution,
    PlatformIdentitySource, resolve_platform_identity,
};
use crate::platform::platform_by_id;

use super::archive_set_identity::ArchiveSetIdentity;
use super::dat_hash_representation::RepresentationMatchOutcome;
use super::identity_orchestrator::IdentityResult;
use super::identity_presentation::{IdentityStatus, present_identity};

// ------------------------------------------------------------------
// The bridge (section 0, "why not a new planner")
// ------------------------------------------------------------------

/// Converts one [`IdentityResult`] into [`PlatformIdentityEvidence`] at the
/// existing, reviewed confidence tiers - never a new tier, never an
/// upgrade of `Suggested` to `Verified` (milestone section 3's own rule).
pub fn identity_result_to_evidence(
    result: &IdentityResult,
    generation: u64,
) -> Vec<PlatformIdentityEvidence> {
    let mut evidence = super::identity_bridge::to_identity_evidence(&result.content, generation);

    // The DAT lane: a confident (cryptographic-hash) representation match
    // is the one thing in this crate's whole identity stack that earns
    // `VerifiedDat` tier - exactly the "exact normalized hash match" the
    // milestone names as a future-rename-authority condition (section 17).
    // Anything weaker (a DAT-source/catalogue name match with no per-file
    // hash) stays at `Inference`, same as content.
    let dat_platform = result.dat.as_ref().and_then(DatPlatformIdentity::platform);
    if let Some(platform) = dat_platform {
        let confident_verdict = result.representation_match.as_ref().and_then(|m| match m {
            RepresentationMatchOutcome::PhysicalOnly { verdict }
            | RepresentationMatchOutcome::NormalizedOnly { verdict }
            | RepresentationMatchOutcome::BothAgree { verdict, .. }
                if verdict.is_confident() =>
            {
                Some(verdict)
            }
            _ => None,
        });
        let item = match confident_verdict {
            Some(verdict) => PlatformIdentityEvidence::canonical(
                platform,
                PlatformIdentitySource::VerifiedDat,
                PlatformIdentityConfidence::Verified,
                generation,
                format!("DAT hash verified this file: {}", verdict_detail(verdict)),
            ),
            None => PlatformIdentityEvidence::canonical(
                platform,
                PlatformIdentitySource::Inference,
                PlatformIdentityConfidence::Inferred,
                generation,
                "DAT-source/catalogue identity, no per-file hash verification".to_string(),
            ),
        };
        evidence.extend(item);
    }
    evidence
}

fn verdict_detail(verdict: &AuditVerdict) -> String {
    match verdict {
        AuditVerdict::Exact {
            game_name,
            algorithm,
            ..
        } => format!("{game_name} ({algorithm})"),
        AuditVerdict::ExactMultipleCandidates { algorithm, .. } => {
            format!("multiple candidates ({algorithm})")
        }
        other => other.label().to_string(),
    }
}

/// [`identity_result_to_evidence`] plus the existing
/// [`resolve_platform_identity`] call - the whole bridge in one step.
///
/// # Why a `combine_identity` disagreement is checked first
///
/// [`resolve_platform_identity`]'s own tier system (Manual absolute, then
/// `VerifiedDat`/`Romm` together as "authoritative", only falling back to
/// `Inference` when no authoritative evidence exists at all) was reviewed
/// and built for a different question: reconciling several *external
/// providers* (a RomM match, a verified DAT audit, a manual assignment).
/// Fed naively, a confident DAT hash match would silently outrank a
/// disagreeing content-fusion inference through that same tier system -
/// exactly "choosing a neat platform just because planning wants a
/// destination," which this milestone's own final rule forbids. Batch 7's
/// [`super::combined_identity::combine_identity`] already exists
/// specifically to catch a content-vs-DAT disagreement regardless of
/// confidence tier (see that module's own doc comment) - when it reports
/// [`super::combined_identity::IdentityRelationship::Disagree`], this
/// function returns [`PlatformIdentityResolution::Conflict`] directly,
/// carrying every piece of evidence gathered, rather than letting the
/// generic multi-provider tier system quietly pick a winner.
pub fn identity_result_to_resolution(
    result: &IdentityResult,
    generation: u64,
) -> PlatformIdentityResolution {
    let evidence = identity_result_to_evidence(result, generation);
    let is_disagreement = result.combined.as_ref().is_some_and(|view| {
        matches!(
            view.relationship,
            super::combined_identity::IdentityRelationship::Disagree { .. }
        )
    });
    if is_disagreement {
        return PlatformIdentityResolution::Conflict {
            generation,
            evidence,
        };
    }
    resolve_platform_identity(generation, evidence)
}

/// The DAT release name a confident hash verdict names, when one exists -
/// the only source this module ever offers as `canonical_name` to
/// [`OrganisationCandidate`] (milestone section 43: prefer the safe
/// fallback of the original basename over a fuzzy rename; a DAT-confirmed
/// release name is the one exception that is not "fuzzy").
fn dat_release_name(result: &IdentityResult) -> Option<String> {
    match &result.representation_match {
        Some(
            RepresentationMatchOutcome::PhysicalOnly { verdict }
            | RepresentationMatchOutcome::NormalizedOnly { verdict }
            | RepresentationMatchOutcome::BothAgree { verdict, .. },
        ) if verdict.is_confident() => match verdict {
            AuditVerdict::Exact { rom_name, .. } => Some(rom_name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Builds one [`OrganisationCandidate`] from a source path and its already
/// computed [`IdentityResult`] - the caller supplies both (this module
/// never re-probes/re-hashes a file itself, matching milestone section 45's
/// performance rule).
pub fn build_organisation_candidate(
    source_path: PathBuf,
    result: &IdentityResult,
    generation: u64,
) -> OrganisationCandidate {
    OrganisationCandidate {
        source_path,
        resolution: identity_result_to_resolution(result, generation),
        canonical_name: dat_release_name(result),
        content_classification: None,
        original_metadata: Default::default(),
    }
}

// ------------------------------------------------------------------
// Plan status (section 5) - synthesis over two existing decisions
// ------------------------------------------------------------------

/// The milestone's own status vocabulary (section 5), computed from the
/// already-decided [`IdentityStatus`] and [`OrganisationStatus`] - never a
/// third independent judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    Ready,
    NeedsReview,
    Ambiguous,
    Conflict,
    Unknown,
    Unsupported,
}

impl PlanStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::NeedsReview => "Needs review",
            Self::Ambiguous => "Ambiguous",
            Self::Conflict => "Conflict",
            Self::Unknown => "Unknown",
            Self::Unsupported => "Unsupported",
        }
    }
}

/// Derives [`PlanStatus`] - identity-level problems (`Conflict`/`Ambiguous`/
/// `Unknown`) always take priority over the organisation entry's own status,
/// since an unreliable identity makes any destination proposal unreliable
/// too, regardless of what `build_organisation_plan` itself decided.
pub fn plan_status(identity_status: IdentityStatus, org_status: OrganisationStatus) -> PlanStatus {
    match identity_status {
        IdentityStatus::Conflict => PlanStatus::Conflict,
        IdentityStatus::Ambiguous => PlanStatus::Ambiguous,
        IdentityStatus::Unknown => PlanStatus::Unknown,
        _ => match org_status {
            OrganisationStatus::Suggested | OrganisationStatus::AlreadyOrganised => {
                PlanStatus::Ready
            }
            OrganisationStatus::Conflict => PlanStatus::Conflict,
            OrganisationStatus::Blocked => PlanStatus::NeedsReview,
            OrganisationStatus::Unsupported => PlanStatus::Unsupported,
        },
    }
}

// ------------------------------------------------------------------
// RomM mapping preview (sections 24-29)
// ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RommMappingStatus {
    Mapped,
    Unmapped,
    Ambiguous,
    Unsupported,
}

/// A read-only RomM mapping preview - milestone section 25. Built entirely
/// from an already-computed [`OrganisationPlanEntry`]; never calls RomM,
/// never writes RomM config.
///
/// Batch 10: `Serialize` only, not `Deserialize` - `warnings` holds
/// `&'static str` literals, which cannot round-trip through an owned
/// deserializer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RommMappingPreview {
    pub canonical_platform: Option<String>,
    pub slug: Option<String>,
    pub status: RommMappingStatus,
    pub warnings: Vec<&'static str>,
}

/// The honest default slug source (milestone section 24's audit found no
/// real production canonical-platform-to-RomM-slug table anywhere in this
/// crate) - always `None`, so every preview reports `Unmapped` unless a
/// caller supplies a real mapping via [`LibraryPlanningContext`].
pub fn no_slug_mapping(_platform: &str) -> Option<String> {
    None
}

/// The library-native destination-folder fallback used when no RomM slug
/// is available (milestone sections 35-36) - the canonical platform id
/// itself, exactly as the registry spells it (e.g. `"N64"`, `"Game Boy
/// Advance"`). Deliberately distinct from, and never confusable with, a
/// RomM slug: RomM slugs are lowercase/dashed provider-specific strings
/// (`"n64"`, `"game-boy-advance"`); this is the crate's own canonical id,
/// used only to compute a real destination path when RomM has none to
/// offer. `entry.slug` on the returned [`LibraryItemPlan::organisation`]
/// is always patched back to the *real* RomM resolution (or `None`) before
/// a plan is returned, so nothing downstream ever mistakes this fallback
/// for a RomM slug. `None` only when `platform_id` is not a real
/// registered platform at all.
fn canonical_library_folder_name(platform_id: &str) -> Option<String> {
    platform_by_id(platform_id).map(|platform| platform.id.to_string())
}

pub fn romm_mapping_preview(entry: &OrganisationPlanEntry) -> RommMappingPreview {
    let status = match (&entry.platform, &entry.slug) {
        (None, _) => RommMappingStatus::Unsupported,
        (Some(_), Some(_)) => RommMappingStatus::Mapped,
        (Some(_), None) if entry.status == OrganisationStatus::Conflict => {
            RommMappingStatus::Ambiguous
        }
        (Some(_), None) => RommMappingStatus::Unmapped,
    };
    let mut warnings = Vec::new();
    if status == RommMappingStatus::Unmapped {
        warnings.push("no RomM slug mapping exists for this canonical platform yet");
    }
    RommMappingPreview {
        canonical_platform: entry.platform.clone(),
        slug: entry.slug.clone(),
        status,
        warnings,
    }
}

// ------------------------------------------------------------------
// Rename suggestion (sections 16-17, 43) - always unauthorized
// ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenameBasis {
    /// The destination basename came from a confident, DAT-hash-verified
    /// release name.
    AuthoritativeDatRelease,
    /// The original basename was preserved (no confirmed release name) -
    /// the milestone's own preferred fallback (section 43).
    OriginalNamePreserved,
    /// No safe suggestion exists at all (Blocked/Unsupported/Conflict).
    Unavailable,
}

/// A read-only rename suggestion - milestone section 16. `authorized` is
/// always `false` in this milestone; nothing here ever renames a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSuggestion {
    pub original_name: String,
    pub proposed_name: Option<String>,
    pub basis: RenameBasis,
    pub blockers: Vec<String>,
    pub authorized: bool,
}

pub fn rename_suggestion(entry: &OrganisationPlanEntry) -> RenameSuggestion {
    let original_name = entry
        .source_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let proposed_name = entry
        .destination_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());

    let (basis, blockers) = match entry.status {
        OrganisationStatus::Suggested | OrganisationStatus::AlreadyOrganised => {
            let basis = if entry.platform_source == "Verified DAT" {
                RenameBasis::AuthoritativeDatRelease
            } else {
                RenameBasis::OriginalNamePreserved
            };
            (basis, Vec::new())
        }
        OrganisationStatus::Conflict
        | OrganisationStatus::Blocked
        | OrganisationStatus::Unsupported => {
            let reason = entry
                .reason
                .clone()
                .unwrap_or_else(|| entry.status.label().to_string());
            (RenameBasis::Unavailable, vec![reason])
        }
    };

    RenameSuggestion {
        original_name,
        proposed_name: if matches!(basis, RenameBasis::Unavailable) {
            None
        } else {
            proposed_name
        },
        basis,
        blockers,
        // Never true in this milestone - see the module doc comment.
        authorized: false,
    }
}

// ------------------------------------------------------------------
// One item's full plan (sections 10-12, 22)
// ------------------------------------------------------------------

/// One source item's complete, read-only plan.
///
/// Batch 10: `Serialize` only (milestone section 40) - `set_identity` and
/// `romm` each carry `&'static str` platform ids that cannot round-trip
/// through an owned `Deserialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryItemPlan {
    pub organisation: OrganisationPlanEntry,
    /// The archive set-identity axis, when this item came from a
    /// multi-member archive - kept entirely separate from `organisation`'s
    /// own single-file destination (milestone sections 10-12: "same
    /// platform" is never conflated with "same game").
    pub set_identity: Option<ArchiveSetIdentity>,
    pub status: PlanStatus,
    pub romm: RommMappingPreview,
    pub rename: RenameSuggestion,
}

/// Builds one item's plan from *two* independently computed organisation
/// entries for the same candidate - milestone sections 35-36's decoupling.
///
/// `library_entry` was built with a slug resolver that falls back to
/// [`canonical_library_folder_name`] when no real RomM slug exists, so its
/// `status`/`destination_path` reflect library-plan readiness on its own:
/// a confidently resolved platform can be `Ready` here with zero RomM
/// involvement. `romm_entry` was built with the caller's real RomM slug
/// resolver only, so its `slug`/`status` are the honest RomM-specific
/// picture (`Unsupported` there just means "no RomM slug", never "identity
/// failed"). Only [`romm_mapping_preview`] ever reads from `romm_entry`;
/// everything else (destination, rename, status) reads from
/// `library_entry`, whose own `.slug` field is patched back to
/// `romm_entry.slug` before being returned so a caller reading
/// `organisation.slug` off the result always sees the real RomM slug (or
/// `None`), never the internal library-folder fallback.
fn build_item_plan(
    library_entry: OrganisationPlanEntry,
    romm_entry: &OrganisationPlanEntry,
    identity_status: IdentityStatus,
    set_identity: Option<ArchiveSetIdentity>,
) -> LibraryItemPlan {
    let romm = romm_mapping_preview(romm_entry);
    let mut entry = library_entry;
    entry.slug = romm_entry.slug.clone();
    let rename = rename_suggestion(&entry);
    let status = plan_status(identity_status, entry.status);
    LibraryItemPlan {
        organisation: entry,
        set_identity,
        status,
        romm,
        rename,
    }
}

// ------------------------------------------------------------------
// Collection-level planner (sections 28-30)
// ------------------------------------------------------------------

/// Explicit planning configuration - milestone section 29. Never bakes in
/// a user-specific path; the caller always supplies `destination_root`.
pub struct LibraryPlanningContext<'a> {
    pub destination_root: &'a Path,
    pub mode: OrganisationMode,
    /// Resolves a canonical platform id to a RomM slug - defaults to
    /// [`no_slug_mapping`] (honest "no mapping exists") when the caller has
    /// no real source; see the module doc comment.
    pub slug_for_platform: &'a dyn Fn(&str) -> Option<String>,
    pub generation: u64,
}

/// One item to plan: its source path, already-computed identity, and
/// optional archive set identity.
pub struct LibraryPlanInput {
    pub source_path: PathBuf,
    pub identity: IdentityResult,
    pub set_identity: Option<ArchiveSetIdentity>,
    /// The physical file's own cryptographic hash, when the caller already
    /// computed one (e.g. during DAT hash-representation auditing) -
    /// Batch 11's duplicate taxonomy indexes on this rather than
    /// re-hashing anything itself (milestone section 53). `None` when the
    /// caller has no hash to offer; duplicate detection simply skips this
    /// axis for that item rather than computing one.
    pub physical_hash: Option<String>,
    /// The normalized representation's hash, under the same "caller
    /// already computed it, planner never re-hashes" rule.
    pub normalized_hash: Option<String>,
    /// Batch 12: this item's DAT-declared `cloneof` lineage, when the
    /// caller already resolved one via
    /// [`super::release_relationship::resolve_release_relationship`] -
    /// `None` when no DAT/index was available. The planner never looks
    /// this up itself (no `DatIndex` is held here).
    pub release_relationship: Option<super::release_relationship::ReleaseRelationship>,
}

/// The aggregated result of planning a whole collection - milestone
/// section 28/30.
///
/// Batch 10: `Serialize` only (milestone section 40) - see
/// [`LibraryItemPlan`]'s own doc comment for why `Deserialize` is not
/// derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryPlanningReport {
    pub organisation_plan: OrganisationPlan,
    pub items: Vec<LibraryItemPlan>,
    pub ready: usize,
    pub needs_review: usize,
    pub ambiguous: usize,
    pub conflict: usize,
    pub unknown: usize,
    pub unsupported: usize,
    pub romm_mapped: usize,
    pub romm_unmapped: usize,
}

/// Plans a whole collection - milestone section 28's `plan_library`. Pure
/// composition of already-reviewed pieces: `build_organisation_plan` does
/// the actual destination/collision/safety work, unchanged. Called
/// *twice* internally (milestone sections 35-36's RomM decoupling - see
/// [`build_item_plan`]'s own doc comment): once with the caller's real RomM
/// slug resolver, once with a library-native fallback, so library-plan
/// readiness is never held hostage to RomM mapping availability.
pub fn plan_library(
    inputs: &[LibraryPlanInput],
    context: &LibraryPlanningContext<'_>,
) -> LibraryPlanningReport {
    let candidates: Vec<OrganisationCandidate> = inputs
        .iter()
        .map(|input| {
            build_organisation_candidate(
                input.source_path.clone(),
                &input.identity,
                context.generation,
            )
        })
        .collect();

    let romm_request = OrganisationPlanRequest {
        master_root: context.destination_root,
        mode: context.mode,
        content_policy: Default::default(),
        candidates: &candidates,
        slug_for_platform: context.slug_for_platform,
        generation: context.generation,
    };
    let romm_plan = build_organisation_plan(&romm_request);

    let library_slug_for_platform = |platform: &str| {
        (context.slug_for_platform)(platform).or_else(|| canonical_library_folder_name(platform))
    };
    let library_request = OrganisationPlanRequest {
        master_root: context.destination_root,
        mode: context.mode,
        content_policy: Default::default(),
        candidates: &candidates,
        slug_for_platform: &library_slug_for_platform,
        generation: context.generation,
    };
    let organisation_plan = build_organisation_plan(&library_request);

    // organisation_plan.entries is sorted (status, source_path,
    // destination_path) by build_organisation_plan itself - match each
    // entry back to its input, and to its counterpart in romm_plan, by
    // source_path for a stable, deterministic per-item plan regardless of
    // input order (milestone section 44).
    let mut items = Vec::with_capacity(organisation_plan.entries.len());
    for entry in &organisation_plan.entries {
        let input = inputs
            .iter()
            .find(|input| input.source_path == entry.source_path)
            .expect("every entry corresponds to a supplied input");
        let romm_entry = romm_plan
            .entries
            .iter()
            .find(|romm_entry| romm_entry.source_path == entry.source_path)
            .expect("romm_plan was built from the same candidates as organisation_plan");
        let identity_status = present_identity(&input.identity).status;
        items.push(build_item_plan(
            entry.clone(),
            romm_entry,
            identity_status,
            input.set_identity.clone(),
        ));
    }

    let mut report = LibraryPlanningReport {
        organisation_plan,
        items,
        ready: 0,
        needs_review: 0,
        ambiguous: 0,
        conflict: 0,
        unknown: 0,
        unsupported: 0,
        romm_mapped: 0,
        romm_unmapped: 0,
    };
    for item in &report.items {
        match item.status {
            PlanStatus::Ready => report.ready += 1,
            PlanStatus::NeedsReview => report.needs_review += 1,
            PlanStatus::Ambiguous => report.ambiguous += 1,
            PlanStatus::Conflict => report.conflict += 1,
            PlanStatus::Unknown => report.unknown += 1,
            PlanStatus::Unsupported => report.unsupported += 1,
        }
        match item.romm.status {
            RommMappingStatus::Mapped => report.romm_mapped += 1,
            _ => report.romm_unmapped += 1,
        }
    }
    report
}

#[cfg(test)]
mod tests;
