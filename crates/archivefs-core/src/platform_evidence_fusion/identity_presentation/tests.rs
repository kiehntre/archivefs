use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::audit::AuditVerdict;
use crate::dat::identity::{
    DatPlatformEvidence, DatPlatformEvidenceKind, resolve_dat_platform_identity,
};
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
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

fn exact_verdict(game: &str) -> AuditVerdict {
    AuditVerdict::Exact {
        game_name: game.to_string(),
        rom_name: format!("{game}.bin"),
        algorithm: "SHA-1",
    }
}

// ------------------------------------------------------------------
// Status derivation (section 14)
// ------------------------------------------------------------------

#[test]
fn unknown_input_yields_unknown_status() {
    let result = inspect_identity(IdentityInspectionInput::default());
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::Unknown);
    assert!(presentation.platform.is_none());
}

#[test]
fn content_only_yields_content_only_status() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::ContentOnly);
    assert_eq!(presentation.platform, Some("Saturn"));
}

#[test]
fn dat_resolved_no_hash_verdict_is_dat_only() {
    let input = IdentityInspectionInput {
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::DatOnly);
}

#[test]
fn confident_hash_verdict_alone_is_verified_by_dat() {
    let input = IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Some Game"),
        }),
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::VerifiedByDat);
}

#[test]
fn strong_content_never_labeled_verified() {
    // The exact distinction section 14 forbids: Strong content inference
    // alone must never be called "Verified."
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_ne!(presentation.status, IdentityStatus::VerifiedByDat);
    assert_ne!(presentation.status.label(), "Verified by DAT");
}

#[test]
fn content_and_dat_agreeing_is_content_and_dat_agree_status() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::ContentAndDatAgree);
}

#[test]
fn content_dat_disagreement_is_conflict_status() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Xbox")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::Conflict);
}

#[test]
fn ambiguous_content_is_ambiguous_status() {
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
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::Ambiguous);
}

#[test]
fn representation_disagreement_is_conflict_status() {
    let input = IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::Disagree {
            physical_verdict: exact_verdict("Game A"),
            normalized_verdict: exact_verdict("Game B"),
        }),
        ..Default::default()
    };
    let result = inspect_identity(input);
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::Conflict);
}

#[test]
fn multi_platform_archive_is_conflict_status() {
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
    let presentation = present_identity(&result);
    assert_eq!(presentation.status, IdentityStatus::Conflict);
}

// ------------------------------------------------------------------
// Summaries
// ------------------------------------------------------------------

#[test]
fn representation_summary_distinguishes_normalized_from_physical() {
    let physical = present_identity(&inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Game"),
        }),
        ..Default::default()
    }));
    let normalized = present_identity(&inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::NormalizedOnly {
            verdict: exact_verdict("Game"),
        }),
        ..Default::default()
    }));
    assert!(physical.representation_summary.contains("physical"));
    assert!(normalized.representation_summary.contains("normalization"));
    assert_ne!(
        physical.representation_summary,
        normalized.representation_summary
    );
}

#[test]
fn both_agree_identical_bytes_summary_differs_from_non_identical() {
    let identical = present_identity(&inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::BothAgree {
            verdict: exact_verdict("Game"),
            identical_bytes: true,
        }),
        ..Default::default()
    }));
    let non_identical = present_identity(&inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::BothAgree {
            verdict: exact_verdict("Game"),
            identical_bytes: false,
        }),
        ..Default::default()
    }));
    assert_ne!(
        identical.representation_summary,
        non_identical.representation_summary
    );
}

#[test]
fn set_summary_distinguishes_single_from_multi_member() {
    let single = present_identity(&inspect_identity(IdentityInspectionInput {
        archive_members: Some(vec![(
            0,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
        )]),
        ..Default::default()
    }));
    let multi = present_identity(&inspect_identity(IdentityInspectionInput {
        archive_members: Some(vec![
            (
                0,
                vec![strong(ContentEvidenceKind::ContentSignature, "LoROM")],
            ),
            (
                1,
                vec![strong(ContentEvidenceKind::ContentSignature, "HiROM")],
            ),
        ]),
        ..Default::default()
    }));
    assert!(single.set_summary.contains("Single member"));
    assert!(multi.set_summary.contains("Multi-member"));
}

#[test]
fn no_archive_input_yields_not_an_archive_set_summary() {
    let presentation = present_identity(&inspect_identity(IdentityInspectionInput::default()));
    assert_eq!(presentation.set_summary, "Not an archive");
}

// ------------------------------------------------------------------
// Conflict rows
// ------------------------------------------------------------------

#[test]
fn conflict_rows_are_populated_for_content_dat_disagreement() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Xbox")),
        ..Default::default()
    };
    let presentation = present_identity(&inspect_identity(input));
    assert!(!presentation.conflict_rows.is_empty());
    assert!(presentation.conflict_rows[0].contains("Saturn"));
    assert!(presentation.conflict_rows[0].contains("Xbox"));
}

#[test]
fn no_conflict_rows_when_nothing_conflicts() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    };
    let presentation = present_identity(&inspect_identity(input));
    assert!(presentation.conflict_rows.is_empty());
}

// ------------------------------------------------------------------
// render_identity_text
// ------------------------------------------------------------------

#[test]
fn render_text_includes_the_platform_and_status() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let presentation = present_identity(&inspect_identity(input));
    let text = render_identity_text(&presentation);
    assert!(text.contains("Saturn"));
    assert!(text.contains("Content and DAT agree"));
    assert!(text.contains("Source modified: No"));
}

#[test]
fn render_text_conflict_never_declares_a_winner() {
    let input = IdentityInspectionInput {
        content_evidence: vec![
            strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
            strong(ContentEvidenceKind::ContentSignature, "XBEH"),
        ],
        dat: Some(resolved_dat("Xbox360")),
        ..Default::default()
    };
    let presentation = present_identity(&inspect_identity(input));
    let text = render_identity_text(&presentation);
    assert!(text.contains("Conflict"));
    assert!(text.contains("did not choose a winner"));
}

#[test]
fn render_text_always_reports_source_modified_no() {
    let presentation = present_identity(&inspect_identity(IdentityInspectionInput::default()));
    let text = render_identity_text(&presentation);
    assert!(text.contains("Source modified: No"));
}

// ------------------------------------------------------------------
// Determinism
// ------------------------------------------------------------------

#[test]
fn present_identity_is_deterministic() {
    let input = IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let result = inspect_identity(input);
    assert_eq!(present_identity(&result), present_identity(&result));
}

#[test]
fn status_priority_is_deterministic() {
    // Content evidence order must never change the derived status.
    let forward = IdentityInspectionInput {
        content_evidence: vec![
            strong(ContentEvidenceKind::BootStructure, "SEGA SEGASATURN"),
            strong(ContentEvidenceKind::Filesystem, "ISO9660"),
        ],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let backward = IdentityInspectionInput {
        content_evidence: vec![
            strong(ContentEvidenceKind::Filesystem, "ISO9660"),
            strong(ContentEvidenceKind::BootStructure, "SEGA SEGASATURN"),
        ],
        dat: Some(resolved_dat("Saturn")),
        ..Default::default()
    };
    let forward_status = present_identity(&inspect_identity(forward)).status;
    let backward_status = present_identity(&inspect_identity(backward)).status;
    assert_eq!(forward_status, backward_status);
}

// ------------------------------------------------------------------
// No action authority
// ------------------------------------------------------------------

#[test]
fn source_modified_is_always_false() {
    for input in [
        IdentityInspectionInput::default(),
        IdentityInspectionInput {
            content_evidence: vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
            ..Default::default()
        },
    ] {
        let presentation = present_identity(&inspect_identity(input));
        assert!(!presentation.source_modified);
    }
}

#[test]
fn identity_presentation_source_never_references_mutation_modules() {
    let source = include_str!("../identity_presentation.rs");
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
            "identity_presentation.rs unexpectedly references {forbidden:?}"
        );
    }
}
