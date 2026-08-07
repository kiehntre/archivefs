//! Read-only collision and conflict detection for rename proposals.
//!
//! Collision detection never resolves anything: it only names what blocks a
//! proposal and upgrades it to [`ProposalState::Conflict`]. "Existing target"
//! and "case-only collision" checks run against a sibling index built from the
//! already-completed audit's file list (no second scan); "two proposals, one
//! target" is detected across the proposals themselves. In every case the
//! collision is reported, never resolved.

use std::collections::BTreeMap;

use super::model::{CollisionInfo, CollisionKind, ProposalState, RenameProposal};

/// The basenames present in one directory, in exact and case-folded form.
///
/// Built from the audit's own file list, so no additional directory scan is
/// needed to answer "does this proposed name already exist here".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirSiblings {
    /// Exact basenames present.
    pub names: std::collections::BTreeSet<String>,
    /// Lowercased basenames present, for case-only collisions.
    pub names_lower: std::collections::BTreeSet<String>,
}

/// Detects a collision between `proposed_basename` and what already lives in
/// the same directory.
///
/// `current_basename` is excluded from the case-folded set so a proposal that
/// only changes the case of its own name is not reported as colliding with
/// itself.
pub fn detect_target_collision(
    current_basename: &str,
    proposed_basename: &str,
    siblings: &DirSiblings,
) -> Option<CollisionInfo> {
    if siblings.names.contains(proposed_basename) {
        return Some(CollisionInfo {
            kind: CollisionKind::ExistingTarget,
            colliding_with: Some(proposed_basename.to_string()),
            colliding_is_symlink: false,
            detail: format!("a file named '{proposed_basename}' already exists in the same folder"),
        });
    }

    // A case-only collision is any sibling that folds to the proposed name
    // but is *not* the current file itself. Comparing the case-folded current
    // name would wrongly skip the check when the current and proposed names
    // are themselves case variants (e.g. "game.bin" -> "Game.bin") while a
    // distinct sibling ("GAME.BIN") still collides. Only the exact current
    // entry is excluded.
    let proposed_lower = proposed_basename.to_ascii_lowercase();
    if let Some(colliding) = siblings.names.iter().find(|name| {
        name.to_ascii_lowercase() == proposed_lower && name.as_str() != current_basename
    }) {
        return Some(CollisionInfo {
            kind: CollisionKind::CaseCollision,
            colliding_with: Some(colliding.clone()),
            colliding_is_symlink: false,
            detail: format!(
                "a file whose name differs from '{proposed_basename}' only by case already \
                 exists in the same folder"
            ),
        });
    }

    None
}

/// Detects collisions *between* proposals: two proposals in the same directory
/// that would produce the same name, or names that differ only by case.
///
/// Deterministic: proposals are visited in their current order, and the first
/// colliding proposal in a group is reported against.
pub fn detect_proposal_collisions(proposals: &mut [RenameProposal]) {
    // Group actionable, named proposals by (parent dir, case-folded name).
    let mut by_dir: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let mut exact_of: BTreeMap<usize, String> = BTreeMap::new();
    for (index, proposal) in proposals.iter().enumerate() {
        if proposal.state != ProposalState::Suggested {
            continue;
        }
        let Some(proposed) = &proposal.proposed_basename else {
            continue;
        };
        let parent = proposal
            .source_path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        by_dir
            .entry((parent, proposed.to_ascii_lowercase()))
            .or_default()
            .push(index);
        exact_of.insert(index, proposed.clone());
    }

    for group in by_dir.values() {
        if group.len() < 2 {
            continue;
        }
        // Distinct exact names within one case-folded group. Every proposal in
        // a colliding group is reported as a Conflict: nothing here resolves
        // which one "wins" - the collision is surfaced, never decided.
        let mut exacts: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &index in group {
            exacts
                .entry(exact_of[&index].clone())
                .or_default()
                .push(index);
        }
        let kind = if exacts.len() == 1 {
            CollisionKind::TwoProposalsSameTarget
        } else {
            CollisionKind::CaseCollision
        };
        let detail = match kind {
            CollisionKind::TwoProposalsSameTarget => format!(
                "another proposal in the same folder also targets '{}'",
                proposals[group[0]]
                    .proposed_basename
                    .as_deref()
                    .unwrap_or_default()
            ),
            _ => "another proposal in the same folder targets a name differing only by case"
                .to_string(),
        };
        for &index in group {
            proposals[index].collision = Some(CollisionInfo {
                kind,
                colliding_with: proposals[index].proposed_basename.clone(),
                colliding_is_symlink: false,
                detail: detail.clone(),
            });
            proposals[index].state = ProposalState::Conflict;
            proposals[index].actionable = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::rename_plan::model::SourceObjectKind;
    use std::path::PathBuf;

    fn proposal(path: &str, proposed: &str) -> RenameProposal {
        RenameProposal {
            source_path: PathBuf::from(path),
            current_basename: "old.bin".to_string(),
            proposed_basename: Some(proposed.to_string()),
            platform: None,
            platform_display: None,
            source_id: "s".to_string(),
            source_display_name: "S".to_string(),
            game_name: Some("Game".to_string()),
            rom_name: Some(proposed.to_string()),
            verdict_label: "Exact".to_string(),
            match_confident: true,
            explanations: Vec::new(),
            state: ProposalState::Suggested,
            object_kind: SourceObjectKind::RegularFile,
            ambiguity_reason: None,
            collision: None,
            blockers: Vec::new(),
            extension_status: None,
            sanitisation_notes: Vec::new(),
            actionable: true,
        }
    }

    fn siblings(names: &[&str]) -> DirSiblings {
        let mut set = DirSiblings::default();
        for name in names {
            set.names.insert(name.to_string());
            set.names_lower.insert(name.to_ascii_lowercase());
        }
        set
    }

    #[test]
    fn existing_target_is_a_conflict() {
        let found = detect_target_collision(
            "game.bin",
            "Game (Europe).bin",
            &siblings(&["game.bin", "Game (Europe).bin"]),
        );
        assert_eq!(found.map(|c| c.kind), Some(CollisionKind::ExistingTarget));
    }

    #[test]
    fn case_only_target_is_a_conflict() {
        let found = detect_target_collision(
            "game.bin",
            "Game (Europe).BIN",
            &siblings(&["game.bin", "game (europe).bin"]),
        );
        assert_eq!(found.map(|c| c.kind), Some(CollisionKind::CaseCollision));
    }

    #[test]
    fn renaming_case_only_is_not_a_self_collision() {
        let found = detect_target_collision("game.bin", "Game.bin", &siblings(&["game.bin"]));
        assert!(found.is_none(), "{found:?}");
    }

    #[test]
    fn a_distinct_sibling_case_collision_is_not_hidden_by_a_self_case_change() {
        // current and proposed are themselves case variants of each other, but
        // a *distinct* sibling still case-folds to the proposed name - that
        // sibling must make the proposal a conflict.
        let found =
            detect_target_collision("game.bin", "Game.bin", &siblings(&["game.bin", "GAME.BIN"]));
        let collision = found.expect("the distinct sibling must collide");
        assert_eq!(collision.kind, CollisionKind::CaseCollision);
        assert_eq!(collision.colliding_with.as_deref(), Some("GAME.BIN"));
    }

    #[test]
    fn a_distinct_sibling_case_collision_is_detected_for_any_input_casing() {
        let found =
            detect_target_collision("Game.bin", "GAME.bin", &siblings(&["Game.bin", "gAmE.BIN"]));
        let collision = found.expect("the distinct sibling must collide");
        assert_eq!(collision.kind, CollisionKind::CaseCollision);
        assert_eq!(collision.colliding_with.as_deref(), Some("gAmE.BIN"));
    }

    #[test]
    fn no_sibling_means_no_conflict() {
        let found =
            detect_target_collision("game.bin", "Game (Europe).bin", &siblings(&["game.bin"]));
        assert!(found.is_none());
    }

    #[test]
    fn two_proposals_one_target_are_all_flagged() {
        let mut proposals = vec![
            proposal("/roms/a.bin", "Game.bin"),
            proposal("/roms/b.bin", "Game.bin"),
        ];
        detect_proposal_collisions(&mut proposals);
        // Nothing is resolved: both proposals report the conflict.
        assert!(proposals.iter().all(|p| p.state == ProposalState::Conflict));
        assert_eq!(
            proposals[0].collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::TwoProposalsSameTarget)
        );
        assert_eq!(
            proposals[1].collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::TwoProposalsSameTarget)
        );
    }

    #[test]
    fn case_only_proposal_collision_is_flagged_for_both() {
        let mut proposals = vec![
            proposal("/roms/a.bin", "Game.bin"),
            proposal("/roms/b.bin", "GAME.bin"),
        ];
        detect_proposal_collisions(&mut proposals);
        assert!(proposals.iter().all(|p| p.state == ProposalState::Conflict));
        assert_eq!(
            proposals[0].collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::CaseCollision)
        );
        assert_eq!(
            proposals[1].collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::CaseCollision)
        );
    }

    #[test]
    fn proposals_in_different_dirs_do_not_conflict() {
        let mut proposals = vec![
            proposal("/roms/a/game.bin", "Game.bin"),
            proposal("/roms/b/game.bin", "Game.bin"),
        ];
        detect_proposal_collisions(&mut proposals);
        assert!(
            proposals
                .iter()
                .all(|p| p.state == ProposalState::Suggested)
        );
    }
}
