//! Declaration-identity lookup and bounded, cycle-guarded chain walking.
//!
//! Every dependency in a DAT is declared by *name*: `cloneof="parent"`,
//! `merge="rom.bin"`, `<device_ref name="dev">`. A name is not an identity,
//! and turning one into an identity is the single most dangerous step in this
//! stage. This module is the only place that conversion happens, so the rules
//! live in one auditable spot:
//!
//! - A name that resolves to exactly one catalogue entry is a usable
//!   reference. A name that resolves to two or more is [`SetRef::Duplicate`]
//!   and is never resolved by taking the first - array order is not identity.
//! - A name that resolves to nothing is [`SetRef::Absent`]. It is reported,
//!   never read as "there is no requirement".
//! - A declared-but-empty name is [`DeclaredName::Malformed`], kept distinct
//!   from an absent attribute: `cloneof=""` states a broken relationship,
//!   whereas no `cloneof` at all states no relationship.
//! - Member names are matched **only inside an already-resolved provider
//!   set**, never across the catalogue. This is what stops an unrelated
//!   same-named file in some other set from satisfying a `merge=`.
//!
//! Matching is exact (after trimming) rather than case-insensitive. A DAT
//! whose `merge=` differs from its target declaration only by case therefore
//! fails closed as an unresolvable target rather than being matched
//! approximately - the wrong direction to guess in.

use std::collections::HashMap;

use crate::dat::index::{DatDiskKey, DatMemberKey};
use crate::dat::model::{DatDiskEntry, DatGameEntry, DatRomEntry};
use crate::dat::set::{declared_disks, declared_roms};

/// How far any single dependency chain may be walked before this stage
/// refuses to continue.
///
/// Real catalogue chains are shallow: MAME software lists forbid a clone of a
/// clone outright, and machine clone chains run to a handful of levels. A
/// bound well above any legitimate depth turns an adversarial or corrupt
/// catalogue into a named refusal instead of unbounded work, and - together
/// with the visited-set guard - keeps every traversal iterative and finite
/// rather than recursing on untrusted input.
pub(crate) const MAX_DEPENDENCY_DEPTH: usize = 64;

/// The result of turning a declared set name into a catalogue reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetRef {
    Unique(usize),
    /// Two or more entries share this name. Unresolvable without positional
    /// identity, which this stage deliberately does not use for dependencies.
    Duplicate,
    Absent,
}

/// A declared name attribute, distinguishing "not stated" from "stated badly".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclaredName<'a> {
    Absent,
    /// Present but empty or whitespace-only: a relationship was declared and
    /// then named nothing.
    Malformed,
    Named(&'a str),
}

/// Classifies an optional declared name attribute.
pub(crate) fn declared_name(value: &Option<String>) -> DeclaredName<'_> {
    match value.as_deref().map(str::trim) {
        None => DeclaredName::Absent,
        Some("") => DeclaredName::Malformed,
        Some(name) => DeclaredName::Named(name),
    }
}

/// A `yes`/`no` style flag, distinguishing every way it can fail to
/// affirmatively confirm something.
///
/// `no`, an absent attribute, and an unrecognised value are three different
/// facts, not one collapsed `false` - a caller validating a claimed identity
/// (is this really a device? is this really not runnable?) needs to fail
/// closed differently for "the catalogue says no" (a contradiction) than for
/// "the catalogue says nothing" (unproven) than for "the catalogue says
/// something we don't understand" (malformed). Collapsing all three to one
/// boolean is exactly how an unconfirmed claim gets silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flag {
    Yes,
    No,
    Absent,
    /// Present but neither a recognised affirmative nor negative.
    Malformed,
}

/// Classifies a raw `yes`/`no` style attribute value.
pub(crate) fn parse_flag(value: &Option<String>) -> Flag {
    match value.as_deref().map(str::trim) {
        None => Flag::Absent,
        Some(v) if v.eq_ignore_ascii_case("yes") => Flag::Yes,
        Some(v) if v.eq_ignore_ascii_case("no") => Flag::No,
        Some(_) => Flag::Malformed,
    }
}

/// How a member-name lookup inside one provider set resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberRef<K> {
    Unique(K),
    /// The provider set declares the name more than once. Fails closed: the
    /// borrow could mean either declaration and they may differ in content.
    Duplicate,
    /// The provider set declares no member with this name.
    Absent,
}

/// Name-indexed view over one parsed catalogue.
pub(crate) struct DependencyGraph<'a> {
    games: &'a [DatGameEntry],
    by_name: HashMap<&'a str, NameSlot>,
}

#[derive(Clone, Copy)]
enum NameSlot {
    Unique(usize),
    Duplicate,
}

impl<'a> DependencyGraph<'a> {
    /// Indexes every entry by its declared name, recording collisions rather
    /// than letting the last writer win.
    pub(crate) fn build(games: &'a [DatGameEntry]) -> Self {
        let mut by_name: HashMap<&'a str, NameSlot> = HashMap::with_capacity(games.len());
        for (index, game) in games.iter().enumerate() {
            let name = game.name.trim();
            if name.is_empty() {
                // An unnamed entry cannot be the target of any declared
                // reference, so it is simply not addressable. It is still a
                // set in its own right and is resolved normally as a subject.
                continue;
            }
            by_name
                .entry(name)
                .and_modify(|slot| *slot = NameSlot::Duplicate)
                .or_insert(NameSlot::Unique(index));
        }
        Self { games, by_name }
    }

    pub(crate) fn game(&self, index: usize) -> &'a DatGameEntry {
        &self.games[index]
    }

    /// Resolves a declared set name to at most one catalogue entry.
    pub(crate) fn resolve_set(&self, name: &str) -> SetRef {
        match self.by_name.get(name.trim()) {
            Some(NameSlot::Unique(index)) => SetRef::Unique(*index),
            Some(NameSlot::Duplicate) => SetRef::Duplicate,
            None => SetRef::Absent,
        }
    }

    /// Finds the uniquely-named ROM declaration `name` inside set `index`.
    ///
    /// Searches the set's own declarations only - top-level `<rom>`s and
    /// software-list `<dataarea>` members - never the wider catalogue.
    pub(crate) fn rom_declared_as(
        &self,
        index: usize,
        name: &str,
    ) -> MemberRef<(DatMemberKey, &'a DatRomEntry)> {
        let wanted = name.trim();
        let mut found: Option<(DatMemberKey, &'a DatRomEntry)> = None;
        for (key, rom) in declared_roms(index, self.game(index)) {
            if rom.name.trim() != wanted {
                continue;
            }
            if found.is_some() {
                return MemberRef::Duplicate;
            }
            found = Some((key, rom));
        }
        found.map_or(MemberRef::Absent, MemberRef::Unique)
    }

    /// Finds the uniquely-named disk declaration `name` inside set `index`.
    pub(crate) fn disk_declared_as(
        &self,
        index: usize,
        name: &str,
    ) -> MemberRef<(DatDiskKey, &'a DatDiskEntry)> {
        let wanted = name.trim();
        let mut found: Option<(DatDiskKey, &'a DatDiskEntry)> = None;
        for (key, disk) in declared_disks(index, self.game(index)) {
            let Some(declared) = disk.name.as_deref().map(str::trim) else {
                continue;
            };
            if declared != wanted {
                continue;
            }
            if found.is_some() {
                return MemberRef::Duplicate;
            }
            found = Some((key, disk));
        }
        found.map_or(MemberRef::Absent, MemberRef::Unique)
    }
}

/// Why a chain walk refused to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainFault {
    /// The chain revisited a set already on the current path.
    Cycle,
    /// The chain exceeded [`MAX_DEPENDENCY_DEPTH`] without terminating.
    DepthExceeded,
}

/// Visited-set and depth guard for one chain walk.
///
/// Every chain this stage follows - clone/ROM-source parents, `merge=`
/// inheritance, `device_ref` expansion, CHD parent links - is walked
/// iteratively through one of these. A revisit is a named fault, never an
/// arbitrary break and never a silent stop, because "stop quietly at the
/// cycle" and "this dependency is satisfied" are indistinguishable to a
/// caller that only sees the final state.
///
/// # A guard tracks one path, not one traversal
///
/// It is [`Clone`] because that distinction is load-bearing. Where a walk
/// fans out - a set borrowing two members from the same provider, a device
/// referencing two sub-devices that share a third - each branch must carry
/// its *own* path. Sharing one visited set across siblings would report an
/// ordinary diamond as a cycle, turning a perfectly resolvable catalogue into
/// a review. Callers clone at every fan-out and pass the clone down.
#[derive(Debug, Default, Clone)]
pub(crate) struct ChainGuard {
    seen: Vec<usize>,
}

impl ChainGuard {
    pub(crate) fn starting_at(index: usize) -> Self {
        Self { seen: vec![index] }
    }

    /// Records a step onto `index`, refusing a revisit or an over-deep chain.
    pub(crate) fn visit(&mut self, index: usize) -> Result<(), ChainFault> {
        if self.seen.contains(&index) {
            return Err(ChainFault::Cycle);
        }
        if self.seen.len() >= MAX_DEPENDENCY_DEPTH {
            return Err(ChainFault::DepthExceeded);
        }
        self.seen.push(index);
        Ok(())
    }
}

/// Visited-set and depth guard for a chain keyed by content identity rather
/// than by catalogue position - used for CHD parent links, which are declared
/// by SHA-1 and may not correspond to any catalogue entry at all.
#[derive(Debug, Default)]
pub(crate) struct IdentityChainGuard {
    seen: Vec<String>,
}

impl IdentityChainGuard {
    pub(crate) fn starting_at(identity: &str) -> Self {
        Self {
            seen: vec![identity.to_string()],
        }
    }

    pub(crate) fn visit(&mut self, identity: &str) -> Result<(), ChainFault> {
        if self.seen.iter().any(|entry| entry == identity) {
            return Err(ChainFault::Cycle);
        }
        if self.seen.len() >= MAX_DEPENDENCY_DEPTH {
            return Err(ChainFault::DepthExceeded);
        }
        self.seen.push(identity.to_string());
        Ok(())
    }
}
