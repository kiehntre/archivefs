use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::rom_organisation::OrganisationMode;
use crate::platform_evidence_fusion::archive_set_identity::ArchiveSetIdentity;
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, no_slug_mapping, plan_library,
};
use std::path::PathBuf;

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    ContentEvidence::new(kind, value, ContentEvidenceConfidence::Strong, "test fact")
}

fn write_temp(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"dummy content").unwrap();
    path
}

fn saturn_identity() -> crate::platform_evidence_fusion::identity_orchestrator::IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        ..Default::default()
    })
}

#[test]
fn ready_plan_shows_a_real_destination_preview() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |p: &str| (p == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.destination_preview.is_some());
    assert_eq!(
        presentation.status,
        crate::platform_evidence_fusion::library_planning::PlanStatus::Ready
    );
}

#[test]
fn non_ready_plan_never_shows_a_fabricated_destination() {
    // A genuinely unresolved identity (never "no RomM slug" - Batch 11's
    // RomM decoupling means a missing RomM mapping alone no longer makes a
    // plan non-Ready; see `library_planning::tests::
    // confident_identity_with_no_romm_mapping_is_ready_not_unsupported`).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = inspect_identity(IdentityInspectionInput::default());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.destination_preview.is_none());
}

#[test]
fn romm_unmapped_alone_still_reaches_ready_with_a_real_destination() {
    // The decoupling itself, exercised from the presentation layer: a
    // confidently resolved platform with no RomM mapping must still show a
    // real destination preview - RomM's own summary independently says
    // Unmapped.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.destination_preview.is_some());
    assert!(
        presentation
            .romm_summary
            .to_lowercase()
            .contains("no romm slug mapping")
    );
}

#[test]
fn rename_summary_always_says_not_authorized_when_a_name_is_proposed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |p: &str| (p == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.rename_summary.contains("NOT AUTHORIZED"));
}

#[test]
fn set_summary_reports_single_file_when_no_archive_context() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.set_summary.contains("Single file"));
}

#[test]
fn set_summary_reports_multi_member_when_archive_context_present() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let set = ArchiveSetIdentity::MultiMemberSamePlatform {
        member_indices: vec![0, 1],
        platform: "SNES",
    };
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug_mapping,
        generation: 1,
    };
    let report = plan_library(
        &[LibraryPlanInput {
            source_path: source,
            identity: identity.clone(),
            set_identity: Some(set),
            physical_hash: None,
            normalized_hash: None,
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(presentation.set_summary.contains("Multi-member"));
    assert!(presentation.set_summary.contains("not collapsed"));
}

#[test]
fn romm_summary_reports_unmapped_honestly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert!(
        presentation
            .romm_summary
            .to_lowercase()
            .contains("no romm slug mapping")
    );
}

#[test]
fn render_text_always_reports_source_modified_no() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    let text = render_library_plan_text(&presentation);
    assert!(text.contains("Source modified:\n  NO"));
}

#[test]
fn render_text_conflict_never_proposes_a_library() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.iso");
    let identity = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![
            strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
            strong(ContentEvidenceKind::ContentSignature, "XBEH"),
        ],
        dat: Some(crate::dat::identity::resolve_dat_platform_identity([
            crate::dat::identity::DatPlatformEvidence {
                platform: "Xbox360".to_string(),
                machine_key: None,
                kind: crate::dat::identity::DatPlatformEvidenceKind::HeaderName,
                confidence: crate::dat::identity::DatPlatformConfidence::Strong,
                detail: "test".to_string(),
            },
        ])),
        ..Default::default()
    });
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let presentation = present_library_plan(&report.items[0], &identity);
    assert_eq!(
        presentation.status,
        crate::platform_evidence_fusion::library_planning::PlanStatus::Conflict
    );
    assert!(presentation.destination_preview.is_none());
    assert!(
        presentation
            .platform_library
            .as_deref()
            .unwrap_or("(none - unresolved identity)")
            .contains("none")
            || presentation.platform_library.is_none()
    );
}

#[test]
fn present_library_plan_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let identity = saturn_identity();
    let slug = |p: &str| (p == "Saturn").then(|| "saturn".to_string());
    let context = LibraryPlanningContext {
        destination_root: &root,
        mode: OrganisationMode::MoveRealFile,
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
        }],
        &context,
    );
    let a = present_library_plan(&report.items[0], &identity);
    let b = present_library_plan(&report.items[0], &identity);
    assert_eq!(a, b);
}

#[test]
fn library_plan_presentation_source_never_references_mutation_functions() {
    let source = include_str!("../library_plan_presentation.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "apply_organisation_transaction",
        "build_organisation_transaction",
    ] {
        assert!(!source.contains(forbidden));
    }
}
