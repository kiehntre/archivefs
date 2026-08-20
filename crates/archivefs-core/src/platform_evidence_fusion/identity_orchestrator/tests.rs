use super::*;
use crate::content_evidence::{ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::identity::{
    DatPlatformEvidence, DatPlatformEvidenceKind, resolve_dat_platform_identity,
};

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn resolved_dat(platform: &str) -> DatPlatformIdentity {
    resolve_dat_platform_identity([DatPlatformEvidence {
        platform: platform.to_string(),
        machine_key: None,
        kind: DatPlatformEvidenceKind::HeaderName,
        confidence: crate::dat::identity::DatPlatformConfidence::Strong,
        detail: "test evidence".to_string(),
    }])
}

// ------------------------------------------------------------------
// Bare content, no DAT, no archive
// ------------------------------------------------------------------

#[test]
fn content_only_input_yields_a_content_only_result() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert_eq!(result.content.resolved_platform, Some("Saturn"));
    assert!(result.dat.is_none());
    assert!(result.combined.is_none());
    assert!(result.set_identity.is_none());
    assert!(!result.has_conflict());
}

#[test]
fn empty_input_is_unknown_with_no_caveats() {
    let result = inspect_identity(IdentityInspectionInput::default());
    assert_eq!(result.content.outcome, super::super::FusionOutcome::Unknown);
    assert!(result.caveats.is_empty());
    assert!(!result.has_conflict());
}

// ------------------------------------------------------------------
// Content + DAT
// ------------------------------------------------------------------

#[test]
fn content_and_dat_agreeing_produces_no_caveats() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert!(
        result
            .combined
            .as_ref()
            .unwrap()
            .relationship
            .is_agreement()
    );
    assert!(result.caveats.is_empty());
    assert!(!result.has_conflict());
}

#[test]
fn content_and_dat_disagreeing_flags_a_conflict_caveat() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Xbox")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert!(result.has_conflict());
    assert!(
        result
            .caveats
            .iter()
            .any(|c| c.contains("content and DAT-source identity disagree"))
    );
}

// ------------------------------------------------------------------
// Representation match
// ------------------------------------------------------------------

#[test]
fn representation_disagreement_flags_a_conflict_caveat() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        representation_match: Some(RepresentationMatchOutcome::Disagree {
            physical_verdict: crate::dat::audit::AuditVerdict::NotInDat,
            normalized_verdict: crate::dat::audit::AuditVerdict::NotInDat,
        }),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert!(result.has_conflict());
    assert!(
        result
            .caveats
            .iter()
            .any(|c| c.contains("physical and normalized"))
    );
}

// ------------------------------------------------------------------
// Archive set identity
// ------------------------------------------------------------------

#[test]
fn multi_platform_archive_flags_a_conflict_caveat() {
    let input = IdentityInspectionInput {
        archive_members: Some(vec![
            (
                0,
                vec![strong(
                    ContentEvidenceKind::BootStructure,
                    "SEGA SEGASATURN",
                )],
            ),
            (
                1,
                vec![
                    strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
                    strong(ContentEvidenceKind::ContentSignature, "XBEH"),
                ],
            ),
        ]),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert!(result.has_conflict());
    assert!(
        result
            .caveats
            .iter()
            .any(|c| c.contains("archive contains members"))
    );
}

#[test]
fn single_member_archive_is_not_a_conflict() {
    let input = IdentityInspectionInput {
        archive_members: Some(vec![(
            0,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
        )]),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert!(!result.has_conflict());
    match result.set_identity {
        Some(super::super::archive_set_identity::ArchiveSetIdentity::SingleMember {
            platform,
            ..
        }) => {
            assert_eq!(platform, "Saturn");
        }
        other => panic!("expected SingleMember, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// Ambiguous content caveat
// ------------------------------------------------------------------

#[test]
fn ambiguous_content_yields_its_own_caveat() {
    let input = IdentityInspectionInput {
        content_evidence: vec![
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
        ],
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert_eq!(
        result.content.outcome,
        super::super::FusionOutcome::Ambiguous
    );
    assert!(result.caveats.iter().any(|c| c.contains("ambiguous")));
    // Ambiguous alone is not a "conflict" - it is insufficient evidence,
    // not a contradiction.
    assert!(!result.has_conflict());
}

// ------------------------------------------------------------------
// Determinism
// ------------------------------------------------------------------

#[test]
fn inspect_identity_is_deterministic() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let first = inspect_identity(input.clone());
    let second = inspect_identity(input);
    assert_eq!(first, second);
}

// ------------------------------------------------------------------
// No action authority
// ------------------------------------------------------------------

#[test]
fn identity_result_carries_no_action_bearing_fields() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    };
    let result = inspect_identity(input);
    let _content: ResolutionExplanation = result.content;
    let _dat: Option<DatPlatformIdentity> = result.dat;
    let _combined = result.combined;
    let _representation_match = result.representation_match;
    let _set_identity = result.set_identity;
    let _caveats: Vec<&str> = result.caveats;
}

#[test]
fn identity_orchestrator_source_never_references_mutation_modules() {
    let source = include_str!("../identity_orchestrator.rs");
    for forbidden in [
        "crate::repair",
        "rename_plan",
        "rename_apply",
        "std::fs::remove",
        "std::fs::rename",
        "std::fs::write",
    ] {
        assert!(
            !source.contains(forbidden),
            "identity_orchestrator.rs unexpectedly references {forbidden:?}"
        );
    }
}
