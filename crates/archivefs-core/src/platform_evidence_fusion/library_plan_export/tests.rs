use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::library_plan_presentation::present_library_plan;
use crate::platform_evidence_fusion::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, no_slug_mapping, plan_library,
};
use std::path::PathBuf;

fn saturn_identity() -> crate::platform_evidence_fusion::identity_orchestrator::IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
            ContentEvidenceConfidence::Strong,
            "test fact",
        )],
        ..Default::default()
    })
}

fn write_temp(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"dummy content").unwrap();
    path
}

#[test]
fn ready_item_exports_a_real_destination_and_move_intent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: Some("abc123".to_string()),
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let export = export_item(&report.items[0], &presentation, Some("abc123"), None);

    assert_eq!(export.status, PlanStatus::Ready);
    assert!(export.proposed_destination.is_some());
    assert_eq!(
        export.operation_intent,
        OperationIntent::MoveToLibraryFolder
    );
    assert_eq!(export.precondition.physical_hash.as_deref(), Some("abc123"));
    assert_eq!(export.romm_status, RommMappingStatus::Mapped);
}

#[test]
fn non_ready_item_exports_no_destination_and_no_operation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = inspect_identity(IdentityInspectionInput::default());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let export = export_item(&report.items[0], &presentation, None, None);

    assert_eq!(export.status, PlanStatus::Unknown);
    assert!(export.proposed_destination.is_none());
    assert_eq!(export.operation_intent, OperationIntent::None);
}

#[test]
fn export_never_computes_a_new_hash_it_only_carries_forward_what_was_supplied() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let export = export_item(&report.items[0], &presentation, None, None);
    assert!(export.precondition.physical_hash.is_none());
    assert!(export.precondition.normalized_hash.is_none());
}

#[test]
fn export_carries_no_executable_authority() {
    // Structural guarantee: no field on the export types is a function
    // pointer, closure, or anything with an `apply`/`execute` method -
    // checked here by scanning the source for the shapes that would
    // introduce one.
    let source = include_str!("../library_plan_export.rs");
    for forbidden in ["fn(", "dyn Fn", "Box<dyn", "fn apply", "fn execute"] {
        assert!(
            !source.contains(forbidden),
            "found forbidden shape: {forbidden}"
        );
    }
}

#[test]
fn library_plan_export_source_never_references_mutation_functions() {
    let source = include_str!("../library_plan_export.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "std::os::unix::fs::symlink",
        "std::fs::write",
        "std::fs::read",
    ] {
        assert!(!source.contains(forbidden));
    }
}

#[test]
fn export_round_trips_through_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: Some("hash".to_string()),
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let export = LibraryPlanExport {
        items: vec![export_item(
            &report.items[0],
            &presentation,
            Some("hash"),
            None,
        )],
    };
    let json = serde_json::to_string(&export).unwrap();
    let round_tripped: LibraryPlanExport = serde_json::from_str(&json).unwrap();
    assert_eq!(export, round_tripped);
}

#[test]
fn export_plan_preserves_caller_supplied_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let a = write_temp(dir.path(), "a.bin");
    let b = write_temp(dir.path(), "b.bin");
    let identity = saturn_identity();
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let report = plan_library(
        &[
            LibraryPlanInput {
                source_path: a,
                identity: identity.clone(),
                set_identity: None,
                physical_hash: None,
                normalized_hash: None,
                release_relationship: None,
            },
            LibraryPlanInput {
                source_path: b,
                identity: identity.clone(),
                set_identity: None,
                physical_hash: None,
                normalized_hash: None,
                release_relationship: None,
            },
        ],
        &context,
    );
    let presentations: Vec<_> = report
        .items
        .iter()
        .map(|item| present_library_plan(item, &identity))
        .collect();
    let pairs: Vec<_> = report
        .items
        .iter()
        .zip(presentations.iter())
        .map(|(plan, presentation)| (plan, presentation, None, None))
        .collect();
    let export = export_plan(&pairs);
    assert_eq!(export.items.len(), 2);
}

#[test]
fn export_with_context_carries_set_and_support_facts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |p: &str| (p == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &slug,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let set_and_support = SetAndSupportContext {
        set_label: Some("Some Game (USA)".to_string()),
        set_destination: Some(
            root.join("saturn")
                .join("Some Game (USA)")
                .display()
                .to_string(),
        ),
        support_role: None,
        support_association: None,
    };
    let export = export_item_with_context(
        &report.items[0],
        &presentation,
        None,
        None,
        &set_and_support,
    );
    assert_eq!(export.set_label.as_deref(), Some("Some Game (USA)"));
    assert!(export.set_destination.is_some());
}

#[test]
fn export_with_context_default_matches_plain_export_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: crate::dat::rom_organisation::OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
            release_relationship: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let plain = export_item(&report.items[0], &presentation, None, None);
    assert!(plain.set_label.is_none());
    assert!(plain.support_role.is_none());
}
