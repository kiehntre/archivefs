//! The Repair Center foundation: TRUTH -> PROPOSAL -> PREFLIGHT -> PREVIEW ->
//! SAFE TRANSACTION -> REVERIFY / AUDIT.
//!
//! This is the smallest trustworthy core a future Repair Center GUI,
//! whole-library scanner, RomM cleanup, archive rebuild, and organisation
//! feature can consume. It deliberately does not fix everything: only
//! same-filesystem `RenamePath` and `MovePath` are executable, and everything
//! else is typed as a non-executable deferred action.
//!
//! # The governing principle
//!
//! **A repair proposal is NOT permission to mutate.** Every action is
//! revalidated immediately before execution: source identity is re-captured
//! (`symlink_metadata`, inode/device where available), the destination is
//! re-checked for existence, and the mutation itself is an atomic
//! no-clobber rename (`renameat2(RENAME_NOREPLACE)`). False refusal is
//! acceptable. Wrong mutation is not.
//!
//! # Reuse, not reinvention
//!
//! The transaction, journal, rollback, recovery, identity, and no-clobber
//! machinery all live in [`crate::dat::rename_apply`]. This module is a thin,
//! typed vocabulary on top of it: [`proposal`] (the typed proposal model),
//! [`plan`] (batch conflict detection), [`preflight`] (dry run), [`execute`]
//! (the transaction driver), and [`adapter`] (the bridge from the existing
//! hardened DAT rename plans). No parallel transaction system exists here.
//!
//! # Layers
//!
//! 1. [`RepairProposal`] - one typed, evidenced claim.
//! 2. [`RepairPlan`] - a batch with every global conflict detected up front.
//! 3. [`run_repair_preflight`] - the pure dry run a GUI renders.
//! 4. [`RepairTransactionResult`] - the journaled, rollbackable outcome with a
//!    post-apply re-verification pass.

pub mod adapter;
pub mod duplicate;
pub mod execute;
pub mod library;
pub mod plan;
pub mod preflight;
pub mod proposal;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod library_tests;

pub use duplicate::{
    DuplicateContentProof, DuplicateHashCache, DuplicatePairClassification, DuplicateProofRefusal,
    candidate_pairs, prove_duplicate_content, prove_duplicate_group,
};
pub use execute::{
    RepairApplyExecution, RepairExecutionError, RepairExecutionOptions, RepairRecoveryReport,
    RepairReverifyEntry, RepairReverifyOutcome, RepairTransactionResult, apply_repair_transaction,
    build_repair_transaction, classify_persisted_transactions, execute_repair_plan,
    reverify_transaction, rollback_repair_transaction,
};
pub use library::{
    ApplySavedPlanError, ApplySavedPlanSelectedError, LibraryRepairPlan, LibraryRepairReport,
    LibraryScanError, LibraryScanOutcome, LibraryScanRequest, PlanItem, RepairProfile,
    ReportCounts, SetItem, apply_library_repair_plan, apply_saved_plan, apply_saved_plan_selected,
    build_library_repair_report, plan_file_from_scan, preview_library_repair_plan,
    re_prove_saved_plan, run_library_scan,
};
pub use plan::{
    PlanConflict, PlanConflictKind, RepairPlan, RepairPlanId, build_repair_plan,
    detect_plan_conflicts, select_repair_plan_subset,
};
pub use preflight::{
    RepairPreflightReport, RepairPreflightResult, RepairPreflightStatus, run_repair_preflight,
};
pub use proposal::{
    DeferredActionKind, RepairAction, RepairAuditRef, RepairEvidence, RepairEvidenceKind,
    RepairProposal, RepairProposalId, SafetyState,
};
