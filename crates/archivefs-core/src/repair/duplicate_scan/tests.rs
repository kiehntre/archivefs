//! Focused tests for the rename-plan -> duplicate-quarantine bridge, using
//! hand-built [`RenamePlan`]/[`RenameProposal`] fixtures over real files in a
//! `TempDir` (content proof always reads real bytes; nothing here fakes a
//! hash).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tempfile::TempDir;

use crate::dat::classification::{ContentSelectionPolicy, DatContentClassification};
use crate::dat::rename_plan::{
    ProposalState, RenamePlan, RenamePlanCounts, RenameProposal, SourceObjectKind,
};
use crate::repair::proposal::RepairAction;
use crate::safe_read::TrustedRoots;

use super::plan_duplicate_quarantine_from_rename_plan;

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

/// One rename-plan proposal member of a candidate duplicate group.
///
/// `state` and `match_confident` are the only two signals
/// [`super::super::quarantine::select_survivor`] is allowed to use, so tests
/// vary exactly those.
fn member(source_path: PathBuf, state: ProposalState, match_confident: bool) -> RenameProposal {
    RenameProposal {
        source_path,
        current_basename: "current.bin".to_string(),
        proposed_basename: Some("canon.bin".to_string()),
        platform: None,
        platform_display: None,
        source_id: "test-source".to_string(),
        source_display_name: "Test Source".to_string(),
        game_name: Some("Game".to_string()),
        rom_name: Some("canon.bin".to_string()),
        verdict_label: "Exact".to_string(),
        match_confident,
        explanations: Vec::new(),
        content_policy: ContentSelectionPolicy::AllEntries,
        content_classification: DatContentClassification::unknown(),
        original_metadata: crate::dat::classification::DatOriginalMetadata::default(),
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

fn rename_plan(scan_root: &Path, proposals: Vec<RenameProposal>) -> RenamePlan {
    RenamePlan {
        generation: 1,
        source_id: "test-source".to_string(),
        source_display_name: "Test Source".to_string(),
        scan_root: scan_root.to_string_lossy().into_owned(),
        platform: None,
        platform_display: None,
        content_policy: ContentSelectionPolicy::AllEntries,
        classifier_version: "test".to_string(),
        counts: RenamePlanCounts::from_proposals(&proposals),
        audited_total: proposals.len(),
        verified_total: proposals.len(),
        truncated: false,
        proposals,
    }
}

// A. two files, same normalized name, different bytes -> no Safe proposal.
#[test]
fn different_content_never_becomes_a_safe_proposal() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let other = write(dir.path(), "other.bin", b"xyzz"); // same length, different bytes

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper, ProposalState::AlreadyCanonical, true),
            member(other, ProposalState::Suggested, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty(), "{proposals:?}");
    assert_eq!(accounting.groups_examined, 1);
    assert_eq!(accounting.groups_content_proven, 1);
    assert_eq!(accounting.quarantine_safe, 0);
    assert_eq!(accounting.content_mismatch_refused, 1);
    assert_eq!(accounting.same_object_ignored, 0);
    assert_eq!(accounting.quarantine_needs_review, 0);
}

// B. two distinct identical files, one objectively canonical -> one Safe.
#[test]
fn one_objective_keeper_produces_one_safe_quarantine_proposal() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let duplicate = write(dir.path(), "duplicate.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper.clone(), ProposalState::AlreadyCanonical, true),
            member(duplicate.clone(), ProposalState::Suggested, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].source_path, duplicate);
    assert!(matches!(proposals[0].action, RepairAction::MovePath { .. }));
    assert_eq!(accounting.quarantine_safe, 1);
    assert_eq!(accounting.groups_content_proven, 1);
    assert_eq!(accounting.quarantine_needs_review, 0);
}

// C. two identical files, neither an objective keeper -> NeedsReview.
#[test]
fn no_objective_keeper_is_needs_review() {
    let dir = temp();
    let a = write(dir.path(), "a.bin", b"test");
    let b = write(dir.path(), "b.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(a, ProposalState::Suggested, false),
            member(b, ProposalState::Suggested, false),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty());
    assert_eq!(accounting.groups_examined, 1);
    assert_eq!(accounting.groups_content_proven, 0);
    assert_eq!(accounting.quarantine_needs_review, 1);
    assert_eq!(accounting.quarantine_safe, 0);
}

// D. two identical files, equal (tied) keeper tier -> NeedsReview.
#[test]
fn a_tied_keeper_tier_is_needs_review() {
    let dir = temp();
    let a = write(dir.path(), "a.bin", b"test");
    let b = write(dir.path(), "b.bin", b"test");

    // Both confidently verified, neither already-canonical: tier 1, tied.
    let plan = rename_plan(
        dir.path(),
        vec![
            member(a, ProposalState::Suggested, true),
            member(b, ProposalState::Suggested, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty());
    assert_eq!(accounting.quarantine_needs_review, 1);
    assert_eq!(accounting.groups_content_proven, 0);
}

// E. hard links -> SameObject, no quarantine proposal.
#[cfg(unix)]
#[test]
fn hard_links_are_never_quarantine_candidates() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let linked = dir.path().join("linked.bin");
    std::fs::hard_link(&keeper, &linked).expect("hard link");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper, ProposalState::AlreadyCanonical, true),
            member(linked, ProposalState::Suggested, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty(), "{proposals:?}");
    assert_eq!(accounting.same_object_ignored, 1);
    assert_eq!(accounting.quarantine_safe, 0);
    assert_eq!(accounting.groups_content_proven, 1);
}

// F. 3 identical files, one unique canonical keeper -> one survivor, two
// quarantine proposals (never N proposals for N files).
#[test]
fn three_identical_files_with_one_keeper_produce_exactly_two_proposals() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let dup1 = write(dir.path(), "dup1.bin", b"test");
    let dup2 = write(dir.path(), "dup2.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper.clone(), ProposalState::AlreadyCanonical, true),
            member(dup1.clone(), ProposalState::Suggested, true),
            member(dup2.clone(), ProposalState::Suggested, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert_eq!(proposals.len(), 2);
    let sources: std::collections::BTreeSet<&PathBuf> =
        proposals.iter().map(|p| &p.source_path).collect();
    assert!(sources.contains(&dup1));
    assert!(sources.contains(&dup2));
    assert!(
        !sources.contains(&keeper),
        "the survivor must never be a source"
    );
    assert_eq!(accounting.quarantine_safe, 2);
}

// G. 3 identical files, no unique keeper -> NeedsReview, no N-way plan.
#[test]
fn three_identical_files_with_no_keeper_is_needs_review() {
    let dir = temp();
    let a = write(dir.path(), "a.bin", b"test");
    let b = write(dir.path(), "b.bin", b"test");
    let c = write(dir.path(), "c.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(a, ProposalState::Suggested, false),
            member(b, ProposalState::Suggested, false),
            member(c, ProposalState::Suggested, false),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty());
    assert_eq!(accounting.quarantine_needs_review, 1);
    assert_eq!(accounting.groups_content_proven, 0);
}

// Conflict-state members (two files that would collide on the same
// canonical name) are still confidently-identified duplicate candidates.
#[test]
fn conflict_state_members_still_seed_a_duplicate_group() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let duplicate = write(dir.path(), "duplicate.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper.clone(), ProposalState::AlreadyCanonical, true),
            member(duplicate.clone(), ProposalState::Conflict, true),
        ],
    );

    let (proposals, _accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].source_path, duplicate);
}

// A group of one (or zero) real candidates is never actionable.
#[test]
fn a_lone_proposal_never_forms_a_group() {
    let dir = temp();
    let only = write(dir.path(), "only.bin", b"test");
    let plan = rename_plan(
        dir.path(),
        vec![member(only, ProposalState::Suggested, true)],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty());
    assert_eq!(accounting.groups_examined, 0);
}

// Ambiguous/Blocked/Unsupported/ExcludedByContentPolicy/UnclassifiedContent
// proposals never seed a group, even sharing (game, rom) with a real member.
#[test]
fn unconfident_states_never_seed_a_group() {
    let dir = temp();
    let keeper = write(dir.path(), "canon.bin", b"test");
    let ambiguous_path = write(dir.path(), "ambiguous.bin", b"test");

    let plan = rename_plan(
        dir.path(),
        vec![
            member(keeper, ProposalState::AlreadyCanonical, true),
            member(ambiguous_path, ProposalState::Ambiguous, true),
        ],
    );

    let (proposals, accounting, _needs_review) = plan_duplicate_quarantine_from_rename_plan(
        &plan,
        dir.path(),
        &TrustedRoots::none(),
        Some(&no_cancel()),
    );

    assert!(proposals.is_empty());
    assert_eq!(accounting.groups_examined, 0);
}
