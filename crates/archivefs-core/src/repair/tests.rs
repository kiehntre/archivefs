//! Repair Center foundation tests: the proposal/plan/preflight/execute matrix
//! plus a false-mutation attack pass. No production code is changed by these
//! tests; every mutation goes through the reused rename engine.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::dat::rename_apply::identity::{capture_identity, identity_matches};
use crate::dat::rename_apply::journal::write_journal;
use crate::dat::rename_apply::model::{RenameTransaction, TransactionState};
use crate::repair::adapter::{repair_plan_from_rename_plan, repair_proposal_from_suggested_rename};
use crate::repair::execute::{
    RepairExecutionError, RepairExecutionOptions, RepairReverifyOutcome, apply_repair_transaction,
    build_repair_transaction, classify_persisted_transactions, execute_repair_plan,
    reverify_transaction, rollback_repair_transaction,
};
use crate::repair::plan::{PlanConflictKind, RepairPlan, RepairPlanId, build_repair_plan};
use crate::repair::preflight::{RepairPreflightStatus, run_repair_preflight};
use crate::repair::proposal::{
    DeferredActionKind, RepairAction, RepairEvidence, RepairEvidenceKind, RepairProposal,
    RepairProposalId, SafetyState,
};
use crate::safe_read::TrustedRoots;

fn proposal(id: &str, source: &Path, destination: &Path, identity: bool) -> RepairProposal {
    RepairProposal {
        id: RepairProposalId::new(id).unwrap(),
        action: RepairAction::RenamePath {
            destination: destination.to_path_buf(),
        },
        source_path: source.to_path_buf(),
        reason: "test".to_string(),
        evidence: vec![RepairEvidence::new(
            RepairEvidenceKind::UserRequestedOrganisation,
            "test",
        )],
        expected_source_identity: if identity {
            Some(capture_identity(source).unwrap())
        } else {
            None
        },
        originating_audit: None,
        safety: SafetyState::Safe,
        blockers: Vec::new(),
        warnings: Vec::new(),
        dat_source_id: None,
        dat_source_display: None,
        game_name: None,
        rom_name: None,
        verdict_label: None,
        match_confident: false,
        is_outer_archive: false,
        is_outer_archive_verified: false,
        survivor_path: None,
    }
}

fn move_proposal(id: &str, source: &Path, destination: &Path, identity: bool) -> RepairProposal {
    RepairProposal {
        action: RepairAction::MovePath {
            destination: destination.to_path_buf(),
        },
        ..proposal(id, source, destination, identity)
    }
}

fn plan(generation: u64, proposals: Vec<RepairProposal>) -> RepairPlan {
    build_repair_plan(
        RepairPlanId::new("plan-test").unwrap(),
        generation,
        10,
        None,
        proposals,
    )
}

fn options(journal_dir: &Path) -> RepairExecutionOptions {
    RepairExecutionOptions {
        trusted: TrustedRoots::from_paths([journal_dir]),
        journal_dir: journal_dir.to_path_buf(),
    }
}

fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}

/// A fully executable two-entry plan over two real files.
fn two_file_plan(dir: &Path, generation: u64) -> (RepairPlan, PathBuf, PathBuf, PathBuf, PathBuf) {
    let a = dir.join("a.bin");
    let b = dir.join("b.bin");
    let a_dest = dir.join("A.bin");
    let b_dest = dir.join("B.bin");
    std::fs::write(&a, b"a-content").unwrap();
    std::fs::write(&b, b"b-content").unwrap();
    let p = plan(
        generation,
        vec![
            proposal("a", &a, &a_dest, true),
            proposal("b", &b, &b_dest, true),
        ],
    );
    (p, a, b, a_dest, b_dest)
}

// ---------------------------------------------------------------------------
// PROPOSALS
// ---------------------------------------------------------------------------

#[test]
fn a_safe_proposal_builds_an_executable_plan() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    assert!(p.all_executable());
    assert!(!p.has_conflicts());
}

#[test]
fn a_needs_review_proposal_cannot_execute() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut reviewed = proposal("a", &source, &dir.path().join("A.bin"), true);
    reviewed.safety = SafetyState::NeedsReview;
    let p = plan(1, vec![reviewed]);
    let report = run_repair_preflight(&p, 1);
    assert_eq!(report.results[0].status, RepairPreflightStatus::NeedsReview);
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::NotExecutable { .. }));
}

#[test]
fn a_blocked_proposal_cannot_execute() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut blocked = proposal("a", &source, &dir.path().join("A.bin"), true);
    blocked.safety = SafetyState::Blocked;
    blocked.blockers = vec!["known unsafe".to_string()];
    let p = plan(1, vec![blocked]);
    let report = run_repair_preflight(&p, 1);
    assert_eq!(report.results[0].status, RepairPreflightStatus::Blocked);
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::NotExecutable { .. }));
}

#[test]
fn an_unsupported_future_action_cannot_execute() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut deferred = proposal("a", &source, &dir.path().join("A.bin"), true);
    deferred.action = RepairAction::Deferred(DeferredActionKind::DeleteDuplicate);
    let p = plan(1, vec![deferred]);
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::UnsupportedProposal)
    );
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::NotExecutable { .. }));
}

// ---------------------------------------------------------------------------
// BATCH CONFLICTS
// ---------------------------------------------------------------------------

#[test]
fn same_source_conflict_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![
            proposal("a", &source, &dir.path().join("A.bin"), true),
            proposal("b", &source, &dir.path().join("B.bin"), true),
        ],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::DuplicateSource)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
    // Nothing may have moved.
    assert!(source.exists());
    assert!(!dir.path().join("A.bin").exists());
    assert!(!dir.path().join("B.bin").exists());
}

#[test]
fn same_destination_conflict_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"y").unwrap();
    let p = plan(
        1,
        vec![
            proposal("a", &a, &dir.path().join("A.bin"), true),
            proposal("b", &b, &dir.path().join("A.bin"), true),
        ],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::DuplicateDestination)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

#[test]
fn destination_already_exists_conflict_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    std::fs::write(dir.path().join("A.bin"), b"taken").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::DestinationExists)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
    assert_eq!(std::fs::read(dir.path().join("A.bin")).unwrap(), b"taken");
}

#[test]
fn two_proposal_cycle_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &a, &b, true), proposal("b", &b, &a, true)],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::RenameCycle)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
    assert!(a.exists() && b.exists());
}

#[test]
fn three_proposal_cycle_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    let c = dir.path().join("c.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    std::fs::write(&c, b"c").unwrap();
    let p = plan(
        1,
        vec![
            proposal("a", &a, &b, true),
            proposal("b", &b, &c, true),
            proposal("c", &c, &a, true),
        ],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::RenameCycle)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

#[test]
fn parent_child_interference_blocks_execution() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("d").join("a.bin");
    std::fs::create_dir_all(dir.path().join("d")).unwrap();
    std::fs::write(&a, b"a").unwrap();
    let x = dir.path().join("x.bin");
    std::fs::write(&x, b"x").unwrap();
    // Second proposal's destination is the *parent directory* of the first.
    let p = plan(
        1,
        vec![
            proposal("a", &a, &dir.path().join("d").join("A.bin"), true),
            proposal("x", &x, &dir.path().join("d"), true),
        ],
    );
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::ParentChildInterference)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

// ---------------------------------------------------------------------------
// STALE EVIDENCE
// ---------------------------------------------------------------------------

#[test]
fn source_removed_after_proposal_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    std::fs::remove_file(&source).unwrap();
    let report = run_repair_preflight(&p, 1);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::MissingSource
    );
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::StaleSource { .. }));
}

#[test]
fn source_replaced_after_proposal_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, b"a replacement object").unwrap();
    let report = run_repair_preflight(&p, 1);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::ChangedSourceIdentity
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

#[test]
fn source_identity_changed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    std::fs::write(&source, b"a much longer payload").unwrap();
    let report = run_repair_preflight(&p, 1);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::ChangedSourceIdentity
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

#[test]
fn symlink_substitution_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), &source).unwrap();
    let report = run_repair_preflight(&p, 1);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::ChangedSourceIdentity
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
}

// ---------------------------------------------------------------------------
// EXECUTION
// ---------------------------------------------------------------------------

#[test]
fn a_successful_rename_executes_and_reverifies() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"payload").unwrap();
    let destination = dir.path().join("A.bin");
    let p = plan(1, vec![proposal("a", &source, &destination, true)]);
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), b"payload");
    assert_eq!(result.transaction.state, TransactionState::Applied);
    assert_eq!(result.transaction.applied_count(), 1);
    assert_eq!(result.reverify.len(), 1);
    assert_eq!(result.reverify[0].outcome, RepairReverifyOutcome::Verified);
    // The journal is durable and classified as complete.
    let report = classify_persisted_transactions(dir.path());
    assert_eq!(report.complete.len(), 1);
    assert!(report.recoverable.is_empty());
    assert!(report.corrupt.is_empty());
}

#[test]
fn a_successful_same_filesystem_move_executes() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("platform");
    std::fs::create_dir(&sub).unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"payload").unwrap();
    let destination = sub.join("a.bin");
    let p = plan(1, vec![move_proposal("a", &source, &destination, true)]);
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), b"payload");
    assert_eq!(result.transaction.applied_count(), 1);
}

// F. a duplicate-quarantine `MovePath` (survivor_path set) must never reach
// the generic executor: it re-validates only the source's own identity,
// never that the source is still a distinct-object duplicate of its
// survivor, so it refuses outright rather than silently accepting a move
// whose safety this code path cannot actually prove. Contrast with
// `a_successful_same_filesystem_move_executes` immediately above: an
// ordinary `MovePath` with no `survivor_path` is unaffected.
#[test]
fn a_move_proposal_with_a_survivor_path_is_refused_by_the_generic_executor() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("platform");
    std::fs::create_dir(&sub).unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"payload").unwrap();
    let destination = sub.join("a.bin");
    let mut quarantine_like = move_proposal("a", &source, &destination, true);
    quarantine_like.survivor_path = Some(dir.path().join("survivor.bin"));
    let p = plan(1, vec![quarantine_like]);

    let error = build_repair_transaction(&p).unwrap_err();
    assert!(
        matches!(error, RepairExecutionError::NotExecutable { .. }),
        "{error:?}"
    );
    let RepairExecutionError::NotExecutable { detail } = error else {
        unreachable!()
    };
    assert!(detail.contains("quarantine"), "{detail}");
    assert!(source.exists(), "nothing was moved");

    // The same refusal reaches every caller that goes through
    // `execute_repair_plan`, not just a direct `build_repair_transaction`
    // call.
    let error = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(error, RepairExecutionError::NotExecutable { .. }));
    assert!(source.exists(), "nothing was moved");
}

#[test]
fn destination_collision_at_apply_is_refused_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let destination = dir.path().join("A.bin");
    std::fs::write(&source, b"new").unwrap();
    // Build an executable plan, then create the destination after planning.
    let _ = plan(1, vec![proposal("a", &source, &destination, true)]);
    std::fs::write(&destination, b"precious").unwrap();
    // The plan-level destination-exists check runs at plan build; simulate a
    // destination that appears *after* planning by rebuilding with the file
    // absent, then create it between build and apply.
    std::fs::remove_file(&destination).unwrap();
    let p = plan(1, vec![proposal("a", &source, &destination, true)]);
    let mut transaction = build_repair_transaction(&p).unwrap();
    std::fs::write(&destination, b"appeared").unwrap();
    let outcome = apply_repair_transaction(&mut crate::repair::execute::RepairApplyExecution {
        transaction: &mut transaction,
        current_generation: 1,
        options: &options(dir.path()),
        cancel: &cancel(),
    });
    assert!(
        outcome.is_err(),
        "the appearing destination must be refused"
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"appeared");
    assert_eq!(std::fs::read(&source).unwrap(), b"new");
}

#[test]
fn exdev_cross_filesystem_move_is_refused() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"x").unwrap();
        let proc = std::path::Path::new("/proc");
        let dir_dev = std::fs::metadata(dir.path()).map(|m| m.dev()).ok();
        let proc_dev = std::fs::metadata(proc).map(|m| m.dev()).ok();
        if dir_dev.is_none() || proc_dev.is_none() || dir_dev == proc_dev {
            // No second filesystem observable in this environment.
            return;
        }
        let destination = proc.join("emuwiz-repair-exdev").join("a.bin");
        let p = plan(1, vec![move_proposal("a", &source, &destination, true)]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::InvalidDestination
        );
        assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
        assert!(source.exists(), "the source must not move");
    }
}

#[test]
fn no_overwrite_ever() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let destination = dir.path().join("A.bin");
    std::fs::write(&source, b"new").unwrap();
    std::fs::write(&destination, b"old").unwrap();
    // Plan build detects the destination exists and blocks it.
    let p = plan(1, vec![proposal("a", &source, &destination, true)]);
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::DestinationExists)
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"old");
    assert_eq!(std::fs::read(&source).unwrap(), b"new");
}

// ---------------------------------------------------------------------------
// ROLLBACK
// ---------------------------------------------------------------------------

#[test]
fn global_preflight_aborts_a_batch_where_any_entry_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let p = plan(
        1,
        vec![
            proposal("a", &a, &dir.path().join("A.bin"), true),
            proposal("b", &b, &dir.path().join("B.bin"), true),
        ],
    );
    // Invalidate entry b's source after the plan was built but before apply.
    std::fs::write(&b, b"b changed before apply").unwrap();
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(
        matches!(err, RepairExecutionError::StaleSource { .. })
            || matches!(err, RepairExecutionError::Apply(_)),
        "the batch must refuse before any mutation: {err:?}"
    );
    // Nothing was applied: the batch is all-or-nothing at build/preflight.
    assert!(a.exists() && b.exists());
    assert!(!dir.path().join("A.bin").exists());
    assert!(!dir.path().join("B.bin").exists());
}

#[test]
fn a_partially_applied_batch_rolls_back_and_reports_partial_result() {
    let dir = tempfile::tempdir().unwrap();
    let (p, a, b, a_dest, b_dest) = two_file_plan(dir.path(), 1);
    // Apply both entries through the real executor.
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    assert_eq!(result.transaction.applied_count(), 2);
    assert!(!a.exists() && !b.exists());
    assert!(a_dest.exists() && b_dest.exists());

    // Occupy the first source path so its rollback must fail; entry two
    // (reversed first) succeeds.
    std::fs::write(&a, b"someone else moved in").unwrap();

    let mut transaction = result.transaction;
    let rollback = rollback_repair_transaction(&mut transaction, dir.path(), &cancel()).unwrap();
    use crate::dat::rename_apply::model::RollbackResult;
    match rollback {
        RollbackResult::PartiallyRolledBack {
            rolled_back,
            failed,
        } => {
            assert_eq!(rolled_back, vec![b.clone()], "entry two reversed first");
            assert_eq!(failed.len(), 1, "entry one could not reverse");
            assert_eq!(failed[0].0, a.clone());
        }
        other => panic!("expected partial rollback, got {other:?}"),
    }
    // Entry two is fully back; entry one stayed at its destination (its source
    // is now occupied by the object we wrote there).
    assert!(b.exists() && !b_dest.exists());
    assert_eq!(std::fs::read(&a).unwrap(), b"someone else moved in");
    assert_eq!(std::fs::read(&a_dest).unwrap(), b"a-content");
    assert!(a_dest.exists());
    assert_eq!(transaction.state, TransactionState::RollbackFailed);
}

#[test]
fn a_clean_rollback_restores_everything_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let (p, a, b, _, _) = two_file_plan(dir.path(), 1);
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    let mut transaction = result.transaction;
    let rollback = rollback_repair_transaction(&mut transaction, dir.path(), &cancel()).unwrap();
    use crate::dat::rename_apply::model::RollbackResult;
    assert_eq!(rollback, RollbackResult::FullyRolledBack);
    assert!(a.exists() && b.exists());
    assert_eq!(transaction.state, TransactionState::RolledBack);
    // Idempotent: a second rollback is a safe no-op.
    let again = rollback_repair_transaction(&mut transaction, dir.path(), &cancel()).unwrap();
    assert_eq!(again, RollbackResult::FullyRolledBack);
    assert!(a.exists() && b.exists());
}

#[test]
fn rollback_destination_collision_is_reported_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let destination = dir.path().join("A.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(1, vec![proposal("a", &source, &destination, true)]);
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    // Occupy the original source path; rollback must refuse, not clobber.
    std::fs::write(&source, b"occupied").unwrap();
    let mut transaction = result.transaction;
    let rollback = rollback_repair_transaction(&mut transaction, dir.path(), &cancel()).unwrap();
    use crate::dat::rename_apply::model::RollbackResult;
    match rollback {
        RollbackResult::RollbackFailed { failed } => {
            assert_eq!(failed.len(), 1);
            assert!(failed[0].1.contains("occupied") || failed[0].1.contains("source path"));
        }
        other => panic!("expected RollbackFailed, got {other:?}"),
    }
    // The occupant is untouched; the destination still holds the moved file.
    assert_eq!(std::fs::read(&source).unwrap(), b"occupied");
    assert_eq!(std::fs::read(&destination).unwrap(), b"x");
}

// ---------------------------------------------------------------------------
// DRY RUN
// ---------------------------------------------------------------------------

#[test]
fn dry_run_never_mutates_and_surfaces_all_blockers() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let missing = dir.path().join("missing.bin");
    std::fs::write(&source, b"x").unwrap();
    std::fs::write(&missing, b"y").unwrap();
    // Capture both identities while both files exist...
    let a_proposal = proposal("a", &source, &dir.path().join("A.bin"), true);
    let b_proposal = proposal("b", &missing, &dir.path().join("B.bin"), true);
    // ...then remove `missing.bin` so preflight reports it missing.
    std::fs::remove_file(&missing).unwrap();
    let p = plan(1, vec![a_proposal, b_proposal]);
    let report = run_repair_preflight(&p, 1);
    assert!(!report.all_ready);
    assert_eq!(report.results.len(), 2);
    let missing_result = report
        .for_proposal(&RepairProposalId::new("b").unwrap())
        .unwrap();
    assert_eq!(missing_result.status, RepairPreflightStatus::MissingSource);
    // Nothing was created or moved.
    assert!(source.exists());
    assert!(!dir.path().join("A.bin").exists());
    assert!(!dir.path().join("B.bin").exists());
}

#[test]
fn dry_run_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let z = dir.path().join("z.bin");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&z, b"z").unwrap();
    let p = plan(
        1,
        vec![
            proposal("z", &z, &dir.path().join("Z.bin"), true),
            proposal("a", &a, &dir.path().join("A.bin"), true),
        ],
    );
    let first = run_repair_preflight(&p, 1);
    let second = run_repair_preflight(&p, 1);
    assert_eq!(first, second);
    assert_eq!(
        first
            .results
            .iter()
            .map(|r| r.proposal_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "z"]
    );
}

// ---------------------------------------------------------------------------
// DAT ADAPTER
// ---------------------------------------------------------------------------

use crate::dat::rename_plan::{
    ExtensionStatus, ProposalState, RenamePlan, RenameProposal, SourceObjectKind,
};

fn rename_proposal(source: &Path, proposed: Option<&str>, state: ProposalState) -> RenameProposal {
    RenameProposal {
        source_path: source.to_path_buf(),
        current_basename: source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        proposed_basename: proposed.map(str::to_string),
        platform: Some("arcade".to_string()),
        platform_display: Some("Arcade".to_string()),
        source_id: "mame".to_string(),
        source_display_name: "MAME".to_string(),
        game_name: Some("Game".to_string()),
        rom_name: Some("game.zip".to_string()),
        verdict_label: "Exact".to_string(),
        match_confident: true,
        explanations: vec!["verified DAT match".to_string()],
        content_policy: crate::dat::classification::ContentSelectionPolicy::GamesOnly,
        content_classification: crate::dat::classification::DatContentClassification::unknown(),
        original_metadata: Default::default(),
        state,
        object_kind: SourceObjectKind::RegularFile,
        ambiguity_reason: None,
        collision: None,
        blockers: Vec::new(),
        extension_status: Some(ExtensionStatus::Preserved),
        sanitisation_notes: Vec::new(),
        actionable: matches!(state, ProposalState::Suggested),
        audited_identity: Some(capture_identity(source).unwrap()),
        is_outer_archive: false,
    }
}

#[test]
fn a_safe_outer_archive_rename_becomes_a_safe_repair_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("game.zip");
    std::fs::write(&source, b"zipbytes").unwrap();
    let mut proposal = rename_proposal(&source, Some("Game (USA).zip"), ProposalState::Suggested);
    proposal.is_outer_archive = true;
    let repair = repair_proposal_from_suggested_rename(&proposal, 7).unwrap();
    assert_eq!(repair.safety, SafetyState::Safe);
    assert!(repair.action.is_executable());
    assert_eq!(
        repair.destination().unwrap(),
        &dir.path().join("Game (USA).zip")
    );
    assert!(repair.is_outer_archive);
    assert!(repair.is_outer_archive_verified);
    assert!(
        repair
            .evidence
            .iter()
            .any(|e| e.kind == RepairEvidenceKind::VerifiedWholeArchiveAttribution)
    );
    // The audited identity is preserved, not re-derived.
    assert!(
        repair
            .expected_source_identity
            .as_ref()
            .is_some_and(|identity| identity_matches(
                identity,
                &capture_identity(&source).unwrap()
            ))
    );
    assert_eq!(repair.originating_audit.unwrap().generation, 7);
}

#[test]
fn an_ambiguous_or_incomplete_dat_result_never_becomes_executable() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("game.zip");
    std::fs::write(&source, b"zipbytes").unwrap();
    for state in [
        ProposalState::Ambiguous,
        ProposalState::Unsupported,
        ProposalState::Blocked,
        ProposalState::AlreadyCanonical,
        ProposalState::ExcludedByContentPolicy,
        ProposalState::UnclassifiedContent,
        ProposalState::Conflict,
    ] {
        let proposal = rename_proposal(&source, Some("Game.zip"), state);
        assert!(
            repair_proposal_from_suggested_rename(&proposal, 1).is_none(),
            "{state:?} must never become an executable Repair proposal"
        );
    }
}

#[test]
fn stale_dat_evidence_fails_repair_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("game.zip");
    std::fs::write(&source, b"zipbytes").unwrap();
    let proposal = rename_proposal(&source, Some("Game (USA).zip"), ProposalState::Suggested);
    let repair = repair_proposal_from_suggested_rename(&proposal, 7).unwrap();
    let p = plan(7, vec![repair]);
    // The source changes after the proposal was built.
    std::fs::write(&source, b"different bytes entirely").unwrap();
    let report = run_repair_preflight(&p, 7);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::ChangedSourceIdentity
    );
    let err = execute_repair_plan(&p, 7, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::StaleSource { .. }));
}

#[test]
fn rename_plan_batch_adapter_builds_a_repair_plan() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("game.zip");
    std::fs::write(&source, b"zipbytes").unwrap();
    let proposal = rename_proposal(&source, Some("Game (USA).zip"), ProposalState::Suggested);
    let plan = RenamePlan {
        generation: 5,
        source_id: "mame".to_string(),
        source_display_name: "MAME".to_string(),
        scan_root: dir.path().to_string_lossy().into_owned(),
        platform: Some("arcade".to_string()),
        platform_display: Some("Arcade".to_string()),
        content_policy: crate::dat::classification::ContentSelectionPolicy::GamesOnly,
        classifier_version: crate::dat::classification::CLASSIFIER_VERSION.to_string(),
        proposals: vec![proposal],
        counts: Default::default(),
        audited_total: 1,
        verified_total: 1,
        truncated: false,
    };
    let repair_plan = repair_plan_from_rename_plan(&plan, 10);
    assert_eq!(repair_plan.generation, 5);
    assert_eq!(repair_plan.proposals.len(), 1);
    assert!(repair_plan.all_executable());
}

// ---------------------------------------------------------------------------
// RECOVERY
// ---------------------------------------------------------------------------

fn journaled_transaction(state: TransactionState) -> RenameTransaction {
    RenameTransaction {
        transaction_id: format!(
            "rec-{}",
            state.label().to_ascii_lowercase().replace(' ', "-")
        ),
        classifier_version: Some(crate::dat::classification::CLASSIFIER_VERSION.to_string()),
        plan_generation: 1,
        created_at_unix: 10,
        source_scan_root: String::new(),
        state,
        entries: Vec::new(),
        created_directories: Vec::new(),
        unknown: Default::default(),
    }
}

#[test]
fn an_interrupted_transaction_is_recognized() {
    let dir = tempfile::tempdir().unwrap();
    write_journal(
        dir.path(),
        &journaled_transaction(TransactionState::Applying),
    )
    .unwrap();
    write_journal(
        dir.path(),
        &journaled_transaction(TransactionState::Planned),
    )
    .unwrap();
    let report = classify_persisted_transactions(dir.path());
    assert_eq!(report.recoverable.len(), 2);
    assert!(report.corrupt.is_empty());
}

#[test]
fn a_completed_transaction_is_not_replayed() {
    let dir = tempfile::tempdir().unwrap();
    write_journal(
        dir.path(),
        &journaled_transaction(TransactionState::Applied),
    )
    .unwrap();
    write_journal(
        dir.path(),
        &journaled_transaction(TransactionState::RolledBack),
    )
    .unwrap();
    let report = classify_persisted_transactions(dir.path());
    assert_eq!(report.complete.len(), 2);
    assert!(report.recoverable.is_empty());
}

#[test]
fn a_malformed_journal_fails_closed_and_is_surfaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("corrupt.json"), "not json {{").unwrap();
    let report = classify_persisted_transactions(dir.path());
    assert!(report.recoverable.is_empty());
    assert_eq!(report.corrupt.len(), 1);
    // The corrupt journal is surfaced, never deleted.
    assert!(std::fs::symlink_metadata(dir.path().join("corrupt.json")).is_ok());
}

// ---------------------------------------------------------------------------
// FALSE-MUTATION ATTACK PASS
// ---------------------------------------------------------------------------

#[test]
fn replaying_the_same_plan_twice_cannot_mutate_again() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    let _ = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    // The source is gone; a second run of the same plan must refuse before any
    // mutation - either because the source is stale or because the destination
    // now exists. Both are refusals, never a re-mutation.
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(
        matches!(
            err,
            RepairExecutionError::StaleSource { .. } | RepairExecutionError::NotExecutable { .. }
        ),
        "the replayed plan must refuse: {err:?}"
    );
    assert!(!source.exists());
    assert_eq!(std::fs::read(dir.path().join("A.bin")).unwrap(), b"x");
}

#[test]
fn a_relative_path_escape_cannot_execute() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut escape = proposal("a", &source, &dir.path().join("A.bin"), true);
    escape.action = RepairAction::RenamePath {
        destination: PathBuf::from("../outside.bin"),
    };
    let p = plan(1, vec![escape]);
    let report = run_repair_preflight(&p, 1);
    assert_eq!(
        report.results[0].status,
        RepairPreflightStatus::InvalidDestination
    );
    assert!(execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).is_err());
    assert!(source.exists());
}

#[test]
fn reverify_detects_a_destination_replaced_after_apply() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    let result = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap();
    assert_eq!(result.reverify[0].outcome, RepairReverifyOutcome::Verified);
    // Replace the destination with a different object; re-verify must flag it.
    std::fs::write(
        &result.transaction.entries[0].destination_path,
        b"different",
    )
    .unwrap();
    let reverify = reverify_transaction(&result.transaction);
    assert_eq!(reverify[0].outcome, RepairReverifyOutcome::Changed);
}

#[test]
fn build_refuses_a_plan_with_any_conflict_or_needs_review() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut reviewed = proposal("a", &source, &dir.path().join("A.bin"), true);
    reviewed.safety = SafetyState::NeedsReview;
    let p = plan(1, vec![reviewed]);
    let err = build_repair_transaction(&p).unwrap_err();
    assert!(matches!(err, RepairExecutionError::NotExecutable { .. }));
}

// ---------------------------------------------------------------------------
// HOSTILE-REVIEW EXECUTION-VALIDATION GAPS
// ---------------------------------------------------------------------------

/// HIGH 1: the caller's actual current generation must be supplied and
/// enforced; it must never be derived from the plan itself.
#[test]
fn stale_generation_refuses_before_journal_or_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let destination = dir.path().join("A.bin");
    // Plan built at generation 7.
    let p = plan(7, vec![proposal("a", &source, &destination, true)]);
    // Execute with the caller's actual current generation 8.
    let err = execute_repair_plan(&p, 8, &options(dir.path()), &cancel()).unwrap_err();
    assert!(
        matches!(
            err,
            RepairExecutionError::StalePlan {
                plan: 7,
                current: 8
            }
        ),
        "got {err:?}"
    );
    // No journal was created and nothing moved.
    let journal_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .collect();
    assert!(journal_files.is_empty(), "no journal may be created");
    assert!(source.exists());
    assert!(!destination.exists());
}

/// HIGH 2: an executable proposal without audited source identity must be
/// refused at preflight, plan validation, transaction construction, and
/// execution - never auto-captured from whatever is at the path.
#[test]
fn safe_proposal_without_identity_refuses_before_any_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let destination = dir.path().join("A.bin");
    // Safe proposal, but deliberately no audited identity.
    let p = plan(1, vec![proposal("a", &source, &destination, false)]);
    // Plan validation flags it as unsupported.
    assert!(
        p.conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::UnsupportedProposal)
    );
    assert!(!p.all_executable());
    // Preflight blocks it.
    let report = run_repair_preflight(&p, 1);
    assert_eq!(report.results[0].status, RepairPreflightStatus::Blocked);
    // The source is replaced after the proposal was created; execution must
    // refuse on identity, before journal or mutation.
    std::fs::write(&source, b"a replacement object").unwrap();
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::NotExecutable { .. }));
    let journal_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .collect();
    assert!(journal_files.is_empty(), "no journal may be created");
    assert_eq!(std::fs::read(&source).unwrap(), b"a replacement object");
    assert!(!destination.exists());
}

/// HIGH 3: execution must re-validate destinations itself; a mutated plan
/// cannot smuggle a `..` destination past the dry-run.
#[test]
fn unsafe_dotdot_destination_refused_at_execute() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    let mut escape = proposal("a", &source, &dir.path().join("A.bin"), true);
    escape.action = RepairAction::RenamePath {
        destination: dir.path().join("sub").join("..").join("outside.bin"),
    };
    let p = plan(1, vec![escape]);
    // Plan build does not resolve the `..`; execution must refuse.
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::Build { .. }), "{err:?}");
    assert!(source.exists());
    assert!(!dir.path().join("sub").join("outside.bin").exists());
    assert!(!dir.path().join("outside.bin").exists());
}

/// HIGH 3: a RenamePath must never silently become a cross-directory move.
#[test]
fn cross_directory_rename_path_refused_at_execute() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let source = dir.path().join("a.bin");
    std::fs::write(&source, b"x").unwrap();
    // RenamePath with a destination in another directory on the same FS.
    let p = plan(1, vec![proposal("a", &source, &sub.join("A.bin"), true)]);
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(matches!(err, RepairExecutionError::Build { .. }), "{err:?}");
    assert!(source.exists());
    assert!(!sub.join("A.bin").exists());
}

/// HIGH 3: a duplicate source introduced *after* the plan was built must be
/// recomputed and refused at execution, not trusted from stored conflicts.
#[test]
fn post_build_duplicate_source_refused_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let other = dir.path().join("b.bin");
    std::fs::write(&source, b"a").unwrap();
    std::fs::write(&other, b"b").unwrap();
    let mut p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    assert!(p.all_executable());
    // Mutate the plan after it was built: a second proposal on the same source.
    p.proposals
        .push(proposal("b", &source, &dir.path().join("B.bin"), true));
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(
        matches!(err, RepairExecutionError::NotExecutable { .. }),
        "{err:?}"
    );
    assert!(source.exists() && other.exists());
    assert!(!dir.path().join("A.bin").exists());
    assert!(!dir.path().join("B.bin").exists());
}

/// HIGH 3: a duplicate destination introduced *after* the plan was built must
/// be recomputed and refused at execution.
#[test]
fn post_build_duplicate_destination_refused_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.bin");
    let other = dir.path().join("b.bin");
    std::fs::write(&source, b"a").unwrap();
    std::fs::write(&other, b"b").unwrap();
    let mut p = plan(
        1,
        vec![proposal("a", &source, &dir.path().join("A.bin"), true)],
    );
    assert!(p.all_executable());
    // Mutate the plan after it was built: a second proposal on the same
    // destination.
    p.proposals
        .push(proposal("b", &other, &dir.path().join("A.bin"), true));
    let err = execute_repair_plan(&p, 1, &options(dir.path()), &cancel()).unwrap_err();
    assert!(
        matches!(err, RepairExecutionError::NotExecutable { .. }),
        "{err:?}"
    );
    assert!(source.exists() && other.exists());
    assert!(!dir.path().join("A.bin").exists());
}
