//! Read-only, format-agnostic DAT storage-completeness classification (Stage 2c).
//!
//! A **catalogue set** is one DAT `<game>` entry; its members are the entry's
//! `<rom>` children. This module answers, for one already-audited archive,
//! which catalogue sets its members touch and whether each is complete —
//! using nothing but evidence [`crate::dat::sources::audit_run::run_dat_audit`]
//! already produced. It never hashes anything, never opens an archive, never
//! calls into [`crate::dat::archive`] or a ZIP/7z reader. A `Complete`
//! resolution may be consumed by [`crate::dat::rename_plan`] only for the
//! narrow, fail-closed purpose of naming the outer archive after an additional
//! one-to-one whole-archive attribution check. Archive-member paths and names
//! never become member-level rename proposals.
//!
//! # Scope
//!
//! Implements the "minimum safe completeness rules" from
//! `docs/research/SET_COMPLETENESS_MILESTONE_RESEARCH.md` §2:
//!
//! - **R1 — membership comes only from the DAT.** A set is emitted only when
//!   at least one of its `<rom>`s was matched by a member's verdict. Nothing
//!   is grouped by filename, basename, or directory.
//! - **R2 — only positionally attributable cryptographic matches count.** A
//!   single candidate counts. Multiple candidates remain ambiguous across
//!   games; within one game, one strong digest may satisfy multiple declared
//!   slots carrying that identical digest. CRC32-only, filename-only, and
//!   unmatched members never count.
//! - **R3 — `nodump` blocks `Complete` unconditionally.** A `<rom
//!   status="nodump">` in the set's DAT entry makes the set
//!   [`SetState::BadMetadata`] the moment the entry is read, whether or not
//!   any member was ever seen for it — a nodump rom is unverifiable by
//!   definition, never "missing".
//! - **R4 — any `baddump` blocks `Complete`, matched or not.** A `baddump`
//!   rom is excluded from the required-member list (its absence is never
//!   read as "incomplete" on its own), but its mere presence in the DAT
//!   entry - whether or not this archive happens to contain a member for it
//!   - makes the set [`SetState::BadMetadata`].
//! - **R5 — classification and unsupported shapes fail closed.** ROMs and
//!   disks are classified by [`MemberClass`]. Contradictory flags, unknown
//!   loadflags, malformed member metadata, duplicate top-level ROM names,
//!   duplicate names used through legacy name-only evidence, and
//!   parser-reported unrepresented structure all refuse a confident verdict.
//!   Software-list part/dataarea/diskarea ownership is traversed without
//!   flattening.
//! - **R9 — a required disk needs CHD header identity, positionally
//!   attributed.** A `MemberClass::PhysicalRequired` disk counts as
//!   storage-present only when this run's CHD evidence
//!   ([`crate::dat::disk_audit`]) reports [`DiskAuditVerdict::Exact`] for the
//!   *exact* [`DatDiskKey`] that disk declares, re-validated against the
//!   current DAT (never trusted from index-build time alone). Ambiguous
//!   (`ExactMultipleCandidates`), malformed-header, not-in-DAT, or duplicate
//!   CHD evidence never counts - mirrors R2's ROM rule. A CHD's own
//!   `parent_required()` fact is surfaced (`disks_parent_required`) but never
//!   blocks or is required for this slot's storage completeness -
//!   parent-chain resolution is Stage 2d's job. Disk evidence in this batch
//!   only *verifies slots for sets already touched by ROM evidence*; a game
//!   declaring disks and no ROMs at all is not yet reachable through this
//!   entry point (see the module's "What this batch does not attempt").
//! - **ClrMamePro is fail-closed unconditionally.** That parser does not
//!   currently detect *any* of the structure above - no disk/sample/part/
//!   dataarea/device detection at all - so it cannot honestly claim `false`
//!   for `unsupported_structure` on any entry. Every ClrMamePro-sourced game
//!   sets it `true` at parse time, and therefore every set built from it is
//!   [`NeedsReviewReason::UnsupportedSetStructure`] until that parser can
//!   prove complete set-structure observation, never `Complete`.
//! - **A duplicate `game_name` is never resolved by first match.** If the
//!   touched name is not unique in `games`, every candidate is left
//!   [`NeedsReviewReason::DuplicateGameName`] - picking the first match would
//!   silently bind completeness to array order, which is exactly the
//!   positional-identity risk [`SetIdentity`] exists to avoid.
//! - **Duplicate archive evidence is never trusted.** If the same
//!   archive-member index appears more than once in one archive's evidence,
//!   every set that member's verdict(s) touch is
//!   [`NeedsReviewReason::DuplicateArchiveEvidence`] - not reachable from the
//!   current ZIP/7z producers (each enumerates members once, by
//!   construction), but this module does not trust that invariant blindly.
//! - **R7 — per-archive only.** This module takes one archive's evidence at
//!   a time and never aggregates across archives; a set split across two
//!   archives is judged independently in each. Multi-disc/game-scope
//!   aggregation is explicitly out of this storage-scoped stage.
//! - **R8 — a partial pass forbids `Complete` for every set it touches.** Any
//!   [`ArchivePassCompletion`] other than `Complete` — cancelled, budget-cut,
//!   a refused member, or the outer file changing mid-pass — means some
//!   member's true status is unknown, so nothing that pass touched can be
//!   safely called `Complete`, even a set whose own required members all
//!   happen to already be present.
//!
//! # Runtime DAT binding (no reparse gap)
//!
//! [`classify_archive_sets`] is `pub(crate)`, not `pub`: its `games`
//! parameter must be the exact [`crate::dat::model::ParsedDat`] instance
//! [`crate::dat::sources::audit_run::run_dat_audit`] already parsed to build
//! the [`crate::dat::index::DatIndex`] that produced the archive's verdicts,
//! never a slice obtained by independently re-parsing "the same" DAT file.
//! Reparsing separately would open a real gap: the file on disk could change
//! between the two parses, and a set's completeness would then be judged
//! against a different catalogue than the one its verdicts were actually
//! matched against. Restricting visibility to this crate, with
//! `run_dat_audit` as the only caller, makes that gap structurally
//! unreachable rather than merely documented against.
//!
//! # R4 resolves a real inconsistency in the milestone research
//!
//! The milestone research's own prose and pseudocode disagree with each
//! other about `baddump`: R4's prose says a matched baddump makes the set
//! "Needs review", but its §4 state machine says `BadMetadata(baddump)`.
//! An earlier revision of this module additionally let an *unmatched*
//! baddump rom pass silently through to `Complete`, on the reasoning that
//! R4's own `members_required` note excludes baddump roms from the
//! required list. A hostile review flagged that as a real false-positive
//! risk: an archive could be reported `Complete` while the DAT itself
//! quietly knows about a bad dump nobody surfaced anywhere. This module
//! now resolves all of it toward the strictly safer reading stated in R4
//! above: any DAT-listed baddump, matched or not, blocks `Complete`.
//!
//! # What Stage 2c deliberately does not attempt
//!
//! - Any change to [`crate::dat::archive`], ZIP/7z sources, or the archive
//!   evidence shape.
//! - MAME set verification (gated on the parser work R5 refuses around).
//! - Any member-level or inner-archive rename. The only rename consumer is the
//!   separately gated outer-archive proposal described above.
//! - Clone/parent merge-mode semantics, BIOS dependency tracking, multi-disc
//!   or game-scope aggregation, and CHD-reconstruction completeness.
//! - Opening the CHD map, decompressing a hunk, or verifying `raw_sha1`
//!   against reconstructed content - CHD evidence here is header identity
//!   only ([`crate::dat::disk_audit`]).
//! - Resolving a CHD's declared parent: `parent_required()` is surfaced on
//!   [`SetResolution::disks_parent_required`], never located, opened, or
//!   verified.
//! - Classifying a set touched only by disk evidence with zero ROM
//!   declarations. This batch's `classify_archive_sets` still seeds set
//!   membership from ROM verdicts only (R1); CHD evidence verifies disk
//!   slots for sets a ROM verdict already touched. A disk-only game (no
//!   `<rom>` children at all) is simply never emitted by this function
//!   today, the same silent-absence behaviour an untouched ROM-only game
//!   already has, not a false verdict. Extending set membership to be
//!   disk-evidence-driven too is left to a follow-up batch.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::archive::ArchivePassCompletion;
use super::audit::AuditVerdict;
use super::disk_audit::{DatDiskAudit, DiskAuditVerdict};
use super::index::{DatDiskKey, DatDiskRef, DatMemberKey, DatRomRef, DiskLocation, MemberLocation};
use super::model::{ChecksumAlgorithm, DatChecksum, DatDiskEntry, DatGameEntry, DatRomEntry};
use super::sources::audit_run::DatArchiveAudit;

/// Durable identity for one catalogue set.
///
/// Deliberately never a positional `game_index`: a DAT can be re-parsed or
/// merged with entries reordered, and an index would silently point at a
/// different game. `source_id` plus the DAT's own game name is what survives
/// that - the same identity a human means when they say "this set".
///
/// # No DAT content digest (deliberate, not an oversight)
///
/// The milestone research's own §4 sketches this identity as `DatDigest +
/// DatSourceId + game_name`. There is currently no canonical DAT-content
/// digest anywhere in the data this consumer or its caller has access to -
/// not on [`crate::dat::model::DatSource`], not on
/// [`crate::dat::sources::audit_run::DatAuditOutcome`], nowhere. `source_id`
/// is a user-facing source *registration* string, not a hash of catalogue
/// *content*: two different DAT file revisions registered under the same
/// `source_id` over time would not be told apart by this type today.
///
/// That is a real gap for a *persisted* identity - which is exactly what a
/// future evidence-persistence milestone would need - but Stage 1 does not
/// persist or cross-compare `SetResolution` across runs at all, so it is
/// inert here. Rather than invent a second, ad hoc hashing/provenance
/// system in this module to satisfy the research's literal type shape, the
/// digest is left out and this limitation is recorded here explicitly:
/// adding a `dat_digest` field is the right fix, but it belongs to whichever
/// milestone actually persists this type, where the digest's source and
/// computation can be decided alongside the rest of that design.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SetIdentity {
    pub source_id: String,
    pub game_name: String,
}

/// Why a `nodump`/`baddump` rom disqualifies a set from `Complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BadMetadataReason {
    /// The DAT declares this rom unverifiable; it can never be "missing"
    /// because no correct dump is even claimed to be checkable (R3).
    NoDump,
    /// The set's DAT entry lists a rom the DAT itself marks as a known bad
    /// dump - present in the entry at all, whether or not this archive
    /// contains a member for it (R4).
    BadDump,
}

/// Why a set cannot be classified `Complete` or `Incomplete` with confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsReviewReason {
    /// A member's cryptographic hash matched more than one DAT entry; this
    /// set is one of the candidates and cannot be ruled in or out (R2).
    AmbiguousMemberAttribution,
    /// The parser or member shape cannot be represented or verified safely,
    /// including physical disks until CHD-aware presence evidence exists.
    UnsupportedSetStructure,
    /// The archive's own pass did not finish examining every member, so some
    /// member this set might need was never actually checked (R8).
    PartialArchivePass,
    /// The touched `game_name` is not unique in the DAT: two or more entries
    /// share it, and this stage has no durable, non-positional way to tell them
    /// apart (`SetIdentity` is deliberately never a positional index).
    /// Neither/none of the ambiguous candidates is resolved.
    DuplicateGameName,
    /// The same archive-member index appeared more than once in one
    /// archive's evidence. Not reachable from the current ZIP/7z producers
    /// (each enumerates members once, by construction) - this is a defensive
    /// check against a future or malformed producer, not a live case.
    DuplicateArchiveEvidence,
    /// Mutually exclusive member markers were declared together, or another
    /// classification field was malformed and cannot be interpreted safely.
    ContradictoryMemberFlags,
    /// A ROM carries a loadflag outside the documented software-list set.
    UnknownLoadflag,
    /// The software list marks this entry unsupported or partially supported,
    /// or supplies a malformed support value.
    UnsupportedSoftware,
    /// The entry declares no ROMs or disks, so there is no storage set to
    /// classify.
    NoDeclaredMembers,
    /// Every member is optional or non-file, with no required or borrowed
    /// storage identity anchoring the set.
    OnlyNonFileOrOptionalMembers,
}

/// One catalogue set's storage-completeness state.
///
/// `Complete` is deliberately the least reachable state: every other variant
/// is what a mixed or partial result degrades to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetState {
    /// Every locally required physical member was strongly verified, or the
    /// set declares only borrowed members. This is storage completeness only;
    /// it does not claim dependencies are resolved or the software runnable.
    Complete,
    /// At least one required rom is absent or was not verified, and nothing
    /// else disqualifies the set outright.
    Incomplete,
    BadMetadata(BadMetadataReason),
    NeedsReview(NeedsReviewReason),
}

/// One ROM or disk that flagged `BadMetadata`, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetBadMember {
    pub rom_name: String,
    pub reason: BadMetadataReason,
}

/// One catalogue set's storage resolution, scoped to a single archive (R7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetResolution {
    pub identity: SetIdentity,
    /// The archive whose members produced this resolution. Provenance only -
    /// never a rename target; see the module doc.
    pub archive_path: PathBuf,
    pub state: SetState,
    /// Rom names required for `Complete`, in DAT order. Excludes `nodump`
    /// and `baddump` roms (R4's `members_required` note).
    pub members_required: Vec<String>,
    /// The subset of `members_required` this archive verified present.
    pub members_verified: Vec<String>,
    pub members_bad: Vec<SetBadMember>,
    /// Optional physical members that this archive strongly verified.
    pub members_optional: Vec<String>,
    /// ROM or disk members borrowed from a parent/dependency set. Resolution
    /// of those dependencies is deferred to Stage 2d.
    pub members_borrowed: Vec<String>,
    /// Physical disks declared locally, in DAT order.
    pub disks_required: Vec<String>,
    /// The subset of `disks_required` this run's CHD evidence verified
    /// present by header identity (`overall_sha1`).
    pub disks_verified: Vec<String>,
    /// Names from `disks_verified` whose matched CHD reported
    /// `parent_required() == true`. Surfaced for a future Stage 2d dependency
    /// pass; never resolved, chased, or allowed to block storage
    /// completeness here.
    pub disks_parent_required: Vec<String>,
}

/// The conceptual role of one ROM or disk declared by a DAT.
///
/// Stage 2b classifies provenance; Stage 2c consumes these values to decide
/// storage completeness without resolving runtime dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberClass {
    PhysicalRequired,
    OptionalPhysical,
    Borrowed,
    NonFile,
    UnverifiableNodump,
    KnownBad,
    Contradictory,
    UnknownLoadflag,
}

const NON_FILE_LOADFLAGS: &[&str] = &["fill", "reload", "reload_plain", "continue", "ignore"];

const PHYSICAL_LOADFLAGS: &[&str] = &[
    "load16_byte",
    "load16_word",
    "load16_word_swap",
    "load32_byte",
    "load32_word",
    "load32_word_swap",
    "load32_dword",
    "load64_word",
    "load64_word_swap",
];

/// Classifies one ROM using the Stage 2b precedence table.
///
/// This is intentionally a pure function. Invalid empty/unknown status,
/// optional, or merge values map to [`MemberClass::Contradictory`] so malformed
/// provenance cannot silently become an ordinary physical member.
pub fn classify_rom_member(rom: &DatRomEntry) -> MemberClass {
    let loadflag = rom.loadflag.as_deref().map(str::trim);
    let is_non_file = loadflag.is_some_and(|value| {
        NON_FILE_LOADFLAGS
            .iter()
            .any(|known| value.eq_ignore_ascii_case(known))
    });

    if is_non_file && rom.merge.is_some() {
        return MemberClass::Contradictory;
    }
    if is_non_file {
        return MemberClass::NonFile;
    }
    if let Some(loadflag) = loadflag
        && !PHYSICAL_LOADFLAGS
            .iter()
            .any(|known| loadflag.eq_ignore_ascii_case(known))
    {
        return MemberClass::UnknownLoadflag;
    }

    match ordinary_status(&rom.status) {
        StatusValue::NoDump => return MemberClass::UnverifiableNodump,
        StatusValue::BadDump => return MemberClass::KnownBad,
        StatusValue::Malformed => return MemberClass::Contradictory,
        StatusValue::Ordinary => {}
    }

    match nonempty_marker(&rom.merge) {
        MarkerValue::Present => return MemberClass::Borrowed,
        MarkerValue::Malformed => return MemberClass::Contradictory,
        MarkerValue::Absent => {}
    }

    match yes_no_marker(&rom.optional) {
        MarkerValue::Present => MemberClass::OptionalPhysical,
        MarkerValue::Malformed => MemberClass::Contradictory,
        MarkerValue::Absent => MemberClass::PhysicalRequired,
    }
}

/// Classifies one disk using the Stage 2b disk precedence table.
pub fn classify_disk_member(disk: &DatDiskEntry) -> MemberClass {
    match ordinary_status(&disk.status) {
        StatusValue::NoDump => return MemberClass::UnverifiableNodump,
        StatusValue::BadDump => return MemberClass::KnownBad,
        StatusValue::Malformed => return MemberClass::Contradictory,
        StatusValue::Ordinary => {}
    }

    match nonempty_marker(&disk.merge) {
        MarkerValue::Present => return MemberClass::Borrowed,
        MarkerValue::Malformed => return MemberClass::Contradictory,
        MarkerValue::Absent => {}
    }

    match yes_no_marker(&disk.optional) {
        MarkerValue::Present => MemberClass::OptionalPhysical,
        MarkerValue::Malformed => MemberClass::Contradictory,
        MarkerValue::Absent => MemberClass::PhysicalRequired,
    }
}

#[derive(Clone, Copy)]
enum StatusValue {
    Ordinary,
    NoDump,
    BadDump,
    Malformed,
}

#[derive(Clone, Copy)]
enum MarkerValue {
    Absent,
    Present,
    Malformed,
}

fn ordinary_status(value: &Option<String>) -> StatusValue {
    match value.as_deref().map(str::trim) {
        None => StatusValue::Ordinary,
        Some(value) if value.eq_ignore_ascii_case("good") => StatusValue::Ordinary,
        Some(value) if value.eq_ignore_ascii_case("nodump") => StatusValue::NoDump,
        Some(value) if value.eq_ignore_ascii_case("baddump") => StatusValue::BadDump,
        Some(_) => StatusValue::Malformed,
    }
}

fn nonempty_marker(value: &Option<String>) -> MarkerValue {
    match value.as_deref().map(str::trim) {
        None => MarkerValue::Absent,
        Some("") => MarkerValue::Malformed,
        Some(_) => MarkerValue::Present,
    }
}

fn yes_no_marker(value: &Option<String>) -> MarkerValue {
    match value.as_deref().map(str::trim) {
        None => MarkerValue::Absent,
        Some(value) if value.eq_ignore_ascii_case("yes") => MarkerValue::Present,
        Some(value) if value.eq_ignore_ascii_case("no") => MarkerValue::Absent,
        Some(_) => MarkerValue::Malformed,
    }
}

#[derive(Default)]
struct TouchedSet {
    verified_member_keys: HashSet<DatMemberKey>,
    legacy_verified_rom_names: HashSet<String>,
    used_legacy_evidence: bool,
    ambiguous: bool,
    /// Set when any member that touched this set shared its archive-member
    /// index with another member in the same archive's evidence (item 7):
    /// the evidence itself cannot be trusted for this set, independent of
    /// what it appears to say.
    duplicate_evidence: bool,
}

/// Classifies every catalogue set touched by one already-audited archive.
///
/// `games` is the DAT's own game list. It must be the *exact* in-memory
/// instance the caller used to build the [`crate::dat::index::DatIndex`]
/// that produced `archive`'s verdicts - never a freshly re-parsed copy of
/// "the same" DAT file. [`crate::dat::sources::audit_run::run_dat_audit`] is
/// the only caller and satisfies this by construction (see its own doc);
/// this function is `pub(crate)` specifically so nothing outside this crate
/// can hand it an independently-sourced slice and reopen a TOCTOU gap
/// between what was indexed and what is used to judge completeness.
///
/// `source_id` identifies which DAT source `games` came from, completing the
/// durable [`SetIdentity`]. Only sets with at least one member match in
/// `archive` are returned (R1) - a DAT can define thousands of sets an
/// archive says nothing about, and none of them appear.
pub(crate) fn classify_archive_sets(
    archive: &DatArchiveAudit,
    disk_evidence: &[DatDiskAudit],
    disk_scan_complete: bool,
    games: &[DatGameEntry],
    source_id: &str,
) -> Vec<SetResolution> {
    // Item 7: an archive-member index appearing more than once in one
    // archive's evidence is not reachable from the current ZIP/7z producers
    // (each enumerates members once, by construction - see their own
    // module docs), but this function does not trust that invariant blindly.
    let mut index_counts: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for member in &archive.members {
        *index_counts.entry(member.evidence.index).or_insert(0) += 1;
    }
    let duplicate_indices: HashSet<usize> = index_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(index, _)| index)
        .collect();

    let mut touched: BTreeMap<String, TouchedSet> = BTreeMap::new();

    for member in &archive.members {
        let at_duplicate_index = duplicate_indices.contains(&member.evidence.index);
        if !member.matched_refs.is_empty() {
            let refs = &member.matched_refs;
            let strong_verdict = matches!(
                member.verdict,
                Some(AuditVerdict::Exact { .. })
                    | Some(AuditVerdict::ExactMultipleCandidates { .. })
            );
            let valid_positions = refs
                .iter()
                .all(|candidate| ref_matches_catalogue(candidate, games));
            let one_game = refs
                .first()
                .map(|first| {
                    refs.iter()
                        .all(|candidate| candidate.game_name == first.game_name)
                })
                .unwrap_or(false);
            let safely_shared_slot_evidence =
                one_game && valid_positions && refs_share_cryptographic_identity(refs);

            if strong_verdict && valid_positions && (refs.len() == 1 || safely_shared_slot_evidence)
            {
                let game_name = refs[0].game_name.clone();
                let set = touched.entry(game_name).or_default();
                set.verified_member_keys
                    .extend(refs.iter().map(DatRomRef::key));
                set.duplicate_evidence |= at_duplicate_index;
            } else {
                for candidate in refs {
                    let set = touched.entry(candidate.game_name.clone()).or_default();
                    set.ambiguous = true;
                    set.duplicate_evidence |= at_duplicate_index;
                }
            }
            continue;
        }

        match &member.verdict {
            Some(AuditVerdict::Exact {
                game_name,
                rom_name,
                ..
            }) => {
                let set = touched.entry(game_name.clone()).or_default();
                set.legacy_verified_rom_names.insert(rom_name.clone());
                set.used_legacy_evidence = true;
                set.duplicate_evidence |= at_duplicate_index;
            }
            Some(AuditVerdict::ExactMultipleCandidates { game_names, .. }) => {
                for game_name in game_names {
                    let set = touched.entry(game_name.clone()).or_default();
                    set.ambiguous = true;
                    set.duplicate_evidence |= at_duplicate_index;
                }
            }
            // Probable/ProbableMultipleCandidates (CRC32-only), FilenameOnly,
            // Ambiguous, NotInDat, NoUsableEvidence, and no-verdict-at-all
            // (refused/corrupt/nested/encrypted members) never count toward
            // set membership - R2.
            _ => {}
        }
    }

    let archive_pass_complete = matches!(archive.completion, ArchivePassCompletion::Complete);
    // R8 extended to disks: a partial CHD scan means some required disk's
    // true presence is unknown, so nothing it touched can be safely called
    // `Complete` either - same reasoning as a partial archive pass.
    let pass_complete = archive_pass_complete && disk_scan_complete;
    let disk_summary = summarize_disk_evidence(disk_evidence, games);

    let mut resolutions = Vec::with_capacity(touched.len());
    for (game_name, touch) in touched {
        let identity = SetIdentity {
            source_id: source_id.to_string(),
            game_name: game_name.clone(),
        };

        // Item 2: a `game_name` that is not unique in the DAT cannot be
        // resolved by picking the first match - that would silently bind
        // this set's completeness to whichever entry happens to sort first,
        // which is exactly the positional-identity risk `SetIdentity` is
        // designed never to have. Every candidate is left unresolved.
        let matching_games: Vec<(usize, &DatGameEntry)> = games
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.name == game_name)
            .collect();
        let (game_index, game) = match matching_games.as_slice() {
            // The verdict named a game that isn't in our own game list. Both
            // come from the same DatIndex build in every real caller; fail
            // closed on the mismatch rather than guess or panic.
            [] => continue,
            [only] => *only,
            _ => {
                let reason = if pass_complete {
                    NeedsReviewReason::DuplicateGameName
                } else {
                    NeedsReviewReason::PartialArchivePass
                };
                resolutions.push(empty_resolution(
                    identity,
                    &archive.archive_path,
                    SetState::NeedsReview(reason),
                ));
                continue;
            }
        };

        let classified_roms: Vec<(DatMemberKey, &DatRomEntry, MemberClass)> =
            declared_roms(game_index, game)
                .map(|(key, rom)| (key, rom, classify_rom_member(rom)))
                .collect();
        let classified_disks: Vec<(DatDiskKey, &DatDiskEntry, MemberClass)> =
            declared_disks(game_index, game)
                .map(|(key, disk)| (key, disk, classify_disk_member(disk)))
                .collect();
        let verified_disk_keys = disk_summary
            .verified
            .get(game_name.as_str())
            .cloned()
            .unwrap_or_default();
        let parent_required_disk_keys = disk_summary
            .parent_required
            .get(game_name.as_str())
            .cloned()
            .unwrap_or_default();

        let mut members_required = Vec::new();
        let mut members_optional = Vec::new();
        let mut members_borrowed = Vec::new();
        let mut disks_required = Vec::new();
        let mut disks_verified = Vec::new();
        let mut disks_parent_required = Vec::new();
        let mut members_bad = Vec::new();
        let mut has_contradictory = false;
        let mut has_unknown_loadflag = false;
        let mut has_non_file_or_optional = false;

        for (key, rom, class) in &classified_roms {
            match class {
                MemberClass::PhysicalRequired => members_required.push(rom.name.clone()),
                MemberClass::OptionalPhysical => {
                    has_non_file_or_optional = true;
                    if member_slot_verified(&touch, *key, &rom.name) {
                        members_optional.push(rom.name.clone());
                    }
                }
                MemberClass::Borrowed => members_borrowed.push(rom.name.clone()),
                MemberClass::NonFile => has_non_file_or_optional = true,
                MemberClass::UnverifiableNodump => {
                    members_bad.push(SetBadMember {
                        rom_name: rom.name.clone(),
                        reason: BadMetadataReason::NoDump,
                    });
                }
                MemberClass::KnownBad => {
                    members_bad.push(SetBadMember {
                        rom_name: rom.name.clone(),
                        reason: BadMetadataReason::BadDump,
                    });
                }
                MemberClass::Contradictory => has_contradictory = true,
                MemberClass::UnknownLoadflag => has_unknown_loadflag = true,
            }
        }

        for (key, disk, class) in &classified_disks {
            let name = disk.name.clone().unwrap_or_default();
            match class {
                MemberClass::PhysicalRequired => {
                    disks_required.push(name.clone());
                    if verified_disk_keys.contains(key) {
                        disks_verified.push(name.clone());
                        if parent_required_disk_keys.contains(key) {
                            disks_parent_required.push(name);
                        }
                    }
                }
                MemberClass::OptionalPhysical => has_non_file_or_optional = true,
                MemberClass::Borrowed => members_borrowed.push(name),
                MemberClass::UnverifiableNodump => members_bad.push(SetBadMember {
                    rom_name: name,
                    reason: BadMetadataReason::NoDump,
                }),
                MemberClass::KnownBad => members_bad.push(SetBadMember {
                    rom_name: name,
                    reason: BadMetadataReason::BadDump,
                }),
                MemberClass::Contradictory => has_contradictory = true,
                MemberClass::NonFile | MemberClass::UnknownLoadflag => {
                    // Disk classification never produces these variants.
                    has_contradictory = true;
                }
            }
        }

        let members_verified: Vec<String> = classified_roms
            .iter()
            .filter(|(key, rom, class)| {
                *class == MemberClass::PhysicalRequired
                    && member_slot_verified(&touch, *key, &rom.name)
            })
            .map(|(_, rom, _)| rom.name.clone())
            .collect();

        // S2c transition 1: an incomplete archive pass (ROM or disk)
        // invalidates confidence in every later catalogue/evidence decision.
        // Lists remain available for diagnostics, but cannot affect the
        // verdict.
        if !pass_complete {
            resolutions.push(SetResolution {
                identity,
                archive_path: archive.archive_path.clone(),
                state: SetState::NeedsReview(NeedsReviewReason::PartialArchivePass),
                members_required,
                members_verified,
                members_bad: Vec::new(),
                members_optional,
                members_borrowed,
                disks_required,
                disks_verified,
                disks_parent_required,
            });
            continue;
        }

        // S2c transition 2: state is determined by evidence integrity before
        // any classification refusal. Member lists are still surfaced for
        // continuity with Stage 1 diagnostics. Disk evidence integrity
        // (ambiguous SHA-1 attribution, duplicate CHD evidence) is folded in
        // exactly like the ROM equivalents - mirrors R2/item 7 for disks.
        let disk_ambiguous = disk_summary.ambiguous_games.contains(game_name.as_str());
        let disk_duplicate_evidence = disk_summary
            .duplicate_evidence_games
            .contains(game_name.as_str());
        let evidence_refusal = if touch.duplicate_evidence || disk_duplicate_evidence {
            Some(NeedsReviewReason::DuplicateArchiveEvidence)
        } else if touch.ambiguous || disk_ambiguous {
            Some(NeedsReviewReason::AmbiguousMemberAttribution)
        } else {
            None
        };
        if let Some(reason) = evidence_refusal {
            resolutions.push(SetResolution {
                identity,
                archive_path: archive.archive_path.clone(),
                state: SetState::NeedsReview(reason),
                members_required,
                members_verified,
                members_bad: Vec::new(),
                members_optional,
                members_borrowed,
                disks_required,
                disks_verified,
                disks_parent_required,
            });
            continue;
        }

        // S2c transition 3: classification contradictions are more specific
        // than the general structural refusal below.
        let classification_refusal = if has_contradictory {
            Some(NeedsReviewReason::ContradictoryMemberFlags)
        } else if has_unknown_loadflag {
            Some(NeedsReviewReason::UnknownLoadflag)
        } else {
            None
        };

        let unsupported_structure = game.unsupported_structure
            || has_unsupported_member_shape(&classified_roms, &classified_disks)
            || has_unsafe_duplicate_evidence_names(&classified_roms, touch.used_legacy_evidence)
            || touch
                .verified_member_keys
                .iter()
                .any(|key| key.game_index != game_index)
            || verified_disk_keys
                .iter()
                .any(|key| key.game_index != game_index);

        let supported_refusal = match game.supported.as_deref().map(str::trim) {
            None => None,
            Some(value) if value.eq_ignore_ascii_case("yes") => None,
            Some(value)
                if value.eq_ignore_ascii_case("no") || value.eq_ignore_ascii_case("partial") =>
            {
                Some(NeedsReviewReason::UnsupportedSoftware)
            }
            Some(_) => Some(NeedsReviewReason::UnsupportedSoftware),
        };

        let has_nodump = members_bad
            .iter()
            .any(|bad| bad.reason == BadMetadataReason::NoDump);
        let has_baddump = members_bad
            .iter()
            .any(|bad| bad.reason == BadMetadataReason::BadDump);

        let no_declared_members = classified_roms.is_empty() && classified_disks.is_empty();
        let only_non_file_or_optional = members_required.is_empty()
            && members_borrowed.is_empty()
            && has_non_file_or_optional
            && !has_nodump
            && !has_baddump;
        let all_roms_present = classified_roms.iter().all(|(key, rom, class)| {
            *class != MemberClass::PhysicalRequired || member_slot_verified(&touch, *key, &rom.name)
        });
        let all_disks_present = classified_disks.iter().all(|(key, _, class)| {
            *class != MemberClass::PhysicalRequired || verified_disk_keys.contains(key)
        });
        let all_required_present = all_roms_present && all_disks_present;

        let state = if let Some(reason) = classification_refusal {
            SetState::NeedsReview(reason)
        } else if unsupported_structure {
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        } else if let Some(reason) = supported_refusal {
            SetState::NeedsReview(reason)
        } else if has_nodump {
            SetState::BadMetadata(BadMetadataReason::NoDump)
        } else if has_baddump {
            SetState::BadMetadata(BadMetadataReason::BadDump)
        } else if no_declared_members {
            SetState::NeedsReview(NeedsReviewReason::NoDeclaredMembers)
        } else if only_non_file_or_optional {
            SetState::NeedsReview(NeedsReviewReason::OnlyNonFileOrOptionalMembers)
        } else if !all_required_present {
            SetState::Incomplete
        } else {
            SetState::Complete
        };

        resolutions.push(SetResolution {
            identity,
            archive_path: archive.archive_path.clone(),
            state,
            members_required,
            members_verified,
            members_bad,
            members_optional,
            members_borrowed,
            disks_required,
            disks_verified,
            disks_parent_required,
        });
    }
    resolutions
}

fn empty_resolution(
    identity: SetIdentity,
    archive_path: &std::path::Path,
    state: SetState,
) -> SetResolution {
    SetResolution {
        identity,
        archive_path: archive_path.to_path_buf(),
        state,
        members_required: Vec::new(),
        members_verified: Vec::new(),
        members_bad: Vec::new(),
        members_optional: Vec::new(),
        members_borrowed: Vec::new(),
        disks_required: Vec::new(),
        disks_verified: Vec::new(),
        disks_parent_required: Vec::new(),
    }
}

fn declared_roms(
    game_index: usize,
    game: &DatGameEntry,
) -> impl Iterator<Item = (DatMemberKey, &DatRomEntry)> {
    game.roms
        .iter()
        .enumerate()
        .map(move |(rom_index, rom)| {
            (
                DatMemberKey {
                    game_index,
                    location: MemberLocation::TopLevel { rom_index },
                },
                rom,
            )
        })
        .chain(
            game.parts
                .iter()
                .enumerate()
                .flat_map(move |(part_index, part)| {
                    part.data_areas
                        .iter()
                        .enumerate()
                        .flat_map(move |(data_area_index, area)| {
                            area.roms
                                .iter()
                                .enumerate()
                                .map(move |(member_index, rom)| {
                                    (
                                        DatMemberKey {
                                            game_index,
                                            location: MemberLocation::DataArea {
                                                part_index,
                                                data_area_index,
                                                member_index,
                                            },
                                        },
                                        rom,
                                    )
                                })
                        })
                }),
        )
}

fn member_slot_verified(touch: &TouchedSet, key: DatMemberKey, name: &str) -> bool {
    touch.verified_member_keys.contains(&key) || touch.legacy_verified_rom_names.contains(name)
}

fn ref_matches_catalogue(candidate: &DatRomRef, games: &[DatGameEntry]) -> bool {
    let key = candidate.key();
    if key.game_index != candidate.game_index {
        return false;
    }
    let Some(game) = games.get(key.game_index) else {
        return false;
    };
    if game.name != candidate.game_name {
        return false;
    }
    let rom = match key.location {
        MemberLocation::TopLevel { rom_index } => game.roms.get(rom_index),
        MemberLocation::DataArea {
            part_index,
            data_area_index,
            member_index,
        } => game
            .parts
            .get(part_index)
            .and_then(|part| part.data_areas.get(data_area_index))
            .and_then(|area| area.roms.get(member_index)),
    };
    rom.is_some_and(|rom| {
        rom.name == candidate.rom_name
            && rom.size_bytes == candidate.size_bytes
            && rom.checksums() == candidate.checksums
    })
}

/// Disk-side twin of [`ref_matches_catalogue`]: re-resolves `candidate`'s
/// positional key against the *current* `games` slice and requires the live
/// entry to still declare the same name and the same normalised SHA-1.
/// Refuses to trust a stored [`DatDiskRef`] just because it existed at
/// index-build time - the DAT/index relationship must still hold now.
fn ref_matches_disk_catalogue(candidate: &DatDiskRef, games: &[DatGameEntry]) -> bool {
    let key = candidate.key();
    if key.game_index != candidate.game_index {
        return false;
    }
    let Some(game) = games.get(key.game_index) else {
        return false;
    };
    if game.name != candidate.game_name {
        return false;
    }
    let disk = match key.location {
        DiskLocation::TopLevel { disk_index } => game.disks.get(disk_index),
        DiskLocation::DiskArea {
            part_index,
            disk_area_index,
            member_index,
        } => game
            .parts
            .get(part_index)
            .and_then(|part| part.disk_areas.get(disk_area_index))
            .and_then(|area| area.disks.get(member_index)),
    };
    disk.is_some_and(|disk| {
        disk.name.as_deref().unwrap_or_default() == candidate.disk_name
            && disk
                .sha1
                .as_deref()
                .and_then(|raw| DatChecksum::parse(ChecksumAlgorithm::Sha1, raw))
                .is_some_and(|checksum| checksum.value == candidate.sha1)
    })
}

/// Per-game-name summary of one run's CHD disk evidence, precomputed once so
/// the per-game classification loop is a cheap lookup rather than a rescan.
#[derive(Default)]
struct DiskEvidenceSummary {
    verified: std::collections::HashMap<String, HashSet<DatDiskKey>>,
    parent_required: std::collections::HashMap<String, HashSet<DatDiskKey>>,
    ambiguous_games: HashSet<String>,
    duplicate_evidence_games: HashSet<String>,
}

/// Builds [`DiskEvidenceSummary`] from one run's `disk_evidence`.
///
/// Mirrors the ROM `matched_refs` handling in [`classify_archive_sets`]:
/// only [`DiskAuditVerdict::Exact`] with a single, catalogue-revalidated
/// candidate counts as verified. [`DiskAuditVerdict::ExactMultipleCandidates`],
/// an `Exact` verdict whose `matched_refs` shape does not match what an
/// `Exact` verdict must look like, or whose candidate fails
/// [`ref_matches_disk_catalogue`], all mark every named game ambiguous
/// rather than silently choosing one. A `.chd` path appearing more than once
/// in `disk_evidence` (a scanner enumerating the same file twice) taints
/// every game its evidence names, mirroring the ROM duplicate-archive-index
/// guard.
fn summarize_disk_evidence(
    disk_evidence: &[DatDiskAudit],
    games: &[DatGameEntry],
) -> DiskEvidenceSummary {
    let mut path_counts: std::collections::HashMap<&std::path::Path, usize> =
        std::collections::HashMap::new();
    for audit in disk_evidence {
        *path_counts.entry(audit.chd_path.as_path()).or_insert(0) += 1;
    }
    let duplicate_paths: HashSet<&std::path::Path> = path_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(path, _)| path)
        .collect();

    let mut summary = DiskEvidenceSummary::default();

    for audit in disk_evidence {
        let at_duplicate_path = duplicate_paths.contains(audit.chd_path.as_path());
        match &audit.verdict {
            Some(DiskAuditVerdict::Exact { .. }) => {
                if let [only] = audit.matched_refs.as_slice()
                    && ref_matches_disk_catalogue(only, games)
                {
                    summary
                        .verified
                        .entry(only.game_name.clone())
                        .or_default()
                        .insert(only.key());
                    if audit.parent_required {
                        summary
                            .parent_required
                            .entry(only.game_name.clone())
                            .or_default()
                            .insert(only.key());
                    }
                    if at_duplicate_path {
                        summary
                            .duplicate_evidence_games
                            .insert(only.game_name.clone());
                    }
                } else {
                    // The verdict claims a single candidate but the stored
                    // shape does not honestly support that, or the DAT no
                    // longer agrees with what was indexed - fail closed
                    // rather than guess which of the two is stale.
                    for candidate in &audit.matched_refs {
                        summary.ambiguous_games.insert(candidate.game_name.clone());
                    }
                }
            }
            Some(DiskAuditVerdict::ExactMultipleCandidates { .. }) => {
                for candidate in &audit.matched_refs {
                    summary.ambiguous_games.insert(candidate.game_name.clone());
                    if at_duplicate_path {
                        summary
                            .duplicate_evidence_games
                            .insert(candidate.game_name.clone());
                    }
                }
            }
            // `NotInDat` and `HeaderMalformed` never count toward set
            // membership or slot verification - the disk equivalent of R2.
            Some(DiskAuditVerdict::NotInDat)
            | Some(DiskAuditVerdict::HeaderMalformed(_))
            | None => {}
        }
    }

    summary
}

/// Whether every ref in `refs` may safely be treated as declarations of one
/// physical member.
///
/// Two conditions must both hold:
/// 1. at least one strong (non-CRC32) algorithm has a value populated by
///    *every* ref, and that value is identical across all of them;
/// 2. no algorithm populated by two or more refs disagrees in value -
///    including CRC32, and including one ref that declares the same
///    algorithm twice with conflicting values.
///
/// A single ref carrying an internal conflict, or an empty checksum value,
/// is treated as malformed and refuses the whole group rather than being
/// silently skipped.
fn refs_share_cryptographic_identity(refs: &[DatRomRef]) -> bool {
    if refs.len() < 2 {
        return false;
    }
    if refs.iter().any(|candidate| {
        ref_has_malformed_checksums(candidate) || ref_has_conflicting_checksums(candidate)
    }) {
        return false;
    }

    let mut algorithms = Vec::new();
    for candidate in refs {
        for checksum in &candidate.checksums {
            if !algorithms.contains(&checksum.algorithm) {
                algorithms.push(checksum.algorithm);
            }
        }
    }

    let mut strong_algorithm_shared_by_all = false;

    for algorithm in algorithms {
        let mut agreed_value: Option<&str> = None;
        let mut populated_count = 0usize;
        let mut populated_by_all = true;

        for candidate in refs {
            match ref_checksum_value(candidate, algorithm) {
                Some(value) => {
                    populated_count += 1;
                    match agreed_value {
                        None => agreed_value = Some(value),
                        Some(existing) if existing == value => {}
                        // Requirement 2/5: any algorithm two or more refs
                        // populate must agree, CRC32 included.
                        Some(_) => return false,
                    }
                }
                None => populated_by_all = false,
            }
        }

        if populated_count >= 2 && populated_by_all && algorithm != ChecksumAlgorithm::Crc32 {
            strong_algorithm_shared_by_all = true;
        }
    }

    strong_algorithm_shared_by_all
}

/// Whether `candidate` declares the same checksum algorithm twice with
/// disagreeing values. Internally contradictory metadata cannot anchor a
/// cross-slot identity decision.
fn ref_has_conflicting_checksums(candidate: &DatRomRef) -> bool {
    for (index, checksum) in candidate.checksums.iter().enumerate() {
        for other in &candidate.checksums[index + 1..] {
            if other.algorithm == checksum.algorithm && other.value != checksum.value {
                return true;
            }
        }
    }
    false
}

/// Whether `candidate` declares an empty checksum value. Malformed metadata
/// at this layer must never silently participate in an identity decision.
fn ref_has_malformed_checksums(candidate: &DatRomRef) -> bool {
    candidate
        .checksums
        .iter()
        .any(|checksum| checksum.value.trim().is_empty())
}

/// The single value `candidate` declares for `algorithm`, or `None` if it
/// declares none. Callers must have already ruled out internal conflicts via
/// [`ref_has_conflicting_checksums`].
fn ref_checksum_value(candidate: &DatRomRef, algorithm: ChecksumAlgorithm) -> Option<&str> {
    candidate
        .checksums
        .iter()
        .find(|checksum| checksum.algorithm == algorithm)
        .map(|checksum| checksum.value.as_str())
}

fn declared_disks(
    game_index: usize,
    game: &DatGameEntry,
) -> impl Iterator<Item = (DatDiskKey, &DatDiskEntry)> {
    game.disks
        .iter()
        .enumerate()
        .map(move |(disk_index, disk)| {
            (
                DatDiskKey {
                    game_index,
                    location: DiskLocation::TopLevel { disk_index },
                },
                disk,
            )
        })
        .chain(
            game.parts
                .iter()
                .enumerate()
                .flat_map(move |(part_index, part)| {
                    part.disk_areas
                        .iter()
                        .enumerate()
                        .flat_map(move |(disk_area_index, area)| {
                            area.disks
                                .iter()
                                .enumerate()
                                .map(move |(member_index, disk)| {
                                    (
                                        DatDiskKey {
                                            game_index,
                                            location: DiskLocation::DiskArea {
                                                part_index,
                                                disk_area_index,
                                                member_index,
                                            },
                                        },
                                        disk,
                                    )
                                })
                        })
                }),
        )
}

fn has_unsupported_member_shape(
    roms: &[(DatMemberKey, &DatRomEntry, MemberClass)],
    disks: &[(DatDiskKey, &DatDiskEntry, MemberClass)],
) -> bool {
    let invalid_rom = roms.iter().any(|(_, rom, class)| match class {
        MemberClass::PhysicalRequired | MemberClass::OptionalPhysical | MemberClass::Borrowed => {
            rom.name.trim().is_empty() || rom.size_bytes.is_none() || rom.checksums().is_empty()
        }
        MemberClass::UnverifiableNodump | MemberClass::KnownBad => rom.name.trim().is_empty(),
        MemberClass::NonFile | MemberClass::Contradictory | MemberClass::UnknownLoadflag => false,
    });
    // A physical/optional disk with no name, or no well-formed SHA-1, is an
    // uninterpretable catalogue entry - not "missing", which would let it
    // silently fall through to `Incomplete` as if CHD evidence could ever
    // exist for it. `Borrowed` is deliberately excluded from the SHA-1
    // requirement (unlike the ROM check above): a merged disk's own DAT
    // entry legitimately declares no SHA-1 of its own, and Borrowed disks
    // are never looked up against CHD evidence in this batch regardless.
    let invalid_disk = disks.iter().any(|(_, disk, class)| {
        let unnamed = disk
            .name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty());
        match class {
            MemberClass::PhysicalRequired | MemberClass::OptionalPhysical => {
                unnamed
                    || disk
                        .sha1
                        .as_deref()
                        .and_then(|raw| DatChecksum::parse(ChecksumAlgorithm::Sha1, raw))
                        .is_none()
            }
            MemberClass::Borrowed | MemberClass::UnverifiableNodump | MemberClass::KnownBad => {
                unnamed
            }
            MemberClass::NonFile | MemberClass::Contradictory | MemberClass::UnknownLoadflag => {
                false
            }
        }
    });
    invalid_rom || invalid_disk
}

/// Keeps legacy name-only evidence from satisfying two file-bearing slots and
/// preserves the pre-bridge refusal for duplicate top-level names. Keyed
/// duplicate names are accepted only when every duplicate is a nested
/// data-area slot. Unnamed/non-file instructions are excluded because they
/// require no archive evidence and commonly repeat an empty name.
fn has_unsafe_duplicate_evidence_names(
    roms: &[(DatMemberKey, &DatRomEntry, MemberClass)],
    used_legacy_evidence: bool,
) -> bool {
    let mut seen = std::collections::HashMap::with_capacity(roms.len());
    roms.iter()
        .filter(|(_, _, class)| {
            matches!(
                class,
                MemberClass::PhysicalRequired
                    | MemberClass::OptionalPhysical
                    | MemberClass::Borrowed
            )
        })
        .any(|(key, rom, _)| {
            let Some(previous) = seen.insert(rom.name.as_str(), key.location) else {
                return false;
            };
            used_legacy_evidence
                || matches!(previous, MemberLocation::TopLevel { .. })
                || matches!(key.location, MemberLocation::TopLevel { .. })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::chd::ChdHeaderError;
    use crate::dat::archive::{
        ArchiveMemberEvidence, ArchiveMemberHashes, ArchiveMemberStatus, ArchivePassStopReason,
    };
    use crate::dat::model::{DatChecksum, DatDataAreaEntry, DatPartEntry, DatRomEntry};
    use crate::dat::sources::audit_run::DatArchiveMemberAudit;

    mod member_classification {
        use super::*;

        fn classified_rom(
            loadflag: Option<&str>,
            status: Option<&str>,
            merge: Option<&str>,
            optional: Option<&str>,
        ) -> DatRomEntry {
            DatRomEntry {
                name: "member.bin".to_string(),
                loadflag: loadflag.map(str::to_string),
                status: status.map(str::to_string),
                merge: merge.map(str::to_string),
                optional: optional.map(str::to_string),
                ..Default::default()
            }
        }

        fn classified_disk(
            status: Option<&str>,
            merge: Option<&str>,
            optional: Option<&str>,
        ) -> DatDiskEntry {
            DatDiskEntry {
                name: Some("member.chd".to_string()),
                status: status.map(str::to_string),
                merge: merge.map(str::to_string),
                optional: optional.map(str::to_string),
                ..Default::default()
            }
        }

        #[test]
        fn ordinary_rom_is_physical_required() {
            assert_eq!(
                classify_rom_member(&classified_rom(None, None, None, None)),
                MemberClass::PhysicalRequired
            );
            assert_eq!(
                classify_rom_member(&classified_rom(None, Some("GOOD"), None, Some("no"))),
                MemberClass::PhysicalRequired
            );
        }

        #[test]
        fn every_documented_physical_loadflag_stays_physical_required() {
            for loadflag in PHYSICAL_LOADFLAGS {
                assert_eq!(
                    classify_rom_member(&classified_rom(Some(loadflag), None, None, None)),
                    MemberClass::PhysicalRequired,
                    "{loadflag} must describe a physical ROM"
                );
            }
        }

        #[test]
        fn every_documented_non_file_loadflag_is_non_file() {
            for loadflag in NON_FILE_LOADFLAGS {
                assert_eq!(
                    classify_rom_member(&classified_rom(Some(loadflag), None, None, None)),
                    MemberClass::NonFile,
                    "{loadflag} must not claim a physical file"
                );
            }
        }

        #[test]
        fn unknown_loadflag_fails_closed() {
            assert_eq!(
                classify_rom_member(&classified_rom(Some("bogus"), None, None, None)),
                MemberClass::UnknownLoadflag
            );
            assert_eq!(
                classify_rom_member(&classified_rom(Some("  "), None, None, None)),
                MemberClass::UnknownLoadflag
            );
        }

        #[test]
        fn merge_classifies_rom_as_borrowed() {
            assert_eq!(
                classify_rom_member(&classified_rom(None, None, Some("parent.bin"), None)),
                MemberClass::Borrowed
            );
        }

        #[test]
        fn merge_with_non_file_loadflag_is_contradictory() {
            assert_eq!(
                classify_rom_member(&classified_rom(
                    Some("fill"),
                    None,
                    Some("parent.bin"),
                    None,
                )),
                MemberClass::Contradictory
            );
        }

        #[test]
        fn optional_yes_classifies_rom_as_optional_physical() {
            assert_eq!(
                classify_rom_member(&classified_rom(None, None, None, Some("YES"))),
                MemberClass::OptionalPhysical
            );
        }

        #[test]
        fn rom_dump_statuses_are_case_insensitive() {
            assert_eq!(
                classify_rom_member(&classified_rom(None, Some("NoDump"), None, None)),
                MemberClass::UnverifiableNodump
            );
            assert_eq!(
                classify_rom_member(&classified_rom(None, Some("BADdump"), None, None)),
                MemberClass::KnownBad
            );
        }

        #[test]
        fn rom_dump_status_precedes_merge_and_optional() {
            assert_eq!(
                classify_rom_member(&classified_rom(
                    None,
                    Some("nodump"),
                    Some("parent.bin"),
                    Some("yes"),
                )),
                MemberClass::UnverifiableNodump
            );
            assert_eq!(
                classify_rom_member(&classified_rom(
                    None,
                    Some("baddump"),
                    Some("parent.bin"),
                    Some("yes"),
                )),
                MemberClass::KnownBad
            );
        }

        #[test]
        fn malformed_rom_status_optional_and_merge_fail_closed() {
            for rom in [
                classified_rom(None, Some(""), None, None),
                classified_rom(None, Some("mystery"), None, None),
                classified_rom(None, None, None, Some("")),
                classified_rom(None, None, None, Some("maybe")),
                classified_rom(None, None, Some(""), None),
            ] {
                assert_eq!(classify_rom_member(&rom), MemberClass::Contradictory);
            }
        }

        #[test]
        fn disk_classification_uses_status_merge_optional_precedence() {
            assert_eq!(
                classify_disk_member(&classified_disk(None, None, None)),
                MemberClass::PhysicalRequired
            );
            assert_eq!(
                classify_disk_member(&classified_disk(None, None, Some("yes"))),
                MemberClass::OptionalPhysical
            );
            assert_eq!(
                classify_disk_member(&classified_disk(None, Some("parent.chd"), Some("yes"))),
                MemberClass::Borrowed
            );
            assert_eq!(
                classify_disk_member(&classified_disk(
                    Some("nodump"),
                    Some("parent.chd"),
                    Some("yes"),
                )),
                MemberClass::UnverifiableNodump
            );
            assert_eq!(
                classify_disk_member(&classified_disk(
                    Some("baddump"),
                    Some("parent.chd"),
                    Some("yes"),
                )),
                MemberClass::KnownBad
            );
        }

        #[test]
        fn malformed_disk_status_optional_and_merge_fail_closed() {
            for disk in [
                classified_disk(Some(""), None, None),
                classified_disk(Some("mystery"), None, None),
                classified_disk(None, None, Some("")),
                classified_disk(None, None, Some("maybe")),
                classified_disk(None, Some(""), None),
            ] {
                assert_eq!(classify_disk_member(&disk), MemberClass::Contradictory);
            }
        }
    }

    mod cross_slot_identity {
        use super::*;

        fn checksum(algorithm: ChecksumAlgorithm, value: &str) -> DatChecksum {
            DatChecksum {
                algorithm,
                value: value.to_string(),
            }
        }

        fn bare_ref(checksums: Vec<DatChecksum>) -> DatRomRef {
            DatRomRef {
                game_index: 0,
                game_name: "Bare".to_string(),
                rom_index: 0,
                member_key: DatMemberKey {
                    game_index: 0,
                    location: MemberLocation::TopLevel { rom_index: 0 },
                },
                rom_name: "bare.bin".to_string(),
                size_bytes: Some(4),
                checksums,
                status: None,
                merge: None,
                content_classification: Default::default(),
                original_metadata: Default::default(),
            }
        }

        #[test]
        fn a_shared_md5_with_conflicting_shared_sha1_is_rejected() {
            let a = bare_ref(vec![
                checksum(ChecksumAlgorithm::Md5, "x"),
                checksum(ChecksumAlgorithm::Sha1, "y"),
            ]);
            let b = bare_ref(vec![
                checksum(ChecksumAlgorithm::Md5, "x"),
                checksum(ChecksumAlgorithm::Sha1, "z"),
            ]);

            assert!(!refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn a_shared_sha1_with_an_extra_unshared_md5_is_accepted() {
            let a = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);
            let b = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha1, "x"),
                checksum(ChecksumAlgorithm::Md5, "y"),
            ]);

            assert!(refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn a_shared_sha1_with_conflicting_shared_md5_is_rejected() {
            let a = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha1, "x"),
                checksum(ChecksumAlgorithm::Md5, "p"),
            ]);
            let b = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha1, "x"),
                checksum(ChecksumAlgorithm::Md5, "q"),
            ]);

            assert!(!refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn crc32_only_equality_is_never_sufficient() {
            let a = bare_ref(vec![checksum(ChecksumAlgorithm::Crc32, "x")]);
            let b = bare_ref(vec![checksum(ChecksumAlgorithm::Crc32, "x")]);

            assert!(!refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn consistent_sha256_sha1_and_md5_across_every_ref_is_accepted() {
            let a = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha256, "x"),
                checksum(ChecksumAlgorithm::Sha1, "y"),
                checksum(ChecksumAlgorithm::Md5, "z"),
            ]);
            let b = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha256, "x"),
                checksum(ChecksumAlgorithm::Sha1, "y"),
                checksum(ChecksumAlgorithm::Md5, "z"),
            ]);

            assert!(refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn a_shared_strong_digest_tolerates_a_weaker_algorithm_missing_on_some_refs() {
            let a = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);
            let b = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha1, "x"),
                checksum(ChecksumAlgorithm::Md5, "y"),
            ]);
            let c = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);

            assert!(refs_share_cryptographic_identity(&[a, b, c]));
        }

        #[test]
        fn one_ref_declaring_the_same_algorithm_twice_with_conflicting_values_is_rejected() {
            let a = bare_ref(vec![
                checksum(ChecksumAlgorithm::Sha1, "x"),
                checksum(ChecksumAlgorithm::Sha1, "not-x"),
            ]);
            let b = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);

            assert!(!refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn an_empty_checksum_value_fails_closed() {
            let a = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);
            let b = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "")]);

            assert!(!refs_share_cryptographic_identity(&[a, b]));
        }

        #[test]
        fn a_single_ref_is_never_shared_identity() {
            let a = bare_ref(vec![checksum(ChecksumAlgorithm::Sha1, "x")]);

            assert!(!refs_share_cryptographic_identity(&[a]));
        }
    }

    fn rom(name: &str, status: Option<&str>) -> DatRomEntry {
        DatRomEntry {
            name: name.to_string(),
            size_bytes: Some(4),
            crc32: Some("deadbeef".into()),
            md5: None,
            sha1: None,
            sha256: None,
            status: status.map(str::to_string),
            merge: None,
            date: None,
            loadflag: None,
            ..Default::default()
        }
    }

    fn game(name: &str, roms: Vec<DatRomEntry>) -> DatGameEntry {
        DatGameEntry {
            name: name.to_string(),
            description: None,
            roms,
            clone_of: None,
            sample_of: None,
            board: None,
            rebuild_to: None,
            year: None,
            manufacturer: None,
            source_file: None,
            comment: None,
            original_metadata: Default::default(),
            content_classification: Default::default(),
            unsupported_structure: false,
            ..Default::default()
        }
    }

    fn evidence(index: usize, name: &str) -> ArchiveMemberEvidence {
        ArchiveMemberEvidence {
            archive_path: "collection.7z".into(),
            member_name_raw: name.as_bytes().to_vec(),
            member_name_display: name.to_string(),
            index,
            logical_size: 4,
            is_nested_archive: false,
            status: ArchiveMemberStatus::HashComplete,
            hashes: Some(ArchiveMemberHashes {
                crc32: "deadbeef".into(),
                md5: "00".into(),
                sha1: "00".into(),
                sha256: "00".into(),
            }),
        }
    }

    fn exact_member(
        index: usize,
        member_name: &str,
        game_name: &str,
        rom_name: &str,
    ) -> DatArchiveMemberAudit {
        DatArchiveMemberAudit {
            evidence: evidence(index, member_name),
            verdict: Some(AuditVerdict::Exact {
                game_name: game_name.to_string(),
                rom_name: rom_name.to_string(),
                algorithm: "SHA-1",
            }),
            matched_refs: Vec::new(),
        }
    }

    fn keyed_member(index: usize, refs: Vec<DatRomRef>) -> DatArchiveMemberAudit {
        let verdict = if refs.len() == 1 {
            AuditVerdict::Exact {
                game_name: refs[0].game_name.clone(),
                rom_name: refs[0].rom_name.clone(),
                algorithm: "SHA-1",
            }
        } else {
            AuditVerdict::ExactMultipleCandidates {
                algorithm: "SHA-1",
                count: refs.len(),
                game_names: refs
                    .iter()
                    .map(|candidate| candidate.game_name.clone())
                    .collect(),
            }
        };
        DatArchiveMemberAudit {
            evidence: evidence(index, &refs[0].rom_name),
            verdict: Some(verdict),
            matched_refs: refs,
        }
    }

    fn nested_game(name: &str, roms: Vec<DatRomEntry>) -> DatGameEntry {
        let mut game = game(name, Vec::new());
        game.parts = vec![DatPartEntry {
            data_areas: vec![DatDataAreaEntry {
                roms,
                ..Default::default()
            }],
            ..Default::default()
        }];
        game
    }

    fn sha1_rom(name: &str, digit: char) -> DatRomEntry {
        DatRomEntry {
            name: name.to_string(),
            size_bytes: Some(4),
            crc32: None,
            sha1: Some(std::iter::repeat_n(digit, 40).collect()),
            ..Default::default()
        }
    }

    fn nested_ref(game_index: usize, game: &DatGameEntry, member_index: usize) -> DatRomRef {
        let rom = &game.parts[0].data_areas[0].roms[member_index];
        DatRomRef {
            game_index,
            game_name: game.name.clone(),
            rom_index: member_index,
            member_key: DatMemberKey {
                game_index,
                location: MemberLocation::DataArea {
                    part_index: 0,
                    data_area_index: 0,
                    member_index,
                },
            },
            rom_name: rom.name.clone(),
            size_bytes: rom.size_bytes,
            checksums: vec![
                DatChecksum::parse(ChecksumAlgorithm::Sha1, rom.sha1.as_deref().unwrap()).unwrap(),
            ],
            status: rom.status.clone(),
            merge: rom.merge.clone(),
            content_classification: game.content_classification.clone(),
            original_metadata: game.original_metadata.clone(),
        }
    }

    fn top_ref(game_index: usize, game: &DatGameEntry, rom_index: usize) -> DatRomRef {
        let rom = &game.roms[rom_index];
        DatRomRef {
            game_index,
            game_name: game.name.clone(),
            rom_index,
            member_key: DatMemberKey {
                game_index,
                location: MemberLocation::TopLevel { rom_index },
            },
            rom_name: rom.name.clone(),
            size_bytes: rom.size_bytes,
            checksums: vec![
                DatChecksum::parse(ChecksumAlgorithm::Sha1, rom.sha1.as_deref().unwrap()).unwrap(),
            ],
            status: rom.status.clone(),
            merge: rom.merge.clone(),
            content_classification: game.content_classification.clone(),
            original_metadata: game.original_metadata.clone(),
        }
    }

    fn archive(
        members: Vec<DatArchiveMemberAudit>,
        completion: ArchivePassCompletion,
    ) -> DatArchiveAudit {
        let total_members = members.len();
        DatArchiveAudit {
            archive_path: "collection.7z".into(),
            outer_identity: None,
            format: "7z".to_string(),
            total_members,
            completion,
            members,
        }
    }

    fn complete_pass() -> ArchivePassCompletion {
        ArchivePassCompletion::Complete
    }

    // -- 1. simple multi-member complete set ---------------------------

    #[test]
    fn multi_member_set_with_every_rom_verified_is_complete() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.cue", None), rom("game (Track 1).bin", None)],
        )];
        let members = vec![
            exact_member(0, "game.cue", "Game (World)", "game.cue"),
            exact_member(
                1,
                "game (Track 1).bin",
                "Game (World)",
                "game (Track 1).bin",
            ),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_required.len(), 2);
        assert_eq!(resolutions[0].members_verified.len(), 2);
    }

    // -- 2. same set, one required member missing -> Incomplete --------

    #[test]
    fn missing_required_member_is_incomplete() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.cue", None), rom("game (Track 1).bin", None)],
        )];
        // Only the cue was ever seen; the track never showed up in this
        // archive at all.
        let members = vec![exact_member(0, "game.cue", "Game (World)", "game.cue")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Incomplete);
        assert_eq!(
            resolutions[0].members_verified,
            vec!["game.cue".to_string()]
        );
    }

    // -- 3. nodump -> not Complete ---------------------------------------

    #[test]
    fn nodump_rom_is_bad_metadata_even_when_every_other_member_is_present() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bonus.bin", Some("nodump"))],
        )];
        // The one verifiable rom is fully present; the nodump rom was never
        // going to appear as a member at all.
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::BadMetadata(BadMetadataReason::NoDump)
        );
        assert!(
            !resolutions[0]
                .members_required
                .contains(&"bonus.bin".to_string()),
            "a nodump rom is never counted as a required member"
        );
    }

    // -- 4. baddump -> not Complete ---------------------------------------

    #[test]
    fn matched_baddump_rom_is_bad_metadata() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bad.bin", Some("baddump"))],
        )];
        let members = vec![
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
            exact_member(1, "bad.bin", "Game (World)", "bad.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::BadMetadata(BadMetadataReason::BadDump)
        );
    }

    #[test]
    fn dat_listed_but_unmatched_baddump_still_blocks_complete() {
        // Conservative Stage 1 rule (task-mandated revision of R4): the
        // baddump rom is entirely absent from this archive - no member for
        // it exists at all - yet the DAT itself still lists it as a known
        // bad dump for this set. That alone must block Complete; it must
        // NOT be excluded from consideration just because nothing was ever
        // seen for it. Every other rom is genuinely present and verified.
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bad.bin", Some("baddump"))],
        )];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::BadMetadata(BadMetadataReason::BadDump),
            "an unmatched baddump must never be silently excluded and still reach Complete"
        );
        assert!(
            !resolutions[0]
                .members_required
                .contains(&"bad.bin".to_string()),
            "a baddump rom is still excluded from members_required - its absence alone is \
             not what disqualifies the set, its presence in the DAT is"
        );
    }

    // -- 5. ambiguous shared member -> NeedsReview -------------------------

    #[test]
    fn ambiguous_multi_candidate_match_leaves_every_candidate_set_needs_review() {
        let games = vec![
            game("10-Yard Fight (Japan)", vec![rom("shared.chr", None)]),
            game("10-Yard Fight (US, Clone)", vec![rom("shared.chr", None)]),
        ];
        let members = vec![DatArchiveMemberAudit {
            evidence: evidence(0, "shared.chr"),
            verdict: Some(AuditVerdict::ExactMultipleCandidates {
                algorithm: "SHA-1",
                count: 2,
                game_names: vec![
                    "10-Yard Fight (Japan)".to_string(),
                    "10-Yard Fight (US, Clone)".to_string(),
                ],
            }),
            matched_refs: Vec::new(),
        }];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 2);
        assert!(
            resolutions.iter().all(|resolution| resolution.state
                == SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution)),
            "a shared member proves the shared chip, not either game - neither set may be Complete"
        );
    }

    // -- 6. partial archive pass -> never Complete -------------------------

    #[test]
    fn partial_pass_forbids_complete_even_when_every_seen_member_matched() {
        let games = vec![game("Game (World)", vec![rom("game.bin", None)])];
        // Every rom this set actually requires WAS verified; the pass still
        // stopped early on something else entirely.
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(
            members,
            ArchivePassCompletion::Incomplete {
                reason: ArchivePassStopReason::RunLogicalBudget,
            },
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::PartialArchivePass),
            "a set must never be reported Complete from a pass that did not finish (R8)"
        );
    }

    // -- MAME-style / structurally unsupported set -> NeedsReview ----------

    #[test]
    fn rom_with_no_hash_refuses_the_whole_set_into_needs_review() {
        let mut mame_rom = rom("cpu.bin", None);
        mame_rom.crc32 = None;
        let games = vec![game("mame-set", vec![mame_rom, rom("gfx.bin", None)])];
        let members = vec![exact_member(1, "gfx.bin", "mame-set", "gfx.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    #[test]
    fn a_physical_disk_without_chd_evidence_is_incomplete_never_complete() {
        let mut disc_game = game("Disc Game (World)", vec![rom("game.cue", None)]);
        disc_game.disks.push(DatDiskEntry {
            name: Some("game".to_string()),
            sha1: Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_string()),
            ..Default::default()
        });
        let games = vec![disc_game];
        let members = vec![exact_member(0, "game.cue", "Disc Game (World)", "game.cue")];
        let audit = archive(members, complete_pass());

        // No CHD evidence at all is supplied - the required disk cannot be
        // verified, so the set is Incomplete, never Complete and never a
        // guessed UnsupportedSetStructure.
        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::Incomplete,
            "ROM evidence alone cannot satisfy a required disk slot"
        );
        assert_eq!(resolutions[0].disks_required, vec!["game"]);
        assert!(resolutions[0].disks_verified.is_empty());
    }

    #[test]
    fn borrowed_and_bad_metadata_disks_use_storage_classification() {
        let cases = [
            (
                DatDiskEntry {
                    name: Some("parent-disk".to_string()),
                    merge: Some("parent.chd".to_string()),
                    ..Default::default()
                },
                SetState::Complete,
            ),
            (
                DatDiskEntry {
                    name: Some("unknown-disk".to_string()),
                    status: Some("nodump".to_string()),
                    ..Default::default()
                },
                SetState::BadMetadata(BadMetadataReason::NoDump),
            ),
            (
                DatDiskEntry {
                    name: Some("bad-disk".to_string()),
                    status: Some("baddump".to_string()),
                    ..Default::default()
                },
                SetState::BadMetadata(BadMetadataReason::BadDump),
            ),
        ];

        for (disk, expected) in cases {
            let disk_name = disk.name.clone().unwrap();
            let mut disk_game = game("Disk Metadata", vec![rom("anchor.bin", None)]);
            disk_game.disks.push(disk);
            let games = vec![disk_game];
            let audit = archive(
                vec![exact_member(0, "anchor.bin", "Disk Metadata", "anchor.bin")],
                complete_pass(),
            );

            let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

            assert_eq!(resolutions[0].state, expected);
            if expected == SetState::Complete {
                assert_eq!(resolutions[0].members_borrowed, vec![disk_name]);
            }
        }
    }

    mod disk_evidence_completeness {
        use super::*;

        fn disc_game(
            game_name: &str,
            rom_name: &str,
            disk_name: &str,
            disk_sha1: &str,
        ) -> DatGameEntry {
            let mut entry = game(game_name, vec![rom(rom_name, None)]);
            entry.disks.push(DatDiskEntry {
                name: Some(disk_name.to_string()),
                sha1: Some(disk_sha1.to_string()),
                ..Default::default()
            });
            entry
        }

        fn disk_sha1(digit: char) -> String {
            std::iter::repeat_n(digit, 40).collect()
        }

        fn disk_ref_for(game_index: usize, game: &DatGameEntry, disk_index: usize) -> DatDiskRef {
            let disk = &game.disks[disk_index];
            DatDiskRef {
                game_index,
                game_name: game.name.clone(),
                disk_key: DatDiskKey {
                    game_index,
                    location: DiskLocation::TopLevel { disk_index },
                },
                disk_name: disk.name.clone().unwrap_or_default(),
                sha1: DatChecksum::parse(ChecksumAlgorithm::Sha1, disk.sha1.as_deref().unwrap())
                    .unwrap()
                    .value,
                status: disk.status.clone(),
                merge: disk.merge.clone(),
                optional: disk.optional.clone(),
            }
        }

        fn exact_disk_audit(
            path: &str,
            disk_ref: DatDiskRef,
            parent_required: bool,
        ) -> DatDiskAudit {
            DatDiskAudit {
                chd_path: path.into(),
                overall_sha1: Some(disk_ref.sha1.clone()),
                parent_required,
                verdict: Some(DiskAuditVerdict::Exact {
                    game_name: disk_ref.game_name.clone(),
                    disk_name: disk_ref.disk_name.clone(),
                }),
                matched_refs: vec![disk_ref],
            }
        }

        fn ambiguous_disk_audit(path: &str, refs: Vec<DatDiskRef>) -> DatDiskAudit {
            let game_names = refs.iter().map(|r| r.game_name.clone()).collect();
            DatDiskAudit {
                chd_path: path.into(),
                overall_sha1: refs.first().map(|r| r.sha1.clone()),
                parent_required: false,
                verdict: Some(DiskAuditVerdict::ExactMultipleCandidates {
                    count: refs.len(),
                    game_names,
                }),
                matched_refs: refs,
            }
        }

        fn not_in_dat_audit(path: &str, sha1: String) -> DatDiskAudit {
            DatDiskAudit {
                chd_path: path.into(),
                overall_sha1: Some(sha1),
                parent_required: false,
                verdict: Some(DiskAuditVerdict::NotInDat),
                matched_refs: Vec::new(),
            }
        }

        fn malformed_disk_audit(path: &str) -> DatDiskAudit {
            DatDiskAudit {
                chd_path: path.into(),
                overall_sha1: None,
                parent_required: false,
                verdict: Some(DiskAuditVerdict::HeaderMalformed(
                    ChdHeaderError::InvalidMagic,
                )),
                matched_refs: Vec::new(),
            }
        }

        // 16 / 25: required disk + Exact CHD + complete scan, ROM also
        // verified -> Complete.
        #[test]
        fn mixed_rom_and_disk_both_verified_is_complete() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('1'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![exact_disk_audit(
                "/lib/game.chd",
                disk_ref_for(0, &games[0], 0),
                false,
            )];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Complete);
            assert_eq!(resolutions[0].disks_required, vec!["game"]);
            assert_eq!(resolutions[0].disks_verified, vec!["game"]);
            assert!(resolutions[0].disks_parent_required.is_empty());
        }

        // 18: required disk + wrong CHD (present, but not this DAT's) -> not
        // Complete.
        #[test]
        fn wrong_chd_leaves_required_disk_unverified() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('2'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![not_in_dat_audit("/lib/other.chd", disk_sha1('9'))];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Incomplete);
            assert!(resolutions[0].disks_verified.is_empty());
        }

        // 19: required disk + malformed CHD -> not Complete.
        #[test]
        fn malformed_chd_leaves_required_disk_unverified() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('3'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![malformed_disk_audit("/lib/game.chd")];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Incomplete);
            assert!(resolutions[0].disks_verified.is_empty());
        }

        // 20: required disk + ambiguous SHA-1 attribution -> not Complete.
        #[test]
        fn ambiguous_disk_sha1_attribution_is_needs_review_not_complete() {
            let shared = disk_sha1('4');
            let games = vec![
                disc_game("Disc Game A", "a.cue", "disk-a", &shared),
                disc_game("Disc Game B", "b.cue", "disk-b", &shared),
            ];
            let audit = archive(
                vec![exact_member(0, "a.cue", "Disc Game A", "a.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![ambiguous_disk_audit(
                "/lib/shared.chd",
                vec![disk_ref_for(0, &games[0], 0), disk_ref_for(1, &games[1], 0)],
            )];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(
                resolutions[0].state,
                SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution)
            );
        }

        // 21: required disk + partial disk scan -> not Complete, even though
        // the CHD evidence gathered so far looks Exact.
        #[test]
        fn partial_disk_scan_forbids_complete_even_with_exact_evidence_seen() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('5'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![exact_disk_audit(
                "/lib/game.chd",
                disk_ref_for(0, &games[0], 0),
                false,
            )];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, false, &games, "collection");

            assert_eq!(
                resolutions[0].state,
                SetState::NeedsReview(NeedsReviewReason::PartialArchivePass)
            );
        }

        // 22: parent_required = true does not block Complete; it is only
        // surfaced.
        #[test]
        fn parent_required_disk_still_reaches_complete_and_is_surfaced() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('6'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![exact_disk_audit(
                "/lib/game.chd",
                disk_ref_for(0, &games[0], 0),
                true,
            )];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Complete);
            assert_eq!(resolutions[0].disks_verified, vec!["game"]);
            assert_eq!(resolutions[0].disks_parent_required, vec!["game"]);
        }

        // 23: mixed set, ROM good but disk missing -> not Complete.
        #[test]
        fn mixed_set_with_rom_present_but_disk_missing_is_incomplete() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('7'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );

            let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Incomplete);
            assert!(
                resolutions[0]
                    .members_required
                    .contains(&"game.cue".to_string())
            );
            assert!(resolutions[0].disks_verified.is_empty());
        }

        // 24: mixed set, disk good but ROM missing -> not Complete. The set
        // must first be touched by ROM evidence to be classified at all in
        // this batch (see the module doc's documented scope cut), so the
        // archive here supplies a weak/unmatched ROM member purely to touch
        // the set; that member never counts toward `members_verified`.
        #[test]
        fn mixed_set_with_disk_present_but_rom_missing_is_incomplete() {
            let mut disc_game_entry = disc_game("Disc Game", "game.cue", "game", &disk_sha1('8'));
            disc_game_entry.roms.push(rom("extra.bin", None));
            let games = vec![disc_game_entry];
            // Touch the set via the verified disk-bearing rom, but leave the
            // second required rom ("extra.bin") completely unmatched.
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![exact_disk_audit(
                "/lib/game.chd",
                disk_ref_for(0, &games[0], 0),
                false,
            )];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(resolutions[0].state, SetState::Incomplete);
            assert_eq!(resolutions[0].disks_verified, vec!["game"]);
            assert!(
                !resolutions[0]
                    .members_verified
                    .contains(&"extra.bin".to_string())
            );
        }

        // 26: a stored DatDiskRef whose position no longer matches the
        // current DAT (stale index / DAT changed underneath it) must not be
        // trusted - fails closed into NeedsReview rather than Complete.
        #[test]
        fn stale_disk_ref_position_mismatch_fails_closed() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('9'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let mut stale_ref = disk_ref_for(0, &games[0], 0);
            // Corrupt the stored ref so it no longer matches the live entry
            // at that position, simulating a DAT that changed after the
            // disk index was built.
            stale_ref.disk_name = "renamed.chd".to_string();
            let disk_evidence = vec![exact_disk_audit("/lib/game.chd", stale_ref, false)];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_ne!(resolutions[0].state, SetState::Complete);
        }

        // 27: duplicate disk evidence (same `.chd` path scanned/reported
        // twice) is never trusted, even though it "agrees with itself".
        #[test]
        fn duplicate_chd_evidence_is_needs_review_not_complete() {
            let games = vec![disc_game("Disc Game", "game.cue", "game", &disk_sha1('a'))];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );
            let disk_evidence = vec![
                exact_disk_audit("/lib/game.chd", disk_ref_for(0, &games[0], 0), false),
                exact_disk_audit("/lib/game.chd", disk_ref_for(0, &games[0], 0), false),
            ];

            let resolutions =
                classify_archive_sets(&audit, &disk_evidence, true, &games, "collection");

            assert_eq!(
                resolutions[0].state,
                SetState::NeedsReview(NeedsReviewReason::DuplicateArchiveEvidence)
            );
        }

        // 28: explicit no-false-Complete regression sweep across every bad
        // disk-evidence shape this batch defines.
        #[test]
        fn no_bad_disk_evidence_shape_ever_reaches_complete() {
            let sha1 = disk_sha1('b');
            let games = vec![disc_game("Disc Game", "game.cue", "game", &sha1)];
            let audit = archive(
                vec![exact_member(0, "game.cue", "Disc Game", "game.cue")],
                complete_pass(),
            );

            let cases: Vec<(Vec<DatDiskAudit>, bool)> = vec![
                (Vec::new(), true),
                (vec![not_in_dat_audit("/lib/x.chd", disk_sha1('c'))], true),
                (vec![malformed_disk_audit("/lib/x.chd")], true),
                (
                    vec![ambiguous_disk_audit(
                        "/lib/x.chd",
                        vec![disk_ref_for(0, &games[0], 0), disk_ref_for(0, &games[0], 0)],
                    )],
                    true,
                ),
                (
                    vec![exact_disk_audit(
                        "/lib/x.chd",
                        disk_ref_for(0, &games[0], 0),
                        false,
                    )],
                    false, // partial disk scan
                ),
            ];

            for (disk_evidence, disk_scan_complete) in cases {
                let resolutions = classify_archive_sets(
                    &audit,
                    &disk_evidence,
                    disk_scan_complete,
                    &games,
                    "collection",
                );
                assert_ne!(resolutions[0].state, SetState::Complete);
            }
        }
    }

    #[test]
    fn a_non_file_loadflag_is_excluded_from_required_members() {
        let mut fill_rom = rom("fill.bin", None);
        fill_rom.loadflag = Some("fill".to_string());
        let games = vec![game("mame-set", vec![fill_rom, rom("gfx.bin", None)])];
        let members = vec![
            exact_member(0, "fill.bin", "mame-set", "fill.bin"),
            exact_member(1, "gfx.bin", "mame-set", "gfx.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_required, vec!["gfx.bin"]);
        assert_eq!(resolutions[0].members_verified, vec!["gfx.bin"]);
    }

    #[test]
    fn duplicate_dat_rom_names_within_one_set_can_never_become_complete() {
        // Distinct from the archive-member duplicate-name test below: here
        // the DAT ITSELF declares "game.bin" twice for one set. One
        // verified member matching that name must not be allowed to
        // silently satisfy both DAT-declared slots.
        let games = vec![game(
            "Malformed Set",
            vec![rom("game.bin", None), rom("game.bin", None)],
        )];
        let members = vec![exact_member(0, "game.bin", "Malformed Set", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "a duplicated required rom name must never let one member satisfy both slots"
        );
    }

    // -- duplicate archive member names, matched by evidence not name ------

    #[test]
    fn duplicate_member_names_are_matched_by_verdict_not_by_shared_name() {
        // Two archive members share a literal name but were independently
        // hashed and matched to two different DAT roms; classification must
        // key off each member's own verdict, never off `member_name_display`.
        let games = vec![game(
            "Game (World)",
            vec![rom("track.bin", None), rom("track.bin (2)", None)],
        )];
        let members = vec![
            exact_member(0, "data.bin", "Game (World)", "track.bin"),
            exact_member(1, "data.bin", "Game (World)", "track.bin (2)"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_verified.len(), 2);
    }

    // -- R1: a game the DAT defines but no member touched never appears ----

    #[test]
    fn untouched_games_produce_no_resolution() {
        let games = vec![
            game("Game (World)", vec![rom("game.bin", None)]),
            game("Untouched Game", vec![rom("other.bin", None)]),
        ];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].identity.game_name, "Game (World)");
    }

    #[test]
    fn ordinary_top_level_keyed_evidence_keeps_complete_behavior() {
        let games = vec![game("Ordinary", vec![sha1_rom("game.bin", '1')])];
        let audit = archive(
            vec![keyed_member(0, vec![top_ref(0, &games[0], 0)])],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
    }

    #[test]
    fn keyed_duplicate_top_level_names_keep_the_existing_fail_closed_gate() {
        let games = vec![game(
            "Duplicate",
            vec![sha1_rom("same.bin", '1'), sha1_rom("same.bin", '2')],
        )];
        let audit = archive(
            vec![
                keyed_member(0, vec![top_ref(0, &games[0], 0)]),
                keyed_member(1, vec![top_ref(0, &games[0], 1)]),
            ],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    #[test]
    fn nested_only_required_roms_are_complete_when_every_key_is_verified() {
        let games = vec![nested_game(
            "Software",
            vec![sha1_rom("one.bin", '1'), sha1_rom("two.bin", '2')],
        )];
        let members = vec![
            keyed_member(0, vec![nested_ref(0, &games[0], 0)]),
            keyed_member(1, vec![nested_ref(0, &games[0], 1)]),
        ];

        let resolutions = classify_archive_sets(
            &archive(members, complete_pass()),
            &[],
            true,
            &games,
            "collection",
        );

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_verified, vec!["one.bin", "two.bin"]);
    }

    #[test]
    fn missing_nested_required_key_is_incomplete_even_when_names_match() {
        let games = vec![nested_game(
            "Software",
            vec![sha1_rom("same.bin", '1'), sha1_rom("same.bin", '2')],
        )];
        let members = vec![keyed_member(0, vec![nested_ref(0, &games[0], 0)])];

        let resolutions = classify_archive_sets(
            &archive(members, complete_pass()),
            &[],
            true,
            &games,
            "collection",
        );

        assert_eq!(resolutions[0].state, SetState::Incomplete);
        assert_eq!(resolutions[0].members_verified, vec!["same.bin"]);
    }

    #[test]
    fn identical_hash_can_satisfy_two_same_game_required_slots() {
        let games = vec![nested_game(
            "Software",
            vec![sha1_rom("same.bin", '1'), sha1_rom("same.bin", '1')],
        )];
        let refs = vec![nested_ref(0, &games[0], 0), nested_ref(0, &games[0], 1)];

        let resolutions = classify_archive_sets(
            &archive(vec![keyed_member(0, refs)], complete_pass()),
            &[],
            true,
            &games,
            "collection",
        );

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(
            resolutions[0].members_verified,
            vec!["same.bin", "same.bin"]
        );
    }

    #[test]
    fn contradictory_shared_md5_with_conflicting_sha1_never_reaches_complete() {
        fn md5_and_sha1_rom(name: &str, md5_digit: char, sha1_digit: char) -> DatRomEntry {
            DatRomEntry {
                name: name.to_string(),
                size_bytes: Some(4),
                crc32: None,
                md5: Some(std::iter::repeat_n(md5_digit, 32).collect()),
                sha1: Some(std::iter::repeat_n(sha1_digit, 40).collect()),
                ..Default::default()
            }
        }

        fn nested_ref_with_full_checksums(
            game_index: usize,
            game: &DatGameEntry,
            member_index: usize,
        ) -> DatRomRef {
            let rom = &game.parts[0].data_areas[0].roms[member_index];
            DatRomRef {
                game_index,
                game_name: game.name.clone(),
                rom_index: member_index,
                member_key: DatMemberKey {
                    game_index,
                    location: MemberLocation::DataArea {
                        part_index: 0,
                        data_area_index: 0,
                        member_index,
                    },
                },
                rom_name: rom.name.clone(),
                size_bytes: rom.size_bytes,
                checksums: rom.checksums(),
                status: rom.status.clone(),
                merge: rom.merge.clone(),
                content_classification: game.content_classification.clone(),
                original_metadata: game.original_metadata.clone(),
            }
        }

        // Same MD5 ('a'..) on both declared slots, but a contradictory SHA1
        // ('1' vs '2') - exactly the internally contradictory DAT metadata
        // the fix must refuse rather than silently trust.
        let games = vec![nested_game(
            "Software",
            vec![
                md5_and_sha1_rom("same.bin", 'a', '1'),
                md5_and_sha1_rom("same.bin", 'a', '2'),
            ],
        )];
        let refs = vec![
            nested_ref_with_full_checksums(0, &games[0], 0),
            nested_ref_with_full_checksums(0, &games[0], 1),
        ];

        let resolutions = classify_archive_sets(
            &archive(vec![keyed_member(0, refs)], complete_pass()),
            &[],
            true,
            &games,
            "collection",
        );

        assert_ne!(resolutions[0].state, SetState::Complete);
    }

    #[test]
    fn nested_and_unrelated_top_level_hash_collision_is_ambiguous() {
        let games = vec![
            nested_game("Software", vec![sha1_rom("nested.bin", '1')]),
            game("Unrelated", vec![sha1_rom("flat.bin", '1')]),
        ];
        let nested = nested_ref(0, &games[0], 0);
        let top = DatRomRef {
            game_index: 1,
            game_name: games[1].name.clone(),
            rom_index: 0,
            member_key: DatMemberKey {
                game_index: 1,
                location: MemberLocation::TopLevel { rom_index: 0 },
            },
            rom_name: games[1].roms[0].name.clone(),
            size_bytes: games[1].roms[0].size_bytes,
            checksums: nested.checksums.clone(),
            status: None,
            merge: None,
            content_classification: games[1].content_classification.clone(),
            original_metadata: games[1].original_metadata.clone(),
        };

        let resolutions = classify_archive_sets(
            &archive(vec![keyed_member(0, vec![nested, top])], complete_pass()),
            &[],
            true,
            &games,
            "collection",
        );

        assert_eq!(resolutions.len(), 2);
        assert!(resolutions.iter().all(|resolution| {
            resolution.state == SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution)
        }));
    }

    #[test]
    fn legacy_exact_evidence_without_matched_refs_keeps_top_level_behavior() {
        let games = vec![game("Legacy", vec![rom("legacy.bin", None)])];
        let audit = archive(
            vec![exact_member(0, "legacy.bin", "Legacy", "legacy.bin")],
            complete_pass(),
        );

        let json = serde_json::to_value(&audit).unwrap();
        assert!(json["members"][0].get("matched_refs").is_none());
        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
    }

    #[test]
    fn filename_only_nested_match_never_creates_a_complete_resolution() {
        let games = vec![nested_game("Software", vec![sha1_rom("nested.bin", '1')])];
        let audit = archive(
            vec![DatArchiveMemberAudit {
                evidence: evidence(0, "nested.bin"),
                verdict: Some(AuditVerdict::FilenameOnly {
                    game_name: "Software".to_string(),
                    rom_name: "nested.bin".to_string(),
                }),
                matched_refs: Vec::new(),
            }],
            complete_pass(),
        );

        assert!(classify_archive_sets(&audit, &[], true, &games, "collection").is_empty());
    }

    #[test]
    fn malformed_nested_structure_stays_needs_review_with_keyed_evidence() {
        let mut game = nested_game("Malformed", vec![sha1_rom("nested.bin", '1')]);
        game.unsupported_structure = true;
        let games = vec![game];
        let audit = archive(
            vec![keyed_member(0, vec![nested_ref(0, &games[0], 0)])],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    // -- R2: CRC32-only / filename-only / not-in-DAT never count -----------

    #[test]
    fn weak_or_absent_verdicts_never_count_toward_membership() {
        let games = vec![game("Game (World)", vec![rom("game.bin", None)])];
        let members = vec![
            DatArchiveMemberAudit {
                evidence: evidence(0, "game.bin"),
                verdict: Some(AuditVerdict::Probable {
                    game_name: "Game (World)".to_string(),
                    rom_name: "game.bin".to_string(),
                }),
                matched_refs: Vec::new(),
            },
            DatArchiveMemberAudit {
                evidence: evidence(1, "extra.bin"),
                verdict: Some(AuditVerdict::NotInDat),
                matched_refs: Vec::new(),
            },
            DatArchiveMemberAudit {
                evidence: evidence(2, "unmatched.bin"),
                verdict: None,
                matched_refs: Vec::new(),
            },
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert!(
            resolutions.is_empty(),
            "no strong-hash-exact match exists, so no set may be reported at all"
        );
    }

    // -- Codex hostile-review fixes -----------------------------------------

    #[test]
    fn a_clrmamepro_sourced_set_can_never_become_complete() {
        // ClrMamePro's parser sets `unsupported_structure: true`
        // unconditionally (see the ClrMamePro parser module doc) - this
        // simulates that output directly: a perfectly ordinary, single-ROM,
        // fully-verified game, with only the flag set exactly as that
        // parser would produce it.
        let mut cmp_game = game("Ordinary Game", vec![rom("game.bin", None)]);
        cmp_game.unsupported_structure = true;
        let games = vec![cmp_game];
        let members = vec![exact_member(0, "game.bin", "Ordinary Game", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "a ClrMamePro-sourced set must never reach Complete, however ordinary it looks"
        );
    }

    #[test]
    fn duplicate_game_names_are_never_resolved_by_first_match() {
        // Two DAT entries share a name. Picking the first (array order)
        // would silently bind completeness to whichever one sorts first;
        // Stage 1 must instead refuse the ambiguity outright.
        let games = vec![
            game("Ambiguous Name", vec![rom("first.bin", None)]),
            game("Ambiguous Name", vec![rom("second.bin", None)]),
        ];
        let members = vec![exact_member(0, "first.bin", "Ambiguous Name", "first.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::DuplicateGameName)
        );
        assert!(
            resolutions[0].members_required.is_empty(),
            "an unresolved duplicate name must not borrow either candidate's rom list"
        );
    }

    #[test]
    fn clone_relationship_is_deferred_without_blocking_storage_complete() {
        let mut clone_game = game("Clone (USA)", vec![rom("game.bin", None)]);
        clone_game.clone_of = Some("Parent (World)".to_string());
        let games = vec![clone_game];
        let members = vec![exact_member(0, "game.bin", "Clone (USA)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
    }

    #[test]
    fn sample_relationship_is_deferred_without_blocking_storage_complete() {
        let mut sample_game = game("Game With Samples", vec![rom("game.bin", None)]);
        sample_game.sample_of = Some("samples".to_string());
        let games = vec![sample_game];
        let members = vec![exact_member(0, "game.bin", "Game With Samples", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
    }

    #[test]
    fn all_borrowed_merged_clone_is_storage_complete_and_surfaces_dependency() {
        let mut merged_rom = rom("shared.bin", None);
        merged_rom.merge = Some("parent.bin".to_string());
        let games = vec![game("Merged Set", vec![merged_rom])];
        let members = vec![exact_member(0, "shared.bin", "Merged Set", "shared.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
        assert!(resolutions[0].members_required.is_empty());
        assert_eq!(resolutions[0].members_borrowed, vec!["shared.bin"]);
    }

    #[test]
    fn optional_absence_does_not_make_a_required_set_incomplete() {
        let mut optional = rom("bonus.bin", None);
        optional.optional = Some("yes".to_string());
        let games = vec![game("Optional Set", vec![rom("game.bin", None), optional])];
        let audit = archive(
            vec![exact_member(0, "game.bin", "Optional Set", "game.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_required, vec!["game.bin"]);
        assert!(resolutions[0].members_optional.is_empty());
    }

    #[test]
    fn verified_optional_member_is_surfaced_separately() {
        let mut optional = rom("bonus.bin", None);
        optional.optional = Some("yes".to_string());
        let games = vec![game("Optional Set", vec![rom("game.bin", None), optional])];
        let audit = archive(
            vec![
                exact_member(0, "game.bin", "Optional Set", "game.bin"),
                exact_member(1, "bonus.bin", "Optional Set", "bonus.bin"),
            ],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_verified, vec!["game.bin"]);
        assert_eq!(resolutions[0].members_optional, vec!["bonus.bin"]);
    }

    #[test]
    fn optional_member_cannot_satisfy_a_required_slot_with_the_same_name() {
        let mut optional = rom("same.bin", None);
        optional.optional = Some("yes".to_string());
        let games = vec![game(
            "Duplicate Role",
            vec![rom("same.bin", None), optional],
        )];
        let audit = archive(
            vec![exact_member(0, "same.bin", "Duplicate Role", "same.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    #[test]
    fn split_clone_is_complete_when_unique_rom_is_verified() {
        let mut borrowed = rom("shared.bin", None);
        borrowed.merge = Some("parent.bin".to_string());
        let games = vec![game("Split Clone", vec![rom("unique.bin", None), borrowed])];
        let audit = archive(
            vec![exact_member(0, "unique.bin", "Split Clone", "unique.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_required, vec!["unique.bin"]);
        assert_eq!(resolutions[0].members_borrowed, vec!["shared.bin"]);
    }

    #[test]
    fn nested_dataarea_rom_participates_without_flattening() {
        let mut software = game("Nested Software", Vec::new());
        software.parts.push(DatPartEntry {
            name: Some("cart".to_string()),
            data_areas: vec![DatDataAreaEntry {
                name: Some("prg".to_string()),
                roms: vec![rom("program.bin", None)],
            }],
            ..Default::default()
        });
        let games = vec![software];
        let audit = archive(
            vec![exact_member(
                0,
                "program.bin",
                "Nested Software",
                "program.bin",
            )],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions[0].state, SetState::Complete);
        assert_eq!(resolutions[0].members_required, vec!["program.bin"]);
    }

    #[test]
    fn unknown_loadflag_uses_specific_needs_review_reason() {
        let mut unknown = rom("game.bin", None);
        unknown.loadflag = Some("mystery".to_string());
        let games = vec![game("Unknown Load", vec![unknown])];
        let audit = archive(
            vec![exact_member(0, "game.bin", "Unknown Load", "game.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnknownLoadflag)
        );
    }

    #[test]
    fn non_file_with_merge_uses_specific_contradiction_reason() {
        let mut contradictory = rom("fill.bin", None);
        contradictory.loadflag = Some("fill".to_string());
        contradictory.merge = Some("parent.bin".to_string());
        let games = vec![game("Contradictory", vec![contradictory])];
        let audit = archive(
            vec![exact_member(0, "fill.bin", "Contradictory", "fill.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::ContradictoryMemberFlags)
        );
    }

    #[test]
    fn touched_entry_with_no_declared_members_needs_review() {
        let games = vec![game("Empty Set", Vec::new())];
        let audit = archive(
            vec![exact_member(0, "orphan.bin", "Empty Set", "orphan.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::NoDeclaredMembers)
        );
    }

    #[test]
    fn only_optional_and_non_file_members_need_review() {
        let mut optional = rom("bonus.bin", None);
        optional.optional = Some("yes".to_string());
        let mut fill = rom("", None);
        fill.loadflag = Some("fill".to_string());
        let games = vec![game("Metadata Only", vec![optional, fill])];
        let audit = archive(
            vec![exact_member(0, "bonus.bin", "Metadata Only", "bonus.bin")],
            complete_pass(),
        );

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::OnlyNonFileOrOptionalMembers)
        );
    }

    #[test]
    fn unsupported_and_malformed_software_support_values_fail_closed() {
        for supported in ["no", "partial", "mystery", ""] {
            let mut software = game("Software", vec![rom("game.bin", None)]);
            software.supported = Some(supported.to_string());
            let games = vec![software];
            let audit = archive(
                vec![exact_member(0, "game.bin", "Software", "game.bin")],
                complete_pass(),
            );

            let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

            assert_eq!(
                resolutions[0].state,
                SetState::NeedsReview(NeedsReviewReason::UnsupportedSoftware),
                "supported={supported:?} must fail closed"
            );
        }
    }

    #[test]
    fn whitespace_padded_nodump_status_is_still_recognised() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bonus.bin", Some("  nodump  "))],
        )];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::BadMetadata(BadMetadataReason::NoDump),
            "a whitespace-padded 'nodump' must be trimmed before comparison, not treated as \
             an unrecognised status"
        );
    }

    #[test]
    fn whitespace_padded_baddump_status_is_still_recognised() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bad.bin", Some("\tbaddump\n"))],
        )];
        let members = vec![
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
            exact_member(1, "bad.bin", "Game (World)", "bad.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::BadMetadata(BadMetadataReason::BadDump)
        );
    }

    #[test]
    fn an_unrecognised_status_value_refuses_the_set() {
        // Not "nodump" or "baddump" - some other value this module has never
        // seen (a typo, a DAT dialect extension). Must not be silently
        // assumed ordinary.
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("weird.bin", Some("verified"))],
        )];
        let members = vec![
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
            exact_member(1, "weird.bin", "Game (World)", "weird.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::ContradictoryMemberFlags),
            "an unrecognised status value must fail closed, not be assumed an ordinary rom"
        );
    }

    #[test]
    fn an_empty_status_string_fails_closed() {
        let games = vec![game("Game (World)", vec![rom("game.bin", Some("   "))])];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::ContradictoryMemberFlags)
        );
    }

    #[test]
    fn duplicate_archive_member_index_refuses_the_affected_set() {
        // Two evidence entries claim the same archive-member index. Not
        // reachable from the current ZIP/7z producers, but this function
        // must not trust that blindly.
        let games = vec![game("Game (World)", vec![rom("game.bin", None)])];
        let members = vec![
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::DuplicateArchiveEvidence)
        );
    }

    #[test]
    fn an_ordinary_logiqx_style_rom_only_set_still_becomes_complete() {
        // Positive control: none of the new fail-closed checks should catch
        // a genuinely ordinary, fully-verified, single-source-of-truth set.
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("game (Track 2).bin", None)],
        )];
        let members = vec![
            exact_member(0, "game.bin", "Game (World)", "game.bin"),
            exact_member(
                1,
                "game (Track 2).bin",
                "Game (World)",
                "game (Track 2).bin",
            ),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &[], true, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
    }
}
