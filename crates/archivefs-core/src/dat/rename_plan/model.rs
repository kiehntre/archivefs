//! The typed, read-only DAT rename proposal model.
//!
//! A [`RenameProposal`] is a *plan*: it names a source file, the canonical
//! basename the verified DAT match implies, why that name was chosen, and why
//! the proposal is (or is not) actionable. It is derived purely from
//! already-available audit and policy data plus read-only filesystem metadata.
//!
//! # No state implies a rename happened
//!
//! This module performs no filesystem mutation and never will. `ProposalState`
//! describes whether a proposal *could* be acted on in a future, separately
//! approved apply stage - it never records that a rename occurred, and
//! [`RenameProposal::actionable`] is a statement about eligibility only.
//!
//! # Review decisions are decisions about the proposal only
//!
//! [`ReviewDecision`] records the user's stance on a proposal (accepted for a
//! future review, ignored, needs manual review). Recording one never touches a
//! file. Persistence of these decisions is deliberately out of scope for the
//! first stage - see `docs/design/DAT_RENAME_PLANNING_STAGE1.md`.

use std::path::PathBuf;

use crate::dat::classification::{
    ContentSelectionPolicy, DatContentClassification, DatOriginalMetadata,
};

/// The kind of filesystem object a proposal would, in a future stage,
/// hypothetically rename. Planning itself only ever calls `symlink_metadata`
/// on it and never follows a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceObjectKind {
    RegularFile,
    /// The path is a symlink whose target resolves.
    Symlink,
    /// The path is a symlink that resolves to nothing.
    BrokenSymlink,
}

impl SourceObjectKind {
    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::RegularFile => "regular file",
            Self::Symlink => "symlink",
            Self::BrokenSymlink => "broken symlink",
        }
    }
}

/// The state of one rename proposal.
///
/// Ordering is significant: `Blocked` and `Unsupported` describe the proposal
/// itself, `Ambiguous` describes the match, and `Conflict` describes the
/// destination. Only [`ProposalState::Suggested`] is actionable in a future
/// stage. No variant implies a rename happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalState {
    /// A verified DAT match produced a canonical name that differs from the
    /// current one, and no collision blocks it. The only actionable state.
    Suggested,
    /// The current filename already equals the proposed canonical name.
    AlreadyCanonical,
    /// The policy could not pick a single winner among verified candidates.
    Ambiguous,
    /// A collision blocks the proposal (the target exists, two proposals
    /// collide, or a case-only collision).
    Conflict,
    /// A canonical name cannot be used safely (the DAT entry names an internal
    /// archive member whose extension differs, or the source is a symlink).
    Unsupported,
    /// No canonical name could be derived (path traversal, empty name, or the
    /// source file is no longer present).
    Blocked,
    /// Confidently non-game content is not selected by Games only.
    ExcludedByContentPolicy,
    /// Classification is Unknown, so restrictive mode requires review and
    /// cannot produce an actionable rename.
    UnclassifiedContent,
}

impl ProposalState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Suggested => "Suggested",
            Self::AlreadyCanonical => "Already canonical",
            Self::Ambiguous => "Ambiguous",
            Self::Conflict => "Conflict",
            Self::Unsupported => "Unsupported",
            Self::Blocked => "Blocked",
            Self::ExcludedByContentPolicy => "Not selected by Games only",
            Self::UnclassifiedContent => "Unknown content — review needed",
        }
    }

    /// Whether this state is a candidate for a future apply stage.
    pub fn is_actionable(self) -> bool {
        self == Self::Suggested
    }
}

/// How the proposed name's extension relates to the source file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionStatus {
    /// The DAT entry's extension matches the source file's (case-insensitive);
    /// the file kind is unchanged.
    Preserved,
    /// The DAT entry's extension differs from the source file's. This is why a
    /// proposal can be `Unsupported`: renaming `game.zip` to `game.iso` would
    /// silently change what the file claims to be.
    Changed,
}

/// Why a proposal is blocked by a collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionKind {
    /// A sibling file with the exact proposed name already exists.
    ExistingTarget,
    /// A sibling file exists whose name differs only by case.
    CaseCollision,
    /// Two proposals in the same directory would produce the same name.
    TwoProposalsSameTarget,
    /// Two actionable proposals refer to the same physical source path.
    DuplicateSource,
}

impl CollisionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ExistingTarget => "Target already exists",
            Self::CaseCollision => "Case-only collision",
            Self::TwoProposalsSameTarget => "Two proposals, one target",
            Self::DuplicateSource => "Two proposals, one source",
        }
    }
}

/// One detected collision on a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionInfo {
    pub kind: CollisionKind,
    /// The basename of the colliding object, when one is known.
    pub colliding_with: Option<String>,
    /// Whether the colliding path is a symlink (a future apply stage would
    /// have to decide what a rename over a link even means).
    pub colliding_is_symlink: bool,
    pub detail: String,
}

/// The user's decision about one proposal. A decision is about the proposal
/// only; recording or clearing one never triggers a file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    /// Keep the proposal for a future review/apply stage.
    AcceptedForReview,
    /// The user does not want this proposal.
    Ignored,
    /// The user needs to review it manually.
    NeedsManualReview,
}

impl ReviewDecision {
    pub fn label(self) -> &'static str {
        match self {
            Self::AcceptedForReview => "Accepted for review",
            Self::Ignored => "Ignored",
            Self::NeedsManualReview => "Needs manual review",
        }
    }
}

/// One read-only rename proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameProposal {
    /// The source file's absolute path. Kept for identity and for a future
    /// apply stage; the GUI shows the basename and a shortened parent, never
    /// the raw path in a way that is not the user's own library.
    pub source_path: PathBuf,
    /// The file's current basename.
    pub current_basename: String,
    /// The canonical basename implied by the verified DAT match. `None` when
    /// no safe name could be derived (ambiguous, unsupported, or blocked).
    pub proposed_basename: Option<String>,
    /// The canonical platform id of the audited source, when assigned and
    /// recognised.
    pub platform: Option<String>,
    /// The platform's display name.
    pub platform_display: Option<String>,
    /// The DAT source that produced the verified match.
    pub source_id: String,
    pub source_display_name: String,
    /// The matched catalogue game and ROM names.
    pub game_name: Option<String>,
    pub rom_name: Option<String>,
    /// The audit verdict this proposal rests on ("Exact", "Exact (multiple)").
    pub verdict_label: String,
    /// Whether the match is a cryptographic-hash exact match.
    pub match_confident: bool,
    /// The policy explanations that led here (e.g. "preferred region matched
    /// (Europe)", "source priority 20 outranked source priority 100").
    pub explanations: Vec<String>,
    pub content_policy: ContentSelectionPolicy,
    pub content_classification: DatContentClassification,
    pub original_metadata: DatOriginalMetadata,
    pub state: ProposalState,
    /// What object the proposal describes on disk.
    pub object_kind: SourceObjectKind,
    /// Why the policy could not decide, when ambiguous.
    pub ambiguity_reason: Option<String>,
    pub collision: Option<CollisionInfo>,
    /// Reasons the proposal is blocked (path traversal, empty name, missing
    /// source, unreadable directory, …).
    pub blockers: Vec<String>,
    /// Whether the proposed name preserves the source file's extension.
    pub extension_status: Option<ExtensionStatus>,
    /// What, if anything, was replaced in the canonical name to make it safe
    /// on this filesystem, in deterministic order.
    pub sanitisation_notes: Vec<String>,
    /// Whether a future apply stage could act on this proposal. True only for
    /// `Suggested` proposals with no collision.
    pub actionable: bool,
    /// Filesystem identity captured for the exact outer archive whose members
    /// were audited. `None` for ordinary loose-file proposals.
    pub audited_identity: Option<crate::dat::rename_apply::ObjectIdentity>,
    /// Whether this proposal renames an outer `.zip`/`.7z` archive as a
    /// whole, derived from Stage 1 set-completeness evidence
    /// ([`crate::dat::set::SetResolution`]) rather than a per-file DAT
    /// match. `false` for every ordinary loose-file proposal. The apply
    /// machinery treats both identically - a rename is a rename of
    /// whatever regular file `source_path` names - this field exists only
    /// so a consumer (the GUI, tests) can tell the two provenances apart
    /// without inferring it from the absence of `rom_name`.
    pub is_outer_archive: bool,
}

impl RenameProposal {
    /// The one-line headline for a proposal row: current → proposed.
    pub fn headline(&self) -> String {
        match &self.proposed_basename {
            Some(proposed) if proposed != &self.current_basename => {
                format!("{} → {}", self.current_basename, proposed)
            }
            _ => self.current_basename.clone(),
        }
    }
}

/// Counts of a plan by proposal state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenamePlanCounts {
    pub suggested: usize,
    pub already_canonical: usize,
    pub ambiguous: usize,
    pub conflicts: usize,
    pub unsupported: usize,
    pub blocked: usize,
    pub excluded_by_content_policy: usize,
    pub unclassified_content: usize,
    pub total: usize,
}

impl RenamePlanCounts {
    pub fn from_proposals(proposals: &[RenameProposal]) -> Self {
        let mut counts = Self {
            total: proposals.len(),
            ..Default::default()
        };
        for proposal in proposals {
            match proposal.state {
                ProposalState::Suggested => counts.suggested += 1,
                ProposalState::AlreadyCanonical => counts.already_canonical += 1,
                ProposalState::Ambiguous => counts.ambiguous += 1,
                ProposalState::Conflict => counts.conflicts += 1,
                ProposalState::Unsupported => counts.unsupported += 1,
                ProposalState::Blocked => counts.blocked += 1,
                ProposalState::ExcludedByContentPolicy => counts.excluded_by_content_policy += 1,
                ProposalState::UnclassifiedContent => counts.unclassified_content += 1,
            }
        }
        counts
    }
}

/// The full read-only rename plan for one audit's verified matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    /// The audit generation this plan was built from. A caller must reject a
    /// plan whose generation no longer matches the current audit's, so a stale
    /// plan can never replace a newer one.
    pub generation: u64,
    pub source_id: String,
    pub source_display_name: String,
    /// The folder that was audited and planned.
    pub scan_root: String,
    pub platform: Option<String>,
    pub platform_display: Option<String>,
    pub content_policy: ContentSelectionPolicy,
    pub classifier_version: String,
    /// Deterministic order: by source path, then proposed name.
    pub proposals: Vec<RenameProposal>,
    pub counts: RenamePlanCounts,
    /// Every file the audit compared, for coverage context.
    pub audited_total: usize,
    /// Files whose cryptographic hash matched a catalogue entry; these are the
    /// ones that produced a proposal.
    pub verified_total: usize,
    /// The audit hit a ceiling, so this plan covers part of the folder.
    pub truncated: bool,
}
