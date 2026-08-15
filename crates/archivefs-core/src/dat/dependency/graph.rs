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
//! - Some catalogues (No-Intro) reference a parent by a second identity
//!   instead of a name: `cloneofid="0272"` names another entry's `<game
//!   id="0272">`, not a `<game name="0272">`. [`DependencyGraph::resolve_set`]
//!   is the single place both identities are tried, so every existing caller
//!   (clone/ROM-source chains, `merge=` providers, device/BIOS-root lookups)
//!   inherits ID resolution automatically rather than needing its own
//!   special case. A name match always wins over an ID match when both exist
//!   and agree; when they exist and *disagree*, or when an ID string is
//!   itself duplicated, that is exactly as unresolvable as a duplicated name
//!   and reuses [`SetRef::Duplicate`] rather than inventing a second kind of
//!   ambiguity a caller would have to handle separately.
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

/// The result of turning a declared set reference (a name, or an ID such as
/// `cloneofid`) into a catalogue entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetRef {
    Unique(usize),
    /// Unresolvable without positional identity, which this stage
    /// deliberately does not use for dependencies. Covers every kind of
    /// collision [`DependencyGraph::resolve_set`] can find: two or more
    /// entries sharing the reference as a name, two or more sharing it as an
    /// `id`, or a name match and an `id` match that disagree about which
    /// entry the reference means.
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

/// Name- and ID-indexed view over one parsed catalogue.
pub(crate) struct DependencyGraph<'a> {
    games: &'a [DatGameEntry],
    by_name: HashMap<&'a str, EntrySlot>,
    by_id: HashMap<&'a str, EntrySlot>,
}

#[derive(Clone, Copy)]
enum EntrySlot {
    Unique(usize),
    Duplicate,
}

impl<'a> DependencyGraph<'a> {
    /// Indexes every entry by its declared name and, separately, by its
    /// declared `id`, recording collisions in either index rather than
    /// letting the last writer win.
    pub(crate) fn build(games: &'a [DatGameEntry]) -> Self {
        let mut by_name: HashMap<&'a str, EntrySlot> = HashMap::with_capacity(games.len());
        let mut by_id: HashMap<&'a str, EntrySlot> = HashMap::new();
        for (index, game) in games.iter().enumerate() {
            let name = game.name.trim();
            if !name.is_empty() {
                by_name
                    .entry(name)
                    .and_modify(|slot| *slot = EntrySlot::Duplicate)
                    .or_insert(EntrySlot::Unique(index));
            }
            // An unnamed or un-identified entry cannot be the target of any
            // declared reference, so it is simply not addressable that way.
            // It is still a set in its own right and is resolved normally as
            // a subject.
            if let Some(id) = game
                .id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                by_id
                    .entry(id)
                    .and_modify(|slot| *slot = EntrySlot::Duplicate)
                    .or_insert(EntrySlot::Unique(index));
            }
        }
        Self {
            games,
            by_name,
            by_id,
        }
    }

    pub(crate) fn game(&self, index: usize) -> &'a DatGameEntry {
        &self.games[index]
    }

    /// Resolves a declared set reference - a name, or (when no name matches)
    /// an `id` such as a No-Intro `cloneofid` target - to at most one
    /// catalogue entry.
    ///
    /// A name match always takes precedence over an ID match: an ID index is
    /// a fallback for references a name lookup cannot explain, never a
    /// competing authority that could override a perfectly good name
    /// resolution. The one exception is disagreement, not precedence - see
    /// the cases below.
    pub(crate) fn resolve_set(&self, reference: &str) -> SetRef {
        let key = reference.trim();
        match (self.by_name.get(key).copied(), self.by_id.get(key).copied()) {
            // A duplicated name is unresolvable on its own; an ID match
            // (agreeing, disagreeing, or itself ambiguous) cannot rescue it.
            (Some(EntrySlot::Duplicate), _) => SetRef::Duplicate,
            (Some(EntrySlot::Unique(by_name)), Some(EntrySlot::Unique(by_id))) => {
                if by_name == by_id {
                    // The same string happens to be both this entry's name
                    // and its id (or another entry's id happens to equal it
                    // while still pointing at the same entry) - harmless
                    // agreement, not a coincidence worth refusing.
                    SetRef::Unique(by_name)
                } else {
                    // The reference names one entry and identifies a
                    // *different* one. Silently preferring either would be
                    // exactly the guess this stage forbids.
                    SetRef::Duplicate
                }
            }
            // A valid unique name is never overridden by an ID collision
            // elsewhere in the catalogue.
            (Some(EntrySlot::Unique(by_name)), None | Some(EntrySlot::Duplicate)) => {
                SetRef::Unique(by_name)
            }
            (None, Some(EntrySlot::Unique(by_id))) => SetRef::Unique(by_id),
            (None, Some(EntrySlot::Duplicate)) => SetRef::Duplicate,
            (None, None) => SetRef::Absent,
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
