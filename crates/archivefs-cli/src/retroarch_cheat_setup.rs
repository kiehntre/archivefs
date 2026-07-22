use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use archivefs_core::default_database_path;
use archivefs_core::emulator_environment::retroarch::{
    DiscoveryEnvironment, ProfileKind, ProfileScope,
};
use archivefs_core::emulator_environment::{EncodedPath, HostReadOnlyFilesystem};
use archivefs_core::patch_manager::{
    CHEAT_INSTALL_BACKUPS_DIRECTORY_NAME, CHEAT_INSTALL_RUNS_DIRECTORY_NAME, CheatInstallOptions,
    CheatInstallRunOutcome, CheatInstallRunStatus, RETROARCH_CHEAT_SETUP_SCHEMA_VERSION,
    RetroArchCheatSetupDiscovery, RetroArchCheatSetupError, RetroArchCheatSetupNextStep,
    RetroArchCheatSetupPlan, RetroArchCheatSetupProfile, RetroArchCheatSetupResult,
    RetroArchCheatSetupStatus, build_retroarch_cheat_setup_plan,
    discover_retroarch_cheat_setup_profiles, execute_cheat_install_run,
    resolve_retroarch_cheat_setup_profile,
};

use crate::retroarch_cheat_sources::{SourceOptions, fetch_source};

#[derive(Debug)]
struct SetupCliOptions {
    catalogue_path: PathBuf,
    source_id: Option<String>,
    source_result: Option<archivefs_core::patch_manager::CheatSourceFetchResult>,
    source_options: Option<SourceOptions>,
    profile_id: Option<String>,
    dry_run: bool,
    yes: bool,
    replace_different: bool,
    json: bool,
    database_path: PathBuf,
    configuration_path: Option<PathBuf>,
}

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = parse_options(args)?;
    if let (Some(source_id), Some(source_options)) = (&options.source_id, &options.source_options) {
        match fetch_source(source_id, source_options) {
            Ok(result) => {
                options.catalogue_path = source_options
                    .cache_root
                    .join(&result.source.source_id)
                    .join("snapshots")
                    .join(&result.manifest.archive_sha256)
                    .join(&result.manifest.catalogue_relative_path);
                options.source_result = Some(result);
            }
            Err(error) => {
                if options.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema_version": 1,
                            "status": "failed",
                            "operation": "source_retrieval",
                            "source_id": source_id,
                            "error": error,
                        }))?
                    );
                }
                return Err(Box::new(error));
            }
        }
    }
    let filesystem = HostReadOnlyFilesystem;
    let environment = DiscoveryEnvironment::from_process_environment();
    let discovery = match discover_retroarch_cheat_setup_profiles(
        &filesystem,
        &environment,
        options.configuration_path.as_deref(),
    ) {
        Ok(discovery) => discovery,
        Err(error) => return fail(&options, None, error),
    };

    let selected = if let Some(profile_id) = options.profile_id.as_deref() {
        match resolve_retroarch_cheat_setup_profile(&discovery, Some(profile_id)) {
            Ok(profile) => Some(profile),
            Err(error) => return fail(&options, Some(&discovery), error),
        }
    } else {
        let eligible = discovery
            .profiles
            .iter()
            .filter(|profile| profile.eligible)
            .cloned()
            .collect::<Vec<_>>();
        match eligible.len() {
            0 => {
                return fail(
                    &options,
                    Some(&discovery),
                    RetroArchCheatSetupError::new(
                        "no_eligible_profiles",
                        "no discovered RetroArch profile has a safe, resolved cheats destination",
                    ),
                );
            }
            1 => Some(eligible[0].clone()),
            _ if options.json || options.dry_run || !io::stdin().is_terminal() => {
                return fail(
                    &options,
                    Some(&discovery),
                    RetroArchCheatSetupError::new(
                        "profile_selection_required",
                        format!(
                            "{} eligible profiles were found; pass --profile with one exact profile ID",
                            eligible.len()
                        ),
                    ),
                );
            }
            _ => {
                print_profile_choices(&eligible);
                let stdin = io::stdin();
                let mut locked = stdin.lock();
                match select_profile_from_reader(&eligible, &mut locked)? {
                    Some(profile) => Some(profile),
                    None => {
                        println!("\nSetup cancelled. No changes were made.");
                        return Ok(());
                    }
                }
            }
        }
    };
    let selected = selected.ok_or("profile selection ended without a selected profile")?;

    let data_directory = default_database_path()?
        .parent()
        .ok_or("could not determine the ArchiveFS data directory")?
        .to_path_buf();
    let run_id = generate_setup_run_id();
    let journal_directory = data_directory.join(CHEAT_INSTALL_RUNS_DIRECTORY_NAME);
    let journal_path = journal_directory.join(format!("{run_id}.json"));
    let plan = match build_retroarch_cheat_setup_plan(
        &filesystem,
        &discovery,
        &selected,
        &options.catalogue_path,
        &options.database_path,
        &journal_path,
        options.replace_different,
    ) {
        Ok(plan) => plan,
        Err(error) => return fail(&options, Some(&discovery), error),
    };

    if !options.json {
        print_preview(&plan, &options);
    }

    let writes = plan.preview.summary.total_writes_proposed;
    let apply = if writes == 0 || options.dry_run || (options.json && !options.yes) {
        false
    } else if options.yes {
        true
    } else {
        println!("Confirmation\n  Apply these changes? Type 'yes' to continue.");
        let stdin = io::stdin();
        let mut locked = stdin.lock();
        confirm_from_reader(&mut locked)?
    };

    if !apply {
        let status = if writes > 0 && !options.dry_run && !options.json && !options.yes {
            RetroArchCheatSetupStatus::Cancelled
        } else {
            RetroArchCheatSetupStatus::Preview
        };
        let result = result_from_plan(&options, &discovery, &plan, status, None, None, Vec::new());
        if options.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if status == RetroArchCheatSetupStatus::Cancelled {
            println!("\nSetup cancelled. No changes were made.");
        } else if options.dry_run {
            println!("\nDry run complete. No changes were made.");
            if writes == 0 {
                print_zero_write_cause(&plan);
            }
        } else if writes == 0 {
            print_zero_write_explanation(&plan);
        }
        return Ok(());
    }

    let installer_options = CheatInstallOptions {
        destination_root: plan.destination_root.clone(),
        allow_replace_different: options.replace_different,
        dry_run: false,
        confirmed: true,
        journal_directory,
        backup_directory: data_directory.join(CHEAT_INSTALL_BACKUPS_DIRECTORY_NAME),
        run_id,
        started_at_unix_seconds: unix_seconds_now(),
        catalogue_source: options
            .source_id
            .clone()
            .unwrap_or_else(|| "local-catalogue".to_string()),
    };
    let outcome = execute_cheat_install_run(&plan.installer_entries, &installer_options);
    let failed = matches!(
        outcome.run.status,
        CheatInstallRunStatus::Failed | CheatInstallRunStatus::PartialFailure
    ) || outcome.journal_error.is_some()
        || outcome.run.summary.writes_succeeded < writes;
    let mut errors = Vec::new();
    if let Some(error) = &outcome.journal_error {
        errors.push(RetroArchCheatSetupError::new(
            "journal_write_failed",
            error.clone(),
        ));
    }
    if failed && errors.is_empty() {
        errors.push(RetroArchCheatSetupError::new(
            "installation_refused_or_failed",
            "one or more previewed writes were refused by installer revalidation or did not complete successfully",
        ));
    }
    let status = if failed {
        RetroArchCheatSetupStatus::Failed
    } else {
        RetroArchCheatSetupStatus::Applied
    };
    let next_steps = if failed {
        vec![step(
            1,
            "review_failed_installation",
            "Review the structured installer entries and journal before retrying; refused writes were left unapplied.",
            outcome.journal_path.as_deref().map(|journal| {
                vec![
                    "archivefs".into(),
                    "retroarch-cheat-inspect".into(),
                    journal.display().to_string(),
                ]
            }),
        )]
    } else {
        outcome
            .journal_path
            .as_deref()
            .map(|journal| next_steps(journal, &plan.destination_root))
            .unwrap_or_default()
    };
    let result = result_from_plan(
        &options,
        &discovery,
        &plan,
        status,
        Some(&outcome),
        outcome.journal_path.as_deref(),
        errors,
    );
    let mut result = result;
    result.next_steps = next_steps;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_install_result(&outcome);
        if failed {
            println!(
                "  setup outcome: failed - at least one previewed write was safely refused or did not complete"
            );
        }
        if !failed {
            print_post_install(outcome.journal_path.as_deref(), &plan.destination_root);
        }
    }
    if failed {
        return Err("retroarch-cheat-setup: installation did not complete safely".into());
    }
    Ok(())
}

fn parse_options(mut args: Vec<String>) -> Result<SetupCliOptions, Box<dyn std::error::Error>> {
    let json = extract_flag(&mut args, "--json");
    let dry_run = extract_flag(&mut args, "--dry-run");
    let yes = extract_flag(&mut args, "--yes");
    let replace_different = extract_flag(&mut args, "--replace-different");
    let profile_id = extract_value(&mut args, "--profile")?;
    let database_path = extract_value(&mut args, "--database")?
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(default_database_path)?;
    let configuration_path = extract_value(&mut args, "--config")?.map(PathBuf::from);
    let source_id = extract_value(&mut args, "--source")?;
    let force_refresh = extract_flag(&mut args, "--force-refresh");
    let offline = extract_flag(&mut args, "--offline");
    let expected_sha256 = extract_value(&mut args, "--expected-sha256")?;
    let cache_root = extract_value(&mut args, "--cache-root")?
        .map(PathBuf::from)
        .unwrap_or(archivefs_core::patch_manager::default_cheat_source_cache_root()?);
    let max_download_bytes = extract_value(&mut args, "--max-download-bytes")?
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| "--max-download-bytes requires a positive integer")?;
    if max_download_bytes == Some(0) {
        return Err("--max-download-bytes must be greater than zero".into());
    }
    if source_id.is_some() == (args.len() == 1) {
        return Err("retroarch-cheat-setup requires exactly one local catalogue path or --source <source-id>, never both".into());
    }
    if source_id.is_none()
        && (force_refresh || offline || expected_sha256.is_some() || max_download_bytes.is_some())
    {
        return Err("retrieval options require --source <source-id>".into());
    }
    if source_id.is_some() && !args.is_empty() {
        return Err("a local catalogue path cannot be combined with --source".into());
    }
    let catalogue_path = args.first().map(PathBuf::from).unwrap_or_default();
    let source_options = source_id.as_ref().map(|_| SourceOptions {
        json,
        force_refresh,
        offline,
        expected_sha256,
        cache_root,
        max_download_bytes,
    });
    Ok(SetupCliOptions {
        catalogue_path,
        source_id,
        source_result: None,
        source_options,
        profile_id,
        dry_run,
        yes,
        replace_different,
        json,
        database_path,
        configuration_path,
    })
}

fn extract_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let present = args.iter().any(|argument| argument == flag);
    args.retain(|argument| argument != flag);
    present
}

fn extract_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let positions = args
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value == flag).then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err(format!("{flag} may be specified only once").into());
    }
    let Some(position) = positions.first().copied() else {
        return Ok(None);
    };
    if position + 1 >= args.len() || args[position + 1].starts_with("--") {
        return Err(format!("{flag} requires a value").into());
    }
    let value = args.remove(position + 1);
    args.remove(position);
    Ok(Some(value))
}

fn fail(
    options: &SetupCliOptions,
    discovery: Option<&RetroArchCheatSetupDiscovery>,
    error: RetroArchCheatSetupError,
) -> Result<(), Box<dyn std::error::Error>> {
    if options.json {
        let result = RetroArchCheatSetupResult::failed(
            discovery,
            &options.catalogue_path,
            &options.database_path,
            error.clone(),
        );
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        eprintln!(
            "RetroArch profile\n  Setup cannot continue: {}",
            error.detail
        );
        if let Some(discovery) = discovery {
            print_discovered_profiles(discovery);
        }
        eprintln!(
            "\nNext steps\n  Check that RetroArch has been launched once, its config is readable, and cheat_database_path is an absolute safe path."
        );
    }
    Err(error.into())
}

fn print_discovered_profiles(discovery: &RetroArchCheatSetupDiscovery) {
    for profile in &discovery.profiles {
        eprintln!(
            "  {} [{} / {}] {}",
            profile.profile_id,
            profile_kind(profile.installation_type),
            profile_scope(profile.scope),
            if profile.eligible {
                "eligible"
            } else {
                "ineligible"
            }
        );
        eprintln!("    config: {}", profile.configuration_path.display);
        if let Some(destination) = &profile.cheat_destination_root {
            eprintln!("    cheats: {}", destination.display);
        }
        for blocker in &profile.blockers {
            eprintln!("    blocked: {} ({})", blocker.detail, blocker.code);
        }
    }
}

fn print_profile_choices(profiles: &[RetroArchCheatSetupProfile]) {
    println!(
        "RetroArch profile\n  More than one usable profile was found. Select one; no default is assumed.\n"
    );
    for (index, profile) in profiles.iter().enumerate() {
        println!(
            "  {}. {} ({})\n     ID: {}\n     config: {}\n     cheats: {}",
            index + 1,
            profile_kind(profile.installation_type),
            profile_scope(profile.scope),
            profile.profile_id,
            profile.configuration_path.display,
            profile
                .cheat_destination_root
                .as_ref()
                .map(|path| path.display.as_str())
                .unwrap_or("unresolved")
        );
    }
}

fn select_profile_from_reader<R: BufRead>(
    profiles: &[RetroArchCheatSetupProfile],
    reader: &mut R,
) -> io::Result<Option<RetroArchCheatSetupProfile>> {
    loop {
        print!("\nSelect profile number, or 'q' to cancel: ");
        io::stdout().flush()?;
        let mut input = String::new();
        if reader.read_line(&mut input)? == 0 {
            return Ok(None);
        }
        let input = input.trim();
        if input.eq_ignore_ascii_case("q") || input.eq_ignore_ascii_case("cancel") {
            return Ok(None);
        }
        if let Ok(number) = input.parse::<usize>()
            && let Some(profile) = number.checked_sub(1).and_then(|index| profiles.get(index))
        {
            return Ok(Some(profile.clone()));
        }
        println!("Please enter a listed number or 'q'.");
    }
}

fn confirm_from_reader<R: BufRead>(reader: &mut R) -> io::Result<bool> {
    print!("  Confirm [yes/no]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    if reader.read_line(&mut input)? == 0 {
        return Ok(false);
    }
    Ok(input.trim().eq_ignore_ascii_case("yes"))
}

fn print_preview(plan: &RetroArchCheatSetupPlan, options: &SetupCliOptions) {
    let profile = &plan.selected_profile;
    let summary = &plan.preview.summary;
    println!(
        "RetroArch profile\n  selected: {}\n  installation type: {} ({})\n  configuration: {}\n  cheat destination: {}\n",
        profile.profile_id,
        profile_kind(profile.installation_type),
        profile_scope(profile.scope),
        profile.configuration_path.display,
        plan.destination_root.display()
    );
    if let Some(source) = &options.source_result {
        println!(
            "Trusted source provenance\n  source: {} ({})\n  URL: {}\n  fetched: {}\n  archive SHA-256: {}\n  validation complete: {}\n  retrieval: {:?} (cache: {}, stale: {})\n  immutable snapshot: {}\n",
            source.source.display_name,
            source.source.source_id,
            source.source.download_url,
            source.manifest.fetched_at_unix_seconds,
            source.manifest.archive_sha256,
            source.manifest.validation_complete,
            source.status,
            source.from_cache,
            source.stale,
            source.immutable_snapshot_path.display,
        );
        for warning in &source.warnings {
            println!("  retrieval warning: {warning}");
        }
    }
    println!(
        "Cheat catalogue\n  path: {}\n  ArchiveFS database: {}\n  game records examined: {}\n  cheat records discovered: {}\n",
        options.catalogue_path.display(),
        options.database_path.display(),
        summary.archivefs_game_records_examined,
        summary.cheat_records_discovered
    );
    println!(
        "Match summary\n  exact/strong matches: {}/{}\n  weak/ambiguous matches (not installable): {}/{}\n  new installations: {}\n  already installed: {}\n  different existing files: {}\n  conflicts: {}\n  malformed or skipped: {}\n  writes proposed: {}\n  backups proposed: {}\n  journal: {}\n",
        summary.exact_matches,
        summary.strong_matches,
        summary.weak_matches,
        summary.ambiguous_matches,
        summary.eligible_new_installations,
        summary.already_installed,
        summary.different_existing_files,
        summary.conflicts,
        summary.malformed_or_skipped_entries,
        summary.total_writes_proposed,
        summary.total_backups_proposed,
        plan.preview.journal_path.display
    );
    println!("Planned changes");
    if plan.preview.planned_entries.is_empty() {
        println!("  No catalogue cheat records were found.");
    }
    let visible_entries = plan
        .preview
        .planned_entries
        .iter()
        .filter(|entry| {
            entry.planned_action
                != archivefs_core::patch_manager::RetroArchCheatSetupPlannedAction::Skipped
        })
        .collect::<Vec<_>>();
    for entry in &visible_entries {
        println!(
            "  {} [{}]\n    source: {}\n    destination: {}\n    action: {}\n    confidence/reason: {:?} / {}",
            entry.display_title,
            entry.platform.as_deref().unwrap_or("unknown platform"),
            entry.source_cheat_path.display,
            entry
                .destination_cheat_path
                .as_ref()
                .map(|path| path.display.as_str())
                .unwrap_or("not resolved"),
            planned_action_label(entry.planned_action),
            entry.match_confidence,
            entry.reason
        );
    }
    let omitted = plan
        .preview
        .planned_entries
        .len()
        .saturating_sub(visible_entries.len());
    if omitted > 0 {
        println!(
            "  {omitted} non-actionable catalogue entries omitted; summary counts still include them."
        );
    }
    if !plan.warnings.is_empty() {
        println!("\nWarnings");
        for warning in &plan.warnings {
            println!("  {} ({})", warning.detail, warning.code);
        }
    }
}

fn print_zero_write_explanation(plan: &RetroArchCheatSetupPlan) {
    println!(
        "\nPreview complete. No changes were needed or eligible, so confirmation was not requested."
    );
    print_zero_write_cause(plan);
}

fn print_zero_write_cause(plan: &RetroArchCheatSetupPlan) {
    let summary = &plan.preview.summary;
    if summary.cheat_records_discovered == 0 {
        println!("  Cause: the supported catalogue contained no cheat records.");
    } else if summary.exact_matches + summary.strong_matches == 0 {
        println!(
            "  Cause: no exact or strong catalogue-to-game matches were available; weak and ambiguous matches remain non-actionable."
        );
    } else if summary.already_installed > 0
        && summary.already_installed == summary.cheat_records_discovered
    {
        println!("  Cause: every matched cheat file is already installed with identical content.");
    } else if summary.replacement_blocked > 0 {
        println!(
            "  Cause: different existing files were protected because --replace-different was not supplied."
        );
    } else if summary.conflicts > 0 {
        println!("  Cause: destination conflicts or unsafe paths prevented installation.");
    } else {
        println!(
            "  Cause: entries were skipped because of matching, platform, parsing, or eligibility rules."
        );
    }
}

fn print_install_result(outcome: &CheatInstallRunOutcome) {
    println!(
        "\nInstallation result\n  status: {}\n  installed new: {}\n  replaced: {}\n  already installed: {}\n  skipped: {}\n  failed: {}",
        install_status_label(outcome.run.status),
        outcome.run.summary.installed_new,
        outcome.run.summary.replaced,
        outcome.run.summary.already_installed,
        outcome.run.summary.skipped,
        outcome.run.summary.failed
    );
    if let Some(path) = &outcome.journal_path {
        println!("  install journal: {}", path.display());
    }
}

fn print_post_install(journal_path: Option<&Path>, destination_root: &Path) {
    println!("\nWhat to do in RetroArch");
    println!("  1. Start the matching game in RetroArch.");
    println!("  2. Open Quick Menu, then Cheats.");
    println!("  3. Use Load Cheat File (or the equivalent loading action).");
    println!("  4. Select the matching cheat file if it was not loaded automatically.");
    println!("  5. Enable the individual cheat entries you want, then use Apply Changes.");
    println!(
        "  6. Optionally use RetroArch's supported auto-apply or game-specific save settings."
    );
    println!(
        "  Installing a file does not enable cheats automatically. Game identity, region, revision, emulator core, and cheat format can affect compatibility."
    );
    if let Some(journal_path) = journal_path {
        println!("\nUndo and history");
        println!("  journal: {}", journal_path.display());
        println!("  archivefs retroarch-cheat-history");
        println!(
            "  archivefs retroarch-cheat-inspect '{}'",
            journal_path.display()
        );
        println!(
            "  archivefs retroarch-cheat-rollback '{}' --cheat-destination-root '{}' --dry-run",
            journal_path.display(),
            destination_root.display()
        );
    }
}

fn result_from_plan(
    options: &SetupCliOptions,
    discovery: &RetroArchCheatSetupDiscovery,
    plan: &RetroArchCheatSetupPlan,
    status: RetroArchCheatSetupStatus,
    install: Option<&CheatInstallRunOutcome>,
    journal_path: Option<&Path>,
    errors: Vec<RetroArchCheatSetupError>,
) -> RetroArchCheatSetupResult {
    let next_steps = match status {
        RetroArchCheatSetupStatus::Preview if plan.preview.summary.total_writes_proposed == 0 => {
            vec![step(
                1,
                "no_change",
                "The preview found no eligible writes; no confirmation or installation is needed.",
                None,
            )]
        }
        RetroArchCheatSetupStatus::Preview => vec![step(
            1,
            "apply_preview",
            "Review the planned entries, then rerun with --yes to approve installation.",
            Some(preview_apply_command(options, plan)),
        )],
        RetroArchCheatSetupStatus::Cancelled => vec![step(
            1,
            "no_change",
            "The setup was cancelled and made no changes.",
            None,
        )],
        RetroArchCheatSetupStatus::Applied | RetroArchCheatSetupStatus::Failed => Vec::new(),
    };
    RetroArchCheatSetupResult {
        schema_version: RETROARCH_CHEAT_SETUP_SCHEMA_VERSION,
        status,
        selected_profile: Some(plan.selected_profile.clone()),
        discovered_profiles: discovery.profiles.clone(),
        configuration_path: Some(plan.selected_profile.configuration_path.clone()),
        cheat_destination_root: Some(EncodedPath::from_path(&plan.destination_root)),
        catalogue_path: EncodedPath::from_path(&options.catalogue_path),
        retrieved_source: options
            .source_result
            .as_ref()
            .map(archivefs_core::patch_manager::CheatSourceSetupContext::from),
        database_path: EncodedPath::from_path(&options.database_path),
        preview: Some(plan.preview.clone()),
        planned_entries: plan.preview.planned_entries.clone(),
        install_result: install.map(|outcome| outcome.run.clone()),
        journal_path: journal_path.map(archivefs_core::patch_manager::CheatInstallPath::from_path),
        warnings: plan.warnings.clone(),
        errors,
        next_steps,
    }
}

fn preview_apply_command(options: &SetupCliOptions, plan: &RetroArchCheatSetupPlan) -> Vec<String> {
    let mut command = vec!["archivefs".into(), "retroarch-cheat-setup".into()];
    if let Some(source_id) = &options.source_id {
        command.push("--source".into());
        command.push(source_id.clone());
        if let Some(source_options) = &options.source_options {
            if source_options.offline {
                command.push("--offline".into());
            }
            command.push("--cache-root".into());
            command.push(source_options.cache_root.display().to_string());
        }
    } else {
        command.push(options.catalogue_path.display().to_string());
    }
    command.extend([
        "--profile".into(),
        plan.selected_profile.profile_id.clone(),
        "--database".into(),
        options.database_path.display().to_string(),
    ]);
    if let Some(configuration_path) = &options.configuration_path {
        command.push("--config".into());
        command.push(configuration_path.display().to_string());
    }
    if options.replace_different {
        command.push("--replace-different".into());
    }
    command.push("--yes".into());
    command
}

fn next_steps(journal_path: &Path, destination_root: &Path) -> Vec<RetroArchCheatSetupNextStep> {
    vec![
        step(
            1,
            "start_game",
            "Start the matching game in RetroArch.",
            None,
        ),
        step(2, "open_quick_menu", "Open Quick Menu, then Cheats.", None),
        step(
            3,
            "load_cheat_file",
            "Use Load Cheat File and select the matching file if necessary.",
            None,
        ),
        step(
            4,
            "enable_entries",
            "Enable only the wanted cheat entries.",
            None,
        ),
        step(
            5,
            "apply_changes",
            "Use Apply Changes; installation alone does not enable cheats.",
            None,
        ),
        step(
            6,
            "view_history",
            "Review ArchiveFS cheat installation history.",
            Some(vec!["archivefs".into(), "retroarch-cheat-history".into()]),
        ),
        step(
            7,
            "inspect_journal",
            "Inspect the installed destinations before rollback.",
            Some(vec![
                "archivefs".into(),
                "retroarch-cheat-inspect".into(),
                journal_path.display().to_string(),
            ]),
        ),
        step(
            8,
            "preview_rollback",
            "Preview a safe rollback with the selected cheat root.",
            Some(vec![
                "archivefs".into(),
                "retroarch-cheat-rollback".into(),
                journal_path.display().to_string(),
                "--cheat-destination-root".into(),
                destination_root.display().to_string(),
                "--dry-run".into(),
            ]),
        ),
    ]
}

fn step(
    order: u32,
    action: &str,
    detail: &str,
    command: Option<Vec<String>>,
) -> RetroArchCheatSetupNextStep {
    RetroArchCheatSetupNextStep {
        order,
        action: action.to_string(),
        detail: detail.to_string(),
        command,
    }
}

fn profile_kind(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Native => "native",
        ProfileKind::AppImage => "AppImage/portable",
        ProfileKind::Flatpak => "Flatpak",
    }
}

fn profile_scope(scope: ProfileScope) -> &'static str {
    match scope {
        ProfileScope::User => "user",
        ProfileScope::System => "system",
    }
}

fn planned_action_label(
    action: archivefs_core::patch_manager::RetroArchCheatSetupPlannedAction,
) -> &'static str {
    use archivefs_core::patch_manager::RetroArchCheatSetupPlannedAction;
    match action {
        RetroArchCheatSetupPlannedAction::InstallNew => "install_new",
        RetroArchCheatSetupPlannedAction::AlreadyInstalled => "already_installed",
        RetroArchCheatSetupPlannedAction::ReplaceDifferent => "replace_different",
        RetroArchCheatSetupPlannedAction::Skipped => "skipped",
    }
}

fn install_status_label(status: CheatInstallRunStatus) -> &'static str {
    match status {
        CheatInstallRunStatus::Success => "success",
        CheatInstallRunStatus::PartialFailure => "partial failure",
        CheatInstallRunStatus::Failed => "failed",
        CheatInstallRunStatus::DryRun => "dry run",
    }
}

fn generate_setup_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("cheat-setup-{nanos:x}-{}", std::process::id())
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use archivefs_core::emulator_environment::retroarch::Diagnostic;
    use archivefs_core::patch_manager::RetroArchCheatSetupProfileState;
    use std::io::Cursor;

    fn profile(id: &str) -> RetroArchCheatSetupProfile {
        RetroArchCheatSetupProfile {
            profile_id: id.to_string(),
            installation_type: ProfileKind::Native,
            scope: ProfileScope::User,
            state: RetroArchCheatSetupProfileState::Eligible,
            eligible: true,
            executable_evidence: Vec::new(),
            configuration_path: EncodedPath {
                display: "/config/retroarch.cfg".to_string(),
                lossy: false,
            },
            cheat_destination_root: Some(EncodedPath {
                display: "/cheats".to_string(),
                lossy: false,
            }),
            blockers: Vec::new(),
            diagnostics: Vec::<Diagnostic>::new(),
        }
    }

    #[test]
    fn confirmation_accepts_only_explicit_yes() {
        assert!(confirm_from_reader(&mut Cursor::new(b"yes\n")).unwrap());
        assert!(!confirm_from_reader(&mut Cursor::new(b"y\n")).unwrap());
        assert!(!confirm_from_reader(&mut Cursor::new(b"\n")).unwrap());
    }

    #[test]
    fn numbered_profile_selection_does_not_default_to_first() {
        let profiles = vec![profile("one"), profile("two")];
        assert!(
            select_profile_from_reader(&profiles, &mut Cursor::new(b"q\n"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            select_profile_from_reader(&profiles, &mut Cursor::new(b"2\n"))
                .unwrap()
                .unwrap()
                .profile_id,
            "two"
        );
    }

    #[test]
    fn parser_supports_setup_options_without_destination_flags() {
        let options = parse_options(vec![
            "/catalogue".into(),
            "--dry-run".into(),
            "--profile".into(),
            "native-user-id".into(),
            "--database".into(),
            "/data/library.sqlite3".into(),
            "--config".into(),
            "/config/retroarch.cfg".into(),
        ])
        .unwrap();
        assert!(options.dry_run);
        assert_eq!(options.profile_id.as_deref(), Some("native-user-id"));
        assert_eq!(
            options.database_path,
            PathBuf::from("/data/library.sqlite3")
        );
    }

    #[test]
    fn parser_keeps_local_and_trusted_source_forms_unambiguous() {
        let source = parse_options(vec![
            "--source".into(),
            "libretro-buildbot-cheats".into(),
            "--offline".into(),
            "--cache-root".into(),
            "/cache".into(),
            "--dry-run".into(),
        ])
        .unwrap();
        assert_eq!(
            source.source_id.as_deref(),
            Some("libretro-buildbot-cheats")
        );
        assert!(source.source_options.unwrap().offline);
        assert!(parse_options(vec!["/catalogue".into(), "--source".into(), "id".into()]).is_err());
        assert!(parse_options(vec!["/catalogue".into(), "--offline".into()]).is_err());
        assert!(parse_options(vec![]).is_err());
    }
}
