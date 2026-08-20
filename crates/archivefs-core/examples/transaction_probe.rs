//! Batch 14 developer probe: preview and (only opt-in, tempdir-only) apply
//! a synthetic frozen [`LibraryPlanExport`] through the real transaction
//! layer.
//!
//! Default behavior is **preview only** - it builds a small synthetic
//! export shaped like a real planner result (never touching
//! `/mnt/games/roms` or any real collection), prints
//! [`render_preview_text`], and stops.
//!
//! ```text
//! cargo run -p archivefs-core --example transaction_probe
//! cargo run -p archivefs-core --example transaction_probe -- --temp-fixture-apply
//! ```
//!
//! `--temp-fixture-apply` is the *only* way to mutate anything, and even
//! then only inside a tempdir this process creates itself
//! (`std::env::temp_dir()` + a random subdirectory) - there is no
//! `--destination` flag at all, so an arbitrary real path can never be
//! supplied (milestone section 49's hard safety guard).

use std::sync::atomic::AtomicBool;

use archivefs_core::platform_evidence_fusion::library_plan_export::{
    LibraryPlanExport, LibraryPlanExportItem, OperationIntent, SourcePrecondition,
};
use archivefs_core::platform_evidence_fusion::library_planning::{
    PlanStatus, RenameBasis, RommMappingStatus,
};
use archivefs_core::platform_evidence_fusion::plan_transaction::{
    apply_plan_transaction, approve_transaction, build_plan_transaction, build_preview,
    plan_generation_of, render_preview_text, rollback_plan_transaction,
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
    let apply_requested = std::env::args().any(|arg| arg == "--temp-fixture-apply");

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

    if !apply_requested {
        println!("(preview only - pass --temp-fixture-apply to run the mutation fixture)");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
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

    let generation = plan_generation_of(&export);
    let journal_dir = fixture_dir.join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let cancel = AtomicBool::new(false);
    let trusted = TrustedRoots::from_paths([fixture_dir.as_path()]);

    match apply_plan_transaction(
        &mut transaction,
        generation,
        &fixture_dir,
        trusted,
        &journal_dir,
        &cancel,
        false,
    ) {
        Ok(outcome) => {
            println!("Applied: {:?}", outcome.transaction.state);
            println!("Destination exists: {}", destination.exists());
        }
        Err(error) => {
            println!("Apply refused: {error}");
        }
    }

    // Roll back immediately so this probe never leaves a mutated fixture
    // behind - it exists to demonstrate the mechanics, not to persist a
    // result.
    if let Ok(rollback) = rollback_plan_transaction(&mut transaction, &journal_dir, &cancel) {
        println!("Rollback: {:?}", rollback.rollback.result);
    }

    let _ = std::fs::remove_dir_all(&fixture_dir);
}
