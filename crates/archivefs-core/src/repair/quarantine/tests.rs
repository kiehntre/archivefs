//! Tests for duplicate-content quarantine planning, build, apply, and
//! rollback.
//!
//! Every fixture lives in a fresh `tempfile::tempdir()` acting as the
//! trusted scan root; nothing here ever touches a real library.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::dat::classification::{
    ContentSelectionPolicy, DatContentClassification, DatOriginalMetadata,
};
use crate::dat::rename_apply::model::{EntryState, ObjectIdentity, TransactionState};
use crate::dat::rename_apply::reconcile::{RecoveryIssueKind, reconcile_recovery};
use crate::dat::rename_plan::{ProposalState, RenameProposal, SourceObjectKind};
use crate::repair::proposal::{DeferredActionKind, RepairAction, RepairEvidenceKind};
use crate::safe_read::TrustedRoots;

use super::*;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).expect("write fixture");
    path
}

fn keeper(path: &Path, already_canonical: bool, verified_confident: bool) -> KeeperEvidence {
    KeeperEvidence {
        path: path.to_path_buf(),
        already_canonical,
        verified_confident,
        dat_source_id: Some("src".to_string()),
        dat_source_display: Some("Source".to_string()),
        game_name: Some("Game".to_string()),
        rom_name: Some("Game.zip".to_string()),
        verdict_label: Some("Exact".to_string()),
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    journal_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = dir.path().join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        Self { dir, journal_dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn trusted(&self) -> TrustedRoots {
        TrustedRoots::from_paths([self.root()])
    }
}

// ---------------------------------------------------------------------------
// select_survivor
// ---------------------------------------------------------------------------

#[test]
fn a_lone_canonical_verified_member_wins() {
    let fx = Fixture::new();
    let a = write(fx.root(), "a.zip", b"content");
    let b = write(fx.root(), "b.zip", b"content");
    let evidence = vec![keeper(&a, true, true), keeper(&b, false, false)];
    match select_survivor(&evidence) {
        SurvivorSelection::Survivor(survivor) => assert_eq!(survivor.path, a),
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_canonical_or_verified_member_needs_review() {
    let fx = Fixture::new();
    let a = write(fx.root(), "a.zip", b"content");
    let b = write(fx.root(), "b.zip", b"content");
    let evidence = vec![keeper(&a, false, false), keeper(&b, false, false)];
    assert!(matches!(
        select_survivor(&evidence),
        SurvivorSelection::NeedsReview { .. }
    ));
}

#[test]
fn two_equally_canonical_members_need_review() {
    let fx = Fixture::new();
    let a = write(fx.root(), "a.zip", b"content");
    let b = write(fx.root(), "b.zip", b"content");
    let evidence = vec![keeper(&a, true, false), keeper(&b, true, false)];
    assert!(matches!(
        select_survivor(&evidence),
        SurvivorSelection::NeedsReview { .. }
    ));
}

#[test]
fn a_canonical_only_and_a_verified_only_member_tie_and_need_review() {
    let fx = Fixture::new();
    let a = write(fx.root(), "a.zip", b"content");
    let b = write(fx.root(), "b.zip", b"content");
    // Neither reaches tier 2 (both signals); each is tier 1 alone - a tie.
    let evidence = vec![keeper(&a, true, false), keeper(&b, false, true)];
    assert!(matches!(
        select_survivor(&evidence),
        SurvivorSelection::NeedsReview { .. }
    ));
}

// ---------------------------------------------------------------------------
// plan_duplicate_quarantine
// ---------------------------------------------------------------------------

#[test]
fn canonical_member_survives_and_the_duplicate_is_planned_for_quarantine() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload");
    let redundant = write(fx.root(), "elsewhere/game (copy).zip", b"identical payload");
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan = plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None)
        .expect("a unique survivor exists");

    assert_eq!(plan.survivor.path, survivor);
    assert_eq!(plan.proposals.len(), 1);
    assert!(plan.skipped.is_empty());
    let proposal = &plan.proposals[0];
    assert_eq!(proposal.source_path, redundant);
    assert_eq!(proposal.safety, crate::repair::proposal::SafetyState::Safe);
    assert!(matches!(proposal.action, RepairAction::MovePath { .. }));
    assert!(
        proposal
            .evidence
            .iter()
            .any(|e| e.kind == RepairEvidenceKind::DuplicateContent)
    );
    assert!(
        proposal
            .destination()
            .unwrap()
            .starts_with(fx.root().join(QUARANTINE_DIRECTORY_NAME))
    );
    // The survivor never appears as a move source.
    assert!(plan.proposals.iter().all(|p| p.source_path != survivor));
}

#[test]
fn no_unique_survivor_produces_no_safe_plan() {
    let fx = Fixture::new();
    let a = write(fx.root(), "a.zip", b"content");
    let b = write(fx.root(), "b.zip", b"content");
    let evidence = vec![keeper(&a, false, false), keeper(&b, false, false)];

    let mut cache = DuplicateHashCache::new();
    let error = plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None)
        .unwrap_err();
    assert!(matches!(error, QuarantinePlanRefusal::NeedsReview { .. }));
}

#[test]
fn a_three_member_group_yields_one_survivor_and_two_moves() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "a/game.zip", b"same content everywhere");
    let redundant_b = write(fx.root(), "b/game.zip", b"same content everywhere");
    let redundant_c = write(fx.root(), "c/game.zip", b"same content everywhere");
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant_b, false, false),
        keeper(&redundant_c, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan = plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None)
        .expect("a unique survivor exists");
    assert_eq!(plan.proposals.len(), 2);
    let sources: Vec<&PathBuf> = plan.proposals.iter().map(|p| &p.source_path).collect();
    assert!(sources.contains(&&redundant_b));
    assert!(sources.contains(&&redundant_c));
    assert!(!sources.contains(&&survivor));
    // Deterministic destinations: distinct basenames even though every
    // proposal shares the same content-hash bucket.
    let destinations: Vec<&PathBuf> = plan
        .proposals
        .iter()
        .map(|p| p.destination().unwrap())
        .collect();
    assert_ne!(destinations[0], destinations[1]);
    let bucket_0 = destinations[0].parent().unwrap();
    let bucket_1 = destinations[1].parent().unwrap();
    assert_eq!(bucket_0, bucket_1, "same content -> same bucket");
}

#[cfg(unix)]
#[test]
fn a_hard_linked_member_is_skipped_never_moved() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "a.zip", b"shared content");
    let linked = fx.root().join("b.zip");
    std::fs::hard_link(&survivor, &linked).unwrap();
    let evidence = vec![keeper(&survivor, true, true), keeper(&linked, false, false)];

    let mut cache = DuplicateHashCache::new();
    let plan = plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None)
        .expect("a unique survivor exists");
    assert!(plan.proposals.is_empty(), "a hard link is never a move");
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].0, linked);
    assert!(plan.skipped[0].1.contains("same filesystem object"));
}

// ---------------------------------------------------------------------------
// quarantine_destination determinism
// ---------------------------------------------------------------------------

#[test]
fn quarantine_destination_is_deterministic_and_preserves_the_basename() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "a/game.zip", b"payload");
    let redundant = write(fx.root(), "b/game.zip", b"payload");
    let mut cache = DuplicateHashCache::new();
    let proof =
        prove_duplicate_content(&redundant, &survivor, &mut cache, &fx.trusted(), None).unwrap();

    let first = quarantine_destination(fx.root(), &proof, &redundant).unwrap();
    let second = quarantine_destination(fx.root(), &proof, &redundant).unwrap();
    assert_eq!(first, second);
    assert!(first.starts_with(fx.root().join(QUARANTINE_DIRECTORY_NAME)));
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("game.zip"),
        "{first:?}"
    );
}

// ---------------------------------------------------------------------------
// build_quarantine_transaction: live re-proof
// ---------------------------------------------------------------------------

#[test]
fn survivor_changed_between_plan_and_build_refuses() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "a.zip", b"payload one two three");
    let redundant = write(fx.root(), "b.zip", b"payload one two three");
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();

    // The survivor is rewritten (same size, different bytes) after planning.
    std::fs::write(&survivor, b"payload ONE TWO three").unwrap();

    let error = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap_err();
    assert!(error.contains("could not be re-proven"), "{error}");
}

#[test]
fn redundant_source_changed_between_plan_and_build_refuses() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "a.zip", b"payload one two three");
    let redundant = write(fx.root(), "b.zip", b"payload one two three");
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();

    std::fs::write(&redundant, b"payload ONE TWO three").unwrap();

    let error = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap_err();
    assert!(error.contains("could not be re-proven"), "{error}");
}

// ---------------------------------------------------------------------------
// End-to-end: build + apply
// ---------------------------------------------------------------------------

/// Plans, builds, and applies a two-member group; returns the fixture, the
/// applied transaction, and the original paths.
fn plan_build_apply(fx: &Fixture) -> (RenameTransaction, PathBuf, PathBuf) {
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    let outcome = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap();
    (outcome.transaction, survivor, redundant)
}

#[test]
fn a_quarantine_move_applies_and_the_journal_records_source_and_destination() {
    let fx = Fixture::new();
    let (tx, survivor, redundant) = plan_build_apply(&fx);

    assert_eq!(tx.entries.len(), 1);
    let entry = &tx.entries[0];
    assert_eq!(entry.state, EntryState::Applied);
    assert_eq!(entry.source_path, redundant);
    assert!(
        entry
            .destination_path
            .starts_with(fx.root().join(QUARANTINE_DIRECTORY_NAME))
    );

    assert!(!redundant.exists(), "the redundant copy left its old path");
    assert!(
        entry.destination_path.exists(),
        "the quarantine copy exists"
    );
    assert!(survivor.exists(), "the survivor was never touched");
    assert_eq!(std::fs::read(&survivor).unwrap(), b"identical payload data");

    // Reload from the durable journal, independent of the in-memory result.
    let reloaded = crate::dat::rename_apply::journal::read_journal(
        &crate::dat::rename_apply::journal::journal_path(&fx.journal_dir, &tx.transaction_id)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reloaded.entries[0].source_path, redundant);
    assert_eq!(reloaded.entries[0].destination_path, entry.destination_path);
    assert_eq!(reloaded.entries[0].state, EntryState::Applied);
}

#[test]
fn survivor_changed_immediately_before_mutation_aborts_the_whole_batch() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    // Survivor rewritten after build, before apply.
    std::fs::write(&survivor, b"DIFFERENT payload data!").unwrap();

    let error = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::dat::rename_apply::executor::ApplyError::Journal(_)
    ));
    assert_eq!(tx.state, TransactionState::ApplyFailed);
    assert!(redundant.exists(), "nothing was moved");
    assert!(
        tx.created_directories.is_empty(),
        "no directory was created"
    );
}

#[test]
fn redundant_source_changed_immediately_before_mutation_aborts() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    std::fs::write(&redundant, b"DIFFERENT payload data!").unwrap();

    let error = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::dat::rename_apply::executor::ApplyError::Journal(_)
    ));
    assert_eq!(tx.state, TransactionState::ApplyFailed);
    assert!(redundant.exists());
}

#[test]
fn a_destination_collision_refuses_without_clobbering() {
    let fx = Fixture::new();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    // Something already occupies the exact computed destination.
    let destination = tx.entries[0].destination_path.clone();
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"already here").unwrap();

    let error = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::dat::rename_apply::executor::ApplyError::HardConflicts(_)
    ));
    assert!(redundant.exists(), "the redundant file was never moved");
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"already here",
        "the pre-existing file at the destination was never clobbered"
    );
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

#[test]
fn rollback_restores_the_exact_original_path() {
    let fx = Fixture::new();
    let (mut tx, survivor, redundant) = plan_build_apply(&fx);

    let outcome =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert_eq!(
        outcome.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    );
    assert!(redundant.exists(), "the original path is restored exactly");
    assert_eq!(
        std::fs::read(&redundant).unwrap(),
        b"identical payload data"
    );
    assert!(survivor.exists());
}

#[test]
fn rollback_refuses_when_the_original_source_path_became_occupied() {
    let fx = Fixture::new();
    let (mut tx, _survivor, redundant) = plan_build_apply(&fx);

    // Something now occupies the original path the rollback would restore.
    std::fs::write(&redundant, b"an unrelated new file").unwrap();

    let outcome =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert!(matches!(
        outcome.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::RollbackFailed { .. }
    ));
    assert_eq!(
        std::fs::read(&redundant).unwrap(),
        b"an unrelated new file",
        "the occupying file was never clobbered"
    );
}

#[test]
fn rollback_refuses_when_the_quarantined_file_identity_changed() {
    let fx = Fixture::new();
    let (mut tx, _survivor, _redundant) = plan_build_apply(&fx);

    let destination = tx.entries[0].destination_path.clone();
    std::fs::write(&destination, b"tampered while quarantined!!").unwrap();

    let outcome =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert!(matches!(
        outcome.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::RollbackFailed { .. }
    ));
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"tampered while quarantined!!"
    );
}

#[test]
fn rollback_removes_an_owned_empty_quarantine_directory() {
    let fx = Fixture::new();
    let (mut tx, _survivor, _redundant) = plan_build_apply(&fx);
    assert_eq!(tx.created_directories.len(), 2, "quarantine root + bucket");

    let outcome =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert_eq!(outcome.directories_removed.len(), 2);
    assert!(outcome.directories_remaining.is_empty());
    assert!(!fx.root().join(QUARANTINE_DIRECTORY_NAME).exists());
}

#[test]
fn rollback_never_removes_a_pre_existing_quarantine_directory() {
    let fx = Fixture::new();
    // The quarantine root already exists (and is non-empty) before anything
    // in this run touches it.
    let quarantine_root = fx.root().join(QUARANTINE_DIRECTORY_NAME);
    std::fs::create_dir_all(&quarantine_root).unwrap();
    write(&quarantine_root, "pre-existing.txt", b"not ours");

    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();
    apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap();

    // Only the bucket directory is ever owned; the pre-existing root is not.
    assert_eq!(tx.created_directories.len(), 1);

    let outcome =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert_eq!(outcome.directories_removed.len(), 1, "the owned bucket dir");
    assert!(
        quarantine_root.exists(),
        "the pre-existing quarantine root is never removed"
    );
    assert!(quarantine_root.join("pre-existing.txt").exists());
}

// ---------------------------------------------------------------------------
// Crash recovery: reconcile_recovery over a quarantine-shaped entry
// ---------------------------------------------------------------------------

fn quarantine_shaped_entry(
    source: &Path,
    destination: &Path,
    identity: ObjectIdentity,
    state: EntryState,
) -> crate::dat::rename_apply::model::TransactionEntry {
    crate::dat::rename_apply::model::TransactionEntry {
        source_path: source.to_path_buf(),
        destination_path: destination.to_path_buf(),
        original_basename: source.file_name().unwrap().to_string_lossy().into_owned(),
        proposed_basename: destination
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        identity,
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    }
}

fn quarantine_shaped_transaction(
    entry: crate::dat::rename_apply::model::TransactionEntry,
) -> RenameTransaction {
    RenameTransaction {
        transaction_id: "quarantine-crash-test".to_string(),
        plan_generation: 1,
        classifier_version: Some(CLASSIFIER_VERSION.to_string()),
        created_at_unix: 1,
        source_scan_root: "/tmp/roms".to_string(),
        state: TransactionState::Applying,
        entries: vec![entry],
        created_directories: Vec::new(),
        unknown: Default::default(),
    }
}

#[test]
fn reconcile_source_exists_only_means_the_move_did_not_happen() {
    let fx = Fixture::new();
    let source = write(fx.root(), "a.zip", b"content");
    let destination = fx
        .root()
        .join(QUARANTINE_DIRECTORY_NAME)
        .join("bucket/a.zip");
    let identity = crate::dat::rename_apply::identity::capture_identity(&source).unwrap();

    let mut tx = quarantine_shaped_transaction(quarantine_shaped_entry(
        &source,
        &destination,
        identity,
        EntryState::Applying,
    ));
    let issues = reconcile_recovery(&mut tx, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameDidNotHappen);
    assert_eq!(tx.entries[0].state, EntryState::Skipped);
}

#[test]
fn reconcile_destination_exists_only_means_the_move_is_confirmed() {
    let fx = Fixture::new();
    let source = write(fx.root(), "a.zip", b"content");
    let destination_dir = fx.root().join(QUARANTINE_DIRECTORY_NAME).join("bucket");
    std::fs::create_dir_all(&destination_dir).unwrap();
    let identity = crate::dat::rename_apply::identity::capture_identity(&source).unwrap();
    let destination = destination_dir.join("a.zip");
    std::fs::rename(&source, &destination).unwrap();

    let mut tx = quarantine_shaped_transaction(quarantine_shaped_entry(
        &source,
        &destination,
        identity,
        EntryState::Applying,
    ));
    let issues = reconcile_recovery(&mut tx, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::RenameConfirmed);
    assert_eq!(tx.entries[0].state, EntryState::Applied);
}

#[test]
fn reconcile_both_present_requires_manual_review() {
    let fx = Fixture::new();
    let source = write(fx.root(), "a.zip", b"content");
    let destination_dir = fx.root().join(QUARANTINE_DIRECTORY_NAME).join("bucket");
    std::fs::create_dir_all(&destination_dir).unwrap();
    let destination = destination_dir.join("a.zip");
    // A hard link, so both paths exist with a matching identity.
    std::fs::hard_link(&source, &destination).unwrap();
    let identity = crate::dat::rename_apply::identity::capture_identity(&source).unwrap();

    let mut tx = quarantine_shaped_transaction(quarantine_shaped_entry(
        &source,
        &destination,
        identity,
        EntryState::Applying,
    ));
    let issues = reconcile_recovery(&mut tx, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::BothSourceAndDestination);
    assert_eq!(tx.entries[0].state, EntryState::Applying, "left unresolved");
}

#[test]
fn reconcile_neither_present_requires_manual_review() {
    let fx = Fixture::new();
    let stand_in = write(fx.root(), "stand-in.zip", b"content");
    let identity = crate::dat::rename_apply::identity::capture_identity(&stand_in).unwrap();
    let source = fx.root().join("gone.zip");
    let destination = fx
        .root()
        .join(QUARANTINE_DIRECTORY_NAME)
        .join("bucket/gone.zip");

    let mut tx = quarantine_shaped_transaction(quarantine_shaped_entry(
        &source,
        &destination,
        identity,
        EntryState::Applying,
    ));
    let issues = reconcile_recovery(&mut tx, &fx.journal_dir).unwrap();
    assert_eq!(issues[0].kind, RecoveryIssueKind::BothAbsent);
    assert_eq!(tx.entries[0].state, EntryState::Applying, "left unresolved");
}

// ---------------------------------------------------------------------------
// DeleteDuplicate remains Deferred and non-executable
// ---------------------------------------------------------------------------

#[test]
fn delete_duplicate_remains_deferred_and_non_executable() {
    let action = RepairAction::Deferred(DeferredActionKind::DeleteDuplicate);
    assert!(!action.is_executable());
    assert!(action.destination().is_none());
}

// ---------------------------------------------------------------------------
// keeper_evidence_from_rename_proposal bridge
// ---------------------------------------------------------------------------

fn rename_proposal_fixture(state: ProposalState, match_confident: bool) -> RenameProposal {
    RenameProposal {
        source_path: PathBuf::from("/roms/a.zip"),
        current_basename: "a.zip".to_string(),
        proposed_basename: Some("A (USA).zip".to_string()),
        platform: None,
        platform_display: None,
        source_id: "no-intro-sms".to_string(),
        source_display_name: "No-Intro SMS".to_string(),
        game_name: Some("A".to_string()),
        rom_name: Some("A (USA).zip".to_string()),
        verdict_label: "Exact".to_string(),
        match_confident,
        explanations: Vec::new(),
        content_policy: ContentSelectionPolicy::AllEntries,
        content_classification: DatContentClassification::unknown(),
        original_metadata: DatOriginalMetadata::default(),
        state,
        object_kind: SourceObjectKind::RegularFile,
        ambiguity_reason: None,
        collision: None,
        blockers: Vec::new(),
        extension_status: None,
        sanitisation_notes: Vec::new(),
        actionable: state == ProposalState::Suggested,
        audited_identity: None,
        is_outer_archive: false,
    }
}

#[test]
fn the_bridge_carries_already_canonical_and_confidence_objectively() {
    let path = PathBuf::from("/roms/a.zip");
    let already_canonical = rename_proposal_fixture(ProposalState::AlreadyCanonical, true);
    let evidence = keeper_evidence_from_rename_proposal(&path, &already_canonical);
    assert!(evidence.already_canonical);
    assert!(evidence.verified_confident);
    assert_eq!(evidence.dat_source_id.as_deref(), Some("no-intro-sms"));
    assert_eq!(evidence.game_name.as_deref(), Some("A"));

    let suggested_unconfident = rename_proposal_fixture(ProposalState::Suggested, false);
    let evidence2 = keeper_evidence_from_rename_proposal(&path, &suggested_unconfident);
    assert!(!evidence2.already_canonical);
    assert!(!evidence2.verified_confident);
}

// ---------------------------------------------------------------------------
// Multi-entry end-to-end: A survivor, B and C redundant
// ---------------------------------------------------------------------------

/// Builds a real three-member (A survivor, B/C redundant) plan and
/// transaction, in that evidence order, so `entries[0]` is always B and
/// `entries[1]` is always C.
fn three_member_transaction(fx: &Fixture) -> (RenameTransaction, PathBuf, PathBuf, PathBuf) {
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant_b = write(fx.root(), "b/game.zip", b"identical payload data");
    let redundant_c = write(fx.root(), "c/game.zip", b"identical payload data");
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant_b, false, false),
        keeper(&redundant_c, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    assert_eq!(plan.proposals.len(), 2);
    assert_eq!(
        plan.proposals[0].source_path, redundant_b,
        "B is proposals[0] because it was listed before C"
    );
    assert_eq!(plan.proposals[1].source_path, redundant_c);

    let tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();
    assert_eq!(tx.entries.len(), 2);
    assert_eq!(tx.entries[0].source_path, redundant_b);
    assert_eq!(tx.entries[1].source_path, redundant_c);
    (tx, survivor, redundant_b, redundant_c)
}

#[test]
fn a_three_member_group_applies_both_moves_and_survivor_is_untouched() {
    let fx = Fixture::new();
    let (mut tx, survivor, redundant_b, redundant_c) = three_member_transaction(&fx);
    let mut cache = DuplicateHashCache::new();

    let outcome = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap();

    assert_eq!(outcome.summary.applied, 2);
    assert_eq!(outcome.summary.failed, 0);
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    for entry in &outcome.transaction.entries {
        assert_eq!(entry.state, EntryState::Applied);
    }

    // Survivor remains untouched throughout.
    assert!(survivor.exists());
    assert_eq!(std::fs::read(&survivor).unwrap(), b"identical payload data");

    // B and C exist only at their quarantine destinations.
    assert!(!redundant_b.exists());
    assert!(!redundant_c.exists());
    let destination_b = outcome.transaction.entries[0].destination_path.clone();
    let destination_c = outcome.transaction.entries[1].destination_path.clone();
    assert!(destination_b.exists());
    assert!(destination_c.exists());
    assert_ne!(destination_b, destination_c);

    // Rollback restores both to their exact original paths, and the
    // survivor stays untouched.
    let mut tx = outcome.transaction;
    let rollback =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert_eq!(
        rollback.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack
    );
    assert!(redundant_b.exists());
    assert!(redundant_c.exists());
    assert!(!destination_b.exists());
    assert!(!destination_c.exists());
    assert!(survivor.exists());
    assert_eq!(std::fs::read(&survivor).unwrap(), b"identical payload data");
}

// ---------------------------------------------------------------------------
// Mid-batch survivor change: B moves, then A changes, then C must not move
// ---------------------------------------------------------------------------

#[test]
fn a_survivor_change_between_two_entries_stops_the_later_move_without_data_loss() {
    let fx = Fixture::new();
    let (mut tx, survivor, redundant_b, redundant_c) = three_member_transaction(&fx);
    let mut cache = DuplicateHashCache::new();

    // Deterministic hook: right before entry 1 (C) is re-proven and moved,
    // mutate the survivor. Entry 0 (B) has already been re-proven and moved
    // by this point - no thread, no sleep, no race.
    let outcome = apply_quarantine_transaction_checkpointed(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
        &mut |checkpoint| {
            if checkpoint == (ApplyCheckpoint::BeforeEntry { index: 1 }) {
                std::fs::write(&survivor, b"the survivor changed mid-batch!!").unwrap();
            }
        },
    )
    .unwrap();

    // B succeeded; C did not.
    assert_eq!(outcome.transaction.entries[0].source_path, redundant_b);
    assert_eq!(outcome.transaction.entries[0].state, EntryState::Applied);
    assert_eq!(outcome.transaction.entries[1].source_path, redundant_c);
    assert_eq!(
        outcome.transaction.entries[1].state,
        EntryState::ApplyFailed
    );
    assert!(outcome.transaction.entries[1].failure_reason.is_some());
    assert_eq!(outcome.transaction.state, TransactionState::ApplyFailed);
    assert_eq!(outcome.summary.applied, 1);
    assert_eq!(outcome.summary.failed, 1);

    // No overwrite, no data loss: C is exactly where it started, byte for
    // byte; B is exactly at its quarantine destination.
    assert!(!redundant_b.exists());
    let destination_b = outcome.transaction.entries[0].destination_path.clone();
    assert!(destination_b.exists());
    assert!(redundant_c.exists(), "C was never moved");
    assert_eq!(
        std::fs::read(&redundant_c).unwrap(),
        b"identical payload data",
        "C's content is untouched"
    );

    // The persisted journal, read back independently, shows exactly this:
    // B moved, C not moved/failed.
    let reloaded = crate::dat::rename_apply::journal::read_journal(
        &crate::dat::rename_apply::journal::journal_path(
            &fx.journal_dir,
            &outcome.transaction.transaction_id,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reloaded.entries[0].state, EntryState::Applied);
    assert_eq!(reloaded.entries[1].state, EntryState::ApplyFailed);

    // Rollback/recovery can still restore B.
    let mut tx = outcome.transaction;
    let rollback =
        rollback_quarantine_transaction(&mut tx, &fx.journal_dir, &no_cancel(), fx.root()).unwrap();
    assert_eq!(
        rollback.rollback.result,
        crate::dat::rename_apply::model::RollbackResult::FullyRolledBack,
        "the one Applied entry (B) rolls back cleanly"
    );
    assert!(
        redundant_b.exists(),
        "B is restored to its exact original path"
    );
    assert!(!destination_b.exists());
}

// ---------------------------------------------------------------------------
// Symlinked quarantine directories fail closed before mutation
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_symlinked_quarantine_root_refuses_before_any_mutation() {
    let fx = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    // `.emuwiz-quarantine` is a symlink to a directory entirely outside the
    // trusted root.
    std::os::unix::fs::symlink(outside.path(), fx.root().join(QUARANTINE_DIRECTORY_NAME)).unwrap();

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    let error = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap_err();
    assert!(
        matches!(error, crate::dat::rename_apply::executor::ApplyError::Journal(ref detail) if detail.contains("symlink")),
        "{error:?}"
    );

    // Nothing was mutated: the redundant source is untouched, the survivor
    // is untouched, and nothing landed outside the trusted root.
    assert!(redundant.exists());
    assert_eq!(
        std::fs::read(&redundant).unwrap(),
        b"identical payload data"
    );
    assert!(survivor.exists());
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing was ever written outside the trusted root"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_content_bucket_directory_refuses_before_any_mutation() {
    let fx = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let survivor = write(fx.root(), "keep/game.zip", b"identical payload data");
    let redundant = write(
        fx.root(),
        "elsewhere/game (copy).zip",
        b"identical payload data",
    );
    let evidence = vec![
        keeper(&survivor, true, true),
        keeper(&redundant, false, false),
    ];

    let mut cache = DuplicateHashCache::new();
    let plan =
        plan_duplicate_quarantine(&evidence, fx.root(), &fx.trusted(), &mut cache, None).unwrap();
    let mut tx = build_quarantine_transaction(
        &plan.proposals,
        &survivor,
        fx.root(),
        1,
        &mut cache,
        &fx.trusted(),
        None,
    )
    .unwrap();

    // The quarantine root is a real, legitimate directory, but the specific
    // content-hash bucket this move needs is a symlink out of the trust
    // boundary.
    let bucket = tx.entries[0]
        .destination_path
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(bucket.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &bucket).unwrap();

    let mut cache = DuplicateHashCache::new();
    let error = apply_quarantine_transaction(
        &mut tx,
        &survivor,
        fx.root(),
        1,
        fx.trusted(),
        &fx.journal_dir,
        &no_cancel(),
        &mut cache,
    )
    .unwrap_err();
    assert!(
        matches!(error, crate::dat::rename_apply::executor::ApplyError::Journal(ref detail) if detail.contains("symlink")),
        "{error:?}"
    );

    assert!(redundant.exists());
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing was ever written outside the trusted root"
    );
}
