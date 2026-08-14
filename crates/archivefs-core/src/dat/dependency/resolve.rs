//! Collection-scoped dependency resolution (Stage 2d).
//!
//! # Why this runs after every archive, not inside one
//!
//! Stage 2c is per-archive by design (R7): a set split across two archives is
//! judged independently in each. Dependencies do not work that way. A clone's
//! borrowed ROM lives in the *parent's* archive, a device's ROMs live in the
//! device's own archive, and a delta CHD's parent may be any `.chd` anywhere
//! under the scan root. Resolving those while archives are still being walked
//! would mean answering "is the parent present?" before the parent had been
//! looked at, and the only honest answer at that point is "unknown" - which
//! would make the whole stage useless.
//!
//! So resolution runs once, over [`CollectionEvidence`] aggregated from the
//! entire run, and rewrites each [`SetResolution`]'s state through the
//! downgrade-only [`super::apply_dependency_state`].
//!
//! # Partial scans can only weaken a negative, never a positive
//!
//! When the run did not finish (cancelled, truncated, a partial archive pass,
//! an incomplete disk scan), a "not found anywhere" conclusion is not
//! trustworthy - the file may sit in the part that was never looked at. Every
//! such conclusion becomes [`DependencyOutcome::EvidenceUnavailable`] instead
//! of [`DependencyOutcome::Missing`]. A *positive* verification is untouched:
//! finding something is still proof under a partial scan. Both block
//! `Complete`, so this asymmetry can never produce a false `Complete`, and it
//! avoids asserting absences the scan never established.
//!
//! # What is deliberately not implemented
//!
//! MAME's `-verifyroms` "set not present" heuristic (audit.cpp) refuses to
//! call a clone correct when its only found files are borrowed *and* the
//! providing parent archive was not itself found. Reproducing it requires
//! deciding that an archive "is" the parent set, and the only available
//! signal for that is the archive's filename. This stage resolves nothing
//! from filenames, so instead of a filename-derived provider check it uses
//! the stronger, content-based question: was the provider's *own declared
//! member* verified, positionally, anywhere in the collection? A borrowed
//! member is satisfied only by that, never by a same-named file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::graph::{
    ChainFault, ChainGuard, DeclaredName, DependencyGraph, IdentityChainGuard, MemberRef, SetRef,
    declared_name, flag_is_yes,
};
use super::{
    DependencyKind, DependencyOutcome, DependencyRequirement, DependencyTarget,
    SetDependencyReport, apply_dependency_state,
};
use crate::dat::disk_audit::DatDiskAudit;
use crate::dat::index::{DatDiskKey, DatMemberKey, parse_disk_sha1};
use crate::dat::model::{DatDiskEntry, DatGameEntry, DatRomEntry};
use crate::dat::set::{
    MemberClass, SetResolution, attribute_archive_members, classify_disk_member,
    classify_rom_member, declared_disks, declared_roms, summarize_disk_evidence,
};
use crate::dat::sources::audit_run::DatArchiveAudit;

/// What one CHD file says about its own parent link.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ParentFact {
    /// The header declares no parent.
    None,
    /// The header declares a parent but the identity was unusable.
    Unusable,
    /// The header declares this parent identity.
    Identity(String),
}

/// Everything one completed audit run proved, aggregated across archives.
///
/// Deliberately built from the *same* attribution functions Stage 2c uses
/// ([`attribute_archive_members`], [`summarize_disk_evidence`]) rather than
/// re-reading raw verdicts, so a dependency can never be satisfied by
/// evidence storage classification rejected.
pub struct CollectionEvidence {
    /// Every member slot proven by positional, cryptographic attribution.
    verified_members: HashSet<DatMemberKey>,
    /// Per-set evidence integrity, keyed by DAT `<game name>`.
    set_flags: BTreeMap<String, SetEvidenceFlags>,
    /// Every disk slot proven by CHD header identity.
    verified_disks: HashSet<DatDiskKey>,
    /// The header identity that proved each verified disk slot.
    verified_disk_identity: HashMap<DatDiskKey, String>,
    /// The parent identity declared by each verified disk slot's CHD.
    verified_disk_parent: HashMap<DatDiskKey, Option<String>>,
    /// Sets whose disk evidence was ambiguous or duplicated.
    disk_tainted_sets: HashSet<String>,
    /// Every readable CHD in the run, by header identity, with the set of
    /// distinct parent claims made about that identity. More than one claim
    /// is contradictory evidence about one image and fails closed.
    chd_identities: BTreeMap<String, BTreeSet<ParentFact>>,
    /// Whether the run examined everything it was pointed at.
    scan_complete: bool,
}

#[derive(Debug, Default, Clone, Copy)]
struct SetEvidenceFlags {
    /// Some archive saw a member it could not attribute to this set alone.
    ambiguous: bool,
    /// Some archive's evidence for this set was internally duplicated.
    duplicate_evidence: bool,
    /// Some member of this set was verified only by name, with no positional
    /// identity available. Such a set can still be storage-complete, but it
    /// cannot *prove* a specific declaration for a dependency to borrow.
    used_legacy_evidence: bool,
}

impl CollectionEvidence {
    /// Aggregates one run's archive and disk evidence.
    ///
    /// `scan_complete` must be false whenever anything about the run was
    /// partial - a truncated walk, a cancelled pass, or an incomplete disk
    /// scan - because it is the switch that stops this stage asserting an
    /// absence it could not have observed.
    pub fn build(
        archives: &[DatArchiveAudit],
        disk_evidence: &[DatDiskAudit],
        games: &[DatGameEntry],
        scan_complete: bool,
    ) -> Self {
        let mut verified_members: HashSet<DatMemberKey> = HashSet::new();
        let mut set_flags: BTreeMap<String, SetEvidenceFlags> = BTreeMap::new();

        for archive in archives {
            for (game_name, touch) in attribute_archive_members(archive, games) {
                verified_members.extend(touch.verified_member_keys.iter().copied());
                let flags = set_flags.entry(game_name).or_default();
                flags.ambiguous |= touch.ambiguous;
                flags.duplicate_evidence |= touch.duplicate_evidence;
                flags.used_legacy_evidence |= touch.used_legacy_evidence;
            }
        }

        let disks = summarize_disk_evidence(disk_evidence, games);
        let verified_disks: HashSet<DatDiskKey> =
            disks.verified.values().flatten().copied().collect();
        let mut disk_tainted_sets: HashSet<String> = HashSet::new();
        disk_tainted_sets.extend(disks.ambiguous_games.iter().cloned());
        disk_tainted_sets.extend(disks.duplicate_evidence_games.iter().cloned());

        // Content presence for a CHD parent is a property of the *file*, not
        // of any catalogue entry: a legitimate parent image may be absent
        // from the DAT entirely. So this map is built from every readable
        // header, independent of verdict - but only from identities that
        // survive the shared validator, so an unset all-zero field can never
        // become a lookup key that matches another unset field.
        let mut chd_identities: BTreeMap<String, BTreeSet<ParentFact>> = BTreeMap::new();
        for audit in disk_evidence {
            let Some(identity) = audit.overall_sha1.as_deref().and_then(parse_disk_sha1) else {
                continue;
            };
            let fact = if !audit.parent_required {
                ParentFact::None
            } else {
                match audit.parent_sha1.as_deref() {
                    Some(parent) => ParentFact::Identity(parent.to_string()),
                    None => ParentFact::Unusable,
                }
            };
            chd_identities.entry(identity).or_default().insert(fact);
        }

        Self {
            verified_members,
            set_flags,
            verified_disks,
            verified_disk_identity: disks.verified_identity,
            verified_disk_parent: disks.verified_parent_identity,
            disk_tainted_sets,
            chd_identities,
            scan_complete,
        }
    }

    fn flags(&self, set_name: &str) -> SetEvidenceFlags {
        self.set_flags.get(set_name).copied().unwrap_or_default()
    }
}

/// Resolves every dependency in `resolutions` and folds the result into each
/// set's state.
///
/// The fold is [`apply_dependency_state`], which is downgrade-only, so this
/// function cannot promote any set. Resolutions whose catalogue entry cannot
/// be identified (a duplicated `<game name>`) keep
/// [`DependencyState::NotEvaluated`] and are left exactly as Stage 2c
/// produced them.
pub(crate) fn resolve_collection(
    resolutions: &mut [SetResolution],
    games: &[DatGameEntry],
    evidence: &CollectionEvidence,
) {
    let graph = DependencyGraph::build(games);
    let resolver = Resolver {
        graph: &graph,
        evidence,
    };

    // One report per distinct set name, reused across the (possibly several)
    // per-archive resolutions naming that set. Dependencies are a property of
    // the catalogue and the collection, not of which archive happened to
    // surface the set, so resolving once per name is both cheaper and
    // guarantees two archives cannot disagree about the same set's
    // dependencies.
    let mut cache: BTreeMap<String, SetDependencyReport> = BTreeMap::new();

    for resolution in resolutions.iter_mut() {
        let name = resolution.identity.game_name.clone();
        let report = match cache.get(&name) {
            Some(cached) => cached.clone(),
            None => {
                let built = match graph.resolve_set(&name) {
                    SetRef::Unique(index) => {
                        SetDependencyReport::from_requirements(resolver.requirements_for(index))
                    }
                    // Stage 2c already refused a duplicated or unknown name;
                    // there is no single entry whose dependencies could be
                    // resolved, and choosing one would be exactly the
                    // positional guess this stage forbids.
                    SetRef::Duplicate | SetRef::Absent => SetDependencyReport::not_evaluated(),
                };
                cache.insert(name.clone(), built.clone());
                built
            }
        };
        resolution.state = apply_dependency_state(resolution.state.clone(), report.state);
        resolution.dependencies = report;
    }
}

struct Resolver<'a> {
    graph: &'a DependencyGraph<'a>,
    evidence: &'a CollectionEvidence,
}

impl<'a> Resolver<'a> {
    /// The verdict for "this was not found anywhere in the collection".
    ///
    /// Downgraded to `EvidenceUnavailable` under a partial scan: absence was
    /// never established, only unobserved.
    fn absent(&self) -> DependencyOutcome {
        if self.evidence.scan_complete {
            DependencyOutcome::Missing
        } else {
            DependencyOutcome::EvidenceUnavailable
        }
    }

    /// Every dependency requirement one set declares, in DAT declaration
    /// order so the report is deterministic regardless of scan order.
    fn requirements_for(&self, index: usize) -> Vec<DependencyRequirement> {
        let game = self.graph.game(index);
        let mut requirements = Vec::new();

        // 1-2. `cloneof` and `romof` are resolved independently and neither
        // is ever synthesised from the other. A set may declare either, both
        // at the same target, both at different targets, or neither.
        self.push_set_reference(
            &mut requirements,
            index,
            DependencyKind::ParentSet,
            &game.clone_of,
            |entry| &entry.clone_of,
        );
        self.push_set_reference(
            &mut requirements,
            index,
            DependencyKind::RomSource,
            &game.rom_of,
            |entry| &entry.rom_of,
        );

        // 3-4. Member-level borrowing.
        for (_, rom) in declared_roms(index, game) {
            if classify_rom_member(rom) == MemberClass::Borrowed {
                requirements.push(self.resolve_merged_rom(index, rom));
            }
        }
        for (_, disk) in declared_disks(index, game) {
            if classify_disk_member(disk) == MemberClass::Borrowed {
                requirements.push(self.resolve_merged_disk(index, disk));
            }
        }

        // 5. BIOS.
        requirements.extend(self.resolve_bios(index));

        // 6. Devices.
        requirements.extend(self.resolve_devices(index));

        // 7. Samples.
        requirements.extend(self.resolve_samples(index));

        // 8. CHD parent links.
        requirements.extend(self.resolve_chd_parents(index));

        requirements
    }

    /// Resolves a whole-set reference (`cloneof` or `romof`), including a
    /// cycle walk over that same attribute's chain.
    ///
    /// The `chain` accessor is what keeps the two kinds independent: a
    /// `cloneof` cycle is walked purely through `cloneof` links and a `romof`
    /// cycle purely through `romof` links, so a loop in one never reports as
    /// a loop in the other.
    fn push_set_reference(
        &self,
        out: &mut Vec<DependencyRequirement>,
        index: usize,
        kind: DependencyKind,
        declared: &Option<String>,
        chain: fn(&DatGameEntry) -> &Option<String>,
    ) {
        let subject = self.graph.game(index);
        let (target, outcome) = match declared_name(declared) {
            DeclaredName::Absent => return,
            DeclaredName::Malformed => (
                DependencyTarget::Undeclared,
                DependencyOutcome::Contradictory,
            ),
            DeclaredName::Named(name) if name == subject.name.trim() => (
                DependencyTarget::Set {
                    name: name.to_string(),
                },
                DependencyOutcome::Contradictory,
            ),
            DeclaredName::Named(name) => {
                let target = DependencyTarget::Set {
                    name: name.to_string(),
                };
                let outcome = match self.graph.resolve_set(name) {
                    SetRef::Duplicate => DependencyOutcome::Ambiguous,
                    // A catalogue that names a parent it does not contain is
                    // internally inconsistent. Reported, never read as "so
                    // there is nothing to depend on".
                    SetRef::Absent => DependencyOutcome::Contradictory,
                    SetRef::Unique(parent) => self.walk_set_chain(index, parent, chain),
                };
                (target, outcome)
            }
        };
        out.push(DependencyRequirement {
            kind,
            target,
            outcome,
            via_member: None,
        });
    }

    /// Walks one attribute's parent chain from `first` looking for a loop.
    fn walk_set_chain(
        &self,
        subject: usize,
        first: usize,
        chain: fn(&DatGameEntry) -> &Option<String>,
    ) -> DependencyOutcome {
        let mut guard = ChainGuard::starting_at(subject);
        let mut current = first;
        loop {
            match guard.visit(current) {
                Ok(()) => {}
                Err(ChainFault::Cycle) => return DependencyOutcome::Cycle,
                Err(ChainFault::DepthExceeded) => return DependencyOutcome::Unsupported,
            }
            match declared_name(chain(self.graph.game(current))) {
                DeclaredName::Absent => return DependencyOutcome::Satisfied,
                DeclaredName::Malformed => return DependencyOutcome::Contradictory,
                DeclaredName::Named(next) => match self.graph.resolve_set(next) {
                    SetRef::Unique(index) => current = index,
                    SetRef::Duplicate => return DependencyOutcome::Ambiguous,
                    SetRef::Absent => return DependencyOutcome::Contradictory,
                },
            }
        }
    }

    /// The set a member-level borrow reads from.
    ///
    /// `romof` is the ROM-source declaration and wins when present; `cloneof`
    /// is the fallback for catalogues that publish only a clone hierarchy
    /// (ClrMamePro-derived and many converted DATs). Neither is invented from
    /// the other: when both are absent, a `merge=` has no provider at all and
    /// the caller reports that rather than picking a plausible set.
    fn borrow_provider(&self, index: usize) -> Option<&Option<String>> {
        let game = self.graph.game(index);
        match declared_name(&game.rom_of) {
            DeclaredName::Absent => match declared_name(&game.clone_of) {
                DeclaredName::Absent => None,
                _ => Some(&game.clone_of),
            },
            _ => Some(&game.rom_of),
        }
    }

    fn resolve_merged_rom(&self, index: usize, rom: &'a DatRomEntry) -> DependencyRequirement {
        let merge_name = match declared_name(&rom.merge) {
            DeclaredName::Named(name) => name,
            // `classify_rom_member` only returns `Borrowed` for a non-empty
            // merge, so these are unreachable; refusing rather than
            // unwrapping keeps that coupling from becoming a panic if the
            // classifier ever changes.
            DeclaredName::Absent | DeclaredName::Malformed => {
                return DependencyRequirement {
                    kind: DependencyKind::MergedRom,
                    target: DependencyTarget::Undeclared,
                    outcome: DependencyOutcome::Contradictory,
                    via_member: Some(rom.name.clone()),
                };
            }
        };
        let (target, outcome) = self.resolve_borrow(index, merge_name, BorrowedMember::Rom(rom));
        DependencyRequirement {
            kind: DependencyKind::MergedRom,
            target,
            outcome,
            via_member: Some(rom.name.clone()),
        }
    }

    fn resolve_merged_disk(&self, index: usize, disk: &'a DatDiskEntry) -> DependencyRequirement {
        let via_member = disk.name.clone();
        let merge_name = match declared_name(&disk.merge) {
            DeclaredName::Named(name) => name,
            DeclaredName::Absent | DeclaredName::Malformed => {
                return DependencyRequirement {
                    kind: DependencyKind::MergedDisk,
                    target: DependencyTarget::Undeclared,
                    outcome: DependencyOutcome::Contradictory,
                    via_member,
                };
            }
        };
        let (target, outcome) = self.resolve_borrow(index, merge_name, BorrowedMember::Disk(disk));
        DependencyRequirement {
            kind: DependencyKind::MergedDisk,
            target,
            outcome,
            via_member,
        }
    }

    /// Resolves one `merge=` to a declaration in the provider set and decides
    /// whether that declaration's content was proven present.
    ///
    /// The lookup is scoped to the resolved provider set and matches the
    /// declared member name exactly. A same-named member in any other set -
    /// or in no set at all - cannot satisfy it, which is the whole point:
    /// filename collisions across a catalogue are common and are never
    /// evidence of anything.
    fn resolve_borrow(
        &self,
        index: usize,
        merge_name: &str,
        member: BorrowedMember<'a>,
    ) -> (DependencyTarget, DependencyOutcome) {
        let Some(provider_decl) = self.borrow_provider(index) else {
            // A member declares itself borrowed while its set declares no set
            // to borrow from. Nothing to resolve against; guessing a provider
            // is exactly how an unrelated same-named file would get in.
            return (
                DependencyTarget::Undeclared,
                DependencyOutcome::Contradictory,
            );
        };
        let provider_name = match declared_name(provider_decl) {
            DeclaredName::Named(name) => name,
            DeclaredName::Absent | DeclaredName::Malformed => {
                return (
                    DependencyTarget::Undeclared,
                    DependencyOutcome::Contradictory,
                );
            }
        };
        let target = DependencyTarget::SetMember {
            set_name: provider_name.to_string(),
            member_name: merge_name.to_string(),
        };
        if provider_name == self.graph.game(index).name.trim() {
            return (target, DependencyOutcome::Contradictory);
        }
        let provider = match self.graph.resolve_set(provider_name) {
            SetRef::Unique(provider) => provider,
            SetRef::Duplicate => return (target, DependencyOutcome::Ambiguous),
            SetRef::Absent => return (target, DependencyOutcome::Contradictory),
        };

        let mut guard = ChainGuard::starting_at(index);
        let outcome = match member {
            BorrowedMember::Rom(rom) => {
                self.follow_rom_merge(provider, merge_name, Some(rom), &mut guard)
            }
            BorrowedMember::Disk(disk) => {
                self.follow_disk_merge(provider, merge_name, Some(disk), &mut guard)
            }
        };
        (target, outcome)
    }

    /// Follows a ROM `merge=` chain to the declaration that actually owns the
    /// content, then asks whether that declaration was verified.
    ///
    /// A merged member may itself be merged from a further ancestor; the walk
    /// continues until it reaches a declaration that owns its content, guarded
    /// against revisits the whole way.
    fn follow_rom_merge(
        &self,
        mut provider: usize,
        wanted: &str,
        mut borrower: Option<&'a DatRomEntry>,
        guard: &mut ChainGuard,
    ) -> DependencyOutcome {
        let mut wanted = wanted.to_string();
        loop {
            match guard.visit(provider) {
                Ok(()) => {}
                Err(ChainFault::Cycle) => return DependencyOutcome::Cycle,
                Err(ChainFault::DepthExceeded) => return DependencyOutcome::Unsupported,
            }
            let provider_game = self.graph.game(provider);
            if provider_game.unsupported_structure {
                return DependencyOutcome::Unsupported;
            }
            let (key, declaration) = match self.graph.rom_declared_as(provider, &wanted) {
                MemberRef::Unique(found) => found,
                MemberRef::Duplicate => return DependencyOutcome::Ambiguous,
                // The provider set declares no such member. The `merge=`
                // points at nothing, which is a catalogue defect - not an
                // invitation to look for the name somewhere else.
                MemberRef::Absent => return DependencyOutcome::Contradictory,
            };
            // A borrow whose declared content disagrees with the content it
            // claims to borrow is contradictory metadata. Without this, a
            // catalogue could route a member at a same-named declaration
            // holding entirely different bytes and have it count.
            if let Some(source) = borrower
                && checksums_conflict(source, declaration)
            {
                return DependencyOutcome::Contradictory;
            }

            match classify_rom_member(declaration) {
                MemberClass::Borrowed => {
                    let next_name = match declared_name(&declaration.merge) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            return DependencyOutcome::Contradictory;
                        }
                    };
                    let Some(next_provider) = self.borrow_provider(provider) else {
                        return DependencyOutcome::Contradictory;
                    };
                    let next_provider = match declared_name(next_provider) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            return DependencyOutcome::Contradictory;
                        }
                    };
                    provider = match self.graph.resolve_set(next_provider) {
                        SetRef::Unique(index) => index,
                        SetRef::Duplicate => return DependencyOutcome::Ambiguous,
                        SetRef::Absent => return DependencyOutcome::Contradictory,
                    };
                    wanted = next_name.to_string();
                    borrower = Some(declaration);
                }
                MemberClass::PhysicalRequired | MemberClass::OptionalPhysical => {
                    return self.member_outcome(provider_game, key);
                }
                // A borrow that lands on a member the catalogue itself says
                // is unverifiable, contradictory, or not a file at all cannot
                // be proven either way.
                MemberClass::NonFile
                | MemberClass::UnverifiableNodump
                | MemberClass::KnownBad
                | MemberClass::Contradictory
                | MemberClass::UnknownLoadflag => return DependencyOutcome::Unsupported,
            }
        }
    }

    fn follow_disk_merge(
        &self,
        mut provider: usize,
        wanted: &str,
        mut borrower: Option<&'a DatDiskEntry>,
        guard: &mut ChainGuard,
    ) -> DependencyOutcome {
        let mut wanted = wanted.to_string();
        loop {
            match guard.visit(provider) {
                Ok(()) => {}
                Err(ChainFault::Cycle) => return DependencyOutcome::Cycle,
                Err(ChainFault::DepthExceeded) => return DependencyOutcome::Unsupported,
            }
            let provider_game = self.graph.game(provider);
            if provider_game.unsupported_structure {
                return DependencyOutcome::Unsupported;
            }
            let (key, declaration) = match self.graph.disk_declared_as(provider, &wanted) {
                MemberRef::Unique(found) => found,
                MemberRef::Duplicate => return DependencyOutcome::Ambiguous,
                MemberRef::Absent => return DependencyOutcome::Contradictory,
            };
            if let Some(source) = borrower
                && disk_sha1_conflict(source, declaration)
            {
                return DependencyOutcome::Contradictory;
            }

            match classify_disk_member(declaration) {
                MemberClass::Borrowed => {
                    let next_name = match declared_name(&declaration.merge) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            return DependencyOutcome::Contradictory;
                        }
                    };
                    let Some(next_provider) = self.borrow_provider(provider) else {
                        return DependencyOutcome::Contradictory;
                    };
                    let next_provider = match declared_name(next_provider) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            return DependencyOutcome::Contradictory;
                        }
                    };
                    provider = match self.graph.resolve_set(next_provider) {
                        SetRef::Unique(index) => index,
                        SetRef::Duplicate => return DependencyOutcome::Ambiguous,
                        SetRef::Absent => return DependencyOutcome::Contradictory,
                    };
                    wanted = next_name.to_string();
                    borrower = Some(declaration);
                }
                MemberClass::PhysicalRequired | MemberClass::OptionalPhysical => {
                    return self.disk_outcome(provider_game, key);
                }
                MemberClass::NonFile
                | MemberClass::UnverifiableNodump
                | MemberClass::KnownBad
                | MemberClass::Contradictory
                | MemberClass::UnknownLoadflag => return DependencyOutcome::Unsupported,
            }
        }
    }

    /// Whether one ROM declaration's content was proven present.
    fn member_outcome(&self, owner: &DatGameEntry, key: DatMemberKey) -> DependencyOutcome {
        let flags = self.evidence.flags(owner.name.trim());
        if flags.duplicate_evidence || flags.ambiguous {
            return DependencyOutcome::Ambiguous;
        }
        if self.evidence.verified_members.contains(&key) {
            return DependencyOutcome::Satisfied;
        }
        if flags.used_legacy_evidence {
            // Something in this set was verified, but only by name. A name is
            // not a declaration identity, so it cannot prove *this* slot -
            // and it equally cannot disprove it. Unresolvable, not absent.
            return DependencyOutcome::Unsupported;
        }
        self.absent()
    }

    /// Whether one disk declaration's content was proven present.
    /// Whether one disk declaration's content was proven present *and usable*.
    ///
    /// A delta image whose parent is absent is present but unusable, so the
    /// parent link is folded in here rather than only being reported against
    /// the set that declares the disk. Otherwise a set borrowing that image
    /// would be told its borrow is satisfied while the image cannot be used -
    /// the requirement would be reported against the provider and silently
    /// dropped for the borrower.
    fn disk_outcome(&self, owner: &DatGameEntry, key: DatDiskKey) -> DependencyOutcome {
        let name = owner.name.trim();
        if self.evidence.disk_tainted_sets.contains(name) {
            return DependencyOutcome::Ambiguous;
        }
        if !self.evidence.verified_disks.contains(&key) {
            return self.absent();
        }
        self.chd_parent_outcome(key)
            .unwrap_or(DependencyOutcome::Satisfied)
    }

    /// The parent-link verdict for one verified disk slot, or `None` when its
    /// image declares no parent.
    fn chd_parent_outcome(&self, key: DatDiskKey) -> Option<DependencyOutcome> {
        let parent = self.evidence.verified_disk_parent.get(&key)?;
        let own_identity = self.evidence.verified_disk_identity.get(&key);
        Some(match parent.as_deref() {
            // The header declared a parent and then gave no usable identity
            // for it. The dependency is real and unresolvable.
            None => DependencyOutcome::Contradictory,
            Some(parent_identity) => match own_identity {
                Some(own) if own == parent_identity => DependencyOutcome::Contradictory,
                Some(own) => self.follow_chd_parents(own, parent_identity),
                // Verified without a recorded identity should be unreachable;
                // refuse rather than resolve blind.
                None => DependencyOutcome::Unsupported,
            },
        })
    }

    /// BIOS relationships.
    ///
    /// Two separate things are checked, and neither is a runnability claim -
    /// see [`super::BIOS_RUNTIME_SELECTION_NOT_MODELLED`]. Which BIOS variant
    /// a user would select at run time is not modelled anywhere in the
    /// current architecture, so this stage resolves only "does the BIOS
    /// storage dependency exist and is it provided".
    fn resolve_bios(&self, index: usize) -> Vec<DependencyRequirement> {
        let game = self.graph.game(index);
        let mut out = Vec::new();

        // (a) Declared `<biosset>` names must be usable and unique. A
        // duplicated variant name makes every `bios=` reference to it
        // ambiguous.
        let mut declared: BTreeMap<&str, usize> = BTreeMap::new();
        let mut malformed_biosset = false;
        for bios_set in &game.bios_sets {
            match declared_name(&bios_set.name) {
                DeclaredName::Named(name) => *declared.entry(name).or_insert(0) += 1,
                DeclaredName::Absent | DeclaredName::Malformed => malformed_biosset = true,
            }
        }
        if malformed_biosset {
            out.push(DependencyRequirement {
                kind: DependencyKind::Bios,
                target: DependencyTarget::Undeclared,
                outcome: DependencyOutcome::Contradictory,
                via_member: None,
            });
        }

        // (b) Every `bios=`-tagged ROM must name a variant this set declares.
        // A tag naming nothing is an unreachable ROM: MAME would skip it for
        // every selection, and this stage must surface that rather than treat
        // the ROM as ordinary storage.
        let mut referenced: BTreeSet<&str> = BTreeSet::new();
        for (_, rom) in declared_roms(index, game) {
            if let DeclaredName::Named(variant) = declared_name(&rom.bios) {
                referenced.insert(variant);
            } else if rom.bios.is_some() {
                out.push(DependencyRequirement {
                    kind: DependencyKind::Bios,
                    target: DependencyTarget::Undeclared,
                    outcome: DependencyOutcome::Contradictory,
                    via_member: Some(rom.name.clone()),
                });
            }
        }
        for variant in referenced {
            let target = DependencyTarget::BiosSet {
                set_name: game.name.clone(),
                bios_set: variant.to_string(),
            };
            let outcome = match declared.get(variant) {
                Some(1) => DependencyOutcome::Satisfied,
                Some(_) => DependencyOutcome::Ambiguous,
                None => DependencyOutcome::Contradictory,
            };
            out.push(DependencyRequirement {
                kind: DependencyKind::Bios,
                target,
                outcome,
                via_member: None,
            });
        }

        // (c) A BIOS-root provider reached through the ROM-source chain must
        // itself be present, because a set may borrow its BIOS content
        // without redeclaring it. Sets that *do* redeclare it are already
        // covered, more precisely, by their `merge=` requirements.
        if let Some(provider_decl) = self.borrow_provider(index)
            && let DeclaredName::Named(provider_name) = declared_name(provider_decl)
            && let SetRef::Unique(provider) = self.graph.resolve_set(provider_name)
            && flag_is_yes(&self.graph.game(provider).is_bios)
        {
            let mut guard = ChainGuard::starting_at(index);
            out.push(DependencyRequirement {
                kind: DependencyKind::Bios,
                target: DependencyTarget::Set {
                    name: provider_name.to_string(),
                },
                outcome: self.set_storage_outcome(provider, &mut guard),
                via_member: None,
            });
        }

        out
    }

    /// `device_ref` requirements, resolved transitively with a cycle guard.
    fn resolve_devices(&self, index: usize) -> Vec<DependencyRequirement> {
        let game = self.graph.game(index);
        let mut out = Vec::new();
        for device_ref in &game.device_refs {
            let (target, outcome) = match declared_name(&device_ref.name) {
                DeclaredName::Absent | DeclaredName::Malformed => (
                    DependencyTarget::Undeclared,
                    DependencyOutcome::Contradictory,
                ),
                DeclaredName::Named(name) => {
                    let target = DependencyTarget::Set {
                        name: name.to_string(),
                    };
                    if name == game.name.trim() {
                        (target, DependencyOutcome::Contradictory)
                    } else {
                        let outcome = match self.graph.resolve_set(name) {
                            SetRef::Duplicate => DependencyOutcome::Ambiguous,
                            // A stripped "games only" catalogue keeps
                            // `device_ref`s while dropping the device nodes.
                            // Whether that device carries ROMs is then
                            // unknowable, and assuming it does not is a
                            // direct route to a false `Complete`.
                            SetRef::Absent => DependencyOutcome::Contradictory,
                            SetRef::Unique(device) => {
                                let mut guard = ChainGuard::starting_at(index);
                                self.device_outcome(device, &mut guard)
                            }
                        };
                        (target, outcome)
                    }
                }
            };
            out.push(DependencyRequirement {
                kind: DependencyKind::Device,
                target,
                outcome,
                via_member: None,
            });
        }
        out
    }

    /// One device node's own requirement: it must really be a device, and its
    /// storage (including its own devices and borrows) must be satisfied.
    fn device_outcome(&self, device: usize, guard: &mut ChainGuard) -> DependencyOutcome {
        let node = self.graph.game(device);
        // A `device_ref` that resolves to something the catalogue explicitly
        // says is not a device, or explicitly says is runnable, is pointing at
        // a game. Accepting that would let an unrelated game's own storage
        // satisfy a device requirement. When the catalogue simply omits the
        // flags - as many non-MAME DATs do - there is nothing to contradict,
        // and the reference is resolved on its declarations instead.
        let denies_device = node
            .is_device
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case("no"));
        let claims_runnable = flag_is_yes(&node.runnable);
        if denies_device || claims_runnable {
            return DependencyOutcome::Contradictory;
        }
        self.set_storage_outcome(device, guard)
    }

    /// Whether a whole set's declared storage was proven present.
    ///
    /// Used for dependency *targets* (a BIOS root, a device), never for the
    /// subject set - the subject's own storage is Stage 2c's verdict and is
    /// not recomputed here.
    fn set_storage_outcome(&self, index: usize, guard: &mut ChainGuard) -> DependencyOutcome {
        match guard.visit(index) {
            Ok(()) => {}
            Err(ChainFault::Cycle) => return DependencyOutcome::Cycle,
            Err(ChainFault::DepthExceeded) => return DependencyOutcome::Unsupported,
        }
        let game = self.graph.game(index);
        if game.unsupported_structure {
            return DependencyOutcome::Unsupported;
        }
        let name = game.name.trim();
        let flags = self.evidence.flags(name);
        if flags.duplicate_evidence
            || flags.ambiguous
            || self.evidence.disk_tainted_sets.contains(name)
        {
            return DependencyOutcome::Ambiguous;
        }

        let mut worst = DependencyOutcome::Satisfied;
        let mut fold = |outcome: DependencyOutcome| {
            if super::severity(outcome) > super::severity(worst) {
                worst = outcome;
            }
        };

        for (key, rom) in declared_roms(index, game) {
            match classify_rom_member(rom) {
                MemberClass::PhysicalRequired => {
                    fold(self.member_outcome(game, key));
                }
                MemberClass::Borrowed => {
                    let name = match declared_name(&rom.merge) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            fold(DependencyOutcome::Contradictory);
                            continue;
                        }
                    };
                    let Some(provider_decl) = self.borrow_provider(index) else {
                        fold(DependencyOutcome::Contradictory);
                        continue;
                    };
                    match declared_name(provider_decl) {
                        DeclaredName::Named(provider_name) => {
                            match self.graph.resolve_set(provider_name) {
                                SetRef::Unique(provider) => {
                                    // Own path per branch: two members
                                    // borrowed from one provider is a
                                    // diamond, not a cycle.
                                    // Own path per branch: two members
                                    // borrowed from one provider is a
                                    // diamond, not a cycle.
                                    fold(self.follow_rom_merge(
                                        provider,
                                        name,
                                        Some(rom),
                                        &mut guard.clone(),
                                    ));
                                }
                                SetRef::Duplicate => fold(DependencyOutcome::Ambiguous),
                                SetRef::Absent => fold(DependencyOutcome::Contradictory),
                            }
                        }
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            fold(DependencyOutcome::Contradictory);
                        }
                    }
                }
                // A target set the catalogue itself marks unverifiable or
                // contradictory cannot be proven satisfied.
                MemberClass::UnverifiableNodump
                | MemberClass::KnownBad
                | MemberClass::Contradictory
                | MemberClass::UnknownLoadflag => fold(DependencyOutcome::Unsupported),
                MemberClass::OptionalPhysical | MemberClass::NonFile => {}
            }
        }

        for (key, disk) in declared_disks(index, game) {
            match classify_disk_member(disk) {
                MemberClass::PhysicalRequired => fold(self.disk_outcome(game, key)),
                MemberClass::Borrowed => {
                    let merge = match declared_name(&disk.merge) {
                        DeclaredName::Named(name) => name,
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            fold(DependencyOutcome::Contradictory);
                            continue;
                        }
                    };
                    let Some(provider_decl) = self.borrow_provider(index) else {
                        fold(DependencyOutcome::Contradictory);
                        continue;
                    };
                    match declared_name(provider_decl) {
                        DeclaredName::Named(provider_name) => {
                            match self.graph.resolve_set(provider_name) {
                                SetRef::Unique(provider) => {
                                    fold(self.follow_disk_merge(
                                        provider,
                                        merge,
                                        Some(disk),
                                        &mut guard.clone(),
                                    ));
                                }
                                SetRef::Duplicate => fold(DependencyOutcome::Ambiguous),
                                SetRef::Absent => fold(DependencyOutcome::Contradictory),
                            }
                        }
                        DeclaredName::Absent | DeclaredName::Malformed => {
                            fold(DependencyOutcome::Contradictory);
                        }
                    }
                }
                MemberClass::UnverifiableNodump
                | MemberClass::KnownBad
                | MemberClass::Contradictory
                | MemberClass::UnknownLoadflag => fold(DependencyOutcome::Unsupported),
                MemberClass::OptionalPhysical | MemberClass::NonFile => {}
            }
        }

        // A target set's own disks may themselves be delta images. Folding
        // the same parent resolution in here stops a host being called
        // satisfied by a device whose CHD is present but unusable without a
        // parent that is not.
        for requirement in self.resolve_chd_parents(index) {
            fold(requirement.outcome);
        }

        // A device may itself require devices; MAME flattens this, and here
        // it is an explicit guarded walk that reaches the same closure.
        for device_ref in &game.device_refs {
            match declared_name(&device_ref.name) {
                DeclaredName::Named(device_name) if device_name != name => {
                    match self.graph.resolve_set(device_name) {
                        SetRef::Unique(device) => {
                            fold(self.device_outcome(device, &mut guard.clone()))
                        }
                        SetRef::Duplicate => fold(DependencyOutcome::Ambiguous),
                        SetRef::Absent => fold(DependencyOutcome::Contradictory),
                    }
                }
                DeclaredName::Named(_) => fold(DependencyOutcome::Contradictory),
                DeclaredName::Absent | DeclaredName::Malformed => {
                    fold(DependencyOutcome::Contradictory);
                }
            }
        }

        worst
    }

    /// Sample dependencies.
    ///
    /// Samples are their own namespace: a `.wav`/`.flac` under a sample path
    /// is not a catalogue `<rom>` and never enters the ROM index. Nothing in
    /// the current architecture scans sample storage at all, so this stage can
    /// neither prove nor disprove a sample dependency and says exactly that.
    /// It deliberately does **not** consult ROM evidence, so a ROM that
    /// happens to share a sample's filename can never satisfy one.
    fn resolve_samples(&self, index: usize) -> Vec<DependencyRequirement> {
        let game = self.graph.game(index);
        let mut out = Vec::new();

        if let DeclaredName::Named(name) = declared_name(&game.sample_of) {
            out.push(DependencyRequirement {
                kind: DependencyKind::Sample,
                target: DependencyTarget::SampleSet {
                    name: name.to_string(),
                },
                outcome: DependencyOutcome::Unsupported,
                via_member: None,
            });
        } else if game.sample_of.is_some() {
            out.push(DependencyRequirement {
                kind: DependencyKind::Sample,
                target: DependencyTarget::Undeclared,
                outcome: DependencyOutcome::Contradictory,
                via_member: None,
            });
        }

        for sample in &game.samples {
            let (target, outcome) = match declared_name(&sample.name) {
                DeclaredName::Named(name) => (
                    DependencyTarget::SampleSet {
                        name: name.to_string(),
                    },
                    DependencyOutcome::Unsupported,
                ),
                DeclaredName::Absent | DeclaredName::Malformed => (
                    DependencyTarget::Undeclared,
                    DependencyOutcome::Contradictory,
                ),
            };
            out.push(DependencyRequirement {
                kind: DependencyKind::Sample,
                target,
                outcome,
                via_member: sample.name.clone(),
            });
        }

        out
    }

    /// CHD parent links, which are a format-level fact read from a verified
    /// image's own header - not the catalogue's `disk merge=`.
    ///
    /// The two are resolved entirely separately and neither can satisfy the
    /// other: `merge=` says *where the same image may be found*, while a
    /// non-zero `parent_sha1` says *a second, different image is also
    /// required*. A delta CHD whose own bytes are present and whose `merge=`
    /// resolves perfectly is still incomplete without its parent.
    fn resolve_chd_parents(&self, index: usize) -> Vec<DependencyRequirement> {
        let game = self.graph.game(index);
        let mut out = Vec::new();
        for (key, disk) in declared_disks(index, game) {
            if !self.evidence.verified_disks.contains(&key) {
                continue;
            }
            let Some(outcome) = self.chd_parent_outcome(key) else {
                continue;
            };
            let via_member = disk.name.clone();
            let target = match self
                .evidence
                .verified_disk_parent
                .get(&key)
                .and_then(Option::as_deref)
            {
                Some(parent_identity) => DependencyTarget::ChdIdentity {
                    overall_sha1: parent_identity.to_string(),
                },
                None => DependencyTarget::Undeclared,
            };
            out.push(DependencyRequirement {
                kind: DependencyKind::ChdParent,
                target,
                outcome,
                via_member,
            });
        }
        out
    }

    /// Walks a delta CHD's parent chain by content identity.
    ///
    /// Presence is decided on the parent image's own header identity, which
    /// is the same value a catalogue `<disk sha1>` publishes. It is never
    /// decided on `raw_sha1` (a different digest, over the internal logical
    /// stream) and never on a filename. This resolves *identity*, not
    /// integrity: no hunk is decompressed and no chain is reconstructed.
    fn follow_chd_parents(&self, own_identity: &str, first_parent: &str) -> DependencyOutcome {
        let mut guard = IdentityChainGuard::starting_at(own_identity);
        let mut wanted = first_parent.to_string();
        loop {
            match guard.visit(&wanted) {
                Ok(()) => {}
                Err(ChainFault::Cycle) => return DependencyOutcome::Cycle,
                Err(ChainFault::DepthExceeded) => return DependencyOutcome::Unsupported,
            }
            let Some(facts) = self.evidence.chd_identities.get(&wanted) else {
                return self.absent();
            };
            // Two readable images claiming the same identity while disagreeing
            // about their own parent link is contradictory evidence about one
            // image; neither claim can be trusted over the other.
            if facts.len() > 1 {
                return DependencyOutcome::Contradictory;
            }
            match facts.iter().next() {
                Some(ParentFact::None) | None => return DependencyOutcome::Satisfied,
                Some(ParentFact::Unusable) => return DependencyOutcome::Contradictory,
                Some(ParentFact::Identity(next)) => wanted = next.clone(),
            }
        }
    }
}

enum BorrowedMember<'a> {
    Rom(&'a DatRomEntry),
    Disk(&'a DatDiskEntry),
}

/// Whether two ROM declarations state different content under a shared
/// algorithm.
///
/// Only algorithms *both* declare are compared, so a catalogue publishing
/// CRC32 on one entry and SHA-1 on another is not reported as conflicting -
/// there is simply nothing comparable. Absence of a comparable pair is never
/// read as agreement.
fn checksums_conflict(left: &DatRomEntry, right: &DatRomEntry) -> bool {
    for mine in left.checksums() {
        for theirs in right.checksums() {
            if mine.algorithm == theirs.algorithm && !mine.value.eq_ignore_ascii_case(&theirs.value)
            {
                return true;
            }
        }
    }
    false
}

/// The disk equivalent of [`checksums_conflict`]. Disks carry only SHA-1.
fn disk_sha1_conflict(left: &DatDiskEntry, right: &DatDiskEntry) -> bool {
    match (
        left.sha1.as_deref().and_then(parse_disk_sha1),
        right.sha1.as_deref().and_then(parse_disk_sha1),
    ) {
        (Some(mine), Some(theirs)) => mine != theirs,
        _ => false,
    }
}
