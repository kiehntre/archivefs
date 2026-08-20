use super::*;
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};

fn exact_verdict(game: &str, rom: &str) -> AuditVerdict {
    AuditVerdict::Exact {
        game_name: game.to_string(),
        rom_name: rom.to_string(),
        algorithm: "SHA-1",
    }
}

fn identity_with_verdict(game: &str, rom: &str) -> IdentityResult {
    identity_with_verdict_and_platform(game, rom, "PSX")
}

fn identity_with_verdict_and_platform(game: &str, rom: &str, platform: &str) -> IdentityResult {
    use crate::dat::identity::{
        DatPlatformConfidence, DatPlatformEvidence, DatPlatformEvidenceKind,
        resolve_dat_platform_identity,
    };
    inspect_identity(IdentityInspectionInput {
        dat: Some(resolve_dat_platform_identity([DatPlatformEvidence {
            platform: platform.to_string(),
            machine_key: None,
            kind: DatPlatformEvidenceKind::HeaderName,
            confidence: DatPlatformConfidence::Strong,
            detail: "test evidence".to_string(),
        }])),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict(game, rom),
        }),
        ..Default::default()
    })
}

fn input(path: &str, identity: IdentityResult) -> LibraryPlanInput {
    LibraryPlanInput {
        source_path: PathBuf::from(path),
        identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }
}

// ------------------------------------------------------------------
// display_label / hierarchy_for (sections 20-21, 28)
// ------------------------------------------------------------------

#[test]
fn confident_dat_match_gives_the_exact_release_title() {
    let identity = identity_with_verdict("Tony Hawk's Pro Skater 2 (USA)", "thps2.z64");
    let hierarchy = hierarchy_for(&identity, Some("N64"), "thps2.z64");
    assert_eq!(hierarchy.game_label, "Tony Hawk's Pro Skater 2 (USA)");
    assert!(hierarchy.game_label_is_dat_confirmed);
}

#[test]
fn no_dat_match_falls_back_to_original_basename_not_invented() {
    let identity = inspect_identity(IdentityInspectionInput::default());
    let hierarchy = hierarchy_for(&identity, None, "some_unmatched_rom.bin");
    assert_eq!(hierarchy.game_label, "some_unmatched_rom.bin");
    assert!(!hierarchy.game_label_is_dat_confirmed);
}

#[test]
fn hierarchy_never_requires_platform_to_be_known() {
    let identity = inspect_identity(IdentityInspectionInput::default());
    let hierarchy = hierarchy_for(&identity, None, "mystery.bin");
    assert!(hierarchy.platform.is_none());
}

#[test]
fn single_disc_title_is_single_file_membership() {
    let identity = identity_with_verdict("Chrono Trigger (USA)", "chrono.sfc");
    let hierarchy = hierarchy_for(&identity, Some("SNES"), "chrono.sfc");
    assert_eq!(hierarchy.set, SetMembership::SingleFile);
}

#[test]
fn multidisc_title_reports_part_and_total() {
    let identity = identity_with_verdict("Final Fantasy VII (USA) (Disc 1 of 3)", "ff7d1.bin");
    let hierarchy = hierarchy_for(&identity, Some("PSX"), "ff7d1.bin");
    assert_eq!(
        hierarchy.set,
        SetMembership::MultiDiscPart {
            base_title: "Final Fantasy VII (USA)".to_string(),
            part: 1,
            total: 3,
        }
    );
}

#[test]
fn title_merely_containing_the_word_disc_is_never_multidisc() {
    let identity = identity_with_verdict("Disc Jockey Simulator (USA)", "dj.bin");
    let hierarchy = hierarchy_for(&identity, Some("PC"), "dj.bin");
    assert_eq!(hierarchy.set, SetMembership::SingleFile);
}

// ------------------------------------------------------------------
// group_multidisc_sets (section 11)
// ------------------------------------------------------------------

#[test]
fn three_disc_set_groups_all_members_in_part_order() {
    let inputs = vec![
        input(
            "/roms/psx/ff7_d2.bin",
            identity_with_verdict("Final Fantasy VII (USA) (Disc 2 of 3)", "ff7d2.bin"),
        ),
        input(
            "/roms/psx/ff7_d1.bin",
            identity_with_verdict("Final Fantasy VII (USA) (Disc 1 of 3)", "ff7d1.bin"),
        ),
        input(
            "/roms/psx/ff7_d3.bin",
            identity_with_verdict("Final Fantasy VII (USA) (Disc 3 of 3)", "ff7d3.bin"),
        ),
    ];
    let sets = group_multidisc_sets(&inputs);
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].base_title, "Final Fantasy VII (USA)");
    assert_eq!(sets[0].declared_total, 3);
    assert_eq!(
        sets[0]
            .discs
            .iter()
            .map(|(part, _)| *part)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn a_lone_disc_with_no_sibling_supplied_is_not_reported_as_a_set() {
    let inputs = vec![input(
        "/roms/psx/ff7_d1.bin",
        identity_with_verdict("Final Fantasy VII (USA) (Disc 1 of 3)", "ff7d1.bin"),
    )];
    assert!(group_multidisc_sets(&inputs).is_empty());
}

#[test]
fn single_disc_games_never_get_grouped_into_a_set() {
    let inputs = vec![
        input(
            "/roms/snes/a.sfc",
            identity_with_verdict("Game A (USA)", "a.sfc"),
        ),
        input(
            "/roms/snes/b.sfc",
            identity_with_verdict("Game B (USA)", "b.sfc"),
        ),
    ];
    assert!(group_multidisc_sets(&inputs).is_empty());
}

#[test]
fn different_platforms_with_the_same_base_title_never_merge_into_one_set() {
    // Same base title text is not itself evidence of the same set when the
    // resolved platform differs - kept apart by the `(platform, base_title)`
    // grouping key.
    let inputs = vec![
        input(
            "/roms/psx/d1.bin",
            identity_with_verdict_and_platform("Some Game (USA) (Disc 1 of 2)", "d1.bin", "PSX"),
        ),
        input(
            "/roms/ps2/d2.bin",
            identity_with_verdict_and_platform("Some Game (USA) (Disc 2 of 2)", "d2.bin", "PS2"),
        ),
    ];
    assert!(group_multidisc_sets(&inputs).is_empty());
}

#[test]
fn grouping_is_deterministic_regardless_of_input_order() {
    let make = || {
        vec![
            input(
                "/roms/psx/ff7_d2.bin",
                identity_with_verdict("Final Fantasy VII (USA) (Disc 2 of 3)", "ff7d2.bin"),
            ),
            input(
                "/roms/psx/ff7_d1.bin",
                identity_with_verdict("Final Fantasy VII (USA) (Disc 1 of 3)", "ff7d1.bin"),
            ),
            input(
                "/roms/psx/ff7_d3.bin",
                identity_with_verdict("Final Fantasy VII (USA) (Disc 3 of 3)", "ff7d3.bin"),
            ),
        ]
    };
    let forward = make();
    let mut backward = make();
    backward.reverse();
    assert_eq!(
        group_multidisc_sets(&forward),
        group_multidisc_sets(&backward)
    );
}

#[test]
fn two_members_declaring_the_same_disc_part_are_both_retained_not_silently_dropped() {
    // A genuine "multi-disc same-name collision" edge case: two distinct
    // real files both confidently DAT-matched to "Disc 1 of 2" under the
    // same base title (a mislabeled/duplicated real-world release, or a
    // region/language variant sharing the same DAT title). This module
    // must never silently keep only one - both stay visible so a human
    // reviewer sees the collision, rather than one disc quietly vanishing.
    let inputs = vec![
        input(
            "/roms/psx/variant_a.bin",
            identity_with_verdict("Ambiguous Game (USA) (Disc 1 of 2)", "a.bin"),
        ),
        input(
            "/roms/psx/variant_b.bin",
            identity_with_verdict("Ambiguous Game (USA) (Disc 1 of 2)", "b.bin"),
        ),
    ];
    let sets = group_multidisc_sets(&inputs);
    assert_eq!(sets.len(), 1);
    assert_eq!(
        sets[0].discs.len(),
        2,
        "both same-part members must be retained, not deduplicated away"
    );
    assert!(sets[0].discs.iter().all(|(part, _)| *part == 1));
}

#[test]
fn no_files_are_read_or_hashed_by_this_module() {
    let source = include_str!("../library_grouping.rs");
    for forbidden in ["std::fs::read", "std::fs::File::open"] {
        assert!(!source.contains(forbidden));
    }
}

#[test]
fn library_grouping_source_never_references_mutation_functions() {
    let source = include_str!("../library_grouping.rs");
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
