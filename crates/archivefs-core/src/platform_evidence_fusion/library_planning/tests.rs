use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::audit::AuditVerdict;
use crate::dat::identity::{
    DatPlatformEvidence, DatPlatformEvidenceKind, resolve_dat_platform_identity,
};
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::{FusionOutcome, fuse_platform_evidence};

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

// ------------------------------------------------------------------
// identity_result_to_evidence / identity_result_to_resolution (the bridge)
// ------------------------------------------------------------------

#[test]
fn resolved_content_becomes_inference_tier_evidence() {
    let result = saturn_identity();
    let evidence = identity_result_to_evidence(&result, 1);
    assert!(
        evidence
            .iter()
            .any(|e| e.platform == "Saturn" && e.source == PlatformIdentitySource::Inference)
    );
}

#[test]
fn confident_dat_hash_becomes_verified_dat_tier() {
    let result = inspect_identity(IdentityInspectionInput {
        dat: Some(resolved_dat("N64")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Some Game", "some.bin"),
        }),
        ..Default::default()
    });
    let evidence = identity_result_to_evidence(&result, 1);
    let dat_item = evidence
        .iter()
        .find(|e| e.platform == "N64")
        .expect("N64 evidence present");
    assert_eq!(dat_item.source, PlatformIdentitySource::VerifiedDat);
    assert_eq!(dat_item.confidence, PlatformIdentityConfidence::Verified);
}

#[test]
fn dat_source_identity_without_hash_stays_inference_tier() {
    let result = inspect_identity(IdentityInspectionInput {
        dat: Some(resolved_dat("N64")),
        ..Default::default()
    });
    let evidence = identity_result_to_evidence(&result, 1);
    let dat_item = evidence.iter().find(|e| e.platform == "N64").unwrap();
    assert_eq!(dat_item.source, PlatformIdentitySource::Inference);
    assert_ne!(dat_item.confidence, PlatformIdentityConfidence::Verified);
}

#[test]
fn dat_hash_disagreement_never_upgrades_a_non_confident_verdict() {
    let result = inspect_identity(IdentityInspectionInput {
        dat: Some(resolved_dat("N64")),
        representation_match: Some(RepresentationMatchOutcome::NoMatch),
        ..Default::default()
    });
    let evidence = identity_result_to_evidence(&result, 1);
    let dat_item = evidence.iter().find(|e| e.platform == "N64").unwrap();
    assert_eq!(dat_item.source, PlatformIdentitySource::Inference);
}

#[test]
fn bridge_never_produces_a_manual_or_romm_tier_item() {
    // Neither of those tiers is ever appropriate for anything this crate's
    // content/DAT stack itself produces - only Inference/VerifiedDat.
    let result = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Game", "game.bin"),
        }),
        ..Default::default()
    });
    let evidence = identity_result_to_evidence(&result, 1);
    for item in &evidence {
        assert!(matches!(
            item.source,
            PlatformIdentitySource::Inference | PlatformIdentitySource::VerifiedDat
        ));
    }
}

#[test]
fn resolution_resolves_when_content_and_verified_dat_agree() {
    let result = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Athlete Kings", "athlete_kings.bin"),
        }),
        ..Default::default()
    });
    let resolution = identity_result_to_resolution(&result, 1);
    assert_eq!(resolution.platform(), Some("Saturn"));
    assert_eq!(
        resolution.evidence().iter().map(|e| e.confidence).max(),
        Some(PlatformIdentityConfidence::Verified)
    );
}

#[test]
fn resolution_conflicts_when_content_and_verified_dat_disagree() {
    let result = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Xbox")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Some Xbox Game", "game.iso"),
        }),
        ..Default::default()
    });
    let resolution = identity_result_to_resolution(&result, 1);
    assert!(resolution.is_conflict());
}

#[test]
fn bridge_is_deterministic() {
    let result = saturn_identity();
    assert_eq!(
        identity_result_to_evidence(&result, 1),
        identity_result_to_evidence(&result, 1)
    );
}

// ------------------------------------------------------------------
// plan_status derivation (section 5)
// ------------------------------------------------------------------

#[test]
fn identity_conflict_always_wins_over_organisation_status() {
    assert_eq!(
        plan_status(IdentityStatus::Conflict, OrganisationStatus::Suggested),
        PlanStatus::Conflict
    );
}

#[test]
fn identity_ambiguous_always_wins_over_organisation_status() {
    assert_eq!(
        plan_status(IdentityStatus::Ambiguous, OrganisationStatus::Suggested),
        PlanStatus::Ambiguous
    );
}

#[test]
fn identity_unknown_always_wins_over_organisation_status() {
    assert_eq!(
        plan_status(IdentityStatus::Unknown, OrganisationStatus::Suggested),
        PlanStatus::Unknown
    );
}

#[test]
fn suggested_organisation_status_is_ready() {
    assert_eq!(
        plan_status(IdentityStatus::ContentOnly, OrganisationStatus::Suggested),
        PlanStatus::Ready
    );
}

#[test]
fn already_organised_is_ready() {
    assert_eq!(
        plan_status(
            IdentityStatus::ContentAndDatAgree,
            OrganisationStatus::AlreadyOrganised
        ),
        PlanStatus::Ready
    );
}

#[test]
fn organisation_conflict_is_conflict() {
    assert_eq!(
        plan_status(IdentityStatus::ContentOnly, OrganisationStatus::Conflict),
        PlanStatus::Conflict
    );
}

#[test]
fn organisation_blocked_is_needs_review() {
    assert_eq!(
        plan_status(IdentityStatus::VerifiedByDat, OrganisationStatus::Blocked),
        PlanStatus::NeedsReview
    );
}

#[test]
fn organisation_unsupported_is_unsupported() {
    assert_eq!(
        plan_status(IdentityStatus::DatOnly, OrganisationStatus::Unsupported),
        PlanStatus::Unsupported
    );
}

#[test]
fn plan_status_labels_are_all_distinct() {
    let statuses = [
        PlanStatus::Ready,
        PlanStatus::NeedsReview,
        PlanStatus::Ambiguous,
        PlanStatus::Conflict,
        PlanStatus::Unknown,
        PlanStatus::Unsupported,
    ];
    let labels: std::collections::BTreeSet<&str> = statuses.iter().map(|s| s.label()).collect();
    assert_eq!(labels.len(), statuses.len());
}

// ------------------------------------------------------------------
// RomM mapping preview (sections 24-29)
// ------------------------------------------------------------------

fn entry(
    platform: Option<&str>,
    slug: Option<&str>,
    status: OrganisationStatus,
) -> OrganisationPlanEntry {
    OrganisationPlanEntry {
        source_path: PathBuf::from("/tmp/whatever"),
        destination_path: PathBuf::new(),
        platform: platform.map(String::from),
        platform_display_name: String::new(),
        platform_source: String::new(),
        slug: slug.map(String::from),
        mode: OrganisationMode::RenameInPlace,
        content_classification: None,
        original_metadata: Default::default(),
        status,
        reason: None,
    }
}

#[test]
fn mapped_when_platform_and_slug_both_present() {
    let e = entry(Some("N64"), Some("n64"), OrganisationStatus::Suggested);
    let preview = romm_mapping_preview(&e);
    assert_eq!(preview.status, RommMappingStatus::Mapped);
    assert!(preview.warnings.is_empty());
}

#[test]
fn unmapped_when_platform_present_but_no_slug() {
    let e = entry(Some("N64"), None, OrganisationStatus::Unsupported);
    let preview = romm_mapping_preview(&e);
    assert_eq!(preview.status, RommMappingStatus::Unmapped);
    assert!(!preview.warnings.is_empty());
}

#[test]
fn unsupported_when_no_platform_at_all() {
    let e = entry(None, None, OrganisationStatus::Blocked);
    let preview = romm_mapping_preview(&e);
    assert_eq!(preview.status, RommMappingStatus::Unsupported);
}

#[test]
fn ambiguous_when_platform_present_no_slug_and_conflict_status() {
    let e = entry(Some("Xbox"), None, OrganisationStatus::Conflict);
    let preview = romm_mapping_preview(&e);
    assert_eq!(preview.status, RommMappingStatus::Ambiguous);
}

#[test]
fn no_slug_mapping_always_returns_none() {
    assert_eq!(no_slug_mapping("N64"), None);
    assert_eq!(no_slug_mapping("anything at all"), None);
}

#[test]
fn romm_never_authors_canonical_identity() {
    // Structural: RommMappingPreview has no field or method that could feed
    // back into platform identity resolution - canonical_platform here is
    // read FROM the already-resolved entry, never the other way around
    // (milestone section 26's "RomM is not canonical" rule).
    let e = entry(Some("N64"), Some("n64"), OrganisationStatus::Suggested);
    let preview = romm_mapping_preview(&e);
    assert_eq!(preview.canonical_platform.as_deref(), Some("N64"));
}

// ------------------------------------------------------------------
// Rename suggestion (sections 16-17, 43)
// ------------------------------------------------------------------

#[test]
fn rename_suggestion_is_never_authorized() {
    for status in [
        OrganisationStatus::Suggested,
        OrganisationStatus::AlreadyOrganised,
        OrganisationStatus::Conflict,
        OrganisationStatus::Blocked,
        OrganisationStatus::Unsupported,
    ] {
        let e = entry(Some("N64"), Some("n64"), status);
        let suggestion = rename_suggestion(&e);
        assert!(!suggestion.authorized);
    }
}

#[test]
fn dat_sourced_platform_yields_authoritative_dat_release_basis() {
    let mut e = entry(Some("N64"), Some("n64"), OrganisationStatus::Suggested);
    e.platform_source = "Verified DAT".to_string();
    e.destination_path = PathBuf::from("/root/N64/Some Game.z64");
    e.source_path = PathBuf::from("/src/Some Game.z64");
    let suggestion = rename_suggestion(&e);
    assert_eq!(suggestion.basis, RenameBasis::AuthoritativeDatRelease);
    assert_eq!(suggestion.proposed_name.as_deref(), Some("Some Game.z64"));
}

#[test]
fn non_dat_platform_yields_original_name_preserved_basis() {
    let mut e = entry(Some("N64"), Some("n64"), OrganisationStatus::Suggested);
    e.platform_source = "Existing game identity".to_string();
    e.destination_path = PathBuf::from("/root/N64/game.z64");
    e.source_path = PathBuf::from("/src/game.z64");
    let suggestion = rename_suggestion(&e);
    assert_eq!(suggestion.basis, RenameBasis::OriginalNamePreserved);
}

#[test]
fn blocked_entry_yields_unavailable_basis_and_no_proposed_name() {
    let mut e = entry(None, None, OrganisationStatus::Blocked);
    e.reason = Some("unknown platform".to_string());
    let suggestion = rename_suggestion(&e);
    assert_eq!(suggestion.basis, RenameBasis::Unavailable);
    assert!(suggestion.proposed_name.is_none());
    assert!(!suggestion.blockers.is_empty());
}

#[test]
fn conflict_entry_yields_unavailable_basis() {
    let e = entry(Some("Xbox"), Some("xbox"), OrganisationStatus::Conflict);
    let suggestion = rename_suggestion(&e);
    assert_eq!(suggestion.basis, RenameBasis::Unavailable);
}

#[test]
fn original_name_always_reflects_the_source_basename() {
    let mut e = entry(Some("N64"), Some("n64"), OrganisationStatus::Suggested);
    e.source_path = PathBuf::from("/src/original name.z64");
    let suggestion = rename_suggestion(&e);
    assert_eq!(suggestion.original_name, "original name.z64");
}

// ------------------------------------------------------------------
// plan_library collection aggregation (sections 28-30, real files)
// ------------------------------------------------------------------

fn write_temp(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"dummy content").unwrap();
    path
}

fn context<'a>(
    root: &'a Path,
    slug: &'a dyn Fn(&str) -> Option<String>,
) -> LibraryPlanningContext<'a> {
    LibraryPlanningContext {
        destination_root: root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: slug,
        generation: 1,
    }
}

#[test]
fn plan_library_resolves_a_confident_single_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.ready, 1);
    assert_eq!(report.romm_mapped, 1);
}

#[test]
fn plan_library_reports_unsupported_when_no_slug_mapping_exists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    assert_eq!(report.romm_unmapped, 1);
    assert_eq!(report.romm_mapped, 0);
}

// ------------------------------------------------------------------
// RomM decoupling (milestone sections 35-36) - the batch's own named
// "IMPORTANT ROMM DECOUPLING TEST".
// ------------------------------------------------------------------

#[test]
fn confident_identity_with_no_romm_mapping_is_ready_not_unsupported() {
    // Section 36's exact case: content+DAT confidently resolve a platform,
    // no RomM mapping exists at all. The library plan must still be able
    // to reach Ready (a real destination, under the library-native
    // fallback folder) - RomM mapping unavailability must never by itself
    // drag the whole plan down to Unsupported.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let identity = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Saturn")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Athlete Kings", "athlete_kings.bin"),
        }),
        ..Default::default()
    });
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));

    assert_eq!(report.items.len(), 1);
    assert_eq!(
        report.items[0].status,
        PlanStatus::Ready,
        "a confidently resolved platform with no RomM mapping must still reach Ready"
    );
    assert_eq!(report.ready, 1);
    assert_eq!(report.unsupported, 0);
    // RomM's own state is independently, honestly Unmapped - never
    // silently upgraded, never allowed to drag the plan down either.
    assert_eq!(report.items[0].romm.status, RommMappingStatus::Unmapped);
    assert_eq!(report.romm_unmapped, 1);
    // The public `organisation.slug` field must report the *real* RomM
    // state (None), never the internal library-folder fallback string.
    assert!(report.items[0].organisation.slug.is_none());
    // A real destination was still computed, using the library-native
    // fallback folder (the canonical platform id).
    assert!(
        report.items[0]
            .organisation
            .destination_path
            .starts_with(&root)
    );
}

#[test]
fn romm_mapped_and_library_ready_agree_when_a_slug_exists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());

    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    assert_eq!(report.items[0].status, PlanStatus::Ready);
    assert_eq!(report.items[0].romm.status, RommMappingStatus::Mapped);
    assert_eq!(
        report.items[0].organisation.slug.as_deref(),
        Some("saturn"),
        "when a real RomM slug exists, organisation.slug must be that real slug, not the \
         library-native fallback"
    );
}

#[test]
fn unresolved_identity_still_makes_the_plan_unsupported_or_needs_review_regardless_of_romm() {
    // Decoupling must never *weaken* identity: an item whose identity is
    // genuinely Unknown must not become Ready just because the library
    // fallback can technically build *a* folder name for a resolved
    // platform - here there is no resolved platform at all, so both plans
    // agree there is nothing to organise.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());

    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: inspect_identity(IdentityInspectionInput::default()),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    assert_eq!(report.items[0].status, PlanStatus::Unknown);
    assert_ne!(report.items[0].status, PlanStatus::Ready);
}

#[test]
fn plan_library_counts_conflicts_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let conflicting_identity = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(
            ContentEvidenceKind::BootStructure,
            "SEGA SEGASATURN",
        )],
        dat: Some(resolved_dat("Xbox")),
        ..Default::default()
    });
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: conflicting_identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    assert_eq!(report.conflict, 1);
    assert_eq!(report.ready, 0);
}

#[test]
fn plan_library_counts_unknown_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let unknown_identity = inspect_identity(IdentityInspectionInput::default());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: unknown_identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    assert_eq!(report.unknown, 1);
}

#[test]
fn plan_library_counts_ambiguous_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let ambiguous_content = fuse_platform_evidence([
        ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "BOOT2",
            ContentEvidenceConfidence::Corroborated,
            "test",
        ),
        ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "ELF",
            ContentEvidenceConfidence::Weak,
            "test",
        ),
    ]);
    assert_eq!(ambiguous_content.outcome, FusionOutcome::Ambiguous);
    let ambiguous_identity = inspect_identity(IdentityInspectionInput {
        content_evidence: ambiguous_content.input_evidence,
        ..Default::default()
    });
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: ambiguous_identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    assert_eq!(report.ambiguous, 1);
}

#[test]
fn plan_library_carries_set_identity_separately_from_platform() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let set = ArchiveSetIdentity::MultiMemberSamePlatform {
        member_indices: vec![0, 1],
        platform: "SNES",
    };
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: Some(set.clone()),
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    assert_eq!(report.items[0].set_identity, Some(set));
    // The organisation destination is still Saturn's own single-file
    // proposal - set identity never collapses or overrides it.
    assert_eq!(
        report.items[0].organisation.platform.as_deref(),
        Some("Saturn")
    );
}

#[test]
fn plan_library_is_deterministic_regardless_of_input_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source_a = write_temp(dir.path(), "a.bin");
    let source_b = write_temp(dir.path(), "b.bin");

    let inputs_forward = vec![
        LibraryPlanInput {
            source_path: source_a.clone(),
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
        LibraryPlanInput {
            source_path: source_b.clone(),
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
    ];
    let inputs_backward = vec![
        LibraryPlanInput {
            source_path: source_b,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
        LibraryPlanInput {
            source_path: source_a,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
    ];
    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let forward = plan_library(&inputs_forward, &context(&root, &slug));
    let backward = plan_library(&inputs_backward, &context(&root, &slug));
    assert_eq!(forward.items, backward.items);
    assert_eq!(forward.ready, backward.ready);
}

#[test]
fn plan_library_empty_input_is_empty_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let report = plan_library(&[], &context(&root, &no_slug_mapping));
    assert!(report.items.is_empty());
    assert_eq!(report.ready, 0);
}

// ------------------------------------------------------------------
// No action authority (section 48)
// ------------------------------------------------------------------

#[test]
fn library_planning_source_never_references_mutation_functions() {
    let source = include_str!("../library_planning.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "std::os::unix::fs::symlink",
        "rename_apply::apply",
        "apply_organisation_transaction",
        "build_organisation_transaction",
        "rollback_organisation_transaction",
    ] {
        assert!(
            !source.contains(forbidden),
            "library_planning.rs unexpectedly references {forbidden:?}"
        );
    }
}

#[test]
fn plan_library_never_writes_to_the_source_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");
    let before = std::fs::read(&source).unwrap();

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source.clone(),
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let _ = plan_library(&inputs, &context(&root, &slug));

    let after = std::fs::read(&source).unwrap();
    assert_eq!(before, after);
    assert!(
        source.exists(),
        "source file must still exist at its original path"
    );
}

#[test]
fn plan_library_never_silently_proposes_the_dat_platform_over_a_disagreeing_content_platform() {
    // The exact regression this milestone's final rule warns about: even
    // with a *confident cryptographic hash* backing the DAT platform, a
    // genuine content-vs-DAT disagreement must still plan to Conflict, not
    // quietly resolve to whichever side happens to carry the stronger
    // confidence tier.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.iso");

    let identity = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![
            strong(ContentEvidenceKind::Filesystem, "XDVDFS"),
            strong(ContentEvidenceKind::ContentSignature, "XBEH"),
        ],
        dat: Some(resolved_dat("Xbox360")),
        representation_match: Some(RepresentationMatchOutcome::PhysicalOnly {
            verdict: exact_verdict("Some Xbox 360 Game", "game.iso"),
        }),
        ..Default::default()
    });
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity,
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let slug = |_: &str| Some("whatever".to_string());
    let report = plan_library(&inputs, &context(&root, &slug));
    assert_eq!(report.conflict, 1);
    assert_eq!(report.ready, 0);
    assert!(report.items[0].organisation.platform.is_none());
}

// ------------------------------------------------------------------
// Adversarial path/name safety through this bridge's own inputs
// (sections 14/19) - exercising `plan_library`/`build_organisation_candidate`
// directly with hostile real source filenames, not re-testing
// `rename_plan`'s own sanitiser in isolation (that already has its own unit
// tests). The property under test here is: whatever a hostile or malformed
// input looks like, the destination this bridge proposes for a Ready item
// must stay strictly inside `destination_root`, and every path derived from
// an unsafe name must be something other than a silent Ready.
// ------------------------------------------------------------------

fn destination_root_contains(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

#[test]
fn adversarial_filename_with_embedded_newline_never_escapes_destination_root() {
    // '\n' is not rejected by the filesystem itself (unlike '/' or NUL), so
    // this is a genuinely creatable real file, not a synthetic PathBuf.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "evil\nname.bin");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let entry = &report.items[0].organisation;
    assert!(
        destination_root_contains(&root, &entry.destination_path),
        "destination {:?} escaped destination_root {:?}",
        entry.destination_path,
        root
    );
}

#[test]
fn adversarial_very_long_filename_is_handled_without_a_silent_ready_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    // Near typical filesystem NAME_MAX (255 bytes); stay a little under it
    // once the ".bin" extension is included so the write itself succeeds.
    let long_stem = "a".repeat(240);
    let source = write_temp(dir.path(), &format!("{long_stem}.bin"));

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let entry = &report.items[0].organisation;
    assert!(destination_root_contains(&root, &entry.destination_path));
}

#[test]
fn adversarial_dot_prefixed_filename_never_escapes_destination_root() {
    // Not literally ".." (the filesystem forbids that as a distinct
    // component here), but a name that is *only* dots plus an extension -
    // the kind of thing a naive string-concatenation destination builder
    // could mishandle.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "...bin");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let entry = &report.items[0].organisation;
    assert!(destination_root_contains(&root, &entry.destination_path));
}

#[test]
fn adversarial_caller_supplied_source_path_with_dotdot_components_is_not_trusted_verbatim() {
    // A caller of this bridge (not the filesystem) could hand in a
    // `LibraryPlanInput::source_path` whose *string* contains `..`
    // components even though the physical file itself sits somewhere
    // normal - `PathBuf` does not forbid this. The property under test is
    // narrow but real: this must never cause the *destination* path to
    // contain a `..` component, regardless of what the source path's own
    // string looks like.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let real = write_temp(&nested, "game.bin");
    // Same physical file, referenced via a path string containing `..`.
    let via_dotdot = nested.join("..").join("nested").join("game.bin");
    assert_eq!(
        std::fs::canonicalize(&via_dotdot).unwrap(),
        std::fs::canonicalize(&real).unwrap()
    );

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: via_dotdot,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let entry = &report.items[0].organisation;
    assert!(
        !entry
            .destination_path
            .components()
            .any(|c| c.as_os_str() == ".."),
        "destination path {:?} contains a '..' component",
        entry.destination_path
    );
    assert!(destination_root_contains(&root, &entry.destination_path));
}

#[test]
fn adversarial_duplicate_destination_collision_is_reported_not_silently_ready() {
    // Two distinct source files that would resolve to the exact same
    // canonical destination (same platform, same DAT-derived proposed
    // name) must never both come back Ready - one occupying a path is a
    // real Conflict, not something this bridge is allowed to paper over.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let a = write_temp(dir.path(), "game_a.bin");
    let b = write_temp(dir.path(), "game_b.bin");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![
        LibraryPlanInput {
            source_path: a,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
        LibraryPlanInput {
            source_path: b,
            identity: saturn_identity(),
            set_identity: None,
            physical_hash: None,
            normalized_hash: None,
        },
    ];
    let report = plan_library(&inputs, &context(&root, &slug));
    assert_eq!(report.items.len(), 2);
    let destinations: std::collections::HashSet<_> = report
        .items
        .iter()
        .map(|item| item.organisation.destination_path.clone())
        .collect();
    if destinations.len() == 1 {
        // Same destination proposed for two different sources: at most one
        // may be Ready.
        let ready_count = report
            .items
            .iter()
            .filter(|item| item.status == PlanStatus::Ready)
            .count();
        assert!(
            ready_count <= 1,
            "two different sources both came back Ready for the same destination {:?}",
            destinations.iter().next().unwrap()
        );
    }
}

// ------------------------------------------------------------------
// Conflict/ambiguous matrix (sections 33/34/46/47)
//
// Real files never spontaneously disagree with themselves - a genuine
// GameCube disc's own header does not also claim to be a Wii disc. So this
// matrix is built from real detector *output shapes* (the exact
// `ContentEvidenceKind`/value strings each real observer in this crate
// actually emits, per the corresponding *_boot_evidence module read above -
// e.g. `("BootStructure", "GameCube")` / `("BootStructure", "Wii")`) fed in
// as two disagreeing evidence bundles, rather than a hunt for real files
// that happen to be mislabeled. The N64/GameCube pairing this batch already
// regression-tests via `plan_library_never_silently_proposes_the_dat_platform_over_a_disagreeing_content_platform`
// (content Xbox vs. verified-DAT Xbox360) is the other half of this matrix.
// ------------------------------------------------------------------

fn conflict_case(
    a_kind: ContentEvidenceKind,
    a: &str,
    b_kind: ContentEvidenceKind,
    b: &str,
) -> IdentityResult {
    inspect_identity(IdentityInspectionInput {
        content_evidence: vec![strong(a_kind, a), strong(b_kind, b)],
        ..Default::default()
    })
}

#[test]
fn matrix_gamecube_vs_wii_boot_structure_is_a_real_platform_conflict() {
    // Both disc kinds emit the exact same evidence kind
    // (`ContentEvidenceKind::BootStructure`) with a different value - the
    // real shape `gamecube_wii_boot_evidence::observe_gc_wii_evidence`
    // produces for each disc kind.
    let identity = conflict_case(
        ContentEvidenceKind::BootStructure,
        "GameCube",
        ContentEvidenceKind::BootStructure,
        "Wii",
    );
    let resolution = identity_result_to_resolution(&identity, 1);
    assert!(
        matches!(resolution, PlatformIdentityResolution::Conflict { .. })
            || matches!(resolution, PlatformIdentityResolution::Unknown { .. }),
        "GameCube/Wii disagreement must never silently resolve to one side"
    );
}

#[test]
fn matrix_gb_vs_gba_is_a_real_platform_conflict() {
    // Both `gb_header_evidence`/`gba_header_evidence` emit
    // `ContentEvidenceKind::BootStructure` - the real shared kind.
    let identity = conflict_case(
        ContentEvidenceKind::BootStructure,
        "Game Boy",
        ContentEvidenceKind::BootStructure,
        "Game Boy Advance",
    );
    let resolution = identity_result_to_resolution(&identity, 1);
    assert!(
        matches!(resolution, PlatformIdentityResolution::Conflict { .. })
            || matches!(resolution, PlatformIdentityResolution::Unknown { .. }),
    );
}

#[test]
fn matrix_ps1_vs_ps2_boot_key_is_a_real_platform_conflict() {
    // PS1's SYSTEM.CNF uses `BOOT=`, PS2's uses `BOOT2=` - a genuinely
    // different, real boot-key convention this crate's own
    // `playstation_boot_evidence`/`ps2_boot_evidence` modules distinguish.
    let identity = conflict_case(
        ContentEvidenceKind::BootStructure,
        "PS1",
        ContentEvidenceKind::BootStructure,
        "PS2",
    );
    let resolution = identity_result_to_resolution(&identity, 1);
    assert!(
        matches!(resolution, PlatformIdentityResolution::Conflict { .. })
            || matches!(resolution, PlatformIdentityResolution::Unknown { .. }),
    );
}

#[test]
fn matrix_multi_platform_archive_is_reported_as_multi_platform_not_collapsed() {
    // Milestone section 18C / 34 - a ZIP whose members resolve to two
    // genuinely different platforms must never be collapsed into either
    // one; `ArchiveSetIdentity::MultiPlatform` exists precisely for this.
    let set =
        crate::platform_evidence_fusion::archive_set_identity::ArchiveSetIdentity::MultiPlatform {
            member_indices: vec![0, 1],
            platforms: vec!["N64", "Game Boy"],
        };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "mixed.zip");
    let identity = saturn_identity();
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: identity.clone(),
        set_identity: Some(set),
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &no_slug_mapping));
    let item = &report.items[0];
    assert!(matches!(
        item.set_identity,
        Some(crate::platform_evidence_fusion::archive_set_identity::ArchiveSetIdentity::MultiPlatform { .. })
    ));
}

#[test]
fn matrix_ambiguous_megadrive_candidate_only_stays_ambiguous_not_promoted() {
    // Real corpus finding this batch: a genuine Mega Drive `.md` file whose
    // console-name field the real header parser did not recognize resolves
    // to `IdentityStatus::Ambiguous` via `fuse_platform_evidence`'s own
    // weak/candidate-only handling - captured directly (not re-derived) as
    // a matrix row here.
    let identity = inspect_identity(IdentityInspectionInput {
        content_evidence: vec![ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "Mega Drive candidate (unrecognized console name)",
            ContentEvidenceConfidence::Weak,
            "matrix test fact",
        )],
        ..Default::default()
    });
    let resolution = identity_result_to_resolution(&identity, 1);
    assert!(
        !matches!(resolution, PlatformIdentityResolution::Resolved { .. }),
        "weak, unrecognized-only evidence must never resolve on its own"
    );
}

// ------------------------------------------------------------------
// Serialization (section 40)
// ------------------------------------------------------------------

#[test]
fn library_planning_report_serializes_to_json_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "game.bin");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: Some(crate::platform_evidence_fusion::archive_set_identity::ArchiveSetIdentity::SingleMember {
            member_index: 0,
            platform: "Saturn",
        }),
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let json = serde_json::to_string_pretty(&report).expect("report must serialize to JSON");
    assert!(json.contains("\"status\""));
    assert!(json.contains("\"ready\""));
    // Every fact in the JSON must trace back to the same in-memory report -
    // no re-derivation, so re-parsing as a generic value and checking a
    // couple of real fields is enough to catch a silently-dropped field.
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["ready"], serde_json::json!(report.ready));
}

#[test]
fn adversarial_empty_extension_filename_is_handled_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let source = write_temp(dir.path(), "no_extension_at_all");

    let slug = |platform: &str| (platform == "Saturn").then(|| "saturn".to_string());
    let inputs = vec![LibraryPlanInput {
        source_path: source,
        identity: saturn_identity(),
        set_identity: None,
        physical_hash: None,
        normalized_hash: None,
    }];
    let report = plan_library(&inputs, &context(&root, &slug));
    let entry = &report.items[0].organisation;
    assert!(destination_root_contains(&root, &entry.destination_path));
}
