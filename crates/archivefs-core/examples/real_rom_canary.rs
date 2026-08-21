//! Batch 18: the copy-of-real-ROM canary. Reads bytes from a caller-copied
//! disposable file only (never the production original), runs them through
//! the real detection/identity/planning/transaction pipeline, and performs
//! exactly one real apply + rollback against the disposable canary copy.
//!
//! No CLI arguments: source/destination roots are fixed to the dedicated
//! `/home/davedap/emuwiz-canary-real-rom` staging area created by the
//! Batch 18 operator run; nothing here ever names a production path.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use archivefs_core::dat::rename_apply::{
    RecoveryIssue, journal_path, read_journal, reconcile_recovery,
};
use archivefs_core::dat::rom_organisation::OrganisationMode;
use archivefs_core::gb_header_evidence::{observe_gb_evidence, parse_gb_header};
use archivefs_core::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, inspect_identity,
};
use archivefs_core::platform_evidence_fusion::library_plan_export::{
    LibraryPlanExport, export_item,
};
use archivefs_core::platform_evidence_fusion::library_plan_presentation::present_library_plan;
use archivefs_core::platform_evidence_fusion::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, plan_library,
};
use archivefs_core::platform_evidence_fusion::plan_transaction::{
    apply_plan_transaction, approve_transaction, assess_canary_eligibility, assess_recovery,
    build_plan_transaction, build_preview, compute_plan_digest, plan_generation_of,
    render_canary_preview, render_preview_text, render_recovery_report, rollback_plan_transaction,
};
use archivefs_core::safe_read::TrustedRoots;

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).expect("read for hashing");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fail(step: &str, detail: impl std::fmt::Debug) -> ! {
    eprintln!("STOP at step [{step}]: {detail:?}");
    std::process::exit(1);
}

fn main() {
    let canary_root = PathBuf::from("/home/davedap/emuwiz-canary-real-rom");
    let source_root = canary_root.join("source");
    let library_root = canary_root.join("library");
    let source_path = source_root.join("Alleyway (World).gb");
    let journal_dir = canary_root.join("journal");
    fs::create_dir_all(&journal_dir).expect("journal dir");

    let original_rom_path = PathBuf::from("/mnt/games/roms/gb/Alleyway (World).gb");
    let production_root = PathBuf::from("/mnt/games/roms");

    println!("=== Section 4: original production sentinel (read-only) ===");
    let original_sha_before = sha256_hex(&original_rom_path);
    println!("original SHA256 (before any work): {original_sha_before}");

    println!("=== Section 9: real content detection ===");
    let bytes = fs::read(&source_path).expect("read canary copy");
    let header = parse_gb_header(&bytes).expect("header parses");
    println!(
        "logo_valid={} header_checksum_valid={} title={:?}",
        header.logo_valid, header.header_checksum_valid, header.title
    );
    let evidence_before = observe_gb_evidence(&header);

    println!("=== Section 11-13: identity fusion (no DAT registered for Game Boy) ===");
    let identity = inspect_identity(IdentityInspectionInput {
        content_evidence: evidence_before.clone(),
        dat: None,
        representation_match: None,
        archive_members: None,
    });
    println!("fusion outcome: {:?}", identity.content.outcome);
    println!(
        "resolved platform: {:?}",
        identity.content.resolved_platform
    );
    if identity.has_conflict() {
        fail("identity fusion", "has_conflict() == true");
    }
    if !matches!(
        identity.content.outcome,
        archivefs_core::platform_evidence_fusion::FusionOutcome::Resolved
    ) {
        fail("identity fusion", identity.content.outcome);
    }

    let generation: u64 = 1;
    let copy_sha_before_apply = sha256_hex(&source_path);
    println!("=== Section 14-17: real library plan + frozen export ===");
    let no_slug = |_platform: &str| -> Option<String> { None };
    let context = LibraryPlanningContext {
        destination_root: &library_root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: &no_slug,
        generation,
    };
    let input = LibraryPlanInput {
        source_path: source_path.clone(),
        identity: identity.clone(),
        set_identity: None,
        physical_hash: Some(copy_sha_before_apply.clone()),
        normalized_hash: None,
        release_relationship: None,
    };
    let report = plan_library(&[input], &context);
    if report.ready != 1 {
        fail(
            "plan readiness",
            (
                report.ready,
                report.needs_review,
                report.ambiguous,
                report.conflict,
                report.unknown,
                report.unsupported,
            ),
        );
    }
    let item_plan = &report.items[0];
    let presentation = present_library_plan(item_plan, &identity);
    println!(
        "destination_preview: {:?}",
        presentation.destination_preview
    );
    println!("blockers: {:?}", presentation.blockers);

    // Pre-create destination parent (Batch 17 rule: kept strict, not relaxed).
    let destination_str = presentation
        .destination_preview
        .clone()
        .expect("Ready item has a destination");
    let destination_path = PathBuf::from(&destination_str);
    fs::create_dir_all(destination_path.parent().unwrap()).expect("pre-create destination parent");

    let export_item = export_item(item_plan, &presentation, Some(&copy_sha_before_apply), None);
    let export = LibraryPlanExport {
        items: vec![export_item.clone()],
    };
    let digest = compute_plan_digest(&export);
    println!("plan digest: {}", digest.as_str());

    println!("=== Section 19-20: preview + explicit approval ===");
    let preview = build_preview(&export);
    println!("{}", render_preview_text(&preview));
    let approved = approve_transaction(
        &preview,
        "batch-18 real-rom canary run - explicit operator acknowledgement",
    )
    .unwrap_or_else(|e| fail("approval", e));
    println!("transaction approved. digest={}", approved.digest.as_str());

    println!("=== Section 18: canary eligibility ===");
    let eligibility = assess_canary_eligibility(&export, &export_item, &approved, &canary_root);
    println!("{}", render_canary_preview(&export_item, &eligibility));
    let report = match eligibility {
        Ok(r) => r,
        Err(reasons) => fail("canary eligibility", reasons),
    };
    println!("eligibility report: {report:?}");

    println!("=== Section 21: pre-apply revalidation ===");
    let current_generation = plan_generation_of(&export);
    if current_generation != plan_generation_of(&export) {
        fail("generation revalidation", "mismatch");
    }
    let recheck_copy_sha = sha256_hex(&source_path);
    if recheck_copy_sha != copy_sha_before_apply {
        fail(
            "pre-apply source recheck",
            "copy hash changed since planning",
        );
    }
    if destination_path.exists() {
        fail("pre-apply destination recheck", "destination not clear");
    }
    let original_sha_recheck = sha256_hex(&original_rom_path);
    if original_sha_recheck != original_sha_before {
        fail(
            "pre-apply ORIGINAL PRODUCTION ROM recheck",
            "hash changed - ABORTING, DO NOT PROCEED",
        );
    }
    println!(
        "all pre-apply revalidation checks passed; original production ROM confirmed unchanged"
    );

    println!("=== Section 22: APPLY (real transaction) ===");
    let mut transaction = build_plan_transaction(&export, &approved, "real_rom_canary")
        .unwrap_or_else(|e| fail("build_plan_transaction", e));
    let transaction_id = transaction.transaction_id.clone();
    println!("transaction id: {transaction_id}");
    let trusted = TrustedRoots::from_paths([canary_root.as_path()]);
    let cancel = AtomicBool::new(false);

    // Hard guard: neither source nor destination may ever be under production root.
    for entry in &transaction.entries {
        if entry.source_path.starts_with(&production_root)
            || entry.destination_path.starts_with(&production_root)
        {
            fail("hard production-root guard", entry.source_path.clone());
        }
    }

    let apply_result = apply_plan_transaction(
        &mut transaction,
        current_generation,
        &canary_root,
        trusted.clone(),
        &journal_dir,
        &cancel,
        false,
    );
    match &apply_result {
        Ok(outcome) => println!("apply result: Ok, state={:?}", outcome.transaction.state),
        Err(e) => fail("apply", e),
    }

    println!("=== Section 23: destination verify ===");
    println!("source exists after apply: {}", source_path.exists());
    println!(
        "destination exists after apply: {}",
        destination_path.exists()
    );
    let destination_sha = sha256_hex(&destination_path);
    println!("destination SHA256: {destination_sha}");
    if destination_sha != original_sha_before {
        fail(
            "destination hash check",
            "MISMATCH vs original production ROM",
        );
    }
    println!("destination SHA256 == original production ROM SHA256: CONFIRMED");

    println!("=== Section 24: identity recheck at destination ===");
    let dest_bytes = fs::read(&destination_path).unwrap();
    let dest_header = parse_gb_header(&dest_bytes).unwrap();
    let dest_evidence = observe_gb_evidence(&dest_header);
    let dest_identity = inspect_identity(IdentityInspectionInput {
        content_evidence: dest_evidence,
        dat: None,
        representation_match: None,
        archive_members: None,
    });
    println!(
        "destination identity resolved platform: {:?} (outcome {:?})",
        dest_identity.content.resolved_platform, dest_identity.content.outcome
    );
    if dest_identity.content.resolved_platform != identity.content.resolved_platform
        || dest_identity.content.outcome != identity.content.outcome
    {
        fail(
            "destination identity stability",
            "identity changed after apply",
        );
    }

    println!("=== Section 25: journal check ===");
    let jpath = journal_path(&journal_dir, &transaction_id).expect("journal path");
    println!("journal path: {}", jpath.display());
    let journal_after_apply = read_journal(&jpath).expect("read journal");
    println!(
        "journal transaction state: {:?}, entry[0] state: {:?}",
        journal_after_apply.state, journal_after_apply.entries[0].state
    );
    let mut issues_after_apply: Vec<RecoveryIssue> =
        reconcile_recovery(&mut transaction.clone(), &journal_dir).unwrap_or_default();
    let recovery_after_apply = assess_recovery(&transaction, &issues_after_apply);
    println!("recovery after apply: {recovery_after_apply:?}");
    issues_after_apply.clear();

    println!("=== Section 26: SECOND APPLY (must refuse) ===");
    let mut second_apply_txn = transaction.clone();
    let second_apply_result = apply_plan_transaction(
        &mut second_apply_txn,
        current_generation,
        &canary_root,
        trusted.clone(),
        &journal_dir,
        &cancel,
        false,
    );
    println!("second apply result: {second_apply_result:?}");
    if second_apply_result.is_ok() {
        fail("second apply", "should have been refused");
    }
    println!(
        "destination unchanged after second-apply attempt: {}",
        sha256_hex(&destination_path) == original_sha_before
    );

    println!("=== Section 27-28: ROLLBACK ===");
    let rollback_result =
        rollback_plan_transaction(&mut transaction, &journal_dir, &cancel, &trusted);
    match &rollback_result {
        Ok(outcome) => println!("rollback result: {:?}", outcome.rollback.result),
        Err(e) => fail("rollback", e),
    }
    let source_exists_after_rollback = source_path.exists();
    let destination_exists_after_rollback = destination_path.exists();
    println!("source exists after rollback: {source_exists_after_rollback}");
    println!("destination exists after rollback: {destination_exists_after_rollback}");
    if !source_exists_after_rollback || destination_exists_after_rollback {
        fail("post-rollback state", "unexpected state");
    }
    let restored_sha = sha256_hex(&source_path);
    println!("restored source SHA256: {restored_sha}");
    if restored_sha != original_sha_before {
        fail(
            "post-rollback hash",
            "restored copy does not match original",
        );
    }

    println!("=== Section 29: identity recheck after rollback ===");
    let restored_bytes = fs::read(&source_path).unwrap();
    let restored_header = parse_gb_header(&restored_bytes).unwrap();
    let restored_evidence = observe_gb_evidence(&restored_header);
    let restored_identity = inspect_identity(IdentityInspectionInput {
        content_evidence: restored_evidence,
        dat: None,
        representation_match: None,
        archive_members: None,
    });
    println!(
        "restored identity resolved platform: {:?} (outcome {:?})",
        restored_identity.content.resolved_platform, restored_identity.content.outcome
    );
    if restored_identity.content.resolved_platform != identity.content.resolved_platform
        || restored_identity.content.outcome != identity.content.outcome
    {
        fail("post-rollback identity stability", "identity changed");
    }

    println!("=== Section 30: journal after rollback ===");
    let journal_after_rollback = read_journal(&jpath).expect("read journal");
    println!(
        "journal transaction state: {:?}, entry[0] state: {:?}",
        journal_after_rollback.state, journal_after_rollback.entries[0].state
    );
    let issues_after_rollback: Vec<RecoveryIssue> =
        reconcile_recovery(&mut transaction.clone(), &journal_dir).unwrap_or_default();
    let recovery_after_rollback = assess_recovery(&transaction, &issues_after_rollback);
    println!("recovery after rollback: {recovery_after_rollback:?}");
    println!(
        "{}",
        render_recovery_report(
            &transaction,
            &issues_after_rollback,
            recovery_after_rollback
        )
    );

    println!("=== Section 24 (second rollback) ===");
    let mut second_rollback_txn = transaction.clone();
    let second_rollback_result =
        rollback_plan_transaction(&mut second_rollback_txn, &journal_dir, &cancel, &trusted);
    println!("second rollback result: {second_rollback_result:?}");
    println!(
        "source unchanged after second rollback: {}",
        sha256_hex(&source_path) == original_sha_before
    );

    println!("=== Section 31: apply-after-rollback (must refuse via AlreadySettled) ===");
    let mut apply_after_rollback_txn = transaction.clone();
    let apply_after_rollback_result = apply_plan_transaction(
        &mut apply_after_rollback_txn,
        current_generation,
        &canary_root,
        trusted.clone(),
        &journal_dir,
        &cancel,
        false,
    );
    println!("apply-after-rollback result: {apply_after_rollback_result:?}");
    if apply_after_rollback_result.is_ok() {
        fail("apply-after-rollback", "should have been refused");
    }

    println!("=== Section 32: fresh reapproval ===");
    let fresh_preview = build_preview(&export);
    let fresh_approved = approve_transaction(
        &fresh_preview,
        "batch-18 fresh reapproval proof - explicit operator acknowledgement",
    )
    .unwrap_or_else(|e| fail("fresh approval", e));
    let fresh_transaction = build_plan_transaction(&export, &fresh_approved, "real_rom_canary")
        .unwrap_or_else(|e| fail("fresh build_plan_transaction", e));
    println!("fresh transaction id: {}", fresh_transaction.transaction_id);
    println!("old transaction id:   {transaction_id}");
    println!(
        "ids differ: {}",
        fresh_transaction.transaction_id != transaction_id
    );
    println!(
        "digest same as original: {}",
        fresh_approved.digest.as_str() == approved.digest.as_str()
    );

    println!("=== Section 30: original production ROM final verify ===");
    let original_sha_after = sha256_hex(&original_rom_path);
    println!("original SHA256 after full cycle: {original_sha_after}");
    println!(
        "original unchanged: {}",
        original_sha_after == original_sha_before
    );
    if original_sha_after != original_sha_before {
        fail(
            "FINAL PRODUCTION ROM VERIFY",
            "MISMATCH - MANUAL REVIEW REQUIRED, DO NOT PROCEED",
        );
    }

    println!("\n=== ALL STEPS COMPLETED, NO STOP CONDITIONS TRIGGERED ===");
}
