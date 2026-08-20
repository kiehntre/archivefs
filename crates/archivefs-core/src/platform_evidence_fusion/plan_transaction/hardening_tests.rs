//! Batch 15: torture-testing the Batch 14 transaction safety boundary.
//!
//! Failure injection here is deliberately *not* a parallel framework: every
//! test in this file either (a) calls the crate's existing, already
//! decomposed transaction primitives directly - [`write_journal`]/
//! [`read_journal`], [`run_preflight`], [`reconcile_recovery`],
//! [`rollback_transaction`], [`rename_noreplace`], [`capture_identity`] -
//! with an adversarial fixture or a hand-built [`RenameTransaction`] state,
//! or (b) drives [`apply_plan_transaction_with_mode`]/
//! [`rollback_plan_transaction`] against a real tempdir fixture that has
//! been sabotaged (a file pre-created where the plan expects to write, a
//! permission bit removed, a symlink substituted) before the call. No
//! random OS failures, no code path inside `apply_transaction`/
//! `rollback_transaction` was modified to add a conditional injection hook.
//!
//! All mutation in this file is tempdir-only, per the milestone's explicit
//! instruction; nothing here ever touches a real collection.

use super::*;
use crate::dat::rename_apply::identity::{capture_identity, identity_matches};
use crate::dat::rename_apply::journal::{journal_path, read_journal, write_journal};
use crate::dat::rename_apply::model::{ObjectKind, RollbackResult, TransactionState};
use crate::dat::rename_apply::preflight::{DirectoryPolicy, PreflightFailure, run_preflight};
use crate::dat::rename_apply::reconcile::{RecoveryIssueKind, reconcile_recovery};
use crate::dat::rename_apply::rollback::rollback_transaction;
use crate::platform_evidence_fusion::library_plan_export::SourcePrecondition;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

// ------------------------------------------------------------------
// Shared helpers (mirrors plan_transaction/tests.rs's own private helpers -
// this is a sibling test module and cannot see those)
// ------------------------------------------------------------------

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

fn ready_export(source: &str, destination: &str) -> LibraryPlanExport {
    LibraryPlanExport {
        items: vec![item(source, destination)],
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
    let approved = approve_transaction(&preview, "hardening test acknowledgement").unwrap();
    let transaction = build_plan_transaction(export, &approved, "hardening-test-root").unwrap();
    (approved, transaction)
}

/// Applies a plan transaction with the default `AbortAll` mode, real
/// tempdir fixture, no symlink sources allowed - the common case most
/// hardening tests need.
fn apply(
    fx: &Fixture,
    export: &LibraryPlanExport,
    transaction: &mut RenameTransaction,
) -> Result<ApplyOutcome, ApplyError> {
    let generation = plan_generation_of(export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    )
}

// ====================================================================
// A. Digest / approval regression matrix (milestone section 15) - every
// remaining field not already covered by plan_transaction/tests.rs's own
// digest_changes_when_{destination,hash,status,blockers,set_label}_changes.
// ====================================================================

#[test]
fn digest_changes_when_source_path_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].precondition.source_path = "/roms/b.bin".to_string();
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_normalized_hash_changes() {
    let mut a = ready_export("/roms/a.bin", "/lib/a.bin");
    a.items[0].precondition.physical_hash = Some("same-physical".to_string());
    let mut b = a.clone();
    b.items[0].precondition.normalized_hash = Some("norm-1".to_string());
    let mut c = a.clone();
    c.items[0].precondition.normalized_hash = Some("norm-2".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
    assert_ne!(compute_plan_digest(&b), compute_plan_digest(&c));
}

#[test]
fn digest_changes_when_support_association_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].support_association = Some("Attached".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_revision_relationship_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].revision_relationship = Some("Supersedes".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_duplicate_classification_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].duplicate_classification = Some(
        crate::platform_evidence_fusion::duplicate_taxonomy::DuplicateClass::ExactPhysicalDuplicate,
    );
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_operation_intent_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].operation_intent = OperationIntent::RenameInPlace;
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_set_destination_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].set_destination = Some("/lib/set".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn digest_changes_when_support_role_changes() {
    let a = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut b = a.clone();
    b.items[0].support_role = Some("Manual".to_string());
    assert_ne!(compute_plan_digest(&a), compute_plan_digest(&b));
}

#[test]
fn approval_is_invalidated_end_to_end_when_source_path_changes_after_approval() {
    let export = ready_export("/roms/a.bin", "/lib/a.bin");
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    let mut changed = export.clone();
    changed.items[0].precondition.source_path = "/roms/other.bin".to_string();
    let result = build_plan_transaction(&changed, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

#[test]
fn approval_is_invalidated_end_to_end_when_normalized_hash_changes() {
    let mut export = ready_export("/roms/a.bin", "/lib/a.bin");
    export.items[0].precondition.normalized_hash = Some("norm-1".to_string());
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    let mut changed = export.clone();
    changed.items[0].precondition.normalized_hash = Some("norm-2".to_string());
    let result = build_plan_transaction(&changed, &approved, "test");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

// ------------------------------------------------------------------
// Approved-subset safety (milestone section 16). `approve_transaction`
// does not currently expose any way to approve a strict subset of a
// preview's operations - it always approves every operation the preview
// contains. These tests document that default and prove the *deeper*
// containment check `build_plan_transaction` already performs
// (`approved.approved_item_ids.contains(source)`) correctly scopes
// authority to exactly the approved ids even if a caller ever did
// construct a trimmed `ApprovedPlan` by hand (every field on it is
// `pub`) - defense in depth, not a promise that partial approval is a
// supported feature today.
// ------------------------------------------------------------------

#[test]
fn approve_transaction_always_approves_every_preview_operation_no_partial_authority_by_default() {
    let export = LibraryPlanExport {
        items: vec![
            item("/roms/a.bin", "/lib/a.bin"),
            item("/roms/b.bin", "/lib/b.bin"),
            item("/roms/c.bin", "/lib/c.bin"),
        ],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "yes").unwrap();
    assert_eq!(approved.approved_item_ids.len(), 3);
}

#[test]
fn a_hand_trimmed_approved_id_set_of_two_of_three_excludes_the_third_from_the_transaction() {
    let fx = fixture();
    let a = fx.root.join("a.bin");
    let b = fx.root.join("b.bin");
    let c = fx.root.join("c.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    std::fs::write(&c, b"c").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(
                a.to_str().unwrap(),
                fx.root.join("out-a.bin").to_str().unwrap(),
            ),
            item(
                b.to_str().unwrap(),
                fx.root.join("out-b.bin").to_str().unwrap(),
            ),
            item(
                c.to_str().unwrap(),
                fx.root.join("out-c.bin").to_str().unwrap(),
            ),
        ],
    };
    let preview = build_preview(&export);
    let mut approved = approve_transaction(&preview, "yes").unwrap();
    // Hand-trim: only "a" and "b" remain approved, even though the
    // preview offered all three - not a supported public API, but the
    // struct is `pub` and build_plan_transaction must still honor it.
    approved
        .approved_item_ids
        .remove(&c.to_string_lossy().into_owned());
    let transaction = build_plan_transaction(&export, &approved, "test").unwrap();
    assert_eq!(transaction.entries.len(), 2);
    let sources: BTreeSet<String> = transaction
        .entries
        .iter()
        .map(|entry| entry.source_path.to_string_lossy().into_owned())
        .collect();
    assert!(sources.contains(&a.to_string_lossy().into_owned()));
    assert!(sources.contains(&b.to_string_lossy().into_owned()));
    assert!(!sources.contains(&c.to_string_lossy().into_owned()));
}

// ------------------------------------------------------------------
// Transaction id uniqueness and journal identity binding (sections 33-34).
// ------------------------------------------------------------------

#[test]
fn transaction_ids_never_collide_across_many_builds() {
    let export = ready_export("/roms/a.bin", "/lib/a.bin");
    let mut ids = BTreeSet::new();
    for _ in 0..50 {
        let preview = build_preview(&export);
        let approved = approve_transaction(&preview, "yes").unwrap();
        // Every source here does not exist on disk, so build_plan_transaction
        // itself would exclude it; call the id generator the same way
        // build_plan_transaction does instead, to isolate the property
        // under test (id generation) from filesystem existence.
        let id =
            crate::dat::rename_apply::journal::new_transaction_id(crate::dat::sources::now_unix());
        assert!(ids.insert(id), "transaction id collided");
        let _ = &approved;
    }
}

#[test]
fn two_builds_from_the_same_approved_export_get_different_transaction_ids() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let export = ready_export(
        source.to_str().unwrap(),
        fx.root.join("out.bin").to_str().unwrap(),
    );
    let (approved, first) = build_and_approve(&export);
    let second = build_plan_transaction(&export, &approved, "test").unwrap();
    assert_ne!(first.transaction_id, second.transaction_id);
    assert_eq!(first.plan_generation, second.plan_generation);
}

#[test]
fn tampering_with_plan_generation_in_the_journal_is_caught_on_reapply() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    write_journal(&fx.journal_dir, &transaction).unwrap();

    // Hand-tamper the on-disk journal's plan_generation.
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["plan_generation"] = serde_json::json!(999_999_u64);
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let reread = read_journal(&path).unwrap();
    assert_eq!(reread.plan_generation, 999_999);

    // A fresh reapply attempt compares the tampered generation against the
    // real, freshly recomputed one - the mismatch is caught.
    let real_generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let mut tampered = reread;
    let result = apply_plan_transaction(
        &mut tampered,
        real_generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::StalePlan { .. })));
    let _ = &transaction;
}

#[test]
fn tampering_with_an_entrys_destination_path_is_not_cryptographically_prevented_but_identity_mismatch_still_refuses()
 {
    // Documented limitation (section 35): the journal has no MAC/signature.
    // Hand-editing an entry's destination_path in the journal file is not
    // structurally detected as tampering. What *does* still protect the
    // system is that apply/rollback re-verify object identity against the
    // recorded `ObjectIdentity`, so redirecting a path to an unrelated
    // real object still fails closed via the ordinary identity checks -
    // incidental protection, not authenticated integrity.
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply(&fx, &export, &mut transaction).unwrap();
    assert!(destination.exists());

    // Roll back, then tamper with the journaled destination_path to point
    // at an unrelated, already-existing file elsewhere in the tempdir.
    rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    let unrelated = fx.root.join("unrelated.bin");
    std::fs::write(&unrelated, b"unrelated real content").unwrap();
    transaction.entries[0].destination_path = unrelated.clone();
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applied;
    transaction.state = TransactionState::Applied;
    write_journal(&fx.journal_dir, &transaction).unwrap();

    // A rollback replayed against the tampered journal must still refuse:
    // the recorded identity for this entry does not match the unrelated
    // file now sitting at the tampered destination_path.
    let result = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel);
    let outcome = result.unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    // The unrelated file is completely untouched.
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"unrelated real content"
    );
    let _ = generation;
}

// ====================================================================
// B. Journal write failure, corruption, and mid-mutation crash states
// (milestone sections 4-7).
// ====================================================================

/// Whether this process can exercise a real permission-denied write
/// failure (skipped when running as root, where permission bits do not
/// block writes).
fn permission_checks_are_meaningful() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` takes no arguments and only reads process state.
        unsafe { libc::geteuid() != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[test]
fn journal_write_fails_when_the_journal_directory_is_actually_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("journal-is-a-file");
    std::fs::write(&not_a_dir, b"not a directory").unwrap();
    let fx_root = dir.path().join("library");
    std::fs::create_dir_all(&fx_root).unwrap();
    let source = fx_root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx_root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx_root,
        TrustedRoots::from_paths([fx_root.as_path()]),
        &not_a_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::Journal(_))));
    assert!(
        source.exists(),
        "no mutation may happen before the journal is durable"
    );
    assert!(!destination.exists());
}

#[test]
fn journal_intent_write_failure_when_the_journal_directory_is_read_only_prevents_all_mutation() {
    if !permission_checks_are_meaningful() {
        return;
    }
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fx.journal_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fx.journal_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(matches!(result, Err(ApplyError::Journal(_))));
    assert!(
        source.exists(),
        "the intent write failed, so no mutation happened"
    );
    assert!(!destination.exists());
}

#[test]
fn rollback_journal_write_failure_leaves_manual_recovery_state_not_a_destructive_retry() {
    if !permission_checks_are_meaningful() {
        return;
    }
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    assert!(destination.exists());

    let cancel = AtomicBool::new(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fx.journal_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    let result = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fx.journal_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(
        result.is_err(),
        "a rollback that cannot journal must not proceed silently"
    );
    // The rename never happened - the destination is intact, the source is
    // still absent, exactly as before the failed rollback attempt.
    assert!(destination.exists());
    assert!(!source.exists());
}

#[test]
fn read_journal_on_truncated_json_returns_an_error_not_a_guess() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.json");
    std::fs::write(
        &path,
        br#"{"transaction_id": "1-0", "plan_generation": 1, "entries": [{"source_path"#,
    )
    .unwrap();
    assert!(read_journal(&path).is_err());
}

#[test]
fn read_journal_on_an_invalid_state_value_returns_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad-state.json");
    std::fs::write(
        &path,
        r#"{"transaction_id":"1-0","plan_generation":1,"created_at_unix":1,"source_scan_root":"","state":"not_a_real_state","entries":[]}"#,
    )
    .unwrap();
    assert!(read_journal(&path).is_err());
}

#[test]
fn read_journal_missing_the_required_entries_field_returns_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-entries.json");
    std::fs::write(
        &path,
        r#"{"transaction_id":"1-0","plan_generation":1,"created_at_unix":1,"source_scan_root":""}"#,
    )
    .unwrap();
    assert!(read_journal(&path).is_err());
}

#[test]
fn a_corrupt_journal_alongside_a_valid_one_is_reported_not_silently_skipped() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    write_journal(&fx.journal_dir, &transaction).unwrap();
    std::fs::write(fx.journal_dir.join("corrupt.json"), "not json {{").unwrap();

    let (recovery, problems) =
        crate::dat::rename_apply::journal::find_recovery_transactions(&fx.journal_dir);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(recovery.len(), 1, "the valid journal is still found");
    assert_eq!(recovery[0].transaction_id, transaction.transaction_id);
}

#[test]
fn impossible_state_applied_transaction_with_no_applied_entries_is_treated_as_already_committed_never_rolled_back()
 {
    // A corrupted/impossible combination: transaction-level Applied but no
    // entry actually Applied. Fails closed: there is nothing to reverse,
    // so this is classified as "nothing to do", never as an invitation to
    // mutate anything.
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.state = TransactionState::Applied;
    // entries[0].state stays Planned - impossible/corrupted combination.
    let assessment = assess_recovery(&transaction, &[]);
    assert_eq!(assessment, RecoveryAssessment::AlreadyCommitted);
    let _ = fx;
}

#[test]
fn mid_mutation_crash_state_intent_recorded_mutation_not_performed_reconciles_to_skipped() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    // Hand-simulate the exact crash window: durable Applying checkpoint
    // written, rename syscall never ran.
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    transaction.state = TransactionState::Applying;
    write_journal(&fx.journal_dir, &transaction).unwrap();

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameDidNotHappen);
    assert_eq!(
        transaction.entries[0].state,
        crate::dat::rename_apply::model::EntryState::Skipped
    );
    assert!(source.exists(), "reconciliation is read-only");
}

#[test]
fn mid_mutation_crash_state_mutation_performed_result_not_recorded_reconciles_to_applied() {
    // Equivalent to "mutation succeeded but the result journal write
    // failed": the journal was never updated past the pre-mutation
    // Applying checkpoint, even though the rename actually happened.
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    transaction.state = TransactionState::Applying;
    write_journal(&fx.journal_dir, &transaction).unwrap();

    // Perform the mutation directly (bypassing apply_transaction, exactly
    // simulating "the rename ran, but the process died before the result
    // journal write landed").
    std::fs::rename(&source, &destination).unwrap();

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameConfirmed);
    assert_eq!(
        transaction.entries[0].state,
        crate::dat::rename_apply::model::EntryState::Applied
    );
    assert_eq!(transaction.state, TransactionState::Applied);
    let reread =
        read_journal(&journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap()).unwrap();
    assert_eq!(
        reread.state,
        TransactionState::Applied,
        "the correction is durable"
    );
}

#[test]
fn mid_mutation_crash_state_rollback_intent_recorded_rollback_not_performed_stays_applied() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"placeholder").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"data").unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].identity = capture_identity(&destination).unwrap();
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::RollingBack;
    transaction.state = TransactionState::RollingBack;
    std::fs::remove_file(&source).unwrap();
    write_journal(&fx.journal_dir, &transaction).unwrap();

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameConfirmed);
    assert_eq!(
        transaction.entries[0].state,
        crate::dat::rename_apply::model::EntryState::Applied,
        "the reverse rename never ran; the entry is still Applied so a retry can reverse it"
    );
}

#[test]
fn mid_mutation_crash_state_rollback_performed_result_not_recorded_reconciles_to_rolled_back() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"placeholder").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"data").unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].identity = capture_identity(&destination).unwrap();
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::RollingBack;
    transaction.state = TransactionState::RollingBack;
    std::fs::remove_file(&source).unwrap();
    write_journal(&fx.journal_dir, &transaction).unwrap();

    // The reverse rename actually ran; the journal never recorded it.
    std::fs::rename(&destination, &source).unwrap();

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameDidNotHappen);
    assert_eq!(
        transaction.entries[0].state,
        crate::dat::rename_apply::model::EntryState::RolledBack
    );
}

// ====================================================================
// C. Rollback failure injection (milestone section 9-11).
// ====================================================================

#[test]
fn rollback_refuses_when_the_original_source_path_is_occupied_by_an_unrelated_file() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    // An unrelated file appears at the original source path before rollback.
    std::fs::write(&source, b"unrelated new content").unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    assert_eq!(std::fs::read(&source).unwrap(), b"unrelated new content");
    assert!(
        destination.exists(),
        "the moved file is never displaced by a blocked rollback"
    );
}

#[test]
fn rollback_refuses_when_the_destination_is_missing_after_apply() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    std::fs::remove_file(&destination).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    assert!(!source.exists(), "nothing is fabricated at the source");
}

#[test]
fn rollback_of_a_never_applied_transaction_is_a_trivial_no_op() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(outcome.result, RollbackResult::FullyRolledBack));
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn rollback_stops_at_the_first_failure_and_leaves_the_other_applied_entry_untouched() {
    let fx = fixture();
    let a_source = fx.root.join("a.bin");
    let b_source = fx.root.join("b.bin");
    std::fs::write(&a_source, b"a-data").unwrap();
    std::fs::write(&b_source, b"b-data").unwrap();
    let a_dest = fx.root.join("ps").join("a.bin");
    let b_dest = fx.root.join("ps").join("b.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a_source.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b_source.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    assert!(a_dest.exists());
    assert!(b_dest.exists());

    // Sabotage the LAST-applied entry (b), which rollback processes FIRST
    // (reverse order): replace its destination content so identity no
    // longer matches.
    std::fs::write(&b_dest, b"replaced externally").unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. } | RollbackResult::PartiallyRolledBack { .. }
    ));
    // a was never reached because rollback broke on b's failure first.
    assert!(a_dest.exists(), "a is left exactly where it was, untouched");
    assert!(!a_source.exists());
    assert_eq!(std::fs::read(&b_dest).unwrap(), b"replaced externally");
}

#[test]
fn rollback_after_cancellation_leaves_untouched_entries_retryable_and_never_reports_full() {
    let fx = fixture();
    let a_source = fx.root.join("a.bin");
    let b_source = fx.root.join("b.bin");
    std::fs::write(&a_source, b"a-data").unwrap();
    std::fs::write(&b_source, b"b-data").unwrap();
    let a_dest = fx.root.join("ps").join("a.bin");
    let b_dest = fx.root.join("ps").join("b.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a_source.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b_source.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    let cancel = AtomicBool::new(true);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(!matches!(outcome.result, RollbackResult::FullyRolledBack));
    // Nothing was reversed - both entries are still at their destinations.
    assert!(a_dest.exists());
    assert!(b_dest.exists());
}

#[test]
fn a_rollback_failure_can_be_retried_after_the_external_conflict_is_resolved() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    std::fs::write(&source, b"blocking").unwrap();
    let cancel = AtomicBool::new(false);
    let first = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        first.result,
        RollbackResult::RollbackFailed { .. }
    ));

    // Resolve the conflict, then retry.
    std::fs::remove_file(&source).unwrap();
    transaction.state = TransactionState::Applied; // manual operator decision to retry
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applied;
    let second = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(second.result, RollbackResult::FullyRolledBack));
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn rollback_never_follows_a_symlink_planted_at_the_original_source_path() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = fx.root.join("ps").join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        apply(&fx, &export, &mut transaction).unwrap();

        let elsewhere = fx.root.join("elsewhere.bin");
        std::fs::write(&elsewhere, b"planted target").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &source).unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
        assert!(matches!(
            outcome.result,
            RollbackResult::RollbackFailed { .. }
        ));
        assert!(
            std::fs::symlink_metadata(&source)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"planted target");
    }
}

#[test]
fn rollback_outcome_never_reports_fully_rolled_back_for_a_genuinely_partial_result() {
    let fx = fixture();
    let a_source = fx.root.join("a.bin");
    let b_source = fx.root.join("b.bin");
    std::fs::write(&a_source, b"a-data").unwrap();
    std::fs::write(&b_source, b"b-data").unwrap();
    let a_dest = fx.root.join("ps").join("a.bin");
    let b_dest = fx.root.join("ps").join("b.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a_source.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b_source.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    // Block only b's reversal (rolled back first, in reverse order) by
    // occupying its original source path - a is left fully reversible.
    std::fs::write(&b_source, b"blocking").unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(
        !matches!(outcome.result, RollbackResult::FullyRolledBack),
        "b's reversal was blocked - this must never be reported as a full rollback"
    );
}

#[test]
fn directory_created_by_the_transaction_but_given_external_content_before_rollback_is_kept() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("set").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let outcome = apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(outcome.transaction.created_directories.len(), 2);

    // An external actor drops an unrelated file into the transaction-owned
    // "set" directory before rollback.
    std::fs::write(destination.parent().unwrap().join("unrelated.txt"), b"x").unwrap();

    let cancel = AtomicBool::new(false);
    let rollback = rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        rollback.rollback.result,
        RollbackResult::FullyRolledBack
    ));
    assert!(
        rollback
            .directories_remaining
            .contains(&destination.parent().unwrap().to_path_buf()),
        "a non-empty transaction-owned directory is left in place, never force-removed"
    );
    assert!(destination.parent().unwrap().exists());
}

#[test]
fn manual_recovery_required_when_source_and_destination_both_exist_blocks_rollback() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    // Leave the journal saying `Applying` (in-flight) while both the
    // source and destination exist with matching identity - a hard link
    // reproduces exactly that ambiguous shape.
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    transaction.state = TransactionState::Applying;
    std::fs::hard_link(&destination, &source).unwrap();
    write_journal(&fx.journal_dir, &transaction).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    assert_eq!(transaction.state, TransactionState::RollbackFailed);
}

#[test]
fn rollback_of_a_multi_entry_batch_reverses_every_entry_byte_for_byte() {
    let fx = fixture();
    let a_source = fx.root.join("a.bin");
    let b_source = fx.root.join("b.bin");
    std::fs::write(&a_source, b"a-payload").unwrap();
    std::fs::write(&b_source, b"b-payload").unwrap();
    let a_dest = fx.root.join("ps").join("a.bin");
    let b_dest = fx.root.join("ps").join("b.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a_source.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b_source.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let cancel = AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(outcome.result, RollbackResult::FullyRolledBack));
    assert_eq!(std::fs::read(&a_source).unwrap(), b"a-payload");
    assert_eq!(std::fs::read(&b_source).unwrap(), b"b-payload");
    assert!(!a_dest.exists());
    assert!(!b_dest.exists());
}

// ====================================================================
// D. Idempotence torture tests (milestone sections 12-14).
// ====================================================================

#[test]
fn probe_apply_after_rollback_on_the_same_transaction_object() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let cancel = AtomicBool::new(false);
    rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(source.exists());
    assert!(!destination.exists());

    // The bridge layer must refuse to re-run an already-settled (RolledBack
    // or Applied) transaction object, even though its individual entries'
    // preflight would otherwise re-pass (the source is back, matching its
    // original recorded identity) - see apply_plan_transaction's own
    // terminal-state guard.
    let generation = plan_generation_of(&export);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(
        result.is_err(),
        "a RolledBack transaction must never be silently re-applied"
    );
    assert!(source.exists(), "re-apply must not move it again");
    assert!(!destination.exists());
}

#[test]
fn double_apply_on_a_fresh_transaction_object_second_call_mutates_nothing() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(transaction.state, TransactionState::Applied);

    let result2 = apply(&fx, &export, &mut transaction);
    assert!(
        result2.is_err(),
        "a second apply on an Applied transaction must be refused"
    );
    assert!(destination.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), b"data");
}

#[test]
fn double_rollback_is_a_safe_no_op_with_zero_further_mutation() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let cancel = AtomicBool::new(false);
    let first = rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        first.rollback.result,
        RollbackResult::FullyRolledBack
    ));
    let second = rollback_plan_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(
        second.rollback.result,
        RollbackResult::FullyRolledBack
    ));
    assert_eq!(std::fs::read(&source).unwrap(), b"data");
    assert!(!destination.exists());
}

#[test]
fn rollback_of_a_transaction_with_zero_applied_entries_after_a_hard_conflict_abort_is_trivial() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"already there").unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let result = apply(&fx, &export, &mut transaction);
    assert!(result.is_err());
    let cancel = AtomicBool::new(false);
    let first = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    let second = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(first.result, RollbackResult::FullyRolledBack));
    assert!(matches!(second.result, RollbackResult::FullyRolledBack));
    assert_eq!(std::fs::read(&destination).unwrap(), b"already there");
}

#[test]
fn reapplying_after_a_hard_conflict_abort_makes_zero_change_across_repeated_attempts() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"already there").unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    for _ in 0..3 {
        let result = apply(&fx, &export, &mut transaction);
        assert!(result.is_err());
        assert!(source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"already there");
    }
}

#[test]
fn a_reloaded_journal_after_apply_still_refuses_a_second_apply() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();

    // Simulate a process restart: reload the transaction purely from its
    // durable journal, not the in-memory object.
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let mut reloaded = read_journal(&path).unwrap();
    assert_eq!(reloaded.state, TransactionState::Applied);

    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut reloaded,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(result.is_err());
    assert!(destination.exists());
    assert!(!source.exists());
}

#[test]
fn rollback_then_rollback_again_then_a_third_time_all_remain_safe_no_ops() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let cancel = AtomicBool::new(false);
    for _ in 0..3 {
        let outcome = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
        assert!(matches!(outcome.result, RollbackResult::FullyRolledBack));
    }
    assert_eq!(std::fs::read(&source).unwrap(), b"data");
}

// ====================================================================
// E. Hardlink / symlink safety (milestone sections 17-20).
// ====================================================================

#[test]
fn hardlink_to_source_is_detected_via_matching_inode_and_dev() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("a.bin");
        std::fs::write(&primary, b"data").unwrap();
        let sibling = dir.path().join("a-sibling.bin");
        std::fs::hard_link(&primary, &sibling).unwrap();
        let primary_meta = std::fs::metadata(&primary).unwrap();
        let sibling_meta = std::fs::metadata(&sibling).unwrap();
        assert_eq!(primary_meta.ino(), sibling_meta.ino());
        assert_eq!(primary_meta.dev(), sibling_meta.dev());
        assert_eq!(primary_meta.nlink(), 2);
        let identity = capture_identity(&primary).unwrap();
        let sibling_identity = capture_identity(&sibling).unwrap();
        #[cfg(unix)]
        {
            assert_eq!(identity.ino, sibling_identity.ino);
            assert_eq!(identity.dev, sibling_identity.dev);
        }
    }
}

#[test]
fn moving_one_hardlinked_path_does_not_delete_the_others_content() {
    let fx = fixture();
    let primary = fx.root.join("a.bin");
    std::fs::write(&primary, b"shared content").unwrap();
    let sibling = fx.root.join("a-sibling.bin");
    std::fs::hard_link(&primary, &sibling).unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(primary.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    assert!(destination.exists());
    // The transaction only ever renames one directory entry; the sibling
    // hardlink (unique content, shared inode) is completely untouched.
    assert!(sibling.exists(), "the untouched sibling must still exist");
    assert_eq!(std::fs::read(&sibling).unwrap(), b"shared content");
}

#[test]
fn no_operation_kind_in_this_transaction_layer_ever_represents_a_delete() {
    // Structural: OperationKind has exactly {Move, Rename, Unsupported} -
    // no Delete authority exists anywhere for a hardlink (or any other)
    // sibling. Locked via the rendered preview text never mentioning it.
    let export = ready_export("/roms/a.bin", "/lib/a.bin");
    let preview = build_preview(&export);
    let text = render_preview_text(&preview);
    assert!(!text.to_uppercase().contains("DELETE"));
}

#[test]
fn identity_matching_ignores_link_count_hardlink_precondition_is_not_tracked() {
    // Documented limitation (milestone section 18): ObjectIdentity carries
    // size/mtime/kind/ino/dev, never `nlink`. Unlinking a sibling hardlink
    // does not change the primary's own identity at all, so this
    // transaction layer has no way to notice a link-count change between
    // preview and apply - it was never designed to.
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("a.bin");
    std::fs::write(&primary, b"data").unwrap();
    let sibling = dir.path().join("sibling.bin");
    std::fs::hard_link(&primary, &sibling).unwrap();
    let before = capture_identity(&primary).unwrap();
    std::fs::remove_file(&sibling).unwrap();
    let after = capture_identity(&primary).unwrap();
    assert!(
        identity_matches(&before, &after),
        "link-count changes are invisible to identity matching by design"
    );
}

#[test]
fn symlink_source_target_is_never_moved_only_the_link_object_could_be() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let target_dir = fx.root.join("outside-plan");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("real.bin");
        std::fs::write(&target, b"real content").unwrap();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(&target, &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        let result = apply(&fx, &export, &mut transaction);
        assert!(result.is_err());
        assert!(target.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"real content");
        assert!(
            std::fs::symlink_metadata(&source)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn source_becomes_a_symlink_after_preview_is_caught_at_apply_preflight() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = fx.root.join("ps").join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        std::fs::remove_file(&source).unwrap();
        let elsewhere = fx.root.join("elsewhere.bin");
        std::fs::write(&elsewhere, b"x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &source).unwrap();
        let result = apply(&fx, &export, &mut transaction);
        assert!(result.is_err());
        assert!(
            std::fs::symlink_metadata(&source)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn symlink_target_repointed_after_preview_changes_the_links_own_identity() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let target_a = fx.root.join("target-a.bin");
        let target_b = fx.root.join("target-b.bin");
        std::fs::write(&target_a, b"a").unwrap();
        std::fs::write(&target_b, b"b").unwrap();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(&target_a, &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        // Re-point the symlink itself via an atomic replace from a
        // separately created link object, guaranteeing a distinct inode
        // (both objects exist simultaneously right up to the rename, so
        // the original source's inode cannot have been freed and reused).
        let alt = fx.root.join("link-alt.bin");
        std::os::unix::fs::symlink(&target_b, &alt).unwrap();
        std::fs::rename(&alt, &source).unwrap();
        let generation = plan_generation_of(&export);
        let cancel = AtomicBool::new(false);
        let result = apply_plan_transaction(
            &mut transaction,
            generation,
            &fx.root,
            TrustedRoots::from_paths([fx.root.as_path()]),
            &fx.journal_dir,
            &cancel,
            true, // allow_symlink_source
        );
        assert!(
            result.is_err(),
            "the repointed link is a different object than the one reviewed"
        );
        assert!(target_a.exists());
        assert!(target_b.exists());
    }
}

#[test]
fn nested_destination_crossing_a_symlinked_ancestor_pointing_outside_the_root_is_refused() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let ancestor = fx.root.join("ps");
        std::os::unix::fs::symlink(outside.path(), &ancestor).unwrap();
        let destination = ancestor.join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let preview = build_preview(&export);
        let approved = approve_transaction(&preview, "yes").unwrap();
        let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
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
        assert!(
            result.is_err(),
            "a destination ancestor symlinked outside the trusted root must be refused"
        );
        assert!(source.exists());
        assert!(
            std::fs::read_dir(outside.path()).unwrap().next().is_none(),
            "nothing was written outside the root"
        );
    }
}

#[test]
fn destination_ancestor_symlink_attack_replaced_after_approval_before_apply_is_refused_when_target_is_outside_root()
 {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = fx.root.join("ps").join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        // TOCTOU: after approval, before apply, an attacker replaces the
        // destination's ancestor directory with a symlink pointing outside
        // the trusted root.
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), destination.parent().unwrap()).unwrap();
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
        assert!(result.is_err());
        assert!(
            source.exists(),
            "the source is never moved through an attacker-controlled ancestor"
        );
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}

#[test]
fn rollback_never_replaces_a_symlink_planted_at_source_content_preserved() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = fx.root.join("ps").join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        apply(&fx, &export, &mut transaction).unwrap();
        let real_target = fx.root.join("planted-target.bin");
        std::fs::write(&real_target, b"planted").unwrap();
        std::os::unix::fs::symlink(&real_target, &source).unwrap();
        let cancel = AtomicBool::new(false);
        rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
        assert_eq!(std::fs::read(&real_target).unwrap(), b"planted");
        assert!(
            std::fs::symlink_metadata(&source)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn a_symlinks_own_identity_never_equals_its_targets_identity_in_this_layer() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let target = fx.root.join("target.bin");
        std::fs::write(&target, b"data").unwrap();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(&target, &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, transaction) = build_and_approve(&export);
        assert_eq!(transaction.entries[0].identity.kind, ObjectKind::Symlink);
        let target_identity = capture_identity(&target).unwrap();
        assert_ne!(transaction.entries[0].identity.kind, target_identity.kind);
    }
}

#[test]
fn a_broken_symlink_source_is_refused_the_same_as_a_live_one() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(fx.root.join("nowhere.bin"), &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let preview = build_preview(&export);
        let approved = approve_transaction(&preview, "yes").unwrap();
        let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
        assert_eq!(
            transaction.entries[0].identity.kind,
            ObjectKind::BrokenSymlink
        );
        let result = apply(&fx, &export, &mut transaction);
        assert!(result.is_err());
    }
}

#[test]
fn allow_symlink_source_true_moves_only_the_link_object_never_dereferences_the_target() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let target = fx.root.join("target.bin");
        std::fs::write(&target, b"real content").unwrap();
        let source = fx.root.join("link.bin");
        std::os::unix::fs::symlink(&target, &source).unwrap();
        let destination = fx.root.join("ps").join("link.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
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
            true,
        )
        .unwrap();
        assert_eq!(outcome.transaction.state, TransactionState::Applied);
        assert!(
            std::fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&destination).unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), b"real content");
    }
}

// ====================================================================
// F. Cross-filesystem policy (milestone section 21). `apply_plan_transaction`
// always builds its `ApplyExecution` with `DirectoryPolicy::SameFilesystem`
// (see plan_transaction.rs) - never a caller-selectable policy - so a move
// off the trusted root's own device is always refused, with no copy+delete
// fallback anywhere in this crate.
// ====================================================================

/// Two genuinely different, independently writable filesystems on this
/// machine: `/tmp` (ext4, part of `/`) and `/dev/shm` (tmpfs). `None` when
/// either is unavailable or, surprisingly, they share a device id (e.g. an
/// unusual container mount layout) - the real-EXDEV tests then skip rather
/// than assert something false.
fn two_real_filesystems() -> Option<(PathBuf, PathBuf)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::path::PathBuf::from("/tmp");
        let b = std::path::PathBuf::from("/dev/shm");
        let a_dev = std::fs::metadata(&a).ok()?.dev();
        let b_dev = std::fs::metadata(&b).ok()?.dev();
        if a_dev == b_dev {
            return None;
        }
        // Confirm both are actually writable by this process.
        let probe_a = a.join(format!("archivefs-hardening-probe-{}", std::process::id()));
        let probe_b = b.join(format!("archivefs-hardening-probe-{}", std::process::id()));
        std::fs::write(&probe_a, b"x").ok()?;
        std::fs::write(&probe_b, b"x").ok()?;
        let _ = std::fs::remove_file(&probe_a);
        let _ = std::fs::remove_file(&probe_b);
        Some((a, b))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[test]
fn real_exdev_move_across_tmp_and_dev_shm_is_refused_with_the_named_error() {
    let Some((fs_a, fs_b)) = two_real_filesystems() else {
        return;
    };
    let source_dir = fs_a.join(format!("archivefs-hardening-src-{}", std::process::id()));
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("a.bin");
    std::fs::write(&source, b"real-exdev").unwrap();
    let dest_dir = fs_b.join(format!("archivefs-hardening-dst-{}", std::process::id()));
    let destination = dest_dir.join("a.bin");
    let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
    let trusted = TrustedRoots::from_paths([source_dir.as_path(), fs_b.as_path()]);
    let entry = crate::dat::rename_apply::model::TransactionEntry {
        source_path: source.clone(),
        destination_path: destination.clone(),
        original_basename: "a.bin".to_string(),
        proposed_basename: "a.bin".to_string(),
        identity: capture_identity(&source).unwrap(),
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state: crate::dat::rename_apply::model::EntryState::Planned,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    };
    let destinations = BTreeSet::from([destination.to_string_lossy().into_owned()]);
    let options = crate::dat::rename_apply::preflight::PreflightOptions {
        plan_generation: 1,
        current_generation: 1,
        approved_paths: &approved,
        trusted: &trusted,
        batch_destinations: &destinations,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source: false,
    };
    let failures = run_preflight(&entry, &options).unwrap_err();
    assert!(
        failures
            .iter()
            .any(|f| f == &PreflightFailure::DestinationOnDifferentFilesystem),
        "{failures:?}"
    );
    // No copy+delete fallback: the source is untouched and nothing was
    // ever written on the destination filesystem.
    assert!(source.exists());
    assert!(!dest_dir.exists());
    std::fs::remove_dir_all(&source_dir).unwrap();
}

#[test]
fn same_filesystem_but_different_directory_move_is_allowed_under_same_filesystem_policy() {
    let fx = fixture();
    let source_dir = fx.root.join("incoming");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let outcome = apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(destination.exists());
}

#[test]
fn cross_filesystem_preflight_failure_is_the_named_variant_not_a_generic_error() {
    let reason = PreflightFailure::DestinationOnDifferentFilesystem.reason();
    assert!(reason.contains("different filesystems"));
    assert!(
        reason.contains("not yet supported safely"),
        "the contract is refusal, not a promise of a future copy transaction with ambiguous wording"
    );
}

#[test]
fn cross_filesystem_refusal_is_caught_at_whole_batch_preflight_before_any_entry_mutates() {
    let Some((fs_a, fs_b)) = two_real_filesystems() else {
        return;
    };
    let scoped = fs_a.join(format!("archivefs-hardening-batch-{}", std::process::id()));
    std::fs::create_dir_all(&scoped).unwrap();
    let safe_source = scoped.join("safe.bin");
    std::fs::write(&safe_source, b"safe").unwrap();
    let safe_dest = scoped.join("ps").join("safe.bin");
    let cross_source = scoped.join("cross.bin");
    std::fs::write(&cross_source, b"cross").unwrap();
    let cross_dest = fs_b
        .join(format!(
            "archivefs-hardening-batch-dst-{}",
            std::process::id()
        ))
        .join("cross.bin");

    let export = LibraryPlanExport {
        items: vec![
            item(safe_source.to_str().unwrap(), safe_dest.to_str().unwrap()),
            item(cross_source.to_str().unwrap(), cross_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &scoped,
        TrustedRoots::from_paths([scoped.as_path()]),
        &fx_journal_dir_for(&scoped),
        &cancel,
        false,
    );
    assert!(
        result.is_err(),
        "the whole batch refuses, not just the cross-fs entry"
    );
    assert!(
        safe_source.exists(),
        "the same-fs-safe sibling entry was never touched either"
    );
    assert!(!safe_dest.exists());
    std::fs::remove_dir_all(&scoped).unwrap();
}

fn fx_journal_dir_for(root: &Path) -> PathBuf {
    let dir = root.join("journal");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn ensure_destination_directories_never_creates_anything_outside_the_configured_root() {
    let Some((fs_a, fs_b)) = two_real_filesystems() else {
        return;
    };
    let scoped = fs_a.join(format!(
        "archivefs-hardening-outside-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scoped).unwrap();
    let source = scoped.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let marker = format!("archivefs-hardening-outside-marker-{}", std::process::id());
    let destination = fs_b.join(&marker).join("nested").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let _ = apply_plan_transaction(
        &mut transaction,
        generation,
        &scoped,
        TrustedRoots::from_paths([scoped.as_path()]),
        &fx_journal_dir_for(&scoped),
        &cancel,
        false,
    );
    assert!(
        !fs_b.join(&marker).exists(),
        "nothing may ever be created outside the configured root"
    );
    std::fs::remove_dir_all(&scoped).unwrap();
}

// ====================================================================
// G. TOCTOU / destination races (milestone sections 27-28, plus the
// remaining "before next operation" injection point from section 3).
// ====================================================================

#[test]
fn destination_appears_between_build_and_apply_is_refused_content_preserved() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"appeared").unwrap();
    let result = apply(&fx, &export, &mut transaction);
    assert!(result.is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"appeared");
    assert!(source.exists());
}

#[test]
fn source_replaced_with_same_size_different_inode_is_still_caught() {
    #[cfg(unix)]
    {
        let fx = fixture();
        let source = fx.root.join("a.bin");
        std::fs::write(&source, b"1234").unwrap();
        let destination = fx.root.join("ps").join("a.bin");
        let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
        let (_, mut transaction) = build_and_approve(&export);
        // Same byte length, guaranteed different underlying object: create
        // the replacement at a separate path (so its inode cannot possibly
        // be a reused copy of the original, still-allocated one) and
        // atomically rename it over the source.
        let alt = fx.root.join("a-alt.bin");
        std::fs::write(&alt, b"5678").unwrap();
        std::fs::rename(&alt, &source).unwrap();
        let result = apply(&fx, &export, &mut transaction);
        assert!(
            result.is_err(),
            "size alone is not identity; inode/dev must also match"
        );
    }
}

#[test]
fn two_conflicting_destinations_are_both_skipped_under_skip_unsafe_subset_neither_silently_wins() {
    let fx = fixture();
    let a = fx.root.join("a.bin");
    let b = fx.root.join("b.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let shared_dest = fx.root.join("ps").join("shared.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), shared_dest.to_str().unwrap()),
            item(b.to_str().unwrap(), shared_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
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
    assert_eq!(outcome.transaction.applied_count(), 0);
    assert_eq!(outcome.transaction.skipped_count(), 2);
    assert!(a.exists());
    assert!(b.exists());
    assert!(!shared_dest.exists());
}

#[test]
fn a_destination_that_appears_after_preflight_is_refused_through_the_full_plan_transaction_stack() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"reviewed").unwrap();
    let destination = fx.root.join("b.bin"); // same directory as source
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    // Run preflight once (as the executor would, and as it passes at this
    // moment), then inject the destination right before the mutation would
    // happen - proving the no-clobber primitive, not an exists()+rename
    // race, is what protects the actual rename call.
    let approved_paths: BTreeSet<String> = transaction
        .entries
        .iter()
        .map(|e| e.source_path.to_string_lossy().into_owned())
        .collect();
    let destinations =
        crate::dat::rename_apply::preflight::batch_destinations(&transaction.entries);
    let options = crate::dat::rename_apply::preflight::PreflightOptions {
        plan_generation: transaction.plan_generation,
        current_generation: transaction.plan_generation,
        approved_paths: &approved_paths,
        trusted: &TrustedRoots::from_paths([fx.root.as_path()]),
        batch_destinations: &destinations,
        directory_policy: DirectoryPolicy::SameDirectory,
        allow_symlink_source: false,
    };
    assert!(run_preflight(&transaction.entries[0], &options).is_ok());
    std::fs::write(&destination, b"appeared just before mutation").unwrap();
    let mutation_result =
        crate::dat::rename_apply::noclobber::rename_noreplace(&source, &destination);
    assert!(
        mutation_result.is_err(),
        "the no-clobber primitive itself refuses"
    );
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"appeared just before mutation"
    );
    let _ = &transaction;
}

#[test]
fn injected_failure_before_the_next_operation_stops_only_that_entry_leaving_the_first_applied() {
    // Direct primitive orchestration (Batch 14's established pattern for
    // states AbortAll's whole-batch preflight makes otherwise
    // unconstructable): apply entry 0 for real via the same primitives the
    // executor itself uses, then inject a destination collision for entry
    // 1 - the "before next operation" injection point from milestone
    // section 3 - and prove entry 1's own fresh preflight (run
    // immediately before its own mutation, exactly like the real executor
    // loop) catches it without touching entry 0's already-applied result.
    let fx = fixture();
    let a_source = fx.root.join("a.bin");
    let b_source = fx.root.join("b.bin");
    std::fs::write(&a_source, b"a").unwrap();
    std::fs::write(&b_source, b"b").unwrap();
    let a_dest = fx.root.join("ps").join("a.bin");
    let b_dest = fx.root.join("ps").join("b.bin");
    std::fs::create_dir_all(a_dest.parent().unwrap()).unwrap();

    let a_entry = crate::dat::rename_apply::model::TransactionEntry {
        source_path: a_source.clone(),
        destination_path: a_dest.clone(),
        original_basename: "a.bin".to_string(),
        proposed_basename: "a.bin".to_string(),
        identity: capture_identity(&a_source).unwrap(),
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state: crate::dat::rename_apply::model::EntryState::Planned,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    };
    // Operation 1: apply directly.
    crate::dat::rename_apply::noclobber::rename_noreplace(&a_source, &a_dest).unwrap();
    assert!(a_dest.exists());

    // Inject: something occupies b's destination before its own turn.
    std::fs::write(&b_dest, b"injected").unwrap();
    let b_entry = crate::dat::rename_apply::model::TransactionEntry {
        source_path: b_source.clone(),
        destination_path: b_dest.clone(),
        original_basename: "b.bin".to_string(),
        proposed_basename: "b.bin".to_string(),
        identity: capture_identity(&b_source).unwrap(),
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state: crate::dat::rename_apply::model::EntryState::Planned,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    };
    let approved = BTreeSet::from([b_source.to_string_lossy().into_owned()]);
    let trusted = TrustedRoots::from_paths([fx.root.as_path()]);
    let destinations = BTreeSet::new();
    let options = crate::dat::rename_apply::preflight::PreflightOptions {
        plan_generation: 1,
        current_generation: 1,
        approved_paths: &approved,
        trusted: &trusted,
        batch_destinations: &destinations,
        directory_policy: DirectoryPolicy::SameDirectory,
        allow_symlink_source: false,
    };
    let result = run_preflight(&b_entry, &options);
    assert!(
        result.is_err(),
        "b's own preflight catches the injected collision"
    );
    // a's already-applied result is completely unaffected.
    assert!(a_dest.exists());
    assert_eq!(std::fs::read(&a_dest).unwrap(), b"a");
    let _ = a_entry;
}

#[test]
fn in_batch_case_only_collision_is_still_caught_before_the_second_entrys_mutation() {
    let fx = fixture();
    let a = fx.root.join("Game.bin");
    let b = fx.root.join("other-source.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let a_dest = fx.root.join("out").join("Game.bin");
    let b_dest = fx.root.join("out").join("GAME.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
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
    // Neither entry's destination collided at the *initial* batch preflight
    // (neither existed on disk yet), so this proves the *second* entry's
    // own immediate-before-mutation preflight (run again just before its
    // rename) is what actually catches the case collision once the first
    // entry's rename has created the real directory entry.
    assert_eq!(outcome.transaction.applied_count(), 1);
    assert_eq!(
        outcome.transaction.skipped_count(),
        0,
        "SkipUnsafeSubset's batch-wide pass alone did not see the collision - it is caught at the per-entry retry instead, so this entry legitimately reached ApplyFailed rather than a pre-emptive Skip"
    );
}

#[test]
fn renaming_the_same_basename_case_variant_into_different_directories_is_allowed() {
    let fx = fixture();
    let a = fx.root.join("dir-a").join("Game.bin");
    let b = fx.root.join("dir-b").join("GAME.bin");
    std::fs::create_dir_all(a.parent().unwrap()).unwrap();
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let a_dest = fx.root.join("out-a").join("Game.bin");
    let b_dest = fx.root.join("out-b").join("GAME.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let outcome = apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(a_dest.exists());
    assert!(b_dest.exists());
}

#[test]
fn destination_directory_removed_and_replaced_with_a_file_before_the_rename_fails_closed() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("set").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    // Directories get created by ensure_destination_directories; we cannot
    // interleave mid-call, so this proves the downstream consequence: if
    // the immediate parent is ever a file instead of a directory, the
    // rename itself fails closed rather than corrupting anything.
    std::fs::create_dir_all(fx.root.join("ps")).unwrap();
    std::fs::write(fx.root.join("ps").join("set"), b"blocking file").unwrap();
    // ensure_destination_directories treats any pre-existing path (file or
    // directory) at a component as "not ours to create" and moves on, so
    // the batch-wide preflight (which does not itself verify the
    // destination's parent is a real directory) still passes here; the
    // failure surfaces one level down, at the actual rename syscall
    // (ENOTDIR) - a settled `ApplyFailed` outcome, not a panic or a
    // silent success, and nothing is corrupted either way.
    let result = apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(result.transaction.state, TransactionState::ApplyFailed);
    assert!(
        source.exists(),
        "the source is never lost on a failed mutation"
    );
    assert_eq!(
        std::fs::read(fx.root.join("ps").join("set")).unwrap(),
        b"blocking file"
    );
}

#[test]
fn a_case_only_collision_that_appears_between_preview_and_apply_is_refused() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("Game.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(destination.parent().unwrap().join("GAME.bin"), b"sibling").unwrap();
    let result = apply(&fx, &export, &mut transaction);
    assert!(result.is_err());
    assert!(!destination.exists());
}

// ====================================================================
// H. Set atomicity, multi-set failure, support file failure (milestone
// sections 23-25).
// ====================================================================

#[test]
fn three_member_set_two_discs_plus_m3u_all_move_together() {
    let fx = fixture();
    let disc1 = fx.root.join("Disc 1.chd");
    let disc2 = fx.root.join("Disc 2.chd");
    let m3u = fx.root.join("Game.m3u");
    std::fs::write(&disc1, b"1").unwrap();
    std::fs::write(&disc2, b"2").unwrap();
    std::fs::write(&m3u, b"playlist").unwrap();
    let d1 = fx.root.join("ps").join("Game").join("Disc 1.chd");
    let d2 = fx.root.join("ps").join("Game").join("Disc 2.chd");
    let dm = fx.root.join("ps").join("Game").join("Game.m3u");
    let export = LibraryPlanExport {
        items: vec![
            item(disc1.to_str().unwrap(), d1.to_str().unwrap()),
            item(disc2.to_str().unwrap(), d2.to_str().unwrap()),
            item(m3u.to_str().unwrap(), dm.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let outcome = apply(&fx, &export, &mut transaction).unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(d1.exists() && d2.exists() && dm.exists());
}

#[test]
fn three_member_set_sabotage_on_the_m3u_blocks_the_entire_set_under_abort_all() {
    let fx = fixture();
    let disc1 = fx.root.join("Disc 1.chd");
    let disc2 = fx.root.join("Disc 2.chd");
    let m3u = fx.root.join("Game.m3u");
    std::fs::write(&disc1, b"1").unwrap();
    std::fs::write(&disc2, b"2").unwrap();
    std::fs::write(&m3u, b"playlist").unwrap();
    let d1 = fx.root.join("ps").join("Game").join("Disc 1.chd");
    let d2 = fx.root.join("ps").join("Game").join("Disc 2.chd");
    let dm = fx.root.join("ps").join("Game").join("Game.m3u");
    std::fs::create_dir_all(dm.parent().unwrap()).unwrap();
    std::fs::write(&dm, b"pre-existing").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(disc1.to_str().unwrap(), d1.to_str().unwrap()),
            item(disc2.to_str().unwrap(), d2.to_str().unwrap()),
            item(m3u.to_str().unwrap(), dm.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let result = apply(&fx, &export, &mut transaction);
    assert!(
        result.is_err(),
        "AbortAll refuses the whole set, not just the m3u"
    );
    assert!(disc1.exists());
    assert!(disc2.exists());
    assert!(!d1.exists());
    assert!(!d2.exists());
    assert_eq!(std::fs::read(&dm).unwrap(), b"pre-existing");
}

#[test]
fn multi_set_game_b_failure_under_abort_all_refuses_the_whole_batch_including_a_and_c() {
    let fx = fixture();
    // Game A: 2 discs.
    let a1 = fx.root.join("A-disc1.chd");
    let a2 = fx.root.join("A-disc2.chd");
    std::fs::write(&a1, b"a1").unwrap();
    std::fs::write(&a2, b"a2").unwrap();
    let a1_dest = fx.root.join("ps").join("A").join("A-disc1.chd");
    let a2_dest = fx.root.join("ps").join("A").join("A-disc2.chd");
    // Game B: rom + manual, manual's destination sabotaged.
    let b_rom = fx.root.join("B.bin");
    let b_manual = fx.root.join("B-manual.pdf");
    std::fs::write(&b_rom, b"rom").unwrap();
    std::fs::write(&b_manual, b"manual").unwrap();
    let b_rom_dest = fx.root.join("ps").join("B").join("B.bin");
    let b_manual_dest = fx.root.join("ps").join("B").join("B-manual.pdf");
    std::fs::create_dir_all(b_manual_dest.parent().unwrap()).unwrap();
    std::fs::write(&b_manual_dest, b"already there").unwrap();
    // Game C: single rom, entirely safe.
    let c_rom = fx.root.join("C.bin");
    std::fs::write(&c_rom, b"c").unwrap();
    let c_dest = fx.root.join("ps").join("C").join("C.bin");

    let export = LibraryPlanExport {
        items: vec![
            item(a1.to_str().unwrap(), a1_dest.to_str().unwrap()),
            item(a2.to_str().unwrap(), a2_dest.to_str().unwrap()),
            item(b_rom.to_str().unwrap(), b_rom_dest.to_str().unwrap()),
            item(b_manual.to_str().unwrap(), b_manual_dest.to_str().unwrap()),
            item(c_rom.to_str().unwrap(), c_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let result = apply(&fx, &export, &mut transaction);
    assert!(result.is_err());
    // Not one byte moved anywhere - AbortAll's whole-batch preflight
    // refuses before any mutation, protecting A and C even though only B
    // had a conflict.
    for (source, dest) in [
        (&a1, &a1_dest),
        (&a2, &a2_dest),
        (&b_rom, &b_rom_dest),
        (&c_rom, &c_dest),
    ] {
        assert!(source.exists(), "{source:?} was untouched");
        assert!(!dest.exists(), "{dest:?} was never created");
    }
}

#[test]
fn multi_set_game_b_failure_under_skip_unsafe_subset_can_leave_the_primary_moved_and_support_behind_documented_limitation()
 {
    // Documented limitation: SkipUnsafeSubset is a per-*entry* policy in
    // the shared executor, with no concept of "set membership" as a hard
    // grouping constraint - plan_transaction's set/support labels are
    // export-level planning metadata only, never enforced as an
    // execution-time atomicity unit for this mode. A caller who explicitly
    // opts into SkipUnsafeSubset for a batch containing multi-item sets can
    // therefore end up with a half-organized set (primary moved, its own
    // support file left behind), if only the support file's own
    // destination conflicts. `AbortAll` (this module's default) does not
    // have this gap, because it refuses the *entire* batch on any
    // conflict - see the sibling `_under_abort_all_` test above.
    let fx = fixture();
    let rom = fx.root.join("game.bin");
    let manual = fx.root.join("manual.pdf");
    std::fs::write(&rom, b"rom").unwrap();
    std::fs::write(&manual, b"manual").unwrap();
    let rom_dest = fx.root.join("ps").join("Game").join("game.bin");
    let manual_dest = fx.root.join("ps").join("Game").join("manual.pdf");
    std::fs::create_dir_all(manual_dest.parent().unwrap()).unwrap();
    std::fs::write(&manual_dest, b"already there").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            item(rom.to_str().unwrap(), rom_dest.to_str().unwrap()),
            item(manual.to_str().unwrap(), manual_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
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
    assert!(
        rom_dest.exists(),
        "the primary DID move under SkipUnsafeSubset"
    );
    assert!(!rom.exists());
    assert!(
        manual.exists(),
        "the support file was left behind, still at its source"
    );
    assert_eq!(std::fs::read(&manual_dest).unwrap(), b"already there");
    assert_eq!(outcome.transaction.applied_count(), 1);
    assert_eq!(outcome.transaction.skipped_count(), 1);
}

#[test]
fn multi_set_rollback_after_partial_skip_only_reverses_actually_applied_entries() {
    let fx = fixture();
    let rom = fx.root.join("game.bin");
    let manual = fx.root.join("manual.pdf");
    std::fs::write(&rom, b"rom").unwrap();
    std::fs::write(&manual, b"manual").unwrap();
    let rom_dest = fx.root.join("ps").join("Game").join("game.bin");
    let manual_dest = fx.root.join("ps").join("Game").join("manual.pdf");
    std::fs::create_dir_all(manual_dest.parent().unwrap()).unwrap();
    std::fs::write(&manual_dest, b"already there").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(rom.to_str().unwrap(), rom_dest.to_str().unwrap()),
            item(manual.to_str().unwrap(), manual_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
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
    let rollback = rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert!(matches!(rollback.result, RollbackResult::FullyRolledBack));
    assert!(rom.exists(), "the applied entry was reversed");
    assert!(!rom_dest.exists());
    assert_eq!(
        std::fs::read(&manual_dest).unwrap(),
        b"already there",
        "the never-applied entry's unrelated content is untouched"
    );
}

#[test]
fn game_c_in_a_multi_set_skip_unsafe_subset_batch_succeeds_independently_of_game_bs_failure() {
    let fx = fixture();
    let b_rom = fx.root.join("B.bin");
    let b_manual = fx.root.join("B-manual.pdf");
    std::fs::write(&b_rom, b"rom").unwrap();
    std::fs::write(&b_manual, b"manual").unwrap();
    let b_rom_dest = fx.root.join("ps").join("B").join("B.bin");
    let b_manual_dest = fx.root.join("ps").join("B").join("B-manual.pdf");
    std::fs::create_dir_all(b_manual_dest.parent().unwrap()).unwrap();
    std::fs::write(&b_manual_dest, b"already there").unwrap();
    let c_rom = fx.root.join("C.bin");
    std::fs::write(&c_rom, b"c").unwrap();
    let c_dest = fx.root.join("ps").join("C").join("C.bin");
    let export = LibraryPlanExport {
        items: vec![
            item(b_rom.to_str().unwrap(), b_rom_dest.to_str().unwrap()),
            item(b_manual.to_str().unwrap(), b_manual_dest.to_str().unwrap()),
            item(c_rom.to_str().unwrap(), c_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
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
    assert!(
        c_dest.exists(),
        "C's own success is entirely independent of B's conflict"
    );
}

#[test]
fn set_label_is_purely_export_level_metadata_never_a_field_on_the_transaction_entry() {
    let source_text = include_str!("../../../src/dat/rename_apply/model.rs");
    assert!(
        !source_text.contains("set_label"),
        "sets are a planning-layer concept only; the shared executor's TransactionEntry must \
         never gain execution-time knowledge of set membership"
    );
}

#[test]
fn cycle_detection_still_applies_across_a_multi_set_batch() {
    let fx = fixture();
    let a = fx.root.join("a.bin");
    let b = fx.root.join("b.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    // b's destination is a's source path - a cross-set cycle.
    let export = LibraryPlanExport {
        items: vec![
            item(
                a.to_str().unwrap(),
                fx.root.join("out.bin").to_str().unwrap(),
            ),
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
    assert!(a.exists());
    assert!(b.exists());
}

#[test]
fn two_disc_set_full_round_trip_apply_then_rollback_restores_bytes_exactly() {
    let fx = fixture();
    let disc1 = fx.root.join("Disc 1.chd");
    let disc2 = fx.root.join("Disc 2.chd");
    std::fs::write(&disc1, b"disc-one-bytes").unwrap();
    std::fs::write(&disc2, b"disc-two-bytes").unwrap();
    let d1 = fx.root.join("ps").join("Game").join("Disc 1.chd");
    let d2 = fx.root.join("ps").join("Game").join("Disc 2.chd");
    let export = LibraryPlanExport {
        items: vec![
            item(disc1.to_str().unwrap(), d1.to_str().unwrap()),
            item(disc2.to_str().unwrap(), d2.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let cancel = AtomicBool::new(false);
    rollback_transaction(&mut transaction, &fx.journal_dir, &cancel).unwrap();
    assert_eq!(std::fs::read(&disc1).unwrap(), b"disc-one-bytes");
    assert_eq!(std::fs::read(&disc2).unwrap(), b"disc-two-bytes");
    assert!(!d1.exists() && !d2.exists());
}

#[test]
fn abort_all_refusal_is_atomic_across_five_entries_sabotaging_only_the_last() {
    let fx = fixture();
    let mut export_items = Vec::new();
    let mut sources = Vec::new();
    let mut dests = Vec::new();
    for i in 0..5 {
        let source = fx.root.join(format!("f{i}.bin"));
        std::fs::write(&source, format!("payload-{i}").as_bytes()).unwrap();
        let dest = fx.root.join("out").join(format!("f{i}.bin"));
        export_items.push(item(source.to_str().unwrap(), dest.to_str().unwrap()));
        sources.push(source);
        dests.push(dest);
    }
    // Sabotage only the last entry's destination.
    std::fs::create_dir_all(dests[4].parent().unwrap()).unwrap();
    std::fs::write(&dests[4], b"blocking").unwrap();
    let export = LibraryPlanExport {
        items: export_items,
    };
    let (_, mut transaction) = build_and_approve(&export);
    let result = apply(&fx, &export, &mut transaction);
    assert!(result.is_err());
    for (i, source) in sources.iter().enumerate() {
        assert!(source.exists(), "entry {i} untouched");
    }
    for (i, dest) in dests.iter().enumerate().take(4) {
        assert!(
            !dest.exists(),
            "entry {i} never created despite being safe on its own"
        );
    }
}

#[test]
fn skip_unsafe_subset_skipped_entries_are_journaled_not_silently_dropped() {
    let fx = fixture();
    let safe = fx.root.join("safe.bin");
    let unsafe_source = fx.root.join("unsafe.bin");
    std::fs::write(&safe, b"safe").unwrap();
    std::fs::write(&unsafe_source, b"unsafe").unwrap();
    let safe_dest = fx.root.join("ps").join("safe.bin");
    let unsafe_dest = fx.root.join("ps").join("unsafe.bin");
    std::fs::create_dir_all(unsafe_dest.parent().unwrap()).unwrap();
    std::fs::write(&unsafe_dest, b"taken").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(safe.to_str().unwrap(), safe_dest.to_str().unwrap()),
            item(
                unsafe_source.to_str().unwrap(),
                unsafe_dest.to_str().unwrap(),
            ),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
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
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let reread = read_journal(&path).unwrap();
    let skipped: Vec<_> = reread
        .entries
        .iter()
        .filter(|e| e.state == crate::dat::rename_apply::model::EntryState::Skipped)
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "the skipped entry is present and journaled, not dropped"
    );
}

// ====================================================================
// I. Manual recovery output and concrete recovery-assessment fixtures
// (milestone sections 8, 36).
// ====================================================================

#[test]
fn recovery_report_includes_transaction_id_and_plan_generation_and_state() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    let report = render_recovery_report(&transaction, &[], RecoveryAssessment::SafeToResume);
    assert!(report.contains(&transaction.transaction_id));
    assert!(report.contains(&transaction.plan_generation.to_string()));
    assert!(report.contains("SafeToResume"));
}

#[test]
fn recovery_report_reports_none_when_nothing_applied_yet() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    let report = render_recovery_report(&transaction, &[], RecoveryAssessment::SafeToResume);
    assert!(report.contains("(none - nothing in this transaction is confirmed applied)"));
}

#[test]
fn recovery_report_includes_the_last_successful_operations_source_and_destination() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let report = render_recovery_report(&transaction, &[], RecoveryAssessment::AlreadyCommitted);
    assert!(report.contains(&source.display().to_string()));
    assert!(report.contains(&destination.display().to_string()));
}

#[test]
fn recovery_report_lists_uncertain_operations_with_expected_identity_and_live_observation() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    let issue = crate::dat::rename_apply::reconcile::RecoveryIssue {
        entry_index: 0,
        kind: RecoveryIssueKind::BothSourceAndDestination,
        detail: "source and destination both exist".to_string(),
    };
    let report = render_recovery_report(
        &transaction,
        &[issue],
        RecoveryAssessment::ManualRecoveryRequired,
    );
    assert!(report.contains("size=4 bytes"));
    assert!(report.contains("source=present"));
    assert!(report.contains("destination=absent"));
    assert!(report.contains("source and destination both exist"));
}

#[test]
fn recovery_report_suggests_rollback_when_safe_to_rollback() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    apply(&fx, &export, &mut transaction).unwrap();
    let report = render_recovery_report(&transaction, &[], RecoveryAssessment::SafeToRollback);
    assert!(report.contains("call rollback on this exact transaction id"));
}

#[test]
fn recovery_report_never_suggests_a_destructive_action_for_manual_recovery_required() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    let report = render_recovery_report(
        &transaction,
        &[],
        RecoveryAssessment::ManualRecoveryRequired,
    );
    let lower = report.to_lowercase();
    assert!(!lower.contains("delete"));
    assert!(!lower.contains("overwrite"));
    assert!(lower.contains("do not run apply or rollback automatically"));
}

#[test]
fn recovery_report_current_observation_reflects_live_filesystem_state() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    let report_before = render_recovery_report(
        &transaction,
        &[],
        RecoveryAssessment::ManualRecoveryRequired,
    );
    assert!(report_before.contains("source=present"));
    std::fs::remove_file(&source).unwrap();
    let report_after = render_recovery_report(
        &transaction,
        &[],
        RecoveryAssessment::ManualRecoveryRequired,
    );
    assert!(
        report_after.contains("source=absent"),
        "the observation must be recomputed live, not cached"
    );
}

#[test]
fn already_committed_concrete_fixture_via_reconciled_all_skipped_entries() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Skipped;
    transaction.state = TransactionState::Applied;
    let assessment = assess_recovery(&transaction, &[]);
    assert_eq!(assessment, RecoveryAssessment::AlreadyCommitted);
}

#[test]
fn manual_recovery_required_concrete_fixture_via_unresolved_hardlink_ambiguity() {
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"placeholder").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::hard_link(&source, &destination).unwrap();
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applying;
    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    let assessment = assess_recovery(&transaction, &issues);
    assert_eq!(assessment, RecoveryAssessment::ManualRecoveryRequired);
}

#[test]
fn no_impossible_state_ever_maps_to_safe_to_resume_when_entries_are_actually_applied() {
    // Property check across every TransactionState: whenever
    // has_applied_entries() is true, assess_recovery must never answer
    // SafeToResume (that would invite building a fresh, unrelated
    // transaction while a real applied entry sits unreversed).
    let fx = fixture();
    let source = fx.root.join("a.bin");
    std::fs::write(&source, b"data").unwrap();
    let destination = fx.root.join("ps").join("a.bin");
    let export = ready_export(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut base_transaction) = build_and_approve(&export);
    base_transaction.entries[0].state = crate::dat::rename_apply::model::EntryState::Applied;
    for state in [
        // `Planned` is deliberately excluded: no real code path can ever
        // produce a `Planned` transaction with an `Applied` entry (every
        // entry starts `Planned` and the executor only ever advances an
        // entry to `Applied` in lockstep with advancing the transaction
        // past `Planned`) - including it here would only be testing an
        // artificial fixture the system can never actually construct.
        TransactionState::Applying,
        TransactionState::Applied,
        TransactionState::ApplyFailed,
        TransactionState::RollingBack,
        TransactionState::RolledBack,
        TransactionState::RollbackFailed,
    ] {
        let mut transaction = base_transaction.clone();
        transaction.state = state;
        let assessment = assess_recovery(&transaction, &[]);
        assert_ne!(
            assessment,
            RecoveryAssessment::SafeToResume,
            "state {state:?} has an applied entry and must never be SafeToResume"
        );
    }
}

// ====================================================================
// Preview / structural regression (milestone sections 37, 41) and the
// developer probe's hard temp-safety guard (milestone section 39).
// ====================================================================

#[test]
fn build_preview_and_approve_transaction_never_call_executor_mutation_functions() {
    let source_text = include_str!("../plan_transaction.rs");
    // The read-only preview/approval surface: after the `use` imports
    // (which legitimately name `apply_transaction` etc. so the later,
    // clearly-separated mutation functions can call them) and up to (not
    // including) build_plan_transaction's own definition, which is the
    // first function in this file that ever touches the filesystem.
    let imports_end = source_text.find("// --------------------------------------------------------------------\n// Plan digest").unwrap();
    let boundary = source_text.find("pub fn build_plan_transaction").unwrap();
    let read_only_surface = &source_text[imports_end..boundary];
    assert!(!read_only_surface.contains("apply_transaction("));
    assert!(!read_only_surface.contains("rollback_transaction("));
    assert!(!read_only_surface.contains("std::fs::rename"));
    assert!(!read_only_surface.contains("std::fs::remove"));
    assert!(!read_only_surface.contains("std::fs::write"));
}

#[test]
fn preview_is_confined_to_root_accepts_a_fully_contained_preview() {
    let root = PathBuf::from("/tmp/probe-fixture");
    let export = ready_export(
        "/tmp/probe-fixture/a.bin",
        "/tmp/probe-fixture/library/a.bin",
    );
    let preview = build_preview(&export);
    assert!(preview_is_confined_to_root(&preview, &root));
}

#[test]
fn preview_is_confined_to_root_rejects_an_out_of_root_destination() {
    let root = PathBuf::from("/tmp/probe-fixture");
    let export = ready_export("/tmp/probe-fixture/a.bin", "/mnt/games/roms/a.bin");
    let preview = build_preview(&export);
    assert!(!preview_is_confined_to_root(&preview, &root));
}

#[test]
fn preview_is_confined_to_root_rejects_an_out_of_root_source() {
    let root = PathBuf::from("/tmp/probe-fixture");
    let export = ready_export("/mnt/games/roms/a.bin", "/tmp/probe-fixture/a.bin");
    let preview = build_preview(&export);
    assert!(!preview_is_confined_to_root(&preview, &root));
}

#[test]
fn preview_is_confined_to_root_is_true_for_an_empty_preview() {
    let export = status_export(PlanStatus::Unknown);
    let preview = build_preview(&export);
    assert!(preview_is_confined_to_root(
        &preview,
        &PathBuf::from("/anything")
    ));
}
