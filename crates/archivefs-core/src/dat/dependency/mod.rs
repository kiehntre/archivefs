//! Read-only, fail-closed dependency vocabulary for set completeness (Stage 2d).
//!
//! Stage 2c ([`crate::dat::set`]) answers one question: *are this set's own
//! declared physical storage members present?* It deliberately excludes
//! borrowed members from `members_required`, surfaces a CHD's
//! `parent_required()` without chasing it, and never looks at `cloneof`,
//! `romof`, `device_ref`, `biosset`, or `sampleof` at all.
//!
//! Stage 2d answers the separate question: *are this set's required dependency
//! relationships satisfied, without ever manufacturing a false `Complete`?*
//!
//! # The two layers never merge
//!
//! ```text
//! direct storage state  (Stage 2c, per archive)
//!   +  dependency state (Stage 2d, per collection)
//!   =  final SetState
//! ```
//!
//! The combine is [`apply_dependency_state`] and it is **downgrade-only**: a
//! set that Stage 2c did not call `Complete` is returned unchanged, so no
//! amount of dependency success can ever promote an `Incomplete`,
//! `BadMetadata`, or `NeedsReview` storage verdict. That direction is asserted
//! exhaustively in this module's tests and is the single most important
//! property of the whole stage.
//!
//! # Distinct concepts stay distinct
//!
//! The eight [`DependencyKind`]s exist because these relationships overlap in
//! practice but are *not* the same statement, and collapsing any pair of them
//! is a documented route to a false `Complete`:
//!
//! - `cloneof` is a hierarchy claim; `romof` is a ROM-source claim. A set may
//!   do either, both (at the same or different targets), or neither. Neither
//!   is ever synthesised from the other.
//! - A `merge=` attribute is a *member-level* borrow of one named declaration
//!   in the provider set. It is never satisfied by an unrelated same-named
//!   member elsewhere in the catalogue.
//! - A BIOS relationship is storage provision. It is **not** a claim that any
//!   particular runtime BIOS selection is runnable; see
//!   [`BIOS_RUNTIME_SELECTION_NOT_MODELLED`].
//! - `device_ref` is a device requirement, not ROM borrowing.
//! - Samples live in their own namespace and are never satisfied by a ROM.
//! - A CHD header's `parent_sha1` is a format-level delta dependency. It is
//!   **not** the DAT's `disk merge=`, and the two are resolved independently.
//!
//! # Fail-closed is the whole design
//!
//! Every resolution answers with a [`DependencyOutcome`], and only
//! [`DependencyOutcome::Satisfied`] permits `Complete`. Missing, ambiguous,
//! cyclic, contradictory, unsupported, and not-yet-observable all block it.
//! False `Incomplete` is an accepted cost; false `Complete` is not.
//!
//! # Interaction with the known disk-only-set emission gap
//!
//! [`crate::dat::set::classify_archive_sets`] still seeds set membership from
//! ROM verdicts only, so a catalogue entry declaring `<disk>`s and no `<rom>`s
//! is never emitted as a [`SetResolution`] at all. That gap is inherited
//! here, and this stage deliberately does not paper over it: a set that is
//! never emitted is never dependency-resolved either, which is silent absence
//! rather than a false verdict - exactly the behaviour an untouched ROM-only
//! set already has.
//!
//! Two consequences worth stating, because they are easy to misread as bugs:
//!
//! - A **disk-only provider** can still satisfy a dependency. Resolution
//!   consumes [`crate::dat::disk_audit`] evidence for the whole run, not the
//!   emitted resolution list, so a borrowed disk or a parent CHD living in a
//!   disk-only set resolves normally. Only the provider's *own* resolution is
//!   missing from the output.
//! - Closing the gap later is additive. Disk-seeded membership would emit more
//!   [`SetResolution`]s; each would flow through this stage unchanged, because
//!   nothing here reads the resolution list to decide what evidence exists.
//!   No dependency rule needs revisiting when that follow-up lands.

use serde::Serialize;

use super::set::{NeedsReviewReason, SetState};

pub mod graph;
pub mod resolve;

#[cfg(test)]
mod tests;

/// Runtime BIOS selection is deliberately not modelled by this stage.
///
/// MAME has two disagreeing BIOS standards: `-verifyroms` requires every
/// declared BIOS variant's ROMs, while a run requires only the *selected*
/// variant's. Stage 2d resolves the first (does the BIOS storage dependency
/// exist?) and makes no claim about the second, because nothing in the current
/// architecture models a selected BIOS. Consequently no state produced by this
/// module may be read as "this runs" - `Complete` means storage plus resolved
/// dependencies, never runnability.
pub const BIOS_RUNTIME_SELECTION_NOT_MODELLED: bool = true;

/// Which kind of relationship a requirement expresses.
///
/// These are kept separate on purpose. They frequently coincide in a
/// well-formed MAME catalogue, which is exactly why collapsing them is
/// tempting and wrong: each one fails differently, and a resolver that treats
/// `cloneof` as `romof` (or `disk merge=` as a CHD parent) will report a set
/// complete on evidence that never addressed the real requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// `<game cloneof="...">` - a hierarchy statement. It asserts the parent
    /// exists in the catalogue; it does not by itself demand any storage.
    ParentSet,
    /// `<game romof="...">` - the set that ROM content may be borrowed from.
    /// Independent of [`DependencyKind::ParentSet`] in both directions.
    RomSource,
    /// `<rom merge="...">` - one named declaration in the provider set whose
    /// content this member reuses.
    MergedRom,
    /// `<disk merge="...">` - the disk equivalent. Never conflated with
    /// [`DependencyKind::ChdParent`].
    MergedDisk,
    /// A BIOS provider set, or a `bios=`/`<biosset>` declaration pairing.
    /// Storage provision only.
    Bios,
    /// `<device_ref name="...">` - a required device, resolved transitively.
    Device,
    /// `<game sampleof="...">` or a `<sample>` list. Its own namespace.
    Sample,
    /// A CHD v5 header's non-zero `parent_sha1`: a delta image that cannot be
    /// used without the parent image's content.
    ChdParent,
}

impl DependencyKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ParentSet => "parent set",
            Self::RomSource => "ROM source set",
            Self::MergedRom => "merged ROM",
            Self::MergedDisk => "merged disk",
            Self::Bios => "BIOS",
            Self::Device => "device",
            Self::Sample => "sample",
            Self::ChdParent => "parent CHD",
        }
    }
}

/// What a requirement points at.
///
/// Every variant names its target by *declaration* identity - a catalogue set
/// name plus, where relevant, the declared member name inside that set, or a
/// CHD's own header SHA-1. No variant is a filesystem path or a display name
/// used as identity: a same-named file in an unrelated set can never resolve
/// any of these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyTarget {
    /// A whole catalogue set, by its DAT `<game name>`.
    Set { name: String },
    /// One declared member inside a named catalogue set.
    SetMember {
        set_name: String,
        member_name: String,
    },
    /// A named `<biosset>` belonging to a named set.
    BiosSet { set_name: String, bios_set: String },
    /// A sample set name (`sampleof`, or the set's own name).
    SampleSet { name: String },
    /// A CHD image identified by its header overall SHA-1. Never `raw_sha1`,
    /// which identifies the internal logical stream and is not a catalogue
    /// identity, and never a filename.
    ChdIdentity { overall_sha1: String },
    /// The DAT declared a relationship but named no usable target - an empty,
    /// whitespace-only, or absent name. Recorded rather than dropped, because
    /// silently ignoring a malformed declaration is indistinguishable from
    /// there being no requirement at all.
    Undeclared,
}

/// The verdict for one requirement.
///
/// Ordered weakest-blocking to strongest-blocking for roll-up purposes; see
/// [`DependencyState::roll_up`]. Only [`DependencyOutcome::Satisfied`] permits
/// a `Complete` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyOutcome {
    /// Proven: the required declaration resolved uniquely and the content it
    /// names was verified present by this run's evidence.
    Satisfied,
    /// Provably absent: the target resolved uniquely and its content was not
    /// verified anywhere in the scanned collection.
    Missing,
    /// Two or more candidate targets, and no non-positional way to choose.
    /// Never resolved by taking the first.
    Ambiguous,
    /// The relationship chain revisited a set already on the current path.
    Cycle,
    /// The declaration contradicts itself or the catalogue: a self-dependency,
    /// a `merge=` whose target set declares no such member, a `bios=` naming
    /// no declared `<biosset>`, a CHD naming itself as its own parent, or a
    /// merge target whose declared checksum disagrees with the borrower's.
    Contradictory,
    /// A real dependency this stage cannot resolve, because no evidence
    /// channel for it exists yet. Distinct from `Missing`: absence was never
    /// established, only unobservability.
    Unsupported,
    /// The scan that produced the evidence did not finish, so a negative
    /// result cannot be trusted. Only ever *replaces* `Missing` - a positive
    /// verification stays positive, because finding something is still proof
    /// under a partial scan.
    EvidenceUnavailable,
}

impl DependencyOutcome {
    /// Whether this outcome permits the set to remain `Complete`.
    pub fn permits_complete(self) -> bool {
        matches!(self, Self::Satisfied)
    }
}

/// One resolved dependency relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyRequirement {
    pub kind: DependencyKind,
    pub target: DependencyTarget,
    pub outcome: DependencyOutcome,
    /// The declaring member's own name, when the requirement came from a
    /// member rather than from the set header. Provenance for reporting only -
    /// never used to resolve anything.
    pub via_member: Option<String>,
}

/// The roll-up of every requirement belonging to one set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyState {
    /// The set declares no dependency relationships at all. This is the
    /// ordinary case for flat catalogues (No-Intro, Redump, TOSEC) and is the
    /// only non-`Satisfied` state that permits `Complete`.
    NotApplicable,
    /// Dependency resolution never ran for this set, because Stage 2c could
    /// not identify which catalogue entry it is (a duplicated `<game name>`).
    /// Deliberately does **not** permit `Complete`: the only reason it is
    /// currently unreachable in a `Complete` set is that such a set is
    /// already `NeedsReview`, and this stage does not rely on that coupling
    /// holding forever.
    NotEvaluated,
    Satisfied,
    Missing,
    Ambiguous,
    Cycle,
    Contradictory,
    Unsupported,
    EvidenceUnavailable,
}

impl DependencyState {
    /// Whether this state permits the set to remain `Complete`.
    pub fn permits_complete(self) -> bool {
        matches!(self, Self::NotApplicable | Self::Satisfied)
    }

    /// Rolls a set's requirement outcomes into one state.
    ///
    /// Structural problems outrank absences deliberately: when a catalogue is
    /// self-contradictory or cyclic, any "this is missing" conclusion drawn
    /// through it was computed over declarations that cannot be trusted, so
    /// reporting the structural fault is both more accurate and more
    /// actionable. Every non-`Satisfied` outcome blocks `Complete` either way,
    /// so this ordering only decides *which* reason is surfaced, never
    /// whether the set is allowed through.
    pub fn roll_up<'a>(outcomes: impl IntoIterator<Item = &'a DependencyOutcome>) -> Self {
        let mut seen_any = false;
        let mut worst: Option<DependencyOutcome> = None;
        for outcome in outcomes {
            seen_any = true;
            let rank = severity(*outcome);
            if worst.is_none_or(|current| rank > severity(current)) {
                worst = Some(*outcome);
            }
        }
        if !seen_any {
            return Self::NotApplicable;
        }
        match worst {
            None | Some(DependencyOutcome::Satisfied) => Self::Satisfied,
            Some(DependencyOutcome::Missing) => Self::Missing,
            Some(DependencyOutcome::Ambiguous) => Self::Ambiguous,
            Some(DependencyOutcome::Cycle) => Self::Cycle,
            Some(DependencyOutcome::Contradictory) => Self::Contradictory,
            Some(DependencyOutcome::Unsupported) => Self::Unsupported,
            Some(DependencyOutcome::EvidenceUnavailable) => Self::EvidenceUnavailable,
        }
    }
}

/// Blocking severity, highest wins. See [`DependencyState::roll_up`].
fn severity(outcome: DependencyOutcome) -> u8 {
    match outcome {
        DependencyOutcome::Satisfied => 0,
        DependencyOutcome::Missing => 1,
        DependencyOutcome::EvidenceUnavailable => 2,
        DependencyOutcome::Unsupported => 3,
        DependencyOutcome::Ambiguous => 4,
        DependencyOutcome::Cycle => 5,
        DependencyOutcome::Contradictory => 6,
    }
}

/// One set's full dependency resolution.
///
/// Deliberately not a single boolean: a consumer (a future Repair Center, a
/// GUI rollup) has to be able to say *which* parent set, BIOS, device, sample
/// set, or parent CHD is missing, and to distinguish "provably missing" from
/// "the catalogue is ambiguous here" from "this stage cannot see that yet".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetDependencyReport {
    pub state: DependencyState,
    /// Every relationship considered, in a deterministic order fixed by the
    /// DAT's own declaration order - never by archive enumeration order.
    pub requirements: Vec<DependencyRequirement>,
}

impl SetDependencyReport {
    /// The report for a set whose catalogue entry could not be identified, so
    /// nothing was resolved. See [`DependencyState::NotEvaluated`].
    pub fn not_evaluated() -> Self {
        Self {
            state: DependencyState::NotEvaluated,
            requirements: Vec::new(),
        }
    }

    /// Builds a report from resolved requirements, rolling up their outcomes.
    pub fn from_requirements(requirements: Vec<DependencyRequirement>) -> Self {
        let state = DependencyState::roll_up(requirements.iter().map(|entry| &entry.outcome));
        Self {
            state,
            requirements,
        }
    }

    /// The requirements that blocked `Complete`, for reporting.
    pub fn blocking(&self) -> impl Iterator<Item = &DependencyRequirement> {
        self.requirements
            .iter()
            .filter(|entry| !entry.outcome.permits_complete())
    }
}

/// Folds a dependency state into a storage state, downgrade-only.
///
/// # The one invariant
///
/// The result is `Complete` **only** when `storage` was already `Complete`
/// *and* the dependency state permits it. There is no input pair for which
/// this function upgrades a set, which is why every non-`Complete` storage
/// state returns unchanged before the dependency state is even examined.
/// Dependency resolution can preserve or downgrade a verdict; it can never
/// manufacture one.
///
/// Storage states other than `Complete` are returned untouched rather than
/// being merged with the dependency reason, because Stage 2c's reason is the
/// more fundamental one: a set whose own required members are missing is not
/// made more informative by also learning its BIOS is absent, and overwriting
/// `BadMetadata(NoDump)` with a dependency reason would lose a fact the
/// catalogue stated outright.
pub fn apply_dependency_state(storage: SetState, dependency: DependencyState) -> SetState {
    if !matches!(storage, SetState::Complete) {
        return storage;
    }
    match dependency {
        DependencyState::NotApplicable | DependencyState::Satisfied => SetState::Complete,
        DependencyState::NotEvaluated => {
            SetState::NeedsReview(NeedsReviewReason::DependencyEvidenceIncomplete)
        }
        DependencyState::Missing => SetState::Incomplete,
        DependencyState::Ambiguous => SetState::NeedsReview(NeedsReviewReason::AmbiguousDependency),
        DependencyState::Cycle => SetState::NeedsReview(NeedsReviewReason::DependencyCycle),
        DependencyState::Contradictory => {
            SetState::NeedsReview(NeedsReviewReason::ContradictoryDependencyMetadata)
        }
        DependencyState::Unsupported => {
            SetState::NeedsReview(NeedsReviewReason::UnsupportedDependencyStructure)
        }
        DependencyState::EvidenceUnavailable => {
            SetState::NeedsReview(NeedsReviewReason::DependencyEvidenceIncomplete)
        }
    }
}
