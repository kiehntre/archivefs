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
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let rom = write_temp(dir.path(), "game.bin");
    let cover = write_temp(dir.path(), "cover.jpg");

    let inputs = vec![
        LibraryPlanInput {
            source_path: rom,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
        LibraryPlanInput {
            source_path: cover,
            identity: inspect_identity(IdentityInspectionInput::default()),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context);
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
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context);
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
        },
        LibraryPlanInput {
            source_path: b,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: Some("hash1".to_string()),
            normalized_hash: None,
        },
    ];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context);
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
    }];
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = build_full_report(&inputs, &context);
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
    let report = build_full_report(&[], &context);
    assert_eq!(report.counts.primary_items, 0);
    assert!(report.duplicate_groups.is_empty());
    assert!(report.multidisc_sets.is_empty());
}
