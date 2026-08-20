//! Batch 14 developer probe: preview and (only opt-in, tempdir-only) apply
//! a synthetic frozen [`LibraryPlanExport`] through the real transaction
//! layer.
//!
//! Default behavior is **preview only** - it builds a small synthetic
//! export shaped like a real planner result (never touching the real,
//! production ROM collection), prints [`render_preview_text`], and stops.
//!
//! ```text
//! cargo run -p archivefs-core --example transaction_probe
//! cargo run -p archivefs-core --example transaction_probe -- --temp-fixture-apply
//! cargo run -p archivefs-core --example transaction_probe -- --temp-fixture-fail-after-1
//! cargo run -p archivefs-core --example transaction_probe -- --temp-fixture-rollback
//! cargo run -p archivefs-core --example transaction_probe -- --canary-check
//! ```
//!
//! `--temp-fixture-apply`/`--temp-fixture-fail-after-1`/
//! `--temp-fixture-rollback` are the *only* ways to mutate anything, and
//! even then only inside a tempdir this process creates itself
//! (`std::env::temp_dir()` + a random subdirectory) - there is no
//! `--destination` flag at all, so an arbitrary real path can never be
//! supplied (milestone section 49's hard safety guard). `--canary-check`
//! (Batch 16) is read-only: it runs `assess_canary_eligibility`/
//! `render_canary_preview` against the same synthetic tempdir fixture and
//! never touches the filesystem beyond that. There is, and must never be,
//! any flag in this file that performs a genuine, non-tempdir apply -
//! see `transaction_probe_source_never_contains_a_real_apply_flag` in
//! `plan_transaction/closeout_tests.rs` for the structural proof.

use std::sync::atomic::AtomicBool;

use archivefs_core::dat::rename_apply::HardConflictMode;
use archivefs_core::platform_evidence_fusion::library_plan_export::{
    LibraryPlanExport, LibraryPlanExportItem, OperationIntent, SourcePrecondition,
};
use archivefs_core::platform_evidence_fusion::library_planning::{
    PlanStatus, RenameBasis, RommMappingStatus,
};
use archivefs_core::platform_evidence_fusion::plan_transaction::{
    apply_plan_transaction_with_mode, approve_transaction, assess_canary_eligibility,
    build_plan_transaction, build_preview, plan_generation_of, preview_is_confined_to_root,
    render_canary_preview, render_preview_text, rollback_plan_transaction,
};
use archivefs_core::safe_read::TrustedRoots;

fn synthetic_export(source: &std::path::Path, destination: &std::path::Path) -> LibraryPlanExport {
    LibraryPlanExport {
        items: vec![LibraryPlanExportItem {
            status: PlanStatus::Ready,
            precondition: SourcePrecondition {
                source_path: source.display().to_string(),
                physical_hash: None,
                normalized_hash: None,
            },
            proposed_destination: Some(destination.display().to_string()),
            operation_intent: OperationIntent::MoveToLibraryFolder,
            platform_library: Some("N64".to_string()),
            display_name: "Synthetic Sample Game (USA)".to_string(),
            romm_status: RommMappingStatus::Mapped,
            romm_slug: Some("n64".to_string()),
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
    let args: Vec<String> = std::env::args().collect();
    let apply_requested = args.iter().any(|arg| arg == "--temp-fixture-apply");
    let fail_after_1_requested = args.iter().any(|arg| arg == "--temp-fixture-fail-after-1");
    let rollback_requested = args.iter().any(|arg| arg == "--temp-fixture-rollback");
    let canary_check_requested = args.iter().any(|arg| arg == "--canary-check");
    let mutation_requested = apply_requested || fail_after_1_requested || rollback_requested;

    // Note: `--canary-check` is intentionally NOT a mutation flag and never
    // sets `mutation_requested` - it only ever calls the read-only
    // `assess_canary_eligibility`/`render_canary_preview` against the
    // probe's own synthetic tempdir fixture (milestone section 25). There
    // is, and must never be, any flag in this file that performs a
    // genuine, non-tempdir apply - see
    // `transaction_probe_source_never_contains_a_real_apply_flag` in
    // `plan_transaction/closeout_tests.rs` for the structural proof.

    // A tempdir this process creates itself - never a caller-supplied
    // path. This is the hard safety guard: there is no flag anywhere in
    // this file that accepts an arbitrary destination.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let fixture_dir = std::env::temp_dir().join(format!("archivefs-transaction-probe-{now}"));
    std::fs::create_dir_all(&fixture_dir).expect("create the probe's own tempdir");
    let source = fixture_dir.join("Sample Game (USA).z64");
    std::fs::write(&source, vec![0u8; 128]).expect("write the synthetic sample file");
    let destination = fixture_dir
        .join("library")
        .join("n64")
        .join("Sample Game (USA).z64");

    let export = synthetic_export(&source, &destination);
    let preview = build_preview(&export);
    println!("{}", render_preview_text(&preview));

    if canary_check_requested {
        let approved = match approve_transaction(&preview, "developer probe canary-check") {
            Ok(approved) => approved,
            Err(error) => {
                eprintln!("could not approve: {error:?}");
                let _ = std::fs::remove_dir_all(&fixture_dir);
                std::process::exit(1);
            }
        };
        let eligibility =
            assess_canary_eligibility(&export, &export.items[0], &approved, &fixture_dir);
        println!("{}", render_canary_preview(&export.items[0], &eligibility));
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    if !mutation_requested {
        println!(
            "(preview only - pass --temp-fixture-apply, --temp-fixture-fail-after-1, or \
             --temp-fixture-rollback to run a mutation fixture)"
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    // Hard temp-safety guard (milestone section 39): every operation must
    // resolve underneath the tempdir this process just created itself.
    // There is no flag anywhere in this file that can point a destination
    // outside `fixture_dir`, so this can only ever be an internal-logic
    // failure, never an attacker-supplied path - but it is checked
    // unconditionally anyway, before the executor is ever invoked.
    if !preview_is_confined_to_root(&preview, &fixture_dir) {
        eprintln!(
            "refusing to proceed: an operation is not confined to the probe's own fixture root"
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        std::process::exit(1);
    }

    let approved = match approve_transaction(&preview, "developer probe fixture apply") {
        Ok(approved) => approved,
        Err(error) => {
            eprintln!("could not approve: {error:?}");
            let _ = std::fs::remove_dir_all(&fixture_dir);
            std::process::exit(1);
        }
    };

    let mut transaction = match build_plan_transaction(&export, &approved, "transaction_probe") {
        Ok(transaction) => transaction,
        Err(error) => {
            eprintln!("could not build transaction: {error}");
            let _ = std::fs::remove_dir_all(&fixture_dir);
            std::process::exit(1);
        }
    };

    if fail_after_1_requested {
        // Sabotage: pre-create the destination so the whole-batch preflight
        // finds a hard conflict and the AbortAll executor refuses before any
        // mutation - demonstrating fail-closed behavior on demand, still
        // entirely inside `fixture_dir`.
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"pre-existing, never overwritten").unwrap();
    }

    let generation = plan_generation_of(&export);
    let journal_dir = fixture_dir.join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fixture_dir.as_path()]);

    match apply_plan_transaction_with_mode(
        &mut transaction,
        generation,
        &fixture_dir,
        trusted,
        &journal_dir,
        &cancel,
        false,
        HardConflictMode::AbortAll,
    ) {
        Ok(outcome) => {
            println!("Applied: {:?}", outcome.transaction.state);
            println!("Destination exists: {}", destination.exists());
        }
        Err(error) => {
            println!("Apply refused: {error}");
        }
    }

    if rollback_requested || apply_requested {
        // Roll back immediately so this probe never leaves a mutated
        // fixture behind - it exists to demonstrate the mechanics, not to
        // persist a result.
        let trusted_for_rollback = TrustedRoots::from_paths([fixture_dir.as_path()]);
        if let Ok(rollback) = rollback_plan_transaction(
            &mut transaction,
            &journal_dir,
            &cancel,
            &trusted_for_rollback,
        ) {
            println!("Rollback: {:?}", rollback.rollback.result);
        }
    }

    let _ = std::fs::remove_dir_all(&fixture_dir);
}
