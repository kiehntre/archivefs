use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::identity::{
    DatPlatformConfidence, DatPlatformEvidence, DatPlatformEvidenceKind,
    resolve_dat_platform_identity,
};
use crate::dat::rom_organisation::OrganisationMode;
use crate::platform_evidence_fusion::dat_hash_representation::RepresentationMatchOutcome;
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, plan_library,
};

fn exact_verdict(game: &str, rom: &str) -> crate::dat::audit::AuditVerdict {
    crate::dat::audit::AuditVerdict::Exact {
        game_name: game.to_string(),
        rom_name: rom.to_string(),
        algorithm: "SHA-1",
    }
}

fn psx_identity(
    game: &str,
    rom: &str,
) -> crate::platform_evidence_fusion::identity_orchestrator::IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PS1",
            ContentEvidenceConfidence::Strong,
            "test fact",
        )],
        dat: Some(resolve_dat_platform_identity([DatPlatformEvidence {
            platform: "PSX".to_string(),
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

fn write_temp(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"dummy content").unwrap();
    path
}

#[test]
fn two_disc_set_gets_a_shared_nested_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let d1 = write_temp(dir.path(), "d1.bin");
    let d2 = write_temp(dir.path(), "d2.bin");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![
        LibraryPlanInput {
            source_path: d1.clone(),
            identity: psx_identity("Some Game (USA) (Disc 1 of 2)", "d1.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: d2.clone(),
            identity: psx_identity("Some Game (USA) (Disc 2 of 2)", "d2.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let report = plan_library(&inputs, &context);
    let multidisc_sets =
        crate::platform_evidence_fusion::library_grouping::group_multidisc_sets(&inputs);
    assert_eq!(multidisc_sets.len(), 1);

    let plan = plan_set_destinations(&report, &multidisc_sets, &[]);
    assert_eq!(plan.sets.len(), 1);
    let set = &plan.sets[0];
    assert_eq!(set.set_label, "Some Game (USA)");
    assert!(set.set_folder.starts_with(&root));
    assert!(set.set_folder.ends_with("Some Game (USA)"));
    assert_eq!(set.member_destinations.len(), 2);
    for (_, destination) in &set.member_destinations {
        assert!(destination.starts_with(&set.set_folder));
    }
}

#[test]
fn single_file_is_never_forced_into_a_nested_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: psx_identity("Solo Game (USA)", "game.bin"),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let report = plan_library(&inputs, &context);
    // Confirm the flat, un-nested shape from build_organisation_plan is
    // untouched by this module for a lone file.
    let flat_destination = report.items[0].organisation.destination_path.clone();
    assert_eq!(flat_destination, root.join("ps").join("game.bin"));

    let plan = plan_set_destinations(&report, &[], &[]);
    assert!(plan.sets.is_empty());
}

#[test]
fn a_blocked_member_prevents_the_whole_set_from_being_planned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let d1 = write_temp(dir.path(), "d1.bin");
    // d2 deliberately never written to disk - its OrganisationPlanEntry
    // will be Blocked ("the source file does not exist").
    let d2 = dir.path().join("d2.bin");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![
        LibraryPlanInput {
            source_path: d1,
            identity: psx_identity("Some Game (USA) (Disc 1 of 2)", "d1.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: d2,
            identity: psx_identity("Some Game (USA) (Disc 2 of 2)", "d2.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let report = plan_library(&inputs, &context);
    let multidisc_sets =
        crate::platform_evidence_fusion::library_grouping::group_multidisc_sets(&inputs);
    let plan = plan_set_destinations(&report, &multidisc_sets, &[]);
    assert!(
        plan.sets.is_empty(),
        "a set with any non-Ready member must not be planned"
    );
}

#[test]
fn attached_support_file_lands_inside_the_matching_set_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let d1 = write_temp(dir.path(), "d1.bin");
    let d2 = write_temp(dir.path(), "d2.bin");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![
        LibraryPlanInput {
            source_path: d1,
            identity: psx_identity("Some Game (USA) (Disc 1 of 2)", "d1.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: d2,
            identity: psx_identity("Some Game (USA) (Disc 2 of 2)", "d2.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let report = plan_library(&inputs, &context);
    let multidisc_sets =
        crate::platform_evidence_fusion::library_grouping::group_multidisc_sets(&inputs);

    let m3u_path = dir.path().join("Some Game (USA).m3u");
    let candidates = vec![SupportCandidate {
        path: &m3u_path,
        role: SideFileRole::Playlist,
        association: SupportAssociation::Attached {
            set_label: "Some Game (USA)".to_string(),
        },
        referenced_members: Vec::new(),
    }];
    let plan = plan_set_destinations(&report, &multidisc_sets, &candidates);
    assert_eq!(plan.support_items.len(), 1);
    let support = &plan.support_items[0];
    assert_eq!(support.status, PlanStatus::Ready);
    let destination = support.proposed_destination.as_ref().unwrap();
    assert!(destination.starts_with(&plan.sets[0].set_folder));
    assert_eq!(destination.file_name().unwrap(), "Some Game (USA).m3u");
}

#[test]
fn candidate_support_is_needs_review_never_silently_attached() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let manual_path = dir.path().join("manual.pdf");
    let candidates = vec![SupportCandidate {
        path: &manual_path,
        role: SideFileRole::Manual,
        association: SupportAssociation::Candidate {
            reason: "ambiguous".to_string(),
        },
        referenced_members: Vec::new(),
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &crate::platform_evidence_fusion::library_planning::no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(&[], &context);
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert_eq!(plan.support_items[0].status, PlanStatus::NeedsReview);
    assert!(plan.support_items[0].proposed_destination.is_none());
}

#[test]
fn unassociated_support_never_gets_a_destination() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let readme_path = dir.path().join("readme.txt");
    let candidates = vec![SupportCandidate {
        path: &readme_path,
        role: SideFileRole::Readme,
        association: SupportAssociation::Unassociated,
        referenced_members: Vec::new(),
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &crate::platform_evidence_fusion::library_planning::no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(&[], &context);
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert_eq!(plan.support_items[0].status, PlanStatus::Unsupported);
    assert!(plan.support_items[0].proposed_destination.is_none());
}

#[test]
fn unsafe_reference_support_is_needs_review_never_attached() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let cue_path = dir.path().join("game.cue");
    let candidates = vec![SupportCandidate {
        path: &cue_path,
        role: SideFileRole::CueSheet,
        association: SupportAssociation::UnsafeReference {
            detail: "path traversal".to_string(),
        },
        referenced_members: Vec::new(),
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &crate::platform_evidence_fusion::library_planning::no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(&[], &context);
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert_eq!(plan.support_items[0].status, PlanStatus::NeedsReview);
    assert!(plan.support_items[0].proposed_destination.is_none());
}

#[test]
fn ad_hoc_cue_bin_set_gets_its_own_folder_from_the_matching_primary_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let bin = write_temp(dir.path(), "7 Sins (Europe).bin");
    let cue_path = dir.path().join("7 Sins (Europe).cue");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![LibraryPlanInput {
        source_path: bin.clone(),
        identity: psx_identity("7 Sins (Europe)", "7 Sins (Europe).bin"),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let report = plan_library(&inputs, &context);
    let candidates = vec![SupportCandidate {
        path: &cue_path,
        role: SideFileRole::CueSheet,
        association: SupportAssociation::Attached {
            set_label: "7 Sins (Europe)".to_string(),
        },
        referenced_members: vec![bin],
    }];
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert_eq!(plan.sets.len(), 1);
    assert_eq!(plan.support_items[0].status, PlanStatus::Ready);
    let destination = plan.support_items[0].proposed_destination.as_ref().unwrap();
    assert_eq!(destination.file_name().unwrap(), "7 Sins (Europe).cue");
}

#[test]
fn no_matching_primary_item_means_no_ad_hoc_set_is_invented() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let cue_path = dir.path().join("orphan.cue");
    // A referenced member that names a real path, but one that was never
    // supplied as a LibraryPlanInput at all - so no matching report item
    // exists for it.
    let phantom_member = dir.path().join("phantom.bin");
    let candidates = vec![SupportCandidate {
        path: &cue_path,
        role: SideFileRole::CueSheet,
        association: SupportAssociation::Attached {
            set_label: "Nonexistent Game".to_string(),
        },
        referenced_members: vec![phantom_member],
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &crate::platform_evidence_fusion::library_planning::no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(&[], &context);
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert!(plan.sets.is_empty());
    assert_eq!(plan.support_items[0].status, PlanStatus::NeedsReview);
}

#[test]
fn destinations_never_escape_the_configured_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let d1 = write_temp(dir.path(), "d1.bin");
    let d2 = write_temp(dir.path(), "d2.bin");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![
        LibraryPlanInput {
            source_path: d1,
            identity: psx_identity("../../etc (USA) (Disc 1 of 2)", "d1.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: d2,
            identity: psx_identity("../../etc (USA) (Disc 2 of 2)", "d2.bin"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let report = plan_library(&inputs, &context);
    let multidisc_sets =
        crate::platform_evidence_fusion::library_grouping::group_multidisc_sets(&inputs);
    let plan = plan_set_destinations(&report, &multidisc_sets, &[]);
    // "../../etc (USA)" is rejected by is_safe_basename (contains no '/'
    // actually - re-check: the base_title itself has no slash, but let's
    // assert the invariant directly regardless of whether this particular
    // set was planned.
    for set in &plan.sets {
        assert!(set.set_folder.starts_with(&root));
        for (_, destination) in &set.member_destinations {
            assert!(destination.starts_with(&root));
        }
    }
}

#[test]
fn set_destination_source_never_references_mutation_functions() {
    let source = include_str!("../set_destination.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "std::os::unix::fs::symlink",
        "std::fs::write",
    ] {
        assert!(!source.contains(forbidden));
    }
}

#[test]
fn two_unrelated_single_disc_games_in_the_same_directory_never_merge() {
    // Regression: an earlier version of infer_ad_hoc_set anchored on
    // "any Ready primary item in the same directory as the support file",
    // which silently merged two completely unrelated single-disc games
    // that merely happened to share a directory. It must anchor only on
    // the support file's own actual resolved references.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let game_a = write_temp(dir.path(), "Game A (USA).chd");
    let game_b = write_temp(dir.path(), "Game B (USA).chd");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![
        LibraryPlanInput {
            source_path: game_a.clone(),
            identity: psx_identity("Game A (USA)", "Game A (USA).chd"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: game_b.clone(),
            identity: psx_identity("Game B (USA)", "Game B (USA).chd"),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let report = plan_library(&inputs, &context);

    let m3u_a = dir.path().join("Game A (USA).m3u");
    let m3u_b = dir.path().join("Game B (USA).m3u");
    let candidates = vec![
        SupportCandidate {
            path: &m3u_a,
            role: SideFileRole::Playlist,
            association: SupportAssociation::Attached {
                set_label: "Game A (USA)".to_string(),
            },
            referenced_members: vec![game_a.clone()],
        },
        SupportCandidate {
            path: &m3u_b,
            role: SideFileRole::Playlist,
            association: SupportAssociation::Attached {
                set_label: "Game B (USA)".to_string(),
            },
            referenced_members: vec![game_b.clone()],
        },
    ];
    let plan = plan_set_destinations(&report, &[], &candidates);
    assert_eq!(plan.sets.len(), 2);
    let set_a = plan
        .sets
        .iter()
        .find(|s| s.set_label == "Game A (USA)")
        .unwrap();
    let set_b = plan
        .sets
        .iter()
        .find(|s| s.set_label == "Game B (USA)")
        .unwrap();
    assert_eq!(
        set_a.member_destinations,
        vec![(game_a, set_a.set_folder.join("Game A (USA).chd"))]
    );
    assert_eq!(
        set_b.member_destinations,
        vec![(game_b, set_b.set_folder.join("Game B (USA).chd"))]
    );
    assert_ne!(set_a.set_folder, set_b.set_folder);
}

#[test]
fn plan_set_destinations_is_deterministic_regardless_of_support_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let bin = write_temp(dir.path(), "game.bin");
    let manual_path = dir.path().join("manual.pdf");
    let cue_path = dir.path().join("game.cue");
    let slug = |p: &str| (p == "PSX").then(|| "ps".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let inputs = vec![LibraryPlanInput {
        source_path: bin,
        identity: psx_identity("Game (USA)", "game.bin"),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let report = plan_library(&inputs, &context);
    let forward = vec![
        SupportCandidate {
            path: &manual_path,
            role: SideFileRole::Manual,
            association: SupportAssociation::Attached {
                set_label: "Game (USA)".to_string(),
            },
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &cue_path,
            role: SideFileRole::CueSheet,
            association: SupportAssociation::Attached {
                set_label: "Game (USA)".to_string(),
            },
            referenced_members: Vec::new(),
        },
    ];
    let backward = vec![
        SupportCandidate {
            path: &cue_path,
            role: SideFileRole::CueSheet,
            association: SupportAssociation::Attached {
                set_label: "Game (USA)".to_string(),
            },
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &manual_path,
            role: SideFileRole::Manual,
            association: SupportAssociation::Attached {
                set_label: "Game (USA)".to_string(),
            },
            referenced_members: Vec::new(),
        },
    ];
    let plan_forward = plan_set_destinations(&report, &[], &forward);
    let plan_backward = plan_set_destinations(&report, &[], &backward);
    assert_eq!(plan_forward.sets, plan_backward.sets);
}
