use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::identity::{DatPlatformEvidenceKind, DatPlatformIdentity};
use crate::platform_evidence_fusion::fuse_platform_evidence;

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn resolved_saturn_content() -> ResolutionExplanation {
    fuse_platform_evidence([strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
    )])
}

fn resolved_xbox_content() -> ResolutionExplanation {
    fuse_platform_evidence([
        strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
        strong(ContentEvidenceKind::ContentSignature, "XBEH"),
    ])
}

fn ambiguous_content() -> ResolutionExplanation {
    fuse_platform_evidence([
        ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "BOOT2",
            ContentEvidenceConfidence::Corroborated,
            "test fact",
        ),
        ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "ELF",
            ContentEvidenceConfidence::Weak,
            "test fact",
        ),
    ])
}

fn unknown_content() -> ResolutionExplanation {
    fuse_platform_evidence(Vec::<ContentEvidence>::new())
}

fn dat_evidence(platform: &str, confidence: DatPlatformConfidence) -> DatPlatformEvidence {
    DatPlatformEvidence {
        platform: platform.to_string(),
        machine_key: None,
        kind: DatPlatformEvidenceKind::HeaderName,
        confidence,
        detail: "test evidence".to_string(),
    }
}

fn resolved_dat(platform: &str) -> DatPlatformIdentity {
    crate::dat::identity::resolve_dat_platform_identity([dat_evidence(
        platform,
        DatPlatformConfidence::Strong,
    )])
}

fn ambiguous_dat(a: &str, b: &str) -> DatPlatformIdentity {
    crate::dat::identity::resolve_dat_platform_identity([
        dat_evidence(a, DatPlatformConfidence::Strong),
        dat_evidence(b, DatPlatformConfidence::Strong),
    ])
}

fn unknown_dat() -> DatPlatformIdentity {
    crate::dat::identity::resolve_dat_platform_identity(Vec::new())
}

// ------------------------------------------------------------------
// Section 5A: Agree
// ------------------------------------------------------------------

#[test]
fn content_x_dat_x_is_agree() {
    let view = combine_identity(&resolved_saturn_content(), &resolved_dat("Saturn"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::Agree { platform: "Saturn" }
    );
    assert!(view.relationship.is_agreement());
}

#[test]
fn agree_retains_both_platforms_separately() {
    let view = combine_identity(&resolved_saturn_content(), &resolved_dat("Saturn"));
    assert_eq!(view.content_platform, Some("Saturn"));
    assert_eq!(view.dat_platform, Some("Saturn"));
}

#[test]
fn agree_folds_equivalent_canonical_ids_pc_engine_turbografx() {
    // Content resolves nothing on its own here (no PC Engine detector
    // exists), so this exercises the equivalence-folding path directly via
    // a synthetic Resolved ResolutionExplanation.
    let synthetic_content = ResolutionExplanation {
        outcome: FusionOutcome::Resolved,
        resolved_platform: Some("PC Engine"),
        fired_candidates: Vec::new(),
        conflicting_platforms: Vec::new(),
        input_evidence: Vec::new(),
    };
    let view = combine_identity(&synthetic_content, &resolved_dat("TurboGrafx-16"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::Agree {
            platform: "PC Engine"
        }
    );
}

// ------------------------------------------------------------------
// Section 5D / 7: Disagree - non-negotiable, adversarial
// ------------------------------------------------------------------

#[test]
fn strong_content_xbox_vs_strong_dat_xbox360_is_disagree_never_a_winner() {
    let view = combine_identity(&resolved_xbox_content(), &resolved_dat("Xbox360"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::Disagree {
            content_platform: "Xbox",
            dat_platform: "Xbox360",
        }
    );
    assert!(view.relationship.is_conflict());
}

#[test]
fn disagree_never_silently_prefers_dat() {
    let view = combine_identity(&resolved_saturn_content(), &resolved_dat("Xbox"));
    match view.relationship {
        IdentityRelationship::Disagree {
            content_platform,
            dat_platform,
        } => {
            assert_eq!(content_platform, "Saturn");
            assert_eq!(dat_platform, "Xbox");
        }
        other => panic!("expected Disagree, got {other:?}"),
    }
}

#[test]
fn disagree_never_silently_prefers_content() {
    // Same bundle, just checking the platform ordering is not swapped or
    // otherwise editorialized.
    let view = combine_identity(&resolved_xbox_content(), &resolved_dat("Saturn"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::Disagree {
            content_platform: "Xbox",
            dat_platform: "Saturn",
        }
    );
}

#[test]
fn disagree_still_retains_both_platforms_on_the_view() {
    let view = combine_identity(&resolved_xbox_content(), &resolved_dat("Xbox360"));
    assert_eq!(view.content_platform, Some("Xbox"));
    assert_eq!(view.dat_platform, Some("Xbox360"));
}

// ------------------------------------------------------------------
// Section 5B/5C: ContentOnly / DatOnly
// ------------------------------------------------------------------

#[test]
fn content_x_dat_unknown_is_content_only() {
    let view = combine_identity(&resolved_saturn_content(), &unknown_dat());
    assert_eq!(
        view.relationship,
        IdentityRelationship::ContentOnly { platform: "Saturn" }
    );
}

#[test]
fn content_unknown_dat_x_is_dat_only() {
    let view = combine_identity(&unknown_content(), &resolved_dat("Saturn"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::DatOnly { platform: "Saturn" }
    );
}

#[test]
fn content_conflict_dat_x_is_still_dat_only() {
    let conflict_content = fuse_platform_evidence([
        strong(ContentEvidenceKind::BootStructure, "SEGA SEGASATURN"),
        strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
        strong(ContentEvidenceKind::ContentSignature, "XBEH"),
    ]);
    assert_eq!(conflict_content.outcome, FusionOutcome::Conflict);
    let view = combine_identity(&conflict_content, &resolved_dat("Saturn"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::DatOnly { platform: "Saturn" }
    );
}

// ------------------------------------------------------------------
// Section 5G: both Unknown
// ------------------------------------------------------------------

#[test]
fn both_unknown_is_neither() {
    let view = combine_identity(&unknown_content(), &unknown_dat());
    assert_eq!(view.relationship, IdentityRelationship::Neither);
}

// ------------------------------------------------------------------
// Section 5E: content Ambiguous, DAT resolves - never auto-promoted
// ------------------------------------------------------------------

#[test]
fn content_ambiguous_dat_resolved_never_auto_promotes() {
    let content = ambiguous_content();
    assert_eq!(content.outcome, FusionOutcome::Ambiguous);
    let view = combine_identity(&content, &resolved_dat("PS2"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::ContentAmbiguous {
            dat_platform: Some("PS2")
        }
    );
    // Structural: never Agree, never ContentOnly, never DatOnly.
    assert!(!view.relationship.is_agreement());
}

#[test]
fn dat_never_silently_promotes_an_ambiguous_content_candidate() {
    // Even when the DAT platform happens to be exactly the same platform
    // that fired as one of content's own Ambiguous candidates, the
    // relationship must still read ContentAmbiguous, not Agree - promotion
    // would require an explicit, separately reviewed rule this milestone
    // does not add.
    let content = ambiguous_content();
    let dat_platform = content
        .fired_candidates
        .first()
        .map(|c| c.platform)
        .expect("ambiguous PS2 bundle fires at least one candidate");
    let view = combine_identity(&content, &resolved_dat(dat_platform));
    assert!(matches!(
        view.relationship,
        IdentityRelationship::ContentAmbiguous { .. }
    ));
    assert_ne!(
        view.relationship,
        IdentityRelationship::Agree {
            platform: dat_platform
        }
    );
}

#[test]
fn content_ambiguous_dat_unknown_still_reports_content_ambiguous() {
    let view = combine_identity(&ambiguous_content(), &unknown_dat());
    assert_eq!(
        view.relationship,
        IdentityRelationship::ContentAmbiguous { dat_platform: None }
    );
}

// ------------------------------------------------------------------
// Section 5F: DAT ambiguous, content resolves - retain both
// ------------------------------------------------------------------

#[test]
fn dat_ambiguous_content_resolved_retains_both_facts() {
    let view = combine_identity(
        &resolved_saturn_content(),
        &ambiguous_dat("Xbox", "Xbox360"),
    );
    assert_eq!(
        view.relationship,
        IdentityRelationship::DatAmbiguous {
            content_platform: Some("Saturn")
        }
    );
}

#[test]
fn dat_ambiguous_content_unknown_reports_no_content_platform() {
    let view = combine_identity(&unknown_content(), &ambiguous_dat("Xbox", "Xbox360"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::DatAmbiguous {
            content_platform: None
        }
    );
}

#[test]
fn dat_ambiguous_content_conflict_reports_no_content_platform() {
    let conflict_content = fuse_platform_evidence([
        strong(ContentEvidenceKind::BootStructure, "SEGA SEGASATURN"),
        strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
        strong(ContentEvidenceKind::ContentSignature, "XBEH"),
    ]);
    let view = combine_identity(&conflict_content, &ambiguous_dat("Xbox", "Xbox360"));
    assert_eq!(
        view.relationship,
        IdentityRelationship::DatAmbiguous {
            content_platform: None
        }
    );
}

// ------------------------------------------------------------------
// Both ambiguous
// ------------------------------------------------------------------

#[test]
fn both_ambiguous_is_both_ambiguous() {
    let view = combine_identity(&ambiguous_content(), &ambiguous_dat("Xbox", "Xbox360"));
    assert_eq!(view.relationship, IdentityRelationship::BothAmbiguous);
}

// ------------------------------------------------------------------
// Structural / determinism / no-action-authority
// ------------------------------------------------------------------

#[test]
fn combine_identity_is_deterministic() {
    let content = resolved_saturn_content();
    let dat = resolved_dat("Saturn");
    assert_eq!(
        combine_identity(&content, &dat),
        combine_identity(&content, &dat)
    );
}

#[test]
fn dat_outcome_mirrors_dat_platform_identity_shape() {
    assert_eq!(
        combine_identity(&unknown_content(), &unknown_dat()).dat_outcome,
        DatOutcome::Unknown
    );
    assert_eq!(
        combine_identity(&unknown_content(), &resolved_dat("Saturn")).dat_outcome,
        DatOutcome::Resolved
    );
    assert_eq!(
        combine_identity(&unknown_content(), &ambiguous_dat("Xbox", "Xbox360")).dat_outcome,
        DatOutcome::Ambiguous
    );
}

#[test]
fn combined_view_carries_no_action_bearing_fields() {
    let view = combine_identity(&resolved_saturn_content(), &resolved_dat("Saturn"));
    let _outcome: FusionOutcome = view.content_outcome;
    let _content_platform: Option<&str> = view.content_platform;
    let _dat_outcome: DatOutcome = view.dat_outcome;
    let _dat_platform: Option<&str> = view.dat_platform;
    let _relationship: IdentityRelationship = view.relationship;
    // No field or method here can mutate a filesystem/database/RomM
    // record - this test documents that boundary, matching every prior
    // batch's own equivalent test.
}

#[test]
fn combined_identity_source_never_references_mutation_modules() {
    let source = include_str!("../combined_identity.rs");
    for forbidden in [
        "crate::repair",
        "rename_plan",
        "rename_apply",
        "std::fs::remove",
        "std::fs::rename",
    ] {
        assert!(
            !source.contains(forbidden),
            "combined_identity.rs unexpectedly references {forbidden:?}"
        );
    }
}

// ------------------------------------------------------------------
// DatSourceProvenance (section 16)
// ------------------------------------------------------------------

#[test]
fn dat_source_provenance_is_none_for_unknown() {
    assert_eq!(dat_source_provenance(&unknown_dat()), None);
}

#[test]
fn dat_source_provenance_is_none_for_ambiguous() {
    assert_eq!(
        dat_source_provenance(&ambiguous_dat("Xbox", "Xbox360")),
        None
    );
}

#[test]
fn dat_source_provenance_names_the_deciding_evidence_kind() {
    let dat = resolved_dat("Saturn");
    let provenance = dat_source_provenance(&dat).unwrap();
    assert_eq!(provenance.platform, "Saturn");
    assert_eq!(provenance.confidence, DatPlatformConfidence::Strong);
    assert_eq!(provenance.deciding_kind_label, "DAT header name");
}

#[test]
fn dat_source_provenance_never_contains_a_filesystem_path() {
    let dat = resolved_dat("Saturn");
    let provenance = dat_source_provenance(&dat).unwrap();
    assert!(!provenance.deciding_kind_label.contains('/'));
}

#[test]
fn dat_source_provenance_is_deterministic() {
    let dat = resolved_dat("Saturn");
    assert_eq!(dat_source_provenance(&dat), dat_source_provenance(&dat));
}
