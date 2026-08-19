use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::platform::identity::{PlatformIdentityResolution, resolve_platform_identity};
use crate::platform_evidence_fusion::fuse_platform_evidence;

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn resolved_saturn() -> ResolutionExplanation {
    fuse_platform_evidence([strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
    )])
}

fn conflict_saturn_vs_xbox() -> ResolutionExplanation {
    fuse_platform_evidence([
        strong(ContentEvidenceKind::BootStructure, "SEGA SEGASATURN"),
        strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
        strong(ContentEvidenceKind::ContentSignature, "XBEH"),
    ])
}

fn ambiguous_ps2() -> ResolutionExplanation {
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

fn unknown_bundle() -> ResolutionExplanation {
    fuse_platform_evidence(Vec::<ContentEvidence>::new())
}

// ------------------------------------------------------------------
// Outcome mapping (section 3, 4)
// ------------------------------------------------------------------

#[test]
fn resolved_outcome_yields_exactly_one_identity_evidence() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].platform, "Saturn");
}

#[test]
fn resolved_evidence_uses_inference_source() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence[0].source, PlatformIdentitySource::Inference);
}

#[test]
fn resolved_evidence_uses_strong_confidence_not_verified_or_high() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence[0].confidence, PlatformIdentityConfidence::Strong);
    assert_ne!(evidence[0].confidence, PlatformIdentityConfidence::Verified);
    assert_ne!(evidence[0].confidence, PlatformIdentityConfidence::High);
    assert_ne!(
        evidence[0].confidence,
        PlatformIdentityConfidence::UserSelected
    );
}

#[test]
fn resolved_evidence_carries_the_requested_generation() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 42);
    assert_eq!(evidence[0].generation, 42);
}

#[test]
fn resolved_evidence_detail_names_the_fired_rule() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert!(evidence[0].detail.contains("saturn_boot_signature"));
}

#[test]
fn unknown_outcome_yields_no_evidence() {
    let explanation = unknown_bundle();
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
    assert!(to_identity_evidence(&explanation, 1).is_empty());
}

#[test]
fn ambiguous_outcome_yields_no_evidence() {
    let explanation = ambiguous_ps2();
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(to_identity_evidence(&explanation, 1).is_empty());
}

#[test]
fn ambiguous_never_produces_a_false_single_platform_assertion() {
    // Regression guard for the exact danger the module doc warns about:
    // an Ambiguous outcome must never be silently narrowed to "the best
    // candidate."
    let explanation = ambiguous_ps2();
    for candidate in &explanation.fired_candidates {
        assert!(
            !to_identity_evidence(&explanation, 1)
                .iter()
                .any(|item| item.platform == candidate.platform)
        );
    }
}

#[test]
fn conflict_outcome_yields_one_item_per_conflicting_platform() {
    let explanation = conflict_saturn_vs_xbox();
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence.len(), explanation.conflicting_platforms.len());
    for platform in &explanation.conflicting_platforms {
        assert!(evidence.iter().any(|item| &item.platform == platform));
    }
}

#[test]
fn conflict_items_are_never_downgraded_below_strong() {
    let explanation = conflict_saturn_vs_xbox();
    let evidence = to_identity_evidence(&explanation, 1);
    for item in &evidence {
        assert_eq!(item.confidence, PlatformIdentityConfidence::Strong);
        assert_eq!(item.source, PlatformIdentitySource::Inference);
    }
}

#[test]
fn conflict_items_all_share_the_requested_generation() {
    let explanation = conflict_saturn_vs_xbox();
    let evidence = to_identity_evidence(&explanation, 7);
    assert!(evidence.iter().all(|item| item.generation == 7));
}

// ------------------------------------------------------------------
// Reusing resolve_platform_identity's own conflict detection (section 3)
// ------------------------------------------------------------------

#[test]
fn conflict_evidence_naturally_becomes_a_platform_identity_conflict() {
    let explanation = conflict_saturn_vs_xbox();
    let evidence = to_identity_evidence(&explanation, 1);
    let resolution = resolve_platform_identity(1, evidence);
    assert!(resolution.is_conflict());
}

#[test]
fn resolved_evidence_naturally_becomes_a_platform_identity_resolution() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    let resolution = resolve_platform_identity(1, evidence);
    match resolution {
        PlatformIdentityResolution::Resolved {
            platform,
            confidence,
            ..
        } => {
            assert_eq!(platform, "Saturn");
            assert_eq!(confidence, PlatformIdentityConfidence::Strong);
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn unknown_evidence_naturally_becomes_a_platform_identity_unknown() {
    let explanation = unknown_bundle();
    let evidence = to_identity_evidence(&explanation, 1);
    let resolution = resolve_platform_identity(1, evidence);
    assert!(matches!(
        resolution,
        PlatformIdentityResolution::Unknown { .. }
    ));
}

#[test]
fn ambiguous_evidence_naturally_becomes_a_platform_identity_unknown() {
    let explanation = ambiguous_ps2();
    let evidence = to_identity_evidence(&explanation, 1);
    let resolution = resolve_platform_identity(1, evidence);
    assert!(matches!(
        resolution,
        PlatformIdentityResolution::Unknown { .. }
    ));
}

#[test]
fn a_stale_generation_is_ignored_exactly_like_any_other_identity_evidence() {
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    // Ask resolve_platform_identity for a different generation - the
    // bridge's own evidence must be filtered out exactly like any other
    // PlatformIdentityEvidence, since to_identity_evidence does not
    // special-case that filtering itself.
    let resolution = resolve_platform_identity(2, evidence);
    assert!(matches!(
        resolution,
        PlatformIdentityResolution::Unknown { .. }
    ));
}

#[test]
fn bridge_evidence_never_outranks_manual_assignment() {
    let explanation = resolved_saturn();
    let mut evidence = to_identity_evidence(&explanation, 1);
    evidence.push(PlatformIdentityEvidence::manual("Dreamcast", 1).unwrap());
    let resolution = resolve_platform_identity(1, evidence);
    assert_eq!(resolution.platform(), Some("Dreamcast"));
}

#[test]
fn bridge_evidence_never_outranks_verified_dat_when_it_agrees_or_disagrees() {
    // resolve_platform_identity's authoritative tier is checked before
    // Inference is ever consulted - the bridge's own evidence simply never
    // gets a vote when DAT/RomM evidence exists at all. This is expected,
    // reviewed behavior (see the module doc's "Content vs. DAT" section)
    // and not something to work around here - compare_content_and_dat is
    // the tool for surfacing a disagreement instead.
    let explanation = resolved_saturn();
    let mut evidence = to_identity_evidence(&explanation, 1);
    evidence.push(
        PlatformIdentityEvidence::canonical(
            "Xbox",
            PlatformIdentitySource::VerifiedDat,
            PlatformIdentityConfidence::Verified,
            1,
            "synthetic verified dat evidence",
        )
        .unwrap(),
    );
    let resolution = resolve_platform_identity(1, evidence);
    assert_eq!(resolution.platform(), Some("Xbox"));
}

// ------------------------------------------------------------------
// Content vs. DAT comparison (sections 5, 6) - ContentDatIdentityView
// ------------------------------------------------------------------

#[test]
fn view_reports_agree_when_dat_matches_resolved_content() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Saturn"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::Agree {
            content_platform: "Saturn",
            dat_platform: "Saturn",
        }
    );
    assert_eq!(view.content_identity.len(), 1);
}

#[test]
fn view_reports_disagree_when_dat_contradicts_resolved_content() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Xbox"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::Disagree {
            content_platform: "Saturn",
            dat_platform: "Xbox",
        }
    );
    // Both trails survive: content_identity still names Saturn, even
    // though the DAT comparison flags the disagreement.
    assert_eq!(view.content_identity[0].platform, "Saturn");
}

#[test]
fn view_disagreement_does_not_erase_the_content_trail() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Xbox"));
    assert!(!view.content_identity.is_empty());
    assert_eq!(
        view.content_identity[0].source,
        PlatformIdentitySource::Inference
    );
}

#[test]
fn view_reports_content_only_when_no_dat_platform_given() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, None);
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::ContentOnly {
            content_platform: "Saturn"
        }
    );
}

#[test]
fn view_reports_dat_only_when_content_did_not_resolve() {
    let explanation = unknown_bundle();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Saturn"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::DatOnly {
            dat_platform: "Saturn"
        }
    );
    assert!(view.content_identity.is_empty());
}

#[test]
fn view_reports_dat_only_when_content_is_ambiguous() {
    let explanation = ambiguous_ps2();
    let view = content_and_dat_identity_view(&explanation, 1, Some("PS2"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::DatOnly {
            dat_platform: "PS2"
        }
    );
    assert!(view.content_identity.is_empty());
}

#[test]
fn view_reports_neither_when_nothing_resolved_and_no_dat_given() {
    let explanation = unknown_bundle();
    let view = content_and_dat_identity_view(&explanation, 1, None);
    assert_eq!(view.dat_comparison, DatContentComparison::Neither);
}

#[test]
fn view_for_conflict_outcome_reports_dat_only_never_a_fake_agreement() {
    let explanation = conflict_saturn_vs_xbox();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Saturn"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::DatOnly {
            dat_platform: "Saturn"
        }
    );
    // Conflict still yields content_identity (both conflicting platforms),
    // even though the DAT comparison itself reads DatOnly (Conflict's
    // resolved_platform is None, which is what compare_content_and_dat
    // actually inspects).
    assert_eq!(view.content_identity.len(), 2);
}

#[test]
fn view_is_deterministic() {
    let explanation = resolved_saturn();
    let first = content_and_dat_identity_view(&explanation, 1, Some("Xbox"));
    let second = content_and_dat_identity_view(&explanation, 1, Some("Xbox"));
    assert_eq!(first, second);
}

#[test]
fn view_generation_flows_through_to_content_identity() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 99, Some("Saturn"));
    assert_eq!(view.content_identity[0].generation, 99);
}

// ------------------------------------------------------------------
// No action authority (section 28)
// ------------------------------------------------------------------

#[test]
fn identity_bridge_source_never_references_mutation_modules() {
    // Source-level structural guard: the bridge module's own source text
    // must never mention any of this crate's action-authority modules -
    // catching an accidental future import long before a behavioral test
    // would.
    let source = include_str!("../identity_bridge.rs");
    for forbidden in [
        "crate::repair",
        "rename_plan",
        "rename_apply",
        "crate::identity_source",
        "std::fs::remove",
        "std::fs::rename",
    ] {
        assert!(
            !source.to_lowercase().contains(&forbidden.to_lowercase()),
            "identity_bridge.rs unexpectedly references {forbidden:?}"
        );
    }
}

#[test]
fn platform_evidence_fusion_source_never_references_mutation_modules() {
    let source = include_str!("../../platform_evidence_fusion.rs");
    for forbidden in [
        "crate::repair",
        "rename_plan",
        "rename_apply",
        "std::fs::remove",
        "std::fs::rename",
    ] {
        assert!(
            !source.contains(forbidden),
            "platform_evidence_fusion.rs unexpectedly references {forbidden:?}"
        );
    }
}

#[test]
fn bridge_output_carries_no_action_bearing_fields() {
    // Structural: PlatformIdentityEvidence's own fields are platform,
    // source, confidence, generation, detail - nothing this bridge
    // produces can be interpreted as a rename/move/delete/apply
    // instruction, matching platform_evidence_fusion's own equivalent
    // Batch 5 test for ResolutionExplanation.
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence[0].platform, "Saturn");
    // No method on PlatformIdentityEvidence or ContentDatIdentityView
    // performs I/O; this test exists to document that boundary.
}

#[test]
fn view_agree_survives_through_to_resolve_platform_identity_alongside_manual_dat_evidence() {
    // End-to-end: a caller feeding both to_identity_evidence's own
    // Inference-tier output and a separately-obtained VerifiedDat item
    // (constructed here the way a real caller would after its own
    // cryptographic audit) into resolve_platform_identity gets the
    // expected Resolved answer, with DAT (not Inference) as the winning
    // tier - exactly the existing, reviewed precedence order.
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, Some("Saturn"));
    assert_eq!(
        view.dat_comparison,
        DatContentComparison::Agree {
            content_platform: "Saturn",
            dat_platform: "Saturn",
        }
    );
    let mut evidence = view.content_identity.clone();
    evidence.push(
        PlatformIdentityEvidence::canonical(
            "Saturn",
            PlatformIdentitySource::VerifiedDat,
            PlatformIdentityConfidence::Verified,
            1,
            "synthetic verified dat evidence agreeing with content",
        )
        .unwrap(),
    );
    let resolution = resolve_platform_identity(1, evidence);
    match resolution {
        PlatformIdentityResolution::Resolved {
            platform,
            confidence,
            ..
        } => {
            assert_eq!(platform, "Saturn");
            assert_eq!(confidence, PlatformIdentityConfidence::Verified);
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn conflict_with_a_single_conflicting_platform_entry_is_structurally_impossible_but_handled() {
    // Defensive: conflicting_platforms should never have length 1 by
    // construction (fuse_platform_evidence only sets Conflict when 2+
    // groups exist), but to_identity_evidence must not panic even if a
    // future fusion change violated that invariant - it would just
    // produce one PlatformIdentityEvidence item, which resolve_platform_identity
    // would then treat as an ordinary Resolved single-platform tier.
    let explanation = ResolutionExplanation {
        outcome: FusionOutcome::Conflict,
        resolved_platform: None,
        fired_candidates: Vec::new(),
        conflicting_platforms: vec!["Saturn"],
        input_evidence: Vec::new(),
    };
    let evidence = to_identity_evidence(&explanation, 1);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].platform, "Saturn");
}

#[test]
fn to_identity_evidence_is_deterministic() {
    let explanation = conflict_saturn_vs_xbox();
    let first = to_identity_evidence(&explanation, 1);
    let second = to_identity_evidence(&explanation, 1);
    assert_eq!(first, second);
}

#[test]
fn resolved_evidence_detail_never_contains_a_filesystem_path() {
    // Matches coverage_inventory's own "no absolute path" discipline -
    // detail text here is provenance prose, not a path a report might
    // leak.
    let explanation = resolved_saturn();
    let evidence = to_identity_evidence(&explanation, 1);
    assert!(!evidence[0].detail.contains("/mnt/"));
    assert!(!evidence[0].detail.contains("/home/"));
}

#[test]
fn view_content_only_never_mistakenly_reports_agree_or_disagree() {
    let explanation = resolved_saturn();
    let view = content_and_dat_identity_view(&explanation, 1, None);
    assert!(!matches!(
        view.dat_comparison,
        DatContentComparison::Agree { .. } | DatContentComparison::Disagree { .. }
    ));
}
