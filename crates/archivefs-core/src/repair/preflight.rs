//! Pure dry-run / preflight for a repair plan.
//!
//! [`run_repair_preflight`] is read-only: it captures filesystem identity with
//! `symlink_metadata`, never follows a link, and mutates nothing. It is what a
//! future Repair Center GUI renders before any transaction is built, and it is
//! re-run (through the transaction executor) immediately before every mutation.
//!
//! The statuses are deliberately exhaustive and named: the GUI never has to
//! infer *why* an action is not executable.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::dat::rename_apply::identity::{capture_identity, identity_matches};
use crate::dat::rename_apply::preflight::is_safe_basename;

use super::plan::RepairPlan;
use super::proposal::{RepairProposalId, SafetyState};

/// The dry-run status of one proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairPreflightStatus {
    /// Every requirement is met right now; the action may be eligible to run.
    WouldExecute,
    /// The proposal or the plan is blocked (a conflict, or `Blocked` safety).
    Blocked,
    /// Evidence exists but the proposal is not safe to execute automatically.
    NeedsReview,
    /// The plan's generation is stale relative to the caller's current audit.
    Stale,
    /// The destination already exists, or the batch has a destination conflict.
    Collision,
    /// The source no longer exists.
    MissingSource,
    /// The source exists but is no longer the object that was proposed.
    ChangedSourceIdentity,
    /// The destination is unsafe, relative, on a different filesystem, or
    /// equal to the source.
    InvalidDestination,
    /// The action kind is not executable by this layer.
    UnsupportedAction,
    /// The proposal participates in a rename cycle; order cannot be resolved.
    OrderConflict,
}

impl RepairPreflightStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::WouldExecute => "would execute",
            Self::Blocked => "blocked",
            Self::NeedsReview => "needs review",
            Self::Stale => "stale",
            Self::Collision => "collision",
            Self::MissingSource => "missing source",
            Self::ChangedSourceIdentity => "changed source identity",
            Self::InvalidDestination => "invalid destination",
            Self::UnsupportedAction => "unsupported action",
            Self::OrderConflict => "order conflict",
        }
    }
}

/// The dry-run result for one proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPreflightResult {
    pub proposal_id: RepairProposalId,
    pub status: RepairPreflightStatus,
    pub detail: String,
}

/// The dry-run report for a whole plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPreflightReport {
    /// One result per proposal, in plan (deterministic) order.
    pub results: Vec<RepairPreflightResult>,
    /// Whether every proposal would execute.
    pub all_ready: bool,
}

impl RepairPreflightReport {
    pub fn for_proposal(&self, proposal_id: &RepairProposalId) -> Option<&RepairPreflightResult> {
        self.results
            .iter()
            .find(|result| &result.proposal_id == proposal_id)
    }
}

/// Runs the pure dry-run for every proposal in `plan`.
///
/// `current_generation` must equal the plan's generation; when it does not,
/// every proposal is reported `Stale` (fail closed: a stale plan is never
/// executed, even if the filesystem happens to be unchanged).
pub fn run_repair_preflight(plan: &RepairPlan, current_generation: u64) -> RepairPreflightReport {
    let plan_conflicts = &plan.conflicts;
    let cycle_ids: BTreeSet<RepairProposalId> = plan_conflicts
        .iter()
        .filter(|conflict| conflict.kind == super::plan::PlanConflictKind::RenameCycle)
        .flat_map(|conflict| conflict.proposal_ids.iter().cloned())
        .collect();
    let colliding_ids: BTreeSet<RepairProposalId> = plan_conflicts
        .iter()
        .filter(|conflict| {
            matches!(
                conflict.kind,
                super::plan::PlanConflictKind::DuplicateDestination
                    | super::plan::PlanConflictKind::DestinationExists
            )
        })
        .flat_map(|conflict| conflict.proposal_ids.iter().cloned())
        .collect();
    // `UnsupportedProposal` conflicts are handled directly by the proposal's
    // own safety/action classification below, not by this set.
    let blocked_ids: BTreeSet<RepairProposalId> = plan_conflicts
        .iter()
        .filter(|conflict| {
            matches!(
                conflict.kind,
                super::plan::PlanConflictKind::DuplicateProposalId
                    | super::plan::PlanConflictKind::DuplicateSource
                    | super::plan::PlanConflictKind::ParentChildInterference
            )
        })
        .flat_map(|conflict| conflict.proposal_ids.iter().cloned())
        .collect();

    let stale = current_generation != plan.generation;

    let mut results = Vec::with_capacity(plan.proposals.len());
    for proposal in &plan.proposals {
        results.push(preflight_one(
            proposal,
            stale,
            &blocked_ids,
            &colliding_ids,
            &cycle_ids,
        ));
    }

    RepairPreflightReport {
        all_ready: results
            .iter()
            .all(|result| result.status == RepairPreflightStatus::WouldExecute),
        results,
    }
}

fn preflight_one(
    proposal: &super::proposal::RepairProposal,
    stale: bool,
    blocked_ids: &BTreeSet<RepairProposalId>,
    colliding_ids: &BTreeSet<RepairProposalId>,
    cycle_ids: &BTreeSet<RepairProposalId>,
) -> RepairPreflightResult {
    let fail = |status: RepairPreflightStatus, detail: &str| RepairPreflightResult {
        proposal_id: proposal.id.clone(),
        status,
        detail: detail.to_string(),
    };

    // Action kind is the cheapest and most fundamental gate.
    if !proposal.action.is_executable() {
        return fail(
            RepairPreflightStatus::UnsupportedAction,
            &format!(
                "proposal requests '{}', which the Repair Center never executes",
                proposal.action.kind_label()
            ),
        );
    }

    if stale {
        return fail(
            RepairPreflightStatus::Stale,
            "the plan generation does not match the current audit generation",
        );
    }

    // `NeedsReview` is checked before the plan-conflict set: a proposal the
    // planner explicitly flagged for review is reported as such, never
    // downgraded to a generic Blocked.
    if proposal.safety == SafetyState::NeedsReview {
        return fail(
            RepairPreflightStatus::NeedsReview,
            "the proposal needs review and is never executed automatically",
        );
    }

    if proposal.safety == SafetyState::Blocked || blocked_ids.contains(&proposal.id) {
        let detail = proposal
            .blockers
            .first()
            .cloned()
            .unwrap_or_else(|| "the plan detected a blocking conflict".to_string());
        return fail(RepairPreflightStatus::Blocked, &detail);
    }

    if cycle_ids.contains(&proposal.id) {
        return fail(
            RepairPreflightStatus::OrderConflict,
            "this proposal participates in a rename cycle that cannot be ordered safely",
        );
    }

    let Some(destination) = proposal.destination() else {
        return fail(
            RepairPreflightStatus::UnsupportedAction,
            "executable actions must carry a destination",
        );
    };

    // Source identity: the strongest available check.
    let Ok(current) = capture_identity(&proposal.source_path) else {
        return fail(
            RepairPreflightStatus::MissingSource,
            "the source no longer exists",
        );
    };
    if current.kind != crate::dat::rename_apply::ObjectKind::RegularFile {
        return fail(
            RepairPreflightStatus::ChangedSourceIdentity,
            &format!(
                "the source is now a {}, never a rename source",
                current.kind.label()
            ),
        );
    }
    if let Some(expected) = proposal.expected_source_identity.as_ref()
        && !identity_matches(expected, &current)
    {
        return fail(
            RepairPreflightStatus::ChangedSourceIdentity,
            "the source is no longer the exact object that was proposed",
        );
    }

    // Destination safety.
    if !is_valid_destination(&proposal.source_path, destination) {
        return fail(
            RepairPreflightStatus::InvalidDestination,
            "the destination is unsafe, relative, equal to the source, or on a different filesystem",
        );
    }

    if colliding_ids.contains(&proposal.id)
        || std::fs::symlink_metadata(destination).is_ok()
        || has_case_collision(destination, &proposal.source_path)
    {
        return fail(
            RepairPreflightStatus::Collision,
            "the destination already exists or collides with another proposal",
        );
    }

    RepairPreflightResult {
        proposal_id: proposal.id.clone(),
        status: RepairPreflightStatus::WouldExecute,
        detail: "all requirements proven at dry-run time".to_string(),
    }
}

/// Whether `destination` is safe to rename/move to: absolute, a single safe
/// basename, not equal to the source, no `..` components, and (when both
/// parents resolve) on the same filesystem.
fn is_valid_destination(source: &std::path::Path, destination: &std::path::Path) -> bool {
    if !destination.is_absolute()
        || destination == source
        || destination
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return false;
    }
    let Some(basename) = destination.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !is_safe_basename(basename) {
        return false;
    }
    same_filesystem(source.parent(), destination.parent())
}

/// Whether a sibling of `destination` differs from it only by case. The
/// source's own basename is never a collision (renaming `a.bin` to `A.bin` is
/// the ordinary case-sensitive rename this layer supports).
fn has_case_collision(destination: &std::path::Path, source: &std::path::Path) -> bool {
    let Some(parent) = destination.parent() else {
        return false;
    };
    let Some(basename) = destination.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let source_basename = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let lower = basename.to_ascii_lowercase();
    std::fs::read_dir(parent)
        .map(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name();
                let name = name.to_str().unwrap_or("");
                name.to_ascii_lowercase() == lower && name != basename && name != source_basename
            })
        })
        .unwrap_or(false)
}

/// Same-filesystem check by device id; a missing directory is treated as
/// *not* the same filesystem so a move is refused rather than guessed.
fn same_filesystem(left: Option<&std::path::Path>, right: Option<&std::path::Path>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(left), std::fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::plan::{RepairPlanId, build_repair_plan};
    use crate::repair::proposal::{
        RepairAction, RepairEvidence, RepairEvidenceKind, RepairProposal, SafetyState,
    };

    fn identity_of(path: &std::path::Path) -> crate::dat::rename_apply::ObjectIdentity {
        capture_identity(path).unwrap()
    }

    fn proposal(
        id: &str,
        source: &std::path::Path,
        destination: &std::path::Path,
        identity: bool,
    ) -> RepairProposal {
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
                Some(identity_of(source))
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
        }
    }

    fn plan(proposals: Vec<RepairProposal>) -> RepairPlan {
        build_repair_plan(RepairPlanId::new("p").unwrap(), 1, 10, None, proposals)
    }

    #[test]
    fn a_clean_proposal_would_execute() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("A.bin");
        let p = plan(vec![proposal("a", &source, &destination, true)]);
        let report = run_repair_preflight(&p, 1);
        assert!(report.all_ready);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::WouldExecute
        );
    }

    #[test]
    fn missing_source_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("gone.bin");
        let destination = dir.path().join("A.bin");
        let p = plan(vec![proposal("a", &source, &destination, false)]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::MissingSource
        );
    }

    #[test]
    fn changed_source_identity_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"one").unwrap();
        let destination = dir.path().join("A.bin");
        // Capture the identity of the original object, then replace the source
        // with a different object; preflight must refuse the stale identity.
        let mut proposed = proposal("a", &source, &destination, true);
        std::fs::write(&source, b"a much longer replacement").unwrap();
        // Re-capture after the change so the *plan* now holds the new identity,
        // then force the stale one to simulate evidence going stale.
        proposed.expected_source_identity = Some(identity_of(&source));
        let stale_identity = crate::dat::rename_apply::ObjectIdentity {
            size_bytes: 3,
            modified_unix: 1,
            kind: crate::dat::rename_apply::ObjectKind::RegularFile,
            #[cfg(unix)]
            ino: u64::MAX,
            #[cfg(unix)]
            dev: 1,
        };
        proposed.expected_source_identity = Some(stale_identity);
        let p = plan(vec![proposed]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::ChangedSourceIdentity
        );
    }

    #[test]
    fn destination_collision_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("A.bin");
        std::fs::write(&destination, b"taken").unwrap();
        let p = plan(vec![proposal("a", &source, &destination, true)]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(report.results[0].status, RepairPreflightStatus::Collision);
    }

    #[test]
    fn a_deferred_action_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let mut proposed = proposal("a", &source, &dir.path().join("A.bin"), true);
        proposed.action =
            RepairAction::Deferred(crate::repair::proposal::DeferredActionKind::RebuildArchive);
        let p = plan(vec![proposed]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::UnsupportedAction
        );
    }

    #[test]
    fn a_needs_review_proposal_is_never_executable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let mut proposed = proposal("a", &source, &dir.path().join("A.bin"), true);
        proposed.safety = SafetyState::NeedsReview;
        let p = plan(vec![proposed]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(report.results[0].status, RepairPreflightStatus::NeedsReview);
        assert!(!report.all_ready);
    }

    #[test]
    fn stale_generation_marks_everything_stale() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let p = plan(vec![proposal(
            "a",
            &source,
            &dir.path().join("A.bin"),
            true,
        )]);
        let report = run_repair_preflight(&p, 99);
        assert_eq!(report.results[0].status, RepairPreflightStatus::Stale);
    }

    #[test]
    fn symlink_substitution_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("A.bin");
        let proposed = proposal("a", &source, &destination, true);
        std::fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &source).unwrap();
        let p = plan(vec![proposed]);
        let report = run_repair_preflight(&p, 1);
        assert_eq!(
            report.results[0].status,
            RepairPreflightStatus::ChangedSourceIdentity
        );
    }

    #[test]
    fn dry_run_never_mutates() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("A.bin");
        let p = plan(vec![proposal("a", &source, &destination, true)]);
        let before = std::fs::read(&source).unwrap();
        let _ = run_repair_preflight(&p, 1);
        assert_eq!(std::fs::read(&source).unwrap(), before);
        assert!(!destination.exists());
    }

    #[test]
    fn order_conflict_is_reported_for_cycle_members() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let p = plan(vec![
            proposal("a", &a, &b, true),
            proposal("b", &b, &a, true),
        ]);
        let report = run_repair_preflight(&p, 1);
        assert!(
            report
                .results
                .iter()
                .any(|r| r.status == RepairPreflightStatus::OrderConflict)
        );
    }
}
