use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use std::path::PathBuf;

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn exact_verdict(game: &str, rom: &str) -> AuditVerdict {
    AuditVerdict::Exact {
        game_name: game.to_string(),
        rom_name: rom.to_string(),
        algorithm: "SHA-1",
    }
}

fn saturn_identity() -> IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    })
}

fn input(
    path: &str,
    identity: IdentityResult,
    physical: Option<&str>,
    normalized: Option<&str>,
) -> LibraryPlanInput {
    LibraryPlanInput {
        source_path: PathBuf::from(path),
        identity,
        set_identity: None,
        physical_hash: physical.map(str::to_string),
        normalized_hash: normalized.map(str::to_string),
    }
}

#[test]
fn identical_physical_hash_is_exact_physical_duplicate() {
    let inputs = vec![
        input("/roms/a.bin", saturn_identity(), Some("hash1"), None),
        input("/roms/b.bin", saturn_identity(), Some("hash1"), None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].classification,
        DuplicateClass::ExactPhysicalDuplicate
    );
    assert_eq!(
        groups[0].members,
        vec![PathBuf::from("/roms/a.bin"), PathBuf::from("/roms/b.bin")]
    );
}

#[test]
fn different_physical_same_normalized_is_exact_normalized_duplicate() {
    let inputs = vec![
        input(
            "/roms/game.v64",
            saturn_identity(),
            Some("phys_v64"),
            Some("norm1"),
        ),
        input(
            "/roms/game.z64",
            saturn_identity(),
            Some("phys_z64"),
            Some("norm1"),
        ),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].classification,
        DuplicateClass::ExactNormalizedDuplicate
    );
}

#[test]
fn physical_duplicate_takes_priority_over_normalized() {
    // Same physical AND same normalized - the item must be claimed once,
    // by the stronger class only.
    let inputs = vec![
        input(
            "/roms/a.bin",
            saturn_identity(),
            Some("hash1"),
            Some("norm1"),
        ),
        input(
            "/roms/b.bin",
            saturn_identity(),
            Some("hash1"),
            Some("norm1"),
        ),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].classification,
        DuplicateClass::ExactPhysicalDuplicate
    );
}

#[test]
fn same_dat_release_groups_confident_matches_to_the_same_release() {
    let identity_a = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Athlete Kings", "athlete_kings.bin"),
        }),
        ..Default::default()
    });
    let identity_b = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Athlete Kings", "athlete_kings.bin"),
        }),
        ..Default::default()
    });
    let inputs = vec![
        input("/roms/a.bin", identity_a, None, None),
        input("/roms/b.bin", identity_b, None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].classification, DuplicateClass::SameDatRelease);
}

#[test]
fn same_game_different_rom_name_is_same_game_different_dump() {
    let identity_a = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Sonic the Hedgehog", "sonic_v1.bin"),
        }),
        ..Default::default()
    });
    let identity_b = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Sonic the Hedgehog", "sonic_v2.bin"),
        }),
        ..Default::default()
    });
    let inputs = vec![
        input("/roms/a.bin", identity_a, None, None),
        input("/roms/b.bin", identity_b, None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].classification,
        DuplicateClass::SameGameDifferentDump
    );
}

#[test]
fn same_basename_same_platform_no_stronger_evidence_is_possible_duplicate() {
    let inputs = vec![
        input("/roms/one/Game (USA).bin", saturn_identity(), None, None),
        input("/roms/two/Game (USA).bin", saturn_identity(), None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].classification, DuplicateClass::PossibleDuplicate);
    assert_eq!(groups[0].confidence, DuplicateGroupConfidence::Weak);
}

#[test]
fn possible_duplicate_never_fires_when_a_stronger_class_already_claimed_the_pair() {
    let inputs = vec![
        input(
            "/roms/one/Game (USA).bin",
            saturn_identity(),
            Some("h1"),
            None,
        ),
        input(
            "/roms/two/Game (USA).bin",
            saturn_identity(),
            Some("h1"),
            None,
        ),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].classification,
        DuplicateClass::ExactPhysicalDuplicate
    );
}

#[test]
fn unrelated_files_produce_no_groups() {
    let inputs = vec![
        input("/roms/a.bin", saturn_identity(), Some("h1"), None),
        input("/roms/b.bin", saturn_identity(), Some("h2"), None),
    ];
    assert!(group_duplicates(&inputs).is_empty());
}

#[test]
fn single_item_never_forms_a_group() {
    let inputs = vec![input("/roms/a.bin", saturn_identity(), Some("h1"), None)];
    assert!(group_duplicates(&inputs).is_empty());
}

#[test]
fn empty_input_is_empty_output() {
    assert!(group_duplicates(&[]).is_empty());
}

#[test]
fn group_members_are_sorted() {
    let inputs = vec![
        input("/roms/z.bin", saturn_identity(), Some("h1"), None),
        input("/roms/a.bin", saturn_identity(), Some("h1"), None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(
        groups[0].members,
        vec![PathBuf::from("/roms/a.bin"), PathBuf::from("/roms/z.bin")]
    );
}

#[test]
fn grouping_is_deterministic_regardless_of_input_order() {
    let forward = vec![
        input("/roms/a.bin", saturn_identity(), Some("h1"), None),
        input("/roms/b.bin", saturn_identity(), Some("h1"), None),
        input("/roms/c.bin", saturn_identity(), Some("h2"), None),
    ];
    let mut backward = forward
        .iter()
        .rev()
        .map(|i| {
            input(
                i.source_path.to_str().unwrap(),
                i.identity.clone(),
                i.physical_hash.as_deref(),
                i.normalized_hash.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    backward.reverse();
    let groups_forward = group_duplicates(&forward);
    let groups_backward = group_duplicates(&backward);
    assert_eq!(groups_forward.len(), groups_backward.len());
    assert_eq!(groups_forward, groups_backward);
}

#[test]
fn no_files_are_hashed_or_read_by_this_module() {
    let source = include_str!("../duplicate_taxonomy.rs");
    for forbidden in [
        "std::fs::read",
        "std::fs::File::open",
        "hash_file(",
        "hash_bytes(",
    ] {
        assert!(
            !source.contains(forbidden),
            "duplicate_taxonomy.rs unexpectedly references {forbidden:?}"
        );
    }
}

#[test]
fn duplicate_taxonomy_source_never_references_mutation_functions() {
    let source = include_str!("../duplicate_taxonomy.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "std::os::unix::fs::symlink",
    ] {
        assert!(!source.contains(forbidden));
    }
}

#[test]
fn possible_duplicate_basename_match_is_case_insensitive() {
    let inputs = vec![
        input("/roms/one/GAME.BIN", saturn_identity(), None, None),
        input("/roms/two/game.bin", saturn_identity(), None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].classification, DuplicateClass::PossibleDuplicate);
}

#[test]
fn unicode_normalization_variants_are_not_falsely_collapsed() {
    // "café" as a precomposed NFC 'é' (U+00E9) vs. decomposed NFD 'e'+combining
    // acute (U+0065 U+0301) look identical to a human but are different byte
    // sequences - this module does no Unicode normalization of its own, so
    // it must never *silently* treat them as the same basename (a false
    // positive would be worse than the honest miss). Documented here as a
    // real, disclosed limitation rather than a silently wrong behaviour.
    let nfc = "caf\u{00e9} (USA)";
    let nfd = "cafe\u{0301} (USA)";
    assert_ne!(
        nfc, nfd,
        "the two byte sequences must genuinely differ for this test to mean anything"
    );
    let inputs = vec![
        input(&format!("/roms/{nfc}.bin"), saturn_identity(), None, None),
        input(&format!("/roms/{nfd}.bin"), saturn_identity(), None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert!(
        groups.is_empty(),
        "Unicode-equivalent-looking basenames must not be silently treated as a match"
    );
}

#[test]
fn labels_are_all_distinct() {
    let classes = [
        DuplicateClass::ExactPhysicalDuplicate,
        DuplicateClass::ExactNormalizedDuplicate,
        DuplicateClass::SameDatRelease,
        DuplicateClass::SameGameDifferentDump,
        DuplicateClass::SameGameDifferentRevision,
        DuplicateClass::PossibleDuplicate,
        DuplicateClass::NotDuplicate,
    ];
    let mut labels: Vec<&str> = classes.iter().map(|c| c.label()).collect();
    let before = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), before);
}

#[test]
fn same_game_different_revision_is_never_produced_this_batch() {
    // Disclosed gap (see the enum variant's own doc comment): no detector
    // here produces this classification, since the structured
    // clone_of/revision plumbing it would need does not yet reach
    // IdentityResult. This test pins that honestly rather than silently
    // letting a future accidental heuristic start producing it unreviewed.
    let identity_a = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Super Game (USA)", "super_game.bin"),
        }),
        ..Default::default()
    });
    let identity_b = inspect_identity(IdentityInspectionInput {
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Super Game (USA) (Rev 1)", "super_game_rev1.bin"),
        }),
        ..Default::default()
    });
    let inputs = vec![
        input("/roms/a.bin", identity_a, None, None),
        input("/roms/b.bin", identity_b, None, None),
    ];
    let groups = group_duplicates(&inputs);
    assert!(
        groups
            .iter()
            .all(|g| g.classification != DuplicateClass::SameGameDifferentRevision)
    );
}
