//! Whole-library repair planning on top of the Repair Center.
//!
//! This is the first manually-testable layer above [`crate::repair`]: it turns
//! a whole ROM directory into a read-only scan, a batch of trusted
//! [`super::proposal::RepairProposal`]s, and a human/machine-readable report —
//! without ever re-implementing DAT verification, archive decoding, or rename
//! safety.
//!
//! # The pipeline
//!
//! ```text
//! ROM root
//!   -> run_dat_audit            (existing DAT audit / archive verification)
//!   -> build_rename_plan        (existing hardened DAT rename rules)
//!   -> repair_plan_from_scan_rename_plan (existing adapter + identity capture)
//!   -> LibraryRepairPlan        (serialisable plan file + report)
//!   -> execute_repair_plan      (existing Repair Center transaction layer)
//!   -> reverify_transaction     (existing post-apply re-verification)
//! ```
//!
//! Every mutation goes through the Repair Center executor; this module never
//! calls `std::fs::rename` and never invents a second transaction system.
//!
//! # Profiles
//!
//! A [`RepairProfile`] decides which *executable* repairs a scan may produce.
//! Only [`RepairProfile::CanonicalInPlace`] is implemented for this batch: keep
//! files in their current directory and rename only when the hardened DAT
//! rename rules prove the canonical name. [`RepairProfile::Romm`] is typed but
//! deliberately produces no executable proposals yet.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::dat::limits::DatLimits;
use crate::dat::rename_apply::identity::capture_identity;
use crate::dat::rename_plan::{
    ProposalState, RenamePlan, RenamePlanContext, RenamePlanError, build_rename_plan,
};
use crate::dat::set::SetState;
use crate::dat::sources::DatSourceKind;
use crate::dat::sources::audit_run::{
    DatAuditError, DatAuditOutcome, DatAuditProgress, DatAuditRequest, run_dat_audit,
};
use crate::dat::sources::now_unix;
use crate::safe_read::TrustedRoots;

use super::adapter::repair_proposal_from_suggested_rename;
use super::duplicate_scan::{
    DuplicateNeedsReviewMember, DuplicateScanAccounting, plan_duplicate_quarantine_from_rename_plan,
};
use super::execute::{
    RepairExecutionError, RepairExecutionOptions, RepairTransactionResult, execute_repair_plan,
};
use super::plan::{RepairPlan, RepairPlanId, build_repair_plan, select_repair_plan_subset};
use super::preflight::{RepairPreflightReport, run_repair_preflight};
use super::proposal::{RepairAction, RepairProposalId};

/// The organisation profile a whole-library scan plans for.
///
/// Only [`RepairProfile::CanonicalInPlace`] is executable today;
/// [`RepairProfile::Romm`] is a typed placeholder for a later batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairProfile {
    /// Keep files in their current directory; rename only when the hardened
    /// DAT rename rules prove the canonical name. Same-filesystem, in place.
    CanonicalInPlace,
    /// RomM platform organisation (moves into a platform tree). Not implemented
    /// yet: this variant produces no executable proposals.
    Romm,
}

impl RepairProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::CanonicalInPlace => "canonical-in-place",
            Self::Romm => "romm",
        }
    }

    /// Whether this profile can produce executable repairs in this batch.
    pub fn is_implemented(self) -> bool {
        matches!(self, Self::CanonicalInPlace)
    }

    /// Parses a profile from a CLI flag value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "canonical-in-place" | "canonical" => Some(Self::CanonicalInPlace),
            "romm" => Some(Self::Romm),
            _ => None,
        }
    }
}

/// What a whole-library scan needs: the library root and a DAT catalogue.
#[derive(Debug, Clone)]
pub struct LibraryScanRequest {
    pub source_id: String,
    pub source_display_name: String,
    pub dat_path: PathBuf,
    pub dat_kind: DatSourceKind,
    pub scan_root: PathBuf,
    pub limits: DatLimits,
    pub profile: RepairProfile,
}

/// Why a whole-library scan could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryScanError {
    /// The underlying DAT audit failed.
    Audit(DatAuditError),
    /// The rename plan could not be built from the audit.
    Plan(RenamePlanError),
}

impl std::fmt::Display for LibraryScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audit(error) => write!(f, "audit failed: {error}"),
            Self::Plan(error) => write!(f, "rename plan failed: {error}"),
        }
    }
}

impl std::error::Error for LibraryScanError {}

/// The full result of one whole-library scan.
#[derive(Debug, Clone)]
pub struct LibraryScanOutcome {
    pub generation: u64,
    pub created_at_unix: u64,
    pub profile: RepairProfile,
    pub audit: DatAuditOutcome,
    pub rename_plan: RenamePlan,
    pub repair_plan: RepairPlan,
    pub report: LibraryRepairReport,
}

/// One non-executable report row. Never a mutation candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub path: String,
    pub reason: String,
}

/// One set-resolution report row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetItem {
    pub game_name: String,
    pub reason: String,
}

/// Categorised counts for a whole-library scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCounts {
    pub complete_sets: usize,
    pub incomplete_sets: usize,
    pub bad_metadata_sets: usize,
    pub needs_review_sets: usize,
    pub safe_repairs: usize,
    pub already_canonical: usize,
    pub needs_review: usize,
    pub blocked_repair: usize,
    pub unsupported: usize,
    pub scan_errors: usize,
    /// Files the scan considered relevant to the DAT: every archive the audit
    /// opened ([`crate::dat::sources::audit_run::DatAuditOutcome::archives`],
    /// regardless of what its own outer-container bytes hashed to - an
    /// archive is a candidate because of its *member* evidence, not its
    /// container hash) plus every other walked file whose own content
    /// produced some positive audit verdict. The complement of
    /// [`Self::ignored_ancillary`] within `files_scanned`.
    ///
    /// Additive field (absent in a plan saved before this field existed
    /// deserialises as `0`, not a panic).
    #[serde(default)]
    pub dat_candidates: usize,
    /// Walked files this scan positively determined are **not** DAT-relevant:
    /// not an archive the audit opened, and their own content matched
    /// nothing in the catalogue (`AuditVerdict::NotInDat` /
    /// `NoUsableEvidence`). Never a guess - a file this stage cannot prove
    /// either way (for example one excluded from the flat report entirely,
    /// such as a CHD) is counted as a DAT candidate by default, not ancillary.
    ///
    /// Additive field, defaults to `0` for an old saved plan.
    #[serde(default)]
    pub ignored_ancillary: usize,
    /// `dat_candidates` minus every candidate this report already accounts
    /// for elsewhere (already-canonical, a safe repair, needs-review,
    /// blocked, or unsupported). Purely arithmetic over counts this same
    /// function already computed - not a new classification pass - so it
    /// stays `0` whenever the existing buckets are exhaustive, and only ever
    /// surfaces a real accounting gap rather than fabricating one.
    ///
    /// Additive field, defaults to `0` for an old saved plan.
    #[serde(default)]
    pub unmatched_candidates: usize,
    /// Duplicate-quarantine candidate groups found by verified DAT (game,
    /// rom) identity, before any content proof
    /// ([`crate::repair::DuplicateScanAccounting::groups_examined`]).
    ///
    /// Additive field, defaults to `0` for an old saved plan. Never distorts
    /// [`Self::dat_candidates`], [`Self::safe_repairs`], or any other
    /// existing bucket above - this is a second, orthogonal accounting of
    /// the same scan from the duplicate-quarantine angle.
    #[serde(default)]
    pub duplicate_groups_examined: usize,
    /// Of those, the groups where a unique objective survivor existed and
    /// the other members were content-proven against it (whether or not that
    /// produced a Safe proposal).
    #[serde(default)]
    pub duplicate_groups_content_proven: usize,
    /// Safe `MovePath` quarantine proposals produced (one per redundant
    /// member independently proven a distinct-object duplicate of its
    /// group's survivor). Counted in files, not groups.
    #[serde(default)]
    pub duplicate_quarantine_safe: usize,
    /// Groups with no unique objective survivor; never an executable Safe
    /// proposal for that group. Counted in groups, not files.
    #[serde(default)]
    pub duplicate_quarantine_needs_review: usize,
    /// Members skipped because they are the same filesystem object
    /// (hard-linked) as their group's survivor.
    #[serde(default)]
    pub duplicate_same_object_ignored: usize,
    /// Members skipped because content proof refused for any other reason.
    #[serde(default)]
    pub duplicate_content_mismatch_refused: usize,
}

/// The categorised, non-executable half of a scan report.
///
/// The executable half is [`LibraryRepairPlan::repair_plan`]. This report never
/// hides skipped or unsupported objects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryRepairReport {
    pub counts: ReportCounts,
    pub complete_sets: Vec<SetItem>,
    pub incomplete_sets: Vec<SetItem>,
    pub bad_metadata_sets: Vec<SetItem>,
    pub needs_review_sets: Vec<SetItem>,
    pub needs_review: Vec<PlanItem>,
    pub blocked: Vec<PlanItem>,
    pub unsupported: Vec<PlanItem>,
    /// Unhashed files, unreadable catalogues, and truncation — surfaced, never
    /// hidden, so a partial scan can never be mistaken for a clean one.
    pub scan_errors: Vec<String>,
    /// Lowercase file extension (no leading dot; `"(none)"` when absent) to
    /// count, for every file [`ReportCounts::ignored_ancillary`] counted.
    /// Deterministically ordered (`BTreeMap`) so text and JSON output are
    /// stable across runs. Never used to decide anything - a breakdown of an
    /// already-computed count, not a second classification of the files.
    ///
    /// Additive field, defaults to empty for an old saved plan.
    #[serde(default)]
    pub ignored_ancillary_by_extension: std::collections::BTreeMap<String, usize>,
    /// One row per member of a duplicate-quarantine candidate group that had
    /// no unique objective survivor
    /// ([`ReportCounts::duplicate_quarantine_needs_review`]) - never an
    /// executable proposal, but never silently dropped either. `reason` is
    /// [`super::quarantine::QuarantinePlanRefusal::NeedsReview`]'s own
    /// explanation, verbatim.
    ///
    /// Additive field, defaults to empty for an old saved plan.
    #[serde(default)]
    pub duplicate_needs_review: Vec<PlanItem>,
}

/// The serialisable plan document written to `--plan-out` and read back by
/// `repair plan` / `repair apply`.
///
/// A saved plan is **evidence and proposal data, never permission**: the
/// executor recomputes every global conflict, re-validates every source
/// identity, and refuses a stale generation immediately before any mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryRepairPlan {
    pub profile: String,
    pub generation: u64,
    pub created_at_unix: u64,
    pub source_id: String,
    pub source_display_name: String,
    pub dat_path: String,
    pub scan_root: String,
    pub truncated: bool,
    pub files_scanned: usize,
    /// The executable Safe repairs, as a validated [`RepairPlan`].
    pub repair_plan: RepairPlan,
    /// The categorised non-executable report.
    pub report: LibraryRepairReport,
}

impl LibraryRepairPlan {
    /// The number of executable proposals in the plan.
    pub fn safe_repair_count(&self) -> usize {
        self.repair_plan.proposals.len()
    }

    /// Whether the plan has any executable proposal.
    pub fn has_safe_repairs(&self) -> bool {
        !self.repair_plan.proposals.is_empty()
    }
}

/// Runs a whole-library scan: audit, rename plan, repair plan, report.
///
/// Read-only. `on_progress` is forwarded to the audit exactly as the GUI
/// forwards it, so a caller can show progress without coupling to the audit.
pub fn run_library_scan(
    request: &LibraryScanRequest,
    trusted: &TrustedRoots,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(DatAuditProgress),
) -> Result<LibraryScanOutcome, LibraryScanError> {
    let generation = dat_generation(&request.dat_path);
    let created_at = now_unix();

    let audit_request = DatAuditRequest {
        source_id: request.source_id.clone(),
        source_display_name: request.source_display_name.clone(),
        dat_path: request.dat_path.clone(),
        dat_kind: request.dat_kind,
        scan_root: request.scan_root.clone(),
        limits: request.limits,
        policy: None,
        platform: None,
    };
    let audit = run_dat_audit(&audit_request, trusted, cancel, on_progress)
        .map_err(LibraryScanError::Audit)?;

    let rename_plan = build_rename_plan(&audit, &RenamePlanContext { generation }, cancel)
        .map_err(LibraryScanError::Plan)?;

    // CanonicalInPlace maps 1:1 onto the existing adapter: only a Suggested,
    // collision-free, regular-file rename becomes an executable Repair proposal.
    // A loose file's audited identity is captured here, at scan time, because
    // the hardened rename plan records identity only for whole outer archives.
    // Duplicate-quarantine proposals are additive on top of that, bridged from
    // the same rename plan (see `duplicate_scan`). Romm is typed but produces
    // nothing executable yet, so it also gets no quarantine proposals.
    let (repair_plan, duplicate_accounting, duplicate_needs_review) =
        if request.profile.is_implemented() {
            repair_plan_from_scan_rename_plan(&rename_plan, created_at, trusted, cancel)
        } else {
            (
                build_repair_plan(
                    RepairPlanId::new(format!(
                        "library-{}-{generation}",
                        request.source_id.replace(['/', '\\'], "_")
                    ))
                    .unwrap_or_else(|| RepairPlanId::new("library-scan").expect("static id")),
                    generation,
                    created_at,
                    Some(request.scan_root.to_string_lossy().into_owned()),
                    Vec::new(),
                ),
                DuplicateScanAccounting::default(),
                Vec::new(),
            )
        };

    let report = build_library_repair_report(
        &audit,
        &rename_plan,
        &repair_plan,
        &duplicate_accounting,
        &duplicate_needs_review,
    );

    Ok(LibraryScanOutcome {
        generation,
        created_at_unix: created_at,
        profile: request.profile,
        audit,
        rename_plan,
        repair_plan,
        report,
    })
}

/// A deterministic, DAT-derived generation stamp.
///
/// The same DAT file (or folder) yields the same generation, so a plan built
/// from it can be independently re-proven at apply time, and a changed DAT
/// yields a different (stale) generation. This is a non-cryptographic identity
/// of the DAT's path + size + modification time, never a wall clock.
fn dat_generation(dat_path: &Path) -> u64 {
    let mut data: Vec<u8> = dat_path.to_string_lossy().as_bytes().to_vec();
    if let Ok(meta) = std::fs::metadata(dat_path) {
        data.extend_from_slice(&meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified()
            && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            data.extend_from_slice(&elapsed.as_secs().to_le_bytes());
            data.extend_from_slice(&elapsed.subsec_nanos().to_le_bytes());
        }
    }
    fnv1a64(&data)
}

/// FNV-1a 64-bit. Deterministic and non-cryptographic; used only to stamp a
/// DAT's identity into a generation number.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Converts a hardened DAT rename plan into an executable [`RepairPlan`],
/// reusing the existing adapter while capturing a loose file's audited
/// identity at scan time.
///
/// The hardened rename plan records [`RenameProposal::audited_identity`] only
/// for whole outer archives. A loose-file rename is just as identity-bound at
/// execution time, so this captures each loose source's identity here — while
/// the file is still the exact object the audit verified — never later.
fn repair_plan_from_scan_rename_plan(
    plan: &RenamePlan,
    created_at_unix: u64,
    trusted: &TrustedRoots,
    cancel: &AtomicBool,
) -> (
    RepairPlan,
    DuplicateScanAccounting,
    Vec<DuplicateNeedsReviewMember>,
) {
    let mut proposals = Vec::new();
    for proposal in &plan.proposals {
        let Some(mut repair) = repair_proposal_from_suggested_rename(proposal, plan.generation)
        else {
            continue;
        };
        if repair.expected_source_identity.is_none() {
            repair.expected_source_identity = capture_identity(&proposal.source_path).ok();
        }
        // The adapter carries the rename plan's policy explanations verbatim; a
        // single exact match has none, so fill in a readable reason here.
        if repair.reason.trim().is_empty() {
            if proposal.is_outer_archive {
                repair.reason = format!(
                    "verified whole archive against DAT set '{}'",
                    proposal.game_name.as_deref().unwrap_or("unknown")
                );
            } else {
                repair.reason = format!(
                    "verified {} match: {} / {}",
                    proposal.verdict_label,
                    proposal.game_name.as_deref().unwrap_or("unknown"),
                    proposal.rom_name.as_deref().unwrap_or("unknown")
                );
            }
        }
        proposals.push(repair);
    }

    // Additive: duplicate-quarantine proposals bridged from this same rename
    // plan, using the real scan root as the trusted root - never a second
    // scan, never EmuWiz state. This never removes or changes a DAT rename
    // proposal above; `detect_plan_conflicts` (run inside `build_repair_plan`
    // below) sees both sets together, so a source that is both a DAT rename
    // target and a quarantine-move source fails closed as an ordinary
    // `DuplicateSource` conflict rather than being silently allowed.
    let scan_root = Path::new(&plan.scan_root);
    let (quarantine_proposals, duplicate_accounting, duplicate_needs_review) =
        plan_duplicate_quarantine_from_rename_plan(plan, scan_root, trusted, Some(cancel));
    proposals.extend(quarantine_proposals);

    let id = RepairPlanId::new(format!(
        "dat-{}-{}",
        plan.source_id.replace(['/', '\\'], "_"),
        plan.generation
    ))
    .unwrap_or_else(|| RepairPlanId::new("dat-rename").expect("static id"));
    let repair_plan = build_repair_plan(
        id,
        plan.generation,
        created_at_unix,
        Some(plan.scan_root.clone()),
        proposals,
    );
    (repair_plan, duplicate_accounting, duplicate_needs_review)
}

/// Builds the categorised report from the audit, the rename plan, and the
/// executable repair plan. Pure: no filesystem access.
pub fn build_library_repair_report(
    audit: &DatAuditOutcome,
    rename_plan: &RenamePlan,
    repair_plan: &RepairPlan,
    duplicate_accounting: &DuplicateScanAccounting,
    duplicate_needs_review: &[DuplicateNeedsReviewMember],
) -> LibraryRepairReport {
    let mut report = LibraryRepairReport::default();

    // Files encountered vs DAT-relevant vs ignored ancillary. Derived purely
    // from evidence the audit already produced - no new file walk, no new
    // hashing, no change to matching semantics.
    //
    // An archive (zip/7z/rar) the audit opened is a DAT candidate because of
    // its *member* evidence, even though its own outer-container bytes were
    // also hashed into `audit.report` and will almost always show
    // `NotInDat` there (a compressed container's raw bytes never equal an
    // uncompressed ROM's declared hash) - so archive paths are excluded from
    // the ancillary count by construction, not by verdict.
    //
    // A file this stage cannot see at all in `audit.report.entries` (a CHD,
    // audited only through header identity, never through this flat pass)
    // is counted as a DAT candidate by default: this reporting layer proves
    // "ancillary" only from a positive `NotInDat`/`NoUsableEvidence`
    // verdict, and never treats "absent from this list" as "junk".
    let archive_paths: std::collections::HashSet<&Path> = audit
        .archives
        .iter()
        .map(|archive| archive.archive_path.as_path())
        .collect();
    for entry in &audit.report.entries {
        if archive_paths.contains(Path::new(entry.local_path.as_str())) {
            continue;
        }
        if matches!(
            entry.verdict,
            crate::dat::audit::AuditVerdict::NotInDat
                | crate::dat::audit::AuditVerdict::NoUsableEvidence
        ) {
            report.counts.ignored_ancillary += 1;
            *report
                .ignored_ancillary_by_extension
                .entry(ancillary_extension(&entry.local_path))
                .or_insert(0) += 1;
        }
    }
    report.counts.dat_candidates = audit
        .files_scanned
        .saturating_sub(report.counts.ignored_ancillary);

    // Per-file classification from the hardened rename plan. Only `Suggested`
    // proposals become executable; every other state is surfaced verbatim.
    for proposal in &rename_plan.proposals {
        match proposal.state {
            ProposalState::Suggested => {}
            ProposalState::AlreadyCanonical => report.counts.already_canonical += 1,
            ProposalState::Ambiguous | ProposalState::UnclassifiedContent => {
                report.counts.needs_review += 1;
                report.needs_review.push(PlanItem {
                    path: proposal.source_path.to_string_lossy().into_owned(),
                    reason: proposal
                        .ambiguity_reason
                        .clone()
                        .unwrap_or_else(|| "ambiguous DAT attribution".to_string()),
                });
            }
            ProposalState::Conflict | ProposalState::Blocked => {
                report.counts.blocked_repair += 1;
                let reason = proposal
                    .blockers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "blocked by a rename-plan conflict".to_string());
                report.blocked.push(PlanItem {
                    path: proposal.source_path.to_string_lossy().into_owned(),
                    reason,
                });
            }
            ProposalState::Unsupported | ProposalState::ExcludedByContentPolicy => {
                report.counts.unsupported += 1;
                report.unsupported.push(PlanItem {
                    path: proposal.source_path.to_string_lossy().into_owned(),
                    reason: proposal
                        .blockers
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "unsupported or excluded by content policy".to_string()),
                });
            }
        }
    }

    // Set completeness from the audit's own resolutions.
    for resolution in &audit.sets {
        match &resolution.state {
            SetState::Complete => {
                report.counts.complete_sets += 1;
                report.complete_sets.push(SetItem {
                    game_name: resolution.identity.game_name.clone(),
                    reason: "storage-complete set".to_string(),
                });
            }
            SetState::Incomplete => {
                report.counts.incomplete_sets += 1;
                report.incomplete_sets.push(SetItem {
                    game_name: resolution.identity.game_name.clone(),
                    reason: "one or more required members are absent or unverified".to_string(),
                });
            }
            SetState::BadMetadata(reason) => {
                report.counts.bad_metadata_sets += 1;
                report.bad_metadata_sets.push(SetItem {
                    game_name: resolution.identity.game_name.clone(),
                    reason: format!("bad metadata: {reason:?}"),
                });
            }
            SetState::NeedsReview(reason) => {
                report.counts.needs_review_sets += 1;
                report.needs_review_sets.push(SetItem {
                    game_name: resolution.identity.game_name.clone(),
                    reason: format!("needs review: {reason:?}"),
                });
            }
        }
    }

    // Scan errors: unhashed files, unreadable catalogues, truncation.
    for unhashed in &audit.unhashed {
        report.scan_errors.push(format!(
            "unhashed {} ({}): {}",
            unhashed.path, unhashed.code, unhashed.detail
        ));
    }
    for catalogue in &audit.unreadable_catalogues {
        report
            .scan_errors
            .push(format!("unreadable catalogue: {catalogue}"));
    }
    if audit.truncated {
        report
            .scan_errors
            .push("the scan hit a ceiling and covers only part of the folder".to_string());
    }
    report.counts.scan_errors = report.scan_errors.len();

    // The executable DAT rename proposals are the repair plan's `RenamePath`
    // proposals; it is authoritative (the adapter drops any Suggested
    // proposal it refused, e.g. a missing identity). `MovePath` proposals in
    // the same plan are duplicate-quarantine moves, accounted for separately
    // below so this count keeps its original, DAT-rename-only meaning.
    report.counts.safe_repairs = repair_plan
        .proposals
        .iter()
        .filter(|proposal| matches!(proposal.action, RepairAction::RenamePath { .. }))
        .count();

    // Duplicate-quarantine accounting: purely additive, never folded into any
    // DAT candidate/ancillary/unmatched bucket above.
    report.counts.duplicate_groups_examined = duplicate_accounting.groups_examined;
    report.counts.duplicate_groups_content_proven = duplicate_accounting.groups_content_proven;
    report.counts.duplicate_quarantine_safe = duplicate_accounting.quarantine_safe;
    report.counts.duplicate_quarantine_needs_review = duplicate_accounting.quarantine_needs_review;
    report.counts.duplicate_same_object_ignored = duplicate_accounting.same_object_ignored;
    report.counts.duplicate_content_mismatch_refused =
        duplicate_accounting.content_mismatch_refused;
    report.duplicate_needs_review = duplicate_needs_review
        .iter()
        .map(|member| PlanItem {
            path: member.path.to_string_lossy().into_owned(),
            reason: format!(
                "duplicate group '{} / {}': {}",
                member.game_name.as_deref().unwrap_or("unknown game"),
                member.rom_name.as_deref().unwrap_or("unknown rom"),
                member.reason
            ),
        })
        .collect();

    // Purely arithmetic, over counts this function already finished
    // computing above: any DAT candidate not already accounted for by one of
    // the existing buckets. Not a new classification pass, so it cannot
    // disagree with them - it can only surface a real gap if one exists.
    let accounted_for = report
        .counts
        .already_canonical
        .saturating_add(report.counts.safe_repairs)
        .saturating_add(report.counts.needs_review)
        .saturating_add(report.counts.blocked_repair)
        .saturating_add(report.counts.unsupported);
    report.counts.unmatched_candidates = report.counts.dat_candidates.saturating_sub(accounted_for);

    report
}

/// The lowercase extension (no leading dot) of a walked file path, for the
/// ignored-ancillary breakdown. `"(none)"` when the file has none - kept
/// distinct from any real extension string rather than dropped, so a
/// dotless ancillary file is still accounted for in the total.
fn ancillary_extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_else(|| "(none)".to_string())
}

/// Builds the serialisable plan document from a completed scan.
pub fn plan_file_from_scan(scan: &LibraryScanOutcome) -> LibraryRepairPlan {
    LibraryRepairPlan {
        profile: scan.profile.label().to_string(),
        generation: scan.generation,
        created_at_unix: scan.created_at_unix,
        source_id: scan.audit.source_id.clone(),
        source_display_name: scan.audit.source_display_name.clone(),
        dat_path: scan.audit.dat_path.clone(),
        scan_root: scan.audit.scan_root.clone(),
        truncated: scan.audit.truncated,
        files_scanned: scan.audit.files_scanned,
        repair_plan: scan.repair_plan.clone(),
        report: scan.report.clone(),
    }
}

/// The pure dry-run for a saved plan. Re-validates every proposal against the
/// live filesystem without mutating anything.
pub fn preview_library_repair_plan(
    plan: &LibraryRepairPlan,
    current_generation: u64,
) -> RepairPreflightReport {
    run_repair_preflight(&plan.repair_plan, current_generation)
}

/// Applies a saved plan through the existing Repair Center executor.
///
/// The executor recomputes every global conflict, re-validates every source
/// identity, refuses a stale generation, and performs no-clobber journaled
/// renames. The caller supplies the *actual* current generation; a plan whose
/// generation does not match is refused before anything is touched.
pub fn apply_library_repair_plan(
    plan: &LibraryRepairPlan,
    current_generation: u64,
    options: &RepairExecutionOptions,
    cancel: &AtomicBool,
) -> Result<RepairTransactionResult, RepairExecutionError> {
    execute_repair_plan(&plan.repair_plan, current_generation, options, cancel)
}

/// Why a saved plan could not be independently re-proven and applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplySavedPlanError {
    /// The authoritative re-scan failed.
    Scan(LibraryScanError),
    /// The saved plan's executable proposals could not be independently
    /// reproduced from the trusted scan inputs. Nothing was executed.
    NotAuthorized(String),
    /// The freshly re-proven plan contains one or more duplicate-quarantine
    /// proposals. A whole-plan apply never mixes backends automatically: the
    /// generic executor refuses a `MovePath` with `survivor_path` set
    /// outright (see `execute::build_repair_transaction`), and this
    /// foundation deliberately has no orchestration layer that would decide
    /// how to split an *unselected* batch across the rename and
    /// quarantine-specific executors and still fail closed on every
    /// overlap. Use [`apply_saved_plan_selected`] instead, which does that
    /// splitting explicitly. Nothing was executed.
    QuarantineRequiresSelectedApply { count: usize },
    /// The Repair Center executor refused the freshly re-proven plan.
    Execute(RepairExecutionError),
}

impl std::fmt::Display for ApplySavedPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scan(error) => write!(f, "re-scan failed: {error}"),
            Self::NotAuthorized(detail) => write!(f, "saved plan is not authorized: {detail}"),
            Self::QuarantineRequiresSelectedApply { count } => write!(
                f,
                "the plan contains {count} duplicate-quarantine proposal(s); apply them \
                 explicitly with selected apply (`--proposal-id`), not a whole-plan apply"
            ),
            Self::Execute(error) => write!(f, "repair apply failed: {error}"),
        }
    }
}

impl std::error::Error for ApplySavedPlanError {}

/// Re-proves a saved plan against a freshly re-run authoritative scan.
///
/// A saved plan is evidence, never permission. Every executable proposal it
/// names must be independently reproduced by a fresh scan over the trusted
/// inputs before anything may be executed. Any mismatch — in scan root, DAT,
/// generation, or any proposal's source, destination, action, or audited
/// identity — refuses.
pub fn re_prove_saved_plan(
    saved: &LibraryRepairPlan,
    fresh: &LibraryScanOutcome,
) -> Result<(), String> {
    if saved.scan_root != fresh.audit.scan_root {
        return Err(format!(
            "saved scan root '{}' does not match the trusted scan root '{}'",
            saved.scan_root, fresh.audit.scan_root
        ));
    }
    if saved.dat_path != fresh.audit.dat_path {
        return Err(format!(
            "saved DAT '{}' does not match the trusted DAT '{}'",
            saved.dat_path, fresh.audit.dat_path
        ));
    }
    if saved.generation != fresh.generation {
        return Err(format!(
            "saved generation {} does not match the fresh generation {}",
            saved.generation, fresh.generation
        ));
    }
    let saved_proposals = &saved.repair_plan.proposals;
    let fresh_proposals = &fresh.repair_plan.proposals;
    if saved_proposals.len() != fresh_proposals.len() {
        return Err(format!(
            "saved plan names {} repairs but the fresh scan authorizes {}",
            saved_proposals.len(),
            fresh_proposals.len()
        ));
    }
    for (saved_proposal, fresh_proposal) in saved_proposals.iter().zip(fresh_proposals.iter()) {
        if saved_proposal.source_path != fresh_proposal.source_path
            || saved_proposal.action != fresh_proposal.action
            || saved_proposal.expected_source_identity != fresh_proposal.expected_source_identity
            || saved_proposal.survivor_path != fresh_proposal.survivor_path
        {
            return Err(format!(
                "saved proposal for '{}' was not independently reproduced by the fresh scan",
                saved_proposal.source_path.display()
            ));
        }
    }
    Ok(())
}

/// Applies a saved plan by re-running the authoritative scan over trusted
/// inputs, re-proving the saved proposals against it, and executing the freshly
/// authorized plan.
///
/// The saved plan is never executed directly: only the fresh plan produced by
/// this re-scan is handed to the executor, so a tampered destination, action,
/// source, identity, safety state, conflict, generation, or scan root in the
/// saved plan can never authorize a mutation. Refusal happens before any
/// transaction is built, any journal is written, or any file is touched.
pub fn apply_saved_plan(
    saved: &LibraryRepairPlan,
    root: &Path,
    dat: &Path,
    current_generation: u64,
    options: &RepairExecutionOptions,
    cancel: &AtomicBool,
) -> Result<RepairTransactionResult, ApplySavedPlanError> {
    // Re-run the authoritative scan over the trusted inputs. The trusted roots
    // come from `options`, never from the saved plan.
    let fresh = rescan_for_saved_plan(saved, root, dat, &options.trusted, cancel)
        .map_err(ApplySavedPlanError::Scan)?;

    // Re-prove: the saved plan must be independently reproducible.
    re_prove_saved_plan(saved, &fresh).map_err(ApplySavedPlanError::NotAuthorized)?;

    // Fail closed rather than mix backends automatically: see
    // `ApplySavedPlanError::QuarantineRequiresSelectedApply`'s doc.
    let quarantine_count = fresh
        .repair_plan
        .proposals
        .iter()
        .filter(|proposal| proposal.is_duplicate_quarantine())
        .count();
    if quarantine_count > 0 {
        return Err(ApplySavedPlanError::QuarantineRequiresSelectedApply {
            count: quarantine_count,
        });
    }

    // Execute the freshly authorized plan (not the saved plan's proposals).
    execute_repair_plan(&fresh.repair_plan, current_generation, options, cancel)
        .map_err(ApplySavedPlanError::Execute)
}

/// Why a user-selected subset of a saved plan could not be independently
/// re-proven and applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplySavedPlanSelectedError {
    /// The authoritative re-scan failed.
    Scan(LibraryScanError),
    /// The saved plan's executable proposals could not be independently
    /// reproduced from the trusted scan inputs. Nothing was executed. Exactly
    /// the same full-plan check [`apply_saved_plan`] performs — selection
    /// never weakens it.
    NotAuthorized(String),
    /// The selected ids could not be resolved into a safe, executable subset
    /// of the freshly proven plan: empty selection, an unknown id, a
    /// duplicate id, a not-Safe proposal, or any conflict anywhere in the
    /// fresh plan (even one that does not touch a selected proposal).
    InvalidSelection(String),
    /// The Repair Center executor refused the freshly re-proven rename subset.
    Execute(RepairExecutionError),
    /// A selected duplicate-quarantine subset could not be rebuilt against
    /// the live filesystem
    /// ([`super::quarantine::build_quarantine_transaction`] refused), after
    /// zero or more earlier groups in this same call already applied.
    /// `completed` carries every rename batch and quarantine group that
    /// already succeeded before this failure — see this type's module doc:
    /// a selection spanning several duplicate-quarantine survivor groups
    /// applies one transaction per survivor, and an earlier group's success
    /// is real and already durably journaled even though this call as a
    /// whole returns `Err`. A caller that only inspects the error would
    /// otherwise have no way to learn what actually happened on disk.
    QuarantineBuild {
        completed: Box<CombinedApplyResult>,
        detail: String,
    },
    /// The quarantine-specific executor refused the freshly re-proven
    /// quarantine subset
    /// ([`super::quarantine::apply_quarantine_transaction`] refused), after
    /// zero or more earlier groups already applied. `completed` carries the
    /// same already-succeeded results as
    /// [`Self::QuarantineBuild`]'s `completed` field — see its doc.
    QuarantineApply {
        completed: Box<CombinedApplyResult>,
        detail: String,
    },
}

impl std::fmt::Display for ApplySavedPlanSelectedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scan(error) => write!(f, "re-scan failed: {error}"),
            Self::NotAuthorized(detail) => write!(f, "saved plan is not authorized: {detail}"),
            Self::InvalidSelection(detail) => {
                write!(
                    f,
                    "selected proposals could not be safely applied: {detail}"
                )
            }
            Self::Execute(error) => write!(f, "repair apply failed: {error}"),
            Self::QuarantineBuild { detail, .. } => {
                write!(f, "quarantine apply could not be built: {detail}")
            }
            Self::QuarantineApply { detail, .. } => write!(f, "quarantine apply failed: {detail}"),
        }
    }
}

impl std::error::Error for ApplySavedPlanSelectedError {}

/// One duplicate-quarantine group's apply outcome, applied through the
/// quarantine-specific backend
/// ([`super::quarantine::build_quarantine_transaction`] /
/// [`super::quarantine::apply_quarantine_transaction`]) — never the generic
/// repair executor. Shaped exactly like [`RepairTransactionResult`]
/// (transaction, summary, reverify) so a caller reports it the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineApplyResult {
    pub survivor_path: PathBuf,
    pub result: RepairTransactionResult,
}

/// The outcome of a selected-proposal apply that may mix ordinary DAT
/// `RenamePath` proposals with duplicate-quarantine `MovePath` proposals.
///
/// Each kind is executed through its own backend — [`execute_repair_plan`]
/// for renames, [`super::quarantine::build_quarantine_transaction`] /
/// [`super::quarantine::apply_quarantine_transaction`] for quarantine,
/// grouped by survivor — so a `MovePath` never reaches the generic executor
/// (which independently refuses one anyway; see
/// `execute::build_repair_transaction`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CombinedApplyResult {
    /// The ordinary DAT rename batch's result, when the selection included
    /// at least one `RenamePath` proposal.
    pub rename: Option<RepairTransactionResult>,
    /// One entry per distinct survivor among the selected quarantine
    /// proposals, in deterministic (survivor path) order.
    pub quarantine: Vec<QuarantineApplyResult>,
}

/// Applies only a caller-selected subset of a saved plan's proposals, through
/// the same full-plan trust boundary [`apply_saved_plan`] uses.
///
/// The saved plan is *never* executed directly and selection is *never*
/// resolved against saved-plan data:
///
/// 1. The authoritative scan is re-run from `root` and `dat` (never the saved
///    plan's recorded paths).
/// 2. The **entire** saved plan is re-proven against that fresh scan via
///    [`re_prove_saved_plan`] — unchanged, unweakened. A tampered saved
///    proposal refuses here even if it was never selected.
/// 3. Only once the full plan is proven equivalent are `selected_ids`
///    resolved — against the **fresh** plan's proposals only
///    ([`select_repair_plan_subset`]), never against the saved JSON. This is
///    also where a mixed selection fails closed on overlap: the fresh *whole*
///    plan must already be conflict-free (including any `DuplicateSource`
///    conflict between a rename and a quarantine move sharing one source),
///    or selection refuses outright — before it ever reaches the split below.
/// 4. The resulting subset is split by [`super::proposal::RepairProposal::survivor_path`]
///    — `Some` only for a duplicate-quarantine `MovePath`, never for an
///    ordinary rename — and each half is executed through its own backend.
///    Every identity, conflict, journal, rollback, and reverify guarantee
///    from both backends stays intact; nothing here re-implements or
///    weakens either.
pub fn apply_saved_plan_selected(
    saved: &LibraryRepairPlan,
    root: &Path,
    dat: &Path,
    current_generation: u64,
    selected_ids: &[RepairProposalId],
    options: &RepairExecutionOptions,
    cancel: &AtomicBool,
) -> Result<CombinedApplyResult, ApplySavedPlanSelectedError> {
    let fresh = rescan_for_saved_plan(saved, root, dat, &options.trusted, cancel)
        .map_err(ApplySavedPlanSelectedError::Scan)?;

    re_prove_saved_plan(saved, &fresh).map_err(ApplySavedPlanSelectedError::NotAuthorized)?;

    let subset_plan = select_repair_plan_subset(&fresh.repair_plan, selected_ids)
        .map_err(ApplySavedPlanSelectedError::InvalidSelection)?;

    // Split by backend. `survivor_path` is the one authoritative signal a
    // `MovePath` is a duplicate-quarantine move (see its doc); this split is
    // what keeps a quarantine proposal from ever being hardened into the
    // generic executor's transaction alongside an ordinary rename.
    let (quarantine_proposals, rename_proposals): (Vec<_>, Vec<_>) = subset_plan
        .proposals
        .into_iter()
        .partition(|proposal| proposal.is_duplicate_quarantine());

    let mut result = CombinedApplyResult::default();

    if !rename_proposals.is_empty() {
        let rename_plan = build_repair_plan(
            subset_plan.id.clone(),
            subset_plan.generation,
            subset_plan.created_at_unix,
            subset_plan.source_scan_id.clone(),
            rename_proposals,
        );
        result.rename = Some(
            execute_repair_plan(&rename_plan, current_generation, options, cancel)
                .map_err(ApplySavedPlanSelectedError::Execute)?,
        );
    }

    if !quarantine_proposals.is_empty() {
        // Group by survivor: `build_quarantine_transaction`/
        // `apply_quarantine_transaction` each take one survivor path for the
        // whole batch they apply, so a selection spanning several duplicate
        // groups runs one transaction per survivor. One cache is shared
        // across every group in this call, exactly as a whole scan shares one.
        let mut by_survivor: std::collections::BTreeMap<
            PathBuf,
            Vec<super::proposal::RepairProposal>,
        > = std::collections::BTreeMap::new();
        for proposal in quarantine_proposals {
            let survivor_path = proposal
                .survivor_path
                .clone()
                .expect("partitioned by survivor_path.is_some() above");
            by_survivor.entry(survivor_path).or_default().push(proposal);
        }

        let mut cache = super::duplicate::DuplicateHashCache::new();
        for (survivor_path, proposals) in by_survivor {
            // Each group is applied independently through its own journaled
            // quarantine transaction. A later group's build/apply failure
            // must never discard an earlier group's already-applied,
            // already-journaled result: `result` (everything completed so
            // far, including any rename batch above) is carried into the
            // error rather than dropped by an early `?` return. See
            // `ApplySavedPlanSelectedError::QuarantineBuild`/`QuarantineApply`'s
            // doc.
            let mut transaction = match super::quarantine::build_quarantine_transaction(
                &proposals,
                &survivor_path,
                root,
                current_generation,
                &mut cache,
                &options.trusted,
                Some(cancel),
            ) {
                Ok(transaction) => transaction,
                Err(detail) => {
                    return Err(ApplySavedPlanSelectedError::QuarantineBuild {
                        completed: Box::new(result),
                        detail,
                    });
                }
            };

            let outcome = match super::quarantine::apply_quarantine_transaction(
                &mut transaction,
                &survivor_path,
                root,
                current_generation,
                options.trusted.clone(),
                &options.journal_dir,
                cancel,
                &mut cache,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err(ApplySavedPlanSelectedError::QuarantineApply {
                        completed: Box::new(result),
                        detail: error.to_string(),
                    });
                }
            };

            let reverify = super::execute::reverify_transaction(&outcome.transaction);
            result.quarantine.push(QuarantineApplyResult {
                survivor_path,
                result: RepairTransactionResult {
                    transaction: outcome.transaction,
                    summary: outcome.summary,
                    reverify,
                },
            });
        }
    }

    Ok(result)
}

/// Builds the [`LibraryScanRequest`] a saved plan's re-scan needs and runs it.
/// Shared by [`apply_saved_plan`] and [`apply_saved_plan_selected`] so both
/// re-scan from exactly the same trusted `root`/`dat`, never the saved plan's
/// recorded paths.
fn rescan_for_saved_plan(
    saved: &LibraryRepairPlan,
    root: &Path,
    dat: &Path,
    trusted: &TrustedRoots,
    cancel: &AtomicBool,
) -> Result<LibraryScanOutcome, LibraryScanError> {
    let dat_kind = if std::fs::metadata(dat).is_ok_and(|meta| meta.is_dir()) {
        DatSourceKind::Folder
    } else {
        DatSourceKind::File
    };
    let request = LibraryScanRequest {
        source_id: saved.source_id.clone(),
        source_display_name: saved.source_display_name.clone(),
        dat_path: dat.to_path_buf(),
        dat_kind,
        scan_root: root.to_path_buf(),
        limits: DatLimits::default(),
        profile: RepairProfile::CanonicalInPlace,
    };
    run_library_scan(&request, trusted, cancel, &|_| {})
}
