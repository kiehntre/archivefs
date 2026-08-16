//! Duplicate-content quarantine: moving only the redundant copy of a proven
//! duplicate-content group into a trusted-root-local quarantine directory,
//! using ordinary [`RepairAction::MovePath`] semantics.
//!
//! This is the first *mutating* slice built on top of the read-only
//! [`super::duplicate`] proof: it still never deletes anything.
//! `DeferredActionKind::DeleteDuplicate` remains permanently non-executable
//! (`RepairAction::is_executable` is `true` only for `RenamePath`/`MovePath`)
//! and this module never constructs one. A quarantine move is fully
//! reversible through the existing journal/rollback machinery.
//!
//! # The pipeline
//!
//! 1. [`select_survivor`] - a pure, objective decision from
//!    [`KeeperEvidence`] only (already-canonical DAT name, or a confidently
//!    verified DAT match). Never size, mtime, lexical path, root order, or
//!    platform. No unique winner -> [`SurvivorSelection::NeedsReview`], and
//!    no Safe move is ever produced for that group.
//! 2. [`plan_duplicate_quarantine`] - proves every other member against the
//!    chosen survivor via [`super::duplicate::prove_duplicate_content`] and
//!    builds one `Safe` `MovePath` [`RepairProposal`] per member proven a
//!    **distinct-object** duplicate. A `SameObject` (hard-linked) member, or
//!    one whose proof refuses, is recorded in
//!    [`QuarantineGroupPlan::skipped`] with why - never silently dropped,
//!    and never blocking the group's other members.
//! 3. [`build_quarantine_transaction`] - re-proves every proposal live
//!    (stored `RepairEvidence`/identity is never mutation authority) and
//!    builds a [`RenameTransaction`]. Deliberately **not**
//!    [`crate::repair::execute::build_repair_transaction`]: that function's
//!    validation requires the `MovePath` destination directory to already
//!    exist, which is never true for a fresh quarantine bucket - directory
//!    creation (and ownership) happens only at apply time, exactly like
//!    [`crate::dat::rom_organisation::transaction`].
//! 4. [`apply_quarantine_transaction`] - re-proves the whole batch live
//!    *again* before any mutation or directory creation, then creates the
//!    quarantine directories this transaction needs (ownership recorded
//!    strictly after `create_dir` succeeds; a pre-existing symlink there is
//!    refused, never followed), then applies entries one at a time -
//!    re-proving **each entry individually against the survivor
//!    immediately before that entry's own move**, so a later entry in a
//!    multi-member group never moves on the strength of the earlier
//!    whole-batch proof alone. Any failure aborts the remaining entries
//!    (`AbortAll` semantics), leaving already-applied entries applied and
//!    the rest untouched.
//! 5. [`rollback_quarantine_transaction`] - the shared rollback engine for
//!    the moves, then removes only the quarantine directories this
//!    transaction created, only while still empty, never a pre-existing one.
//!
//! # Not sole mutation authority either
//!
//! Exactly like [`super::duplicate::DuplicateContentProof`], nothing built
//! here is ever trusted from a stored copy: [`build_quarantine_transaction`]
//! and [`apply_quarantine_transaction`] each independently re-prove against
//! the live filesystem before doing anything with the result.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dat::classification::CLASSIFIER_VERSION;
use crate::dat::rename_apply::executor::{ApplyError, ApplyOutcome, apply_mutation};
use crate::dat::rename_apply::identity::identity_matches;
use crate::dat::rename_apply::journal::{new_transaction_id, write_journal};
use crate::dat::rename_apply::model::{
    EntryState, RenameTransaction, TransactionEntry, TransactionState, TransactionSummary,
};
use crate::dat::rename_apply::preflight::{
    DirectoryPolicy, PreflightOptions, batch_destinations, is_safe_basename, run_preflight,
};
use crate::dat::rename_apply::rollback::{RollbackOutcome, rollback_transaction};
use crate::dat::rename_plan::{ProposalState, RenameProposal};
use crate::dat::sources::now_unix;
use crate::identity_source::hashing::Crc32;
use crate::safe_read::TrustedRoots;

use super::duplicate::{
    DuplicateContentProof, DuplicateHashCache, DuplicatePairClassification, prove_duplicate_content,
};
use super::plan::detect_plan_conflicts;
use super::proposal::{RepairAction, RepairProposal, RepairProposalId, SafetyState};

/// The quarantine directory's name directly beneath a trusted scan root.
pub const QUARANTINE_DIRECTORY_NAME: &str = ".emuwiz-quarantine";

// ---------------------------------------------------------------------------
// Survivor selection
// ---------------------------------------------------------------------------

/// Objective DAT-derived evidence for one candidate member of a
/// duplicate-content group, bridged from the existing rename-plan/audit
/// side.
///
/// Deliberately thin and separate from [`DuplicateContentProof`]: this
/// carries only what [`select_survivor`] needs to make an objective
/// decision, and content proof (whether two paths actually contain the same
/// bytes) is a completely independent question this type never answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperEvidence {
    pub path: PathBuf,
    /// The file's current name already equals its DAT-canonical name
    /// (`ProposalState::AlreadyCanonical`), never inferred from size, mtime,
    /// or lexical path.
    pub already_canonical: bool,
    /// The DAT match itself rests on a cryptographic hash
    /// (`RenameProposal::match_confident`), not merely a filename or a
    /// CRC32-only hit.
    pub verified_confident: bool,
    pub dat_source_id: Option<String>,
    pub dat_source_display: Option<String>,
    pub game_name: Option<String>,
    pub rom_name: Option<String>,
    pub verdict_label: Option<String>,
}

impl KeeperEvidence {
    /// The objective claim strength: both signals (2), exactly one (1), or
    /// neither (0). Never a ranking between "canonical-only" and
    /// "verified-only" - the task only defines these two signals as equally
    /// objective, so a canonical-only claim and a verified-only claim tie at
    /// the same tier.
    fn claim_tier(&self) -> u8 {
        match (self.already_canonical, self.verified_confident) {
            (true, true) => 2,
            (true, false) | (false, true) => 1,
            (false, false) => 0,
        }
    }
}

/// Bridges the minimum DAT provenance a [`RenameProposal`] already carries
/// into [`KeeperEvidence`] for one duplicate-group member.
///
/// A pure, narrow adapter - mirrors
/// [`super::adapter::repair_proposal_from_suggested_rename`]'s bridging
/// style without redesigning `DuplicateContentProof` into a metadata
/// container.
pub fn keeper_evidence_from_rename_proposal(
    path: &Path,
    proposal: &RenameProposal,
) -> KeeperEvidence {
    KeeperEvidence {
        path: path.to_path_buf(),
        already_canonical: proposal.state == ProposalState::AlreadyCanonical,
        verified_confident: proposal.match_confident,
        dat_source_id: Some(proposal.source_id.clone()),
        dat_source_display: Some(proposal.source_display_name.clone()),
        game_name: proposal.game_name.clone(),
        rom_name: proposal.rom_name.clone(),
        verdict_label: Some(proposal.verdict_label.clone()),
    }
}

/// The result of choosing a survivor for a duplicate-content group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurvivorSelection {
    /// Exactly one member holds the strongest objective claim.
    Survivor(KeeperEvidence),
    /// No unique objective survivor exists. Never a Safe quarantine move for
    /// this group - the caller classifies it NeedsReview / non-actionable.
    NeedsReview { reason: String },
}

/// Selects the objective survivor for a duplicate-content group, or refuses.
///
/// Uses only [`KeeperEvidence::already_canonical`] and
/// [`KeeperEvidence::verified_confident`] - never size, mtime, lexical path,
/// root order, or platform alone (this function never even sees those
/// fields). Exactly one member at the group's highest non-empty claim tier
/// wins; zero members with any claim, or more than one member tied at the
/// top tier, both refuse.
pub fn select_survivor(evidence: &[KeeperEvidence]) -> SurvivorSelection {
    if evidence.len() < 2 {
        return SurvivorSelection::NeedsReview {
            reason: "a duplicate group needs at least two members".to_string(),
        };
    }
    let top_tier = evidence
        .iter()
        .map(KeeperEvidence::claim_tier)
        .max()
        .unwrap_or(0);
    if top_tier == 0 {
        return SurvivorSelection::NeedsReview {
            reason: "no member has an already-canonical name or a confidently verified DAT match"
                .to_string(),
        };
    }
    let winners: Vec<&KeeperEvidence> = evidence
        .iter()
        .filter(|candidate| candidate.claim_tier() == top_tier)
        .collect();
    match winners.as_slice() {
        [only] => SurvivorSelection::Survivor((*only).clone()),
        _ => SurvivorSelection::NeedsReview {
            reason: format!(
                "{} members share an equally strong canonical/verified claim; no unique survivor",
                winners.len()
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// Quarantine destination scheme
// ---------------------------------------------------------------------------

/// The deterministic quarantine destination for one redundant member of a
/// proven duplicate group:
///
/// `<trusted_root>/.emuwiz-quarantine/<content-hash-prefix>/<source-disambiguator>-<original-basename>`
///
/// - the content-hash prefix (16 hex characters of the proof's SHA-1) buckets
///   every copy of the *same* content together, tying the quarantine
///   location directly to the proof rather than an arbitrary name;
/// - the source disambiguator (8 hex characters, CRC32 of the redundant
///   file's full absolute source path - reusing the existing [`Crc32`],
///   never a second hash implementation) is a **deterministic label, not a
///   uniqueness guarantee**: it exists only to make the common case (two
///   identical-content copies from two different source paths) produce two
///   different, readable basenames inside one bucket. A 32-bit checksum can
///   collide, so it is never trusted as the safety boundary - see below;
/// - the original basename is preserved visibly, exactly as it existed at
///   the source.
///
/// Deterministic by construction: the same redundant path always maps to the
/// same destination every time it is computed. The actual safety against a
/// collision (whether from the CRC32 disambiguator or anything else) is
/// [`build_quarantine_transaction`]'s conflict detection
/// ([`detect_plan_conflicts`], which refuses a batch with two proposals
/// targeting the same destination) plus the no-clobber
/// (`RENAME_NOREPLACE`) rename primitive at apply time, which refuses rather
/// than overwrites if a destination is unexpectedly already occupied. No
/// suffix hunting either way.
pub fn quarantine_destination(
    trusted_root: &Path,
    proof: &DuplicateContentProof,
    redundant_source: &Path,
) -> Result<PathBuf, String> {
    let Some(basename) = redundant_source.file_name().and_then(|name| name.to_str()) else {
        return Err(format!(
            "'{}' has no usable basename",
            redundant_source.display()
        ));
    };
    let content_bucket = &proof.hash[..proof.hash.len().min(16)];
    if !is_safe_basename(content_bucket) {
        return Err("the proof's content hash is not a safe directory name".to_string());
    }
    let disambiguator = Crc32::of(redundant_source.to_string_lossy().as_bytes());
    let destination_basename = format!("{disambiguator}-{basename}");
    if !is_safe_basename(&destination_basename) {
        return Err(format!(
            "'{destination_basename}' is not a safe quarantine basename"
        ));
    }
    Ok(trusted_root
        .join(QUARANTINE_DIRECTORY_NAME)
        .join(content_bucket)
        .join(destination_basename))
}

// ---------------------------------------------------------------------------
// Planning: survivor selection + per-member live proof -> Safe proposals
// ---------------------------------------------------------------------------

/// One proven, evidenced plan for quarantining every redundant copy of one
/// duplicate-content group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineGroupPlan {
    pub survivor: KeeperEvidence,
    /// `Safe` `MovePath` proposals, one per redundant member independently
    /// proven a distinct-object duplicate of the survivor. The survivor
    /// itself never appears here as a source.
    pub proposals: Vec<RepairProposal>,
    /// Members that could not become a Safe proposal, with why - never
    /// silently dropped, and never blocking the group's other members. A
    /// `SameObject` (hard-linked) pair lands here, never as a move.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Why a whole group could not produce any quarantine plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantinePlanRefusal {
    /// [`select_survivor`] could not find a unique objective survivor.
    NeedsReview { reason: String },
}

/// Plans quarantine of every redundant member of a duplicate-content group.
///
/// The survivor is chosen purely from `evidence` via [`select_survivor`].
/// Every *other* member is then independently, live-proven against the
/// survivor via [`prove_duplicate_content`] - this is the "at planning"
/// proof: it is what first attaches `DuplicateContent` evidence to a
/// proposal, and [`build_quarantine_transaction`] re-proves it again rather
/// than trusting it.
pub fn plan_duplicate_quarantine(
    evidence: &[KeeperEvidence],
    trusted_root: &Path,
    trusted: &TrustedRoots,
    cache: &mut DuplicateHashCache,
    cancel: Option<&AtomicBool>,
) -> Result<QuarantineGroupPlan, QuarantinePlanRefusal> {
    let survivor = match select_survivor(evidence) {
        SurvivorSelection::Survivor(survivor) => survivor,
        SurvivorSelection::NeedsReview { reason } => {
            return Err(QuarantinePlanRefusal::NeedsReview { reason });
        }
    };

    let mut proposals = Vec::new();
    let mut skipped = Vec::new();
    for candidate in evidence {
        if candidate.path == survivor.path {
            continue;
        }
        match prove_duplicate_content(&candidate.path, &survivor.path, cache, trusted, cancel) {
            Ok(proof) if proof.classification == DuplicatePairClassification::DistinctObjects => {
                match build_quarantine_proposal(trusted_root, &proof, &candidate.path, &survivor) {
                    Ok(proposal) => proposals.push(proposal),
                    Err(detail) => skipped.push((candidate.path.clone(), detail)),
                }
            }
            Ok(_same_object) => skipped.push((
                candidate.path.clone(),
                "the same filesystem object as the survivor (hard-linked); not a reclaimable \
                 duplicate"
                    .to_string(),
            )),
            Err(refusal) => skipped.push((candidate.path.clone(), refusal.to_string())),
        }
    }

    Ok(QuarantineGroupPlan {
        survivor,
        proposals,
        skipped,
    })
}

fn build_quarantine_proposal(
    trusted_root: &Path,
    proof: &DuplicateContentProof,
    redundant_source: &Path,
    survivor: &KeeperEvidence,
) -> Result<RepairProposal, String> {
    let destination = quarantine_destination(trusted_root, proof, redundant_source)?;
    let content_bucket = &proof.hash[..proof.hash.len().min(16)];
    let disambiguator = Crc32::of(redundant_source.to_string_lossy().as_bytes());
    let id = RepairProposalId::new(format!("quarantine-{content_bucket}-{disambiguator}"))
        .ok_or_else(|| "could not build a safe proposal id".to_string())?;
    let reason = format!(
        "'{}' is a redundant byte-identical copy of the kept file '{}'; moved to quarantine \
         rather than deleted",
        redundant_source.display(),
        survivor.path.display(),
    );
    Ok(RepairProposal {
        id,
        action: RepairAction::MovePath { destination },
        source_path: redundant_source.to_path_buf(),
        reason,
        evidence: vec![proof.evidence()],
        // Bound to *this* live proof, produced moments ago - never a stale
        // value carried across a save/load boundary.
        expected_source_identity: Some(proof.identity_a.clone()),
        originating_audit: None,
        safety: SafetyState::Safe,
        blockers: Vec::new(),
        warnings: Vec::new(),
        dat_source_id: survivor.dat_source_id.clone(),
        dat_source_display: survivor.dat_source_display.clone(),
        game_name: survivor.game_name.clone(),
        rom_name: survivor.rom_name.clone(),
        verdict_label: survivor.verdict_label.clone(),
        match_confident: survivor.verified_confident,
        is_outer_archive: false,
        is_outer_archive_verified: false,
        // The one authoritative signal that this `MovePath` is a
        // duplicate-quarantine move: see `RepairProposal::survivor_path`'s
        // doc. Every caller (selected apply, the generic executor's guard)
        // uses this, never the action kind alone, to route execution.
        survivor_path: Some(survivor.path.clone()),
    })
}

// ---------------------------------------------------------------------------
// Transaction build: live re-proof, never trusting stored evidence
// ---------------------------------------------------------------------------

/// Builds the transaction for a quarantine group's Safe proposals.
///
/// Deliberately not [`crate::repair::execute::build_repair_transaction`]:
/// that function's per-action validation requires a `MovePath` destination
/// directory to already exist, which is never true for a fresh quarantine
/// bucket - the bucket is created (and ownership recorded) only at apply
/// time in [`apply_quarantine_transaction`], exactly like
/// [`crate::dat::rom_organisation::transaction::build_organisation_transaction`].
/// Most other checks that function performs are still reused here: global
/// conflict detection ([`detect_plan_conflicts`]), absolute/no-`..`
/// destination, and safe basename. Same-filesystem is *not* checked here
/// (the bucket directory does not exist yet, so it cannot be stat'd) - the
/// shared executor's own preflight enforces it at apply time, once the
/// directory exists.
///
/// `proposals`' stored `RepairEvidence`/`expected_source_identity` is never
/// trusted: every proposal's source is independently re-proven a
/// distinct-object duplicate of `survivor_path` right here, live, via
/// [`prove_duplicate_content`]. The entry's identity comes from *this* fresh
/// proof, never the proposal's stored one.
pub fn build_quarantine_transaction(
    proposals: &[RepairProposal],
    survivor_path: &Path,
    trusted_root: &Path,
    dat_generation: u64,
    cache: &mut DuplicateHashCache,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Result<RenameTransaction, String> {
    if proposals.is_empty() {
        return Err("the quarantine plan has no Safe proposals to build".to_string());
    }
    if proposals
        .iter()
        .any(|proposal| proposal.source_path == survivor_path)
    {
        return Err("the survivor must never be a quarantine move source".to_string());
    }

    let conflicts = detect_plan_conflicts(proposals);
    if !conflicts.is_empty() {
        let detail = conflicts
            .iter()
            .map(|conflict| format!("{}: {}", conflict.kind.clone().label(), conflict.detail))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "the quarantine plan has unresolved conflicts: {detail}"
        ));
    }

    let quarantine_root = trusted_root.join(QUARANTINE_DIRECTORY_NAME);
    let mut entries = Vec::with_capacity(proposals.len());
    for proposal in proposals {
        let RepairAction::MovePath { destination } = &proposal.action else {
            return Err(format!("proposal '{}' is not a MovePath", proposal.id));
        };
        if proposal.safety != SafetyState::Safe || !proposal.blockers.is_empty() {
            return Err(format!("proposal '{}' is not Safe", proposal.id));
        }
        validate_quarantine_destination(&proposal.source_path, destination, &quarantine_root)?;

        // Live re-proof: the proposal's stored evidence and identity are
        // never trusted, only what this call proves right now.
        let proof =
            prove_duplicate_content(&proposal.source_path, survivor_path, cache, trusted, cancel)
                .map_err(|refusal| {
                format!(
                    "'{}' could not be re-proven a duplicate at build time: {refusal}",
                    proposal.source_path.display()
                )
            })?;
        if proof.classification != DuplicatePairClassification::DistinctObjects {
            return Err(format!(
                "'{}' is the same filesystem object as the survivor; refusing to quarantine",
                proposal.source_path.display()
            ));
        }

        let original_basename = proposal
            .source_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let proposed_basename = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        entries.push(TransactionEntry {
            source_path: proposal.source_path.clone(),
            destination_path: destination.clone(),
            original_basename,
            proposed_basename,
            identity: proof.identity_a,
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        });
    }

    Ok(RenameTransaction {
        transaction_id: new_transaction_id(now_unix()),
        plan_generation: dat_generation,
        classifier_version: Some(CLASSIFIER_VERSION.to_string()),
        created_at_unix: now_unix(),
        source_scan_root: trusted_root.to_string_lossy().into_owned(),
        state: TransactionState::Planned,
        entries,
        // Deliberately left empty here, exactly like
        // `build_organisation_transaction`: a directory is only ever
        // recorded as owned strictly after `create_dir` succeeds at apply
        // time, so a pre-existing directory can never be journalled as
        // EmuWiz's to remove.
        created_directories: Vec::new(),
        unknown: Default::default(),
    })
}

fn validate_quarantine_destination(
    source: &Path,
    destination: &Path,
    quarantine_root: &Path,
) -> Result<(), String> {
    if !destination.is_absolute() {
        return Err("the destination must be an absolute path".to_string());
    }
    if destination == source {
        return Err("the destination must differ from the source".to_string());
    }
    if destination
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err("the destination must not contain '..' components".to_string());
    }
    let Some(basename) = destination.file_name().and_then(|name| name.to_str()) else {
        return Err("the destination must have a usable basename".to_string());
    };
    if !is_safe_basename(basename) {
        return Err("the destination basename is not a safe single component".to_string());
    }
    if !destination.starts_with(quarantine_root) {
        return Err(
            "the destination must be inside the trusted root's quarantine directory".to_string(),
        );
    }
    // Same-filesystem is deliberately not checked here: the quarantine
    // bucket directory does not exist yet at build time (see this module's
    // doc), so `std::fs::metadata` on its parent would always fail. The
    // shared executor's own preflight (`DirectoryPolicy::SameFilesystem`,
    // run inside `apply_transaction`) enforces it at apply time, after
    // `apply_quarantine_transaction` has created the directory - the same
    // ordering `rom_organisation` relies on.
    let _ = source;
    Ok(())
}

// ---------------------------------------------------------------------------
// Apply: live re-proof immediately before mutation, then directory ownership
// ---------------------------------------------------------------------------

/// Applies a quarantine transaction.
///
/// # Two layers of live re-proof
///
/// 1. **Whole-batch pre-proof**, before a single directory is created or a
///    single file moved: every entry's source is re-proven a distinct-object
///    duplicate of `survivor_path`. Any disagreement here aborts the entire
///    batch immediately, before any mutation - this is the cheap, obvious
///    check.
/// 2. **Per-entry pre-move re-proof**, immediately before *each* individual
///    move, interleaved with the apply loop below: a later entry never moves
///    on the strength of the batch-wide proof above alone. This closes the
///    multi-entry TOCTOU a whole-batch-only proof leaves open - the survivor
///    could still be replaced, renamed, or turned into a hard link *between*
///    two entries' moves in the same batch, after the first check already
///    passed and after an earlier entry already moved.
///
/// Deliberately not [`apply_transaction`] for the mutation loop: the shared
/// executor applies a whole batch in one call with no hook between entries,
/// and adding a domain-specific "re-prove against an external survivor path"
/// hook there would broaden its generic rename semantics for one caller.
/// Instead this function runs its own per-entry loop, reusing the exact same
/// primitives the shared executor uses internally
/// (`run_preflight`, `apply_mutation`, `write_journal`) - never a second
/// implementation of preflight or the mutation itself, only different
/// control flow around them.
///
/// # Directory ownership
///
/// Only after every entry passes the whole-batch pre-proof are the
/// quarantine directories this transaction needs created, one at a time. A
/// pre-existing **symlink** at a quarantine directory's path is refused
/// outright, never followed or treated as pre-existing-and-usable - a
/// quarantine directory must always be a real directory EmuWiz can prove it
/// owns or safely ignores. A directory is appended to
/// `transaction.created_directories` **only after `create_dir` succeeds**,
/// and the journal is rewritten durably immediately afterwards - the same
/// contract as
/// [`crate::dat::rom_organisation::transaction::apply_organisation_transaction`].
#[allow(clippy::too_many_arguments)]
pub fn apply_quarantine_transaction(
    transaction: &mut RenameTransaction,
    survivor_path: &Path,
    trusted_root: &Path,
    dat_generation: u64,
    trusted: TrustedRoots,
    journal_dir: &Path,
    cancel: &AtomicBool,
    cache: &mut DuplicateHashCache,
) -> Result<ApplyOutcome, ApplyError> {
    apply_quarantine_transaction_checkpointed(
        transaction,
        survivor_path,
        trusted_root,
        dat_generation,
        trusted,
        journal_dir,
        cancel,
        cache,
        &mut |_| {},
    )
}

/// A fixed point inside [`apply_quarantine_transaction_checkpointed`]'s
/// per-entry apply loop, used only by this module's own tests to
/// synchronize a mutation deterministically into a specific window (for
/// example "after entry 0 has moved, before entry 1's re-proof") instead of
/// racing a background thread against wall-clock scheduling. Never
/// reachable from outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyCheckpoint {
    /// About to re-prove and move `transaction.entries[index]`. Every
    /// earlier entry (if any) has already reached a terminal state
    /// (`Applied` or `ApplyFailed`).
    BeforeEntry { index: usize },
}

/// [`apply_quarantine_transaction`], with a checkpoint callback fired
/// immediately before each entry's own re-proof and move. The callback runs
/// synchronously on the same thread, so a test can mutate the filesystem
/// from inside it and be certain the mutation lands exactly between two
/// specific entries - no thread, no sleep, no scheduler-dependent race.
#[allow(clippy::too_many_arguments)]
fn apply_quarantine_transaction_checkpointed(
    transaction: &mut RenameTransaction,
    survivor_path: &Path,
    trusted_root: &Path,
    dat_generation: u64,
    trusted: TrustedRoots,
    journal_dir: &Path,
    cancel: &AtomicBool,
    cache: &mut DuplicateHashCache,
    checkpoint: &mut dyn FnMut(ApplyCheckpoint),
) -> Result<ApplyOutcome, ApplyError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ApplyError::Cancelled);
    }
    if dat_generation != transaction.plan_generation {
        return Err(ApplyError::StalePlan {
            plan: transaction.plan_generation,
            current: dat_generation,
        });
    }

    // Durable intent, before anything is created or moved.
    transaction.state = TransactionState::Applying;
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    // Layer 1: whole-batch pre-proof. Every entry must independently still
    // prove a distinct-object duplicate of the survivor right now, before
    // anything is created or moved.
    for entry in &transaction.entries {
        if cancel.load(Ordering::Relaxed) {
            transaction.state = TransactionState::ApplyFailed;
            write_journal(journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            return Err(ApplyError::Cancelled);
        }
        if let Err(detail) =
            reprove_entry_against_survivor(entry, survivor_path, cache, &trusted, cancel)
        {
            transaction.state = TransactionState::ApplyFailed;
            write_journal(journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            return Err(ApplyError::Journal(format!(
                "'{}' failed the whole-batch pre-proof: {detail}",
                entry.source_path.display()
            )));
        }
    }

    // Create only the quarantine directories that do not already exist. Each
    // one is appended to `created_directories` and journalled durably as
    // soon as `create_dir` succeeds. A directory that is a symlink is
    // refused outright - never followed, never treated as usable.
    let planned = planned_quarantine_directories(transaction, trusted_root);
    for directory in &planned {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                transaction.state = TransactionState::ApplyFailed;
                write_journal(journal_dir, transaction)
                    .map_err(|error| ApplyError::Journal(error.to_string()))?;
                return Err(ApplyError::Journal(format!(
                    "'{}' is a symlink; refusing to use it as a quarantine directory",
                    directory.display()
                )));
            }
            Ok(_) => {
                // Already present as a real directory (or appeared
                // concurrently as one): pre-existing, never ours.
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(directory) {
                    Ok(()) => {
                        transaction.created_directories.push(directory.clone());
                        write_journal(journal_dir, transaction)
                            .map_err(|error| ApplyError::Journal(error.to_string()))?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Appeared concurrently; pre-existing, not ours. It
                        // could in principle now be a symlink too, but this
                        // create_dir loop never touches it further - the
                        // shared preflight below still refuses any
                        // destination outside the trusted roots.
                    }
                    Err(error) => {
                        transaction.state = TransactionState::ApplyFailed;
                        write_journal(journal_dir, transaction).map_err(|journal_error| {
                            ApplyError::Journal(journal_error.to_string())
                        })?;
                        return Err(ApplyError::Journal(format!(
                            "could not create quarantine directory {}: {error}",
                            directory.display()
                        )));
                    }
                }
            }
            Err(_) => {
                transaction.state = TransactionState::ApplyFailed;
                write_journal(journal_dir, transaction)
                    .map_err(|error| ApplyError::Journal(error.to_string()))?;
                return Err(ApplyError::Journal(format!(
                    "could not inspect quarantine directory {}",
                    directory.display()
                )));
            }
        }
    }

    let approved_paths: BTreeSet<String> = transaction
        .entries
        .iter()
        .map(|entry| entry.source_path.to_string_lossy().into_owned())
        .collect();
    let destinations = batch_destinations(&transaction.entries);
    let preflight_options = PreflightOptions {
        plan_generation: transaction.plan_generation,
        current_generation: dat_generation,
        approved_paths: &approved_paths,
        trusted: &trusted,
        batch_destinations: &destinations,
        directory_policy: DirectoryPolicy::SameFilesystem,
        allow_symlink_source: false,
    };

    // Whole-batch preflight (trusted roots, safe basenames, destination
    // collisions, generation) before any mutation - mirrors
    // `apply_transaction`'s own first pass, in AbortAll mode.
    let mut hard_conflicts: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for entry in &mut transaction.entries {
        if let Err(failures) = run_preflight(entry, &preflight_options) {
            entry.preflight_failures = failures.iter().map(|f| f.reason()).collect();
            entry.preflight_passed = false;
            hard_conflicts.push((entry.source_path.clone(), entry.preflight_failures.clone()));
        } else {
            entry.preflight_passed = true;
        }
    }
    if !hard_conflicts.is_empty() {
        transaction.state = TransactionState::ApplyFailed;
        write_journal(journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
        return Err(ApplyError::HardConflicts(hard_conflicts));
    }
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    // Layer 2: apply one entry at a time, re-proving *this* entry against the
    // survivor immediately before *its own* move - never relying on the
    // whole-batch proof above for any entry after the first.
    for index in 0..transaction.entries.len() {
        if cancel.load(Ordering::Relaxed) {
            transaction.state = TransactionState::ApplyFailed;
            write_journal(journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            let summary = TransactionSummary::from_transaction(transaction);
            return Ok(ApplyOutcome {
                transaction: transaction.clone(),
                summary,
            });
        }

        checkpoint(ApplyCheckpoint::BeforeEntry { index });

        if let Err(detail) = reprove_entry_against_survivor(
            &transaction.entries[index],
            survivor_path,
            cache,
            &trusted,
            cancel,
        ) {
            transaction.entries[index].state = EntryState::ApplyFailed;
            transaction.entries[index].failure_reason = Some(detail);
            transaction.state = TransactionState::ApplyFailed;
            write_journal(journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            break;
        }

        // Re-preflight this one entry: a destination may have appeared, or
        // the batch-wide check above may simply be stale by now.
        if let Err(failures) = run_preflight(&transaction.entries[index], &preflight_options) {
            transaction.entries[index].preflight_passed = false;
            transaction.entries[index].preflight_failures =
                failures.iter().map(|f| f.reason()).collect();
            transaction.entries[index].state = EntryState::ApplyFailed;
            transaction.entries[index].failure_reason = Some(
                failures
                    .iter()
                    .map(|f| f.reason())
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            transaction.state = TransactionState::ApplyFailed;
            write_journal(journal_dir, transaction)
                .map_err(|error| ApplyError::Journal(error.to_string()))?;
            break;
        }
        transaction.entries[index].preflight_passed = true;

        // Durable `Applying` checkpoint before the rename syscall.
        transaction.entries[index].state = EntryState::Applying;
        write_journal(journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;

        match apply_mutation(&transaction.entries[index]) {
            Ok(()) => {
                transaction.entries[index].state = EntryState::Applied;
                transaction.entries[index].applied_at_unix = Some(now_unix());
            }
            Err((state, reason)) => {
                transaction.entries[index].state = state;
                transaction.entries[index].failure_reason = Some(reason);
                transaction.state = TransactionState::ApplyFailed;
                write_journal(journal_dir, transaction)
                    .map_err(|error| ApplyError::Journal(error.to_string()))?;
                break;
            }
        }
        write_journal(journal_dir, transaction)
            .map_err(|error| ApplyError::Journal(error.to_string()))?;
    }

    if transaction.state == TransactionState::Applying {
        transaction.state = TransactionState::Applied;
    }
    write_journal(journal_dir, transaction)
        .map_err(|error| ApplyError::Journal(error.to_string()))?;

    let summary = TransactionSummary::from_transaction(transaction);
    Ok(ApplyOutcome {
        transaction: transaction.clone(),
        summary,
    })
}

/// Re-proves one entry's source against the survivor, live, requiring
/// [`DuplicatePairClassification::DistinctObjects`] and that the source's
/// identity still matches what the entry recorded. A single, narrow check
/// shared by both re-proof layers in [`apply_quarantine_transaction`].
fn reprove_entry_against_survivor(
    entry: &TransactionEntry,
    survivor_path: &Path,
    cache: &mut DuplicateHashCache,
    trusted: &TrustedRoots,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let proof = prove_duplicate_content(
        &entry.source_path,
        survivor_path,
        cache,
        trusted,
        Some(cancel),
    )
    .map_err(|refusal| format!("could not be re-proven a duplicate of the survivor: {refusal}"))?;
    if proof.classification != DuplicatePairClassification::DistinctObjects {
        return Err(
            "is now the same filesystem object as the survivor; refusing to quarantine".to_string(),
        );
    }
    if !identity_matches(&entry.identity, &proof.identity_a) {
        return Err("no longer matches the identity this move was proven for".to_string());
    }
    Ok(())
}

/// The quarantine directories one transaction may need to create, in
/// creation order (the bucket root before any content-hash subdirectory),
/// derived from the entries' destinations. Purely prospective: each becomes
/// owned (and rollback-removable) only once `create_dir` succeeds in
/// [`apply_quarantine_transaction`].
fn planned_quarantine_directories(
    transaction: &RenameTransaction,
    trusted_root: &Path,
) -> Vec<PathBuf> {
    let quarantine_root = trusted_root.join(QUARANTINE_DIRECTORY_NAME);
    let mut planned = vec![quarantine_root.clone()];
    for entry in &transaction.entries {
        if let Some(parent) = entry.destination_path.parent()
            && parent.parent() == Some(quarantine_root.as_path())
            && !planned.contains(&parent.to_path_buf())
        {
            planned.push(parent.to_path_buf());
        }
    }
    planned
}

// ---------------------------------------------------------------------------
// Rollback: shared engine for the moves, then owned-directory cleanup
// ---------------------------------------------------------------------------

/// The outcome of rolling back a quarantine transaction: the shared rollback
/// of the moved entries, plus which quarantine directories this transaction
/// created were removed and which remain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRollbackOutcome {
    pub rollback: RollbackOutcome,
    /// Quarantine directories this transaction created that were removed
    /// (still empty).
    pub directories_removed: Vec<PathBuf>,
    /// Quarantine directories this transaction created that remain (not
    /// empty, or not removable). Never a pre-existing directory.
    pub directories_remaining: Vec<PathBuf>,
}

/// Rolls back a quarantine transaction: the entry moves via the shared
/// rollback engine, then any quarantine directories this transaction created
/// that are now empty.
///
/// A pre-existing directory is never removed, and a directory is only
/// removed when it is empty and sits exactly where an owned quarantine
/// directory must ([`is_owned_quarantine_directory`]) - the same defensive
/// re-check [`crate::dat::rom_organisation::transaction::rollback_organisation_transaction`]
/// performs before trusting `created_directories`.
pub fn rollback_quarantine_transaction(
    transaction: &mut RenameTransaction,
    journal_dir: &Path,
    cancel: &AtomicBool,
    trusted_root: &Path,
) -> Result<QuarantineRollbackOutcome, String> {
    let rollback = rollback_transaction(transaction, journal_dir, cancel)?;
    let quarantine_root = trusted_root.join(QUARANTINE_DIRECTORY_NAME);

    let mut directories_removed = Vec::new();
    let mut directories_remaining = Vec::new();
    for directory in transaction.created_directories.iter().rev() {
        if !is_owned_quarantine_directory(directory, &quarantine_root) {
            directories_remaining.push(directory.clone());
            continue;
        }
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.is_dir() => {
                let is_empty = std::fs::read_dir(directory)
                    .map(|mut read_dir| read_dir.next().is_none())
                    .unwrap_or(false);
                if is_empty {
                    if std::fs::remove_dir(directory).is_ok() {
                        directories_removed.push(directory.clone());
                    } else {
                        directories_remaining.push(directory.clone());
                    }
                } else {
                    directories_remaining.push(directory.clone());
                }
            }
            // Missing (never created, or already gone): nothing to clean.
            _ => {}
        }
    }
    Ok(QuarantineRollbackOutcome {
        rollback,
        directories_removed,
        directories_remaining,
    })
}

/// A directory EmuWiz may remove on rollback: the quarantine root itself, or
/// exactly one safe-basename component directly beneath it.
fn is_owned_quarantine_directory(directory: &Path, quarantine_root: &Path) -> bool {
    if directory == quarantine_root {
        return true;
    }
    directory.parent() == Some(quarantine_root)
        && directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_safe_basename)
}

#[cfg(test)]
mod tests;
