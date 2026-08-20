//! Batch 11: read-only game/release/set hierarchy and multi-disc grouping -
//! milestone sections 9, 11, 20-21.
//!
//! # Evidence used
//!
//! Multi-disc membership is derived *only* from a confident DAT audit
//! verdict's own `game_name` (already available on
//! [`super::identity_orchestrator::IdentityResult::representation_match`])
//! run through [`crate::dat::classification::multidisc_group_key`] - the
//! same reviewed `"(Disc N of M)"` detector
//! [`crate::dat::classification::classify_catalogue`] itself already uses
//! internally, never a second looser parser (milestone section 27: "no
//! free-form filename parsing unless an existing reviewed parser already
//! exists"). A file with no confident DAT match never gets grouped into a
//! multi-disc set by this module - filenames that merely *look* similar
//! are never evidence (milestone section 11's explicit warning).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use super::dat_hash_representation::RepresentationMatchOutcome;
use super::identity_orchestrator::IdentityResult;
use super::library_planning::LibraryPlanInput;
use crate::dat::audit::AuditVerdict;
use crate::dat::classification::multidisc_group_key;

/// Milestone section 20's hierarchy, read-only, every level optional
/// (section 20: "Do not require every level to be known").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameReleaseSetHierarchy {
    /// The resolved canonical platform id, when known.
    pub platform: Option<String>,
    /// Milestone section 28's display-name policy, already applied: exact
    /// DAT release title, else the original basename - never a fuzzy
    /// guess. See [`display_label`].
    pub game_label: String,
    /// Whether `game_label` came from a confident DAT match (`true`) or
    /// fell back to the original basename (`false`) - so a caller/renderer
    /// never mistakes an unverified label for an authoritative one.
    pub game_label_is_dat_confirmed: bool,
    pub set: SetMembership,
}

/// Which set (if any) this file belongs to - milestone section 9/11.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SetMembership {
    /// Not part of any recognised multi-part set.
    SingleFile,
    /// A confidently DAT-recognised `"(Disc N of M)"` member.
    MultiDiscPart {
        base_title: String,
        part: u16,
        total: u16,
    },
}

/// Milestone section 28's display-name policy: exact DAT release title
/// first, then the original basename - fuzzy naming never outranks exact
/// evidence, and nothing here invents a title that was never confidently
/// matched.
fn display_label(identity: &IdentityResult, original_basename: &str) -> (String, bool) {
    match confident_dat_verdict(identity) {
        Some(AuditVerdict::Exact { game_name, .. }) => (game_name.clone(), true),
        _ => (original_basename.to_string(), false),
    }
}

fn confident_dat_verdict(identity: &IdentityResult) -> Option<&AuditVerdict> {
    match identity.representation_match.as_ref()? {
        RepresentationMatchOutcome::PhysicalOnly { verdict }
        | RepresentationMatchOutcome::NormalizedOnly { verdict } => {
            verdict.is_confident().then_some(verdict)
        }
        RepresentationMatchOutcome::BothAgree { verdict, .. } => {
            verdict.is_confident().then_some(verdict)
        }
        RepresentationMatchOutcome::Disagree { .. } | RepresentationMatchOutcome::NoMatch => None,
    }
}

fn set_membership(identity: &IdentityResult) -> SetMembership {
    let Some(AuditVerdict::Exact { game_name, .. }) = confident_dat_verdict(identity) else {
        return SetMembership::SingleFile;
    };
    match multidisc_group_key(game_name) {
        Some(token) => SetMembership::MultiDiscPart {
            base_title: token.base_title,
            part: token.part,
            total: token.total,
        },
        None => SetMembership::SingleFile,
    }
}

/// Builds one item's hierarchy view - milestone sections 20-21.
/// `resolved_platform` is the already-resolved canonical platform id (the
/// caller's own resolver output, e.g.
/// [`super::library_planning::identity_result_to_resolution`]'s
/// `.platform()`) - never re-derived here.
pub fn hierarchy_for(
    identity: &IdentityResult,
    resolved_platform: Option<&str>,
    original_basename: &str,
) -> GameReleaseSetHierarchy {
    let (game_label, game_label_is_dat_confirmed) = display_label(identity, original_basename);
    GameReleaseSetHierarchy {
        platform: resolved_platform.map(str::to_string),
        game_label,
        game_label_is_dat_confirmed,
        set: set_membership(identity),
    }
}

/// One multi-disc set - milestone section 50's example shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MultiDiscSet {
    pub base_title: String,
    /// `(part, source_path)`, sorted by part.
    pub discs: Vec<(u16, PathBuf)>,
    pub declared_total: u16,
}

/// Groups every confidently-recognised multi-disc member across `inputs`
/// into [`MultiDiscSet`]s, keyed by `(resolved platform, base_title)` -
/// never by filename similarity alone. A lone disc with no sibling present
/// in `inputs` is left ungrouped (its own [`hierarchy_for`] result already
/// names its `part`/`total`, so nothing is lost - this function only
/// reports sets where at least two members were actually supplied).
type MultidiscGroupKey = (String, String);
type MultidiscMember = (u16, u16, PathBuf);

pub fn group_multidisc_sets(inputs: &[LibraryPlanInput]) -> Vec<MultiDiscSet> {
    let mut by_key: BTreeMap<MultidiscGroupKey, Vec<MultidiscMember>> = BTreeMap::new();
    for input in inputs {
        let resolution = super::library_planning::identity_result_to_resolution(&input.identity, 0);
        let Some(platform) = resolution.platform() else {
            continue;
        };
        if let SetMembership::MultiDiscPart {
            base_title,
            part,
            total,
        } = set_membership(&input.identity)
        {
            by_key
                .entry((platform.to_string(), base_title))
                .or_default()
                .push((part, total, input.source_path.clone()));
        }
    }
    let mut sets = Vec::new();
    for ((_, base_title), mut members) in by_key {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|(part, _, path)| (*part, path.clone()));
        let declared_total = members[0].1;
        sets.push(MultiDiscSet {
            base_title,
            discs: members
                .into_iter()
                .map(|(part, _, path)| (part, path))
                .collect(),
            declared_total,
        });
    }
    sets.sort_by(|a, b| a.base_title.cmp(&b.base_title));
    sets
}

#[cfg(test)]
mod tests;
