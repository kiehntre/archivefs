//! Read-only, format-agnostic DAT set-completeness classification (Stage 1).
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
//! # Scope (Stage 1)
//!
//! Implements the "minimum safe completeness rules" from
//! `docs/research/SET_COMPLETENESS_MILESTONE_RESEARCH.md` §2:
//!
//! - **R1 — membership comes only from the DAT.** A set is emitted only when
//!   at least one of its `<rom>`s was matched by a member's verdict. Nothing
//!   is grouped by filename, basename, or directory.
//! - **R2 — only a single-candidate cryptographic match counts as verified.**
//!   [`AuditVerdict::Exact`] counts; [`AuditVerdict::ExactMultipleCandidates`]
//!   cannot be attributed to one set, so every named candidate set is marked
//!   [`NeedsReviewReason::AmbiguousMemberAttribution`] instead. CRC32-only,
//!   filename-only, and unmatched members never count.
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
//! - **R5 — structurally unsupported sets fail closed.** [`SetState::Complete`]
//!   is allowed only for parser shapes Stage 1 explicitly understands.
//!   `<part>` and `<dataarea>` are not parsed by this codebase, and every
//!   structural/relationship signal Stage 1 does track is a provenance
//!   marker only, never interpreted (see
//!   [`crate::dat::model::DatGameEntry::unsupported_structure`] and
//!   [`crate::dat::model::DatRomEntry::loadflag`]). A set is refused into
//!   [`NeedsReviewReason::UnsupportedSetStructure`], rather than guessed at,
//!   when its DAT entry: sets `unsupported_structure` (Logiqx: a `<disk>`,
//!   `<sample>`, `<part>`, `<dataarea>`, or device/dependency-style child was
//!   detected; ClrMamePro: *unconditionally*, on every entry - see below);
//!   declares `clone_of` or `sample_of` (a clone/parent relationship Stage 1
//!   does not implement merge-mode semantics for); contains a `<rom>` with no
//!   name, no declared size, or no hash; contains a `<rom>` carrying any
//!   `loadflag` value at all (not just `fill`/`reload` - this codebase
//!   cannot distinguish a physical-content loadflag from a non-physical one,
//!   so every value is treated the same, conservatively); contains a `<rom>`
//!   with a `merge` reference (it belongs to another set's rename group, a
//!   relationship this module does not model); contains a `<rom>` whose
//!   trimmed, case-insensitive `status` is a non-empty value that is neither
//!   `nodump` nor `baddump` (an unrecognised status is never assumed
//!   ordinary); or declares the same rom name more than once (a single
//!   verified member could otherwise satisfy two distinct required slots).
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
//!   aggregation is explicitly out of Stage 1.
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
//! # What Stage 1 deliberately does not attempt
//!
//! - Any change to [`crate::dat::archive`], ZIP/7z sources, or the archive
//!   evidence shape.
//! - MAME set verification (gated on the parser work R5 refuses around).
//! - Any member-level or inner-archive rename. The only rename consumer is the
//!   separately gated outer-archive proposal described above.
//! - Clone/parent merge-mode semantics, BIOS dependency tracking, multi-disc
//!   or game-scope aggregation, and CHD-reconstruction completeness.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::archive::ArchivePassCompletion;
use super::audit::AuditVerdict;
use super::model::{DatGameEntry, DatRomEntry};
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
    /// The set's DAT entry contains a rom with no name, no declared size, or
    /// no hash at all - the honest Stage 1 fallback for set shapes this
    /// parser cannot see fully yet (R5).
    UnsupportedSetStructure,
    /// The archive's own pass did not finish examining every member, so some
    /// member this set might need was never actually checked (R8).
    PartialArchivePass,
    /// The touched `game_name` is not unique in the DAT: two or more entries
    /// share it, and Stage 1 has no durable, non-positional way to tell them
    /// apart (`SetIdentity` is deliberately never a positional index).
    /// Neither/none of the ambiguous candidates is resolved.
    DuplicateGameName,
    /// The same archive-member index appeared more than once in one
    /// archive's evidence. Not reachable from the current ZIP/7z producers
    /// (each enumerates members once, by construction) - this is a defensive
    /// check against a future or malformed producer, not a live case.
    DuplicateArchiveEvidence,
}

/// One catalogue set's Stage 1 completeness state.
///
/// `Complete` is deliberately the least reachable state: every other variant
/// is what a mixed or partial result degrades to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetState {
    /// Every required (non-`nodump`, non-`baddump`) rom was verified present,
    /// no `nodump` rom exists in the entry, no `baddump` rom exists in the
    /// entry (matched or not), no member's attribution was ambiguous, and
    /// the archive pass completed.
    Complete,
    /// At least one required rom is absent or was not verified, and nothing
    /// else disqualifies the set outright.
    Incomplete,
    BadMetadata(BadMetadataReason),
    NeedsReview(NeedsReviewReason),
}

/// One `<rom>` that flagged `BadMetadata`, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetBadMember {
    pub rom_name: String,
    pub reason: BadMetadataReason,
}

/// One catalogue set's Stage 1 resolution, scoped to a single archive (R7).
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
}

#[derive(Default)]
struct TouchedSet {
    verified_rom_names: HashSet<String>,
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
        match &member.verdict {
            Some(AuditVerdict::Exact {
                game_name,
                rom_name,
                ..
            }) => {
                let set = touched.entry(game_name.clone()).or_default();
                set.verified_rom_names.insert(rom_name.clone());
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
        let matching_games: Vec<&DatGameEntry> = games
            .iter()
            .filter(|candidate| candidate.name == game_name)
            .collect();
        let game = match matching_games.as_slice() {
            // The verdict named a game that isn't in our own game list. Both
            // come from the same DatIndex build in every real caller; fail
            // closed on the mismatch rather than guess or panic.
            [] => continue,
            [only] => *only,
            _ => {
                resolutions.push(SetResolution {
                    identity,
                    archive_path: archive.archive_path.clone(),
                    state: SetState::NeedsReview(NeedsReviewReason::DuplicateGameName),
                    members_required: Vec::new(),
                    members_verified: Vec::new(),
                    members_bad: Vec::new(),
                });
                continue;
            }
        };

        // R5 / item 3 / item 6: a rom with no name, no declared size, or no
        // hash at all means this set's true membership cannot be reasoned
        // about honestly. `unsupported_structure` and `loadflag` are
        // provenance-only markers (see their doc comments on
        // `DatGameEntry`/`DatRomEntry`): neither is interpreted here beyond
        // "this entry has structure Stage 1 does not safely understand". A
        // duplicate required rom name is also refused here rather than
        // risked (one verified member could otherwise satisfy two distinct
        // DAT entries that happen to share a name), as is any clone/parent
        // relationship (`clone_of`/`sample_of`) or `merge` reference - Stage
        // 1 implements none of MAME's merge-mode semantics, so a rom that
        // belongs to another set's rename group must not be silently
        // treated as an ordinary standalone rom - and any rom whose
        // `status` is a non-empty value this module does not recognise
        // (trimmed, case-insensitive) at all.
        let unsupported_structure = game.unsupported_structure
            || game.clone_of.is_some()
            || game.sample_of.is_some()
            || game.roms.iter().any(|rom| {
                rom.name.trim().is_empty()
                    || rom.size_bytes.is_none()
                    || rom.checksums().is_empty()
                    || rom.loadflag.is_some()
                    || rom.merge.is_some()
                    || has_unrecognised_status(rom)
            })
            || has_duplicate_rom_names(game);
        if unsupported_structure {
            resolutions.push(SetResolution {
                identity,
                archive_path: archive.archive_path.clone(),
                state: SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
                members_required: Vec::new(),
                members_verified: Vec::new(),
                members_bad: Vec::new(),
            });
            continue;
        }

        // Item 7: the evidence itself is not trustworthy for this set,
        // independent of what any individual verdict claims.
        if touch.duplicate_evidence {
            resolutions.push(SetResolution {
                identity,
                archive_path: archive.archive_path.clone(),
                state: SetState::NeedsReview(NeedsReviewReason::DuplicateArchiveEvidence),
                members_required: required_rom_names(game),
                members_verified: verified_required_names(game, &touch.verified_rom_names),
                members_bad: Vec::new(),
            });
            continue;
        }

        if touch.ambiguous {
            resolutions.push(SetResolution {
                identity,
                archive_path: archive.archive_path.clone(),
                state: SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution),
                members_required: required_rom_names(game),
                members_verified: verified_required_names(game, &touch.verified_rom_names),
                members_bad: Vec::new(),
            });
            continue;
        }

        let mut members_required = Vec::new();
        let mut members_bad = Vec::new();
        let mut all_required_present = true;

        for rom in &game.roms {
            // Item 6: trimmed before comparison. By this point
            // `has_unrecognised_status` above has already refused the whole
            // set if any rom's trimmed status were anything other than
            // empty/absent/nodump/baddump, so only those four shapes can
            // reach here.
            match rom.status.as_deref().map(str::trim) {
                Some(status) if status.eq_ignore_ascii_case("nodump") => {
                    members_bad.push(SetBadMember {
                        rom_name: rom.name.clone(),
                        reason: BadMetadataReason::NoDump,
                    });
                }
                Some(status) if status.eq_ignore_ascii_case("baddump") => {
                    // Conservative Stage 1 rule: any baddump rom the DAT
                    // lists for this set blocks Complete, matched or not.
                    // The catalogue already knows this content is bad; an
                    // archive that simply never contains a member for it is
                    // not thereby "more complete" than one that does. Still
                    // excluded from `members_required` (R4's note) - its
                    // absence alone is not "incomplete", it is bad metadata.
                    members_bad.push(SetBadMember {
                        rom_name: rom.name.clone(),
                        reason: BadMetadataReason::BadDump,
                    });
                }
                _ => {
                    members_required.push(rom.name.clone());
                    if !touch.verified_rom_names.contains(&rom.name) {
                        all_required_present = false;
                    }
                }
            }
        }

        let members_verified: Vec<String> = members_required
            .iter()
            .filter(|name| touch.verified_rom_names.contains(*name))
            .cloned()
            .collect();

        let has_nodump = members_bad
            .iter()
            .any(|bad| bad.reason == BadMetadataReason::NoDump);
        let has_baddump = members_bad
            .iter()
            .any(|bad| bad.reason == BadMetadataReason::BadDump);

        // Order matches the research's state machine (§4) exactly: a partial
        // pass is checked first because it can invalidate confidence in
        // every later check, not just "is a member missing".
        let state = if !archive_pass_complete {
            SetState::NeedsReview(NeedsReviewReason::PartialArchivePass)
        } else if has_nodump {
            SetState::BadMetadata(BadMetadataReason::NoDump)
        } else if has_baddump {
            SetState::BadMetadata(BadMetadataReason::BadDump)
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
        });
    }
    resolutions
}

/// Whether `game` declares the same rom name more than once.
///
/// A single verified archive member is keyed by DAT rom name (see the module
/// doc); if the DAT itself lists a name twice, one member matching that name
/// would otherwise silently satisfy both slots, even when the archive only
/// truly contains one of the two "required" copies. Treated as unsupported
/// structure (R5) rather than guessed at - this is malformed/unusual DAT
/// data, not a shape Stage 1 has a safe answer for.
fn has_duplicate_rom_names(game: &DatGameEntry) -> bool {
    let mut seen = HashSet::with_capacity(game.roms.len());
    game.roms.iter().any(|rom| !seen.insert(rom.name.as_str()))
}

/// Whether `rom`'s status (trimmed, case-insensitive) is a non-empty value
/// this module does not recognise.
///
/// Item 6: an absent status and an empty-after-trim status are both treated
/// as "ordinary" (no metadata claim at all), exactly as before. `nodump` and
/// `baddump` are the only non-empty values Stage 1 understands. Anything
/// else - a typo, a value from a DAT dialect this codebase has never seen,
/// deliberately malformed input - is not silently assumed ordinary; the
/// whole set fails closed into `UnsupportedSetStructure` instead.
fn has_unrecognised_status(rom: &DatRomEntry) -> bool {
    match rom.status.as_deref().map(str::trim) {
        None => false,
        Some(status) => {
            !status.is_empty()
                && !status.eq_ignore_ascii_case("nodump")
                && !status.eq_ignore_ascii_case("baddump")
        }
    }
}

/// Rom names required for `Complete` (excludes `nodump`/`baddump`), DAT order.
fn required_rom_names(game: &DatGameEntry) -> Vec<String> {
    game.roms
        .iter()
        .filter(|rom| {
            !rom.status.as_deref().is_some_and(|status| {
                status.eq_ignore_ascii_case("nodump") || status.eq_ignore_ascii_case("baddump")
            })
        })
        .map(|rom| rom.name.clone())
        .collect()
}

fn verified_required_names(game: &DatGameEntry, verified: &HashSet<String>) -> Vec<String> {
    required_rom_names(game)
        .into_iter()
        .filter(|name| verified.contains(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::{
        ArchiveMemberEvidence, ArchiveMemberHashes, ArchiveMemberStatus, ArchivePassStopReason,
    };
    use crate::dat::model::DatRomEntry;
    use crate::dat::sources::audit_run::DatArchiveMemberAudit;

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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
        }];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    #[test]
    fn a_game_with_disk_entries_can_never_become_complete() {
        // Every visible <rom> is genuinely present and verified; the entry
        // also had <disk> children the parser could not represent. Even
        // though `roms` alone looks completely satisfied, the set must not
        // be reported Complete - `roms` is known to not be the whole
        // picture for this entry.
        let mut disc_game = game("Disc Game (World)", vec![rom("game.cue", None)]);
        disc_game.unsupported_structure = true;
        let games = vec![disc_game];
        let members = vec![exact_member(0, "game.cue", "Disc Game (World)", "game.cue")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "a DAT entry with <disk> children must never reach Complete from its <rom>s alone"
        );
    }

    #[test]
    fn a_rom_with_any_loadflag_value_can_never_become_complete() {
        // Deliberately not "fill" or "reload" specifically: this codebase
        // cannot tell a non-physical loadflag from a physical one, so every
        // value is treated the same, conservatively. The rom otherwise has
        // full, well-formed name/size/hash metadata and IS verified present
        // - only the loadflag marks it as not an ordinary physical ROM.
        let mut fill_rom = rom("fill.bin", None);
        fill_rom.loadflag = Some("fill".to_string());
        let games = vec![game("mame-set", vec![fill_rom, rom("gfx.bin", None)])];
        let members = vec![
            exact_member(0, "fill.bin", "mame-set", "fill.bin"),
            exact_member(1, "gfx.bin", "mame-set", "gfx.bin"),
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "a rom with any loadflag value must never reach Complete, even when it has \
             complete-looking name/size/hash metadata and was itself verified present"
        );
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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].identity.game_name, "Game (World)");
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
            },
            DatArchiveMemberAudit {
                evidence: evidence(1, "extra.bin"),
                verdict: Some(AuditVerdict::NotInDat),
            },
            DatArchiveMemberAudit {
                evidence: evidence(2, "unmatched.bin"),
                verdict: None,
            },
        ];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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
    fn a_clone_of_relationship_refuses_the_set() {
        let mut clone_game = game("Clone (USA)", vec![rom("game.bin", None)]);
        clone_game.clone_of = Some("Parent (World)".to_string());
        let games = vec![clone_game];
        let members = vec![exact_member(0, "game.bin", "Clone (USA)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "Stage 1 implements no clone/parent merge-mode semantics"
        );
    }

    #[test]
    fn a_sample_of_relationship_refuses_the_set() {
        let mut sample_game = game("Game With Samples", vec![rom("game.bin", None)]);
        sample_game.sample_of = Some("samples".to_string());
        let games = vec![sample_game];
        let members = vec![exact_member(0, "game.bin", "Game With Samples", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure)
        );
    }

    #[test]
    fn a_rom_merge_reference_refuses_the_set() {
        let mut merged_rom = rom("shared.bin", None);
        merged_rom.merge = Some("parent.bin".to_string());
        let games = vec![game("Merged Set", vec![merged_rom])];
        let members = vec![exact_member(0, "shared.bin", "Merged Set", "shared.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "a rom belonging to another set's rename group must not be treated as ordinary"
        );
    }

    #[test]
    fn whitespace_padded_nodump_status_is_still_recognised() {
        let games = vec![game(
            "Game (World)",
            vec![rom("game.bin", None), rom("bonus.bin", Some("  nodump  "))],
        )];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            "an unrecognised status value must fail closed, not be assumed an ordinary rom"
        );
    }

    #[test]
    fn an_empty_status_string_is_treated_as_ordinary() {
        // Contrast with the test above: empty-after-trim is "no status
        // claim at all", same as an absent status - not unrecognised.
        let games = vec![game("Game (World)", vec![rom("game.bin", Some("   "))])];
        let members = vec![exact_member(0, "game.bin", "Game (World)", "game.bin")];
        let audit = archive(members, complete_pass());

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

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

        let resolutions = classify_archive_sets(&audit, &games, "collection");

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].state, SetState::Complete);
    }
}
