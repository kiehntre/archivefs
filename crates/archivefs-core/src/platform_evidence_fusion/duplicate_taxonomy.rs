//! Batch 11: read-only duplicate classification - milestone sections
//! 13-19.
//!
//! # No re-hashing, no O(n^2) pairwise comparison
//!
//! Every classification here is derived from data the caller *already
//! computed* and supplied on each [`super::library_planning::LibraryPlanInput`]
//! (`physical_hash`/`normalized_hash`) or that already lives on its
//! [`super::identity_orchestrator::IdentityResult`] (a confident DAT audit
//! verdict's `game_name`/`rom_name`). [`group_duplicates`] builds simple
//! hash-map indices over these already-known keys and groups by them -
//! never a second hashing pass, never a pairwise `O(n^2)` byte comparison
//! (milestone section 53). This is deliberately a different, narrower job
//! than [`crate::repair::duplicate::prove_duplicate_content`], which
//! TOCTOU-safely *re-proves* one pair by reading both files live,
//! immediately before a real mutation - this module never reads a file at
//! all and produces planning statistics, not mutation-ready proof.
//!
//! # Never mutation authority
//!
//! A [`DuplicateGroup`] is read-only evidence, exactly like
//! [`crate::repair::duplicate::DuplicateContentProof`]: it names members
//! and a classification, never which one (if any) should be removed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::dat_hash_representation::RepresentationMatchOutcome;
use super::identity_orchestrator::IdentityResult;
use super::library_planning::LibraryPlanInput;
use crate::dat::audit::AuditVerdict;

/// Milestone section 13's taxonomy, at minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateClass {
    /// Same physical cryptographic hash - the strongest fact
    /// (milestone section 14).
    ExactPhysicalDuplicate,
    /// Different physical bytes, same reviewed normalized representation
    /// hash (milestone section 15 - e.g. N64 `.v64`/`.z64`).
    ExactNormalizedDuplicate,
    /// Both confidently match the same exact DAT release (same
    /// `game_name` *and* `rom_name`) but differ physically (milestone
    /// section 16).
    SameDatRelease,
    /// Same DAT `game_name`, but confidently matched to *different*
    /// `rom_name` entries under it - a different specific dump of the
    /// same catalogued game (milestone section 18's "same game, different
    /// dump" - not to be confused with section 17's revision case).
    SameGameDifferentDump,
    /// Distinguished from [`Self::SameGameDifferentDump`] by *structured*
    /// release lineage: two items whose caller-supplied
    /// [`super::release_relationship::ReleaseRelationship`] share a
    /// `cloneof` lineage root (Batch 12) - a real DAT parent/clone
    /// relationship, never a filename guess ("Rev 1"/"Rev 2" in a title is
    /// never itself evidence). Only produced when the caller actually
    /// supplies `LibraryPlanInput::release_relationship`; most callers
    /// with no DAT clone_of data simply never populate this axis, which is
    /// the honest default, not a bug.
    SameGameDifferentRevision,
    /// Same original basename (case-insensitive), same resolved platform,
    /// but no stronger evidence links them - the weakest class,
    /// deliberately never used for anything but reporting (milestone
    /// section 18: "never stronger... never deletion/merge authority").
    PossibleDuplicate,
    /// Compared and found unrelated - never produced by [`group_duplicates`]
    /// itself (which only ever emits groups of *related* items), but kept
    /// in the enum for a future caller that wants to record an explicit
    /// negative result for a pair it did compare.
    NotDuplicate,
}

impl DuplicateClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::ExactPhysicalDuplicate => "Exact physical duplicate",
            Self::ExactNormalizedDuplicate => "Exact normalized duplicate",
            Self::SameDatRelease => "Same DAT release",
            Self::SameGameDifferentDump => "Same game, different dump",
            Self::SameGameDifferentRevision => "Same game, different revision",
            Self::PossibleDuplicate => "Possible duplicate",
            Self::NotDuplicate => "Not a duplicate",
        }
    }
}

/// One group of related items - milestone section 19. Stable ordering:
/// `members` is always sorted by path, and [`group_duplicates`]'s overall
/// result is sorted by `(classification, first member path)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateGroup {
    pub classification: DuplicateClass,
    pub members: Vec<PathBuf>,
    /// Why these members were grouped - human-readable, cites the real
    /// shared key (hash prefix, DAT release name, ...), never invented.
    pub basis: String,
    pub confidence: DuplicateGroupConfidence,
}

/// How strong the shared evidence is - independent of, and never a
/// substitute for, [`DuplicateClass`] itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateGroupConfidence {
    /// A cryptographic hash or DAT release match.
    Strong,
    /// A basename-only match (never stronger - section 18).
    Weak,
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
        // A genuine physical-vs-normalized disagreement is a conflict, not
        // a release key - never used for grouping.
        RepresentationMatchOutcome::Disagree { .. } | RepresentationMatchOutcome::NoMatch => None,
    }
}

/// Groups `inputs` into [`DuplicateGroup`]s. Every axis below is indexed
/// once (`O(n)` to build, `O(n)` to walk groups) - never a pairwise
/// comparison. An item may appear in at most one returned group: once
/// claimed by its strongest class, it is never also reported under a
/// weaker one.
pub fn group_duplicates(inputs: &[LibraryPlanInput]) -> Vec<DuplicateGroup> {
    let mut claimed = vec![false; inputs.len()];
    let mut groups = Vec::new();

    macro_rules! index_and_emit {
        ($key_fn:expr, $classification:expr, $confidence:expr, $basis_fn:expr) => {{
            let mut index: BTreeMap<_, Vec<usize>> = BTreeMap::new();
            for (i, input) in inputs.iter().enumerate() {
                if claimed[i] {
                    continue;
                }
                if let Some(key) = $key_fn(input) {
                    index.entry(key).or_default().push(i);
                }
            }
            for (key, member_indices) in index {
                if member_indices.len() < 2 {
                    continue;
                }
                let mut members: Vec<PathBuf> = member_indices
                    .iter()
                    .map(|&i| inputs[i].source_path.clone())
                    .collect();
                members.sort();
                for &i in &member_indices {
                    claimed[i] = true;
                }
                groups.push(DuplicateGroup {
                    classification: $classification,
                    members,
                    basis: $basis_fn(&key),
                    confidence: $confidence,
                });
            }
        }};
    }

    index_and_emit!(
        |input: &LibraryPlanInput| input.physical_hash.clone(),
        DuplicateClass::ExactPhysicalDuplicate,
        DuplicateGroupConfidence::Strong,
        |hash: &String| format!("identical physical hash {hash}")
    );

    index_and_emit!(
        |input: &LibraryPlanInput| input.normalized_hash.clone(),
        DuplicateClass::ExactNormalizedDuplicate,
        DuplicateGroupConfidence::Strong,
        |hash: &String| format!("identical normalized representation hash {hash}")
    );

    index_and_emit!(
        |input: &LibraryPlanInput| confident_dat_verdict(&input.identity).and_then(|v| match v {
            AuditVerdict::Exact {
                game_name,
                rom_name,
                ..
            } => Some((game_name.clone(), rom_name.clone())),
            _ => None,
        }),
        DuplicateClass::SameDatRelease,
        DuplicateGroupConfidence::Strong,
        |key: &(String, String)| format!(
            "both confidently match the same DAT release: {} / {}",
            key.0, key.1
        )
    );

    index_and_emit!(
        |input: &LibraryPlanInput| confident_dat_verdict(&input.identity).and_then(|v| match v {
            AuditVerdict::Exact { game_name, .. } => Some(game_name.clone()),
            _ => None,
        }),
        DuplicateClass::SameGameDifferentDump,
        DuplicateGroupConfidence::Strong,
        |game_name: &String| format!(
            "both confidently match the same DAT game {game_name:?}, under different rom entries"
        )
    );

    // Batch 12: real DAT `cloneof` lineage (never a filename guess) - two
    // items sharing a lineage root but *not* already claimed by a
    // stronger, same-release class above are genuinely different specific
    // releases of the same underlying game.
    index_and_emit!(
        |input: &LibraryPlanInput| {
            let relationship = input.release_relationship.as_ref()?;
            relationship.lineage_root().map(str::to_string)
        },
        DuplicateClass::SameGameDifferentRevision,
        DuplicateGroupConfidence::Strong,
        |root: &String| format!("share a DAT-declared cloneof lineage rooted at {root:?}")
    );

    index_and_emit!(
        |input: &LibraryPlanInput| {
            let resolution =
                super::library_planning::identity_result_to_resolution(&input.identity, 0);
            let platform = resolution.platform()?.to_string();
            let stem = input
                .source_path
                .file_stem()?
                .to_str()?
                .to_ascii_lowercase();
            Some((platform, stem))
        },
        DuplicateClass::PossibleDuplicate,
        DuplicateGroupConfidence::Weak,
        |key: &(String, String)| format!(
            "same basename ({:?}) and resolved platform ({}) - filename similarity only, never \
             stronger",
            key.1, key.0
        )
    );

    groups.sort_by(|a, b| {
        (a.classification, &a.members)
            .partial_cmp(&(b.classification, &b.members))
            .unwrap()
    });
    groups
}

#[cfg(test)]
mod tests;
