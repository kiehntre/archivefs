//! Batch 16: transaction closeout before the first real canary.
//!
//! Covers exactly the milestone's own five closeout categories:
//! set-aware `SkipUnsafeSubset` (never a partial-set move), multi-set
//! atomicity, `AlreadySettled`/terminal-state regressions, fresh
//! reapproval/new-transaction-id semantics, the first-real-canary
//! eligibility model, and a handful of journal/rollback edge cases beyond
//! Batch 15's own matrix. All mutation is tempdir-only.

use super::*;
use crate::dat::rename_apply::journal::{journal_path, read_journal};
use crate::dat::rename_apply::model::{RollbackResult, TransactionState};
use crate::dat::rename_apply::reconcile::reconcile_recovery;
use crate::platform_evidence_fusion::library_plan_export::SourcePrecondition;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

// ------------------------------------------------------------------
// Shared helpers (mirrors the sibling test modules' own private helpers)
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

fn set_item(source: &str, destination: &str, set_label: &str) -> LibraryPlanExportItem {
    let mut export_item = item(source, destination);
    export_item.set_label = Some(set_label.to_string());
    export_item.set_destination = Some(format!("{set_label}-dir"));
    export_item
}

fn support_item(source: &str, destination: &str, set_label: &str) -> LibraryPlanExportItem {
    let mut export_item = set_item(source, destination, set_label);
    export_item.support_role = Some("manual".to_string());
    export_item.support_association = Some("attached".to_string());
    export_item
}

fn hashed_item(source: &str, destination: &str) -> LibraryPlanExportItem {
    let mut export_item = item(source, destination);
    export_item.precondition.physical_hash = Some("deadbeef".to_string());
    export_item
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
    let approved = approve_transaction(&preview, "test acknowledgement").unwrap();
    let transaction = build_plan_transaction(export, &approved, "test-root").unwrap();
    (approved, transaction)
}

fn entry_state(transaction: &RenameTransaction, source: &Path) -> EntryState {
    transaction
        .entries
        .iter()
        .find(|entry| entry.source_path == source)
        .expect("entry present")
        .state
}

fn write_rom(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"rom data").unwrap();
}

// ==================================================================
// A. Set-aware SkipUnsafeSubset (>= 15) - milestone sections 3-9
// ==================================================================

#[test]
fn a_single_unsafe_member_never_leaves_a_partial_set_move() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    let m3u = fx.root.join("incoming").join("game.m3u");
    write_rom(&disc1);
    write_rom(&disc2);
    write_rom(&m3u);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    let m3u_dest = fx.root.join("lib").join("game.m3u");
    // Sabotage: pre-create disc2's destination so its own preflight fails.
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
            ),
            set_item(m3u.to_str().unwrap(), m3u_dest.to_str().unwrap(), "game"),
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

    // Never "Disc1 moved, Disc2 skipped, playlist moved": all three must be
    // skipped together, and disc1/m3u must still be at their ORIGINAL
    // location, not the destination.
    assert_eq!(
        entry_state(&outcome.transaction, &disc1),
        EntryState::Skipped
    );
    assert_eq!(
        entry_state(&outcome.transaction, &disc2),
        EntryState::Skipped
    );
    assert_eq!(entry_state(&outcome.transaction, &m3u), EntryState::Skipped);
    assert!(disc1.exists() && !disc1_dest.exists());
    assert!(m3u.exists() && !m3u_dest.exists());
}

#[test]
fn a_fully_safe_set_applies_every_member() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
            ),
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

    assert_eq!(
        entry_state(&outcome.transaction, &disc1),
        EntryState::Applied
    );
    assert_eq!(
        entry_state(&outcome.transaction, &disc2),
        EntryState::Applied
    );
    assert!(disc1_dest.exists() && disc2_dest.exists());
}

#[test]
fn an_ungrouped_unsafe_entry_is_still_only_individually_skipped() {
    let fx = fixture();
    let solo = fx.root.join("incoming").join("Solo.bin");
    let other = fx.root.join("incoming").join("Other.bin");
    write_rom(&solo);
    write_rom(&other);
    let solo_dest = fx.root.join("lib").join("Solo.bin");
    let other_dest = fx.root.join("lib").join("Other.bin");
    std::fs::create_dir_all(solo_dest.parent().unwrap()).unwrap();
    std::fs::write(&solo_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            item(solo.to_str().unwrap(), solo_dest.to_str().unwrap()),
            item(other.to_str().unwrap(), other_dest.to_str().unwrap()),
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

    assert_eq!(
        entry_state(&outcome.transaction, &solo),
        EntryState::Skipped
    );
    assert_eq!(
        entry_state(&outcome.transaction, &other),
        EntryState::Applied
    );
}

#[test]
fn set_skip_leaves_zero_bytes_moved_on_disk_for_any_member() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    std::fs::create_dir_all(disc1_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc1_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
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

    assert!(disc2.exists(), "disc2 must never have moved");
    assert!(!disc2_dest.exists());
}

#[test]
fn set_skip_is_journaled_for_every_member_not_silently_dropped() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
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
    let persisted = read_journal(&path).unwrap();
    assert_eq!(entry_state(&persisted, &disc1), EntryState::Skipped);
    assert_eq!(entry_state(&persisted, &disc2), EntryState::Skipped);
}

#[test]
fn abort_all_with_an_unsafe_set_member_refuses_the_whole_batch_before_mutation() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
            ),
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
    assert!(disc1.exists() && !disc1_dest.exists());
}

#[test]
fn set_label_survives_a_journal_round_trip() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    write_rom(&disc1);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let export = LibraryPlanExport {
        items: vec![set_item(
            disc1.to_str().unwrap(),
            disc1_dest.to_str().unwrap(),
            "game",
        )],
    };
    let (_, transaction) = build_and_approve(&export);
    assert_eq!(
        set_label_of(&transaction.entries[0]),
        Some("game".to_string())
    );

    write_journal(&fx.journal_dir, &transaction).unwrap();
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let persisted = read_journal(&path).unwrap();
    assert_eq!(
        set_label_of(&persisted.entries[0]),
        Some("game".to_string())
    );
}

#[test]
fn two_member_set_first_unsafe_skips_both() {
    let fx = fixture();
    let a = fx.root.join("incoming").join("a.bin");
    let b = fx.root.join("incoming").join("b.bin");
    write_rom(&a);
    write_rom(&b);
    let a_dest = fx.root.join("lib").join("a.bin");
    let b_dest = fx.root.join("lib").join("b.bin");
    std::fs::create_dir_all(a_dest.parent().unwrap()).unwrap();
    std::fs::write(&a_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(a.to_str().unwrap(), a_dest.to_str().unwrap(), "set"),
            set_item(b.to_str().unwrap(), b_dest.to_str().unwrap(), "set"),
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
    assert_eq!(entry_state(&outcome.transaction, &a), EntryState::Skipped);
    assert_eq!(entry_state(&outcome.transaction, &b), EntryState::Skipped);
}

#[test]
fn two_member_set_second_unsafe_skips_both_order_independence() {
    let fx = fixture();
    let a = fx.root.join("incoming").join("a.bin");
    let b = fx.root.join("incoming").join("b.bin");
    write_rom(&a);
    write_rom(&b);
    let a_dest = fx.root.join("lib").join("a.bin");
    let b_dest = fx.root.join("lib").join("b.bin");
    std::fs::create_dir_all(b_dest.parent().unwrap()).unwrap();
    std::fs::write(&b_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(a.to_str().unwrap(), a_dest.to_str().unwrap(), "set"),
            set_item(b.to_str().unwrap(), b_dest.to_str().unwrap(), "set"),
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
    assert_eq!(entry_state(&outcome.transaction, &a), EntryState::Skipped);
    assert_eq!(entry_state(&outcome.transaction, &b), EntryState::Skipped);
}

#[test]
fn set_aware_skip_does_not_affect_an_unrelated_ungrouped_entry_in_the_same_batch() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    let solo = fx.root.join("incoming").join("Solo.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    write_rom(&solo);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    let solo_dest = fx.root.join("lib").join("Solo.bin");
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
            ),
            item(solo.to_str().unwrap(), solo_dest.to_str().unwrap()),
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
    assert_eq!(
        entry_state(&outcome.transaction, &disc1),
        EntryState::Skipped
    );
    assert_eq!(
        entry_state(&outcome.transaction, &solo),
        EntryState::Applied
    );
    assert!(solo_dest.exists());
}

#[test]
fn a_three_member_multi_disc_set_with_one_unsafe_member_skips_all_three() {
    let fx = fixture();
    let discs: Vec<PathBuf> = (1..=3)
        .map(|n| fx.root.join("incoming").join(format!("Disc {n}.bin")))
        .collect();
    for disc in &discs {
        write_rom(disc);
    }
    let dests: Vec<PathBuf> = (1..=3)
        .map(|n| fx.root.join("lib").join(format!("Disc {n}.bin")))
        .collect();
    std::fs::create_dir_all(dests[2].parent().unwrap()).unwrap();
    std::fs::write(&dests[2], b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: discs
            .iter()
            .zip(dests.iter())
            .map(|(s, d)| set_item(s.to_str().unwrap(), d.to_str().unwrap(), "trilogy"))
            .collect(),
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
    for disc in &discs {
        assert_eq!(entry_state(&outcome.transaction, disc), EntryState::Skipped);
        assert!(disc.exists());
    }
}

#[test]
fn rollback_after_a_whole_set_skip_is_a_safe_no_op() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
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

    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    assert!(disc1.exists() && !disc1_dest.exists());
}

#[test]
fn a_skipped_set_members_preflight_failure_includes_not_approved() {
    let fx = fixture();
    let disc1 = fx.root.join("incoming").join("Disc 1.bin");
    let disc2 = fx.root.join("incoming").join("Disc 2.bin");
    write_rom(&disc1);
    write_rom(&disc2);
    let disc1_dest = fx.root.join("lib").join("Disc 1.bin");
    let disc2_dest = fx.root.join("lib").join("Disc 2.bin");
    std::fs::create_dir_all(disc2_dest.parent().unwrap()).unwrap();
    std::fs::write(&disc2_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(
                disc1.to_str().unwrap(),
                disc1_dest.to_str().unwrap(),
                "game",
            ),
            set_item(
                disc2.to_str().unwrap(),
                disc2_dest.to_str().unwrap(),
                "game",
            ),
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
    let disc1_entry = outcome
        .transaction
        .entries
        .iter()
        .find(|entry| entry.source_path == disc1)
        .unwrap();
    assert!(
        disc1_entry
            .preflight_failures
            .iter()
            .any(|failure| failure.to_lowercase().contains("approv"))
    );
}

#[test]
fn entries_with_no_set_label_are_never_grouped_together() {
    let fx = fixture();
    let a = fx.root.join("incoming").join("a.bin");
    let b = fx.root.join("incoming").join("b.bin");
    write_rom(&a);
    write_rom(&b);
    let a_dest = fx.root.join("lib").join("a.bin");
    let b_dest = fx.root.join("lib").join("b.bin");
    std::fs::create_dir_all(a_dest.parent().unwrap()).unwrap();
    std::fs::write(&a_dest, b"pre-existing").unwrap();
    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, transaction) = build_and_approve(&export);
    assert!(set_label_of(&transaction.entries[0]).is_none());
    assert!(set_label_of(&transaction.entries[1]).is_none());
}

// ==================================================================
// B. Multi-set atomicity (>= 10) - milestone section 9, 24
// ==================================================================

struct MultiSetFixture {
    fx: Fixture,
    export: LibraryPlanExport,
    a1: PathBuf,
    a1_dest: PathBuf,
    a2: PathBuf,
    a2_dest: PathBuf,
    b_rom: PathBuf,
    b_rom_dest: PathBuf,
    b_manual: PathBuf,
    b_manual_dest: PathBuf,
    c_rom: PathBuf,
    c_rom_dest: PathBuf,
}

fn multi_set_fixture() -> MultiSetFixture {
    let fx = fixture();
    let a1 = fx.root.join("incoming").join("A Disc 1.bin");
    let a2 = fx.root.join("incoming").join("A Disc 2.bin");
    let b_rom = fx.root.join("incoming").join("B.bin");
    let b_manual = fx.root.join("incoming").join("B.pdf");
    let c_rom = fx.root.join("incoming").join("C.bin");
    for p in [&a1, &a2, &b_rom, &b_manual, &c_rom] {
        write_rom(p);
    }
    let a1_dest = fx.root.join("lib").join("A Disc 1.bin");
    let a2_dest = fx.root.join("lib").join("A Disc 2.bin");
    let b_rom_dest = fx.root.join("lib").join("B.bin");
    let b_manual_dest = fx.root.join("lib").join("B.pdf");
    let c_rom_dest = fx.root.join("lib").join("C.bin");
    // Sabotage only Set B's rom destination.
    std::fs::create_dir_all(b_rom_dest.parent().unwrap()).unwrap();
    std::fs::write(&b_rom_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            set_item(a1.to_str().unwrap(), a1_dest.to_str().unwrap(), "SetA"),
            set_item(a2.to_str().unwrap(), a2_dest.to_str().unwrap(), "SetA"),
            set_item(
                b_rom.to_str().unwrap(),
                b_rom_dest.to_str().unwrap(),
                "SetB",
            ),
            support_item(
                b_manual.to_str().unwrap(),
                b_manual_dest.to_str().unwrap(),
                "SetB",
            ),
            set_item(
                c_rom.to_str().unwrap(),
                c_rom_dest.to_str().unwrap(),
                "SetC",
            ),
        ],
    };
    MultiSetFixture {
        fx,
        export,
        a1,
        a1_dest,
        a2,
        a2_dest,
        b_rom,
        b_rom_dest,
        b_manual,
        b_manual_dest,
        c_rom,
        c_rom_dest,
    }
}

#[test]
fn multi_set_abort_all_rejects_everything_when_set_b_is_unsafe() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(result, Err(ApplyError::HardConflicts(_))));
    assert!(mf.a1.exists() && !mf.a1_dest.exists());
    assert!(mf.c_rom.exists() && !mf.c_rom_dest.exists());
}

#[test]
fn multi_set_skip_unsafe_subset_skips_only_set_b() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    let outcome = apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert_eq!(
        entry_state(&outcome.transaction, &mf.a1),
        EntryState::Applied
    );
    assert_eq!(
        entry_state(&outcome.transaction, &mf.a2),
        EntryState::Applied
    );
    assert_eq!(
        entry_state(&outcome.transaction, &mf.c_rom),
        EntryState::Applied
    );
    assert_eq!(
        entry_state(&outcome.transaction, &mf.b_rom),
        EntryState::Skipped
    );
    assert_eq!(
        entry_state(&outcome.transaction, &mf.b_manual),
        EntryState::Skipped
    );
}

#[test]
fn multi_set_c_single_rom_applies_independently_of_set_b_failure() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert!(mf.c_rom_dest.exists());
    assert!(!mf.c_rom.exists());
}

#[test]
fn multi_set_set_a_moves_together_both_discs() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert!(mf.a1_dest.exists() && mf.a2_dest.exists());
}

#[test]
fn multi_set_disk_state_after_partial_skip_matches_expectation() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert!(mf.b_manual.exists(), "set B's support file must stay put");
    assert!(!mf.b_manual_dest.exists());
    // Set B's pre-existing (sabotage) destination file is exactly what
    // caused the whole set to be skipped in the first place - it must be
    // completely untouched, never overwritten by the aborted attempt.
    assert_eq!(
        std::fs::read(&mf.b_rom_dest).unwrap(),
        b"pre-existing".to_vec()
    );
}

#[test]
fn multi_set_journal_records_set_b_skipped_a_and_c_applied() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    let path = journal_path(&mf.fx.journal_dir, &transaction.transaction_id).unwrap();
    let persisted = read_journal(&path).unwrap();
    assert_eq!(entry_state(&persisted, &mf.a1), EntryState::Applied);
    assert_eq!(entry_state(&persisted, &mf.b_rom), EntryState::Skipped);
    assert_eq!(entry_state(&persisted, &mf.c_rom), EntryState::Applied);
}

#[test]
fn multi_set_abort_all_leaves_zero_mutation_on_disk() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    let _ = apply_plan_transaction(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
    );
    for p in [&mf.a1_dest, &mf.a2_dest, &mf.c_rom_dest, &mf.b_manual_dest] {
        assert!(!p.exists());
    }
}

#[test]
fn multi_set_rollback_after_partial_skip_only_reverses_the_applied_sets() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &mf.fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([mf.fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    assert!(mf.a1.exists() && mf.c_rom.exists());
    // Set B's own untouched files remain exactly where the skip left them.
    assert!(mf.b_rom.exists() && mf.b_manual.exists());
}

#[test]
fn multi_set_three_independent_sets_all_safe_all_apply() {
    let fx = fixture();
    let a = fx.root.join("incoming").join("A.bin");
    let b = fx.root.join("incoming").join("B.bin");
    let c = fx.root.join("incoming").join("C.bin");
    for p in [&a, &b, &c] {
        write_rom(p);
    }
    let a_dest = fx.root.join("lib").join("A.bin");
    let b_dest = fx.root.join("lib").join("B.bin");
    let c_dest = fx.root.join("lib").join("C.bin");
    let export = LibraryPlanExport {
        items: vec![
            set_item(a.to_str().unwrap(), a_dest.to_str().unwrap(), "SetA"),
            set_item(b.to_str().unwrap(), b_dest.to_str().unwrap(), "SetB"),
            set_item(c.to_str().unwrap(), c_dest.to_str().unwrap(), "SetC"),
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
    assert!(a_dest.exists() && b_dest.exists() && c_dest.exists());
    assert_eq!(outcome.transaction.applied_count(), 3);
}

#[test]
fn multi_set_support_file_in_set_b_is_skipped_alongside_its_primary() {
    let mf = multi_set_fixture();
    let (_, mut transaction) = build_and_approve(&mf.export);
    let generation = plan_generation_of(&mf.export);
    let cancel = AtomicBool::new(false);
    let outcome = apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &mf.fx.root,
        TrustedRoots::from_paths([mf.fx.root.as_path()]),
        &mf.fx.journal_dir,
        &cancel,
        false,
        HardConflictMode::SkipUnsafeSubset,
    )
    .unwrap();
    assert_eq!(
        entry_state(&outcome.transaction, &mf.b_manual),
        EntryState::Skipped
    );
}

// ==================================================================
// C. Terminal-state / AlreadySettled regression matrix (>= 10) -
// milestone sections 18-22
// ==================================================================

fn apply_a_single_move(fx: &Fixture) -> (LibraryPlanExport, RenameTransaction, PathBuf, PathBuf) {
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    (export, transaction, source, destination)
}

fn ready_export_local(source: &str, destination: &str) -> LibraryPlanExport {
    LibraryPlanExport {
        items: vec![item(source, destination)],
    }
}

#[test]
fn applying_an_already_committed_transaction_again_mutates_nothing_new() {
    let fx = fixture();
    let (export, mut transaction, source, destination) = apply_a_single_move(&fx);
    assert_eq!(transaction.state, TransactionState::Applied);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let before = std::fs::read(&destination).unwrap();
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    // The source is gone (already moved), so the shared executor's own
    // preflight refuses this cleanly - no `AlreadySettled` guard is needed
    // for `Applied`, and none exists; this pins that this is still safe.
    assert!(result.is_err());
    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), before);
}

#[test]
fn applying_a_rolled_back_transaction_again_is_refused_already_settled() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(transaction.state, TransactionState::RolledBack);

    let generation = transaction.plan_generation;
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(
        result,
        Err(ApplyError::AlreadySettled {
            state: TransactionState::RolledBack,
            ..
        })
    ));
    assert!(source.exists() && !destination.exists());
}

#[test]
fn applying_a_rolling_back_state_is_refused_already_settled() {
    let fx = fixture();
    let (_, mut transaction, ..) = apply_a_single_move(&fx);
    transaction.state = TransactionState::RollingBack;
    let cancel = AtomicBool::new(false);
    let generation = transaction.plan_generation;
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(
        result,
        Err(ApplyError::AlreadySettled {
            state: TransactionState::RollingBack,
            ..
        })
    ));
}

#[test]
fn applying_a_rollback_failed_state_is_refused_already_settled() {
    let fx = fixture();
    let (_, mut transaction, ..) = apply_a_single_move(&fx);
    transaction.state = TransactionState::RollbackFailed;
    let cancel = AtomicBool::new(false);
    let generation = transaction.plan_generation;
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(
        result,
        Err(ApplyError::AlreadySettled {
            state: TransactionState::RollbackFailed,
            ..
        })
    ));
}

#[test]
fn applying_an_apply_failed_transaction_again_is_a_deterministic_retry_not_a_resurrection() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("game.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"pre-existing").unwrap();
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let _ = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    // A retry with the sabotage still in place fails the same way again -
    // deterministic, never a silent success on the second try.
    let retry = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(retry, Err(ApplyError::HardConflicts(_))));
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"pre-existing".to_vec()
    );
}

#[test]
fn double_apply_on_a_freshly_rolled_back_transaction_leaves_the_filesystem_byte_identical() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    let before = std::fs::read(&source).unwrap();
    let generation = transaction.plan_generation;
    for _ in 0..2 {
        let _ = apply_plan_transaction(
            &mut transaction,
            generation,
            &fx.root,
            TrustedRoots::from_paths([fx.root.as_path()]),
            &fx.journal_dir,
            &cancel,
            false,
        );
    }
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert!(!destination.exists());
}

#[test]
fn already_settled_error_names_the_correct_transaction_id_and_state() {
    let fx = fixture();
    let (_, mut transaction, ..) = apply_a_single_move(&fx);
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    let id = transaction.transaction_id.clone();
    let generation = transaction.plan_generation;
    let result = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    match result {
        Err(ApplyError::AlreadySettled {
            transaction_id,
            state,
        }) => {
            assert_eq!(transaction_id, id);
            assert_eq!(state, TransactionState::RolledBack);
        }
        other => panic!("expected AlreadySettled, got {other:?}"),
    }
}

#[test]
fn abort_all_hard_conflict_demotes_a_stuck_applying_state_to_apply_failed() {
    // Batch 16 finding: `apply_transaction`'s AbortAll path can return
    // `Err(HardConflicts)` before ever writing anything past the
    // pre-mutation `Applying` checkpoint. `apply_plan_transaction_with_mode`
    // now demotes the journal to `ApplyFailed` on any error while still
    // `Applying`, so a hard-conflict refusal never leaves the journal
    // durably claiming an in-flight batch that never started.
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("game.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"pre-existing").unwrap();
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    assert!(result.is_err());
    assert_eq!(transaction.state, TransactionState::ApplyFailed);
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let persisted = read_journal(&path).unwrap();
    assert_eq!(persisted.state, TransactionState::ApplyFailed);
}

#[test]
fn no_apply_error_variant_can_silently_resurrect_a_rolled_back_transaction() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    let generation = transaction.plan_generation;
    for mode in [
        HardConflictMode::AbortAll,
        HardConflictMode::SkipUnsafeSubset,
    ] {
        let result = apply_plan_transaction_with_mode(
            &mut transaction,
            generation,
            &fx.root,
            TrustedRoots::from_paths([fx.root.as_path()]),
            &fx.journal_dir,
            &cancel,
            false,
            mode,
        );
        assert!(matches!(result, Err(ApplyError::AlreadySettled { .. })));
    }
    assert!(source.exists() && !destination.exists());
}

// ==================================================================
// D. Fresh reapproval / new transaction id (>= 8) - milestone sections
// 20-22
// ==================================================================

#[test]
fn building_a_transaction_twice_from_the_same_export_yields_different_ids() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("a.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("a.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let t1 = build_plan_transaction(&export, &approved, "root").unwrap();
    let t2 = build_plan_transaction(&export, &approved, "root").unwrap();
    assert_ne!(t1.transaction_id, t2.transaction_id);
    assert_eq!(t1.plan_generation, t2.plan_generation);
}

#[test]
fn rollback_then_a_fresh_build_produces_a_new_transaction_id() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let old_id = transaction.transaction_id.clone();
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(source.exists() && !destination.exists());

    // Genuinely wanting to apply the same plan again requires a fresh
    // export, fresh preview, fresh approval, and a new transaction -
    // never reusing the settled one.
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, fresh_transaction) = build_and_approve(&export);
    assert_ne!(fresh_transaction.transaction_id, old_id);
}

#[test]
fn a_genuinely_fresh_reapply_after_rollback_succeeds_via_a_new_transaction() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();

    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, mut fresh_transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let outcome = apply_plan_transaction(
        &mut fresh_transaction,
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
}

#[test]
fn approval_digest_is_identical_across_two_independent_builds() {
    let export = ready_export_local("/roms/a.bin", "/lib/a.bin");
    let preview = build_preview(&export);
    let approved1 = approve_transaction(&preview, "ack one").unwrap();
    let approved2 = approve_transaction(&preview, "ack two").unwrap();
    assert_eq!(approved1.digest, approved2.digest);
}

#[test]
fn transaction_id_never_influences_the_plan_digest() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("a.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("a.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let d1 = compute_plan_digest(&export);
    let d2 = compute_plan_digest(&export);
    assert_eq!(d1, d2);
    // The digest is computed purely from export content - it has no
    // transaction id anywhere in its input, so two builds from this export
    // (different ids) still produce this identical digest.
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let t1 = build_plan_transaction(&export, &approved, "root").unwrap();
    let t2 = build_plan_transaction(&export, &approved, "root").unwrap();
    assert_ne!(t1.transaction_id, t2.transaction_id);
    assert_eq!(t1.plan_generation, t2.plan_generation);
}

#[test]
fn two_approvals_of_one_preview_share_a_digest_but_have_independent_timestamps() {
    let export = ready_export_local("/roms/a.bin", "/lib/a.bin");
    let preview = build_preview(&export);
    let a1 = approve_transaction(&preview, "first").unwrap();
    let a2 = approve_transaction(&preview, "second").unwrap();
    assert_eq!(a1.digest, a2.digest);
    assert_ne!(a1.acknowledgement, a2.acknowledgement);
}

#[test]
fn rebuilding_after_an_external_export_change_produces_a_new_digest_and_a_new_id() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("a.bin");
    write_rom(&source);
    let destination1 = fx.root.join("lib").join("a.bin");
    let destination2 = fx.root.join("lib").join("renamed-a.bin");
    let export1 = ready_export_local(source.to_str().unwrap(), destination1.to_str().unwrap());
    let preview1 = build_preview(&export1);
    let approved1 = approve_transaction(&preview1, "ack").unwrap();
    let t1 = build_plan_transaction(&export1, &approved1, "root").unwrap();

    let export2 = ready_export_local(source.to_str().unwrap(), destination2.to_str().unwrap());
    let preview2 = build_preview(&export2);
    let approved2 = approve_transaction(&preview2, "ack").unwrap();
    let t2 = build_plan_transaction(&export2, &approved2, "root").unwrap();

    assert_ne!(approved1.digest, approved2.digest);
    assert_ne!(t1.plan_generation, t2.plan_generation);
    assert_ne!(t1.transaction_id, t2.transaction_id);
}

#[test]
fn stale_approval_is_refused_even_though_the_old_transaction_id_would_have_been_new() {
    let export = ready_export_local("/roms/a.bin", "/lib/a.bin");
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let mut changed = export.clone();
    changed.items[0].proposed_destination = Some("/lib/different.bin".to_string());
    let result = build_plan_transaction(&changed, &approved, "root");
    assert!(matches!(
        result,
        Err(PlanTransactionError::DigestMismatch { .. })
    ));
}

// ==================================================================
// E. Canary eligibility (>= 15) - milestone sections 10-15, 23-24
// ==================================================================

struct CanaryFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

fn canary_fixture() -> CanaryFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let source = root.join("incoming").join("Game.bin");
    write_rom(&source);
    let destination = root.join("lib").join("Game.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    CanaryFixture {
        _dir: dir,
        root,
        source,
        destination,
    }
}

fn canary_export_and_approval(
    cf: &CanaryFixture,
) -> (LibraryPlanExport, ApprovedPlan, LibraryPlanExportItem) {
    let export_item = hashed_item(
        cf.source.to_str().unwrap(),
        cf.destination.to_str().unwrap(),
    );
    let export = LibraryPlanExport {
        items: vec![export_item.clone()],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "canary ack").unwrap();
    (export, approved, export_item)
}

#[test]
fn a_small_single_file_regular_source_is_canary_eligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(result.is_ok(), "{result:?}");
    assert!(result.unwrap().strong_enough_for_canary);
}

#[test]
fn stale_digest_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (mut export, approved, item) = canary_export_and_approval(&cf);
    export.items[0].proposed_destination = Some("/somewhere/else.bin".to_string());
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::DigestStale)
    );
}

#[test]
fn not_ready_status_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (mut export, approved, mut item) = canary_export_and_approval(&cf);
    item.status = PlanStatus::NeedsReview;
    export.items[0].status = PlanStatus::NeedsReview;
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::NotReady)
    );
}

#[test]
fn blockers_make_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, mut item) = canary_export_and_approval(&cf);
    item.blockers = vec!["needs review".to_string()];
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::HasBlockers)
    );
}

#[test]
fn an_unapproved_item_is_canary_ineligible() {
    let cf = canary_fixture();
    let (export, mut approved, item) = canary_export_and_approval(&cf);
    approved.approved_item_ids = BTreeSet::new();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::NotApproved)
    );
}

#[test]
fn no_destination_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, mut item) = canary_export_and_approval(&cf);
    item.proposed_destination = None;
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::NoDestination)
    );
}

#[test]
fn set_membership_makes_a_candidate_ineligible_no_multi_disc_for_canary_1() {
    let cf = canary_fixture();
    let (export, approved, mut item) = canary_export_and_approval(&cf);
    item.set_label = Some("SetA".to_string());
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::BelongsToSet)
    );
}

#[test]
fn support_association_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, mut item) = canary_export_and_approval(&cf);
    item.support_role = Some("manual".to_string());
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::HasSupportAssociation)
    );
}

#[test]
fn a_missing_source_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    std::fs::remove_file(&cf.source).unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::SourceMissing)
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_source_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    std::fs::remove_file(&cf.source).unwrap();
    let target = cf.root.join("real-target.bin");
    write_rom(&target);
    std::os::unix::fs::symlink(&target, &cf.source).unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::SourceIsSymlink)
    );
}

#[test]
fn an_existing_destination_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    std::fs::write(&cf.destination, b"already here").unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::DestinationAlreadyExists)
    );
}

#[test]
fn a_missing_destination_parent_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    std::fs::remove_dir_all(cf.destination.parent().unwrap()).unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::DestinationParentMissing)
    );
}

#[test]
fn a_source_over_the_size_ceiling_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    // Sparse file: sets the logical length without writing real bytes.
    let file = std::fs::File::create(&cf.source).unwrap();
    file.set_len(CANARY_MAX_SIZE_BYTES + 1).unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    let reasons = result.unwrap_err();
    assert!(
        reasons
            .iter()
            .any(|r| matches!(r, CanaryIneligibleReason::SourceTooLarge { .. }))
    );
}

#[test]
fn no_frozen_hash_makes_a_candidate_ineligible() {
    let cf = canary_fixture();
    let export_item = item(
        cf.source.to_str().unwrap(),
        cf.destination.to_str().unwrap(),
    );
    let export = LibraryPlanExport {
        items: vec![export_item.clone()],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export_item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::NoHashPrecondition)
    );
}

#[test]
fn a_source_under_the_production_roms_root_is_unconditionally_ineligible() {
    let cf = canary_fixture();
    let (mut export, approved, mut item) = canary_export_and_approval(&cf);
    item.precondition.source_path = "/mnt/games/roms/psx/Fake Game.chd".to_string();
    export.items[0].precondition.source_path = item.precondition.source_path.clone();
    let approved_matching = ApprovedPlan {
        digest: compute_plan_digest(&export),
        approved_at_unix: approved.approved_at_unix,
        approved_item_ids: [item.precondition.source_path.clone()]
            .into_iter()
            .collect(),
        acknowledgement: approved.acknowledgement.clone(),
    };
    let result = assess_canary_eligibility(&export, &item, &approved_matching, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::SourceUnderProductionRoot)
    );
}

#[test]
fn a_destination_under_the_production_roms_root_is_unconditionally_ineligible() {
    let cf = canary_fixture();
    let (mut export, _approved, mut item) = canary_export_and_approval(&cf);
    item.proposed_destination = Some("/mnt/games/roms/psx/Fake Destination.chd".to_string());
    export.items[0].proposed_destination = item.proposed_destination.clone();
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::DestinationUnderProductionRoot)
    );
}

#[test]
fn a_source_outside_the_supplied_canary_root_is_ineligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let other_root = tempfile::tempdir().unwrap();
    let result = assess_canary_eligibility(&export, &item, &approved, other_root.path());
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::SourceOutsideCanaryRoot)
    );
}

#[test]
fn a_duplicate_destination_target_makes_both_candidates_ineligible() {
    let cf = canary_fixture();
    let other_source = cf.root.join("incoming").join("Other.bin");
    write_rom(&other_source);
    let mut other = hashed_item(
        other_source.to_str().unwrap(),
        cf.destination.to_str().unwrap(),
    );
    other.precondition.physical_hash = Some("cafef00d".to_string());
    let mut primary = hashed_item(
        cf.source.to_str().unwrap(),
        cf.destination.to_str().unwrap(),
    );
    primary.precondition.physical_hash = Some("deadbeef".to_string());
    let export = LibraryPlanExport {
        items: vec![primary.clone(), other],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &primary, &approved, &cf.root);
    assert!(
        result
            .unwrap_err()
            .contains(&CanaryIneligibleReason::CycleOrDuplicateTarget)
    );
}

#[test]
fn real_apply_policy_canary_can_only_ever_produce_abort_all() {
    assert_eq!(
        RealApplyPolicy::Canary.hard_conflict_mode(),
        HardConflictMode::AbortAll
    );
}

#[test]
fn canary_size_ceiling_is_exactly_64_mib() {
    assert_eq!(CANARY_MAX_SIZE_BYTES, 64 * 1024 * 1024);
}

#[test]
fn a_same_filesystem_small_file_reports_same_filesystem_true() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let report = assess_canary_eligibility(&export, &item, &approved, &cf.root).unwrap();
    assert!(report.same_filesystem);
    assert!(report.is_regular_file);
    assert!(!report.is_symlink);
    assert!(report.destination_clear);
}

// ==================================================================
// F. Journal / rollback edge cases beyond Batch 15 (>= 10) - milestone
// sections 16-17
// ==================================================================

#[test]
fn rollback_refuses_when_the_original_sources_parent_directory_was_removed() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf();
    std::fs::remove_dir(&source_parent).unwrap();
    let cancel = AtomicBool::new(false);
    let result = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    // Fail closed either way: an explicit Err, or a settled outcome that is
    // never `FullyRolledBack` while the parent is still missing - but the
    // destination's data must never be lost either way.
    if let Ok(outcome) = result {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(destination.exists(), "moved data must never be dropped");
}

#[test]
fn rollback_refuses_when_the_original_sources_parent_was_replaced_by_a_file() {
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf();
    std::fs::remove_dir(&source_parent).unwrap();
    std::fs::write(&source_parent, b"now a file, not a directory").unwrap();
    let cancel = AtomicBool::new(false);
    let result = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    if let Ok(outcome) = result {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(destination.exists());
    assert_eq!(
        std::fs::read(&source_parent).unwrap(),
        b"now a file, not a directory".to_vec()
    );
}

#[cfg(unix)]
#[test]
fn rollback_through_a_substituted_source_parent_symlink_is_refused_even_when_the_target_is_inside_the_trusted_root()
 {
    // Batch 16 flagged this as a documented gap (item 57): `rollback_mutation`
    // refused a symlinked *leaf*, but an intermediate ancestor directory
    // replaced with a symlink was transparent to ordinary path resolution,
    // so rollback would silently write through the substituted directory
    // instead of the original one. This closeout patch adds an ancestor
    // containment re-check immediately before the reverse-rename mutation
    // (`rollback_transaction_confined`) - closing this even when the
    // symlink's target is itself still inside the trusted root, because the
    // caller's expected canonical location was replaced regardless of where
    // the symlink points.
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf();
    std::fs::remove_dir(&source_parent).unwrap();
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &source_parent).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );

    // Refused, not a settled full rollback: no mutation happens through the
    // substituted directory, the moved file's data is never lost (still at
    // `destination`), and nothing is written to `elsewhere` either.
    if let Ok(outcome) = outcome {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(destination.exists(), "moved data must never be dropped");
    assert!(!elsewhere.join(source.file_name().unwrap()).exists());
}

// ==================================================================
// G. Ancestor-symlink rollback hardening (closeout patch after Batch 16
// item 57) - the `rollback_transaction_confined` / `ancestor_chain_is_confined`
// gap closure.
// ==================================================================

/// Two-level-deep fixture so both a "parent" and a "grandparent" ancestor
/// exist on each side, and so the destination side exercises two
/// transaction-*created* directories (both nested levels are new).
fn apply_a_deeply_nested_move(fx: &Fixture) -> (RenameTransaction, PathBuf, PathBuf) {
    let source = fx.root.join("in").join("deep").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("nested").join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    (transaction, source, destination)
}

#[cfg(unix)]
#[test]
fn rollback_through_a_substituted_source_parent_symlink_targeting_outside_the_trusted_root_is_refused()
 {
    let fx = fixture();
    let (mut transaction, source, destination) = apply_a_deeply_nested_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf();
    std::fs::remove_dir(&source_parent).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &source_parent).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    if let Ok(outcome) = outcome {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(destination.exists(), "moved data must never be dropped");
    assert!(!outside.path().join("game.bin").exists());
}

#[cfg(unix)]
#[test]
fn rollback_through_a_substituted_source_grandparent_symlink_is_refused() {
    let fx = fixture();
    let (mut transaction, source, destination) = apply_a_deeply_nested_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf(); // .../in/deep
    let source_grandparent = source_parent.parent().unwrap().to_path_buf(); // .../in
    std::fs::remove_dir(&source_parent).unwrap();
    std::fs::remove_dir(&source_grandparent).unwrap();
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &source_grandparent).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    if let Ok(outcome) = outcome {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(destination.exists(), "moved data must never be dropped");
    // Nothing was ever written anywhere under the substituted directory.
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn rollback_through_a_substituted_destination_parent_symlink_is_refused_the_transaction_created_directory_is_never_written_through()
 {
    // The destination's parent (`lib/nested`) was created *by this very
    // transaction* (`apply_a_deeply_nested_move`'s destination is two levels
    // deep and neither level existed before). Substituting a
    // transaction-owned directory is exactly as dangerous as substituting a
    // pre-existing one, so it must be refused identically.
    let fx = fixture();
    let (mut transaction, source, destination) = apply_a_deeply_nested_move(&fx);
    assert_eq!(transaction.created_directories.len(), 2);
    let destination_parent = destination.parent().unwrap().to_path_buf(); // .../lib/nested
    // Rename (not remove) the whole directory, then plant a symlink in its
    // place - this preserves the moved file's reachability through the
    // symlink (so the leaf identity check alone would still pass) and
    // isolates what's actually being tested: the ancestor-confinement
    // check, not a leaf-identity mismatch.
    let elsewhere = fx.root.join("elsewhere");
    std::fs::rename(&destination_parent, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &destination_parent).unwrap();
    assert!(
        destination.exists(),
        "the file is still reachable through the symlink"
    );

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    if let Ok(outcome) = outcome {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    // No mutation happened through the substituted directory: the original
    // source is still absent, and the file is still exactly where it was
    // (under `elsewhere`, i.e. still reachable at `destination` through the
    // symlink) - never moved, never lost.
    assert!(!source.exists());
    assert!(elsewhere.join("game.bin").exists());
}

#[cfg(unix)]
#[test]
fn rollback_through_a_substituted_destination_grandparent_symlink_targeting_outside_the_trusted_root_is_refused()
 {
    let fx = fixture();
    let (mut transaction, source, _destination) = apply_a_deeply_nested_move(&fx);
    let destination_grandparent = fx.root.join("lib"); // .../lib, the outer created dir
    // Empty it first: rollback has not yet removed `lib/nested`, so `lib`
    // still contains it.
    std::fs::remove_dir_all(&destination_grandparent).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &destination_grandparent).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );
    if let Ok(outcome) = outcome {
        assert_ne!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    }
    assert!(!source.exists());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn rollback_refusal_via_ancestor_symlink_is_reported_as_a_specific_reason_not_a_vague_missing_file_message()
 {
    let fx = fixture();
    let (_, mut transaction, source, _destination) = apply_a_single_move(&fx);
    let source_parent = source.parent().unwrap().to_path_buf();
    std::fs::remove_dir(&source_parent).unwrap();
    let elsewhere = fx.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &source_parent).unwrap();

    let cancel = AtomicBool::new(false);
    rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(transaction.state, TransactionState::RollbackFailed);

    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    let assessment = assess_recovery(&transaction, &issues);
    let report = render_recovery_report(&transaction, &issues, assessment);

    assert!(
        report.contains("ancestor directory has been replaced")
            && report.contains("manual recovery required"),
        "report must name the actual cause, not just \"missing\": {report}"
    );
    assert!(
        !report.to_lowercase().contains("missing file"),
        "the real cause (a substituted ancestor) is detectable, so a generic \
         \"missing file\" message would be misleading: {report}"
    );
}

#[cfg(unix)]
#[test]
fn ordinary_rollback_of_a_deeply_nested_move_is_unaffected_by_the_ancestor_confinement_check() {
    // No symlink substitution anywhere - proves the new check does not
    // regress the normal, legitimate case: nested transaction-created
    // directories, deepest-first cleanup, full byte-for-byte restore.
    let fx = fixture();
    let (mut transaction, source, destination) = apply_a_deeply_nested_move(&fx);
    let created = transaction.created_directories.clone();
    assert_eq!(created.len(), 2);
    let expected_bytes = std::fs::read(&destination).unwrap();

    let cancel = AtomicBool::new(false);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();

    assert_eq!(outcome.rollback.result, RollbackResult::FullyRolledBack);
    assert!(source.exists());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), expected_bytes);
    // Both transaction-created directories, now empty, are removed
    // deepest-first.
    assert_eq!(outcome.directories_removed.len(), 2);
    for directory in &created {
        assert!(!directory.exists());
    }
}

#[test]
fn a_nested_transaction_created_directory_survives_rollback_when_non_empty() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx
        .root
        .join("lib")
        .join("nested")
        .join("dir")
        .join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    // An external actor drops an unrelated file into the newly created
    // innermost directory before rollback runs.
    let external = destination.parent().unwrap().join("external.txt");
    std::fs::write(&external, b"not ours").unwrap();

    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(
        outcome
            .directories_remaining
            .contains(&destination.parent().unwrap().to_path_buf())
    );
    assert!(external.exists(), "external file must never be removed");
}

#[test]
fn created_directories_are_removed_deepest_first_on_rollback() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx
        .root
        .join("lib")
        .join("a")
        .join("b")
        .join("c")
        .join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    // `lib`, `a`, `b`, `c` are all newly created beneath `fx.root` - four
    // levels, not three: `lib` itself does not exist yet either.
    assert_eq!(transaction.created_directories.len(), 4);
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert_eq!(outcome.directories_removed.len(), 4);
    assert!(outcome.directories_remaining.is_empty());
    for dir in &transaction.created_directories {
        assert!(!dir.exists());
    }
}

#[test]
fn created_directories_are_journaled_and_round_trip_through_read_journal() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("nested").join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let persisted = read_journal(&path).unwrap();
    assert_eq!(
        persisted.created_directories,
        transaction.created_directories
    );
}

#[cfg(unix)]
#[test]
fn rollback_after_journal_directory_permission_sabotage_leaves_the_move_undone_but_intact() {
    // Root can always write regardless of the permission bit; skip there.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fx = fixture();
    let (_, mut transaction, source, destination) = apply_a_single_move(&fx);
    let mut perms = std::fs::metadata(&fx.journal_dir).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o500);
    std::fs::set_permissions(&fx.journal_dir, perms).unwrap();

    let cancel = AtomicBool::new(false);
    let result = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    );

    let mut restore = std::fs::metadata(&fx.journal_dir).unwrap().permissions();
    restore.set_mode(0o700);
    std::fs::set_permissions(&fx.journal_dir, restore).unwrap();

    // Whatever happened, the applied file must still be exactly where it
    // is - no destructive retry, no silent data loss.
    assert!(destination.exists());
    let _ = (result, source);
}

#[test]
fn abort_all_hard_conflict_still_leaves_any_already_created_directories_removable_by_rollback() {
    let fx = fixture();
    let a = fx.root.join("incoming").join("a.bin");
    let b = fx.root.join("incoming").join("b.bin");
    write_rom(&a);
    write_rom(&b);
    let a_dest = fx.root.join("lib").join("nested").join("a.bin");
    let b_dest = fx.root.join("lib").join("nested").join("b.bin");
    std::fs::create_dir_all(b_dest.parent().unwrap()).unwrap();
    std::fs::write(&b_dest, b"pre-existing").unwrap();

    let export = LibraryPlanExport {
        items: vec![
            item(a.to_str().unwrap(), a_dest.to_str().unwrap()),
            item(b.to_str().unwrap(), b_dest.to_str().unwrap()),
        ],
    };
    let (_, mut transaction) = build_and_approve(&export);
    let generation = plan_generation_of(&export);
    let cancel = AtomicBool::new(false);
    let _ = apply_plan_transaction(
        &mut transaction,
        generation,
        &fx.root,
        TrustedRoots::from_paths([fx.root.as_path()]),
        &fx.journal_dir,
        &cancel,
        false,
    );
    assert_eq!(transaction.state, TransactionState::ApplyFailed);
    // The directory was created (harmlessly empty) before the batch was
    // refused; nothing has been applied, so rollback is a safe cleanup.
    let outcome = rollback_plan_transaction(
        &mut transaction,
        &fx.journal_dir,
        &cancel,
        &TrustedRoots::from_paths([fx.root.as_path()]),
    )
    .unwrap();
    assert!(outcome.directories_remaining.is_empty() || !outcome.directories_removed.is_empty());
}

#[test]
fn a_tampered_plan_generation_in_a_reloaded_journal_is_caught_as_stale_at_apply() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
    let (_, transaction) = build_and_approve(&export);
    let real_generation = plan_generation_of(&export);
    write_journal(&fx.journal_dir, &transaction).unwrap();
    let path = journal_path(&fx.journal_dir, &transaction.transaction_id).unwrap();
    let mut reloaded = read_journal(&path).unwrap();
    reloaded.plan_generation = real_generation.wrapping_add(1);
    write_journal(&fx.journal_dir, &reloaded).unwrap();

    let mut tampered = read_journal(&path).unwrap();
    let cancel = AtomicBool::new(false);
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
    let _ = transaction;
}

#[test]
fn reconcile_recovery_on_a_directory_only_transaction_never_guesses_a_missing_entry_state() {
    let fx = fixture();
    let source = fx.root.join("incoming").join("game.bin");
    write_rom(&source);
    let destination = fx.root.join("lib").join("nested").join("game.bin");
    let export = ready_export_local(source.to_str().unwrap(), destination.to_str().unwrap());
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
    let issues = reconcile_recovery(&mut transaction, &fx.journal_dir).unwrap();
    // A cleanly Applied transaction has nothing left uncertain to report.
    assert!(issues.is_empty());
    let assessment = assess_recovery(&transaction, &issues);
    assert_eq!(assessment, RecoveryAssessment::SafeToRollback);
}

// ==================================================================
// G. Preview / guardrails (>= 8) - milestone sections 24-27, 37-39
// ==================================================================

#[test]
fn render_canary_preview_shows_pass_and_blast_radius_when_eligible() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let eligibility = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    let rendered = render_canary_preview(&item, &eligibility);
    assert!(rendered.contains("REAL APPLY CANARY PREVIEW"));
    assert!(rendered.contains("Mode:\n  AbortAll"));
    assert!(rendered.contains("Preconditions:\n  PASS"));
    assert!(rendered.contains("Blast radius:\n  1 file"));
    assert!(rendered.contains("Approval:\n  REQUIRED"));
    assert!(rendered.contains("Applied:\n  NO"));
}

#[test]
fn render_canary_preview_shows_fail_and_reasons_when_ineligible() {
    let cf = canary_fixture();
    let (export, approved, mut item) = canary_export_and_approval(&cf);
    item.set_label = Some("SetA".to_string());
    let eligibility = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    let rendered = render_canary_preview(&item, &eligibility);
    assert!(rendered.contains("Preconditions:\n  FAIL"));
    assert!(rendered.contains("BelongsToSet"));
    assert!(!rendered.contains("Blast radius"));
    assert!(rendered.contains("Applied:\n  NO"));
}

#[test]
fn canary_preview_never_claims_applied() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let eligibility = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    let rendered = render_canary_preview(&item, &eligibility);
    assert!(!rendered.contains("Applied:\n  YES"));
}

#[test]
fn real_apply_policy_type_structurally_cannot_express_skip_unsafe_subset() {
    // There is exactly one variant, and it can only ever map to AbortAll -
    // proven exhaustively, not just documented.
    let all = [RealApplyPolicy::Canary];
    for policy in all {
        assert_eq!(policy.hard_conflict_mode(), HardConflictMode::AbortAll);
    }
}

#[test]
fn transaction_probe_source_never_contains_a_real_apply_flag() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/transaction_probe.rs"
    ))
    .expect("probe source readable");
    assert!(!source.contains("--real-apply"));
}

#[test]
fn transaction_probe_source_never_hardcodes_the_production_roms_path() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/transaction_probe.rs"
    ))
    .expect("probe source readable");
    assert!(!source.contains("/mnt/games/roms"));
}

#[test]
fn assess_canary_eligibility_never_mutates_the_filesystem() {
    let cf = canary_fixture();
    let (export, approved, item) = canary_export_and_approval(&cf);
    let before = std::fs::metadata(&cf.source).unwrap().modified().unwrap();
    let _ = assess_canary_eligibility(&export, &item, &approved, &cf.root);
    let after = std::fs::metadata(&cf.source).unwrap().modified().unwrap();
    assert_eq!(before, after);
    assert!(cf.source.exists());
}

#[test]
fn preview_confined_to_root_rejects_a_canary_style_out_of_root_destination() {
    let cf = canary_fixture();
    let export_item = hashed_item(
        cf.source.to_str().unwrap(),
        "/somewhere/completely/different/game.bin",
    );
    let export = LibraryPlanExport {
        items: vec![export_item],
    };
    let preview = build_preview(&export);
    assert!(!preview_is_confined_to_root(&preview, &cf.root));
}
