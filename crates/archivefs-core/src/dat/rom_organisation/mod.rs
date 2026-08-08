//! Canonical ROM organisation into a user-configured master ROM root.
//!
//! The user configures a master ROM root (for example `/mnt/games/roms`).
//! ArchiveFS can then *plan*, and after explicit approval *apply*, moving
//! identified games into canonical RomM-compatible platform directories
//! beneath that root - never guessing a folder name from a display label.
//!
//! # Safety
//!
//! - Planning is read-only; nothing moves until the user approves an apply.
//! - The destination platform directory always comes from the canonical
//!   platform id's RomM slug mapping, never from display text.
//! - Mutations reuse the `rename_apply` engine: durable journal before any
//!   mutation, per-entry `Applying` checkpoint, `renameat2(RENAME_NOREPLACE)`
//!   no-clobber moves, identity re-check immediately before each move, shared
//!   crash reconciliation and rollback, honest cancellation.
//! - Only same-filesystem moves are supported. A cross-filesystem destination
//!   is refused with "Unsupported" - there is no copy+delete fallback.
//! - Symlink-only mode moves the link *object* and never dereferences or
//!   touches its target.
//! - Only canonical platform directories created by the apply are ever
//!   removed on rollback, and only while empty; a pre-existing user directory
//!   is never removed.
//!
//! # Modes
//!
//! Three explicit, never-combined modes: rename in place, move the real file,
//! or organise a symlink object only.

pub mod model;
pub mod plan;
pub mod transaction;

#[cfg(test)]
mod tests;

pub use model::{OrganisationMode, OrganisationPlan, OrganisationPlanEntry, OrganisationStatus};
pub use plan::{OrganisationCandidate, OrganisationPlanRequest, build_organisation_plan};
pub use transaction::{
    OrganisationRollbackOutcome, apply_organisation_transaction, build_organisation_transaction,
    rollback_organisation_transaction,
};
