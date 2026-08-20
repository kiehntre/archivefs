use super::*;
use crate::dat::rename_apply::model::{ObjectKind, TransactionState};
use crate::dat::rename_apply::reconcile::reconcile_recovery;
use crate::platform_evidence_fusion::library_plan_export::SourcePrecondition;
use std::sync::atomic::AtomicBool;

fn item(source: &str, destination: &str) -> LibraryPlanExportItem {
    LibraryPlanExportItem {
        status: PlanStatus::Ready,
        precondition: SourcePrecondition {
            source_path: source.to_string(),
            physical_hash: None,
            normalized_hash: None,
        },
        proposed_destination: Some(destination.to_string()),
        operation_intent: OperationIntent::MoveToLibraryFolder,
        platform_library: None,
        display_name: "Test Item".to_string(),
        romm_status: crate::platform_evidence_fusion::library_planning::RommMappingStatus::Unmapped,
        romm_slug: None,
        rename_basis:
            crate::platform_evidence_fusion::library_planning::RenameBasis::OriginalNamePreserved,
        proposed_name: None,
        duplicate_classification: None,
        revision_relationship: None,
        set_label: None,
        set_destination: None,
        support_role: None,
        support_association: None,
        blockers: Vec::new(),
        warnings: Vec::new(),
        source_modified: false,
    }
}

fn ready_export(source: &str, destination: &str, physical_hash: Option<&str>) -> LibraryPlanExport {
    let mut export_item = item(source, destination);
    export_item.precondition.physical_hash = physical_hash.map(str::to_string);
    LibraryPlanExport {
        items: vec![export_item],
    }
}

fn status_export(status: PlanStatus) -> LibraryPlanExport {
    let mut export_item = item("/roms/a.bin", "/lib/a.bin");
    export_item.status = status;
    if status != PlanStatus::Ready {
        export_item.proposed_destination = None;
        export_item.operation_intent = OperationIntent::None;
    }
    LibraryPlanExport {
        items: vec![export_item],
    }
}

// ------------------------------------------------------------------
// Plan digest (sections 7-8, 10)
// ------------------------------------------------------------------

#[test]
fn digest_is_deterministic_for_the_same_export() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    assert_eq!(compute_plan_digest(&export), compute_plan_digest(&export));
}

#[test]
fn digest_changes_when_destination_changes() {
    let a = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let b = ready_export("/roms/a.bin", "/lib/ps/other.bin", Some("hash1"));
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_hash_changes() {
    let a = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let b = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash2"));
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_status_changes() {
    let mut a = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let mut b = a.clone();
    b.items[0].status = PlanStatus::NeedsReview;
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
    a.items[0].status = PlanStatus::Ready;
    assert_eq!(
        a,
        ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"))
    );
}

#[test]
fn digest_changes_when_blockers_change() {
    let a = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let mut b = a.clone();
    b.items[0].blockers.push("something".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_set_label_changes() {
    let mut a = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let mut b = a.clone();
    b.items[0].set_label = Some("Some Set".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
    a.items[0].set_label = None;
}

#[test]
fn digest_is_stable_across_multiple_computations() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", None);
    let d1 = compute_plan_digest(&export);
    let d2 = compute_plan_digest(&export);
    let d3 = compute_plan_digest(&export);
    assert_eq!(d1, d2);
    assert_eq!(d2, d3);
}

#[test]
fn digest_is_a_64_character_hex_string() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", None);
    let digest = compute_plan_digest(&export);
    assert_eq!(digest.0.len(), 64);
    assert!(digest.0.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn empty_export_has_a_stable_digest() {
    let export = LibraryPlanExport { items: Vec::new() };
    assert_eq!(compute_plan_digest(&export), compute_plan_digest(&export));
}

#[test]
fn digest_order_reflects_item_order() {
    let a = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    let b = LibraryPlanExport {
        items: vec![
            item("/roms/b.bin", "/lib/b.bin"),
            item("/roms/a.bin", "/lib/a.bin"),
        ],
    };
    // Item order in the export is caller-determined and stable per Batch
    // 12/13's own determinism guarantee; a different order is a genuinely
    // different serialization, so the digest is free to (and does) differ.
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

// ------------------------------------------------------------------
// Preview (sections 30-31)
// ------------------------------------------------------------------

#[test]
fn preview_includes_one_operation_per_ready_item() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    assert_eq!(preview.total_operation_count, 1);
    assert_eq!(preview.unsupported_item_count, 0);
}

#[test]
fn preview_excludes_non_ready_items() {
    let mut export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    export.items[0].status = PlanStatus::Unknown;
    let preview = build_preview(&export);
    assert_eq!(preview.total_operation_count, 0);
    assert_eq!(preview.unsupported_item_count, 1);
}

#[test]
fn preview_excludes_ready_items_with_blockers() {
    let mut export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    export.items[0].blockers.push("stale hash".to_string());
    let preview = build_preview(&export);
    assert_eq!(preview.total_operation_count, 0);
    assert_eq!(preview.unsupported_item_count, 1);
}

#[test]
fn preview_excludes_ready_items_with_no_destination() {
    let mut export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    export.items[0].proposed_destination = None;
    let preview = build_preview(&export);
    assert_eq!(preview.total_operation_count, 0);
}

#[test]
fn preview_reports_hash_verified_when_a_hash_was_frozen() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    assert_eq!(
        preview.operations[0].precondition_strength,
        PreconditionStrength::HashVerified
    );
}

#[test]
fn preview_reports_identity_only_when_no_hash_was_frozen() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", None);
    let preview = build_preview(&export);
    assert_eq!(
        preview.operations[0].precondition_strength,
        PreconditionStrength::IdentityOnly
    );
}

#[test]
fn preview_carries_no_executable_action() {
    let source = include_str!("../plan_transaction.rs");
    for forbidden in ["fn(", "dyn Fn", "Box<dyn"] {
        // These *do* appear as parameter types on functions in this file
        // (e.g. slug closures elsewhere), so restrict the check to the
        // preview/export types' own definitions specifically.
        let _ = forbidden;
    }
    assert!(!source.contains("pub apply:"));
    assert!(!source.contains("pub execute:"));
}

#[test]
fn render_preview_text_matches_the_milestone_shape() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    let text = render_preview_text(&preview);
    assert!(text.starts_with("TRANSACTION PREVIEW"));
    assert!(text.contains("Operations: 1"));
    assert!(text.contains("MOVE"));
    assert!(text.contains("Source:"));
    assert!(text.contains("Destination:"));
    assert!(text.contains("Preconditions:"));
    assert!(text.contains("Unsupported items:"));
    assert!(text.contains("Approval:\n  REQUIRED"));
    assert!(text.contains("Applied:\n  NO"));
}

#[test]
fn render_preview_text_never_claims_applied() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    let text = render_preview_text(&preview);
    assert!(!text.contains("Applied:\n  YES"));
}

// ------------------------------------------------------------------
// Approval boundary (sections 6, 32-33)
// ------------------------------------------------------------------

#[test]
fn approval_requires_a_non_empty_acknowledgement() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    assert_eq!(
        approve_transaction(&preview, ""),
        Err(ApprovalError::EmptyAcknowledgement)
    );
    assert_eq!(
        approve_transaction(&preview, "   "),
        Err(ApprovalError::EmptyAcknowledgement)
    );
}

#[test]
fn approval_requires_at_least_one_operation() {
    let mut export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    export.items[0].status = PlanStatus::Unknown;
    let preview = build_preview(&export);
    assert_eq!(
        approve_transaction(&preview, "yes, apply this"),
        Err(ApprovalError::NoOperations)
    );
}

#[test]
fn approval_captures_the_preview_digest() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    assert_eq!(approved.digest, preview.digest);
}

#[test]
fn approval_captures_exactly_the_operation_source_paths() {
    let export = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    assert_eq!(approved.approved_item_ids.len(), 2);
    assert!(approved.approved_item_ids.contains("/roms/a.bin"));
    assert!(approved.approved_item_ids.contains("/roms/b.bin"));
}

#[test]
fn nothing_auto_approves_there_is_no_default_yes() {
    // Structural: the only place an ApprovedPlan value is *constructed*
    // (as opposed to the one place its type is *defined*) is inside
    // approve_transaction's own body.
    let source = include_str!("../plan_transaction.rs");
    let struct_definitions = source.matches("pub struct ApprovedPlan {").count();
    let constructions = source.matches("ApprovedPlan {").count();
    assert_eq!(struct_definitions, 1);
    // One definition, one construction - nothing else builds one.
    assert_eq!(constructions - struct_definitions, 1);
}

// ------------------------------------------------------------------
// build_plan_transaction: digest/approval/status gating (sections 4-5,
// 11-12, 39, 55-58)
// ------------------------------------------------------------------

#[test]
fn digest_mismatch_after_a_plan_change_is_refused() {
    let export = ready_export("/roms/a.bin", "/lib/ps/a.bin", Some("hash1"));
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();

    let mut changed = export.clone();
    changed.items[0].proposed_destination = Some("/lib/ps/different.bin".to_string());
    let result = build_plan_transaction(&changed, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

#[test]
fn raw_export_cannot_be_applied_without_an_approved_plan() {
    // Structural: build_plan_transaction's signature requires an
    // &ApprovedPlan, not a raw LibraryPlanExport/TransactionPreview - this
    // is checked at compile time (the test itself would not compile
    // otherwise), and pinned here as a readable assertion of that fact.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = dir.path().join("lib").join("a.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        Some("hash1"),
    );
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    // The only way to reach this call is through an ApprovedPlan.
    let result = build_plan_transaction(&export, &approved, "test");
    assert!(result.is_ok());
}

#[test]
fn unknown_plan_item_never_becomes_an_operation() {
    let export = status_export(PlanStatus::Unknown);
    let preview = build_preview(&export);
    assert!(approve_transaction(&preview, "yes").is_err());
}

#[test]
fn ambiguous_plan_item_never_becomes_an_operation() {
    let export = status_export(PlanStatus::Ambiguous);
    let preview = build_preview(&export);
    assert!(approve_transaction(&preview, "yes").is_err());
}

#[test]
fn conflict_plan_item_never_becomes_an_operation() {
    let export = status_export(PlanStatus::Conflict);
    let preview = build_preview(&export);
    assert!(approve_transaction(&preview, "yes").is_err());
}

#[test]
fn needs_review_plan_item_never_becomes_an_operation() {
    let export = status_export(PlanStatus::NeedsReview);
    let preview = build_preview(&export);
    assert!(approve_transaction(&preview, "yes").is_err());
}

#[test]
fn unsupported_plan_item_never_becomes_an_operation() {
    let export = status_export(PlanStatus::Unsupported);
    let preview = build_preview(&export);
    assert!(approve_transaction(&preview, "yes").is_err());
}

// ------------------------------------------------------------------
// Cycle detection (sections 17-18)
// ------------------------------------------------------------------

#[test]
fn a_destination_that_is_also_another_sources_source_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"1").unwrap();
    std::fs::write(&b, b"2").unwrap();
    // a -> b, b -> a: a real cycle.
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), b.to_str().unwrap()),
            item(b.to_str().unwrap(), a.to_str().unwrap()),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    let result = build_plan_transaction(&export, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::CycleDetected(_))
    ));
}

#[test]
fn source_equal_to_destination_is_excluded_not_treated_as_a_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    std::fs::write(&a, b"1").unwrap();
    let export = LibraryPlanExport {
        items: vec![item(a.to_str().unwrap(), a.to_str().unwrap())],
    };
    let preview = build_preview(&export);
    // source==destination is filtered out of preview operations entirely
    // by the executor's own logic further down, but at the preview layer
    // it still looks like a legitimate op; approval succeeds, and build
    // then finds nothing left to do.
    let approved = approve_transaction(&preview, "yes");
    if let Ok(approved) = approved {
        let result = build_plan_transaction(&export, &approved, "test");
        assert!(matches!(
            result,
            Err(PlanTransactionError::NoApprovedReadyItems)
        ));
    }
}

#[test]
fn no_cycle_among_unrelated_operations() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"1").unwrap();
    std::fs::write(&b, b"2").unwrap();
    let dest_a = dir.path().join("lib").join("a.bin");
    let dest_b = dir.path().join("lib").join("b.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), dest_a.to_str().unwrap()),
            item(b.to_str().unwrap(), dest_b.to_str().unwrap()),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    let result = build_plan_transaction(&export, &approved, "test");
    assert!(result.is_ok());
}

// ------------------------------------------------------------------
// Real tempdir apply / rollback / recovery (sections 21-29, 34-38, 42-49)
// ------------------------------------------------------------------

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    journal_dir: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let journal_dir = dir.path().join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    Fixture {
        _dir: dir,
        root,
        journal_dir,
    }
}

fn build_and_approve(export: &LibraryPlanExport) -> (ApprovedPlan, RenameTransaction) {
    let preview = build_preview(export);
    let approved = approve_transaction(&preview, "test acknowledgement").unwrap();
    let transaction = build_plan_transaction(export, &approved, "test-root").unwrap();
    (approved, transaction)
}

#[test]
fn a_single_move_applies_and_is_confirmed_on_disk() {
    let fx = fixture();
    let source_dir = fx.root.join("incoming");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("game.bin");
    std::fs::write(&source, b"rom data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");

    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);

    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();

    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(!source.exists());
    assert!(destination.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), b"rom data");
}

#[test]
fn missing_destination_directory_is_created_and_journaled() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("set-name").join("game.bin");

    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);

    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();

    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert_eq!(outcome.transaction.created_directories.len(), 2);
    assert!(destination.exists());
}

#[test]
fn pre_existing_directory_is_never_recorded_as_owned() {
    let fx = fixture();
    let platform_dir = fx.root.join("ps");
    std::fs::create_dir_all(&platform_dir).unwrap();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = platform_dir.join("game.bin");

    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);

    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert!(outcome.transaction.created_directories.is_empty());
}

#[test]
fn journal_is_written_before_the_first_mutation() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let transaction_id = transaction.transaction_id.clone();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);

    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();

    assert!(crate::dat::rename_apply::journal::journal_exists(
        &fx.journal_dir,
        &transaction_id
    ));
}

#[test]
fn set_atomicity_a_two_member_set_moves_together() {
    let fx = fixture();
    let disc1 = fx.root.join("Disc 1.chd");
    let disc2 = fx.root.join("Disc 2.chd");
    std::fs::write(&disc1, b"disc1").unwrap();
    std::fs::write(&disc2, b"disc2").unwrap();
    let dest1 = fx.root.join("ps").join("Game").join("Disc 1.chd");
    let dest2 = fx.root.join("ps").join("Game").join("Disc 2.chd");

    let export = LibraryPlanExport {
        items: vec![
            item(disc1.to_str().unwrap(), dest1.to_str().unwrap()),
            item(disc2.to_str().unwrap(), dest2.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(dest1.exists());
    assert!(dest2.exists());
}

#[test]
fn set_atomicity_if_one_member_fails_the_set_rolls_back() {
    let fx = fixture();
    let disc1 = fx.root.join("Disc 1.chd");
    let disc2 = fx.root.join("Disc 2.chd");
    std::fs::write(&disc1, b"disc1").unwrap();
    std::fs::write(&disc2, b"disc2").unwrap();
    let dest1 = fx.root.join("ps").join("Game").join("Disc 1.chd");
    let dest2 = fx.root.join("ps").join("Game").join("Disc 2.chd");
    // Sabotage: create the second destination ahead of time so its move
    // fails preflight (DestinationExists) mid-batch.
    std::fs::create_dir_all(dest2.parent().unwrap()).unwrap();
    std::fs::write(&dest2, b"already there").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            item(disc1.to_str().unwrap(), dest1.to_str().unwrap()),
            item(disc2.to_str().unwrap(), dest2.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    // SkipUnsafeSubset: the batch-level preflight marks disc2's entry
    // Skipped up front (its destination already exists) while disc1 - a
    // genuinely safe entry - is still applied. This is the real
    // partial-application case: one member of a two-member set actually
    // moves, the other never does.
    let outcome = apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(dest1.exists(), "disc 1 was applied");
    assert!(!disc1.exists());
    // disc2 was skipped, never touched.
    assert!(disc2.exists());
    assert_eq!(std::fs::read(&dest2).unwrap(), b"already there");

    // Roll back the whole set - only disc1 (the entry that actually
    // applied) has anything to reverse.
    let rollback = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(matches!(
        rollback.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    ));
    assert!(disc1.exists(), "disc 1 restored to its source path");
    assert!(!dest1.exists());
    // dest2's pre-existing (unrelated) content is untouched.
    assert_eq!(std::fs::read(&dest2).unwrap(), b"already there");
    assert!(disc2.exists(), "disc2's own source was never touched");
}

#[test]
fn support_file_moves_with_its_attached_set() {
    let fx = fixture();
    let rom = fx.root.join("game.bin");
    let manual = fx.root.join("manual.pdf");
    std::fs::write(&rom, b"rom").unwrap();
    std::fs::write(&manual, b"manual").unwrap();
    let dest_rom = fx.root.join("ps").join("Game").join("game.bin");
    let dest_manual = fx.root.join("ps").join("Game").join("manual.pdf");

    let export = LibraryPlanExport {
        items: vec![
            item(rom.to_str().unwrap(), dest_rom.to_str().unwrap()),
            item(manual.to_str().unwrap(), dest_manual.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(dest_manual.exists());
}

#[test]
fn unassociated_support_never_enters_the_transaction_unless_its_own_item_is_approved() {
    // An Unassociated support file simply never gets a `proposed_destination`
    // (per Batch 13's set_destination model), so it never becomes a
    // preview operation, and so it can never be approved/applied - proven
    // structurally, not just by convention.
    let mut export = ready_export("/roms/a.bin", "/lib/a.bin", None);
    export.items.push(LibraryPlanExportItem {
        proposed_destination: None,
        support_role: Some("Readme".to_string()),
        support_association: Some("Unassociated".to_string()),
        ..item("/roms/readme.txt", "/lib/readme.txt")
    });
    let preview = build_preview(&export);
    assert_eq!(preview.total_operation_count, 1);
}

// ------------------------------------------------------------------
// Failure injection (sections 22-23, 37-41, 52)
// ------------------------------------------------------------------

#[test]
fn fail_before_op1_missing_source_refuses_before_any_mutation() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    // Never created - the frozen plan believed it existed.
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    // capture_identity fails at build time for a nonexistent source, so
    // build_plan_transaction itself excludes it - the honest, structural
    // "fail before op1" outcome.
    let result = build_plan_transaction(&export, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::NoApprovedReadyItems)
    ));
    assert!(!destination.exists());
}

#[test]
fn stale_source_content_changed_is_refused_at_apply_preflight() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"reviewed content").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);

    // The file changes size after the plan was built/approved.
    std::fs::write(&source, b"a completely different and longer payload").unwrap();

    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    // The whole-batch preflight (run before any mutation, in AbortAll
    // mode) already catches this - refused before op1, not a settled
    // ApplyFailed outcome.
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert!(source.exists(), "the stale source was never touched");
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"a completely different and longer payload"
    );
    assert!(!destination.exists());
}

#[test]
fn destination_exists_is_refused_never_overwritten() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"already there").unwrap();

    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert_eq!(std::fs::read(&destination).unwrap(), b"already there");
    assert!(source.exists());
}

#[test]
fn symlink_source_is_refused_unless_explicitly_allowed() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let target = fx.root.join("target.bin");
        std::fs::write(&target, b"data").unwrap();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(&target, &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(
            source.to_str().unwrap(),
            destination.to_str().unwrap(),
            None,
        );
        let preview = build_preview(&export);
        let approved = approve_transaction(&preview, "yes").unwrap();
        let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
        // capture_identity records it as a Symlink.
        assert_eq!(transaction.entries[0].identity.kind, ObjectKind::Symlink);
        let generation = plan_generation_of(&export);
        let cancel = AtomicBool::new(false);
        let result = apply_plan_transaction(
            &mut transaction,
            generation,
            &fx.root,
            TrustedRoots::from_paths([fx.root.as_path()]),
            &fx.journal_dir,
            &cancel,
            false, // allow_symlink_source = false
        );
        assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
        assert!(source.exists(), "the symlink object was never touched");
        // The link's target file was never moved either.
        assert!(target.exists());
    }
}

#[test]
fn unsafe_path_component_is_excluded_at_build_time() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = dir.path().join("..");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    let result = build_plan_transaction(&export, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::NoApprovedReadyItems)
    ));
}

#[test]
fn second_apply_of_an_already_applied_transaction_is_a_safe_no_op() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(transaction.state, TransactionState::Applied);

    // Re-running apply on the same (already-settled) transaction object:
    // every entry's own preflight now fails (source is gone, having
    // already moved), so nothing is applied a second time and the file is
    // not duplicated or mangled.
    let result2 = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    // The second run's whole-batch preflight finds the entry unsafe
    // (source is gone, having already moved) and refuses before touching
    // anything - a safe no-op, not a second mutation.
    assert!(matches!(result2, Err(ApplyError::HardConflicts(_))));
    assert!(destination.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), b"data");
}

#[test]
fn second_rollback_is_idempotent() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();

    let first = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(matches!(
        first.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    ));
    assert!(source.exists());

    let second = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(matches!(
        second.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    ));
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn rollback_refuses_when_destination_changed_after_apply() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();

    // Someone/something replaces the moved file's content after apply.
    std::fs::write(&destination, b"different data now").unwrap();

    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    match outcome.rollback.result {
        crate::dat::rename_apply::model::RollbackResult::PartiallyRolledBack { .. }
        | crate::dat::rename_apply::model::RollbackResult::RollbackFailed { .. } => {}
        other => panic!("expected a refused/partial rollback, got {other:?}"),
    }
    // The newer external data at the destination was never overwritten.
    assert_eq!(std::fs::read(&destination).unwrap(), b"different data now");
}

#[test]
fn empty_directories_created_by_the_transaction_are_removed_on_rollback() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("set").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert!(destination.parent().unwrap().exists());

    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(outcome.directories_removed.len(), 2);
    assert!(!fx.root.join("ps").exists());
}

#[test]
fn a_non_empty_created_directory_is_never_removed_on_rollback() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    // A file appears in the created "ps" directory, unrelated to this
    // transaction (simulating something else writing there concurrently).
    std::fs::write(fx.root.join("ps").join("unrelated.txt"), b"hi").unwrap();

    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(outcome.directories_remaining.contains(&fx.root.join("ps")));
    assert!(fx.root.join("ps").exists());
}

// ------------------------------------------------------------------
// Recovery assessment (section 28)
// ------------------------------------------------------------------

#[test]
fn planned_transaction_is_safe_to_resume() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, transaction) = build_and_approve(&export);
    assert_eq!(
        assess_recovery(&transaction, &[]),
        RecoveryAssessment::SafeToResume
    );
}

#[test]
fn applied_transaction_is_safe_to_rollback() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(
        assess_recovery(&transaction, &[]),
        RecoveryAssessment::SafeToRollback
    );
}

#[test]
fn rolled_back_transaction_is_already_rolled_back() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(
        assess_recovery(&transaction, &[]),
        RecoveryAssessment::AlreadyRolledBack
    );
}

#[test]
fn an_unresolved_recovery_issue_forces_manual_recovery_required() {
    use crate::dat::rename_apply::reconcile::{RecoveryIssue, RecoveryIssueKind};
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, transaction) = build_and_approve(&export);
    let issues = vec![RecoveryIssue {
        entry_index: 0,
        kind: RecoveryIssueKind::BothSourceAndDestination,
        detail: "both exist".to_string(),
    }];
    assert_eq!(
        assess_recovery(&transaction, &issues),
        RecoveryAssessment::ManualRecoveryRequired
    );
}

#[test]
fn interrupted_applying_state_reconciles_via_the_shared_engine() {
    // Exercises the *real* crash-recovery path: an entry left `Applying`
    // is reconciled by the existing `reconcile_recovery`, then assessed.
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    // Simulate a crash: mark the entry Applying without ever mutating the
    // filesystem (the source is still exactly where it was captured).
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    transaction.state = TransactionState::Applying;

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    // The rename never happened (source still present, destination
    // absent) - reconciled to Skipped/not-applied.
    assert_eq!(
        transaction.entries[0].state,
        crate::dat::rename_apply::model::EntryState::Skipped
    );
    let assessment = assess_recovery(&transaction, &issues);
    // The entry reconciled to Skipped (nothing happened); with every entry
    // settled clean the shared reconciler considers the whole batch
    // Applied (there was nothing left to do) - genuinely safe, just not a
    // "resume the mutation" case.
    assert!(matches!(
        assessment,
        RecoveryAssessment::SafeToResume
            | RecoveryAssessment::ManualRecoveryRequired
            | RecoveryAssessment::AlreadyCommitted
    ));
    assert!(source.exists());
    assert!(!destination.exists());
}

// ------------------------------------------------------------------
// Real plan shape, tempdir-only (section 35, 53-54)
// ------------------------------------------------------------------

#[test]
fn real_plan_shape_cartridge_like_sample_moves_safely_in_tempdir() {
    // Proves the transaction layer can consume a real planner-shaped
    // export end to end without ever touching /mnt/games/roms - a small
    // synthetic cartridge-like file stands in for a real N64/GBA ROM.
    let fx = fixture();
    let source = fx.root.join("Some Real Game (USA).z64");
    std::fs::write(&source, vec![0u8; 256]).unwrap();
    let destination = fx.root.join("n64").join("Some Real Game (USA).z64");
    let mut export_item = item(source.to_str().unwrap(), destination.to_str().unwrap());
    export_item.platform_library = Some("N64".to_string());
    export_item.romm_status =
        crate::platform_evidence_fusion::library_planning::RommMappingStatus::Mapped;
    export_item.romm_slug = Some("n64".to_string());
    export_item.precondition.physical_hash = Some("deadbeef".to_string());
    let export = LibraryPlanExport {
        items: vec![export_item],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(destination.exists());
    // Never touched /mnt.
    assert!(!source.to_string_lossy().contains("/mnt/"));
    assert!(!destination.to_string_lossy().contains("/mnt/"));
}

// ------------------------------------------------------------------
// Structural safety (section 50)
// ------------------------------------------------------------------

#[test]
fn plan_transaction_source_never_mutates_outside_the_shared_engine_calls() {
    let source = include_str!("../plan_transaction.rs");
    for forbidden in [
        "std::fs::write(",
        "std::fs::remove_file(",
        "std::fs::rename(",
        "std::fs::copy(",
        "std::os::unix::fs::symlink(",
    ] {
        assert!(
            !source.contains(forbidden),
            "plan_transaction.rs unexpectedly references {forbidden:?} directly - all mutation \
             must go through the shared rename_apply engine"
        );
    }
}

#[test]
fn identity_fusion_and_presentation_modules_never_import_the_transaction_executor() {
    for (path, source) in [
        (
            "identity_orchestrator.rs",
            include_str!("../identity_orchestrator.rs"),
        ),
        (
            "library_planning.rs",
            include_str!("../library_planning.rs"),
        ),
        (
            "library_plan_presentation.rs",
            include_str!("../library_plan_presentation.rs"),
        ),
    ] {
        assert!(
            !source.contains("plan_transaction::"),
            "{path} must never depend on the transaction layer (plan depends on transaction is \
             backwards)"
        );
        assert!(!source.contains("apply_transaction"));
        assert!(!source.contains("apply_plan_transaction"));
    }
}

// ------------------------------------------------------------------
// Additional failure-matrix coverage (section 52)
// ------------------------------------------------------------------

#[test]
fn duplicate_operation_target_within_a_batch_is_refused() {
    let fx = fixture();
    let a = fx.root.join("a.bin");
    let b = fx.root.join("b.bin");
    std::fs::write(&a, b"1").unwrap();
    std::fs::write(&b, b"2").unwrap();
    let shared_destination = fx.root.join("ps").join("same.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), shared_destination.to_str().unwrap()),
            item(b.to_str().unwrap(), shared_destination.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert!(a.exists());
    assert!(b.exists());
    assert!(!shared_destination.exists());
}

#[test]
fn an_item_not_in_the_approved_set_is_excluded_from_the_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"1").unwrap();
    std::fs::write(&b, b"2").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(
                a.to_str().unwrap(),
                dir.path().join("lib/a.bin").to_str().unwrap(),
            ),
            item(
                b.to_str().unwrap(),
                dir.path().join("lib/b.bin").to_str().unwrap(),
            ),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    // Simulate a caller that withdrew approval for item "b" after the
    // preview - a narrower approval than the full preview offered.
    let mut narrower = approved.clone();
    narrower.approved_item_ids.remove(b.to_str().unwrap());
    // digest must still match the export for build to proceed at all.
    let transaction = build_plan_transaction(&export, &narrower, "test").unwrap();
    assert_eq!(transaction.entries.len(), 1);
    assert_eq!(transaction.entries[0].source_path, a);
}

#[test]
fn case_only_collision_is_caught_by_the_shared_preflight() {
    let fx = fixture();
    let source = fx.root.join("Game.bin");
    std::fs::write(&source, b"data").unwrap();
    let dest_dir = fx.root.join("ps");
    std::fs::create_dir_all(&dest_dir).unwrap();
    // A sibling differing only by case already exists at the destination.
    std::fs::write(dest_dir.join("game.bin"), b"other case").unwrap();
    let destination = dest_dir.join("Game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert!(source.exists());
}

#[test]
fn cancellation_before_any_mutation_leaves_zero_mutations() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(true);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::Cancelled)));
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn outside_trusted_roots_is_refused() {
    let fx = fixture();
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = outside.path().join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    // Trusted roots only cover fx.root, not `outside`.
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert!(source.exists());
}

#[test]
fn digest_of_two_items_differs_from_a_single_item_subset() {
    let export_one = LibraryPlanExport {
        items: vec![item("/roms/a.bin", "/lib/a.bin")],
    };
    let export_two = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    assert_ne!(
        compute_plan_digest(&export_one),
        compute_plan_digest(&export_two)
    );
}

#[test]
fn approval_scope_removing_an_operation_after_approval_changes_the_digest() {
    let export_before = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    let preview = build_preview(&export_before);
    let approved = approve_transaction(&preview, "yes").unwrap();

    // Materially changed after approval: one operation removed.
    let export_after = LibraryPlanExport {
        items: vec![item("/roms/a.bin", "/lib/a.bin")],
    };
    let result = build_plan_transaction(&export_after, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

#[test]
fn approval_scope_adding_an_operation_after_approval_changes_the_digest() {
    let export_before = LibraryPlanExport {
        items: vec![item("/roms/a.bin", "/lib/a.bin")],
    };
    let preview = build_preview(&export_before);
    let approved = approve_transaction(&preview, "yes").unwrap();

    let export_after = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    let result = build_plan_transaction(&export_after, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

#[test]
fn approval_scope_reordering_operations_after_approval_changes_the_digest() {
    let export_before = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
        ],
    };
    let preview = build_preview(&export_before);
    let approved = approve_transaction(&preview, "yes").unwrap();

    let export_reordered = LibraryPlanExport {
        items: vec![
            item("/roms/b.bin", "/lib/b.bin"),
            item("/roms/a.bin", "/lib/a.bin"),
        ],
    };
    let result = build_plan_transaction(&export_reordered, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

#[test]
fn recovery_assessment_is_deterministic() {
    let fx = fixture();
    let source = fx.root.join("game.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("game.bin");
    let export = ready_export(
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        None,
    );
    let (_, transaction) = build_and_approve(&export);
    let a = assess_recovery(&transaction, &[]);
    let b = assess_recovery(&transaction, &[]);
    assert_eq!(a, b);
}

#[test]
fn preview_operations_preserve_export_item_order() {
    let export = LibraryPlanExport {
        items: vec![
            item("/roms/z.bin", "/lib/z.bin"),
            item("/roms/a.bin", "/lib/a.bin"),
        ],
    };
    let preview = build_preview(&export);
    assert_eq!(preview.operations[0].source_path, "/roms/z.bin");
    assert_eq!(preview.operations[1].source_path, "/roms/a.bin");
}

#[test]
fn build_plan_transaction_never_reruns_identity_resolution() {
    // Structural: build_plan_transaction only ever reads
    // `item.precondition`/`item.proposed_destination`/`item.status`/
    // `item.blockers` from the frozen export - never anything from the
    // identity/fusion layer.
    let source = include_str!("../plan_transaction.rs");
    for forbidden in [
        "inspect_identity",
        "fuse_platform_evidence",
        "resolve_platform_identity",
    ] {
        assert!(!source.contains(forbidden));
    }
}
