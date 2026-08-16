//! `emuwiz-cli repair <command>`: whole-library repair planning.
//!
//! The first manually-testable Repair Planner CLI:
//!
//! ```text
//! emuwiz-cli repair scan     --root <dir> --dat <file> [--plan-out <json>] [--json]
//! emuwiz-cli repair plan     --plan <json> [--json]
//! emuwiz-cli repair apply    --plan <json> --root <dir> --dat <file> --generation <n> \
//!     [--journal-dir <dir>] [--proposal-id <id> ...]
//! emuwiz-cli repair history  [--journal-dir <dir>] [--limit <n>] [--json]
//! emuwiz-cli repair rollback --transaction <id> --journal-dir <dir> [--json]
//! ```
//!
//! `scan` is read-only by default: it audits, plans, and previews, and only
//! mutates when given an explicit `apply`. The CLI never calls `fs::rename`
//! directly — every mutation goes through the Repair Center executor.
//!
//! `apply` applies the whole freshly re-proven plan by default. One or more
//! `--proposal-id <id>` flags switch to [`apply_saved_plan_selected`]: the
//! full saved plan is still re-proven against the fresh scan exactly as
//! before, and only then are the given ids resolved against the fresh plan
//! and executed.
//!
//! `history` and `rollback` are a thin CLI over the exact journal/transaction
//! machinery the GUI's Repair History page already uses
//! ([`archivefs_core::dat::rename_apply::list_journals`],
//! [`archivefs_core::dat::rename_apply::reconcile_recovery`],
//! [`archivefs_core::dat::rename_apply::rollback_transaction`],
//! [`archivefs_core::repair::execute::reverify_transaction`]): no second
//! transaction database, no hand-rolled inverse renames, and a duplicate
//! quarantine transaction rolls back through exactly the same call as an
//! ordinary rename transaction - it is not special-cased.

use std::path::{Path, PathBuf};

use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::rename_apply::journal::default_rename_transaction_dir;
use archivefs_core::dat::rename_apply::model::RollbackStatus;
use archivefs_core::dat::rename_apply::{
    EntryState, RenameTransaction, RollbackResult, TransactionSummary, journal_path, list_journals,
    read_journal, reconcile_recovery, rollback_transaction,
};
use archivefs_core::dat::sources::{DatSourceKind, suggest_display_name};
use archivefs_core::repair::QUARANTINE_DIRECTORY_NAME;
use archivefs_core::repair::execute::{
    RepairExecutionOptions, RepairReverifyEntry, RepairReverifyOutcome, RepairTransactionResult,
    reverify_transaction,
};
use archivefs_core::repair::library::{
    CombinedApplyResult, LibraryRepairPlan, RepairProfile, apply_saved_plan,
    apply_saved_plan_selected, plan_file_from_scan, preview_library_repair_plan, run_library_scan,
};
use archivefs_core::repair::proposal::{RepairEvidenceKind, RepairProposalId};
use archivefs_core::safe_read::TrustedRoots;

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(command) = args.first().cloned() else {
        return Err(
            "repair requires a sub-command: scan | plan | apply | history | rollback".into(),
        );
    };
    let rest = args[1..].to_vec();
    match command.as_str() {
        "scan" => run_scan(rest),
        "plan" => run_plan(rest),
        "apply" => run_apply(rest),
        "history" => run_history(rest),
        "rollback" => run_rollback(rest),
        _ => Err(format!(
            "unknown repair sub-command '{command}' (expected scan, plan, apply, history, or \
             rollback)"
        )
        .into()),
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
    let root = take_path_value(&mut args, "--root")?
        .ok_or("repair apply requires --root <library directory>")?;
    let dat = take_path_value(&mut args, "--dat")?
        .ok_or("repair apply requires --dat <catalogue file>")?;
    let generation = take_u64_value(&mut args, "--generation")?
        .ok_or("repair apply requires --generation <n> (the current audit generation)")?;
    let journal_dir = take_path_value(&mut args, "--journal-dir")?;
    let proposal_ids = take_repeated_string_values(&mut args, "--proposal-id")?;
    if !args.is_empty() {
        return Err(format!("repair apply does not accept {args:?}").into());
    }

    let plan = read_plan(&plan_path)?;

    let journal_dir = journal_dir.unwrap_or_else(|| {
        default_rename_transaction_dir().unwrap_or_else(|_| PathBuf::from("rename-transactions"))
    });
    // The trusted mutation root comes from the caller's `--root`, never from the
    // saved plan, so an edited plan cannot expand or redefine what may be touched.
    let trusted = TrustedRoots::from_paths([&root]);
    let options = RepairExecutionOptions {
        trusted,
        journal_dir,
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);

    // Re-scan with the trusted inputs, re-prove the saved plan against the fresh
    // scan, and execute the freshly authorized plan (or, when `--proposal-id`
    // is given, only the selected fresh proposals). `--generation` is the
    // caller's independent assertion of the current generation.
    //
    // A whole-plan apply (no `--proposal-id`) never mixes backends: it refuses
    // outright if the fresh plan contains any duplicate-quarantine proposal
    // (`ApplySavedPlanError::QuarantineRequiresSelectedApply`) rather than
    // silently excluding them or routing a `MovePath` through the generic
    // executor. A selected apply may freely mix DAT renames and
    // duplicate-quarantine moves in one invocation: each is executed through
    // its own backend and the results are reported together below.
    let combined = if proposal_ids.is_empty() {
        let result = apply_saved_plan(&plan, &root, &dat, generation, &options, &cancel)?;
        CombinedApplyResult {
            rename: Some(result),
            quarantine: Vec::new(),
        }
    } else {
        let selected: Vec<RepairProposalId> = proposal_ids
            .into_iter()
            .map(|raw| {
                RepairProposalId::new(raw.clone())
                    .ok_or_else(|| format!("--proposal-id '{raw}' is not a valid proposal id"))
            })
            .collect::<Result<_, _>>()?;
        apply_saved_plan_selected(&plan, &root, &dat, generation, &selected, &options, &cancel)?
    };

    let still_needs_review = plan.report.counts.needs_review + plan.report.counts.needs_review_sets;
    let still_needs_review_duplicate_groups = plan.report.counts.duplicate_quarantine_needs_review;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "still_needs_review": still_needs_review,
                "still_needs_review_duplicate_groups": still_needs_review_duplicate_groups,
                "rename": combined.rename.as_ref().map(transaction_result_json),
                "quarantine": combined
                    .quarantine
                    .iter()
                    .map(|entry| {
                        let mut value = transaction_result_json(&entry.result);
                        value["survivor_path"] = serde_json::json!(entry.survivor_path);
                        value
                    })
                    .collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("Repair apply complete:");
        println!("  Still NeedsReview: {still_needs_review}");
        println!("  Still NeedsReview duplicate groups: {still_needs_review_duplicate_groups}");
        if let Some(rename) = &combined.rename {
            println!();
            print!("{}", format_transaction_result("Rename batch", rename, &[]));
        }
        for entry in &combined.quarantine {
            println!();
            print!(
                "{}",
                format_transaction_result(
                    "Duplicate quarantine batch",
                    &entry.result,
                    &[("Survivor", entry.survivor_path.display().to_string())],
                )
            );
        }
        if combined.rename.is_none() && combined.quarantine.is_empty() {
            println!("  (nothing was applied)");
        }
    }
    Ok(())
}

/// Whether a journaled transaction is an ordinary repair rename or a
/// duplicate-quarantine move, derived only from where its entries actually
/// went - never from [`archivefs_core::repair::proposal::RepairEvidence`]'s
/// free-text `detail`, which the journal format does not even carry (a
/// [`RenameTransaction`] entry has no evidence field at all; evidence lives
/// only on the proposal that produced it, at plan time).
///
/// A quarantine move always lands under `<trusted_root>/.emuwiz-quarantine`
/// (see [`QUARANTINE_DIRECTORY_NAME`] and
/// `archivefs_core::repair::quarantine::quarantine_destination`); an
/// ordinary rename or move never does. This is checked per entry so a
/// transaction is only ever classified when every entry agrees - a
/// transaction with no entries, or (never produced by any current code path)
/// a mix of quarantine and non-quarantine destinations, is honestly reported
/// [`TransactionKind::Unknown`] rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionKind {
    DuplicateQuarantine,
    MoveRename,
    Unknown,
}

impl TransactionKind {
    fn label(self) -> &'static str {
        match self {
            Self::DuplicateQuarantine => "Duplicate quarantine",
            Self::MoveRename => "Move/Rename transaction",
            Self::Unknown => "UNKNOWN",
        }
    }

    fn json_tag(self) -> &'static str {
        match self {
            Self::DuplicateQuarantine => "duplicate_quarantine",
            Self::MoveRename => "move_rename",
            Self::Unknown => "unknown",
        }
    }
}

fn classify_transaction_kind(transaction: &RenameTransaction) -> TransactionKind {
    if transaction.entries.is_empty() {
        return TransactionKind::Unknown;
    }
    let quarantine_component = std::ffi::OsStr::new(QUARANTINE_DIRECTORY_NAME);
    let is_quarantine = |entry: &archivefs_core::dat::rename_apply::TransactionEntry| {
        entry
            .destination_path
            .components()
            .any(|component| component.as_os_str() == quarantine_component)
    };
    if transaction.entries.iter().all(is_quarantine) {
        TransactionKind::DuplicateQuarantine
    } else if transaction
        .entries
        .iter()
        .all(|entry| !is_quarantine(entry))
    {
        TransactionKind::MoveRename
    } else {
        TransactionKind::Unknown
    }
}

/// `emuwiz-cli repair history`: lists every transaction journaled in the
/// journal directory (the same one [`run_apply`] and the GUI's Repair
/// History page both read), reused verbatim - never a second transaction
/// database or log format.
fn run_history(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let journal_dir = take_path_value(&mut args, "--journal-dir")?.unwrap_or_else(|| {
        default_rename_transaction_dir().unwrap_or_else(|_| PathBuf::from("rename-transactions"))
    });
    let limit = take_string_value(&mut args, "--limit")?
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| format!("--limit value '{raw}' is not a valid number"))
        })
        .transpose()?;
    if !args.is_empty() {
        return Err(format!("repair history does not accept {args:?}").into());
    }

    let (mut transactions, load_problems) = list_journals(&journal_dir);
    // Reconcile any transaction a crash left mid-flight, exactly as the
    // GUI's own history refresh does, before reporting state/rollbackability
    // - never a second implementation of that classification.
    for transaction in &mut transactions {
        let needs_reconciliation = transaction
            .entries
            .iter()
            .any(|entry| matches!(entry.state, EntryState::Applying | EntryState::RollingBack));
        if needs_reconciliation {
            let _ = reconcile_recovery(transaction, &journal_dir);
        }
    }
    transactions.sort_by(|a, b| {
        b.created_at_unix
            .cmp(&a.created_at_unix)
            .then_with(|| b.transaction_id.cmp(&a.transaction_id))
    });
    if let Some(limit) = limit {
        transactions.truncate(limit);
    }

    if json {
        let entries: Vec<serde_json::Value> = transactions.iter().map(history_entry_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "journal_dir": journal_dir,
                "transaction_count": transactions.len(),
                "load_problems": load_problems,
                "transactions": entries,
            }))?
        );
    } else {
        println!(
            "Repair history: {} transaction(s) in {}",
            transactions.len(),
            journal_dir.display()
        );
        if !load_problems.is_empty() {
            println!(
                "  {} journal file(s) could not be parsed:",
                load_problems.len()
            );
            for problem in &load_problems {
                println!("    {problem}");
            }
        }
        for transaction in &transactions {
            print!("{}", format_history_entry_text(transaction));
        }
    }
    Ok(())
}

/// One transaction's full structured detail as JSON: [`RenameTransaction`]
/// already implements `Serialize`, so its complete entry list (source,
/// destination, per-entry state, failure reasons) is embedded verbatim
/// rather than re-derived field by field.
fn history_entry_json(transaction: &RenameTransaction) -> serde_json::Value {
    let summary = TransactionSummary::from_transaction(transaction);
    let reverify = reverify_transaction(transaction);
    let kind = classify_transaction_kind(transaction);
    serde_json::json!({
        "transaction_id": transaction.transaction_id,
        "state": transaction.state.label(),
        "kind": kind.json_tag(),
        "is_rollbackable": transaction.is_rollbackable(),
        "requested": summary.requested,
        "applied": summary.applied,
        "failed": summary.failed,
        "skipped": summary.skipped,
        "rollback_status": summary.rollback.label(),
        "reverify": reverify_json(&reverify),
        "transaction": transaction,
    })
}

fn reverify_json(reverify: &[RepairReverifyEntry]) -> Vec<serde_json::Value> {
    reverify
        .iter()
        .map(|entry| {
            serde_json::json!({
                "source_path": entry.source_path,
                "destination_path": entry.destination_path,
                "outcome": entry.outcome.label(),
                "detail": entry.detail,
            })
        })
        .collect()
}

/// One transaction's text summary for `repair history`: id, kind (never
/// derived from evidence strings - see [`classify_transaction_kind`]),
/// state, rollback status/rollbackability, requested/applied/failed/skipped,
/// the first source -> destination pair plus "+ N more", and a reverify
/// summary - the same information the GUI's Repair History card shows.
fn format_history_entry_text(transaction: &RenameTransaction) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let summary = TransactionSummary::from_transaction(transaction);
    let reverify = reverify_transaction(transaction);
    let kind = classify_transaction_kind(transaction);

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Transaction {} [{}]",
        transaction.transaction_id,
        kind.label()
    );
    let _ = writeln!(out, "  State: {}", transaction.state.label());
    let _ = writeln!(out, "  Rollback status: {}", summary.rollback.label());
    let _ = writeln!(
        out,
        "  Rollbackable now: {}",
        if transaction.is_rollbackable() {
            "yes"
        } else {
            "no"
        }
    );
    let _ = writeln!(
        out,
        "  Requested: {}  Applied: {}  Failed: {}  Skipped: {}",
        summary.requested, summary.applied, summary.failed, summary.skipped
    );
    match transaction.entries.first() {
        Some(first) => {
            let _ = writeln!(
                out,
                "  {} -> {}",
                first.source_path.display(),
                first.destination_path.display()
            );
            let remaining = transaction.entries.len() - 1;
            if remaining > 0 {
                let _ = writeln!(out, "  + {remaining} more");
            }
        }
        None => {
            let _ = writeln!(out, "  (no entries recorded)");
        }
    }
    if reverify.is_empty() {
        let _ = writeln!(
            out,
            "  Reverify: not applicable (nothing currently applied)"
        );
    } else {
        let verified = reverify
            .iter()
            .filter(|entry| entry.outcome == RepairReverifyOutcome::Verified)
            .count();
        let _ = writeln!(
            out,
            "  Reverify: {} of {} destination(s) verified",
            verified,
            reverify.len()
        );
        for entry in &reverify {
            if entry.outcome != RepairReverifyOutcome::Verified {
                let _ = writeln!(
                    out,
                    "    [{}] {}: {}",
                    entry.outcome.label(),
                    entry.destination_path.display(),
                    entry.detail
                );
            }
        }
    }
    out
}

/// `emuwiz-cli repair rollback`: undoes one journaled transaction through
/// the exact same [`rollback_transaction`] the GUI's "Undo" button and the
/// DAT Sources page's recovery UI both call. Never a hand-rolled inverse
/// rename: a duplicate-quarantine transaction rolls back through this same
/// call, with no special case - the quarantine backend journals ordinary
/// `MovePath` entries into the same [`RenameTransaction`]/journal format, so
/// the shared rollback engine already knows how to reverse it.
fn run_rollback(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let transaction_id = take_string_value(&mut args, "--transaction")?
        .ok_or("repair rollback requires --transaction <id>")?;
    let journal_dir = take_path_value(&mut args, "--journal-dir")?
        .ok_or("repair rollback requires --journal-dir <path>")?;
    if !args.is_empty() {
        return Err(format!("repair rollback does not accept {args:?}").into());
    }

    // 1. Resolve the exact journal path safely (rejects an id that could
    //    escape the journal directory or name something unsafe).
    let path = journal_path(&journal_dir, &transaction_id)
        .ok_or_else(|| format!("transaction id '{transaction_id}' cannot name a journal file"))?;

    // 2. Read the current journal fresh from disk - never a cached/earlier
    //    in-memory copy.
    let mut transaction = read_journal(&path).map_err(|error| {
        format!("transaction '{transaction_id}' could not be read from {journal_dir:?}: {error}")
    })?;

    // 3. Reconcile any in-flight recovery state before deciding
    //    rollbackability - the same read-only-to-files reconciliation the
    //    GUI's history refresh and `rollback_transaction` itself both rely
    //    on.
    let _ = reconcile_recovery(&mut transaction, &journal_dir);

    // 4. Require the existing rollbackability check. Refuses here, before
    //    calling into the rollback engine at all, for a transaction that is
    //    not rollbackable (nothing applied, or already fully rolled back).
    if !transaction.is_rollbackable() {
        let rollback_status = TransactionSummary::from_transaction(&transaction).rollback;
        return Err(format!(
            "transaction '{transaction_id}' is not rollbackable (state: {}, rollback: {})",
            transaction.state.label(),
            rollback_status.label()
        )
        .into());
    }

    // 5. Call the existing production rollback path. Every remaining
    //    refusal (source path occupied, destination missing/changed,
    //    ambiguous recovery state) is enforced inside `rollback_transaction`
    //    itself and reported back as a typed `RollbackResult`, never
    //    constructed here.
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let outcome = rollback_transaction(&mut transaction, &journal_dir, &cancel)
        .map_err(|error| format!("rollback could not complete: {error}"))?;

    // 6. Re-read/reverify the journal after rollback for reporting.
    let reread = read_journal(&path).unwrap_or_else(|_| outcome.transaction.clone());
    let reverify = reverify_transaction(&reread);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "transaction_id": reread.transaction_id,
                "result": rollback_result_label(&outcome.result),
                "restored": outcome.result.rolled_back_paths(),
                "failed": outcome
                    .result
                    .failed()
                    .iter()
                    .map(|(path, reason)| serde_json::json!({"path": path, "reason": reason}))
                    .collect::<Vec<_>>(),
                "reverify": reverify_json(&reverify),
                "transaction": reread,
            }))?
        );
    } else {
        println!("Repair rollback: transaction {}", reread.transaction_id);
        println!("  Result: {}", rollback_result_label(&outcome.result));
        match &outcome.result {
            RollbackResult::FullyRolledBack => {
                println!("  Entries restored: {}", reread.rolled_back_count());
            }
            RollbackResult::PartiallyRolledBack {
                rolled_back,
                failed,
            } => {
                println!("  Entries restored: {}", rolled_back.len());
                println!("  Entries not restored: {}", failed.len());
                for (path, reason) in failed {
                    println!("    {}: {}", path.display(), reason);
                }
            }
            RollbackResult::RollbackFailed { failed } => {
                println!("  Entries not restored: {}", failed.len());
                for (path, reason) in failed {
                    println!("    {}: {}", path.display(), reason);
                }
            }
        }
        println!("  Reverify:");
        if reverify.is_empty() {
            println!("    not applicable (nothing currently applied)");
        }
        for entry in &reverify {
            println!(
                "    [{}] {} -> {}: {}",
                entry.outcome.label(),
                entry.source_path.display(),
                entry.destination_path.display(),
                entry.detail
            );
        }
    }

    // The normal result (text or JSON) is always printed above before this
    // returns: only the exit/`Result` signals success. A rollback that did
    // not fully complete is never reported as success just because the
    // command itself ran to completion without a hard error.
    match &outcome.result {
        RollbackResult::FullyRolledBack => Ok(()),
        RollbackResult::PartiallyRolledBack { .. } => {
            Err(format!("transaction '{transaction_id}' was only partially rolled back").into())
        }
        RollbackResult::RollbackFailed { .. } => {
            Err(format!("transaction '{transaction_id}' rollback failed").into())
        }
    }
}

fn rollback_result_label(result: &RollbackResult) -> &'static str {
    match result {
        RollbackResult::FullyRolledBack => "FullyRolledBack",
        RollbackResult::PartiallyRolledBack { .. } => "PartiallyRolledBack",
        RollbackResult::RollbackFailed { .. } => "RollbackFailed",
    }
}

/// One applied transaction (an ordinary DAT rename batch, or one
/// duplicate-quarantine survivor group), reported as JSON. Reused for both
/// so a caller can tell the two apart only by which section they appear in
/// (`rename` vs `quarantine`), never by inventing a different shape for each.
fn transaction_result_json(result: &RepairTransactionResult) -> serde_json::Value {
    let rolled_back = matches!(
        result.summary.rollback,
        RollbackStatus::FullyRolledBack | RollbackStatus::PartiallyRolledBack
    );
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
    let destinations: Vec<serde_json::Value> = result
        .transaction
        .entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "source_path": entry.source_path,
                "destination_path": entry.destination_path,
            })
        })
        .collect();
    serde_json::json!({
        "transaction_id": result.summary.transaction_id,
        "requested": result.summary.requested,
        "applied": result.summary.applied,
        "failed": result.summary.failed,
        "skipped": result.summary.skipped,
        "rolled_back": rolled_back,
        "destinations": destinations,
        "reverify": reverify,
    })
}

/// The same one-transaction report, as text. `extra` adds label/value rows
/// specific to the caller (a quarantine batch's survivor path) without
/// duplicating the rest of the format.
fn format_transaction_result(
    label: &str,
    result: &RepairTransactionResult,
    extra: &[(&str, String)],
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let rolled_back = matches!(
        result.summary.rollback,
        RollbackStatus::FullyRolledBack | RollbackStatus::PartiallyRolledBack
    );
    let _ = writeln!(
        out,
        "{label} (transaction {}):",
        result.summary.transaction_id
    );
    for (key, value) in extra {
        let _ = writeln!(out, "  {key}: {value}");
    }
    let _ = writeln!(out, "  Requested: {}", result.summary.requested);
    let _ = writeln!(out, "  Applied: {}", result.summary.applied);
    let _ = writeln!(out, "  Failed: {}", result.summary.failed);
    let _ = writeln!(out, "  Skipped: {}", result.summary.skipped);
    let _ = writeln!(
        out,
        "  Rolled back: {}",
        if rolled_back { "yes" } else { "no" }
    );
    if !result.transaction.entries.is_empty() {
        let _ = writeln!(out, "  Destinations:");
        for entry in &result.transaction.entries {
            let _ = writeln!(
                out,
                "    {} -> {}",
                entry.source_path.display(),
                entry.destination_path.display()
            );
        }
    }
    let _ = writeln!(out, "  Reverify:");
    for entry in &result.reverify {
        let _ = writeln!(
            out,
            "    [{}] {} -> {}: {}",
            entry.outcome.label(),
            entry.source_path.display(),
            entry.destination_path.display(),
            entry.detail
        );
    }
    out
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
    let _ = writeln!(
        out,
        "  Truncated: {}",
        if plan.truncated { "yes" } else { "no" }
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  Files encountered:   {}", plan.files_scanned);
    let _ = writeln!(out, "  DAT candidates:      {}", counts.dat_candidates);
    let _ = writeln!(out, "  Ignored ancillary:   {}", counts.ignored_ancillary);
    if !plan.report.ignored_ancillary_by_extension.is_empty() {
        let _ = writeln!(out, "    by extension:");
        for (extension, count) in &plan.report.ignored_ancillary_by_extension {
            let _ = writeln!(out, "      {extension}: {count}");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Counts:");
    let _ = writeln!(out, "  Complete sets: {}", counts.complete_sets);
    let _ = writeln!(out, "  Incomplete sets: {}", counts.incomplete_sets);
    let _ = writeln!(out, "  Bad metadata sets: {}", counts.bad_metadata_sets);
    let _ = writeln!(out, "  NeedsReview sets: {}", counts.needs_review_sets);
    let _ = writeln!(out, "  Already canonical: {}", counts.already_canonical);
    let _ = writeln!(out, "  Safe repairs: {}", counts.safe_repairs);
    let _ = writeln!(
        out,
        "  Unmatched candidates: {}",
        counts.unmatched_candidates
    );
    let _ = writeln!(out, "  NeedsReview: {}", counts.needs_review);
    let _ = writeln!(out, "  Blocked: {}", counts.blocked_repair);
    let _ = writeln!(out, "  Unsupported: {}", counts.unsupported);
    let _ = writeln!(out, "  Scan errors: {}", counts.scan_errors);
    let _ = writeln!(out);
    let _ = writeln!(out, "Duplicate quarantine:");
    let _ = writeln!(
        out,
        "  Groups examined: {}",
        counts.duplicate_groups_examined
    );
    let _ = writeln!(
        out,
        "  Groups content-proven: {}",
        counts.duplicate_groups_content_proven
    );
    let _ = writeln!(out, "  Safe: {}", counts.duplicate_quarantine_safe);
    let _ = writeln!(
        out,
        "  NeedsReview: {}",
        counts.duplicate_quarantine_needs_review
    );
    let _ = writeln!(
        out,
        "  SameObject ignored: {}",
        counts.duplicate_same_object_ignored
    );
    let _ = writeln!(
        out,
        "  Content mismatch/refused: {}",
        counts.duplicate_content_mismatch_refused
    );
    let _ = writeln!(out);

    // Additive and clearly distinguishable from the ordinary DAT rename
    // batch: `survivor_path` is set only for a duplicate-quarantine
    // `MovePath` proposal (see `RepairProposal::survivor_path`'s doc), never
    // for a `RenamePath` or any other `MovePath` this foundation might grow.
    let (quarantine_proposals, rename_proposals): (Vec<_>, Vec<_>) = plan
        .repair_plan
        .proposals
        .iter()
        .partition(|proposal| proposal.is_duplicate_quarantine());

    let _ = writeln!(out, "SAFE");
    for proposal in &rename_proposals {
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

    let _ = writeln!(out, "SAFE DUPLICATE QUARANTINE");
    for proposal in &quarantine_proposals {
        let duplicate_content_evidence = proposal
            .evidence
            .iter()
            .any(|evidence| evidence.kind == RepairEvidenceKind::DuplicateContent);
        let _ = writeln!(
            out,
            "{} [id {}]",
            proposal.source_path.display(),
            proposal.id
        );
        let _ = writeln!(
            out,
            "  Destination: {}",
            proposal
                .destination()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        );
        let _ = writeln!(
            out,
            "  Survivor: {}",
            proposal
                .survivor_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        let _ = writeln!(out, "  Safety: {}", proposal.safety.label());
        let _ = writeln!(
            out,
            "  DuplicateContent evidence: {}",
            if duplicate_content_evidence {
                "yes"
            } else {
                "no"
            }
        );
        let _ = writeln!(out, "  Reason: {}", proposal.reason);
    }

    for (heading, items) in [
        ("NEEDS REVIEW", &plan.report.needs_review),
        ("BLOCKED", &plan.report.blocked),
        ("UNSUPPORTED", &plan.report.unsupported),
        (
            "DUPLICATE NEEDS REVIEW",
            &plan.report.duplicate_needs_review,
        ),
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

/// Collects every occurrence of a repeated flag (e.g. `--proposal-id a
/// --proposal-id b`), removing them from `args` and preserving the order the
/// caller passed them in. Selection *execution* order is decided later from
/// the fresh plan, never from this order.
fn take_repeated_string_values(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if i + 1 >= args.len() {
                return Err(format!("{flag} requires a value").into());
            }
            args.remove(i);
            values.push(args.remove(i));
        } else {
            i += 1;
        }
    }
    Ok(values)
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
    use archivefs_core::dat::rename_apply::TransactionState;
    use archivefs_core::repair::proposal::{RepairAction, SafetyState};

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

        let plan: archivefs_core::repair::library::LibraryRepairPlan =
            serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();

        // apply: explicit, with the trusted inputs and the current generation.
        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
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
    fn apply_without_generation_refuses() {
        let (dir, dat, roms) = fixture();
        let plan_path = dir.path().join("plan.json");
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

        // Missing --generation must refuse before any mutation (--root/--dat given).
        let error = run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--generation"), "{error}");
        assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
    }

    #[test]
    fn apply_with_stale_generation_refuses() {
        let (dir, dat, roms) = fixture();
        let plan_path = dir.path().join("plan.json");
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
        let plan: archivefs_core::repair::library::LibraryRepairPlan =
            serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();

        // A generation that does not match the fresh re-scan's generation.
        let error = run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.wrapping_add(1).to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("stale"), "{error}");
        assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
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

    const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    /// A two-game DAT + two wrongly-named loose ROMs, for `--proposal-id`
    /// selected-apply tests.
    fn two_proposal_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("two.dat");
        std::fs::write(
            &dat,
            format!(
                r#"<datafile><header><name>Two</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
            ),
        )
        .unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir(&roms).unwrap();
        std::fs::write(roms.join("a.bin"), b"test").unwrap();
        std::fs::write(roms.join("b.bin"), b"abc").unwrap();
        (dir, dat, roms)
    }

    #[test]
    fn repeated_proposal_id_applies_only_the_selected_proposal() {
        let (dir, dat, roms) = two_proposal_fixture();
        let plan_path = dir.path().join("plan.json");

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

        let plan: LibraryRepairPlan =
            serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();
        let beta_id = plan
            .repair_plan
            .proposals
            .iter()
            .find(|p| p.source_path.file_name().unwrap() == "b.bin")
            .expect("a beta proposal exists")
            .id
            .as_str()
            .to_string();

        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
            "--proposal-id".into(),
            beta_id,
        ])
        .unwrap();

        assert!(roms.join("beta.bin").exists(), "the selected rename ran");
        assert!(
            roms.join("a.bin").exists(),
            "the unselected rom is untouched"
        );
        assert!(
            !roms.join("alpha.bin").exists(),
            "the unselected rename did not run"
        );
    }

    #[test]
    fn an_unknown_proposal_id_refuses_before_mutation() {
        let (dir, dat, roms) = two_proposal_fixture();
        let plan_path = dir.path().join("plan.json");

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
        let plan: LibraryRepairPlan =
            serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();

        let error = run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
            "--proposal-id".into(),
            "does-not-exist".into(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
        assert!(roms.join("a.bin").exists());
        assert!(roms.join("b.bin").exists());
    }

    // ---------------------------------------------------------------------
    // Duplicate quarantine through the CLI: scan output, selected apply, and
    // the fresh re-proof/fail-closed behaviour, all through `run()` exactly as
    // a user would invoke it.
    // ---------------------------------------------------------------------

    /// A DAT declaring one game/rom `canon.bin`; a library containing the
    /// canonical keeper and a byte-identical redundant copy under a different
    /// name in the same directory (so the redundant copy's own DAT rename
    /// collides with the keeper and is never itself a rename proposal - only a
    /// quarantine candidate).
    fn duplicate_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("dup.dat");
        std::fs::write(
            &dat,
            format!(
                r#"<?xml version="1.0"?>
<datafile>
    <header><name>Dup</name></header>
    <game name="Game">
        <rom name="canon.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
            ),
        )
        .unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir(&roms).unwrap();
        std::fs::write(roms.join("canon.bin"), b"test").unwrap();
        std::fs::write(roms.join("redundant-copy.bin"), b"test").unwrap();
        (dir, dat, roms)
    }

    /// A DAT declaring one game/rom `canon.bin`; two wrongly-named loose copies
    /// of it in two different directories, so both are `Suggested` and
    /// confidently verified but neither is already-canonical - a tie, so no
    /// unique objective survivor exists.
    fn needs_review_duplicate_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dat = dir.path().join("tie.dat");
        std::fs::write(
            &dat,
            format!(
                r#"<?xml version="1.0"?>
<datafile>
    <header><name>Tie</name></header>
    <game name="Game">
        <rom name="canon.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
            ),
        )
        .unwrap();
        let roms = dir.path().join("roms");
        std::fs::create_dir(&roms).unwrap();
        let a = roms.join("a");
        let b = roms.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("wrong-a.bin"), b"test").unwrap();
        std::fs::write(b.join("wrong-b.bin"), b"test").unwrap();
        (dir, dat, roms)
    }

    fn scan_to_plan_path(
        roms: &Path,
        dat: &Path,
        plan_path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        run(vec![
            "scan".into(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--plan-out".into(),
            plan_path.display().to_string(),
        ])
    }

    fn read_saved_plan(plan_path: &Path) -> LibraryRepairPlan {
        serde_json::from_str(&std::fs::read_to_string(plan_path).unwrap()).unwrap()
    }

    // A. scan output includes a Safe duplicate-quarantine proposal, distinct
    // from the ordinary rename output.
    #[test]
    fn scan_output_shows_a_safe_duplicate_quarantine_proposal() {
        let (dir, dat, roms) = duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);

        let quarantine_proposal = plan
            .repair_plan
            .proposals
            .iter()
            .find(|p| p.survivor_path.is_some())
            .expect("a Safe duplicate quarantine proposal exists");
        assert_eq!(
            quarantine_proposal.source_path,
            roms.join("redundant-copy.bin")
        );
        assert_eq!(
            quarantine_proposal.survivor_path.as_deref(),
            Some(roms.join("canon.bin").as_path())
        );
        assert_eq!(quarantine_proposal.safety, SafetyState::Safe);
        assert!(matches!(
            quarantine_proposal.action,
            RepairAction::MovePath { .. }
        ));
        assert!(
            quarantine_proposal
                .evidence
                .iter()
                .any(|e| e.kind == RepairEvidenceKind::DuplicateContent)
        );

        let report = format_report(&plan);
        assert!(report.contains("SAFE DUPLICATE QUARANTINE"));
        assert!(report.contains(&roms.join("redundant-copy.bin").display().to_string()));
        assert!(report.contains(&format!("Survivor: {}", roms.join("canon.bin").display())));
        assert!(report.contains("Safety: safe"));
        assert!(report.contains("DuplicateContent evidence: yes"));
    }

    // B. a NeedsReview duplicate group is visible in scan output but produces
    // no Safe/actionable proposal.
    #[test]
    fn scan_output_shows_a_needs_review_duplicate_group_but_no_safe_proposal() {
        let (dir, dat, roms) = needs_review_duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);

        assert!(
            !plan
                .repair_plan
                .proposals
                .iter()
                .any(|p| p.survivor_path.is_some()),
            "a tied group must never produce a Safe quarantine proposal"
        );
        assert_eq!(plan.report.counts.duplicate_quarantine_needs_review, 1);
        assert_eq!(plan.report.duplicate_needs_review.len(), 2);

        let report = format_report(&plan);
        assert!(report.contains("DUPLICATE NEEDS REVIEW"));
        assert!(report.contains(&roms.join("a").join("wrong-a.bin").display().to_string()));
        assert!(report.contains(&roms.join("b").join("wrong-b.bin").display().to_string()));
    }

    // C. a selected Safe quarantine proposal applies through the CLI and the
    // quarantine-specific backend (never the generic executor, which would
    // refuse a `MovePath` into a directory that does not exist yet).
    #[test]
    fn selected_quarantine_proposal_applies_through_the_cli() {
        let (dir, dat, roms) = duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);

        let quarantine_id = plan
            .repair_plan
            .proposals
            .iter()
            .find(|p| p.survivor_path.is_some())
            .expect("a quarantine proposal exists")
            .id
            .as_str()
            .to_string();

        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
            "--proposal-id".into(),
            quarantine_id,
        ])
        .unwrap();

        assert!(
            !roms.join("redundant-copy.bin").exists(),
            "the duplicate moved out of its original location"
        );
        assert!(roms.join("canon.bin").exists(), "the survivor is untouched");
        assert_eq!(std::fs::read(roms.join("canon.bin")).unwrap(), b"test");
        assert!(
            roms.join(".emuwiz-quarantine").exists(),
            "only the quarantine-specific backend creates this directory"
        );
    }

    // E. a survivor changed between scan and apply is caught by the fresh
    // re-scan/re-proof, so the CLI apply refuses before any mutation.
    #[test]
    fn a_changed_survivor_refuses_the_cli_apply() {
        let (dir, dat, roms) = duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);

        let quarantine_id = plan
            .repair_plan
            .proposals
            .iter()
            .find(|p| p.survivor_path.is_some())
            .expect("a quarantine proposal exists")
            .id
            .as_str()
            .to_string();

        // The survivor's content changes after the plan was saved.
        std::fs::write(roms.join("canon.bin"), b"different-content-same-slot").unwrap();

        let error = run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            dir.path().join("journal").display().to_string(),
            "--proposal-id".into(),
            quarantine_id,
        ])
        .unwrap_err();
        assert!(error.to_string().contains("not authorized"), "{error}");
        assert!(
            roms.join("redundant-copy.bin").exists(),
            "nothing was moved"
        );
        assert!(!roms.join(".emuwiz-quarantine").exists());
    }

    // I. `repair scan` (and the plan it writes) never creates the quarantine
    // directory - planning stays read-only even when it discovers a Safe
    // duplicate-quarantine proposal.
    #[test]
    fn scan_creates_no_quarantine_directory() {
        let (dir, dat, roms) = duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();

        assert!(!roms.join(".emuwiz-quarantine").exists());
        assert!(roms.join("canon.bin").exists());
        assert!(roms.join("redundant-copy.bin").exists());

        // `repair plan` (the dry-run preview over the saved plan) is equally
        // read-only.
        run(vec![
            "plan".into(),
            "--plan".into(),
            plan_path.display().to_string(),
        ])
        .unwrap();
        assert!(!roms.join(".emuwiz-quarantine").exists());
    }

    // -----------------------------------------------------------------
    // `repair history` / `repair rollback`
    //
    // The formatted output itself (`format_history_entry_text`,
    // `history_entry_json`) is tested directly as a pure function, exactly
    // like `format_report` above - this crate's tests do not capture the
    // process's real stdout (`run()` writes via `println!`/`print!`
    // directly, with no injected writer), so command-level tests here
    // assert `run()`'s `Result` and the resulting filesystem/journal state,
    // never printed text.
    // -----------------------------------------------------------------

    fn journal_dir_for(dir: &Path) -> PathBuf {
        dir.join("journal")
    }

    /// Runs a `scan` + `apply` of the ordinary (non-duplicate) fixture and
    /// returns the journal directory and the applied transaction id.
    fn scanned_and_applied(dir: &tempfile::TempDir, dat: &Path, roms: &Path) -> (PathBuf, String) {
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(roms, dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);
        let journal_dir = journal_dir_for(dir.path());

        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();

        let (transactions, problems) = list_journals(&journal_dir);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(transactions.len(), 1, "{transactions:?}");
        (journal_dir, transactions[0].transaction_id.clone())
    }

    #[test]
    fn repair_history_runs_and_lists_the_applied_transaction() {
        let (dir, dat, roms) = fixture();
        let (journal_dir, transaction_id) = scanned_and_applied(&dir, &dat, &roms);

        run(vec![
            "history".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();
        run(vec![
            "history".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
            "--json".into(),
        ])
        .unwrap();
        run(vec![
            "history".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
            "--limit".into(),
            "1".into(),
        ])
        .unwrap();

        let (transactions, _) = list_journals(&journal_dir);
        let transaction = transactions
            .iter()
            .find(|t| t.transaction_id == transaction_id)
            .expect("the applied transaction is journaled");
        assert_eq!(transaction.state, TransactionState::Applied);
        assert!(transaction.is_rollbackable());

        // The text and JSON formatters agree it is an ordinary rename, never
        // a duplicate quarantine, and the text summary names the actual
        // source/destination.
        assert_eq!(
            classify_transaction_kind(transaction),
            TransactionKind::MoveRename
        );
        let text = format_history_entry_text(transaction);
        assert!(text.contains(&transaction.transaction_id));
        assert!(text.contains("Move/Rename transaction"));
        assert!(text.contains("wrongname.bin"));
        assert!(text.contains("super.bin"));
        assert!(text.contains("Rollbackable now: yes"));

        let json = history_entry_json(transaction);
        assert_eq!(json["kind"], "move_rename");
        assert_eq!(json["is_rollbackable"], true);
        assert_eq!(
            json["transaction"]["transaction_id"],
            transaction.transaction_id.as_str()
        );
    }

    #[test]
    fn repair_history_with_an_unreadable_journal_dir_reports_no_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = dir.path().join("does-not-exist");
        // `list_journals` tolerates a missing directory (empty result, no
        // error) - `repair history` must too.
        run(vec![
            "history".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();
    }

    #[test]
    fn repair_rollback_restores_the_original_file() {
        let (dir, dat, roms) = fixture();
        let (journal_dir, transaction_id) = scanned_and_applied(&dir, &dat, &roms);

        assert!(roms.join("super.bin").exists());
        assert!(!roms.join("wrongname.bin").exists());

        run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction_id.clone(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();

        assert!(
            roms.join("wrongname.bin").exists(),
            "rollback restored the original name"
        );
        assert!(!roms.join("super.bin").exists());

        let (transactions, _) = list_journals(&journal_dir);
        let transaction = transactions
            .iter()
            .find(|t| t.transaction_id == transaction_id)
            .unwrap();
        assert_eq!(transaction.state, TransactionState::RolledBack);
        assert!(!transaction.is_rollbackable(), "already rolled back");
    }

    #[test]
    fn repair_rollback_json_reports_the_full_rolled_back_transaction() {
        let (dir, dat, roms) = fixture();
        let (journal_dir, transaction_id) = scanned_and_applied(&dir, &dat, &roms);

        run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction_id.clone(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
            "--json".into(),
        ])
        .unwrap();
        assert!(roms.join("wrongname.bin").exists());
    }

    #[test]
    fn repair_rollback_refuses_a_transaction_already_rolled_back() {
        let (dir, dat, roms) = fixture();
        let (journal_dir, transaction_id) = scanned_and_applied(&dir, &dat, &roms);

        run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction_id.clone(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();

        let error = run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction_id,
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("not rollbackable"), "{error}");
    }

    #[test]
    fn repair_rollback_refuses_an_unknown_transaction_id() {
        let dir = tempfile::tempdir().unwrap();
        let journal_dir = dir.path().join("journal");
        std::fs::create_dir_all(&journal_dir).unwrap();

        let error = run(vec![
            "rollback".into(),
            "--transaction".into(),
            "does-not-exist".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("could not be read"), "{error}");
    }

    #[test]
    fn repair_rollback_refuses_a_destination_that_changed_since_apply() {
        let (dir, dat, roms) = fixture();
        let (journal_dir, transaction_id) = scanned_and_applied(&dir, &dat, &roms);

        // The applied destination is replaced with different-sized content
        // before rollback is attempted, so the recorded identity's size
        // guarantees a mismatch regardless of mtime resolution -
        // `rollback_transaction` itself must refuse rather than clobber it.
        std::fs::write(
            roms.join("super.bin"),
            b"tampered-content-of-a-different-size",
        )
        .unwrap();

        // The command still runs to completion and reports the normal
        // result, but a `RollbackFailed` outcome is never reported as
        // success: the `Result` (and therefore the process exit code) is
        // `Err`.
        let error = run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction_id.clone(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("rollback failed"), "{error}");

        // Nothing was restored: the changed file is untouched and the
        // original name never reappeared.
        assert_eq!(
            std::fs::read(roms.join("super.bin")).unwrap(),
            b"tampered-content-of-a-different-size"
        );
        assert!(!roms.join("wrongname.bin").exists());

        let (transactions, _) = list_journals(&journal_dir);
        let transaction = transactions
            .iter()
            .find(|t| t.transaction_id == transaction_id)
            .unwrap();
        assert_eq!(transaction.state, TransactionState::RollbackFailed);
    }

    #[test]
    fn repair_rollback_requires_transaction_and_journal_dir_flags() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(vec!["rollback".into()]).unwrap_err();
        assert!(error.to_string().contains("--transaction"), "{error}");

        let error = run(vec![
            "rollback".into(),
            "--transaction".into(),
            "some-id".into(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--journal-dir"), "{error}");
        let _ = dir;
    }

    #[test]
    fn a_quarantine_transaction_appears_in_history_and_rolls_back_through_the_same_command() {
        let (dir, dat, roms) = duplicate_fixture();
        let plan_path = dir.path().join("plan.json");
        scan_to_plan_path(&roms, &dat, &plan_path).unwrap();
        let plan = read_saved_plan(&plan_path);
        let journal_dir = journal_dir_for(dir.path());

        let quarantine_id = plan
            .repair_plan
            .proposals
            .iter()
            .find(|p| p.is_duplicate_quarantine())
            .expect("a quarantine proposal exists")
            .id
            .as_str()
            .to_string();

        run(vec![
            "apply".into(),
            "--plan".into(),
            plan_path.display().to_string(),
            "--root".into(),
            roms.display().to_string(),
            "--dat".into(),
            dat.display().to_string(),
            "--generation".into(),
            plan.generation.to_string(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
            "--proposal-id".into(),
            quarantine_id,
        ])
        .unwrap();

        assert!(!roms.join("redundant-copy.bin").exists());
        assert!(roms.join(".emuwiz-quarantine").exists());

        let (transactions, problems) = list_journals(&journal_dir);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(transactions.len(), 1, "{transactions:?}");
        let transaction = &transactions[0];

        // `repair history` classifies it as a duplicate quarantine - derived
        // only from its journaled destination path, exactly as
        // `classify_transaction_kind`'s doc says.
        assert_eq!(
            classify_transaction_kind(transaction),
            TransactionKind::DuplicateQuarantine
        );
        let text = format_history_entry_text(transaction);
        assert!(text.contains("Duplicate quarantine"));

        run(vec![
            "history".into(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();

        // Rollback runs through the exact same `repair rollback` command as
        // an ordinary rename transaction - no special case.
        run(vec![
            "rollback".into(),
            "--transaction".into(),
            transaction.transaction_id.clone(),
            "--journal-dir".into(),
            journal_dir.display().to_string(),
        ])
        .unwrap();

        assert!(
            roms.join("redundant-copy.bin").exists(),
            "the quarantined file was restored to its original location"
        );
        assert_eq!(
            std::fs::read(roms.join("redundant-copy.bin")).unwrap(),
            b"test"
        );
    }
}
