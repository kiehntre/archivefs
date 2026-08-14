//! The batch repair plan and its global conflict detection.
//!
//! A [`RepairPlan`] holds many [`RepairProposal`]s plus every *global* conflict
//! the planner can see before any mutation. The plan is deterministic: the
//! proposals and the conflicts are both ordered, and building it performs no
//! filesystem mutation (the only reads are `symlink_metadata` on destinations
//! and canonicalisation of parent directories, both read-only).
//!
//! # Fail closed
//!
//! Any conflict marks the affected proposals `Blocked` and records the conflict
//! on the plan. The executor refuses to start a batch whose plan has any
//! conflict or any non-executable proposal: **no action may start if the
//! planner already knows the batch is invalid.** There is deliberately no
//! "apply the safe subset" mode in this foundation, and no force mode.
//!
//! # Rename cycles are detected, never solved
//!
//! A cycle (`A -> B`, `B -> A`, or any longer loop over sources and
//! destinations) is a [`PlanConflictKind::RenameCycle`]; the batch is blocked.
//! This foundation does not attempt temporary-rename staging.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::proposal::{RepairAction, RepairProposal, RepairProposalId};

/// A durable, single-component identifier for one repair plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepairPlanId(String);

impl RepairPlanId {
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty()
            || id.len() > 128
            || id.contains(['/', '\\', '\0'])
            || id == "."
            || id == ".."
        {
            None
        } else {
            Some(Self(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepairPlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A conflict the planner detected before any mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanConflictKind {
    /// Two or more proposals share one proposal id.
    DuplicateProposalId,
    /// Two or more proposals act on the same source path.
    DuplicateSource,
    /// Two or more executable proposals target the same destination.
    DuplicateDestination,
    /// An executable proposal's destination already exists on disk.
    DestinationExists,
    /// Renames form a cycle over sources and destinations.
    RenameCycle,
    /// One path in the batch is an ancestor of another (parent/child
    /// interference), which this foundation never stages around.
    ParentChildInterference,
    /// A proposal is not executable (a deferred future action, or a proposal
    /// the planner classified below `Safe`).
    UnsupportedProposal,
}

impl PlanConflictKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::DuplicateProposalId => "duplicate proposal id",
            Self::DuplicateSource => "duplicate source",
            Self::DuplicateDestination => "duplicate destination",
            Self::DestinationExists => "destination already exists",
            Self::RenameCycle => "rename cycle",
            Self::ParentChildInterference => "parent/child path interference",
            Self::UnsupportedProposal => "unsupported proposal",
        }
    }
}

/// One detected conflict, with every proposal it touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanConflict {
    pub kind: PlanConflictKind,
    pub detail: String,
    pub proposal_ids: Vec<RepairProposalId>,
}

/// A batch repair plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPlan {
    pub id: RepairPlanId,
    /// A generation stamp. The executor requires the caller's current
    /// generation to equal this, so a stale plan is never executed.
    pub generation: u64,
    pub created_at_unix: u64,
    /// The scan/audit that produced the proposals, when one exists.
    pub source_scan_id: Option<String>,
    /// Proposals in deterministic order (sorted by id).
    pub proposals: Vec<RepairProposal>,
    /// Every global conflict detected at build time, deterministically ordered.
    pub conflicts: Vec<PlanConflict>,
}

impl RepairPlan {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    /// Whether every proposal is executable and no conflict was detected.
    /// This is the executor's precondition: the batch must be entirely safe
    /// before any action may start.
    pub fn all_executable(&self) -> bool {
        !self.has_conflicts() && self.proposals.iter().all(RepairProposal::actionable)
    }

    /// The executable proposals, in plan order. Only meaningful on a plan
    /// where [`Self::all_executable`] holds.
    pub fn executable_proposals(&self) -> impl Iterator<Item = &RepairProposal> {
        self.proposals
            .iter()
            .filter(|proposal| proposal.actionable())
    }

    /// The conflicts touching a given proposal, deterministically ordered.
    pub fn conflicts_for(&self, proposal_id: &RepairProposalId) -> Vec<&PlanConflict> {
        self.conflicts
            .iter()
            .filter(|conflict| conflict.proposal_ids.contains(proposal_id))
            .collect()
    }
}

/// Builds a deterministic repair plan, detecting every global conflict the
/// planner can see without mutating anything.
pub fn build_repair_plan(
    id: RepairPlanId,
    generation: u64,
    created_at_unix: u64,
    source_scan_id: Option<String>,
    mut proposals: Vec<RepairProposal>,
) -> RepairPlan {
    proposals.sort_by(|a, b| a.id.cmp(&b.id));
    let mut conflicts = detect_plan_conflicts(&proposals);
    sort_conflicts(&mut conflicts);

    RepairPlan {
        id,
        generation,
        created_at_unix,
        source_scan_id,
        proposals,
        conflicts,
    }
}

/// Recomputes every global conflict over the given proposals.
///
/// This is order-independent and is the authoritative validation the executor
/// re-runs immediately before transaction construction. The executor never
/// trusts a plan's *stored* `conflicts` field as authoritative - a mutated or
/// deserialised plan must be re-checked here.
pub fn detect_plan_conflicts(proposals: &[RepairProposal]) -> Vec<PlanConflict> {
    let mut conflicts = Vec::new();

    // 1. Duplicate proposal ids.
    {
        let mut by_id: BTreeMap<&RepairProposalId, Vec<&RepairProposal>> = BTreeMap::new();
        for proposal in proposals {
            by_id.entry(&proposal.id).or_default().push(proposal);
        }
        for (id, group) in by_id {
            if group.len() > 1 {
                conflicts.push(PlanConflict {
                    kind: PlanConflictKind::DuplicateProposalId,
                    detail: format!("proposal id '{id}' appears {} times", group.len()),
                    proposal_ids: group.into_iter().map(|p| p.id.clone()).collect(),
                });
            }
        }
    }

    // 2. Duplicate source (after path normalisation).
    {
        let mut by_source: BTreeMap<PathBuf, Vec<&RepairProposal>> = BTreeMap::new();
        for proposal in proposals {
            by_source
                .entry(normalise_path(&proposal.source_path))
                .or_default()
                .push(proposal);
        }
        for (source, group) in by_source {
            if group.len() > 1 {
                conflicts.push(PlanConflict {
                    kind: PlanConflictKind::DuplicateSource,
                    detail: format!(
                        "{} proposals act on the same source '{}'",
                        group.len(),
                        source.display()
                    ),
                    proposal_ids: group.into_iter().map(|p| p.id.clone()).collect(),
                });
            }
        }
    }

    // 3. Unsupported proposals (deferred future actions, anything the planner
    //    did not classify Safe, or an executable proposal with no audited
    //    source identity). These block the batch outright.
    for proposal in proposals {
        if proposal.action.is_executable() && proposal.expected_source_identity.is_none() {
            conflicts.push(PlanConflict {
                kind: PlanConflictKind::UnsupportedProposal,
                detail: format!(
                    "proposal '{}' is executable but has no audited source identity",
                    proposal.id
                ),
                proposal_ids: vec![proposal.id.clone()],
            });
        } else if !proposal.actionable() {
            let detail = match &proposal.action {
                RepairAction::Deferred(kind) => format!(
                    "proposal '{}' requests '{}', which this foundation never executes",
                    proposal.id,
                    kind.label()
                ),
                RepairAction::RenamePath { .. } | RepairAction::MovePath { .. } => format!(
                    "proposal '{}' is not safe to execute (safety = {})",
                    proposal.id,
                    proposal.safety.label()
                ),
            };
            conflicts.push(PlanConflict {
                kind: PlanConflictKind::UnsupportedProposal,
                detail,
                proposal_ids: vec![proposal.id.clone()],
            });
        }
    }

    // 4. Executable-only destination analysis.
    let executables: Vec<&RepairProposal> = proposals
        .iter()
        .filter(|proposal| proposal.actionable())
        .collect();

    // 4a. Duplicate destinations.
    {
        let mut by_dest: BTreeMap<PathBuf, Vec<&RepairProposal>> = BTreeMap::new();
        for proposal in &executables {
            if let Some(destination) = proposal.destination() {
                by_dest
                    .entry(normalise_path(destination))
                    .or_default()
                    .push(proposal);
            }
        }
        for (destination, group) in by_dest {
            if group.len() > 1 {
                conflicts.push(PlanConflict {
                    kind: PlanConflictKind::DuplicateDestination,
                    detail: format!(
                        "{} proposals target the same destination '{}'",
                        group.len(),
                        destination.display()
                    ),
                    proposal_ids: group.into_iter().map(|p| p.id.clone()).collect(),
                });
            }
        }
    }

    // 4b. Destination already exists on disk (read-only check).
    for proposal in &executables {
        if let Some(destination) = proposal.destination()
            && std::fs::symlink_metadata(destination).is_ok()
        {
            conflicts.push(PlanConflict {
                kind: PlanConflictKind::DestinationExists,
                detail: format!(
                    "destination '{}' already exists and is never overwritten",
                    destination.display()
                ),
                proposal_ids: vec![proposal.id.clone()],
            });
        }
    }

    // 4c. Rename cycles over source -> destination edges.
    if let Some(cycle) = find_rename_cycle(&executables) {
        conflicts.push(PlanConflict {
            kind: PlanConflictKind::RenameCycle,
            detail: format!(
                "renames form a cycle: {}",
                cycle
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            proposal_ids: cycle,
        });
    }

    // 4d. Parent/child path interference among every source and destination.
    if let Some(conflict) = find_parent_child_interference(proposals) {
        conflicts.push(conflict);
    }

    conflicts
}

/// Deterministic ordering of conflicts: by kind, then by the joined proposal ids.
fn sort_conflicts(conflicts: &mut [PlanConflict]) {
    conflicts.sort_by(|a, b| {
        let kind = format!("{:?}", a.kind);
        let other_kind = format!("{:?}", b.kind);
        kind.cmp(&other_kind).then_with(|| {
            a.proposal_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
                .cmp(
                    &b.proposal_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )
        })
    });
}

/// Normalises a path for *conflict detection only*: the parent directory is
/// canonicalised (read-only; follows directory symlinks) and the final
/// component is re-joined. Never used for execution. Falls back to the raw
/// path when the parent cannot be canonicalised.
fn normalise_path(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    match path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
    {
        Some(parent) => parent.join(file_name),
        None => path.to_path_buf(),
    }
}

/// Detects a directed cycle over the sources and destinations of the given
/// executable proposals, returning the proposal ids in the cycle (in
/// deterministic order). A proposal whose destination equals its own source is
/// itself a one-edge cycle.
fn find_rename_cycle(executables: &[&RepairProposal]) -> Option<Vec<RepairProposalId>> {
    // Build a map from a normalised path to the proposals leaving it.
    let mut edges: BTreeMap<PathBuf, Vec<&RepairProposal>> = BTreeMap::new();
    for proposal in executables {
        if proposal.destination().is_some() {
            let source = normalise_path(&proposal.source_path);
            edges.entry(source).or_default().push(proposal);
        }
    }

    // DFS with colouring: 0 = unvisited, 1 = in current path, 2 = done.
    let mut colour: BTreeMap<PathBuf, u8> = BTreeMap::new();
    let mut stack: Vec<PathBuf> = Vec::new();

    fn visit(
        node: &PathBuf,
        edges: &BTreeMap<PathBuf, Vec<&RepairProposal>>,
        colour: &mut BTreeMap<PathBuf, u8>,
        stack: &mut Vec<PathBuf>,
    ) -> Option<Vec<RepairProposalId>> {
        colour.insert(node.clone(), 1);
        stack.push(node.clone());
        if let Some(outgoing) = edges.get(node) {
            for proposal in outgoing {
                if let Some(destination) = proposal.destination() {
                    let next = normalise_path(destination);
                    match colour.get(&next).copied().unwrap_or(0) {
                        1 => {
                            // Cycle: from the first occurrence of `next` in the
                            // stack to the end. Collect the proposal ids that
                            // produced the edges in the cycle.
                            let start = stack.iter().position(|p| p == &next).unwrap_or(0);
                            let mut ids: Vec<RepairProposalId> = Vec::new();
                            for window in stack[start..].windows(2) {
                                let from = &window[0];
                                if let Some(candidates) = edges.get(from) {
                                    for candidate in candidates {
                                        if let Some(d) = candidate.destination()
                                            && normalise_path(d) == window[1]
                                        {
                                            ids.push(candidate.id.clone());
                                            break;
                                        }
                                    }
                                }
                            }
                            // Close the cycle: the edge from the last stack node
                            // back to `next`.
                            let last = stack.last().cloned().unwrap_or_default();
                            if let Some(candidates) = edges.get(&last) {
                                for candidate in candidates {
                                    if let Some(d) = candidate.destination()
                                        && normalise_path(d) == next
                                    {
                                        ids.push(candidate.id.clone());
                                        break;
                                    }
                                }
                            }
                            ids.sort();
                            return Some(ids);
                        }
                        0 => {
                            if let Some(ids) = visit(&next, edges, colour, stack) {
                                return Some(ids);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        stack.pop();
        colour.insert(node.clone(), 2);
        None
    }

    for start in edges.keys() {
        if colour.get(start).copied().unwrap_or(0) == 0
            && let Some(ids) = visit(start, &edges, &mut colour, &mut stack)
        {
            return Some(ids);
        }
    }
    None
}

/// Detects any pair of distinct paths (sources and destinations) where one is
/// an ancestor of the other at a path-component boundary. Files only move here,
/// so this is conservative: such a relationship is never staged around.
fn find_parent_child_interference(proposals: &[RepairProposal]) -> Option<PlanConflict> {
    let mut paths: Vec<(PathBuf, &RepairProposal)> = Vec::new();
    for proposal in proposals {
        paths.push((normalise_path(&proposal.source_path), proposal));
        if let Some(destination) = proposal.destination() {
            paths.push((normalise_path(destination), proposal));
        }
    }
    // Deterministic regardless of input order.
    paths.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            if paths[i].1.id == paths[j].1.id {
                continue;
            }
            let (left, right) = (&paths[i].0, &paths[j].0);
            if left == right {
                continue;
            }
            if path_is_ancestor_of(left, right) || path_is_ancestor_of(right, left) {
                let (ancestor, descendant) = if path_is_ancestor_of(left, right) {
                    (left, right)
                } else {
                    (right, left)
                };
                let mut ids = vec![paths[i].1.id.clone(), paths[j].1.id.clone()];
                ids.sort();
                ids.dedup();
                return Some(PlanConflict {
                    kind: PlanConflictKind::ParentChildInterference,
                    detail: format!(
                        "path '{}' is an ancestor of '{}'; parent/child interference is never staged around",
                        ancestor.display(),
                        descendant.display()
                    ),
                    proposal_ids: ids,
                });
            }
        }
    }
    None
}

/// Whether `candidate` is a strict ancestor of `path` at a component boundary.
fn path_is_ancestor_of(ancestor: &Path, candidate: &Path) -> bool {
    let ancestor_components: Vec<Component<'_>> = ancestor.components().collect();
    let candidate_components: Vec<Component<'_>> = candidate.components().collect();
    if candidate_components.len() <= ancestor_components.len() {
        return false;
    }
    ancestor_components
        .iter()
        .zip(candidate_components.iter())
        .all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::proposal::{
        RepairAction, RepairEvidence, RepairEvidenceKind, RepairProposal, RepairProposalId,
        SafetyState,
    };

    fn proposal(id: &str, source: &str, destination: Option<&str>) -> RepairProposal {
        let action = match destination {
            Some(destination) => RepairAction::RenamePath {
                destination: PathBuf::from(destination),
            },
            None => {
                RepairAction::Deferred(crate::repair::proposal::DeferredActionKind::FetchMissing)
            }
        };
        RepairProposal {
            id: RepairProposalId::new(id).unwrap(),
            action,
            source_path: PathBuf::from(source),
            reason: "test".to_string(),
            evidence: vec![RepairEvidence::new(
                RepairEvidenceKind::UserRequestedOrganisation,
                "test",
            )],
            // Test proposals are executable, so they carry a synthetic audited
            // identity (these are fake paths; nothing is captured).
            expected_source_identity: Some(crate::dat::rename_apply::ObjectIdentity {
                size_bytes: 1,
                modified_unix: 1,
                kind: crate::dat::rename_apply::ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 1,
                #[cfg(unix)]
                dev: 1,
            }),
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

    fn plan(id: &str, proposals: Vec<RepairProposal>) -> RepairPlan {
        build_repair_plan(RepairPlanId::new(id).unwrap(), 1, 10, None, proposals)
    }

    #[test]
    fn duplicate_proposal_ids_are_detected() {
        let mut a = proposal("dup", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin"));
        let mut b = proposal("dup", "/tmp/roms/b.bin", Some("/tmp/roms/B.bin"));
        a.expected_source_identity = None;
        b.expected_source_identity = None;
        let p = plan("p", vec![a, b]);
        assert!(p.has_conflicts());
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::DuplicateProposalId)
        );
        assert!(!p.all_executable());
    }

    #[test]
    fn same_source_twice_is_a_conflict() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin")),
                proposal("b", "/tmp/roms/a.bin", Some("/tmp/roms/B.bin")),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::DuplicateSource)
        );
    }

    #[test]
    fn same_destination_is_a_conflict() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin")),
                proposal("b", "/tmp/roms/b.bin", Some("/tmp/roms/A.bin")),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::DuplicateDestination)
        );
    }

    #[test]
    fn an_existing_destination_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"a").unwrap();
        std::fs::write(dir.path().join("A.bin"), b"existing").unwrap();
        let p = plan(
            "p",
            vec![proposal(
                "a",
                dir.path().join("a.bin").to_str().unwrap(),
                Some(dir.path().join("A.bin").to_str().unwrap()),
            )],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::DestinationExists)
        );
    }

    #[test]
    fn a_two_proposal_rename_cycle_is_detected() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/b.bin")),
                proposal("b", "/tmp/roms/b.bin", Some("/tmp/roms/a.bin")),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::RenameCycle)
        );
        assert!(!p.all_executable());
    }

    #[test]
    fn a_longer_rename_cycle_is_detected() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/b.bin")),
                proposal("b", "/tmp/roms/b.bin", Some("/tmp/roms/c.bin")),
                proposal("c", "/tmp/roms/c.bin", Some("/tmp/roms/a.bin")),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::RenameCycle)
        );
    }

    #[test]
    fn a_non_cycle_chain_is_not_a_cycle() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/b.bin")),
                proposal("b", "/tmp/roms/b.bin", Some("/tmp/roms/c.bin")),
                proposal("c", "/tmp/roms/c.bin", Some("/tmp/roms/d.bin")),
            ],
        );
        assert!(
            !p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::RenameCycle)
        );
    }

    #[test]
    fn parent_child_interference_is_detected() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/dir/a.bin", Some("/tmp/roms/dir/A.bin")),
                // Another proposal whose destination is a parent directory of a's.
                proposal("b", "/tmp/roms/x.bin", Some("/tmp/roms/dir")),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::ParentChildInterference)
        );
    }

    #[test]
    fn siblings_in_one_directory_are_not_parent_child_conflicts() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin")),
                proposal("b", "/tmp/roms/b.bin", Some("/tmp/roms/B.bin")),
            ],
        );
        assert!(
            !p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::ParentChildInterference)
        );
    }

    #[test]
    fn a_deferred_action_blocks_the_plan() {
        let p = plan(
            "p",
            vec![
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin")),
                proposal("d", "/tmp/roms/d.bin", None),
            ],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::UnsupportedProposal)
        );
        assert!(!p.all_executable());
    }

    #[test]
    fn a_clean_plan_is_deterministic_and_executable() {
        let p = plan(
            "p",
            vec![
                proposal("z", "/tmp/roms/z.bin", Some("/tmp/roms/Z.bin")),
                proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin")),
            ],
        );
        // Order is deterministic: sorted by id.
        assert_eq!(p.proposals[0].id.as_str(), "a");
        assert_eq!(p.proposals[1].id.as_str(), "z");
        assert!(!p.has_conflicts());
        assert!(p.all_executable());
        // The (empty) conflicts vector is stable to re-parse.
        let reparsed: RepairPlan =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(reparsed, p);
    }

    #[test]
    fn a_needs_review_proposal_blocks_the_plan() {
        let mut reviewed = proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/A.bin"));
        reviewed.safety = SafetyState::NeedsReview;
        let p = plan("p", vec![reviewed]);
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::UnsupportedProposal)
        );
        assert!(!p.all_executable());
    }

    #[test]
    fn destination_equals_source_is_a_cycle() {
        let p = plan(
            "p",
            vec![proposal("a", "/tmp/roms/a.bin", Some("/tmp/roms/a.bin"))],
        );
        assert!(
            p.conflicts
                .iter()
                .any(|c| c.kind == PlanConflictKind::RenameCycle)
        );
    }
}
