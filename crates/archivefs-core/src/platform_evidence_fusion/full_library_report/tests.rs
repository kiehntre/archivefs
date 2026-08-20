use super::*;
use crate::dat::rom_organisation::OrganisationMode;
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::library_planning::no_slug_mapping;
use std::path::PathBuf;

fn write_temp(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"dummy content").unwrap();
    path
}

fn saturn_identity() -> crate::platform_evidence_fusion::identity_orchestrator::IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![crate::content_evidence::ContentEvidence::new(
            crate::content_evidence::ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
            crate::content_evidence::ContentEvidenceConfidence::Strong,
            "test fact",
        )],
        ..Default::default()
    })
}

#[test]
fn full_report_counts_primary_and_support_files_separately() {
    // Batch 13: primary/support are now the caller's own two disjoint
    // lists (`inputs` vs `support_candidates`), not auto-detected by
    // extension inside this function - a cover.jpg the caller wants
    // classified as support is now passed via `support_candidates`.
    use crate::platform_evidence_fusion::set_destination::SupportCandidate;
    use crate::platform_evidence_fusion::side_file_classification::SideFileRole;
    use crate::platform_evidence_fusion::support_attachment::SupportAssociation;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");
    let cover_path = dir.path().join("cover.jpg");

    let inputs = vec![LibraryPlanInput {
        source_path: rom,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let support_candidates = vec![SupportCandidate {
        path: &cover_path,
        role: SideFileRole::Artwork,
        association: SupportAssociation::Unassociated,
        referenced_members: Vec::new(),
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &support_candidates);
    assert_eq!(report.counts.primary_items, 1);
    assert_eq!(report.counts.support_files, 1);
}

#[test]
fn full_report_status_counts_match_the_underlying_plan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");

    let inputs = vec![LibraryPlanInput {
        source_path: rom,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &[]);
    assert_eq!(report.counts.ready, report.plan.ready);
    assert_eq!(report.counts.romm_unmapped, report.plan.romm_unmapped);
}

#[test]
fn full_report_reflects_duplicate_groups() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let a = write_temp(dir.path(), "a.bin");
    let b = write_temp(dir.path(), "b.bin");

    let inputs = vec![
        LibraryPlanInput {
            source_path: a,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: Some("hash1".to_string()),
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: b,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: Some("hash1".to_string()),
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &[]);
    assert_eq!(report.counts.exact_physical_duplicates, 1);
    assert_eq!(report.duplicate_groups.len(), 1);
}

#[test]
fn full_report_serializes_to_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");
    let inputs = vec![LibraryPlanInput {
        source_path: rom,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &[]);
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("primary_items"));
}

#[test]
fn empty_input_yields_a_well_formed_empty_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&[], &context, &[]);
    assert_eq!(report.counts.primary_items, 0);
    assert!(report.duplicate_groups.is_empty());
    assert!(report.multidisc_sets.is_empty());
}

// ------------------------------------------------------------------
// Batch 13: support integration + count consistency (sections 13-14)
// ------------------------------------------------------------------

#[test]
fn support_candidates_are_counted_and_reconcile() {
    use crate::platform_evidence_fusion::set_destination::SupportCandidate;
    use crate::platform_evidence_fusion::side_file_classification::SideFileRole;
    use crate::platform_evidence_fusion::support_attachment::SupportAssociation;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");
    let manual_path = dir.path().join("manual.pdf");
    let readme_path = dir.path().join("readme.txt");
    let cue_path = dir.path().join("bad.cue");

    let inputs = vec![LibraryPlanInput {
        source_path: rom,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let candidates = vec![
        SupportCandidate {
            path: &manual_path,
            role: SideFileRole::Manual,
            association: SupportAssociation::Candidate {
                reason: "ambiguous".to_string(),
            },
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &readme_path,
            role: SideFileRole::Readme,
            association: SupportAssociation::Unassociated,
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &cue_path,
            role: SideFileRole::CueSheet,
            association: SupportAssociation::UnsafeReference {
                detail: "traversal".to_string(),
            },
            referenced_members: Vec::new(),
        },
    ];
    let report = build_full_report(&inputs, &context, &candidates);
    assert_eq!(report.counts.support_files, 3);
    assert_eq!(report.counts.support_candidate, 1);
    assert_eq!(report.counts.support_unassociated, 1);
    assert_eq!(report.counts.support_unsafe, 1);
    assert_eq!(report.counts.support_attached, 0);
    assert!(report.counts.check_invariants().is_ok());
}

#[test]
fn attached_support_reaches_a_real_set_destination_via_full_report() {
    use crate::platform_evidence_fusion::set_destination::SupportCandidate;
    use crate::platform_evidence_fusion::side_file_classification::SideFileRole;
    use crate::platform_evidence_fusion::support_attachment::SupportAssociation;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");
    let cue_path = dir.path().join("game.cue");

    let inputs = vec![LibraryPlanInput {
        source_path: rom.clone(),
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
        release_relationship: None,
    }];
    let slug = |p: &str| (p == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let candidates = vec![SupportCandidate {
        path: &cue_path,
        role: SideFileRole::CueSheet,
        association: SupportAssociation::Attached {
            set_label: "game".to_string(),
        },
        referenced_members: vec![rom],
    }];
    let report = build_full_report(&inputs, &context, &candidates);
    assert_eq!(report.set_destinations.len(), 1);
    assert_eq!(report.counts.support_attached, 1);
    assert!(report.support_items[0].proposed_destination.is_some());
    assert!(report.counts.check_invariants().is_ok());
}

#[test]
fn plan_status_totals_reconcile_with_primary_items() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom_a = write_temp(dir.path(), "a.bin");
    let rom_b = write_temp(dir.path(), "b.bin");
    let inputs = vec![
        LibraryPlanInput {
            source_path: rom_a,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
        LibraryPlanInput {
            source_path: rom_b,
            identity: inspect_identity(IdentityInspectionInput::default()),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &[]);
    assert!(report.counts.check_invariants().is_ok());
    assert_eq!(
        report.counts.ready
            + report.counts.needs_review
            + report.counts.ambiguous
            + report.counts.conflict
            + report.counts.unknown
            + report.counts.unsupported,
        report.counts.primary_items
    );
}

#[test]
fn different_revision_groups_are_counted_and_reconcile() {
    use crate::platform_evidence_fusion::release_relationship::ReleaseRelationship;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let a = write_temp(dir.path(), "a.bin");
    let b = write_temp(dir.path(), "b.bin");
    let inputs = vec![
        LibraryPlanInput {
            source_path: a,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: Some(ReleaseRelationship::Canonical {
                game_name: "Game (USA)".to_string(),
            }),
        },
        LibraryPlanInput {
            source_path: b,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: Some(ReleaseRelationship::CloneOf {
                game_name: "Game (USA) (Rev 1)".to_string(),
                parent: "Game (USA)".to_string(),
            }),
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context, &[]);
    assert_eq!(report.counts.different_revision_groups, 1);
    assert_eq!(report.counts.release_groups, 1);
}

#[test]
fn no_item_counted_twice_across_mutually_exclusive_support_states() {
    // Structural: the four support-state buckets are computed from a
    // single match over `SupportAssociation`, which is itself an enum -
    // no candidate can ever land in two buckets. Verified directly via
    // check_invariants over a mixed batch.
    use crate::platform_evidence_fusion::set_destination::SupportCandidate;
    use crate::platform_evidence_fusion::side_file_classification::SideFileRole;
    use crate::platform_evidence_fusion::support_attachment::SupportAssociation;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let paths: Vec<PathBuf> = (0..4)
        .map(|i| dir.path().join(format!("f{i}.txt")))
        .collect();
    let candidates = vec![
        SupportCandidate {
            path: &paths[0],
            role: SideFileRole::Readme,
            association: SupportAssociation::Unassociated,
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &paths[1],
            role: SideFileRole::Manual,
            association: SupportAssociation::Candidate {
                reason: "x".to_string(),
            },
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &paths[2],
            role: SideFileRole::CueSheet,
            association: SupportAssociation::UnsafeReference {
                detail: "x".to_string(),
            },
            referenced_members: Vec::new(),
        },
        SupportCandidate {
            path: &paths[3],
            role: SideFileRole::Artwork,
            association: SupportAssociation::Unassociated,
            referenced_members: Vec::new(),
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&[], &context, &candidates);
    assert_eq!(report.counts.support_unassociated, 2);
    assert_eq!(report.counts.support_candidate, 1);
    assert_eq!(report.counts.support_unsafe, 1);
    assert!(report.counts.check_invariants().is_ok());
}

#[test]
fn no_negative_or_impossible_derived_counts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&[], &context, &[]);
    // usize fields can never be negative by construction; this asserts the
    // more meaningful "empty input produces an entirely zeroed, internally
    // consistent count block" property.
    assert_eq!(
        report.counts,
        crate::platform_evidence_fusion::full_library_report::FullLibraryCounts::default()
    );
    assert!(report.counts.check_invariants().is_ok());
}
