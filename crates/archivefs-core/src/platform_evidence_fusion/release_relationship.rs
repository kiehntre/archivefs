//! Batch 12: DAT `cloneof` relationship plumbing - milestone sections
//! 13-17.
//!
//! # Where this data was dying before this batch
//!
//! `DatGameEntry::clone_of` (parsed straight from the DAT's `cloneof`/
//! `cloneofid` XML attribute - `crates/archivefs-core/src/dat/model.rs`)
//! was read into `DatIndex` (via `DatIndex::build`,
//! `crates/archivefs-core/src/dat/index.rs`) but never carried on
//! `DatRomRef`, so `dat::audit::handle_candidates` had no way to attach it
//! to an `AuditVerdict`, and `AuditVerdict` (a stable, pervasively-matched
//! type used across ~20 files) was left untouched rather than adding a
//! field to it and rippling that change through every existing call site.
//! Instead: `DatRomRef` now also carries `clone_of` (a genuinely additive
//! field), and `DatIndex` now also carries a `game_clone_of` name->parent
//! map built once at index time. [`resolve_release_relationship`] is the
//! new, independent lookup a caller with a confident `AuditVerdict::Exact`
//! and the same `DatIndex` can use to answer "is this release a clone, and
//! of what?" - `AuditVerdict`'s own shape is never touched.
//!
//! # What `cloneof` means here
//!
//! DAT `cloneof` is a MAME-style lineage relationship: a clone is a
//! variant of its parent - in practice, for console ROM DATs, this is
//! overwhelmingly a revision, regional variant, or bugfix release of the
//! same underlying game, not a mechanically unrelated title. This module
//! treats "two releases share a `cloneof` lineage" as real, structured
//! evidence of "same game, different specific release" - never a filename
//! guess.

use std::collections::HashMap;

use serde::Serialize;

use crate::dat::index::DatIndex;

/// One release's DAT-declared lineage - milestone section 14. Reuses
/// `DatGameEntry::clone_of` verbatim; nothing here is derived from a title
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReleaseRelationship {
    /// No DAT match exists to ask the question of.
    Unknown,
    /// A confidently-matched release the DAT declares no `cloneof` for -
    /// either a genuine parent/standalone release, or simply a DAT that
    /// carries no lineage data at all for this game.
    Canonical { game_name: String },
    /// A confidently-matched release the DAT explicitly declares a parent
    /// for.
    CloneOf { game_name: String, parent: String },
}

impl ReleaseRelationship {
    /// The lineage root used to group same-family releases together: a
    /// `CloneOf`'s declared parent, or a `Canonical`'s own name (it *is*
    /// its own root). `None` for `Unknown`.
    pub fn lineage_root(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Canonical { game_name } => Some(game_name),
            Self::CloneOf { parent, .. } => Some(parent),
        }
    }

    pub fn game_name(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Canonical { game_name } | Self::CloneOf { game_name, .. } => Some(game_name),
        }
    }
}

/// Looks up `game_name`'s DAT-declared lineage in `index` - O(1), no
/// re-parsing, no filename heuristics. `None` from
/// `DatIndex::game_clone_of` (the game exists in the DAT with no
/// `cloneof`) becomes [`ReleaseRelationship::Canonical`]; a `game_name`
/// absent from the map entirely (should not happen for a name the caller
/// actually matched against this same index, but handled honestly rather
/// than panicking) also falls back to `Canonical` rather than a fabricated
/// parent.
pub fn resolve_release_relationship(index: &DatIndex, game_name: &str) -> ReleaseRelationship {
    match index.game_clone_of.get(game_name) {
        Some(Some(parent)) => ReleaseRelationship::CloneOf {
            game_name: game_name.to_string(),
            parent: parent.clone(),
        },
        _ => ReleaseRelationship::Canonical {
            game_name: game_name.to_string(),
        },
    }
}

/// Whether `a` and `b` share a DAT-declared lineage root - the structural
/// "same game, different revision" test milestone section 16 asks for.
/// Two `Unknown`s, or two releases with no shared root, are never related.
pub fn same_lineage(a: &ReleaseRelationship, b: &ReleaseRelationship) -> bool {
    match (a.lineage_root(), b.lineage_root()) {
        (Some(root_a), Some(root_b)) => root_a == root_b,
        _ => false,
    }
}

/// Groups a batch of `(key, relationship)` pairs by lineage root, for a
/// caller building a revision-grouping report - never mutates, never
/// re-looks-up the DAT.
pub fn group_by_lineage<'a, K: Clone>(
    items: impl IntoIterator<Item = (K, &'a ReleaseRelationship)>,
) -> HashMap<String, Vec<K>> {
    let mut groups: HashMap<String, Vec<K>> = HashMap::new();
    for (key, relationship) in items {
        if let Some(root) = relationship.lineage_root() {
            groups.entry(root.to_string()).or_default().push(key);
        }
    }
    groups
}

#[cfg(test)]
mod tests;
