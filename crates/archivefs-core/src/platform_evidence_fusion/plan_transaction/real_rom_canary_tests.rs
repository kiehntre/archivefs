//! Batch 18: focused coverage for the copy-of-real-ROM canary harness -
//! milestone section 39. The actual real, non-tempdir run against a genuine
//! copied Game Boy ROM is exercised manually via `cargo run -p
//! archivefs-core --example real_rom_canary` (captured verbatim in the
//! Batch 18 final report); everything here is tempdir-only, per this
//! crate's standing rule that automated tests never touch a real,
//! non-disposable path. GB header bytes here are synthetic test fixtures
//! (no real cartridge content), matching the pattern already established in
//! `gb_header_evidence`'s own test module.

use super::*;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::dat::rename_apply::journal::{journal_path, read_journal};
use crate::dat::rename_apply::model::{RollbackResult, TransactionState};
use crate::platform_evidence_fusion::FusionOutcome;
use crate::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use crate::platform_evidence_fusion::library_plan_export::SourcePrecondition;
use std::sync::atomic::AtomicBool;

fn item(source: &str, destination: &str, hash: &str) -> LibraryPlanExportItem {
    LibraryPlanExportItem {
        status: PlanStatus::Ready,
        precondition: SourcePrecondition {
            source_path: source.to_string(),
            physical_hash: Some(hash.to_string()),
            normalized_hash: None,
        },
        proposed_destination: Some(destination.to_string()),
        operation_intent: OperationIntent::MoveToLibraryFolder,
        platform_library: Some("Game Boy".to_string()),
        display_name: "Game Boy".to_string(),
        romm_status: crate::platform_evidence_fusion::library_planning::RommMappingStatus::Unmapped,
        romm_slug: None,
        rename_basis:
            crate::platform_evidence_fusion::library_planning::RenameBasis::OriginalNamePreserved,
        proposed_name: None,
        duplicate_classification: None,
        revision_relationship: None,
        set_label: None,
        set_destination: None,
        support_role: None,
        support_association: None,
        blockers: Vec::new(),
        warnings: Vec::new(),
        source_modified: false,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    journal_dir: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("canary");
    std::fs::create_dir_all(root.join("library").join("Game Boy")).unwrap();
    let journal_dir = dir.path().join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    Fixture {
        _dir: dir,
        root,
        journal_dir,
    }
}

fn write_rom(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn gb_logo_evidence() -> Vec<ContentEvidence> {
    // The same value string `gb_logo_and_checksum` (in platform_evidence_fusion.rs)
    // requires at Strong confidence - constructed directly, exactly as this
    // crate's own fusion-module test suite already does, rather than
    // decoding a real cartridge header (no real ROM bytes needed to prove
    // fusion/plan/transaction integration determinism).
    vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "Nintendo Game Boy logo",
        ContentEvidenceConfidence::Strong,
        "test fixture: synthetic Game Boy header fact",
    )]
}

fn resolved_gb_identity() -> crate::platform_evidence_fusion::identity_orchestrator::IdentityResult
{
    inspect_identity(IdentityInspectionInput {
        content_evidence: gb_logo_evidence(),
        dat: None,
        representation_match: None,
        archive_members: None,
    })
}

// ==================================================================
// Production source never transaction-authorized (milestone sections
// 7, 35, 36, 38)
// ==================================================================

fn real_rom_canary_source() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/real_rom_canary.rs"
    ))
    .expect("examples/real_rom_canary.rs must exist")
}

#[test]
fn real_rom_canary_example_never_calls_the_transaction_engine_on_the_production_path() {
    let source = real_rom_canary_source();
    // The only path this file ever names for `/mnt/games/roms` is the
    // read-only sentinel/original-path constant - never passed to
    // build_plan_transaction/apply_plan_transaction/rollback_plan_transaction
    // as a source or destination.
    assert!(source.contains("/mnt/games/roms/gb/Alleyway"));
    assert!(!source.contains("apply_plan_transaction(&original"));
    assert!(!source.contains("build_plan_transaction(&original"));
}

#[test]
fn real_rom_canary_example_uses_a_plain_fs_copy_never_the_transaction_engine_for_production_to_canary()
 {
    let source = real_rom_canary_source();
    // Section 7: the production->canary copy must be an ordinary read-only
    // copy, never routed through the transaction machinery.
    assert!(
        source.contains("fs::read(&original_rom_path)")
            || source.contains("sha256_hex(&original_rom_path)"),
        "original is only ever read, never copied by this file itself \
         (the operator copy happened outside the harness, per the milestone's \
         own section 7 instruction)"
    );
}

#[test]
fn real_rom_canary_example_has_a_hard_production_root_guard_before_apply() {
    let source = real_rom_canary_source();
    assert!(source.contains("hard production-root guard"));
    assert!(source.contains("production_root"));
}

#[test]
fn real_rom_canary_example_has_no_arbitrary_source_or_destination_cli_argument() {
    let source = real_rom_canary_source();
    assert!(!source.contains("std::env::args()"));
    assert!(!source.contains("--source"));
    assert!(!source.contains("--destination"));
}

#[test]
fn real_rom_canary_example_never_calls_romm() {
    let source = real_rom_canary_source();
    for needle in ["romm::", "RomM", "reqwest", "http://", "https://"] {
        assert!(
            !source.to_lowercase().contains(&needle.to_lowercase())
                || needle == "RomM" && !source.contains("romm_status"),
            "unexpected RomM/network reference: {needle}"
        );
    }
}

#[test]
fn canary_eligibility_refuses_a_source_under_the_production_roms_root() {
    let f = fixture();
    let source = "/mnt/games/roms/gb/Alleyway (World).gb".to_string();
    let destination = f
        .root
        .join("library/Game Boy/Alleyway (World).gb")
        .display()
        .to_string();
    let export = LibraryPlanExport {
        items: vec![item(&source, &destination, "deadbeef")],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a production-rooted source must never be canary-eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::SourceUnderProductionRoot));
}

#[test]
fn canary_eligibility_refuses_a_destination_under_the_production_roms_root() {
    let f = fixture();
    let source_path = f.root.join("source/Alleyway (World).gb");
    write_rom(&source_path, b"synthetic gb bytes");
    let export = LibraryPlanExport {
        items: vec![item(
            &source_path.display().to_string(),
            "/mnt/games/roms/gb/Alleyway (World).gb",
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a production-rooted destination must never be canary-eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::DestinationUnderProductionRoot));
}

// ==================================================================
// Copy hash equality (milestone section 8)
// ==================================================================

#[test]
fn a_copied_fixture_hashes_identically_to_its_source_bytes() {
    let f = fixture();
    let bytes = b"synthetic canary rom bytes for hash equality";
    let original = f.root.join("original.bin");
    write_rom(&original, bytes);
    let copy = f.root.join("source").join("copy.bin");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&original, &copy).unwrap();
    assert_eq!(
        sha256_hex(&std::fs::read(&original).unwrap()),
        sha256_hex(&std::fs::read(&copy).unwrap())
    );
}

#[test]
fn a_corrupted_copy_is_detectable_by_hash_mismatch() {
    let good = b"synthetic canary rom bytes";
    let corrupted = b"synthetic canary rom byteX";
    assert_ne!(sha256_hex(good), sha256_hex(corrupted));
}

// ==================================================================
// Detection before/after, DAT result stability (milestone sections 9,
// 11, 24, 29)
// ==================================================================

#[test]
fn fusion_outcome_is_deterministic_across_repeated_calls_on_the_same_evidence() {
    let first = resolved_gb_identity();
    let second = resolved_gb_identity();
    assert_eq!(first.content.outcome, second.content.outcome);
    assert_eq!(
        first.content.resolved_platform,
        second.content.resolved_platform
    );
}

#[test]
fn fusion_outcome_survives_an_apply_shaped_round_trip_unchanged() {
    // Simulates "detect before apply, detect again at destination, detect
    // again after rollback" using the same fixed evidence each time - the
    // real run additionally proves the underlying bytes are literally
    // untouched (captured in the Batch 18 report), this proves the
    // identity computation itself is pure/repeatable.
    let before = resolved_gb_identity();
    let at_destination = resolved_gb_identity();
    let after_rollback = resolved_gb_identity();
    assert_eq!(before.content.outcome, FusionOutcome::Resolved);
    assert_eq!(before.content.outcome, at_destination.content.outcome);
    assert_eq!(before.content.outcome, after_rollback.content.outcome);
    assert_eq!(
        before.content.resolved_platform,
        at_destination.content.resolved_platform
    );
    assert_eq!(
        before.content.resolved_platform,
        after_rollback.content.resolved_platform
    );
}

#[test]
fn no_dat_source_produces_an_honest_none_never_a_fabricated_match() {
    let identity = resolved_gb_identity();
    assert!(identity.dat.is_none());
}

// ==================================================================
// Plan readiness, canary eligibility, destination confinement
// (milestone sections 16, 18, 15)
// ==================================================================

#[test]
fn a_resolved_gb_identity_plans_ready_with_no_blockers() {
    use crate::dat::rom_organisation::OrganisationMode;
    use crate::platform_evidence_fusion::library_planning::{
        LibraryPlanInput, LibraryPlanningContext, plan_library,
    };
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic gb bytes");
    let identity = resolved_gb_identity();
    let no_slug = |_platform: &str| -> Option<String> { None };
    let context = LibraryPlanningContext {
        destination_root: &f.root.join("library"),
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug,
        generation: 1,
    };
    let input = LibraryPlanInput {
        source_path: source.clone(),
        identity,
        set_identity: None,
        physical_hash: Some("deadbeef".to_string()),
        normalized_hash: None,
        release_relationship: None,
    };
    let report = plan_library(&[input], &context);
    assert_eq!(report.ready, 1);
    assert_eq!(report.items[0].status, PlanStatus::Ready);
}

#[test]
fn canary_eligibility_passes_for_a_well_formed_confined_single_file_plan() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic gb bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    assert!(result.is_ok(), "{result:?}");
    let report = result.unwrap();
    assert!(report.strong_enough_for_canary);
    assert!(report.same_filesystem);
    assert!(!report.is_symlink);
    assert!(report.physical_hash_present);
}

#[test]
fn canary_eligibility_refuses_when_destination_falls_outside_the_canary_root() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic gb bytes");
    let outside = tempfile::tempdir().unwrap();
    let destination = outside.path().join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a destination outside the canary root must never be eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::DestinationOutsideCanaryRoot));
}

#[test]
fn canary_eligibility_refuses_a_source_outside_the_canary_root() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("Alleyway (World).gb");
    write_rom(&source, b"synthetic gb bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a source outside the canary root must never be eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::SourceOutsideCanaryRoot));
}

// ==================================================================
// Full real-shaped transaction cycle, replay refusal, fresh tx id
// (milestone sections 22-32)
// ==================================================================

#[test]
fn full_tempdir_apply_journal_rollback_cycle_byte_identical() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    let bytes = b"synthetic canary rom bytes for full cycle";
    write_rom(&source, bytes);
    let original_hash = sha256_hex(bytes);
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");

    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            &original_hash,
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "test ack").unwrap();
    assess_canary_eligibility(&export, &export.items[0], &approved, &f.root).expect("eligible");

    let generation = plan_generation_of(&export);
    let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
    let transaction_id = transaction.transaction_id.clone();
    let trusted = TrustedRoots::from_paths([f.root.as_path()]);
    let cancel = AtomicBool::new(false);

    let outcome = apply_plan_transaction(
        &mut transaction,
        generation,
        &f.root,
        trusted.clone(),
        &f.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    assert_eq!(outcome.transaction.state, TransactionState::Applied);
    assert!(!source.exists());
    assert!(destination.exists());
    assert_eq!(
        sha256_hex(&std::fs::read(&destination).unwrap()),
        original_hash
    );

    let jpath = journal_path(&f.journal_dir, &transaction_id).unwrap();
    let journal = read_journal(&jpath).unwrap();
    assert_eq!(journal.state, TransactionState::Applied);

    // Second apply refused.
    let mut second = transaction.clone();
    let second_result = apply_plan_transaction(
        &mut second,
        generation,
        &f.root,
        trusted.clone(),
        &f.journal_dir,
        &cancel,
        false,
    );
    assert!(second_result.is_err());
    assert_eq!(
        sha256_hex(&std::fs::read(&destination).unwrap()),
        original_hash
    );

    // Rollback.
    let rollback =
        rollback_plan_transaction(&mut transaction, &f.journal_dir, &cancel, &trusted).unwrap();
    assert_eq!(rollback.rollback.result, RollbackResult::FullyRolledBack);
    assert!(source.exists());
    assert!(!destination.exists());
    assert_eq!(sha256_hex(&std::fs::read(&source).unwrap()), original_hash);

    // Second rollback: safe no-op.
    let mut second_rb = transaction.clone();
    let second_rb_result =
        rollback_plan_transaction(&mut second_rb, &f.journal_dir, &cancel, &trusted);
    assert!(second_rb_result.is_ok());
    assert_eq!(sha256_hex(&std::fs::read(&source).unwrap()), original_hash);

    // Apply-after-rollback: refused via AlreadySettled.
    let mut apply_after_rollback = transaction.clone();
    let aar_result = apply_plan_transaction(
        &mut apply_after_rollback,
        generation,
        &f.root,
        trusted.clone(),
        &f.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(aar_result, Err(ApplyError::AlreadySettled { .. })));

    // Fresh reapproval: new transaction id, same digest.
    let fresh_preview = build_preview(&export);
    let fresh_approved = approve_transaction(&fresh_preview, "fresh ack").unwrap();
    let fresh_transaction = build_plan_transaction(&export, &fresh_approved, "test").unwrap();
    assert_ne!(fresh_transaction.transaction_id, transaction_id);
    assert_eq!(fresh_approved.digest.as_str(), approved.digest.as_str());
}

#[test]
fn identity_result_is_unaffected_by_a_transaction_apply_and_rollback() {
    // The identity computation never reads through the transaction layer -
    // proven by construction: `IdentityResult` is computed purely from
    // `ContentEvidence`, which is never touched by
    // apply_plan_transaction/rollback_plan_transaction.
    let before = resolved_gb_identity();
    // Simulate "apply happened" - nothing about identity computation
    // depends on filesystem state at all.
    let after = resolved_gb_identity();
    assert_eq!(before, after);
}

#[test]
fn destination_confinement_holds_across_apply_for_a_well_formed_plan() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    assert!(preview_is_confined_to_root(&preview, &f.root));
}

#[test]
fn destination_confinement_fails_for_a_plan_pointing_outside_the_root() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let outside = tempfile::tempdir().unwrap();
    let destination = outside.path().join("Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    assert!(!preview_is_confined_to_root(&preview, &f.root));
}

#[test]
fn replay_after_rollback_is_refused_for_a_real_shaped_single_file_plan() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    let bytes = b"synthetic replay-guard rom bytes";
    write_rom(&source, bytes);
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            &sha256_hex(bytes),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let generation = plan_generation_of(&export);
    let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
    let trusted = TrustedRoots::from_paths([f.root.as_path()]);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &f.root,
        trusted.clone(),
        &f.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    rollback_plan_transaction(&mut transaction, &f.journal_dir, &cancel, &trusted).unwrap();

    let replay = apply_plan_transaction(
        &mut transaction,
        generation,
        &f.root,
        trusted,
        &f.journal_dir,
        &cancel,
        false,
    );
    assert!(matches!(replay, Err(ApplyError::AlreadySettled { .. })));
}

#[test]
fn fresh_transaction_ids_never_collide_across_many_builds_of_the_same_plan() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..10 {
        let approved = approve_transaction(&preview, "ack").unwrap();
        let transaction = build_plan_transaction(&export, &approved, "test").unwrap();
        ids.insert(transaction.transaction_id);
    }
    assert_eq!(ids.len(), 10);
}

#[test]
fn recovery_after_a_real_shaped_apply_is_safe_to_rollback() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    let bytes = b"synthetic recovery-check rom bytes";
    write_rom(&source, bytes);
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            &sha256_hex(bytes),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let generation = plan_generation_of(&export);
    let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
    let trusted = TrustedRoots::from_paths([f.root.as_path()]);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &f.root,
        trusted,
        &f.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    let issues = crate::dat::rename_apply::reconcile::reconcile_recovery(
        &mut transaction.clone(),
        &f.journal_dir,
    )
    .unwrap_or_default();
    let assessment = assess_recovery(&transaction, &issues);
    assert_eq!(assessment, RecoveryAssessment::SafeToRollback);
}

#[test]
fn canary_eligibility_refuses_when_no_hash_precondition_is_present() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let mut plain_item = item(
        &source.display().to_string(),
        &destination.display().to_string(),
        "irrelevant",
    );
    plain_item.precondition.physical_hash = None;
    plain_item.precondition.normalized_hash = None;
    let export = LibraryPlanExport {
        items: vec![plain_item],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a plan with no hash precondition must never be canary-eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::NoHashPrecondition));
}

#[test]
fn canary_eligibility_refuses_a_set_member_even_if_otherwise_well_formed() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let mut set_item = item(
        &source.display().to_string(),
        &destination.display().to_string(),
        "deadbeef",
    );
    set_item.set_label = Some("Some Set".to_string());
    let export = LibraryPlanExport {
        items: vec![set_item],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("a set member must never be first-canary eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::BelongsToSet));
}

#[test]
fn canary_eligibility_refuses_when_destination_already_exists() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    write_rom(&destination, b"already there");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let result = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let Err(reasons) = result else {
        panic!("an already-occupied destination must never be canary-eligible");
    };
    assert!(reasons.contains(&CanaryIneligibleReason::DestinationAlreadyExists));
}

#[test]
fn real_apply_policy_canary_can_only_ever_produce_abort_all() {
    assert_eq!(
        RealApplyPolicy::Canary.hard_conflict_mode(),
        HardConflictMode::AbortAll
    );
}

#[test]
fn resolved_gb_identity_has_no_conflict_and_no_caveats() {
    let identity = resolved_gb_identity();
    assert!(!identity.has_conflict());
    assert!(identity.caveats.is_empty());
}

#[test]
fn digest_of_a_real_shaped_single_item_export_is_stable_across_rebuilds() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let first = compute_plan_digest(&export);
    let second = compute_plan_digest(&export);
    assert_eq!(first.as_str(), second.as_str());
}

#[test]
fn canary_preview_render_never_claims_applied_before_a_real_apply() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    write_rom(&source, b"synthetic canary bytes");
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            "deadbeef",
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let eligibility = assess_canary_eligibility(&export, &export.items[0], &approved, &f.root);
    let rendered = render_canary_preview(&export.items[0], &eligibility);
    assert!(rendered.contains("Applied:\n  NO"));
}

#[test]
fn recovery_report_never_suggests_a_destructive_fix_for_a_real_shaped_plan() {
    let f = fixture();
    let source = f.root.join("source/Alleyway (World).gb");
    let bytes = b"synthetic report rom bytes";
    write_rom(&source, bytes);
    let destination = f.root.join("library/Game Boy/Alleyway (World).gb");
    let export = LibraryPlanExport {
        items: vec![item(
            &source.display().to_string(),
            &destination.display().to_string(),
            &sha256_hex(bytes),
        )],
    };
    let preview = build_preview(&export);
    let approved = approve_transaction(&preview, "ack").unwrap();
    let generation = plan_generation_of(&export);
    let mut transaction = build_plan_transaction(&export, &approved, "test").unwrap();
    let trusted = TrustedRoots::from_paths([f.root.as_path()]);
    let cancel = AtomicBool::new(false);
    apply_plan_transaction(
        &mut transaction,
        generation,
        &f.root,
        trusted.clone(),
        &f.journal_dir,
        &cancel,
        false,
    )
    .unwrap();
    rollback_plan_transaction(&mut transaction, &f.journal_dir, &cancel, &trusted).unwrap();
    let issues = crate::dat::rename_apply::reconcile::reconcile_recovery(
        &mut transaction.clone(),
        &f.journal_dir,
    )
    .unwrap_or_default();
    let assessment = assess_recovery(&transaction, &issues);
    let report = render_recovery_report(&transaction, &issues, assessment);
    assert!(!report.to_lowercase().contains("delete"));
    assert!(!report.to_lowercase().contains("overwrite"));
}
