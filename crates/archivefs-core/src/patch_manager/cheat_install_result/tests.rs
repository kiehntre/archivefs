use std::path::Path;

use super::*;
use crate::emulator_environment::EncodedPath;
use crate::patch_manager::cheat_catalogue::{
    CheatCatalogueFormat, CheatGameMatch, CheatGameRecord, CheatInstalledState,
    CheatMatchConfidence, CheatStagingPlan,
};

// ---------------------------------------------------------------------
// Fixture helpers - no filesystem access, no clock reads, no database,
// no RetroArch environment. Every value is supplied directly, matching
// how `plan_cheat_install_entry` only ever reads `entry.staging_plan`.
// ---------------------------------------------------------------------

fn dummy_game_record() -> CheatGameRecord {
    CheatGameRecord {
        source_game_name: "Frogger".to_string(),
        source_platform: Some("Atari2600".to_string()),
        source_region: None,
        source_revision: None,
        source_identifier: None,
        source_content_hash: None,
        target_emulator: Some("retroarch".to_string()),
        cheat_count: 0,
        cheats: Vec::new(),
        enabled_by_default_count: 0,
        source_file_path: EncodedPath::from_path(Path::new("/fixture/catalogue/Frogger.cht")),
        source_file_hash: Some("sourcehash".to_string()),
        format: CheatCatalogueFormat::JsonManifest,
        parsing_complete: true,
        parsing_diagnostics: Vec::new(),
    }
}

fn dummy_game_match(confidence: CheatMatchConfidence) -> CheatGameMatch {
    CheatGameMatch {
        confidence,
        evidence: Vec::new(),
        candidates: Vec::new(),
    }
}

fn destination() -> EncodedPath {
    EncodedPath::from_path(Path::new("/fixture/dest/Atari2600/Frogger.cht"))
}

fn staging_plan(
    action: CheatStagingAction,
    reason: &'static str,
    existing_hash: Option<&str>,
) -> CheatStagingPlan {
    CheatStagingPlan {
        source_cheat_path: EncodedPath::from_path(Path::new("/fixture/catalogue/Frogger.cht")),
        proposed_destination_path: (!matches!(action, CheatStagingAction::NotEligible))
            .then(destination),
        source_file_hash: Some("sourcehash".to_string()),
        existing_destination_hash: existing_hash.map(str::to_string),
        planned_action: action,
        reason,
    }
}

fn availability_entry(
    confidence: CheatMatchConfidence,
    plan: CheatStagingPlan,
) -> CheatAvailabilityEntry {
    CheatAvailabilityEntry {
        game: dummy_game_record(),
        game_match: dummy_game_match(confidence),
        installed_state: CheatInstalledState::Unknown,
        installed_state_detail: Vec::new(),
        staging_candidate: false,
        destructive_if_applied: false,
        staging_plan: plan,
    }
}

fn entry_result_with_outcome(outcome: CheatInstallOutcome) -> CheatInstallEntryResult {
    CheatInstallEntryResult {
        source_path: CheatInstallPath::from_path(Path::new("/fixture/catalogue/Frogger.cht")),
        expected_source_hash: Some("sourcehash".to_string()),
        observed_source_hash: None,
        destination_path: Some(CheatInstallPath::from_path(Path::new(
            "/fixture/dest/Atari2600/Frogger.cht",
        ))),
        previous_destination_state: PreviousDestinationState::Unknown,
        previous_destination_hash: None,
        backup_path: None,
        resulting_destination_hash: None,
        outcome,
        reason_code: "fixture".to_string(),
        detail: Vec::new(),
        applied: false,
        eligible: true,
        write_required: true,
    }
}

// ---------------------------------------------------------------------
// Stable enum serialization
// ---------------------------------------------------------------------

#[test]
fn every_outcome_serializes_to_the_expected_stable_string() {
    let cases = [
        (CheatInstallOutcome::InstalledNew, "\"installed_new\""),
        (
            CheatInstallOutcome::AlreadyInstalled,
            "\"already_installed\"",
        ),
        (
            CheatInstallOutcome::ReplacedWithBackup,
            "\"replaced_with_backup\"",
        ),
        (
            CheatInstallOutcome::SkippedReplaceNotAllowed,
            "\"skipped_replace_not_allowed\"",
        ),
        (
            CheatInstallOutcome::SkippedNotEligible,
            "\"skipped_not_eligible\"",
        ),
        (CheatInstallOutcome::SkippedConflict, "\"skipped_conflict\""),
        (
            CheatInstallOutcome::SkippedSourceChanged,
            "\"skipped_source_changed\"",
        ),
        (
            CheatInstallOutcome::SkippedDestinationChanged,
            "\"skipped_destination_changed\"",
        ),
        (
            CheatInstallOutcome::FailedUnsafePath,
            "\"failed_unsafe_path\"",
        ),
        (CheatInstallOutcome::FailedBackup, "\"failed_backup\""),
        (CheatInstallOutcome::FailedWrite, "\"failed_write\""),
        (
            CheatInstallOutcome::FailedVerification,
            "\"failed_verification\"",
        ),
    ];
    for (outcome, expected) in cases {
        assert_eq!(serde_json::to_string(&outcome).unwrap(), expected);
    }
}

#[test]
fn every_run_status_serializes_correctly() {
    let cases = [
        (CheatInstallRunStatus::Success, "\"success\""),
        (CheatInstallRunStatus::PartialFailure, "\"partial_failure\""),
        (CheatInstallRunStatus::Failed, "\"failed\""),
        (CheatInstallRunStatus::DryRun, "\"dry_run\""),
    ];
    for (status, expected) in cases {
        assert_eq!(serde_json::to_string(&status).unwrap(), expected);
    }
}

#[test]
fn every_previous_destination_state_serializes_correctly() {
    let cases = [
        (PreviousDestinationState::Absent, "\"absent\""),
        (
            PreviousDestinationState::PresentMatchingSource,
            "\"present_matching_source\"",
        ),
        (
            PreviousDestinationState::PresentDifferent,
            "\"present_different\"",
        ),
        (PreviousDestinationState::Unknown, "\"unknown\""),
    ];
    for (state, expected) in cases {
        assert_eq!(serde_json::to_string(&state).unwrap(), expected);
    }
}

// ---------------------------------------------------------------------
// JSON round-trip and schema versioning
// ---------------------------------------------------------------------

#[test]
fn full_run_json_round_trip() {
    let entries = vec![
        availability_entry(
            CheatMatchConfidence::Strong,
            staging_plan(CheatStagingAction::InstallNew, "destination_missing", None),
        ),
        availability_entry(
            CheatMatchConfidence::Weak,
            staging_plan(
                CheatStagingAction::NotEligible,
                "weak_match_not_eligible",
                None,
            ),
        ),
    ];
    let run = plan_cheat_install_run(
        "run-1".to_string(),
        1_700_000_000,
        false,
        Some(Path::new("/fixture/dest")),
        "Fixture Catalogue".to_string(),
        &entries,
    );

    let json = serde_json::to_string_pretty(&run).unwrap();
    let parsed = parse_cheat_install_run(&json).expect("valid schema version round-trips");
    assert_eq!(parsed.run_id, run.run_id);
    assert_eq!(parsed.entries.len(), run.entries.len());
    assert_eq!(parsed.summary, run.summary);
    assert_eq!(parsed.status, run.status);
    // Full structural equality via re-serialization, since `CheatInstallRun`
    // itself does not derive `PartialEq` (its nested `&'static str`-free
    // fields all do, but re-serializing is the simplest whole-structure
    // proof of a lossless round-trip).
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        serde_json::to_string(&run).unwrap()
    );
}

#[test]
fn unsupported_schema_version_rejected() {
    let entries = vec![availability_entry(
        CheatMatchConfidence::Strong,
        staging_plan(CheatStagingAction::InstallNew, "destination_missing", None),
    )];
    let run = plan_cheat_install_run(
        "run-2".to_string(),
        1_700_000_000,
        false,
        None,
        "Fixture Catalogue".to_string(),
        &entries,
    );
    let mut value = serde_json::to_value(&run).unwrap();
    value["schema_version"] = serde_json::json!(9999);
    let json = serde_json::to_string(&value).unwrap();

    let error = parse_cheat_install_run(&json).expect_err("future schema version must be rejected");
    assert_eq!(error, CheatInstallRunSchemaError::UnsupportedVersion(9999));
    assert!(error.to_string().contains("9999"));
}

#[test]
fn malformed_json_is_rejected_clearly() {
    let error = parse_cheat_install_run("{ not json").expect_err("malformed JSON must be rejected");
    assert!(matches!(error, CheatInstallRunSchemaError::Malformed(_)));
}

#[cfg(unix)]
#[test]
fn lossless_non_utf8_path_round_trip() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let raw = OsString::from_vec(b"/fixture/bad-\xFF-name.cht".to_vec());
    let path = CheatInstallPath::from_path(Path::new(&raw));
    assert!(path.lossy);
    assert!(path.display.contains('\u{FFFD}'));

    let json = serde_json::to_string(&path).unwrap();
    let parsed: CheatInstallPath = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, path);
    assert!(parsed.lossy);
}

#[test]
fn encoded_path_conversion_preserves_display_and_lossy() {
    let encoded = EncodedPath::from_path(Path::new("/fixture/catalogue/Frogger.cht"));
    let converted = CheatInstallPath::from(&encoded);
    assert_eq!(converted.display, encoded.display);
    assert_eq!(converted.lossy, encoded.lossy);
}

// ---------------------------------------------------------------------
// Deterministic ordering
// ---------------------------------------------------------------------

#[test]
fn deterministic_entry_ordering_matches_input_order() {
    let mut entries = Vec::new();
    for index in 0..5 {
        let mut plan = staging_plan(CheatStagingAction::InstallNew, "destination_missing", None);
        plan.source_cheat_path =
            EncodedPath::from_path(Path::new(&format!("/fixture/game-{index}.cht")));
        entries.push(availability_entry(CheatMatchConfidence::Strong, plan));
    }
    let results = plan_cheat_install_entries(&entries, false);
    let paths: Vec<String> = results
        .iter()
        .map(|entry| entry.source_path.display.clone())
        .collect();
    let expected: Vec<String> = (0..5)
        .map(|index| format!("/fixture/game-{index}.cht"))
        .collect();
    assert_eq!(paths, expected);

    // Running the same pure mapping again produces byte-identical output.
    let results_again = plan_cheat_install_entries(&entries, false);
    assert_eq!(
        serde_json::to_string(&results).unwrap(),
        serde_json::to_string(&results_again).unwrap()
    );
}

// ---------------------------------------------------------------------
// Derived summary counts
// ---------------------------------------------------------------------

#[test]
fn derived_summary_counts_cannot_drift_from_entries() {
    let entries = vec![
        entry_result_with_outcome(CheatInstallOutcome::InstalledNew),
        entry_result_with_outcome(CheatInstallOutcome::AlreadyInstalled),
        entry_result_with_outcome(CheatInstallOutcome::ReplacedWithBackup),
        entry_result_with_outcome(CheatInstallOutcome::SkippedNotEligible),
        entry_result_with_outcome(CheatInstallOutcome::SkippedConflict),
        entry_result_with_outcome(CheatInstallOutcome::FailedWrite),
    ];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    let rederived = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(
        summary, rederived,
        "re-deriving the summary must be idempotent"
    );

    assert_eq!(summary.requested, 6);
    assert_eq!(summary.installed_new, 1);
    assert_eq!(summary.already_installed, 1);
    assert_eq!(summary.replaced, 1);
    assert_eq!(summary.skipped, 2);
    assert_eq!(summary.failed, 1);
}

#[test]
fn summary_write_required_and_backup_counts() {
    let mut installed = entry_result_with_outcome(CheatInstallOutcome::InstalledNew);
    installed.write_required = true;
    let mut already = entry_result_with_outcome(CheatInstallOutcome::AlreadyInstalled);
    already.write_required = false;
    let mut replaced = entry_result_with_outcome(CheatInstallOutcome::ReplacedWithBackup);
    replaced.write_required = true;
    replaced.backup_path = Some(CheatInstallPath::from_path(Path::new(
        "/fixture/backup/Frogger.cht.bak",
    )));

    let entries = vec![installed, already, replaced];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(summary.writes_required, 2);
    assert_eq!(summary.backups_created, 1);
}

// ---------------------------------------------------------------------
// Dry-run semantics
// ---------------------------------------------------------------------

#[test]
fn dry_run_actions_never_count_as_attempted_or_successful_writes() {
    let mut installed = entry_result_with_outcome(CheatInstallOutcome::InstalledNew);
    installed.write_required = true;
    installed.applied = false;
    let entries = vec![installed];

    let summary = CheatInstallSummary::from_entries(&entries, true);
    assert_eq!(summary.dry_run_actions, 1);
    assert_eq!(summary.writes_attempted, 0);
    assert_eq!(summary.writes_succeeded, 0);
    assert_eq!(summary.writes_required, 1);
}

#[test]
fn real_run_write_attempted_and_succeeded_are_tracked_independently() {
    let mut installed_ok = entry_result_with_outcome(CheatInstallOutcome::InstalledNew);
    installed_ok.write_required = true;
    installed_ok.applied = true;

    let mut installed_failed = entry_result_with_outcome(CheatInstallOutcome::FailedWrite);
    installed_failed.write_required = true;
    installed_failed.applied = false;

    let entries = vec![installed_ok, installed_failed];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(summary.writes_required, 2);
    assert_eq!(summary.writes_attempted, 2);
    assert_eq!(summary.writes_succeeded, 1);
    assert_eq!(summary.dry_run_actions, 0);
}

#[test]
fn dry_run_run_status_is_always_dry_run() {
    let entries = vec![
        entry_result_with_outcome(CheatInstallOutcome::InstalledNew),
        entry_result_with_outcome(CheatInstallOutcome::FailedWrite),
    ];
    let summary = CheatInstallSummary::from_entries(&entries, true);
    assert_eq!(
        CheatInstallRunStatus::derive(&summary, true),
        CheatInstallRunStatus::DryRun
    );
}

#[test]
fn plan_cheat_install_run_is_always_a_dry_run_preview() {
    let entries = vec![availability_entry(
        CheatMatchConfidence::Strong,
        staging_plan(CheatStagingAction::InstallNew, "destination_missing", None),
    )];
    let run = plan_cheat_install_run(
        "run-3".to_string(),
        1_700_000_000,
        false,
        None,
        "Fixture Catalogue".to_string(),
        &entries,
    );
    assert!(run.dry_run);
    assert_eq!(run.status, CheatInstallRunStatus::DryRun);
    assert_eq!(run.summary.writes_attempted, 0);
    assert_eq!(run.summary.writes_succeeded, 0);
    assert!(run.entries.iter().all(|entry| !entry.applied));
    assert_eq!(
        run.started_at_unix_seconds,
        run.completed_at_unix_seconds.unwrap()
    );
}

// ---------------------------------------------------------------------
// Run status derivation (non-dry-run)
// ---------------------------------------------------------------------

#[test]
fn partial_failure_status_is_derived_correctly() {
    let entries = vec![
        entry_result_with_outcome(CheatInstallOutcome::InstalledNew),
        entry_result_with_outcome(CheatInstallOutcome::FailedWrite),
    ];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(
        CheatInstallRunStatus::derive(&summary, false),
        CheatInstallRunStatus::PartialFailure
    );
}

#[test]
fn failed_status_when_nothing_succeeded() {
    let entries = vec![entry_result_with_outcome(CheatInstallOutcome::FailedWrite)];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(
        CheatInstallRunStatus::derive(&summary, false),
        CheatInstallRunStatus::Failed
    );
}

#[test]
fn success_status_when_no_failures() {
    let entries = vec![
        entry_result_with_outcome(CheatInstallOutcome::InstalledNew),
        entry_result_with_outcome(CheatInstallOutcome::AlreadyInstalled),
    ];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(
        CheatInstallRunStatus::derive(&summary, false),
        CheatInstallRunStatus::Success
    );
}

#[test]
fn skipped_not_eligible_alone_is_never_treated_as_a_failure() {
    let entries = vec![
        entry_result_with_outcome(CheatInstallOutcome::SkippedNotEligible),
        entry_result_with_outcome(CheatInstallOutcome::SkippedConflict),
        entry_result_with_outcome(CheatInstallOutcome::SkippedReplaceNotAllowed),
    ];
    let summary = CheatInstallSummary::from_entries(&entries, false);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        CheatInstallRunStatus::derive(&summary, false),
        CheatInstallRunStatus::Success
    );
}

// ---------------------------------------------------------------------
// Pure bridge from staging preview
// ---------------------------------------------------------------------

#[test]
fn install_new_preview_maps_to_eligible_planned_install() {
    let entry = availability_entry(
        CheatMatchConfidence::Strong,
        staging_plan(CheatStagingAction::InstallNew, "destination_missing", None),
    );
    let result = plan_cheat_install_entry(&entry, false);
    assert_eq!(result.outcome, CheatInstallOutcome::InstalledNew);
    assert!(result.eligible);
    assert!(!result.applied);
    assert!(result.write_required);
    assert_eq!(
        result.previous_destination_state,
        PreviousDestinationState::Absent
    );
    assert_eq!(
        result.destination_path.unwrap().display,
        destination().display
    );
}

#[test]
fn already_installed_maps_to_no_write_result() {
    let entry = availability_entry(
        CheatMatchConfidence::Exact,
        staging_plan(
            CheatStagingAction::AlreadyInstalled,
            "hash_match",
            Some("sourcehash"),
        ),
    );
    let result = plan_cheat_install_entry(&entry, false);
    assert_eq!(result.outcome, CheatInstallOutcome::AlreadyInstalled);
    assert!(result.eligible);
    assert!(!result.write_required);
    assert!(!result.applied);
    assert_eq!(
        result.previous_destination_state,
        PreviousDestinationState::PresentMatchingSource
    );
}

#[test]
fn replace_different_without_permission_maps_to_skipped_replace_not_allowed() {
    let entry = availability_entry(
        CheatMatchConfidence::Strong,
        staging_plan(
            CheatStagingAction::ReplaceDifferent,
            "hash_mismatch",
            Some("otherhash"),
        ),
    );
    let result = plan_cheat_install_entry(&entry, false);
    assert_eq!(
        result.outcome,
        CheatInstallOutcome::SkippedReplaceNotAllowed
    );
    assert!(result.eligible);
    assert!(result.write_required);
    assert!(!result.applied);
    assert_eq!(result.reason_code, "replace_different_not_permitted");
    assert_eq!(
        result.previous_destination_state,
        PreviousDestinationState::PresentDifferent
    );
}

#[test]
fn replace_different_with_permission_maps_to_planned_replacement() {
    let entry = availability_entry(
        CheatMatchConfidence::Strong,
        staging_plan(
            CheatStagingAction::ReplaceDifferent,
            "hash_mismatch",
            Some("otherhash"),
        ),
    );
    let result = plan_cheat_install_entry(&entry, true);
    assert_eq!(result.outcome, CheatInstallOutcome::ReplacedWithBackup);
    assert!(result.eligible);
    assert!(result.write_required);
    assert!(!result.applied);
    assert_eq!(result.reason_code, "hash_mismatch");
}

#[test]
fn weak_match_maps_to_skipped_not_eligible() {
    let entry = availability_entry(
        CheatMatchConfidence::Weak,
        staging_plan(
            CheatStagingAction::NotEligible,
            "weak_match_not_eligible",
            None,
        ),
    );
    let result = plan_cheat_install_entry(&entry, true);
    assert_eq!(result.outcome, CheatInstallOutcome::SkippedNotEligible);
    assert!(!result.eligible);
    assert!(!result.write_required);
    assert_eq!(result.reason_code, "weak_match_not_eligible");
}

#[test]
fn ambiguous_match_maps_to_skipped_not_eligible() {
    let entry = availability_entry(
        CheatMatchConfidence::Ambiguous,
        staging_plan(
            CheatStagingAction::NotEligible,
            "ambiguous_match_not_eligible",
            None,
        ),
    );
    let result = plan_cheat_install_entry(&entry, true);
    assert_eq!(result.outcome, CheatInstallOutcome::SkippedNotEligible);
    assert!(!result.eligible);
    assert_eq!(result.reason_code, "ambiguous_match_not_eligible");
}

#[test]
fn conflict_maps_to_skipped_conflict() {
    let entry = availability_entry(
        CheatMatchConfidence::Exact,
        staging_plan(CheatStagingAction::Conflict, "duplicate_destination", None),
    );
    let result = plan_cheat_install_entry(&entry, true);
    assert_eq!(result.outcome, CheatInstallOutcome::SkippedConflict);
    assert!(!result.eligible);
    assert!(!result.write_required);
    assert_eq!(result.reason_code, "duplicate_destination");
    assert_eq!(
        result.previous_destination_state,
        PreviousDestinationState::Unknown
    );
}

#[test]
fn bridge_never_invents_hashes_or_destinations_the_preview_did_not_provide() {
    // `not_eligible` plans never carry a destination path - the bridge
    // must not invent one.
    let entry = availability_entry(
        CheatMatchConfidence::Unsupported,
        staging_plan(
            CheatStagingAction::NotEligible,
            "unsupported_match_not_eligible",
            None,
        ),
    );
    let result = plan_cheat_install_entry(&entry, true);
    assert!(result.destination_path.is_none());
    assert!(result.previous_destination_hash.is_none());
    assert!(result.backup_path.is_none());
    assert!(result.resulting_destination_hash.is_none());
    assert!(result.observed_source_hash.is_none());
}

// ---------------------------------------------------------------------
// Architectural guardrails
// ---------------------------------------------------------------------

#[test]
fn module_source_never_touches_filesystem_network_process_or_database() {
    let source = include_str!("../cheat_install_result.rs");
    for forbidden in [
        "std::fs::",
        "std::process::Command",
        "Command::new",
        "ureq::",
        "reqwest",
        "TcpStream",
        "UdpSocket",
        "Database::open",
        "Database::create",
        "SystemTime::now",
        "Instant::now",
        "env::var(\"HOME\")",
        "env::var(\"XDG_",
        "std::env::home_dir",
        "ReadOnlyHostFilesystem",
    ] {
        assert!(
            !source.contains(forbidden),
            "cheat_install_result.rs must never reference `{forbidden}` - it is a pure data model, \
             not an executor or filesystem reader"
        );
    }
}
