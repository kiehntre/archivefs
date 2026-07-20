//! Data model for a future RetroArch cheat installation run - stable
//! structured types and pure conversion logic only. **This module contains
//! no installer.** Nothing here reads a cheat file's bytes, opens a
//! destination path, creates a directory, writes a file, creates a backup,
//! or writes a journal to disk. Every type below is either constructed
//! directly by a caller (a future, separately-reviewed execution phase) or
//! derived, purely, from an already-computed
//! [`super::cheat_catalogue::CheatAvailabilityEntry`] - the existing
//! read-only staging preview.
//!
//! This module does not resolve, sanitize, or validate any destination
//! path itself, and does not decide whether a symlink is safe to follow.
//! Those decisions remain entirely inside `cheat_catalogue.rs`'s existing
//! staging-preview resolver (and, in the future, whatever safety-checked
//! executor consumes this data model) - this module only *describes*
//! results and journal entries in terms of values that resolver already
//! produced. See `docs/PATCH_CHEAT_MANAGER_DESIGN.md`'s "Phase 3" for the
//! full transactional install/journal/rollback design this is a narrow,
//! forward-compatible slice of; no atomic rename, content-addressed
//! backup, manifest generation, or crash-recovery state machine is
//! implemented or implied by anything here.
//!
//! ## Why a local lossless-path type instead of reusing `EncodedPath`
//!
//! [`crate::emulator_environment::EncodedPath`] - the project's existing
//! lossless-safe path representation - derives `Serialize` but not
//! `Deserialize`. This module's run/journal records need full
//! serialize/deserialize round-tripping (see the schema-version handling
//! below), so [`CheatInstallPath`] mirrors `EncodedPath`'s exact shape and
//! semantics (a lossy-safe UTF-8 `display` string plus a `lossy` flag,
//! never a bare possibly-failing `PathBuf`/`String` conversion) as its own
//! small, independently `Deserialize`-able type, rather than widening the
//! shared type's derive list while a separate module is also actively
//! changing in this area.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::emulator_environment::EncodedPath;

use super::cheat_catalogue::{CheatAvailabilityEntry, CheatStagingAction};

pub const CHEAT_INSTALL_RUN_SCHEMA_VERSION: u32 = 1;

/// Mirrors [`EncodedPath`]'s exact shape - see the module doc comment for
/// why this is a separate type rather than a derive added to that one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatInstallPath {
    pub display: String,
    pub lossy: bool,
}

impl CheatInstallPath {
    pub fn from_path(path: &Path) -> Self {
        match path.to_str() {
            Some(text) => Self {
                display: text.to_string(),
                lossy: false,
            },
            None => Self {
                display: path.to_string_lossy().into_owned(),
                lossy: true,
            },
        }
    }
}

impl From<&EncodedPath> for CheatInstallPath {
    fn from(value: &EncodedPath) -> Self {
        Self {
            display: value.display.clone(),
            lossy: value.lossy,
        }
    }
}

/// What happened (or, for every outcome this milestone's pure bridge can
/// itself produce, what *would* happen) to one catalogue entry. The same
/// outcome value is used for a dry-run preview and a real future
/// execution - see [`CheatInstallEntryResult::applied`] and
/// [`CheatInstallRun::dry_run`] for how those are told apart; the outcome
/// itself is never renamed depending on which one produced it.
///
/// Only [`Self::InstalledNew`], [`Self::AlreadyInstalled`],
/// [`Self::ReplacedWithBackup`] (planned, not yet applied),
/// [`Self::SkippedReplaceNotAllowed`], [`Self::SkippedNotEligible`], and
/// [`Self::SkippedConflict`] are ever produced by
/// [`plan_cheat_install_entries`] in this milestone. The remaining
/// variants (`SkippedSourceChanged`, `SkippedDestinationChanged`, and
/// every `Failed*` variant) exist for a future executor that actually
/// revalidates and writes; nothing in this module can produce them today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatInstallOutcome {
    InstalledNew,
    AlreadyInstalled,
    ReplacedWithBackup,
    SkippedReplaceNotAllowed,
    SkippedNotEligible,
    SkippedConflict,
    /// The source file's content changed between preview and a future
    /// execution attempt (revalidation mismatch). Never produced today -
    /// this milestone performs no revalidation.
    SkippedSourceChanged,
    /// The destination's content changed between preview and a future
    /// execution attempt (revalidation mismatch). Never produced today -
    /// this milestone performs no revalidation.
    SkippedDestinationChanged,
    /// A future executor's own path/symlink safety check rejected the
    /// destination immediately before acting on it. Never produced today -
    /// this module performs no destination probing of its own.
    FailedUnsafePath,
    /// A future executor could not create or verify a pre-replacement
    /// backup. Never produced today.
    FailedBackup,
    /// A future executor's write itself failed (I/O error, disk full,
    /// permission denied, ...). Never produced today.
    FailedWrite,
    /// A future executor's post-write hash verification did not match the
    /// expected content. Never produced today.
    FailedVerification,
}

/// What is known about the destination immediately before a (real or
/// planned) install action, derived only from values the staging preview
/// already computed - never a fresh probe. `Unknown` is the honest answer
/// whenever the preview itself could not determine or trust the
/// destination (`conflict`/`not_eligible`), not a placeholder to be filled
/// in later by guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousDestinationState {
    Absent,
    PresentMatchingSource,
    PresentDifferent,
    Unknown,
}

/// One catalogue entry's installation result - a calculation for every
/// outcome this milestone can produce; a record shape a future real
/// executor can populate for the rest. See the module doc comment: no
/// field here is ever populated by reading, writing, or probing a real
/// file from within this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatInstallEntryResult {
    pub source_path: CheatInstallPath,
    /// The hash the source was expected to have when this result was
    /// planned (from the staging preview's own bounded read at parse
    /// time) - never a fresh read performed by this module.
    pub expected_source_hash: Option<String>,
    /// The hash actually observed immediately before a real write, from a
    /// future executor's own revalidation. Always `None` from
    /// [`plan_cheat_install_entries`] - this milestone never re-reads a
    /// source file.
    pub observed_source_hash: Option<String>,
    pub destination_path: Option<CheatInstallPath>,
    pub previous_destination_state: PreviousDestinationState,
    pub previous_destination_hash: Option<String>,
    /// Where a pre-replacement backup was (or would be) written. Always
    /// `None` from [`plan_cheat_install_entries`] - backup creation is not
    /// implemented anywhere in this codebase yet.
    pub backup_path: Option<CheatInstallPath>,
    /// The destination's content hash after a real write. Always `None`
    /// from [`plan_cheat_install_entries`] - nothing is ever written by
    /// this module.
    pub resulting_destination_hash: Option<String>,
    pub outcome: CheatInstallOutcome,
    /// A stable identifier for why `outcome` was chosen. A plain `String`
    /// rather than this codebase's usual `&'static str` diagnostic-code
    /// convention specifically so this type can round-trip through
    /// `Deserialize` (a `&'static str` cannot); the *set* of values this
    /// module itself produces is still a small, fixed, documented list -
    /// see [`plan_cheat_install_entries`].
    pub reason_code: String,
    pub detail: Vec<String>,
    /// `true` only when a real write was actually carried out. Always
    /// `false` from [`plan_cheat_install_entries`] - this milestone never
    /// applies anything, regardless of `outcome`.
    pub applied: bool,
    /// `true` when the staging preview's match/eligibility rules allowed
    /// this entry to become an actionable install candidate at all
    /// (`install_new`/`already_installed`/`replace_different`) -
    /// independent of whether a run-level policy (e.g.
    /// `allow_replace_different`) then declined to act on it.
    pub eligible: bool,
    /// `true` when carrying out `outcome` would require writing to the
    /// filesystem (a new file or a replacement) - `false` for
    /// `already_installed` and every `skipped`/rejected-before-attempt
    /// outcome.
    pub write_required: bool,
}

/// The result of one whole cheat-installation run over every entry a
/// staging preview produced. Deliberately mirrors
/// `CheatAvailabilityReport`'s shape (format/schema version, ordered
/// entries, a derived summary) rather than inventing a different
/// convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatInstallRun {
    pub schema_version: u32,
    /// Caller-supplied identifier for this run - never generated from a
    /// clock or counter inside this module (see the module doc comment on
    /// determinism).
    pub run_id: String,
    pub started_at_unix_seconds: u64,
    pub completed_at_unix_seconds: Option<u64>,
    pub dry_run: bool,
    pub allow_replace_different: bool,
    pub destination_root: Option<CheatInstallPath>,
    pub catalogue_source: String,
    pub entries: Vec<CheatInstallEntryResult>,
    pub summary: CheatInstallSummary,
    pub status: CheatInstallRunStatus,
}

/// Counts derived entirely from `entries` by [`CheatInstallSummary::from_entries`] -
/// never incremented by hand while entries are being built. Every field
/// here must always equal what re-deriving it from the same `entries`
/// slice would produce; see this module's tests for a standing proof of
/// that invariant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatInstallSummary {
    pub requested: usize,
    pub eligible: usize,
    pub installed_new: usize,
    pub already_installed: usize,
    pub replaced: usize,
    pub skipped: usize,
    pub failed: usize,
    pub backups_created: usize,
    pub writes_required: usize,
    /// Real writes a future executor tried (succeeded or not). Always `0`
    /// for a dry run, by definition - see [`Self::dry_run_actions`].
    pub writes_attempted: usize,
    /// Real writes a future executor confirmed completed. Always `0` for a
    /// dry run.
    pub writes_succeeded: usize,
    /// Entries that would have required a write, previewed under
    /// `dry_run: true` rather than actually attempted.
    pub dry_run_actions: usize,
}

impl CheatInstallSummary {
    /// The only constructor - every field is derived from `entries`, so a
    /// summary can never drift from the results it describes. `dry_run`
    /// must match the enclosing [`CheatInstallRun::dry_run`]; it decides
    /// only whether a required write counts as `dry_run_actions` or as a
    /// (real) `writes_attempted`/`writes_succeeded` candidate - it does
    /// not change `requested`/`eligible`/outcome-bucket counts.
    pub fn from_entries(entries: &[CheatInstallEntryResult], dry_run: bool) -> Self {
        let mut summary = Self {
            requested: entries.len(),
            ..Self::default()
        };
        for entry in entries {
            if entry.eligible {
                summary.eligible += 1;
            }
            match entry.outcome {
                CheatInstallOutcome::InstalledNew => summary.installed_new += 1,
                CheatInstallOutcome::AlreadyInstalled => summary.already_installed += 1,
                CheatInstallOutcome::ReplacedWithBackup => summary.replaced += 1,
                CheatInstallOutcome::SkippedReplaceNotAllowed
                | CheatInstallOutcome::SkippedNotEligible
                | CheatInstallOutcome::SkippedConflict
                | CheatInstallOutcome::SkippedSourceChanged
                | CheatInstallOutcome::SkippedDestinationChanged => summary.skipped += 1,
                CheatInstallOutcome::FailedUnsafePath
                | CheatInstallOutcome::FailedBackup
                | CheatInstallOutcome::FailedWrite
                | CheatInstallOutcome::FailedVerification => summary.failed += 1,
            }
            if entry.backup_path.is_some() {
                summary.backups_created += 1;
            }
            if !entry.write_required {
                continue;
            }
            summary.writes_required += 1;
            if dry_run {
                summary.dry_run_actions += 1;
                continue;
            }
            let attempted = matches!(
                entry.outcome,
                CheatInstallOutcome::InstalledNew
                    | CheatInstallOutcome::ReplacedWithBackup
                    | CheatInstallOutcome::FailedBackup
                    | CheatInstallOutcome::FailedWrite
                    | CheatInstallOutcome::FailedVerification
            );
            if attempted {
                summary.writes_attempted += 1;
                if entry.applied {
                    summary.writes_succeeded += 1;
                }
            }
        }
        summary
    }
}

/// The run's overall disposition - derived, never chosen independently of
/// [`CheatInstallSummary`]/`dry_run`. See [`Self::derive`] for the exact
/// rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatInstallRunStatus {
    Success,
    PartialFailure,
    Failed,
    DryRun,
}

impl CheatInstallRunStatus {
    /// `dry_run` always wins first: a dry run is never reported as
    /// `success`/`failed` in the execution sense, because nothing
    /// executed. Otherwise: any `failed` entry alongside at least one
    /// `installed_new`/`already_installed`/`replaced` entry is
    /// `partial_failure`; any `failed` entry with none of those is
    /// `failed`; zero `failed` entries is `success` - `skipped_*` entries
    /// (including `skipped_not_eligible`) never affect this status on
    /// their own, since declining an ineligible or disallowed entry is not
    /// an execution failure.
    pub fn derive(summary: &CheatInstallSummary, dry_run: bool) -> Self {
        if dry_run {
            return Self::DryRun;
        }
        if summary.failed == 0 {
            return Self::Success;
        }
        let succeeded = summary.installed_new + summary.already_installed + summary.replaced;
        if succeeded > 0 {
            Self::PartialFailure
        } else {
            Self::Failed
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheatInstallRunSchemaError {
    UnsupportedVersion(u32),
    Malformed(String),
}

impl std::fmt::Display for CheatInstallRunSchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported cheat install run schema version {version} (expected {CHEAT_INSTALL_RUN_SCHEMA_VERSION})"
            ),
            Self::Malformed(message) => write!(formatter, "malformed cheat install run: {message}"),
        }
    }
}

impl std::error::Error for CheatInstallRunSchemaError {}

/// Parses and validates a [`CheatInstallRun`] from JSON, rejecting an
/// unsupported `schema_version` explicitly rather than letting an old or
/// future schema silently deserialize into today's field set with the
/// wrong meaning. Prefer this over a bare `serde_json::from_str` wherever
/// a `CheatInstallRun` is read back.
pub fn parse_cheat_install_run(json: &str) -> Result<CheatInstallRun, CheatInstallRunSchemaError> {
    let run: CheatInstallRun = serde_json::from_str(json)
        .map_err(|error| CheatInstallRunSchemaError::Malformed(error.to_string()))?;
    if run.schema_version != CHEAT_INSTALL_RUN_SCHEMA_VERSION {
        return Err(CheatInstallRunSchemaError::UnsupportedVersion(
            run.schema_version,
        ));
    }
    Ok(run)
}

/// The stable reason code [`plan_cheat_install_entries`] attaches when a
/// `replace_different` staging plan is downgraded to
/// `skipped_replace_not_allowed` by run-level policy - distinct from the
/// staging plan's own `reason` (which explains the *match*/*hash*
/// decision, not the run's replace policy).
const REPLACE_DIFFERENT_NOT_PERMITTED_REASON: &str = "replace_different_not_permitted";

/// Maps one already-computed [`CheatAvailabilityEntry`] to a planned
/// [`CheatInstallEntryResult`] - pure, deterministic, and the only
/// function in this module that touches staging-preview data. Performs no
/// filesystem access, no destination probing, and no path
/// resolution/validation of its own: every path, hash, and safety
/// decision it reports was already computed by `cheat_catalogue.rs`'s
/// existing staging-preview resolver. `allow_replace_different` is a pure
/// run-level policy switch, not a filesystem capability check.
pub fn plan_cheat_install_entry(
    entry: &CheatAvailabilityEntry,
    allow_replace_different: bool,
) -> CheatInstallEntryResult {
    let plan = &entry.staging_plan;

    let (outcome, eligible, write_required, reason_code): (
        CheatInstallOutcome,
        bool,
        bool,
        String,
    ) = match plan.planned_action {
        CheatStagingAction::InstallNew => (
            CheatInstallOutcome::InstalledNew,
            true,
            true,
            plan.reason.to_string(),
        ),
        CheatStagingAction::AlreadyInstalled => (
            CheatInstallOutcome::AlreadyInstalled,
            true,
            false,
            plan.reason.to_string(),
        ),
        CheatStagingAction::ReplaceDifferent if allow_replace_different => (
            CheatInstallOutcome::ReplacedWithBackup,
            true,
            true,
            plan.reason.to_string(),
        ),
        CheatStagingAction::ReplaceDifferent => (
            CheatInstallOutcome::SkippedReplaceNotAllowed,
            true,
            true,
            REPLACE_DIFFERENT_NOT_PERMITTED_REASON.to_string(),
        ),
        CheatStagingAction::Conflict => (
            CheatInstallOutcome::SkippedConflict,
            false,
            false,
            plan.reason.to_string(),
        ),
        CheatStagingAction::NotEligible => (
            CheatInstallOutcome::SkippedNotEligible,
            false,
            false,
            plan.reason.to_string(),
        ),
    };

    let previous_destination_state = match plan.planned_action {
        CheatStagingAction::InstallNew => PreviousDestinationState::Absent,
        CheatStagingAction::AlreadyInstalled => PreviousDestinationState::PresentMatchingSource,
        CheatStagingAction::ReplaceDifferent => PreviousDestinationState::PresentDifferent,
        CheatStagingAction::Conflict | CheatStagingAction::NotEligible => {
            PreviousDestinationState::Unknown
        }
    };

    CheatInstallEntryResult {
        source_path: CheatInstallPath::from(&plan.source_cheat_path),
        expected_source_hash: plan.source_file_hash.clone(),
        observed_source_hash: None,
        destination_path: plan
            .proposed_destination_path
            .as_ref()
            .map(CheatInstallPath::from),
        previous_destination_state,
        previous_destination_hash: plan.existing_destination_hash.clone(),
        backup_path: None,
        resulting_destination_hash: None,
        outcome,
        reason_code,
        detail: Vec::new(),
        applied: false,
        eligible,
        write_required,
    }
}

/// Maps every entry in `entries` (in the same, already-deterministic order
/// `build_cheat_availability_report` produced) to a planned
/// [`CheatInstallEntryResult`] - see [`plan_cheat_install_entry`].
pub fn plan_cheat_install_entries(
    entries: &[CheatAvailabilityEntry],
    allow_replace_different: bool,
) -> Vec<CheatInstallEntryResult> {
    entries
        .iter()
        .map(|entry| plan_cheat_install_entry(entry, allow_replace_different))
        .collect()
}

/// Assembles a full, always-`dry_run: true` [`CheatInstallRun`] preview
/// from an already-built list of catalogue entries - the only way this
/// module produces a `CheatInstallRun` today, since no executor exists
/// yet to produce a real one. `run_id` and `started_at_unix_seconds` are
/// caller-supplied (never read from a clock here); `completed_at_unix_seconds`
/// is set to the same value as `started_at_unix_seconds`, since planning a
/// dry run is itself instantaneous relative to any future real execution
/// it previews.
pub fn plan_cheat_install_run(
    run_id: String,
    started_at_unix_seconds: u64,
    allow_replace_different: bool,
    destination_root: Option<&Path>,
    catalogue_source: String,
    entries: &[CheatAvailabilityEntry],
) -> CheatInstallRun {
    let planned_entries = plan_cheat_install_entries(entries, allow_replace_different);
    let summary = CheatInstallSummary::from_entries(&planned_entries, true);
    let status = CheatInstallRunStatus::derive(&summary, true);
    CheatInstallRun {
        schema_version: CHEAT_INSTALL_RUN_SCHEMA_VERSION,
        run_id,
        started_at_unix_seconds,
        completed_at_unix_seconds: Some(started_at_unix_seconds),
        dry_run: true,
        allow_replace_different,
        destination_root: destination_root.map(CheatInstallPath::from_path),
        catalogue_source,
        entries: planned_entries,
        summary,
        status,
    }
}

#[cfg(test)]
mod tests;
