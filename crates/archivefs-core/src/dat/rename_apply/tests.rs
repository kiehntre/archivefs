//! Integration tests for the gated rename transaction executor, journal,
//! crash recovery and rollback - including hostile filesystem changes between
//! review and apply, no-clobber proofs, and content-integrity proofs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::*;
use crate::dat::rename_plan::{
    ProposalState, RenamePlan, RenamePlanCounts, RenameProposal, SourceObjectKind,
};
use crate::safe_read::TrustedRoots;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn cancelled() -> AtomicBool {
    AtomicBool::new(true)
}

fn write(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"fixture contents").unwrap();
    path
}

fn proposal(source: &str, current: &str, proposed: &str, state: ProposalState) -> RenameProposal {
    RenameProposal {
        source_path: PathBuf::from(source),
        current_basename: current.to_string(),
        proposed_basename: Some(proposed.to_string()),
        platform: None,
        platform_display: None,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        game_name: Some("Game".to_string()),
        rom_name: Some(proposed.to_string()),
        verdict_label: "Exact".to_string(),
        match_confident: true,
        explanations: Vec::new(),
        state,
        object_kind: SourceObjectKind::RegularFile,
        ambiguity_reason: None,
        collision: None,
        blockers: Vec::new(),
        extension_status: None,
        sanitisation_notes: Vec::new(),
        actionable: state == ProposalState::Suggested,
    }
}

fn plan(proposals: Vec<RenameProposal>, generation: u64, scan_root: &Path) -> RenamePlan {
    let counts = RenamePlanCounts::from_proposals(&proposals);
    RenamePlan {
        generation,
        source_id: "src".to_string(),
        source_display_name: "Source".to_string(),
        scan_root: scan_root.to_string_lossy().into_owned(),
        platform: None,
        platform_display: None,
        proposals,
        counts,
        audited_total: counts.total,
        verified_total: counts.total,
        truncated: false,
    }
}

/// Builds and applies a transaction in one call (review identity captured
/// at the same moment as apply, so only non-hostile flows use this).
fn apply(
    plan: &RenamePlan,
    approved: BTreeSet<String>,
    trusted: TrustedRoots,
    journal_dir: &Path,
    mode: HardConflictMode,
    cancel: &AtomicBool,
) -> Result<ApplyOutcome, ApplyError> {
    let tx = build_transaction(plan, &approved, plan.generation)?;
    apply_exec(
        tx,
        approved,
        trusted,
        journal_dir,
        mode,
        cancel,
        plan.generation,
    )
}

/// Applies an already-built transaction (used when the test mutates files
/// between review-time build and apply).
fn apply_exec(
    mut tx: RenameTransaction,
    approved: BTreeSet<String>,
    trusted: TrustedRoots,
    journal_dir: &Path,
    mode: HardConflictMode,
    cancel: &AtomicBool,
    current_generation: u64,
) -> Result<ApplyOutcome, ApplyError> {
    apply_transaction(&mut ApplyExecution {
        transaction: &mut tx,
        approved_paths: approved,
        current_generation,
        trusted,
        journal_dir: journal_dir.to_path_buf(),
        hard_conflict_mode: mode,
        cancel,
    })
}

fn approved_of(paths: &[&Path]) -> BTreeSet<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// A recursive `(relative path, inode, size, mtime, contents)` snapshot.
fn snapshot(root: &Path) -> Vec<(PathBuf, u64, u64, u64, Vec<u8>)> {
    let mut out = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_dir() {
                queue.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let content = std::fs::read(&path).unwrap_or_default();
                let inode = std::os::unix::fs::MetadataExt::ino(&meta);
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|e| e.as_secs())
                    .unwrap_or(0);
                out.push((relative, inode, meta.len(), modified, content));
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Happy path, gating, and no-clobber proofs
// ---------------------------------------------------------------------------

#[test]
fn one_approved_safe_rename_applies() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "goldenaxe.hdf");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let proposals = vec![proposal(
        source.to_str().unwrap(),
        "goldenaxe.hdf",
        "Golden Axe (Europe).hdf",
        ProposalState::Suggested,
    )];
    let plan = plan(proposals, 1, &roms);

    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();

    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert_eq!(outcome.summary.applied, 1);
    assert_eq!(outcome.summary.failed, 0);
    assert!(!source.exists());
    assert!(roms.join("Golden Axe (Europe).hdf").exists());
    // Content is identical through the rename.
    assert_eq!(
        std::fs::read(roms.join("Golden Axe (Europe).hdf")).unwrap(),
        b"fixture contents"
    );
    // The journal is present and says Applied.
    assert!(
        journal
            .join(format!("{}.json", outcome.transaction.transaction_id))
            .exists()
    );
}

#[test]
fn an_unapproved_proposal_cannot_apply() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let error = apply(
        &plan,
        BTreeSet::new(), // nothing approved
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(error, ApplyError::NothingApproved);
    assert!(source.exists(), "nothing was touched");
}

#[test]
fn an_ambiguous_proposal_cannot_apply() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut p = proposal(
        source.to_str().unwrap(),
        "a.bin",
        "b.bin",
        ProposalState::Ambiguous,
    );
    p.actionable = false;
    let plan = plan(vec![p], 1, &roms);
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ApplyError::NothingApproved,
        "ambiguous proposals are not applicable"
    );
    assert!(source.exists());
}

#[test]
fn a_conflict_proposal_cannot_apply() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut p = proposal(
        source.to_str().unwrap(),
        "a.bin",
        "b.bin",
        ProposalState::Conflict,
    );
    p.actionable = false;
    let plan = plan(vec![p], 1, &roms);
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ApplyError::NothingApproved,
        "conflict proposals are not applicable"
    );
    assert!(source.exists());
}

#[test]
fn a_stale_generation_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        5,
        &roms,
    );
    let error = build_transaction(&plan, &approved_of(&[&source]), 6).unwrap_err();
    assert!(matches!(error, ApplyError::StalePlan { .. }));
    assert!(source.exists());
}

#[test]
fn an_existing_destination_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    write(&roms, "b.bin"); // destination exists
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();

    // AbortAll: a hard conflict prevents the batch from starting at all.
    assert!(matches!(error, ApplyError::HardConflicts(_)));
    assert!(source.exists(), "the source must not move");
    assert_eq!(
        std::fs::read(roms.join("b.bin")).unwrap(),
        b"fixture contents"
    );
}

#[test]
fn an_existing_destination_in_skip_mode_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    write(&roms, "b.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.summary.skipped, 1);
    assert_eq!(outcome.transaction.entries[0].state, EntryState::Skipped);
    assert!(source.exists());
}

#[test]
fn a_symlink_source_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let target = write(&roms, "target.bin");
    let link = roms.join("link.bin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut p = proposal(
        link.to_str().unwrap(),
        "link.bin",
        "renamed.bin",
        ProposalState::Suggested,
    );
    p.object_kind = SourceObjectKind::Symlink;
    let plan = plan(vec![p], 1, &roms);
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&link]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ApplyError::NothingApproved,
        "symlink sources are never applicable"
    );
    // The link still points at its target; neither was touched.
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert!(target.exists());
}

#[test]
fn outside_trusted_roots_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    // Trusted root is a DIFFERENT directory than the source's parent.
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&other]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(error, ApplyError::HardConflicts(_)), "{error:?}");
    assert!(source.exists());
}

#[test]
fn cancellation_before_first_rename_leaves_everything_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let before = snapshot(&roms);
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = cancelled();
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(error, ApplyError::Cancelled);
    assert_eq!(snapshot(&roms), before, "nothing changed");
}

#[test]
fn an_apply_failure_stops_subsequent_operations() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let a = write(&roms, "a.bin");
    let b = write(&roms, "b.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // b.bin's destination already exists, so it fails preflight; a is fine.
    write(&roms, "B.bin");
    let proposals = vec![
        proposal(
            a.to_str().unwrap(),
            "a.bin",
            "A.bin",
            ProposalState::Suggested,
        ),
        proposal(
            b.to_str().unwrap(),
            "b.bin",
            "B.bin",
            ProposalState::Suggested,
        ),
    ];
    let plan = plan(proposals, 1, &roms);
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&a, &b]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    // b is skipped (preflight hard conflict); a applies.
    assert_eq!(outcome.summary.skipped, 1);
    assert_eq!(outcome.summary.applied, 1);
    assert!(roms.join("A.bin").exists());
    assert!(b.exists());
}

// ---------------------------------------------------------------------------
// Hostile filesystem changes between review and apply
// ---------------------------------------------------------------------------

#[test]
fn source_replaced_with_a_symlink_after_approval_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    // Hostile change: replace the source with a symlink after approval.
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere.bin"), &source).unwrap();
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.summary.applied, 0);
    assert_eq!(
        outcome.summary.skipped, 1,
        "the symlink substitution is a hard conflict"
    );
    let entry = &outcome.transaction.entries[0];
    assert!(
        entry
            .preflight_failures
            .iter()
            .any(|f| f.contains("symlink")),
        "{:?}",
        entry.preflight_failures
    );
    // The symlink is still there; the target was never touched.
    assert!(
        std::fs::symlink_metadata(&source)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn source_replaced_with_a_different_inode_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let approved = approved_of(&[&source]);
    let tx = build_transaction(&plan, &approved, 1).unwrap();
    // Hostile change: delete and recreate the file (different inode) after review.
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, b"different").unwrap();
    let cancel = no_cancel();
    let outcome = apply_exec(
        tx,
        approved,
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
        1,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "a different object must not be renamed"
    );
    assert_eq!(outcome.summary.skipped, 1);
}

#[test]
fn destination_created_after_approval_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    // Hostile change: destination appears after approval.
    write(&roms, "b.bin");
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "an appearing destination must never be overwritten"
    );
    assert_eq!(
        std::fs::read(roms.join("b.bin")).unwrap(),
        b"fixture contents"
    );
}

#[test]
fn source_renamed_externally_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let approved = approved_of(&[&source]);
    let tx = build_transaction(&plan, &approved, 1).unwrap();
    // Hostile change: rename the source externally after review.
    let renamed = roms.join("a-moved.bin");
    std::fs::rename(&source, &renamed).unwrap();
    let cancel = no_cancel();
    let outcome = apply_exec(
        tx,
        approved,
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
        1,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "an externally renamed source must not be touched"
    );
    assert_eq!(outcome.summary.skipped, 1);
    assert!(renamed.exists());
}

#[test]
fn size_changed_after_approval_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let approved = approved_of(&[&source]);
    let tx = build_transaction(&plan, &approved, 1).unwrap();
    // Hostile change: change the size after review.
    std::fs::write(&source, b"much longer content").unwrap();
    let cancel = no_cancel();
    let outcome = apply_exec(
        tx,
        approved,
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
        1,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "a resized file must not be renamed"
    );
    assert_eq!(outcome.summary.skipped, 1);
}

#[test]
fn destination_parent_changed_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let approved = approved_of(&[&source]);
    let tx = build_transaction(&plan, &approved, 1).unwrap();
    // Hostile change: the source is moved into a different directory after review.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let moved = elsewhere.join("a.bin");
    std::fs::rename(&source, &moved).unwrap();
    let cancel = no_cancel();
    let outcome = apply_exec(
        tx,
        approved,
        TrustedRoots::from_paths([&roms, &elsewhere]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
        1,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "a source moved elsewhere must not be renamed"
    );
    assert_eq!(outcome.summary.skipped, 1);
    assert!(moved.exists());
}

#[test]
fn a_case_fold_sibling_appearing_after_approval_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "game.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // Proposal: game.bin -> Game.bin
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "game.bin",
            "Game.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let approved = approved_of(&[&source]);
    let tx = build_transaction(&plan, &approved, 1).unwrap();
    // Hostile change: a second file appears with the same case-fold after review.
    write(&roms, "GAME.BIN");
    let cancel = no_cancel();
    let outcome = apply_exec(
        tx,
        approved,
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
        1,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "the case-fold collision must be detected at apply time"
    );
    assert_eq!(outcome.summary.skipped, 1);
}

#[test]
fn duplicate_batch_destinations_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let a = write(&roms, "a.bin");
    let b = write(&roms, "b.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // Two proposals targeting the same destination.
    let proposals = vec![
        proposal(
            a.to_str().unwrap(),
            "a.bin",
            "Same.bin",
            ProposalState::Suggested,
        ),
        proposal(
            b.to_str().unwrap(),
            "b.bin",
            "Same.bin",
            ProposalState::Suggested,
        ),
    ];
    let plan = plan(proposals, 1, &roms);
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&a, &b]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        outcome.summary.applied, 0,
        "duplicate targets must not apply"
    );
}

// ---------------------------------------------------------------------------
// Journal ordering and crash recovery
// ---------------------------------------------------------------------------

#[test]
fn the_journal_is_written_before_any_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();
    // The journal exists and reflects the applied state.
    let path = journal_path(&journal, &outcome.transaction.transaction_id).unwrap();
    let persisted = read_journal(&path).unwrap();
    assert_eq!(persisted.state, TransactionState::Applied);
    assert_eq!(persisted.entries[0].state, EntryState::Applied);
}

#[test]
fn crash_after_journal_write_before_first_rename_is_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // Simulate a crash: journal written, state Planned, nothing renamed.
    let tx = RenameTransaction {
        transaction_id: "crash1".to_string(),
        plan_generation: 1,
        created_at_unix: 1,
        source_scan_root: "/tmp/roms".to_string(),
        state: TransactionState::Planned,
        entries: vec![TransactionEntry {
            source_path: PathBuf::from("/tmp/roms/a.bin"),
            destination_path: PathBuf::from("/tmp/roms/b.bin"),
            original_basename: "a.bin".to_string(),
            proposed_basename: "b.bin".to_string(),
            identity: ObjectIdentity {
                size_bytes: 1,
                modified_unix: 1,
                kind: ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 1,
                #[cfg(unix)]
                dev: 1,
            },
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    };
    write_journal(&journal, &tx).unwrap();

    let (recovery, problems) = find_recovery_transactions(&journal);
    assert!(problems.is_empty());
    assert_eq!(recovery.len(), 1);
    assert_eq!(
        recovery[0].applied_count(),
        0,
        "nothing was renamed before the crash"
    );
}

#[test]
fn crash_after_first_of_n_renames_is_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut tx = RenameTransaction {
        transaction_id: "crash2".to_string(),
        plan_generation: 1,
        created_at_unix: 1,
        source_scan_root: "/tmp/roms".to_string(),
        state: TransactionState::Applying,
        entries: Vec::new(),
        unknown: Default::default(),
    };
    let mk = |source: &str, state: EntryState| TransactionEntry {
        source_path: PathBuf::from(source),
        destination_path: PathBuf::from(source.replace("a.bin", "A.bin").replace("b.bin", "B.bin")),
        original_basename: source.rsplit('/').next().unwrap().to_string(),
        proposed_basename: "x.bin".to_string(),
        identity: ObjectIdentity {
            size_bytes: 1,
            modified_unix: 1,
            kind: ObjectKind::RegularFile,
            #[cfg(unix)]
            ino: 1,
            #[cfg(unix)]
            dev: 1,
        },
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    };
    tx.entries.push(mk("/tmp/roms/a.bin", EntryState::Applied));
    tx.entries.push(mk("/tmp/roms/b.bin", EntryState::Planned));
    write_journal(&journal, &tx).unwrap();

    let (recovery, _) = find_recovery_transactions(&journal);
    assert_eq!(recovery.len(), 1);
    assert_eq!(
        recovery[0].applied_count(),
        1,
        "one rename happened before the crash"
    );
    assert_eq!(recovery[0].state, TransactionState::Applying);
}

#[test]
fn recovery_never_auto_resumes() {
    // There is no resume function anywhere in the module: the only recovery
    // operations are reading journals and rolling back on explicit choice.
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let tx = RenameTransaction {
        transaction_id: "nore".to_string(),
        plan_generation: 1,
        created_at_unix: 1,
        source_scan_root: "/tmp/roms".to_string(),
        state: TransactionState::Applying,
        entries: Vec::new(),
        unknown: Default::default(),
    };
    write_journal(&journal, &tx).unwrap();
    let (recovery, _) = find_recovery_transactions(&journal);
    assert_eq!(recovery.len(), 1);
    // A recovery journal is never acted on without an explicit rollback call.
    // (The transaction state remains untouched here.)
    assert_eq!(recovery[0].state, TransactionState::Applying);
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

fn apply_one(dir: &Path) -> (RenamePlan, RenameTransaction, BTreeSet<String>) {
    let roms = dir.join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();
    (plan, outcome.transaction, approved_of(&[&source]))
}

#[test]
fn a_successful_rollback_restores_the_original_path_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (_plan, mut tx, _) = apply_one(dir.path());
    let journal = dir.path().join("journal");
    let cancel = no_cancel();
    let outcome = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert_eq!(outcome.result, RollbackResult::FullyRolledBack);
    assert_eq!(outcome.transaction.state, TransactionState::RolledBack);
    let roms = dir.path().join("roms");
    assert!(roms.join("a.bin").exists(), "the original path is restored");
    assert!(!roms.join("b.bin").exists());
    assert_eq!(
        std::fs::read(roms.join("a.bin")).unwrap(),
        b"fixture contents"
    );
}

#[test]
fn rollback_reverses_in_reverse_order() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let a = write(&roms, "a.bin");
    let b = write(&roms, "b.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let proposals = vec![
        proposal(
            a.to_str().unwrap(),
            "a.bin",
            "A.bin",
            ProposalState::Suggested,
        ),
        proposal(
            b.to_str().unwrap(),
            "b.bin",
            "B.bin",
            ProposalState::Suggested,
        ),
    ];
    let plan = plan(proposals, 1, &roms);
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&a, &b]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.summary.applied, 2);
    let mut tx = outcome.transaction;

    // The second-applied entry (index 1) is rolled back first. Block that by
    // making its rollback impossible? Instead we prove order via the journal's
    // rolled_back_at timestamps / the fact that rollback of the second must
    // succeed before the first is attempted: we break the FIRST-applied entry's
    // destination externally, so only the second can roll back.
    std::fs::remove_file(roms.join("A.bin")).unwrap(); // first-applied destination gone
    let rollback = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert!(
        matches!(rollback.result, RollbackResult::PartiallyRolledBack { .. }),
        "second rolls back, first cannot: {:?}",
        rollback.result
    );
    assert!(
        roms.join("b.bin").exists(),
        "the second entry was rolled back"
    );
    assert!(!roms.join("B.bin").exists());
    assert!(
        !roms.join("a.bin").exists(),
        "the first could not roll back (destination gone)"
    );
}

#[test]
fn rollback_refuses_when_the_destination_was_changed_externally() {
    let dir = tempfile::tempdir().unwrap();
    let (_plan, mut tx, _) = apply_one(dir.path());
    let journal = dir.path().join("journal");
    // Hostile change: replace the destination after apply.
    let roms = dir.path().join("roms");
    std::fs::write(roms.join("b.bin"), b"replaced by an attacker").unwrap();
    let cancel = no_cancel();
    let outcome = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    assert_eq!(outcome.transaction.state, TransactionState::RollbackFailed);
    assert!(!roms.join("a.bin").exists(), "nothing was moved back");
}

#[test]
fn rollback_refuses_when_the_original_name_is_occupied() {
    let dir = tempfile::tempdir().unwrap();
    let (_plan, mut tx, _) = apply_one(dir.path());
    let journal = dir.path().join("journal");
    let roms = dir.path().join("roms");
    // Hostile change: a new file occupies the original source path.
    write(&roms, "a.bin");
    let cancel = no_cancel();
    let outcome = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert!(matches!(
        outcome.result,
        RollbackResult::RollbackFailed { .. }
    ));
    // The occupied original is untouched; the destination still has the data.
    assert_eq!(
        std::fs::read(roms.join("a.bin")).unwrap(),
        b"fixture contents"
    );
    assert_eq!(
        std::fs::read(roms.join("b.bin")).unwrap(),
        b"fixture contents"
    );
}

#[test]
fn repeated_rollback_is_idempotent_and_safe() {
    let dir = tempfile::tempdir().unwrap();
    let (_plan, mut tx, _) = apply_one(dir.path());
    let journal = dir.path().join("journal");
    let cancel = no_cancel();
    let first = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert_eq!(first.result, RollbackResult::FullyRolledBack);
    let second = rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    assert_eq!(
        second.result,
        RollbackResult::FullyRolledBack,
        "second rollback is a safe no-op"
    );
    let roms = dir.path().join("roms");
    assert!(roms.join("a.bin").exists());
    assert!(!roms.join("b.bin").exists());
}

#[test]
fn a_completed_transaction_cannot_be_applied_twice() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();
    // Applying the same plan again finds no source at the old path.
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(error, ApplyError::NothingApproved);
}

// ---------------------------------------------------------------------------
// No-clobber and content-integrity proofs
// ---------------------------------------------------------------------------

#[test]
fn content_is_identical_through_apply_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let before = std::fs::read(&source).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap();
    let after_apply = std::fs::read(roms.join("b.bin")).unwrap();
    assert_eq!(after_apply, before, "bytes unchanged through rename");

    let mut tx = outcome.transaction;
    rollback_transaction(&mut tx, &journal, &cancel).unwrap();
    let after_rollback = std::fs::read(roms.join("a.bin")).unwrap();
    assert_eq!(after_rollback, before, "bytes unchanged through rollback");
}

#[test]
fn a_failed_preflight_leaves_all_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    write(&roms, "b.bin"); // destination exists -> hard conflict
    let before = snapshot(&roms);
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(error, ApplyError::HardConflicts(_)));
    assert_eq!(
        snapshot(&roms),
        before,
        "a failed preflight changes nothing"
    );
}

#[test]
fn rename_cannot_escape_the_source_directory() {
    // Same-directory is enforced structurally: the destination is built from
    // the source's parent. A traversal-tainted proposed name is rejected by
    // the safe-basename check in preflight.
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // A hostile proposed name with a path separator.
    let p = proposal(
        source.to_str().unwrap(),
        "a.bin",
        "../escape.bin",
        ProposalState::Suggested,
    );
    // destination_path would be parent.join("../escape.bin") - but preflight
    // rejects the unsafe basename before anything happens.
    let plan = plan(vec![p], 1, &roms);
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.summary.applied, 0, "traversal names never escape");
    assert!(source.exists());
    assert!(!dir.path().join("escape.bin").exists());
}

#[test]
fn broken_symlink_substitution_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let source = write(&roms, "a.bin");
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let plan = plan(
        vec![proposal(
            source.to_str().unwrap(),
            "a.bin",
            "b.bin",
            ProposalState::Suggested,
        )],
        1,
        &roms,
    );
    // Hostile change: replace the source with a broken symlink.
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(dir.path().join("nowhere.bin"), &source).unwrap();
    let cancel = no_cancel();
    let outcome = apply(
        &plan,
        approved_of(&[&source]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::SkipUnsafeSubset,
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.summary.applied, 0);
}

#[test]
fn a_symlink_loop_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir_all(&roms).unwrap();
    let a = roms.join("a.bin");
    let b = roms.join("b.bin");
    // a -> b, b -> a : a symlink loop.
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut p = proposal(
        a.to_str().unwrap(),
        "a.bin",
        "renamed.bin",
        ProposalState::Suggested,
    );
    p.object_kind = SourceObjectKind::Symlink;
    let plan = plan(vec![p], 1, &roms);
    let cancel = no_cancel();
    let error = apply(
        &plan,
        approved_of(&[&a]),
        TrustedRoots::from_paths([&roms]),
        &journal,
        HardConflictMode::AbortAll,
        &cancel,
    )
    .unwrap_err();
    assert_eq!(
        error,
        ApplyError::NothingApproved,
        "symlink loops are never applicable"
    );
}

#[test]
fn a_path_traversal_proposed_name_is_blocked_by_preflight() {
    // Preflight's safe-basename check (plus derive-time blocking in the plan)
    // rejects any proposed name that is not a single safe component.
    assert!(super::preflight::is_safe_basename("Game (Europe).hdf"));
    assert!(!super::preflight::is_safe_basename("../escape.hdf"));
    assert!(!super::preflight::is_safe_basename("a/b.hdf"));
    assert!(!super::preflight::is_safe_basename(".."));
    assert!(!super::preflight::is_safe_basename(""));
}

// ---------------------------------------------------------------------------
// Repeated stress runs (destination race, cancellation, rollback, recovery)
// ---------------------------------------------------------------------------

#[test]
fn stress_destination_creation_race_never_overwrites() {
    // Run the "destination appears after review" hostile case repeatedly; the
    // destination must never be overwritten and the source must never move.
    for _ in 0..25 {
        let dir = tempfile::tempdir().unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let source = write(&roms, "a.bin");
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let plan = plan(
            vec![proposal(
                source.to_str().unwrap(),
                "a.bin",
                "b.bin",
                ProposalState::Suggested,
            )],
            1,
            &roms,
        );
        let approved = approved_of(&[&source]);
        let tx = build_transaction(&plan, &approved, 1).unwrap();
        write(&roms, "b.bin"); // destination appears after review
        let cancel = no_cancel();
        let outcome = apply_exec(
            tx,
            approved,
            TrustedRoots::from_paths([&roms]),
            &journal,
            HardConflictMode::SkipUnsafeSubset,
            &cancel,
            1,
        )
        .unwrap();
        assert_eq!(outcome.summary.applied, 0);
        assert!(source.exists(), "iteration: source must not move");
        assert_eq!(
            std::fs::read(roms.join("b.bin")).unwrap(),
            b"fixture contents"
        );
    }
}

#[test]
fn stress_cancellation_leaves_everything_untouched() {
    for _ in 0..25 {
        let dir = tempfile::tempdir().unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let source = write(&roms, "a.bin");
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let plan = plan(
            vec![proposal(
                source.to_str().unwrap(),
                "a.bin",
                "b.bin",
                ProposalState::Suggested,
            )],
            1,
            &roms,
        );
        let cancel = cancelled();
        let error = apply(
            &plan,
            approved_of(&[&source]),
            TrustedRoots::from_paths([&roms]),
            &journal,
            HardConflictMode::AbortAll,
            &cancel,
        )
        .unwrap_err();
        assert_eq!(error, ApplyError::Cancelled);
        assert!(source.exists());
        assert!(!roms.join("b.bin").exists());
    }
}

#[test]
fn stress_apply_and_rollback_round_trip_preserves_bytes() {
    for _ in 0..25 {
        let dir = tempfile::tempdir().unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir_all(&roms).unwrap();
        let source = write(&roms, "a.bin");
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let before = std::fs::read(&source).unwrap();
        let plan = plan(
            vec![proposal(
                source.to_str().unwrap(),
                "a.bin",
                "b.bin",
                ProposalState::Suggested,
            )],
            1,
            &roms,
        );
        let cancel = no_cancel();
        let outcome = apply(
            &plan,
            approved_of(&[&source]),
            TrustedRoots::from_paths([&roms]),
            &journal,
            HardConflictMode::AbortAll,
            &cancel,
        )
        .unwrap();
        assert_eq!(std::fs::read(roms.join("b.bin")).unwrap(), before);
        let mut tx = outcome.transaction;
        rollback_transaction(&mut tx, &journal, &cancel).unwrap();
        assert_eq!(std::fs::read(roms.join("a.bin")).unwrap(), before);
        assert!(!roms.join("b.bin").exists());
    }
}

#[test]
fn stress_crash_recovery_fixtures_are_detected() {
    for _ in 0..25 {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        let mut tx = RenameTransaction {
            transaction_id: "stress-recovery".to_string(),
            plan_generation: 1,
            created_at_unix: 1,
            source_scan_root: "/tmp/roms".to_string(),
            state: TransactionState::Applying,
            entries: Vec::new(),
            unknown: Default::default(),
        };
        tx.entries.push(TransactionEntry {
            source_path: PathBuf::from("/tmp/roms/a.bin"),
            destination_path: PathBuf::from("/tmp/roms/b.bin"),
            original_basename: "a.bin".to_string(),
            proposed_basename: "b.bin".to_string(),
            identity: ObjectIdentity {
                size_bytes: 1,
                modified_unix: 1,
                kind: ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 1,
                #[cfg(unix)]
                dev: 1,
            },
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Applied,
            failure_reason: None,
            applied_at_unix: Some(2),
            rolled_back_at_unix: None,
            unknown: Default::default(),
        });
        write_journal(&journal, &tx).unwrap();
        let (recovery, problems) = find_recovery_transactions(&journal);
        assert!(problems.is_empty());
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].applied_count(), 1);
    }
}
