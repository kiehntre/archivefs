//! Batch 5 sections 27 (DAT/content interaction) and 28 (normalized-view
//! provenance).

use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

fn strong(kind: ContentEvidenceKind, value: &str, detail: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, detail)
}

// ------------------------------------------------------------------
// DAT / content interaction (section 27)
// ------------------------------------------------------------------

#[test]
fn content_only_when_no_dat_platform_supplied() {
    let evidence = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let explanation = fuse_platform_evidence(evidence);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    let comparison = compare_content_and_dat(&explanation, None);
    assert_eq!(
        comparison,
        DatContentComparison::ContentOnly {
            content_platform: "Saturn"
        }
    );
}

#[test]
fn dat_only_when_content_did_not_resolve() {
    let explanation = fuse_platform_evidence(Vec::<ContentEvidence>::new());
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
    let comparison = compare_content_and_dat(&explanation, Some("Saturn"));
    assert_eq!(
        comparison,
        DatContentComparison::DatOnly {
            dat_platform: "Saturn"
        }
    );
}

#[test]
fn dat_only_when_content_is_only_ambiguous() {
    let evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "ELF",
        ContentEvidenceConfidence::Weak,
        "generic ELF magic",
    )];
    let explanation = fuse_platform_evidence(evidence);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
    let comparison = compare_content_and_dat(&explanation, Some("PS2"));
    assert_eq!(
        comparison,
        DatContentComparison::DatOnly {
            dat_platform: "PS2"
        }
    );
}

#[test]
fn neither_when_nothing_resolved_and_no_dat_supplied() {
    let explanation = fuse_platform_evidence(Vec::<ContentEvidence>::new());
    let comparison = compare_content_and_dat(&explanation, None);
    assert_eq!(comparison, DatContentComparison::Neither);
}

#[test]
fn agree_when_dat_names_the_same_canonical_platform() {
    let evidence = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let explanation = fuse_platform_evidence(evidence);
    let comparison = compare_content_and_dat(&explanation, Some("Saturn"));
    assert_eq!(
        comparison,
        DatContentComparison::Agree {
            content_platform: "Saturn",
            dat_platform: "Saturn",
        }
    );
}

#[test]
fn agree_when_dat_names_an_equivalent_canonical_platform() {
    // PC Engine / TurboGrafx-16 fold via the crate's own equivalence rules,
    // never a false conflict - see platform::equivalent_platform_ids.
    let saturn_explanation = fuse_platform_evidence(vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )]);
    // Sanity: Saturn has no PC Engine equivalence, so build the comparison
    // directly against a fabricated Resolved explanation naming one of the
    // real equivalent pair instead, to exercise the equivalence branch
    // itself without depending on Saturn.
    let _ = saturn_explanation; // keep the realistic case above too

    let synthetic = ResolutionExplanation {
        outcome: FusionOutcome::Resolved,
        resolved_platform: Some("PC Engine"),
        fired_candidates: Vec::new(),
        conflicting_platforms: Vec::new(),
        input_evidence: Vec::new(),
    };
    let comparison = compare_content_and_dat(&synthetic, Some("TurboGrafx-16"));
    assert_eq!(
        comparison,
        DatContentComparison::Agree {
            content_platform: "PC Engine",
            dat_platform: "TurboGrafx-16",
        }
    );
}

#[test]
fn disagree_when_dat_names_a_genuinely_different_platform() {
    let evidence = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let explanation = fuse_platform_evidence(evidence);
    let comparison = compare_content_and_dat(&explanation, Some("Xbox"));
    assert_eq!(
        comparison,
        DatContentComparison::Disagree {
            content_platform: "Saturn",
            dat_platform: "Xbox",
        }
    );
}

#[test]
fn disagree_never_silently_prefers_either_source() {
    // A DAT match must never silently override a strong internal-content
    // contradiction - the comparison must name both sides, not pick one.
    let evidence = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let explanation = fuse_platform_evidence(evidence);
    let comparison = compare_content_and_dat(&explanation, Some("Xbox"));
    match comparison {
        DatContentComparison::Disagree {
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
fn comparison_is_deterministic() {
    let evidence = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let explanation = fuse_platform_evidence(evidence);
    let first = compare_content_and_dat(&explanation, Some("Xbox"));
    let second = compare_content_and_dat(&explanation, Some("Xbox"));
    assert_eq!(first, second);
}

#[test]
fn conflict_content_outcome_still_compares_as_no_content_platform() {
    // When content fusion itself lands on FusionOutcome::Conflict (two
    // strong, non-equivalent platforms), resolved_platform is None - so a
    // DAT comparison against that explanation must read as DatOnly, never
    // as if content had actually resolved to something.
    let evidence = vec![
        strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
            "Saturn boot header",
        ),
        strong(ContentEvidenceKind::Filesystem, "XDVDFS", "Xbox filesystem"),
        strong(
            ContentEvidenceKind::ContentSignature,
            "XBEH",
            "Xbox executable",
        ),
    ];
    let explanation = fuse_platform_evidence(evidence);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    let comparison = compare_content_and_dat(&explanation, Some("Saturn"));
    assert_eq!(
        comparison,
        DatContentComparison::DatOnly {
            dat_platform: "Saturn"
        }
    );
}

// ------------------------------------------------------------------
// Normalized-view provenance (section 28)
// ------------------------------------------------------------------

#[test]
fn tagging_prefixes_the_detail_field() {
    let evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "n64-header-valid",
        ContentEvidenceConfidence::Strong,
        "byte-order-corrected N64 header",
    )];
    let tagged = tag_normalized_view_evidence(evidence);
    assert_eq!(tagged.len(), 1);
    assert!(tagged[0].detail.starts_with("[normalized view] "));
    assert!(tagged[0].detail.contains("byte-order-corrected N64 header"));
}

#[test]
fn tagging_never_changes_kind_value_or_confidence() {
    let original = ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "n64-header-valid",
        ContentEvidenceConfidence::Strong,
        "byte-order-corrected N64 header",
    );
    let tagged = tag_normalized_view_evidence(vec![original.clone()]);
    assert_eq!(tagged[0].kind, original.kind);
    assert_eq!(tagged[0].value, original.value);
    assert_eq!(tagged[0].confidence, original.confidence);
}

#[test]
fn tagging_is_idempotent() {
    let evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "n64-header-valid",
        ContentEvidenceConfidence::Strong,
        "byte-order-corrected N64 header",
    )];
    let once = tag_normalized_view_evidence(evidence);
    let twice = tag_normalized_view_evidence(once.clone());
    assert_eq!(once, twice);
    assert_eq!(twice[0].detail.matches("[normalized view]").count(), 1);
}

#[test]
fn tagging_as_normalized_never_changes_the_fusion_outcome() {
    let plain = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "Saturn boot header",
    )];
    let tagged = tag_normalized_view_evidence(plain.clone());
    let plain_explanation = fuse_platform_evidence(plain);
    let tagged_explanation = fuse_platform_evidence(tagged);
    assert_eq!(plain_explanation.outcome, tagged_explanation.outcome);
    assert_eq!(
        plain_explanation.resolved_platform,
        tagged_explanation.resolved_platform
    );
}

#[test]
fn tagging_an_empty_bundle_yields_an_empty_bundle() {
    let tagged = tag_normalized_view_evidence(Vec::<ContentEvidence>::new());
    assert!(tagged.is_empty());
}

#[test]
fn tagged_evidence_survives_into_the_explanation_for_provenance_display() {
    let evidence = tag_normalized_view_evidence(vec![strong(
        ContentEvidenceKind::ContentSignature,
        "n64-header-valid",
        "byte-order-corrected N64 header",
    )]);
    let explanation = fuse_platform_evidence(evidence);
    assert!(
        explanation
            .input_evidence
            .iter()
            .any(|fact| fact.detail.starts_with("[normalized view] "))
    );
}

#[test]
fn mixed_normalized_and_physical_evidence_both_keep_their_own_provenance() {
    let normalized = tag_normalized_view_evidence(vec![strong(
        ContentEvidenceKind::ContentSignature,
        "n64-header-valid",
        "byte-order-corrected N64 header",
    )]);
    let physical = vec![strong(
        ContentEvidenceKind::BootStructure,
        "SEGA SEGASATURN",
        "raw Saturn boot header, no normalization involved",
    )];
    let mut combined = normalized;
    combined.extend(physical);
    let tagged_count = combined
        .iter()
        .filter(|fact| fact.detail.starts_with("[normalized view] "))
        .count();
    let untagged_count = combined.len() - tagged_count;
    assert_eq!(tagged_count, 1);
    assert_eq!(untagged_count, 1);
}

#[test]
fn tagging_multiple_facts_tags_every_one() {
    let evidence = vec![
        strong(
            ContentEvidenceKind::ContentSignature,
            "z64",
            "byte-order-corrected N64 header magic",
        ),
        strong(
            ContentEvidenceKind::ContentSignature,
            "n64-region",
            "byte-order-corrected N64 region field",
        ),
    ];
    let tagged = tag_normalized_view_evidence(evidence);
    assert_eq!(tagged.len(), 2);
    assert!(
        tagged
            .iter()
            .all(|fact| fact.detail.starts_with("[normalized view] "))
    );
}

#[test]
fn tagging_does_not_add_a_prefix_twice_when_reapplied_to_a_mixed_bundle() {
    let once = tag_normalized_view_evidence(vec![strong(
        ContentEvidenceKind::ContentSignature,
        "z64",
        "byte-order-corrected N64 header",
    )]);
    let mut mixed = once;
    mixed.push(strong(
        ContentEvidenceKind::BootStructure,
        "LYNX",
        "raw Lynx header, no normalization involved",
    ));
    let retagged = tag_normalized_view_evidence(mixed);
    assert_eq!(
        retagged[0].detail.matches("[normalized view]").count(),
        1,
        "already-tagged fact must not gain a second prefix"
    );
    assert!(
        retagged[1].detail.starts_with("[normalized view] "),
        "previously-untagged fact must gain exactly one prefix"
    );
}

#[test]
fn normalized_provenance_is_visible_on_a_resolved_outcome_not_just_unknown() {
    // The provenance tag must survive through to a real Resolved outcome,
    // not only be inspectable on bundles that never resolve.
    let evidence = tag_normalized_view_evidence(vec![strong(
        ContentEvidenceKind::ContentSignature,
        "z64",
        "byte-order-corrected N64 header magic",
    )]);
    let explanation = fuse_platform_evidence(evidence);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("N64"));
    assert!(
        explanation
            .input_evidence
            .iter()
            .any(|fact| fact.detail.starts_with("[normalized view] "))
    );
}
