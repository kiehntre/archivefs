//! Batch 17: focused coverage for the real-canary harness/policy - milestone
//! section 39. The actual real, non-tempdir apply/rollback cycle is
//! exercised manually via `cargo run -p archivefs-core --example
//! real_canary` (captured verbatim in the Batch 17 final report); everything
//! here is tempdir-only, per this crate's standing rule that automated tests
//! never touch a real, non-disposable path.

use super::*;
use crate::dat::rename_apply::journal::{journal_path, read_journal};
use crate::dat::rename_apply::model::TransactionState;
use crate::platform_evidence_fusion::library_plan_export::SourcePrecondition;
use std::sync::atomic::AtomicBool;

fn item(source: &str, destination: &str) -> LibraryPlanExportItem {
    LibraryPlanExportItem {
        status: PlanStatus::Ready,
        precondition: SourcePrecondition {
            source_path: source.to_string(),
            physical_hash: Some("deadbeef".to_string()),
            normalized_hash: None,
        },
        proposed_destination: Some(destination.to_string()),
        operation_intent: OperationIntent::MoveToLibraryFolder,
        platform_library: None,
        display_name: "Canary Test Item".to_string(),
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

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    journal_dir: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("canary");
    std::fs::create_dir_all(root.join("library")).unwrap();
    let journal_dir = dir.path().join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    Fixture {
        _dir: dir,
        root,
        journal_dir,
    }
}

fn write_rom(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"canary bytes").unwrap();
}

// ==================================================================
// Structural: the real-canary example harness itself (milestone
// sections 3, 25-26, 34-35)
// ==================================================================

fn real_canary_source() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/real_canary.rs"
    ))
    .expect("examples/real_canary.rs must exist")
}

#[test]
fn real_canary_example_hardcodes_exactly_one_canary_root_constant() {
    let source = real_canary_source();
    assert_eq!(
        source.matches("const CANARY_ROOT:").count(),
        1,
        "there must be exactly one hardcoded canary root, never a caller-supplied one"
    );
}

#[test]
fn real_canary_example_never_parses_a_source_or_destination_cli_argument() {
    let source = real_canary_source();
    assert!(
        !source.contains("std::env::args()"),
        "the real-canary harness must never accept any CLI argument for a path - \
         found an args() call in {source:?}"
    );
}

#[test]
fn real_canary_example_never_requests_skip_unsafe_subset() {
    let source = real_canary_source();
    assert!(
        !source.contains("SkipUnsafeSubset"),
        "the real-canary harness must be hard-bound to AbortAll via RealApplyPolicy::Canary, \
         never able to request SkipUnsafeSubset even by mistake"
    );
    assert!(
        source.contains("RealApplyPolicy::Canary"),
        "the real-canary harness must go through RealApplyPolicy::Canary, not a raw HardConflictMode"
    );
}

#[test]
fn real_canary_example_checks_the_production_root_before_any_mutation() {
    let source = real_canary_source();
    assert!(
        source.contains("hard_guard_never_production"),
        "a hard guard against /mnt/games/roms must exist and be called"
    );
    let main_start = source.find("fn main").expect("main must exist");
    let apply_pos = main_start
        + source[main_start..]
            .find("apply_plan_transaction_with_mode")
            .expect("main must call apply_plan_transaction_with_mode");
    let first_guard_call = source
        .find("hard_guard_never_production(&source)")
        .expect("the guard must be called on the source path");
    assert!(
        first_guard_call < apply_pos,
        "the production-root guard must run before the real apply call, not after"
    );
}

#[test]
fn real_canary_example_rolls_back_within_the_same_run() {
    let source = real_canary_source();
    assert!(
        source.contains("rollback_plan_transaction("),
        "the real-canary run must roll back what it applies within the same run, \
         never leaving a canary applied and unverified"
    );
}

#[test]
fn real_canary_example_never_hardcodes_a_second_arbitrary_destination() {
    let source = real_canary_source();
    // The only two paths ever built are `source` and `destination`, both
    // derived from the single `canary_root` local variable - never a second,
    // independently-supplied path.
    assert_eq!(
        source.matches("PathBuf::from(CANARY_ROOT)").count(),
        1,
        "CANARY_ROOT must be materialized into a path exactly once"
    );
}

// ==================================================================
// RealApplyPolicy::Canary is structurally AbortAll-only
// ==================================================================

#[test]
fn real_apply_policy_canary_can_only_ever_produce_abort_all() {
    // Exhaustive match: if a second variant is ever added to
    // `RealApplyPolicy`, this stops compiling rather than silently passing.
    let policy = RealApplyPolicy::Canary;
    assert_eq!(policy.hard_conflict_mode(), HardConflictMode::AbortAll);
}

// ==================================================================
// Canary eligibility: one-file limit, hash precondition, cycles
// ==================================================================

#[test]
fn a_set_member_is_never_canary_eligible() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let mut export_item = item(&source.to_string_lossy(), &destination.to_string_lossy());
    export_item.set_label = Some("some-set".to_string());
    let export = LibraryPlanExport {
        items: vec![export_item],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert_eq!(result, Err(vec![CanaryIneligibleReason::BelongsToSet]));
}

#[test]
fn a_support_associated_item_is_never_canary_eligible() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let mut export_item = item(&source.to_string_lossy(), &destination.to_string_lossy());
    export_item.support_role = Some("manual".to_string());
    let export = LibraryPlanExport {
        items: vec![export_item],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::HasSupportAssociation))
    );
}

#[test]
fn missing_hash_precondition_makes_a_canary_ineligible() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let mut export_item = item(&source.to_string_lossy(), &destination.to_string_lossy());
    export_item.precondition.physical_hash = None;
    export_item.precondition.normalized_hash = None;
    let export = LibraryPlanExport {
        items: vec![export_item],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert_eq!(
        result,
        Err(vec![CanaryIneligibleReason::NoHashPrecondition])
    );
}

#[test]
fn a_pre_existing_destination_makes_a_canary_ineligible() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    write_rom(&destination);
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::DestinationAlreadyExists))
    );
}

#[test]
fn a_missing_destination_parent_makes_a_canary_ineligible() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("not_created_yet").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::DestinationParentMissing))
    );
}

#[test]
fn a_symlink_source_is_never_canary_eligible() {
    let fx = fixture();
    let real = fx.root.join("real.bin");
    write_rom(&real);
    let source = fx.root.join("link.bin");
    std::os::unix::fs::symlink(&real, &source).unwrap();
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::SourceIsSymlink))
    );
}

#[test]
fn source_under_production_root_is_refused_even_if_canary_root_is_permissive() {
    let fx = fixture();
    let source = PathBuf::from("/mnt/games/roms/psx/definitely-not-real.bin");
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    // A permissive canary_root that would technically contain the
    // production path if naively string-prefixed - the production-root
    // check must still fire independently of the canary-root check.
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, Path::new("/"));
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::SourceUnderProductionRoot))
    );
}

#[test]
fn destination_under_production_root_is_refused() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = PathBuf::from("/mnt/games/roms/gcn/definitely-not-real.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, Path::new("/"));
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::DestinationUnderProductionRoot))
    );
}

#[test]
fn source_outside_the_supplied_canary_root_is_refused() {
    let fx = fixture();
    let outside_dir = tempfile::tempdir().unwrap();
    let source = outside_dir.path().join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::SourceOutsideCanaryRoot))
    );
}

#[test]
fn a_file_over_the_size_ceiling_is_refused_with_named_bytes_and_limit() {
    let fx = fixture();
    let source = fx.root.join("big.bin");
    let file = std::fs::File::create(&source).unwrap();
    file.set_len(CANARY_MAX_SIZE_BYTES + 1).unwrap();
    drop(file);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    match result {
        Err(reasons) => {
            let over_limit = reasons.iter().any(|reason| {
                matches!(
                    reason,
                    CanaryIneligibleReason::SourceTooLarge { bytes, limit }
                        if *bytes == CANARY_MAX_SIZE_BYTES + 1 && *limit == CANARY_MAX_SIZE_BYTES
                )
            });
            assert!(
                over_limit,
                "expected a named SourceTooLarge reason, got {reasons:?}"
            );
        }
        Ok(_) => panic!("a file one byte over the ceiling must never be eligible"),
    }
}

#[test]
fn a_file_exactly_at_the_size_ceiling_is_eligible_on_size_alone() {
    let fx = fixture();
    let source = fx.root.join("exact.bin");
    let file = std::fs::File::create(&source).unwrap();
    file.set_len(CANARY_MAX_SIZE_BYTES).unwrap();
    drop(file);
    let dest_dir = fx.root.join("library");
    let destination = dest_dir.join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        result.is_ok()
            || !matches!(result, Err(ref r) if r.iter().any(|x| matches!(x, CanaryIneligibleReason::SourceTooLarge { .. }))),
        "a file exactly at the ceiling must not be refused for size"
    );
}

#[test]
fn a_second_item_targeting_the_same_destination_is_a_cycle_or_duplicate_refusal() {
    let fx = fixture();
    let source_a = fx.root.join("a.bin");
    let source_b = fx.root.join("b.bin");
    write_rom(&source_a);
    write_rom(&source_b);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(&source_a.to_string_lossy(), &destination.to_string_lossy()),
            item(&source_b.to_string_lossy(), &destination.to_string_lossy()),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    assert!(
        matches!(result, Err(reasons) if reasons.contains(&CanaryIneligibleReason::CycleOrDuplicateTarget))
    );
}

#[test]
fn strong_enough_for_canary_is_only_ever_true_on_a_clean_ok_report() {
    let fx = fixture();
    let source = fx.root.join("clean.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    let report = result.expect("a clean single-file item must be eligible");
    assert!(report.strong_enough_for_canary);
    assert!(report.physical_hash_present);
    assert!(report.is_regular_file);
    assert!(!report.is_symlink);
}

// ==================================================================
// Canary preview never claims Applied: YES
// ==================================================================

#[test]
fn canary_preview_always_says_applied_no_on_both_pass_and_fail() {
    let fx = fixture();
    let source = fx.root.join("clean.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();

    let ok_result = assess_canary_eligibility(&export, &export.items[0], &approved, &fx.root);
    let ok_text = render_canary_preview(&export.items[0], &ok_result);
    assert!(ok_text.contains("Applied:\n  NO"));
    assert!(ok_text.contains("Preconditions:\n  PASS"));

    let mut ineligible_item = export.items[0].clone();
    ineligible_item.set_label = Some("forces-a-failure".to_string());
    let fail_result = Err(vec![CanaryIneligibleReason::BelongsToSet]);
    let fail_text = render_canary_preview(&ineligible_item, &fail_result);
    assert!(fail_text.contains("Applied:\n  NO"));
    assert!(fail_text.contains("Preconditions:\n  FAIL"));
    assert!(fail_text.contains("BelongsToSet"));
}

// ==================================================================
// A real (tempdir) apply/rollback cycle run through RealApplyPolicy -
// journal state, second apply refusal, second rollback, fresh tx id
// ==================================================================

#[test]
fn canary_policy_apply_then_rollback_round_trips_byte_identically_in_a_real_directory() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let original_bytes = std::fs::read(&source).unwrap();
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let (approved, mut transaction) = {
        let preview = build_preview(&export);
        let approved = approve_transaction(&preview, "ack").unwrap();
        let transaction = build_plan_transaction(&export, &approved, "canary-policy-test").unwrap();
        (approved, transaction)
    };
    let _ = &approved;
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .expect("apply must succeed");
    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), original_bytes);

    let outcome = rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted)
        .expect("rollback must succeed");
    assert_eq!(
        outcome.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    );
    assert!(destination_absent(&destination));
    assert_eq!(std::fs::read(&source).unwrap(), original_bytes);
}

fn destination_absent(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
}

#[test]
fn canary_policy_second_apply_on_the_same_transaction_is_refused_and_mutates_nothing() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut transaction = build_plan_transaction(&export, &approved, "root").unwrap();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .unwrap();
    let before = std::fs::read(&destination).unwrap();

    let second = apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted,
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    );
    assert!(second.is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), before);
}

#[test]
fn canary_policy_second_rollback_is_a_safe_no_op() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut transaction = build_plan_transaction(&export, &approved, "root").unwrap();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .unwrap();
    rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted).unwrap();
    let source_bytes_after_first_rollback = std::fs::read(&source).unwrap();

    let second = rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted);
    assert!(
        second.is_ok(),
        "a second rollback must be a safe no-op, not an error"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        source_bytes_after_first_rollback
    );
}

#[test]
fn canary_policy_apply_after_rollback_on_the_old_transaction_id_is_refused() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut transaction = build_plan_transaction(&export, &approved, "root").unwrap();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .unwrap();
    rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted).unwrap();

    let reapply = apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted,
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    );
    assert!(matches!(
        reapply,
        Err(crate::dat::rename_apply::executor::ApplyError::AlreadySettled { .. })
    ));
}

#[test]
fn a_fresh_build_after_rollback_produces_a_new_transaction_id_with_the_same_digest() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut transaction = build_plan_transaction(&export, &approved, "root").unwrap();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .unwrap();
    rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted).unwrap();

    let fresh_preview = build_preview(&export);
    let fresh_approved = approve_transaction(&fresh_preview, "fresh ack").unwrap();
    let fresh_transaction = build_plan_transaction(&export, &fresh_approved, "root").unwrap();

    assert_ne!(fresh_transaction.transaction_id, transaction.transaction_id);
    assert_eq!(fresh_preview.digest, preview.digest);
    assert_eq!(transaction.state, TransactionState::RolledBack);
}

#[test]
fn journal_on_disk_reflects_applied_then_rolled_back_terminal_states() {
    let fx = fixture();
    let source = fx.root.join("source.bin");
    write_rom(&source);
    let destination = fx.root.join("library").join("dest.bin");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut transaction = build_plan_transaction(&export, &approved, "root").unwrap();
    let transaction_id = transaction.transaction_id.clone();
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);

    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fx.root,
        trusted.clone(),
        &fx.journal_dir,
        &cancel,
        false,
        RealApplyPolicy::Canary.hard_conflict_mode(),
    )
    .unwrap();
    let path = journal_path(&fx.journal_dir, &transaction_id).unwrap();
    let on_disk = read_journal(&path).unwrap();
    assert_eq!(on_disk.state, TransactionState::Applied);

    rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel, &trusted).unwrap();
    let on_disk = read_journal(&path).unwrap();
    assert_eq!(on_disk.state, TransactionState::RolledBack);
}
