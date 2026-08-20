use super::*;
use crate::content_evidence::{ContentEvidenceConfidence, ContentEvidenceKind};

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn weak(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Weak, "test fact")
}

// ------------------------------------------------------------------
// Section 18A: single game-like member
// ------------------------------------------------------------------

#[test]
fn game_plus_junk_is_single_member() {
    let members = vec![
        (
            0,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
        ),
        (1, vec![]), // README/cover - never produces evidence
    ];
    let identity = classify_archive_set(&members);
    assert_eq!(
        identity,
        ArchiveSetIdentity::SingleMember {
            member_index: 0,
            platform: "Saturn"
        }
    );
    assert!(!identity.is_multi_member());
}

#[test]
fn no_useful_members_is_unknown() {
    let members = vec![
        (0, vec![]),
        (1, vec![weak(ContentEvidenceKind::Filesystem, "ISO9660")]),
    ];
    let identity = classify_archive_set(&members);
    assert_eq!(identity, ArchiveSetIdentity::Unknown);
}

#[test]
fn empty_archive_is_unknown() {
    assert_eq!(classify_archive_set(&[]), ArchiveSetIdentity::Unknown);
}

// ------------------------------------------------------------------
// Section 18B: multiple members, same platform
// ------------------------------------------------------------------

#[test]
fn two_snes_members_are_multi_member_same_platform() {
    let members = vec![
        (
            0,
            vec![strong(ContentEvidenceKind::ContentSignature, "LoROM")],
        ),
        (
            1,
            vec![strong(ContentEvidenceKind::ContentSignature, "HiROM")],
        ),
    ];
    let identity = classify_archive_set(&members);
    match &identity {
        ArchiveSetIdentity::MultiMemberSamePlatform {
            member_indices,
            platform,
        } => {
            assert_eq!(*platform, "SNES");
            assert_eq!(member_indices, &vec![0, 1]);
        }
        other => panic!("expected MultiMemberSamePlatform, got {other:?}"),
    }
    assert!(identity.is_multi_member());
    assert!(!identity.is_conflict());
}

#[test]
fn equivalent_platforms_fold_into_same_platform_not_multi_platform() {
    // Synthetic Resolved explanations aren't directly constructible via
    // fuse_platform_evidence for PC Engine (no detector exists), so this
    // exercises the same equivalence-folding path with Saturn against
    // itself via two members that both independently resolve Saturn.
    let members = vec![
        (
            0,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
        ),
        (
            1,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "SEGA SEGASATURN",
            )],
        ),
    ];
    let identity = classify_archive_set(&members);
    assert!(matches!(
        identity,
        ArchiveSetIdentity::MultiMemberSamePlatform { .. }
    ));
}

// ------------------------------------------------------------------
// Section 18C: multiple platforms - conflict, never a winner
// ------------------------------------------------------------------

#[test]
fn two_different_platforms_is_multi_platform_conflict() {
    let members = vec![
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
    ];
    let identity = classify_archive_set(&members);
    assert!(identity.is_conflict());
    match identity {
        ArchiveSetIdentity::MultiPlatform {
            member_indices,
            platforms,
        } => {
            assert_eq!(member_indices, vec![0, 1]);
            assert_eq!(platforms, vec!["Saturn", "Xbox"]);
        }
        other => panic!("expected MultiPlatform, got {other:?}"),
    }
}

#[test]
fn multi_platform_never_silently_picks_a_winner() {
    let members = vec![
        (
            0,
            vec![strong(ContentEvidenceKind::ContentSignature, "iNES")],
        ),
        (
            1,
            vec![strong(
                ContentEvidenceKind::BootStructure,
                "GBA cartridge header",
            )],
        ),
    ];
    let identity = classify_archive_set(&members);
    assert!(identity.is_conflict());
}

#[test]
fn three_platforms_all_named_not_just_the_first_two() {
    let members = vec![
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
        (
            2,
            vec![strong(ContentEvidenceKind::ContentSignature, "iNES")],
        ),
    ];
    let identity = classify_archive_set(&members);
    match identity {
        ArchiveSetIdentity::MultiPlatform { platforms, .. } => {
            assert_eq!(platforms.len(), 3);
        }
        other => panic!("expected MultiPlatform, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// Section 18E: no strong members
// ------------------------------------------------------------------

#[test]
fn weak_only_members_never_resolve_the_set() {
    let members = vec![
        (0, vec![weak(ContentEvidenceKind::ContentSignature, "ELF")]),
        (1, vec![weak(ContentEvidenceKind::Filesystem, "ISO9660")]),
    ];
    assert_eq!(classify_archive_set(&members), ArchiveSetIdentity::Unknown);
}

// ------------------------------------------------------------------
// StructuredSet - never fabricated
// ------------------------------------------------------------------

#[test]
fn structured_set_is_never_produced_without_a_real_detector() {
    // No combination of synthetic evidence this crate can produce today
    // should ever yield StructuredSet - confirmed by construction: nothing
    // in classify_archive_set ever constructs that variant.
    let members = vec![
        (
            0,
            vec![strong(ContentEvidenceKind::ContentSignature, "LoROM")],
        ),
        (
            1,
            vec![strong(ContentEvidenceKind::ContentSignature, "HiROM")],
        ),
        (
            2,
            vec![strong(ContentEvidenceKind::ContentSignature, "ExHiROM")],
        ),
    ];
    let identity = classify_archive_set(&members);
    assert!(!matches!(
        identity,
        ArchiveSetIdentity::StructuredSet { .. }
    ));
}

// ------------------------------------------------------------------
// participating_members
// ------------------------------------------------------------------

#[test]
fn participating_members_is_empty_for_unknown() {
    assert!(participating_members(&ArchiveSetIdentity::Unknown).is_empty());
}

#[test]
fn participating_members_matches_single_member_index() {
    let identity = ArchiveSetIdentity::SingleMember {
        member_index: 5,
        platform: "Saturn",
    };
    assert_eq!(participating_members(&identity), BTreeSet::from([5]));
}

#[test]
fn participating_members_matches_multi_platform_indices() {
    let identity = ArchiveSetIdentity::MultiPlatform {
        member_indices: vec![1, 3, 7],
        platforms: vec!["Saturn", "Xbox"],
    };
    assert_eq!(participating_members(&identity), BTreeSet::from([1, 3, 7]));
}

// ------------------------------------------------------------------
// Determinism
// ------------------------------------------------------------------

#[test]
fn classify_archive_set_is_deterministic() {
    let members = vec![
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
    ];
    assert_eq!(
        classify_archive_set(&members),
        classify_archive_set(&members)
    );
}

#[test]
fn member_order_never_affects_the_classification() {
    let forward = vec![
        (
            0,
            vec![strong(ContentEvidenceKind::ContentSignature, "LoROM")],
        ),
        (
            1,
            vec![strong(ContentEvidenceKind::ContentSignature, "HiROM")],
        ),
    ];
    let backward = vec![
        (
            1,
            vec![strong(ContentEvidenceKind::ContentSignature, "HiROM")],
        ),
        (
            0,
            vec![strong(ContentEvidenceKind::ContentSignature, "LoROM")],
        ),
    ];
    let forward_result = classify_archive_set(&forward);
    let backward_result = classify_archive_set(&backward);
    match (forward_result, backward_result) {
        (
            ArchiveSetIdentity::MultiMemberSamePlatform { platform: p1, .. },
            ArchiveSetIdentity::MultiMemberSamePlatform { platform: p2, .. },
        ) => assert_eq!(p1, p2),
        other => panic!("expected matching MultiMemberSamePlatform, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// No action authority
// ------------------------------------------------------------------

#[test]
fn archive_set_identity_source_never_references_mutation_modules() {
    let source = include_str!("../archive_set_identity.rs");
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
            "archive_set_identity.rs unexpectedly references {forbidden:?}"
        );
    }
}
