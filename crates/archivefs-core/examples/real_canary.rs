//! Batch 17: the first real, non-tempdir apply/rollback cycle through the
//! production transaction path - milestone "MASS TRANSACTION BATCH 17".
//!
//! This is a dedicated, narrowly-scoped harness, deliberately kept out of
//! `transaction_probe` rather than widening that tool's CLI authority
//! (milestone section 35). Every path this binary ever touches is derived
//! from exactly one hardcoded, disposable staging root
//! (`CANARY_ROOT` below) - there is no CLI flag anywhere in this file that
//! accepts an arbitrary source or destination. The canary content is a
//! small synthetic, generated file - never real ROM data - and the run is
//! hard-bound to `RealApplyPolicy::Canary` (`AbortAll` only), one file, and
//! same-filesystem.
//!
//! ```text
//! cargo run -p archivefs-core --example real_canary
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use archivefs_core::dat::rename_apply::executor::ApplyError;
use archivefs_core::platform_evidence_fusion::library_plan_export::{
    LibraryPlanExport, LibraryPlanExportItem, OperationIntent, SourcePrecondition,
};
use archivefs_core::platform_evidence_fusion::library_planning::{
    PlanStatus, RenameBasis, RommMappingStatus,
};
use archivefs_core::platform_evidence_fusion::plan_transaction::{
    RealApplyPolicy, apply_plan_transaction_with_mode, approve_transaction,
    assess_canary_eligibility, assess_recovery, build_plan_transaction, build_preview,
    plan_generation_of, preview_is_confined_to_root, render_canary_preview, render_recovery_report,
    rollback_plan_transaction,
};
use archivefs_core::safe_read::TrustedRoots;
use sha2::{Digest, Sha256};

/// The one and only staging root this binary will ever touch. Never
/// supplied by a caller, never derived from an argument.
const CANARY_ROOT: &str = "/home/davedap/emuwiz-canary";
/// The hardcoded production root this binary refuses to go near, checked
/// independently of `assess_canary_eligibility`'s own internal check -
/// belt and braces (milestone section 34).
const PRODUCTION_ROMS_ROOT: &str = "/mnt/games/roms";

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file for hashing");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fingerprint(label: &str, path: &Path) {
    println!("--- fingerprint: {label} ---");
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                println!("  path:   {}", path.display());
                println!(
                    "  type:   {}",
                    if meta.is_symlink() {
                        "symlink"
                    } else if meta.is_dir() {
                        "dir"
                    } else {
                        "regular file"
                    }
                );
                println!("  size:   {}", meta.len());
                println!("  mode:   {:o}", meta.mode());
                println!("  uid:    {}", meta.uid());
                println!("  gid:    {}", meta.gid());
                println!("  mtime:  {}", meta.mtime());
                println!("  inode:  {}", meta.ino());
                println!("  dev:    {}", meta.dev());
            }
            if meta.is_file() {
                println!("  sha256: {}", sha256_hex(path));
            }
        }
        Err(error) => println!("  absent ({error})"),
    }
}

/// Hard guard (milestone section 34): asserts no path this run is about to
/// touch is ever under the production ROM root. Checked independently at
/// two points: once here before anything is built, and again implicitly by
/// `assess_canary_eligibility`'s own `SourceUnderProductionRoot`/
/// `DestinationUnderProductionRoot` checks.
fn hard_guard_never_production(path: &Path) {
    if path.starts_with(PRODUCTION_ROMS_ROOT) {
        panic!(
            "REFUSING: {} is under the production ROM root {PRODUCTION_ROMS_ROOT} - this must never happen",
            path.display()
        );
    }
}

fn synthetic_export(source: &Path, destination: &Path, hash: &str) -> LibraryPlanExport {
    LibraryPlanExport {
        items: vec![LibraryPlanExportItem {
            status: PlanStatus::Ready,
            precondition: SourcePrecondition {
                source_path: source.display().to_string(),
                physical_hash: Some(hash.to_string()),
                normalized_hash: None,
            },
            proposed_destination: Some(destination.display().to_string()),
            operation_intent: OperationIntent::MoveToLibraryFolder,
            platform_library: Some("TestPlatform".to_string()),
            display_name: "Canary Fixture".to_string(),
            romm_status: RommMappingStatus::Unmapped,
            romm_slug: None,
            rename_basis: RenameBasis::OriginalNamePreserved,
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
        }],
    }
}

fn main() {
    let canary_root = PathBuf::from(CANARY_ROOT);
    hard_guard_never_production(&canary_root);
    assert!(
        !canary_root.starts_with(PRODUCTION_ROMS_ROOT),
        "canary root must never be under the production root"
    );

    println!("=== SECTION 3: CANARY ROOT ===");
    if canary_root.exists() {
        panic!(
            "REFUSING: {} already exists - this harness requires a clean start",
            canary_root.display()
        );
    }
    std::fs::create_dir_all(canary_root.join("source")).unwrap();
    // `assess_canary_eligibility` (Batch 16) deliberately requires the
    // destination's *parent* to already exist - stricter than the general
    // transaction bridge, which is willing to create missing ancestors.
    // So for this canary to be eligible at all, `library/TestPlatform`
    // must be pre-existing; the transaction therefore owns zero
    // directories in this run (see item 37/section 28 in the final
    // report for the honest note on what this does and does not prove
    // about directory ownership).
    std::fs::create_dir_all(canary_root.join("library").join("TestPlatform")).unwrap();
    println!("  canary root: {}", canary_root.display());

    let source = canary_root.join("source").join("canary.bin");
    let destination = canary_root
        .join("library")
        .join("TestPlatform")
        .join("canary.bin");
    hard_guard_never_production(&source);
    hard_guard_never_production(&destination);

    println!("\n=== SECTION 4: CANARY FILE ===");
    // Deterministic, synthetic, non-copyrighted content: 4096 bytes of a
    // repeating counter pattern.
    let contents: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
    std::fs::write(&source, &contents).unwrap();
    println!("  wrote {} bytes to {}", contents.len(), source.display());

    let sentinel = canary_root.join("DO_NOT_TOUCH.txt");
    std::fs::write(
        &sentinel,
        b"unrelated sibling file - must survive byte-identical\n",
    )
    .unwrap();

    println!("\n=== SECTION 5: PRE-CANARY FINGERPRINT ===");
    fingerprint("source (before)", &source);
    fingerprint("destination (before, must be absent)", &destination);
    fingerprint("sentinel (before)", &sentinel);
    let pre_source_hash = sha256_hex(&source);
    let sentinel_hash_before = sha256_hex(&sentinel);
    println!("staging tree listing:");
    for entry in walk(&canary_root) {
        println!("  {}", entry.display());
    }

    println!("\n=== SECTION 6: SAME-FILESYSTEM CHECK ===");
    #[cfg(unix)]
    let (source_dev, dest_dev) = {
        use std::os::unix::fs::MetadataExt;
        let s = std::fs::metadata(source.parent().unwrap()).unwrap().dev();
        let d = std::fs::metadata(destination.parent().unwrap().parent().unwrap())
            .unwrap()
            .dev();
        (s, d)
    };
    #[cfg(unix)]
    {
        println!("  source parent device id:      {source_dev}");
        println!("  destination ancestor device id: {dest_dev}");
        if source_dev != dest_dev {
            eprintln!("STOP: source and destination are on different filesystems");
            cleanup(&canary_root);
            std::process::exit(1);
        }
        println!("  SAME FILESYSTEM: YES");
    }

    println!("\n=== SECTION 7: SYMLINK / HARDLINK CHECK ===");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(&source).unwrap();
        println!("  source is symlink: {}", meta.file_type().is_symlink());
        println!("  source hardlink count: {}", meta.nlink());
        if meta.file_type().is_symlink() {
            eprintln!("STOP: source is a symlink");
            cleanup(&canary_root);
            std::process::exit(1);
        }
        if meta.nlink() != 1 {
            eprintln!("STOP: source hardlink count is not 1; choose a fresh fixture");
            cleanup(&canary_root);
            std::process::exit(1);
        }
    }

    println!("\n=== SECTIONS 8-9: BUILD REAL PLAN ===");
    let export = synthetic_export(&source, &destination, &pre_source_hash);
    println!("  physical hash frozen into precondition: {pre_source_hash}");

    println!("\n=== SECTION 10: CANARY ELIGIBILITY ===");
    let preview = build_preview(&export);
    let plan_digest = preview.digest.as_str().to_string();
    println!("  plan digest: {plan_digest}");
    let approved = approve_transaction(
        &preview,
        "batch-17 real canary run - explicit operator acknowledgement",
    )
    .expect("approval must succeed for a single-op preview");
    println!("  approval acknowledgement: {:?}", approved.acknowledgement);
    let eligibility = assess_canary_eligibility(&export, &export.items[0], &approved, &canary_root);
    match &eligibility {
        Ok(report) => {
            println!("  ELIGIBLE: {report:?}");
        }
        Err(reasons) => {
            eprintln!("STOP: canary ineligible: {reasons:?}");
            cleanup(&canary_root);
            std::process::exit(1);
        }
    }

    println!("\n=== SECTION 11: PREVIEW ===");
    let preview_text = render_canary_preview(&export.items[0], &eligibility);
    println!("{preview_text}");

    println!("\n=== SECTION 12: EXPLICIT APPROVAL BOUNDARY ===");
    if !preview_is_confined_to_root(&preview, &canary_root) {
        eprintln!("STOP: preview is not confined to the canary root");
        cleanup(&canary_root);
        std::process::exit(1);
    }
    let mut transaction = build_plan_transaction(&export, &approved, "real_canary_batch17")
        .expect("build_plan_transaction must succeed for an eligible single-op plan");
    let transaction_id = transaction.transaction_id.clone();
    println!("  transaction id: {transaction_id}");

    println!("\n=== SECTION 13: IMMEDIATE REVALIDATION ===");
    let fresh_export = synthetic_export(&source, &destination, &pre_source_hash);
    let current_generation = plan_generation_of(&fresh_export);
    println!("  recomputed current_generation: {current_generation}");
    println!(
        "  transaction.plan_generation:   {}",
        transaction.plan_generation
    );
    assert_eq!(
        current_generation, transaction.plan_generation,
        "generation must match immediately before apply"
    );

    let journal_dir = canary_root.join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let cancel = AtomicBool::new(false);

    println!("\n=== SECTION 14: APPLY (real, production bridge) ===");
    let mode = RealApplyPolicy::Canary.hard_conflict_mode();
    let apply_result = apply_plan_transaction_with_mode(
        &mut transaction,
        current_generation,
        &canary_root,
        TrustedRoots::from_paths([canary_root.as_path()]),
        &journal_dir,
        &cancel,
        false,
        mode,
    );
    match &apply_result {
        Ok(outcome) => println!(
            "  APPLY OK: transaction state = {:?}",
            outcome.transaction.state
        ),
        Err(error) => {
            eprintln!("STOP: apply failed: {error}");
            report_recovery(&mut transaction, &journal_dir);
            cleanup_after_manual_review(&canary_root);
            std::process::exit(1);
        }
    }

    println!("\n=== SECTION 15: POST-APPLY CHECK ===");
    fingerprint("source (after apply, must be absent)", &source);
    fingerprint("destination (after apply)", &destination);
    let dest_hash_after_apply = sha256_hex(&destination);
    println!("  destination sha256: {dest_hash_after_apply}");
    assert!(!source.exists(), "source must be gone after apply");
    assert!(
        destination.is_file(),
        "destination must be a regular file after apply"
    );
    assert_eq!(
        dest_hash_after_apply, pre_source_hash,
        "destination must be byte-identical to original source"
    );

    println!("\n=== SECTION 16: JOURNAL INSPECTION ===");
    let journal_path =
        archivefs_core::dat::rename_apply::journal::journal_path(&journal_dir, &transaction_id)
            .expect("valid transaction id must produce a journal path");
    println!("  journal path: {}", journal_path.display());
    let journal_raw = std::fs::read_to_string(&journal_path).unwrap();
    println!("  journal contents:\n{journal_raw}");

    println!("\n=== SECTION 17: RECOVERY ASSESSMENT AFTER APPLY ===");
    let (assessment_after_apply, _issues_after_apply) =
        reconcile_and_assess(&mut transaction, &journal_dir);
    println!("  recovery assessment: {assessment_after_apply:?}");

    println!("\n=== SECTION 18: SECOND APPLY REFUSAL ===");
    let second_apply = apply_plan_transaction_with_mode(
        &mut transaction,
        current_generation,
        &canary_root,
        TrustedRoots::from_paths([canary_root.as_path()]),
        &journal_dir,
        &cancel,
        false,
        mode,
    );
    println!("  second apply result: {second_apply:?}");
    assert!(
        second_apply.is_err(),
        "second apply on the same transaction id must be refused"
    );
    let dest_hash_after_second_apply = sha256_hex(&destination);
    assert_eq!(
        dest_hash_after_second_apply, pre_source_hash,
        "destination must be unchanged after refused second apply"
    );

    println!("\n=== SECTIONS 19-20: ROLLBACK ===");
    fingerprint("destination (pre-rollback)", &destination);
    let rollback_result = rollback_plan_transaction(
        &mut transaction,
        &journal_dir,
        &cancel,
        &TrustedRoots::from_paths([canary_root.as_path()]),
    );
    match &rollback_result {
        Ok(outcome) => println!("  ROLLBACK OK: {:?}", outcome.rollback.result),
        Err(error) => {
            eprintln!("STOP: rollback failed: {error}");
            report_recovery(&mut transaction, &journal_dir);
            cleanup_after_manual_review(&canary_root);
            std::process::exit(1);
        }
    }

    println!("\n=== SECTION 21: POST-ROLLBACK CHECK ===");
    fingerprint("source (after rollback, must be present)", &source);
    fingerprint("destination (after rollback, must be absent)", &destination);
    assert!(source.is_file(), "source must be restored after rollback");
    assert!(
        !destination.exists(),
        "destination must be gone after rollback"
    );
    if let Ok(outcome) = &rollback_result {
        println!("  directories removed: {:?}", outcome.directories_removed);
        println!(
            "  directories remaining: {:?}",
            outcome.directories_remaining
        );
    }
    println!(
        "  pre-existing 'library/TestPlatform' directory still present: {}",
        canary_root.join("library").join("TestPlatform").is_dir()
    );
    assert!(
        canary_root.join("library").join("TestPlatform").is_dir(),
        "pre-existing library/TestPlatform dir must survive (the transaction never owned it - \
         assess_canary_eligibility requires the destination parent to pre-exist)"
    );

    println!("\n=== SECTION 22: METADATA CHECK ===");
    let post_rollback_hash = sha256_hex(&source);
    println!(
        "  BYTE IDENTITY (sha256): before={pre_source_hash} after={post_rollback_hash} match={}",
        pre_source_hash == post_rollback_hash
    );
    assert_eq!(
        pre_source_hash, post_rollback_hash,
        "byte identity must hold after rollback"
    );
    fingerprint("source (final metadata)", &source);

    println!("\n=== SECTION 23: JOURNAL AFTER ROLLBACK ===");
    let journal_after_rollback = std::fs::read_to_string(&journal_path).unwrap();
    println!("{journal_after_rollback}");
    let (assessment_after_rollback, _issues) = reconcile_and_assess(&mut transaction, &journal_dir);
    println!("  recovery assessment after rollback: {assessment_after_rollback:?}");

    println!("\n=== SECTION 24: SECOND ROLLBACK ===");
    let second_rollback = rollback_plan_transaction(
        &mut transaction,
        &journal_dir,
        &cancel,
        &TrustedRoots::from_paths([canary_root.as_path()]),
    );
    println!("  second rollback result: {second_rollback:?}");
    assert!(source.exists(), "source presence sanity");
    assert!(
        source.is_file(),
        "source must remain present after second rollback"
    );

    println!("\n=== SECTION 25: APPLY-AFTER-ROLLBACK REFUSAL ===");
    let apply_after_rollback = apply_plan_transaction_with_mode(
        &mut transaction,
        current_generation,
        &canary_root,
        TrustedRoots::from_paths([canary_root.as_path()]),
        &journal_dir,
        &cancel,
        false,
        mode,
    );
    println!("  apply-after-rollback result: {apply_after_rollback:?}");
    assert!(
        matches!(apply_after_rollback, Err(ApplyError::AlreadySettled { .. })),
        "apply on the old, rolled-back transaction id must be refused as AlreadySettled"
    );

    println!("\n=== SECTION 26: FRESH RE-APPROVAL ===");
    let fresh_export2 = synthetic_export(&source, &destination, &pre_source_hash);
    let fresh_digest =
        archivefs_core::platform_evidence_fusion::plan_transaction::compute_plan_digest(
            &fresh_export2,
        );
    let fresh_preview = build_preview(&fresh_export2);
    let fresh_approved = approve_transaction(&fresh_preview, "batch-17 fresh reapproval").unwrap();
    let fresh_transaction =
        build_plan_transaction(&fresh_export2, &fresh_approved, "real_canary_batch17_fresh")
            .unwrap();
    println!("  old transaction id: {transaction_id}");
    println!("  new transaction id: {}", fresh_transaction.transaction_id);
    println!("  plan digest (old run):   {plan_digest}");
    println!("  plan digest (fresh run): {}", fresh_digest.as_str());
    assert_ne!(
        fresh_transaction.transaction_id, transaction_id,
        "a fresh build must produce a new transaction id"
    );
    assert_eq!(
        fresh_digest.as_str(),
        plan_digest,
        "identical plan content must still produce the identical digest"
    );
    println!(
        "  old transaction remains terminal: {:?}",
        transaction.state
    );
    assert_eq!(
        transaction.state,
        archivefs_core::dat::rename_apply::model::TransactionState::RolledBack
    );
    println!(
        "  (fresh transaction built but NOT applied - blast radius stays at zero for this proof)"
    );

    println!("\n=== SECTION 29: UNRELATED SENTINEL ===");
    let sentinel_hash_after = sha256_hex(&sentinel);
    println!("  sentinel before: {sentinel_hash_before}");
    println!("  sentinel after:  {sentinel_hash_after}");
    assert_eq!(
        sentinel_hash_before, sentinel_hash_after,
        "sentinel must be byte-identical"
    );

    println!("\n=== SECTION 38: CLEANUP ===");
    cleanup(&canary_root);
    println!("  removed {}", canary_root.display());

    println!("\n=== RESULT: FULL REAL CANARY CYCLE COMPLETED SUCCESSFULLY ===");
}

fn reconcile_and_assess(
    transaction: &mut archivefs_core::dat::rename_apply::model::RenameTransaction,
    journal_dir: &Path,
) -> (
    archivefs_core::platform_evidence_fusion::plan_transaction::RecoveryAssessment,
    Vec<archivefs_core::dat::rename_apply::reconcile::RecoveryIssue>,
) {
    let issues =
        archivefs_core::dat::rename_apply::reconcile::reconcile_recovery(transaction, journal_dir)
            .unwrap_or_default();
    let assessment = assess_recovery(transaction, &issues);
    (assessment, issues)
}

fn report_recovery(
    transaction: &mut archivefs_core::dat::rename_apply::model::RenameTransaction,
    journal_dir: &Path,
) {
    let issues =
        archivefs_core::dat::rename_apply::reconcile::reconcile_recovery(transaction, journal_dir)
            .unwrap_or_default();
    let assessment = assess_recovery(transaction, &issues);
    eprintln!(
        "{}",
        render_recovery_report(transaction, &issues, assessment)
    );
    if assessment == archivefs_core::platform_evidence_fusion::plan_transaction::RecoveryAssessment::ManualRecoveryRequired {
        eprintln!("MANUAL RECOVERY REQUIRED - halting. No automatic filesystem repair will be attempted.");
    }
}

fn cleanup_after_manual_review(canary_root: &Path) {
    eprintln!(
        "Leaving {} in place for manual review (not auto-cleaned after a failure).",
        canary_root.display()
    );
}

fn cleanup(canary_root: &Path) {
    let _ = std::fs::remove_dir_all(canary_root);
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            out.push(path.clone());
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}
