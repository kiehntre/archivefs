//! Read-only DAT rename planning.
//!
//! This module derives a *proposed canonical filename* for each file the DAT
//! audit verified against a catalogue, ranks the already-verified candidates
//! with the user's effective DAT policy, and reports the proposal, its
//! explanations, and anything that blocks it - without ever touching a file.
//!
//! # Hard safety rule
//!
//! Nothing in this module (or anywhere in this PR) renames, moves, deletes,
//! rewrites, chmods, truncates, replaces, or otherwise mutates a ROM or game
//! file. Planning only:
//!
//! - inspects an existing audit result and read-only `symlink_metadata`;
//! - derives a proposed canonical filename from the authoritative DAT entry;
//! - explains why and surfaces ambiguity and conflicts;
//! - produces proposals a future, separately approved apply stage could act on.
//!
//! # What a proposal means
//!
//! [`RenameProposal`] never records that a rename happened. Its [`ProposalState`]
//! describes what the plan *could* do: `Suggested` is the only actionable
//! state, and [`RenameProposal::actionable`] is an eligibility statement, never
//! an executed action. `Ambiguous` means the policy could not pick a winner;
//! `Conflict` means a collision blocks the proposal; `Unsupported` and
//! `Blocked` mean no safe canonical name exists.
//!
//! # Review decisions are session-only
//!
//! [`ReviewDecision`] lets the user record their stance on a proposal. Recording
//! one never touches a file. Persisting these decisions is deliberately
//! deferred: `dat_sources.toml` owns *preferences*, and per-file review state
//! belongs in the library database, which a schema migration would be needed
//! to extend - that is out of scope here. See
//! `docs/design/DAT_RENAME_PLANNING_STAGE1.md`.

pub mod derive;
pub mod model;

pub use derive::{DeriveOutcome, DerivedName, derive_proposed_basename};
pub use model::{
    CollisionInfo, CollisionKind, ExtensionStatus, ProposalState, RenamePlan, RenamePlanCounts,
    RenameProposal, ReviewDecision, SourceObjectKind,
};
