//! The typed Repair Center proposal vocabulary.
//!
//! A [`RepairProposal`] is a *claim*: it names a source object, an action
//! (a rename or a same-filesystem move), why that action is proposed, and the
//! evidence the claim rests on. A proposal is **never permission to mutate** -
//! every action is revalidated by [`crate::repair::preflight`] and again by the
//! transaction executor immediately before execution.
//!
//! # Deliberately small
//!
//! Only `RenamePath` and `MovePath` are executable. Every future action kind
//! (`DeleteDuplicate`, `RebuildArchive`, `RewriteArchiveMember`, `ConvertDisc`,
//! `FetchMissing`) exists only as a typed, non-executable [`RepairAction::Deferred`]
//! variant so a future planner can name its intent without ever acquiring a
//! mutation path through this layer.
//!
//! # Identity is typed, never a display string
//!
//! `expected_source_identity` is the strongest existing filesystem identity
//! model (`crate::dat::rename_apply::ObjectIdentity`: size, kind, inode and
//! device where available, captured without following symlinks). It is carried
//! so preflight and execution can refuse an action whose source changed after
//! the proposal was created. DAT provenance is carried as typed optional
//! fields, not a single opaque string.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dat::rename_apply::ObjectIdentity;

/// A durable, single-component identifier for one repair proposal.
///
/// Never a filesystem path: it names the *proposal*, so the GUI can reference
/// it across a plan without quoting paths. Rejects anything that could escape
/// a filename or a JSON key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepairProposalId(String);

/// Upper bound on a proposal id's length, matching the journal-name ceiling.
const MAX_PROPOSAL_ID_BYTES: usize = 128;

impl RepairProposalId {
    /// Builds a proposal id, rejecting empty, overlong, or path-unsafe values.
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_PROPOSAL_ID_BYTES
            || id.contains(['/', '\\', '\0'])
            || id == "."
            || id == ".."
        {
            None
        } else {
            Some(Self(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepairProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The action a proposal requests. Only the two rename variants are ever
/// executable in this foundation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    /// Rename the source in place to a new basename in the same directory.
    RenamePath { destination: PathBuf },
    /// Move the source to a different directory on the same filesystem.
    MovePath { destination: PathBuf },
    /// A future action kind, represented but never executed by this layer.
    Deferred(DeferredActionKind),
}

/// Future action kinds. These are vocabulary only: no executable stub exists,
/// so nothing here can accidentally mutate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredActionKind {
    DeleteDuplicate,
    RebuildArchive,
    RewriteArchiveMember,
    ConvertDisc,
    FetchMissing,
}

impl DeferredActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::DeleteDuplicate => "delete duplicate",
            Self::RebuildArchive => "rebuild archive",
            Self::RewriteArchiveMember => "rewrite archive member",
            Self::ConvertDisc => "convert disc image",
            Self::FetchMissing => "fetch missing ROM",
        }
    }
}

impl RepairAction {
    /// Whether this action kind may ever be executed by the Repair Center.
    pub fn is_executable(&self) -> bool {
        matches!(self, Self::RenamePath { .. } | Self::MovePath { .. })
    }

    /// The destination path, for rename and move actions.
    pub fn destination(&self) -> Option<&PathBuf> {
        match self {
            Self::RenamePath { destination } | Self::MovePath { destination } => Some(destination),
            Self::Deferred(_) => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::RenamePath { .. } => "rename",
            Self::MovePath { .. } => "move",
            Self::Deferred(kind) => kind.label(),
        }
    }
}

/// The safety classification of a proposal.
///
/// - [`SafetyState::Safe`]: every requirement is proven; the action may be
///   eligible for execution (preflight still revalidates it immediately before
///   mutation).
/// - [`SafetyState::NeedsReview`]: evidence exists but ambiguity or an
///   unsupported policy prevents automatic mutation. There is **no force
///   mode**: a `NeedsReview` proposal is never promoted to executable.
/// - [`SafetyState::Blocked`]: known unsafe or impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyState {
    Safe,
    NeedsReview,
    Blocked,
}

impl SafetyState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::NeedsReview => "needs review",
            Self::Blocked => "blocked",
        }
    }
}

/// The typed reason a proposal is (claimed to be) safe.
///
/// Only evidence kinds the current code can actually produce are defined here;
/// nothing is fabricated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairEvidenceKind {
    /// The canonical name came from a verified DAT match.
    CanonicalDatName,
    /// A whole outer archive was attributed to exactly one verified set
    /// (`SetState::Complete`), so renaming the archive is safe.
    VerifiedWholeArchiveAttribution,
    /// The source matched exactly one DAT member by cryptographic hash.
    ExactDatMemberIdentity,
    /// Two sources were identified as the same content (future cleanup).
    DuplicateContent,
    /// The user explicitly requested this organisation.
    UserRequestedOrganisation,
}

impl RepairEvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::CanonicalDatName => "canonical DAT name",
            Self::VerifiedWholeArchiveAttribution => "verified whole-archive attribution",
            Self::ExactDatMemberIdentity => "exact DAT member identity",
            Self::DuplicateContent => "duplicate content",
            Self::UserRequestedOrganisation => "user-requested organisation",
        }
    }
}

/// One piece of evidence attached to a proposal, ready for GUI explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairEvidence {
    pub kind: RepairEvidenceKind,
    pub detail: String,
}

impl RepairEvidence {
    pub fn new(kind: RepairEvidenceKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// A reference to the scan/audit that produced the evidence, when the current
/// architecture has one. `source_id` is the DAT source registration; the
/// generation is the audit-generation stamp the rename-plan layer already uses
/// to reject stale plans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairAuditRef {
    pub source_id: String,
    pub generation: u64,
}

/// One Repair Center proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairProposal {
    pub id: RepairProposalId,
    pub action: RepairAction,
    /// The source object's path at proposal time. Identity is carried
    /// separately in `expected_source_identity`; this path is never the
    /// identity itself.
    pub source_path: PathBuf,
    /// Why this action is proposed, for GUI explanation.
    pub reason: String,
    pub evidence: Vec<RepairEvidence>,
    /// The strongest audited filesystem identity of the source, when one was
    /// captured. Preflight and execution compare the live source against this
    /// and refuse on any difference.
    pub expected_source_identity: Option<ObjectIdentity>,
    /// The scan/audit that produced this proposal, when known.
    pub originating_audit: Option<RepairAuditRef>,
    pub safety: SafetyState,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// DAT provenance, carried when the proposal came from a DAT rename plan.
    #[serde(default)]
    pub dat_source_id: Option<String>,
    #[serde(default)]
    pub dat_source_display: Option<String>,
    #[serde(default)]
    pub game_name: Option<String>,
    #[serde(default)]
    pub rom_name: Option<String>,
    #[serde(default)]
    pub verdict_label: Option<String>,
    #[serde(default)]
    pub match_confident: bool,
    #[serde(default)]
    pub is_outer_archive: bool,
    /// Whether the outer archive's set was storage-`Complete` when proposed.
    #[serde(default)]
    pub is_outer_archive_verified: bool,
}

impl RepairProposal {
    /// The destination, for rename and move actions.
    pub fn destination(&self) -> Option<&PathBuf> {
        self.action.destination()
    }

    /// Whether the proposal may even be considered for execution: an
    /// executable action, classified `Safe`, with no blockers. The plan and
    /// the executor re-check every condition; this is a shorthand for the
    /// planner's classification, never an override.
    pub fn actionable(&self) -> bool {
        self.action.is_executable() && self.safety == SafetyState::Safe && self.blockers.is_empty()
    }

    /// A one-line headline for a proposal row.
    pub fn headline(&self) -> String {
        match self.destination() {
            Some(destination) => format!(
                "{} -> {}",
                self.source_path.display(),
                destination.display()
            ),
            None => format!(
                "{}: {}",
                self.action.kind_label(),
                self.source_path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposal_id_rejects_path_unsafe_values() {
        assert!(RepairProposalId::new("a-b-1").is_some());
        assert!(RepairProposalId::new("").is_none());
        assert!(RepairProposalId::new(".").is_none());
        assert!(RepairProposalId::new("..").is_none());
        assert!(RepairProposalId::new("a/b").is_none());
        assert!(RepairProposalId::new("a\\b").is_none());
        assert!(RepairProposalId::new("a\0b").is_none());
        assert!(RepairProposalId::new("x".repeat(129)).is_none());
    }

    #[test]
    fn deferred_actions_are_never_executable() {
        for kind in [
            DeferredActionKind::DeleteDuplicate,
            DeferredActionKind::RebuildArchive,
            DeferredActionKind::RewriteArchiveMember,
            DeferredActionKind::ConvertDisc,
            DeferredActionKind::FetchMissing,
        ] {
            assert!(!RepairAction::Deferred(kind).is_executable());
            assert_eq!(RepairAction::Deferred(kind).destination(), None);
        }
        assert!(
            RepairAction::RenamePath {
                destination: PathBuf::from("/x")
            }
            .is_executable()
        );
        assert!(
            RepairAction::MovePath {
                destination: PathBuf::from("/y")
            }
            .is_executable()
        );
    }

    #[test]
    fn a_blocked_or_review_proposal_is_not_actionable() {
        let base = RepairProposal {
            id: RepairProposalId::new("p1").unwrap(),
            action: RepairAction::RenamePath {
                destination: PathBuf::from("/tmp/x/new.bin"),
            },
            source_path: PathBuf::from("/tmp/x/old.bin"),
            reason: "r".to_string(),
            evidence: Vec::new(),
            expected_source_identity: None,
            originating_audit: None,
            safety: SafetyState::Safe,
            blockers: Vec::new(),
            warnings: Vec::new(),
            dat_source_id: None,
            dat_source_display: None,
            game_name: None,
            rom_name: None,
            verdict_label: None,
            match_confident: false,
            is_outer_archive: false,
            is_outer_archive_verified: false,
        };
        assert!(base.actionable());
        assert!(
            !RepairProposal {
                safety: SafetyState::NeedsReview,
                ..base.clone()
            }
            .actionable()
        );
        assert!(
            !RepairProposal {
                safety: SafetyState::Blocked,
                ..base.clone()
            }
            .actionable()
        );
        assert!(
            !RepairProposal {
                blockers: vec!["blocked".to_string()],
                ..base.clone()
            }
            .actionable()
        );
        assert!(
            !RepairProposal {
                action: RepairAction::Deferred(DeferredActionKind::FetchMissing),
                ..base
            }
            .actionable()
        );
    }
}
