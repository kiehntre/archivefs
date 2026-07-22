use std::path::PathBuf;

use archivefs_core::patch_manager::{
    CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION, CachePruneExecutionResult, CachePrunePlan,
    CachePrunePolicy, CheatSourceError, SnapshotInventoryEntry, SnapshotInventoryReport,
    SnapshotPinResult, SnapshotVerificationReport, default_cheat_source_cache_root,
    execute_retroarch_cheat_cache_prune, inventory_retroarch_cheat_snapshots,
    plan_retroarch_cheat_cache_prune, set_retroarch_cheat_snapshot_pin,
    verify_retroarch_cheat_snapshots,
};
use serde::Serialize;
use serde_json::{Value, json};

type CommandResult = Result<(), Box<dyn std::error::Error>>;
type CommandRunner = fn(Vec<String>) -> CommandResult;

#[derive(Serialize)]
struct JsonFailure {
    schema_version: u32,
    status: &'static str,
    error: Value,
}

#[derive(Serialize)]
struct PruneOutput<'a> {
    schema_version: u32,
    status: &'static str,
    plan: &'a CachePrunePlan,
    execution: &'a CachePruneExecutionResult,
}

pub fn run_list(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    run_with_json_failure(args, run_list_inner)
}

fn run_list_inner(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json")?;
    let source = take_value(&mut args, "--source")?;
    let cache_root = cache_root(&mut args)?;
    reject_extra(&args)?;
    match inventory_retroarch_cheat_snapshots(&cache_root) {
        Ok(mut report) => {
            if let Some(source) = source {
                report
                    .entries
                    .retain(|entry| entry.source_id.as_deref() == Some(&source));
            }
            render_inventory(&report, json)?;
            Ok(())
        }
        Err(error) => fail(error, json),
    }
}

pub fn run_verify(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    run_with_json_failure(args, run_verify_inner)
}

fn run_verify_inner(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json")?;
    let all = take_flag(&mut args, "--all")?;
    let source = take_value(&mut args, "--source")?;
    let cache_root = cache_root(&mut args)?;
    let snapshot = if args.len() == 1 && !args[0].starts_with('-') {
        Some(args.remove(0))
    } else if args.is_empty() {
        None
    } else {
        return Err(
            "snapshot verification accepts one snapshot ID, --source <source-id>, or --all".into(),
        );
    };
    let selectors =
        usize::from(all) + usize::from(source.is_some()) + usize::from(snapshot.is_some());
    if selectors != 1 {
        return Err("snapshot verification requires exactly one snapshot ID, --source <source-id>, or --all".into());
    }
    match verify_retroarch_cheat_snapshots(&cache_root, snapshot.as_deref(), source.as_deref()) {
        Ok(report) => {
            render_verification(&report, json)?;
            if report.invalid_count > 0 {
                Err("one or more snapshots failed verification".into())
            } else {
                Ok(())
            }
        }
        Err(error) => fail(error, json),
    }
}

pub fn run_pin(args: Vec<String>, pinned: bool) -> Result<(), Box<dyn std::error::Error>> {
    let json = args.iter().any(|argument| argument == "--json");
    let result = run_pin_inner(args, pinned);
    render_failure_if_json(result, json)
}

fn run_pin_inner(args: Vec<String>, pinned: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json")?;
    let cache_root = cache_root(&mut args)?;
    if args.len() != 1 || args[0].starts_with('-') {
        return Err("snapshot pin/unpin requires exactly one snapshot ID".into());
    }
    match set_retroarch_cheat_snapshot_pin(&cache_root, &args[0], pinned) {
        Ok(result) => {
            render_pin(&result, json)?;
            Ok(())
        }
        Err(error) => fail(error, json),
    }
}

pub fn run_prune(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    run_with_json_failure(args, run_prune_inner)
}

fn run_prune_inner(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json")?;
    let yes = take_flag(&mut args, "--yes")?;
    let dry_run = take_flag(&mut args, "--dry-run")?;
    let include_staging = take_flag(&mut args, "--include-abandoned-staging")?;
    if yes && dry_run {
        return Err("--yes and --dry-run cannot be combined".into());
    }
    let keep = parse_usize(take_value(&mut args, "--keep")?, "--keep")?;
    let older_days = parse_u64(
        take_value(&mut args, "--older-than-days")?,
        "--older-than-days",
    )?;
    let max_cache_bytes = parse_u64(
        take_value(&mut args, "--max-cache-bytes")?,
        "--max-cache-bytes",
    )?;
    let staging_age_hours = parse_u64(
        take_value(&mut args, "--abandoned-staging-min-hours")?,
        "--abandoned-staging-min-hours",
    )?;
    if staging_age_hours.is_some() && !include_staging {
        return Err("--abandoned-staging-min-hours requires --include-abandoned-staging".into());
    }
    let source = take_value(&mut args, "--source")?;
    let cache_root = cache_root(&mut args)?;
    reject_extra(&args)?;
    let policy = CachePrunePolicy {
        keep_newest_per_source: keep,
        retain_newer_than_seconds: older_days
            .map(|days| days.checked_mul(24 * 60 * 60))
            .transpose_or_error("--older-than-days is too large")?,
        max_cache_bytes,
        source_filter: source,
        include_abandoned_staging: include_staging,
        abandoned_staging_min_age_seconds: staging_age_hours
            .unwrap_or(24)
            .checked_mul(60 * 60)
            .ok_or("--abandoned-staging-min-hours is too large")?,
    };
    let plan = match plan_retroarch_cheat_cache_prune(&cache_root, &policy) {
        Ok(value) => value,
        Err(error) => return fail(error, json),
    };
    let execution = match execute_retroarch_cheat_cache_prune(&cache_root, &plan, yes && !dry_run) {
        Ok(value) => value,
        Err(error) => return fail(error, json),
    };
    render_prune(&plan, &execution, json)?;
    if yes
        && matches!(
            execution.status,
            archivefs_core::patch_manager::CachePruneExecutionStatus::PartialFailure
        )
    {
        Err("one or more prune candidates were not deleted safely".into())
    } else {
        Ok(())
    }
}

trait CheckedOptionExt {
    fn transpose_or_error(
        self,
        message: &'static str,
    ) -> Result<Option<u64>, Box<dyn std::error::Error>>;
}

impl CheckedOptionExt for Option<Option<u64>> {
    fn transpose_or_error(
        self,
        message: &'static str,
    ) -> Result<Option<u64>, Box<dyn std::error::Error>> {
        match self {
            Some(Some(value)) => Ok(Some(value)),
            Some(None) => Err(message.into()),
            None => Ok(None),
        }
    }
}

fn render_inventory(report: &SnapshotInventoryReport, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("RetroArch cheat snapshot inventory");
    println!("  Cache root: {}", report.cache_root.display);
    if report.entries.is_empty() {
        println!("  No cached snapshots found.");
    }
    for entry in &report.entries {
        render_entry(entry);
    }
    for warning in &report.warnings {
        println!("  Warning: {warning}");
    }
    Ok(())
}

fn render_entry(entry: &SnapshotInventoryEntry) {
    println!(
        "  {} / {}",
        entry.source_id.as_deref().unwrap_or("<unsafe-source>"),
        entry.snapshot_id.as_deref().unwrap_or("<unsafe-snapshot>")
    );
    println!("    Path: {}", entry.cache_path.display);
    println!(
        "    Verification: {:?}  Current: {}  Pinned: {}  Freshness: {:?}",
        entry.verification_state, entry.current, entry.pinned, entry.freshness
    );
    if let Some(timestamp) = entry.retrieved_at_unix_seconds {
        println!("    Retrieved: {timestamp}");
    }
    if let Some(hash) = &entry.archive_sha256 {
        println!("    Archive SHA-256: {hash}");
    }
    for finding in &entry.verification_findings {
        println!("    Integrity: {:?}: {}", finding.state, finding.message);
    }
}

fn render_verification(
    report: &SnapshotVerificationReport,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("RetroArch cheat snapshot verification");
        println!(
            "  Valid: {}  Invalid: {}",
            report.valid_count, report.invalid_count
        );
        for entry in &report.entries {
            render_entry(entry);
        }
    }
    Ok(())
}

fn render_pin(result: &SnapshotPinResult, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        println!("Snapshot {}: {:?}", result.snapshot_id, result.status);
        println!("  Source: {}", result.source_id);
        println!("  Snapshot contents were not modified.");
    }
    Ok(())
}

fn render_prune(
    plan: &CachePrunePlan,
    execution: &CachePruneExecutionResult,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&PruneOutput {
                schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
                status: if execution.confirmed {
                    "applied"
                } else {
                    "preview"
                },
                plan,
                execution,
            })?
        );
        return Ok(());
    }
    println!("RetroArch cheat cache prune plan");
    println!(
        "  Candidates: {} ({} bytes)",
        plan.candidate_count, plan.candidate_bytes
    );
    println!("  Protected: {}", plan.protected_count);
    for entry in &plan.entries {
        println!(
            "  {:?}: {} — {:?}",
            entry.disposition, entry.path.display, entry.reasons
        );
    }
    if execution.confirmed {
        println!("Applied: {:?}", execution.status);
        println!("  Bytes reclaimed: {}", execution.bytes_reclaimed);
        for entry in &execution.entries {
            println!(
                "  {:?}: {} — {}",
                entry.status, entry.path.display, entry.detail
            );
        }
    } else {
        println!("Preview only. Re-run with --yes to apply this policy after re-planning.");
    }
    Ok(())
}

fn fail(error: CheatSourceError, _json: bool) -> Result<(), Box<dyn std::error::Error>> {
    Err(Box::new(error))
}

fn run_with_json_failure(args: Vec<String>, command: CommandRunner) -> CommandResult {
    let json = args.iter().any(|argument| argument == "--json");
    render_failure_if_json(command(args), json)
}

fn render_failure_if_json(
    result: Result<(), Box<dyn std::error::Error>>,
    json_requested: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = &result
        && json_requested
    {
        println!("{}", format_json_failure(error.as_ref())?);
    }
    result
}

fn format_json_failure(
    error: &(dyn std::error::Error + 'static),
) -> Result<String, serde_json::Error> {
    let detail = if let Some(error) = error.downcast_ref::<CheatSourceError>() {
        serde_json::to_value(error)?
    } else {
        json!({
            "schema_version": CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
            "stage": "command",
            "code": "invalid_arguments",
            "message": error.to_string(),
        })
    };
    serde_json::to_string_pretty(&JsonFailure {
        schema_version: CHEAT_CACHE_MAINTENANCE_SCHEMA_VERSION,
        status: "failed",
        error: detail,
    })
}

fn cache_root(args: &mut Vec<String>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(take_value(args, "--cache-root")?
        .map(PathBuf::from)
        .unwrap_or(default_cheat_source_cache_root()?))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let positions = args.iter().filter(|value| value.as_str() == flag).count();
    if positions > 1 {
        return Err(format!("{flag} may be specified only once").into());
    }
    if let Some(index) = args.iter().position(|value| value == flag) {
        args.remove(index);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn take_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let positions = args.iter().filter(|value| value.as_str() == flag).count();
    if positions > 1 {
        return Err(format!("{flag} may be specified only once").into());
    }
    if let Some(index) = args.iter().position(|value| value == flag) {
        args.remove(index);
        if index >= args.len() || args[index].starts_with('-') {
            return Err(format!("{flag} requires a value").into());
        }
        Ok(Some(args.remove(index)))
    } else {
        Ok(None)
    }
}

fn parse_u64(value: Option<String>, flag: &str) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    value
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{flag} requires a non-negative integer").into())
        })
        .transpose()
}

fn parse_usize(
    value: Option<String>,
    flag: &str,
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    value
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("{flag} requires a non-negative integer").into())
        })
        .transpose()
}

fn reject_extra(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected argument: {}", args.join(" ")).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_option_parsing_is_strict() {
        let mut args = vec!["--cache-root".into(), "/tmp/cache".into()];
        assert_eq!(cache_root(&mut args).unwrap(), PathBuf::from("/tmp/cache"));
        assert!(args.is_empty());
        assert!(take_flag(&mut vec!["--json".into(), "--json".into()], "--json").is_err());
        assert!(parse_u64(Some("bad".into()), "--keep").is_err());
        let error: Box<dyn std::error::Error> = "--keep requires a non-negative integer".into();
        let failure: serde_json::Value =
            serde_json::from_str(&format_json_failure(error.as_ref()).unwrap()).unwrap();
        assert_eq!(failure["schema_version"], 1);
        assert_eq!(failure["status"], "failed");
        assert_eq!(failure["error"]["stage"], "command");
        assert_eq!(failure["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn empty_inventory_json_schema_is_stable() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-empty-maintenance-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let report = inventory_retroarch_cheat_snapshots(&root).unwrap();
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert!(value["entries"].as_array().unwrap().is_empty());
        assert!(!root.exists());
    }

    #[test]
    fn prune_output_has_plan_and_execution() {
        let root =
            std::env::temp_dir().join(format!("archivefs-empty-plan-{}", std::process::id()));
        let plan = plan_retroarch_cheat_cache_prune(&root, &Default::default()).unwrap();
        let execution = execute_retroarch_cheat_cache_prune(&root, &plan, false).unwrap();
        let value = serde_json::to_value(PruneOutput {
            schema_version: 1,
            status: "preview",
            plan: &plan,
            execution: &execution,
        })
        .unwrap();
        assert_eq!(value["status"], "preview");
        assert!(value.get("plan").is_some() && value.get("execution").is_some());
    }
}
