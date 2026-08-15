//! `emuwiz-cli repair <command>`: whole-library repair planning.
//!
//! The first manually-testable Repair Planner CLI:
//!
//! ```text
//! emuwiz-cli repair scan  --root <dir> --dat <file> [--plan-out <json>] [--json]
//! emuwiz-cli repair plan  --plan <json> [--json]
//! emuwiz-cli repair apply --plan <json> [--generation <n>] [--journal-dir <dir>]
//! ```
//!
//! `scan` is read-only by default: it audits, plans, and previews, and only
//! mutates when given an explicit `apply`. The CLI never calls `fs::rename`
//! directly — every mutation goes through the Repair Center executor.

use std::path::{Path, PathBuf};

use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir;
use archivefs_core::dat::sources::{DatSourceKind, suggest_display_name};
use archivefs_core::repair::execute::RepairExecutionOptions;
use archivefs_core::repair::library::{
    LibraryRepairPlan, RepairProfile, apply_library_repair_plan, plan_file_from_scan,
    preview_library_repair_plan, run_library_scan,
};
use archivefs_core::safe_read::TrustedRoots;

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(command) = args.first().cloned() else {
        return Err("repair requires a sub-command: scan | plan | apply".into());
    };
    let rest = args[1..].to_vec();
    match command.as_str() {
        "scan" => run_scan(rest),
        "plan" => run_plan(rest),
        "apply" => run_apply(rest),
        _ => Err(
            format!("unknown repair sub-command '{command}' (expected scan, plan, or apply)")
                .into(),
        ),
    }
}

fn run_scan(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let root = take_path_value(&mut args, "--root")?
        .ok_or("repair scan requires --root <library directory>")?;
    let dat = take_path_value(&mut args, "--dat")?
        .ok_or("repair scan requires --dat <catalogue file>")?;
    let source_id = take_string_value(&mut args, "--source-id")?;
    let profile_raw = take_string_value(&mut args, "--profile")?;
    let plan_out = take_path_value(&mut args, "--plan-out")?;
    if !args.is_empty() {
        return Err(format!("repair scan does not accept {args:?}").into());
    }

    let profile = match profile_raw.as_deref() {
        None => RepairProfile::CanonicalInPlace,
        Some(raw) => RepairProfile::parse(raw).ok_or_else(|| {
            format!("unknown --profile '{raw}' (expected canonical-in-place | romm)")
        })?,
    };
    if !profile.is_implemented() {
        return Err(format!(
            "profile '{}' is not implemented yet; only 'canonical-in-place' produces executable repairs",
            profile.label()
        )
        .into());
    }

    let dat_kind = if std::fs::metadata(&dat).is_ok_and(|m| m.is_dir()) {
        DatSourceKind::Folder
    } else {
        DatSourceKind::File
    };
    let source_id = source_id.unwrap_or_else(|| slug(&dat));
    let source_display_name = suggest_display_name(&dat);

    let request = archivefs_core::repair::library::LibraryScanRequest {
        source_id,
        source_display_name,
        dat_path: dat.clone(),
        dat_kind,
        scan_root: root.clone(),
        limits: DatLimits::default(),
        profile,
    };

    eprintln!(
        "Repair scan: auditing {} against {}",
        root.display(),
        dat.display()
    );
    let trusted = TrustedRoots::from_paths([&root]);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let outcome = run_library_scan(&request, &trusted, &cancel, &|_| {})?;

    let plan = plan_file_from_scan(&outcome);

    if let Some(plan_out) = &plan_out {
        std::fs::write(plan_out, serde_json::to_string_pretty(&plan)?)?;
        eprintln!("Plan written to {}", plan_out.display());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print!("{}", format_report(&plan));
    }
    Ok(())
}

fn run_plan(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let plan_path =
        take_path_value(&mut args, "--plan")?.ok_or("repair plan requires --plan <plan file>")?;
    if !args.is_empty() {
        return Err(format!("repair plan does not accept {args:?}").into());
    }

    let plan = read_plan(&plan_path)?;
    let preflight = preview_library_repair_plan(&plan, plan.generation);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plan": plan,
                "preflight": preflight,
            }))?
        );
    } else {
        print!("{}", format_report(&plan));
        println!();
        println!("Dry-run preflight (read-only):");
        for result in &preflight.results {
            println!(
                "  [{}] {}: {}",
                result.status.label(),
                result.proposal_id,
                result.detail
            );
        }
    }
    Ok(())
}

fn run_apply(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let plan_path =
        take_path_value(&mut args, "--plan")?.ok_or("repair apply requires --plan <plan file>")?;
    let generation = take_u64_value(&mut args, "--generation")?;
    let journal_dir = take_path_value(&mut args, "--journal-dir")?;
    if !args.is_empty() {
        return Err(format!("repair apply does not accept {args:?}").into());
    }

    let plan = read_plan(&plan_path)?;
    let current_generation = generation.unwrap_or(plan.generation);
    if current_generation != plan.generation {
        return Err(format!(
            "the plan is stale (plan generation {}, current generation {}); re-run `repair scan`",
            plan.generation, current_generation
        )
        .into());
    }

    let journal_dir = journal_dir.unwrap_or_else(|| {
        default_rename_transaction_dir().unwrap_or_else(|_| PathBuf::from("rename-transactions"))
    });
    let trusted = TrustedRoots::from_paths([Path::new(&plan.scan_root)]);
    let options = RepairExecutionOptions {
        trusted,
        journal_dir,
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);

    let result = apply_library_repair_plan(&plan, current_generation, &options, &cancel)?;

    let rolled_back = matches!(
        result.summary.rollback,
        archivefs_core::dat::rename_apply::model::RollbackStatus::FullyRolledBack
            | archivefs_core::dat::rename_apply::model::RollbackStatus::PartiallyRolledBack
    );
    let still_needs_review = plan.report.counts.needs_review + plan.report.counts.needs_review_sets;

    if json {
        let reverify: Vec<serde_json::Value> = result
            .reverify
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "source_path": entry.source_path,
                    "destination_path": entry.destination_path,
                    "outcome": entry.outcome.label(),
                    "detail": entry.detail,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "transaction_id": result.summary.transaction_id,
                "requested": result.summary.requested,
                "applied": result.summary.applied,
                "failed": result.summary.failed,
                "skipped": result.summary.skipped,
                "rolled_back": rolled_back,
                "still_needs_review": still_needs_review,
                "reverify": reverify,
            }))?
        );
    } else {
        println!(
            "Repair apply complete (transaction {}):",
            result.summary.transaction_id
        );
        println!("  Applied: {}", result.summary.applied);
        println!("  Failed: {}", result.summary.failed);
        println!("  Rolled back: {}", if rolled_back { "yes" } else { "no" });
        println!("  Still NeedsReview: {still_needs_review}");
        println!("  Reverify:");
        for entry in &result.reverify {
            println!(
                "    [{}] {} -> {}: {}",
                entry.outcome.label(),
                entry.source_path.display(),
                entry.destination_path.display(),
                entry.detail
            );
        }
    }
    Ok(())
}

fn read_plan(path: &Path) -> Result<LibraryRepairPlan, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// A short, safe source id derived from the DAT path's stem.
fn slug(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dat".to_string());
    let mut out = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "dat".to_string()
    } else {
        trimmed
    }
}

fn format_report(plan: &LibraryRepairPlan) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let counts = &plan.report.counts;
    let _ = writeln!(out, "Whole-Library Repair Scan");
    let _ = writeln!(out, "  Profile: {}", plan.profile);
    let _ = writeln!(
        out,
        "  Source: {} ({})",
        plan.source_display_name, plan.source_id
    );
    let _ = writeln!(out, "  Scan root: {}", plan.scan_root);
    let _ = writeln!(out, "  Generation: {}", plan.generation);
    let _ = writeln!(out, "  Files scanned: {}", plan.files_scanned);
    let _ = writeln!(
        out,
        "  Truncated: {}",
        if plan.truncated { "yes" } else { "no" }
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Counts:");
    let _ = writeln!(out, "  Complete sets: {}", counts.complete_sets);
    let _ = writeln!(out, "  Incomplete sets: {}", counts.incomplete_sets);
    let _ = writeln!(out, "  Bad metadata sets: {}", counts.bad_metadata_sets);
    let _ = writeln!(out, "  NeedsReview sets: {}", counts.needs_review_sets);
    let _ = writeln!(out, "  Safe repairs: {}", counts.safe_repairs);
    let _ = writeln!(out, "  Already canonical: {}", counts.already_canonical);
    let _ = writeln!(out, "  NeedsReview: {}", counts.needs_review);
    let _ = writeln!(out, "  Blocked: {}", counts.blocked_repair);
    let _ = writeln!(out, "  Unsupported: {}", counts.unsupported);
    let _ = writeln!(out, "  Scan errors: {}", counts.scan_errors);
    let _ = writeln!(out);

    let _ = writeln!(out, "SAFE");
    for proposal in &plan.repair_plan.proposals {
        let _ = writeln!(
            out,
            "{} -> {}",
            proposal.source_path.display(),
            proposal
                .destination()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        );
        let _ = writeln!(out, "  Reason: {}", proposal.reason);
    }

    for (heading, items) in [
        ("NEEDS REVIEW", &plan.report.needs_review),
        ("BLOCKED", &plan.report.blocked),
        ("UNSUPPORTED", &plan.report.unsupported),
    ] {
        let _ = writeln!(out, "{heading}");
        for item in items {
            let _ = writeln!(out, "{}", item.path);
            let _ = writeln!(out, "  Reason: {}", item.reason);
        }
    }

    let _ = writeln!(out, "COMPLETE SETS");
    for item in &plan.report.complete_sets {
        let _ = writeln!(out, "{}", item.game_name);
    }
    let _ = writeln!(out, "INCOMPLETE SETS");
    for item in &plan.report.incomplete_sets {
        let _ = writeln!(out, "{}: {}", item.game_name, item.reason);
    }
    let _ = writeln!(out, "BAD METADATA SETS");
    for item in &plan.report.bad_metadata_sets {
        let _ = writeln!(out, "{}: {}", item.game_name, item.reason);
    }
    let _ = writeln!(out, "NEEDS REVIEW SETS");
    for item in &plan.report.needs_review_sets {
        let _ = writeln!(out, "{}: {}", item.game_name, item.reason);
    }
    let _ = writeln!(out, "SCAN ERRORS");
    if plan.report.scan_errors.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for error in &plan.report.scan_errors {
            let _ = writeln!(out, "{error}");
        }
    }
    out
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let had = args.iter().any(|a| a == flag);
    args.retain(|a| a != flag);
    had
}

fn take_string_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(i, a)| (a == flag).then_some(i))
        .collect();
    if positions.len() > 1 {
        return Err(format!("{flag} may be specified only once").into());
    }
    let Some(pos) = positions.first().copied() else {
        return Ok(None);
    };
    if pos + 1 >= args.len() {
        return Err(format!("{flag} requires a value").into());
    }
    let value = args.remove(pos + 1);
    args.remove(pos);
    Ok(Some(value))
}

fn take_path_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    Ok(take_string_value(args, flag)?.map(PathBuf::from))
}

fn take_u64_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    match take_string_value(args, flag)? {
        None => Ok(None),
        Some(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("{flag} value '{raw}' is not a valid generation number").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1_TEST: &str = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("single.dat");
        std::fs::write(
            &dat,
            format!(
                r#"<?xml version="1.0"?>
<datafile>
    <header><name>Single</name></header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
            ),
        )
        .unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir(&roms).unwrap();
        std::fs::write(roms.join("wrongname.bin"), b"test").unwrap();
        (dir, dat, roms)
    }

    #[test]
    fn scan_is_read_only_and_apply_executes_through_repair_center() {
        let (dir, dat, roms) = fixture();
        let plan_path = dir.path().join("plan.json");

        // scan: read-only, writes only the plan file.
        run(vec![
            "scan".into(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--plan-out".into(),
            plan_path.display().to_string(),
        ])
        .unwrap();

        assert!(roms.join("wrongname.bin").exists(), "scan never renames");
        assert!(
            !roms.join("super.bin").exists(),
            "scan never writes the canonical name"
        );
        assert!(plan_path.exists(), "the plan file is written");

        // apply: explicit, through the Repair Center executor.
        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
        ])
        .unwrap();

        assert!(
            roms.join("super.bin").exists(),
            "apply renames to the canonical name"
        );
        assert!(!roms.join("wrongname.bin").exists(), "the old name is gone");
    }

    #[test]
    fn scan_rejects_an_unimplemented_profile() {
        let (_dir, dat, roms) = fixture();
        let error = run(vec![
            "scan".into(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--profile".into(),
            "romm".into(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("not implemented"), "{error}");
    }
}
