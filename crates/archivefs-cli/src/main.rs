use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use archivefs_core::patch_manager::{
    AdvisoryPatchPlan, HttpsMetadataFetcher, ReadOnlyPcsx2Adapter,
};
use archivefs_core::{
    ArchiveFsError, ArchiveIndex, ArchiveIndexEntry, ArchiveIndexFreshness, ArchiveIndexSummary,
    ArchiveInfo, ArchiveScanner, ArchiveStats, ArchiveStatus, BulkPlatformAssignmentResult,
    CatalogueHealthReport, CatalogueStats, CompletedScanSummary, Config, ConfigCheckReport,
    ConfigCheckStatus, Database, DatabaseHealth, DoctorReport, DuplicateDetector, DuplicateEntry,
    DuplicateReport, FilenameDuplicateDetector, LibraryViewApplyOutcome, LibraryViewApplyReport,
    LibraryViewConfig, LibraryViewPlan, LibraryViewPlanAction, LibraryViewPlanEntry,
    MissingArchiveRemovalResult, MountPlan, PersistedArchive, PlatformAlias,
    PlatformAssignmentChange, ScanPersistSummary, SourceAvailability, SourceFolderView,
    WatchRebuildSummary, add_source_folder_default, apply_library_view_default,
    build_and_write_archive_index, canonical_platform_names, catalogue_health_report,
    check_archive_index_freshness, check_database_health, clean_mount_root,
    cleanup_selected_mount_dir, current_archive_info, current_archive_stats, current_statuses,
    default_database_path, default_index_path, find_archive_index_entries, latest_schema_version,
    list_source_folder_views_default, load_library_view_configs_default,
    load_source_folder_configs_default, mount_archives, mount_one_archive,
    persisted_archive_has_unknown_platform, preview_library_view_default,
    read_default_archive_index, remove_library_view_default, remove_source_folder_default,
    repair_library_view_default, resolve_source_folder_identifier, run_config_check_default,
    run_doctor_default, scan_all_enabled_sources_default, scan_and_persist,
    scan_source_folder_default, set_source_folder_enabled_default, summarize_archive_index,
    unmount_archives, unmount_one_archive, watch_archive_index,
};
use serde::Serialize;

static LOGGER: StderrLogger = StderrLogger;
static LOGGER_INIT: OnceLock<()> = OnceLock::new();

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliArgs {
    log_level: log::LevelFilter,
    command: String,
    args: Vec<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("archivefs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(level: log::LevelFilter) {
    LOGGER_INIT.get_or_init(|| {
        let _ = log::set_logger(&LOGGER);
    });
    log::set_max_level(level);
}

fn parse_cli_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut log_level = log::LevelFilter::Off;
    let mut rest = args.into_iter().collect::<Vec<_>>();

    while let Some(flag) = rest.first() {
        match flag.as_str() {
            "--debug" => {
                log_level = log::LevelFilter::Debug;
                rest.remove(0);
            }
            "--verbose" | "-v" => {
                if log_level < log::LevelFilter::Info {
                    log_level = log::LevelFilter::Info;
                }
                rest.remove(0);
            }
            _ => break,
        }
    }

    let command = if rest.is_empty() {
        "help".to_string()
    } else {
        rest.remove(0)
    };

    CliArgs {
        log_level,
        command,
        args: rest,
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli_args(env::args().skip(1));
    init_logging(cli.log_level);
    let command = cli.command;
    let mut args = cli.args.into_iter();

    match command.as_str() {
        "scan" => {
            let config = Config::load_default()?;
            let scanner = ArchiveScanner::new(&config);
            for archive in scanner.scan_archives()? {
                println!("{}", archive.path.display());
            }
        }
        "mount" => {
            let config = Config::load_default()?;
            print_statuses(&mount_archives(&config)?);
        }
        "mount-one" => {
            let Some(first) = args.next() else {
                return Err("mount-one requires an archive path or name".into());
            };
            let input = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let config = Config::load_default()?;
            print_mount_one(&mount_one_archive(&config, &input)?);
            warn_if_index_refresh_failed(&config);
        }
        "unmount" => {
            let config = Config::load_default()?;
            print_statuses(&unmount_archives(&config)?);
        }
        "unmount-one" => {
            let Some(first) = args.next() else {
                return Err("unmount-one requires an archive path or name".into());
            };
            let input = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let config = Config::load_default()?;
            let plan = unmount_one_archive(&config, &input)?;
            print_unmount_one(&plan);
            warn_if_mount_dir_cleanup_failed(&config, &plan);
            warn_if_index_refresh_failed(&config);
        }
        "status" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let statuses = current_statuses(&config)?;
            if json {
                print_statuses_json(&statuses)?;
            } else {
                print_statuses(&statuses);
            }
        }
        "stats" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let stats = current_archive_stats(&config)?;
            if json {
                print_archive_stats_json(&stats)?;
            } else {
                print_archive_stats(&stats);
            }
        }
        "duplicates" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let report = build_duplicate_report(&config)?;
            if json {
                print_duplicate_report_json(&report)?;
            } else {
                print_duplicate_report(&report);
            }
        }
        "info" => {
            let Some(first) = args.next() else {
                return Err("info requires an archive path or name".into());
            };
            let mut input_args = std::iter::once(first).chain(args).collect::<Vec<_>>();
            let json = input_args.last().is_some_and(|arg| arg == "--json");
            if json {
                input_args.pop();
            }
            let input = input_args.join(" ");
            if input.is_empty() {
                return Err("info requires an archive path or name".into());
            }
            let config = Config::load_default()?;
            let info = current_archive_info(&config, &input)?;
            if json {
                print_archive_info_json(&info)?;
            } else {
                print_archive_info(&info);
            }
        }
        "doctor" => {
            let json = args.any(|arg| arg == "--json");
            let report = run_doctor_default();
            if json {
                print_doctor_report_json(&report)?;
            } else {
                print_doctor_report(&report);
            }
        }
        "config-check" => {
            print_config_check_report(&run_config_check_default());
        }
        "pcsx2-patch-preview" => {
            let mut input_args = args.collect::<Vec<_>>();
            let json = extract_flag(&mut input_args, "--json");
            if !input_args.is_empty() {
                return Err("pcsx2-patch-preview accepts only --json".into());
            }
            let fetcher = HttpsMetadataFetcher::new();
            let adapter = ReadOnlyPcsx2Adapter::from_environment()?;
            let plan = archivefs_core::patch_manager::preview_pcsx2_metadata(
                &fetcher,
                &adapter,
                &default_database_path()?,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print!("{}", format_advisory_patch_plan(&plan));
            }
        }
        "index-build" => {
            let config = Config::load_default()?;
            let index = build_and_write_archive_index(&config)?;
            println!(
                "Wrote index: {} ({} archives)",
                default_index_path()?.display(),
                index.archives.len()
            );
        }
        "index-show" => {
            let Some(index) = read_index_or_print_build_hint()? else {
                return Ok(());
            };
            print_index_warnings(&check_archive_index_freshness(&index));
            print_index_summary(&summarize_archive_index(&index));
        }
        "index-find" => {
            let Some(first) = args.next() else {
                return Err("index-find requires a query".into());
            };
            let query = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let Some(index) = read_index_or_print_build_hint()? else {
                return Ok(());
            };
            print_index_warnings(&check_archive_index_freshness(&index));
            print_index_find_results(&query, &find_archive_index_entries(&index, &query));
        }
        "library-status" => {
            let json = args.any(|arg| arg == "--json");
            let view = build_library_status_view(&default_database_path()?);
            if json {
                print_library_status_json(&view)?;
            } else {
                print_library_status(&view);
            }
        }
        "health" => {
            let json = args.any(|arg| arg == "--json");
            let database_path = default_database_path()?;
            let database = Database::open_or_create(&database_path)?;
            let archives = database.load_archives()?;
            let report = catalogue_health_report(&archives);
            if json {
                print_health_report_json(&report)?;
            } else {
                print_health_report(&report);
            }
        }
        "library-scan" => {
            let json = args.any(|arg| arg == "--json");
            let config = Config::load_default()?;
            let database_path = default_database_path()?;
            let report = run_library_scan(&config, &database_path, "cli-library-scan")?;
            if json {
                print_library_scan_json(&report)?;
            } else {
                print_library_scan(&report);
            }
        }
        "library-list" => {
            let input_args: Vec<String> = args.collect();
            let json = input_args.iter().any(|arg| arg == "--json");
            let unknown_only = input_args.iter().any(|arg| arg == "--unknown-only");
            let database_path = default_database_path()?;
            let entries = build_library_entries(&database_path, unknown_only)?;
            if json {
                print_library_entries_json(&entries)?;
            } else {
                print_library_entries(&database_path, &entries);
            }
        }
        "library-find" => {
            let Some(first) = args.next() else {
                return Err("library-find requires a query".into());
            };
            let mut input_args = std::iter::once(first).chain(args).collect::<Vec<_>>();
            let unknown_only = extract_flag(&mut input_args, "--unknown-only");
            let json = input_args.last().is_some_and(|arg| arg == "--json");
            if json {
                input_args.pop();
            }
            let query = input_args.join(" ");
            if query.is_empty() {
                return Err("library-find requires a query".into());
            }
            let database_path = default_database_path()?;
            let entries = build_library_entries(&database_path, unknown_only)?;
            let matches = filter_library_entries(&entries, &query);
            if json {
                print_library_entries_json(&matches)?;
            } else {
                print_library_find_results(&query, &matches);
            }
        }
        "library-set-platform" => {
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");
            let custom = extract_flag(&mut input_args, "--custom");
            let id = extract_id_flag(&mut input_args)?;
            let path = extract_path_flag(&mut input_args)?;
            let Some(platform) = input_args.pop() else {
                return Err(
                    "library-set-platform requires a platform, e.g. archivefs-cli library-set-platform \"007 Legends\" Xbox360 (or --id <id> Xbox360, or --path <path> Xbox360)"
                        .into(),
                );
            };
            let selector = resolve_target_selector("library-set-platform", id, path, input_args)?;
            let platform = resolve_platform_argument(platform, custom)?;
            let database_path = default_database_path()?;
            let change = run_library_set_platform(&database_path, &selector, &platform)?;
            if json {
                print_library_platform_change_json(&change)?;
            } else {
                print_library_platform_change("Set Platform", &change);
            }
        }
        "library-clear-platform" => {
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");
            let id = extract_id_flag(&mut input_args)?;
            let path = extract_path_flag(&mut input_args)?;
            let selector = resolve_target_selector("library-clear-platform", id, path, input_args)?;
            let database_path = default_database_path()?;
            let change = run_library_clear_platform(&database_path, &selector)?;
            if json {
                print_library_platform_change_json(&change)?;
            } else {
                print_library_platform_change("Clear Platform", &change);
            }
        }
        "library-set-platform-bulk" => {
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");
            let custom = extract_flag(&mut input_args, "--custom");
            let ids = extract_repeated_id_flags(&mut input_args)?;
            let paths = extract_repeated_path_flags(&mut input_args)?;
            let Some(platform) = input_args.pop() else {
                return Err(
                    "library-set-platform-bulk requires a platform, e.g. archivefs-cli library-set-platform-bulk --id 1 --id 2 GameCube"
                        .into(),
                );
            };
            if !input_args.is_empty() {
                return Err(
                    "library-set-platform-bulk does not accept a free-text query - use --id/--path"
                        .into(),
                );
            }
            require_at_least_one_bulk_selector("library-set-platform-bulk", &ids, &paths)?;
            let platform = resolve_platform_argument(platform, custom)?;
            let database_path = default_database_path()?;
            let summary = run_library_set_platform_bulk(&database_path, &ids, &paths, &platform)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print_bulk_platform_change("Set Platform (bulk)", &summary);
            }
        }
        "library-clear-platform-bulk" => {
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");
            let ids = extract_repeated_id_flags(&mut input_args)?;
            let paths = extract_repeated_path_flags(&mut input_args)?;
            if !input_args.is_empty() {
                return Err(
                    "library-clear-platform-bulk does not accept a free-text query - use --id/--path"
                        .into(),
                );
            }
            require_at_least_one_bulk_selector("library-clear-platform-bulk", &ids, &paths)?;
            let database_path = default_database_path()?;
            let summary = run_library_clear_platform_bulk(&database_path, &ids, &paths)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print_bulk_platform_change("Clear Platform (bulk)", &summary);
            }
        }
        "library-remove-missing" => {
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");
            let ids = extract_repeated_id_flags(&mut input_args)?;
            let paths = extract_repeated_path_flags(&mut input_args)?;
            if !input_args.is_empty() {
                return Err(
                    "library-remove-missing accepts only exact --id/--path selectors".into(),
                );
            }
            require_at_least_one_bulk_selector("library-remove-missing", &ids, &paths)?;
            let database_path = default_database_path()?;
            let result = run_library_remove_missing(&database_path, &ids, &paths)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print!("{}", format_missing_removal(&result));
            }
        }
        "platform-alias-list" => {
            let json = args.any(|arg| arg == "--json");
            let database_path = default_database_path()?;
            let aliases = list_platform_aliases_or_empty(&database_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&aliases)?);
            } else {
                print_platform_aliases(&aliases);
            }
        }
        "platform-alias-add" => {
            let (json, alias, platform) = parse_platform_alias_add_args(args.collect())?;
            let database_path = default_database_path()?;
            let mut database = Database::open_or_create(&database_path)?;
            let saved = database.add_platform_alias(&alias, &platform)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&saved)?);
            } else {
                print_platform_alias_saved(&saved);
            }
        }
        "platform-alias-remove" => {
            let Some(alias) = args.next() else {
                return Err(
                    "platform-alias-remove requires an alias, e.g. archivefs-cli platform-alias-remove gc"
                        .into(),
                );
            };
            let database_path = default_database_path()?;
            let mut database = Database::open_or_create(&database_path)?;
            if database.remove_platform_alias(&alias)? {
                println!(
                    "Removed platform alias '{alias}'. Run a library scan to apply this change."
                );
            } else {
                return Err(format!("no platform alias matches '{alias}'").into());
            }
        }
        "clean" => {
            let config = Config::load_default()?;
            print_cleaned_dirs(&clean_mount_root(&config)?);
        }
        "watch" => {
            let config = Config::load_default()?;
            watch_archive_index(
                &config,
                || println!("Watching configured source folders for archive changes."),
                print_watch_rebuild,
            )?;
        }
        "sources" => {
            let mut input_args: Vec<String> = args.collect();
            if input_args.first().is_some_and(|arg| arg == "scan-all") {
                input_args.remove(0);
                let json = extract_flag(&mut input_args, "--json");
                let summary =
                    scan_all_enabled_sources_default().map_err(|error| error.to_string())?;
                let report = LibraryScanReport::from(&summary);
                if json {
                    print_library_scan_json(&report)?;
                } else {
                    print_library_scan(&report);
                }
            } else {
                let json = extract_flag(&mut input_args, "--json");
                let views =
                    list_source_folder_views_default().map_err(|error| error.to_string())?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&views)?);
                } else {
                    print_source_folder_views(&views);
                }
            }
        }
        "source" => {
            let Some(sub_command) = args.next() else {
                return Err(
                    "source requires a sub-command: add <path> | enable <id-or-path> | \
                     disable <id-or-path> | scan <id-or-path> | remove <id-or-path> \
                     (--keep-catalogue | --remove-catalogue)"
                        .into(),
                );
            };
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");

            match sub_command.as_str() {
                "add" => {
                    let Some(path) = input_args.first().cloned() else {
                        return Err(
                            "source add requires a path, e.g. archivefs-cli source add /mnt/roms"
                                .into(),
                        );
                    };
                    let added = add_source_folder_default(Path::new(&path))
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&added)?);
                    } else {
                        println!("Added source folder: {}", added.path.display());
                        println!(
                            "Run 'archivefs-cli source scan {}' to scan it.",
                            added.path.display()
                        );
                    }
                }
                "enable" | "disable" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err(format!("source {sub_command} requires an id or path").into());
                    };
                    let target = resolve_source_identifier(&identifier)?;
                    let enabled = sub_command == "enable";
                    let outcome = set_source_folder_enabled_default(&target, enabled)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&outcome.source)?);
                    } else if enabled {
                        println!("Enabled source folder: {}", outcome.source.path.display());
                        if let Some(scan) = &outcome.scan {
                            print_library_scan(&LibraryScanReport::from(scan));
                        }
                    } else {
                        println!("Disabled source folder: {}", outcome.source.path.display());
                        println!(
                            "Its catalogue entries are preserved and excluded from future scans."
                        );
                    }
                }
                "scan" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("source scan requires an id or path".into());
                    };
                    let target = resolve_source_identifier(&identifier)?;
                    let summary =
                        scan_source_folder_default(&target).map_err(|error| error.to_string())?;
                    let report = LibraryScanReport::from(&summary);
                    if json {
                        print_library_scan_json(&report)?;
                    } else {
                        print_library_scan(&report);
                    }
                }
                "remove" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("source remove requires an id or path".into());
                    };
                    let keep_catalogue = extract_flag(&mut input_args, "--keep-catalogue");
                    let remove_catalogue = extract_flag(&mut input_args, "--remove-catalogue");
                    let keep = resolve_keep_catalogue_flag(keep_catalogue, remove_catalogue)?;
                    let target = resolve_source_identifier(&identifier)?;
                    let outcome = remove_source_folder_default(&target, keep)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&RemoveSourceFolderReport {
                                removed_path: outcome.removed_source.path.clone(),
                                catalogue_rows_removed: outcome.catalogue_rows_removed,
                            })?
                        );
                    } else {
                        println!(
                            "Removed source folder from configuration: {}",
                            outcome.removed_source.path.display()
                        );
                        println!("ArchiveFS did not delete the folder or any files inside it.");
                        match outcome.catalogue_rows_removed {
                            Some(count) => println!(
                                "Removed {count} catalogue entr{} for this source.",
                                if count == 1 { "y" } else { "ies" }
                            ),
                            None => println!("Catalogue entries for this source were kept."),
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "unknown 'source' sub-command '{other}' (expected add, enable, disable, scan, or remove)"
                    )
                    .into());
                }
            }
        }
        "view" => {
            let Some(sub_command) = args.next() else {
                return Err(
                    "view requires a sub-command: list | preview <name> | apply <name> | \
                     repair <name> | remove <name> [--keep-definition]"
                        .into(),
                );
            };
            let mut input_args: Vec<String> = args.collect();
            let json = extract_flag(&mut input_args, "--json");

            match sub_command.as_str() {
                "list" => {
                    let views =
                        load_library_view_configs_default().map_err(|error| error.to_string())?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&views)?);
                    } else {
                        print_library_views(&views);
                    }
                }
                "preview" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("view preview requires a view id or name".into());
                    };
                    let (view, plan) = preview_library_view_default(&identifier)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&LibraryViewPreviewJson {
                                view: &view,
                                plan: &plan
                            })?
                        );
                    } else {
                        print_library_view_plan(&view, &plan);
                    }
                }
                "apply" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("view apply requires a view id or name".into());
                    };
                    let (view, report) = apply_library_view_default(&identifier)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&LibraryViewApplyJson {
                                view: &view,
                                report: &report
                            })?
                        );
                    } else {
                        print_library_view_apply_report(&view, &report, "Apply");
                    }
                }
                "repair" => {
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("view repair requires a view id or name".into());
                    };
                    let (view, report) = repair_library_view_default(&identifier)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&LibraryViewApplyJson {
                                view: &view,
                                report: &report
                            })?
                        );
                    } else {
                        print_library_view_apply_report(&view, &report, "Repair");
                    }
                }
                "remove" => {
                    // Extracted before reading the identifier positionally,
                    // unlike `source remove`'s flags - so `--keep-definition`
                    // is recognised no matter where it appears on the
                    // command line, not only after the identifier.
                    let keep_definition = extract_flag(&mut input_args, "--keep-definition");
                    let Some(identifier) = input_args.first().cloned() else {
                        return Err("view remove requires a view id or name".into());
                    };
                    let (view, report) = remove_library_view_default(&identifier, keep_definition)
                        .map_err(|error| error.to_string())?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&LibraryViewApplyJson {
                                view: &view,
                                report: &report
                            })?
                        );
                    } else {
                        print_library_view_remove_report(&view, &report, keep_definition);
                    }
                }
                other => {
                    return Err(format!(
                        "unknown 'view' sub-command '{other}' (expected list, preview, apply, repair, or remove)"
                    )
                    .into());
                }
            }
        }
        // `--version`/`-V` are recognised only in the command position -
        // exactly the same scope `--help`/`-h` already have (neither is
        // special-cased inside any individual subcommand's own argument
        // parsing, here or anywhere else in this file). Any trailing
        // `cli.args` (e.g. `archivefs-cli --version library-list`) are
        // ignored rather than rejected: `print_version` never reads
        // `args`, so this deliberately mirrors how `--help`/`-h` already
        // silently ignore trailing arguments today - "version wins and
        // exits" is the simplest behaviour consistent with this existing
        // hand-written parser, not an oversight.
        "--version" | "-V" => print_version(),
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
}

fn print_config_check_report(report: &ConfigCheckReport) {
    println!("ArchiveFS Config Check");
    println!("Config: {}", report.config_path.display());
    println!();
    println!("Checks:");
    for check in &report.checks {
        println!(
            "  [{:<5}] {:<28} {}",
            check.status, check.name, check.detail
        );
    }

    let warnings = report
        .checks
        .iter()
        .filter(|check| check.status == ConfigCheckStatus::Warn)
        .collect::<Vec<_>>();
    println!();
    println!("Warnings:");
    if warnings.is_empty() {
        println!("  none");
    } else {
        for warning in warnings {
            println!("  {}: {}", warning.name, warning.detail);
        }
    }

    let errors = report
        .checks
        .iter()
        .filter(|check| check.status == ConfigCheckStatus::Error)
        .collect::<Vec<_>>();
    println!();
    println!("Errors:");
    if errors.is_empty() {
        println!("  none");
    } else {
        for error in errors {
            println!("  {}: {}", error.name, error.detail);
        }
    }

    println!();
    println!("Summary:");
    println!("  Errors: {}", report.error_count());
    println!("  Warnings: {}", report.warning_count());
    println!(
        "  Status: {}",
        if report.is_ok() {
            "OK"
        } else {
            "Needs attention"
        }
    );
}

fn format_advisory_patch_plan(plan: &AdvisoryPatchPlan) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    writeln!(&mut output, "ArchiveFS PCSX2 Patch Metadata Preview").unwrap();
    writeln!(
        &mut output,
        "Advisory only: yes (executable: {})",
        plan.executable
    )
    .unwrap();
    writeln!(&mut output, "Plan format: {}", plan.format_version).unwrap();
    writeln!(&mut output, "Plan ID: {}", plan.plan_id).unwrap();
    writeln!(&mut output, "Source: {}", plan.source.display_name).unwrap();
    writeln!(&mut output, "Endpoint: {}", plan.source.endpoint).unwrap();
    writeln!(&mut output, "Provenance: {}", plan.source.provenance).unwrap();
    writeln!(&mut output, "License: {}", plan.source.license_notice).unwrap();
    writeln!(
        &mut output,
        "Metadata schema: {}",
        plan.source.metadata_schema
    )
    .unwrap();
    writeln!(
        &mut output,
        "Source version: {}",
        plan.source.source_version
    )
    .unwrap();
    writeln!(
        &mut output,
        "Metadata SHA-256: {}",
        plan.source.metadata_sha256
    )
    .unwrap();
    writeln!(&mut output, "Verification: {:?}", plan.source.verification).unwrap();
    writeln!(
        &mut output,
        "Verification detail: {}",
        plan.source.verification_explanation
    )
    .unwrap();
    writeln!(
        &mut output,
        "Freshness: {}",
        plan.source.freshness_explanation
    )
    .unwrap();
    writeln!(&mut output).unwrap();
    writeln!(
        &mut output,
        "PCSX2 candidates: {}",
        plan.installation_candidates.len()
    )
    .unwrap();
    for installation in &plan.installation_candidates {
        writeln!(
            &mut output,
            "  {}: {} ({}, confidence: {:?}, version: {}, mutation readiness: {})",
            installation.kind,
            installation.data_root.display(),
            installation.provenance,
            installation.discovery_confidence,
            installation
                .detected_version
                .as_deref()
                .unwrap_or("not inspected"),
            installation.mutation_readiness
        )
        .unwrap();
    }
    writeln!(&mut output).unwrap();
    writeln!(
        &mut output,
        "Records: {} | exact: {} | probable: {} | uncertain: {} | ambiguous: {} | no match: {}",
        plan.summary.metadata_records,
        plan.summary.exact_matches,
        plan.summary.probable_matches,
        plan.summary.uncertain_matches,
        plan.summary.ambiguous_matches,
        plan.summary.missing_games
    )
    .unwrap();
    for entry in &plan.entries {
        writeln!(&mut output).unwrap();
        writeln!(&mut output, "{}", entry.record.record_id).unwrap();
        writeln!(&mut output, "  disposition: {:?}", entry.disposition).unwrap();
        writeln!(
            &mut output,
            "  match confidence: {:?}",
            entry.game_match.confidence
        )
        .unwrap();
        writeln!(
            &mut output,
            "  catalogue archive IDs: {:?}",
            entry.game_match.catalogue_archive_ids
        )
        .unwrap();
        writeln!(
            &mut output,
            "  identity: serial={} executable_crc={}",
            entry.record.serial.as_deref().unwrap_or("none"),
            entry.record.executable_crc.as_deref().unwrap_or("none")
        )
        .unwrap();
        for reason in &entry.reasons {
            writeln!(&mut output, "  reason: {reason}").unwrap();
        }
        if entry.hypothetical_destinations.is_empty() {
            writeln!(
                &mut output,
                "  hypothetical PNACH destination: unavailable (no PCSX2 candidate)"
            )
            .unwrap();
        } else {
            for destination in &entry.hypothetical_destinations {
                writeln!(
                    &mut output,
                    "  hypothetical PNACH destination ({}, not created): {}",
                    destination.candidate_kind, destination.display_path
                )
                .unwrap();
            }
        }
    }
    output
}

fn print_watch_rebuild(index: &ArchiveIndex, summary: &WatchRebuildSummary) {
    let event_word = if summary.archive_event_count == 1 {
        "event"
    } else {
        "events"
    };
    println!(
        "Rebuilt index ({} archives) after {} archive {}:",
        index.archives.len(),
        summary.archive_event_count,
        event_word
    );
    for path in &summary.changed_paths {
        println!("  {}", path.display());
    }
}

fn warn_if_mount_dir_cleanup_failed(config: &Config, plan: &MountPlan) {
    if let Err(error) = cleanup_selected_mount_dir(config, &plan.mount_path) {
        eprintln!(
            "Warning: unmounted {}, but mount directory cleanup failed: {error}",
            plan.mount_path.display()
        );
    }
}

fn warn_if_index_refresh_failed(config: &Config) {
    if let Err(error) = build_and_write_archive_index(config) {
        eprintln!("Warning: mounted state changed, but index refresh failed: {error}");
    }
}

fn read_index_or_print_build_hint() -> Result<Option<ArchiveIndex>, Box<dyn std::error::Error>> {
    let index_path = default_index_path()?;
    if !Path::new(&index_path).exists() {
        println!(
            "No archive index found at {}. Run: archivefs index-build",
            index_path.display()
        );
        return Ok(None);
    }
    Ok(Some(read_default_archive_index()?))
}

fn print_index_warnings(freshness: &ArchiveIndexFreshness) {
    if !freshness.missing_archive_paths.is_empty() {
        println!("Warning: index contains missing archive paths. Run archivefs index-build.");
    }
    if !freshness.stale_archive_paths.is_empty() {
        println!("Warning: index may be stale. Run archivefs index-build.");
    }
}

// ---------------------------------------------------------------------
// Library database commands (library-status, library-scan, library-list,
// library-find). These read/write the persistent SQLite catalogue
// (archivefs_core::Database) - a separate store from the JSON index above.
// They never touch mount or unmount behavior, and index-build/index-show/
// index-find are unchanged and unaffected by any of this.
// ---------------------------------------------------------------------

/// Combined status view for `library-status`. Built from
/// [`check_database_health`] plus, only when the schema is already
/// current, [`Database::catalogue_stats`] and
/// [`Database::latest_completed_scan`] - a status check never triggers a
/// migration itself.
#[derive(Debug, Clone, Serialize)]
struct LibraryStatusView {
    #[serde(flatten)]
    health: DatabaseHealth,
    latest_known_schema_version: i64,
    stats: Option<CatalogueStats>,
    last_completed_scan: Option<CompletedScanSummary>,
}

fn build_library_status_view(database_path: &Path) -> LibraryStatusView {
    let health = check_database_health(database_path);
    let (stats, last_completed_scan) = if health.database_opens && health.migrations_current {
        match Database::open_or_create(database_path) {
            Ok(database) => (
                database.catalogue_stats().ok(),
                database.latest_completed_scan().ok().flatten(),
            ),
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    LibraryStatusView {
        health,
        latest_known_schema_version: latest_schema_version(),
        stats,
        last_completed_scan,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_library_status(view: &LibraryStatusView) {
    print!("{}", format_library_status(view));
}

fn print_library_status_json(view: &LibraryStatusView) -> Result<(), serde_json::Error> {
    println!("{}", format_library_status_json(view)?);
    Ok(())
}

fn format_library_status_json(view: &LibraryStatusView) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(view)
}

fn format_library_status(view: &LibraryStatusView) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Library Status\n\n");
    output.push_str(&format!(
        "Database: {}\n",
        view.health.resolved_path.display()
    ));
    output.push_str(&format!(
        "  Exists: {}\n",
        yes_no(view.health.database_exists)
    ));

    if !view.health.database_exists {
        output.push_str("\nNo library database yet. Run: archivefs-cli library-scan\n");
        return output;
    }

    output.push_str(&format!(
        "  Opens: {}\n",
        yes_no(view.health.database_opens)
    ));
    if let Some(error) = &view.health.error {
        output.push_str(&format!("  Error: {error}\n"));
    }

    if !view.health.database_opens {
        output.push_str(
            "\nThe database file exists but could not be opened. It is always safe to \
             delete it and run archivefs-cli library-scan to rebuild it from your \
             configured source folders.\n",
        );
        return output;
    }

    output.push_str(&format!(
        "  Schema version: {}\n",
        view.health
            .schema_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!(
        "  Migrations current: {}\n",
        yes_no(view.health.migrations_current)
    ));
    output.push_str(&format!(
        "  Foreign keys enabled: {}\n",
        yes_no(view.health.foreign_keys_enabled)
    ));

    if !view.health.migrations_current {
        if let Some(schema_version) = view.health.schema_version {
            if schema_version > view.latest_known_schema_version {
                output.push_str(&format!(
                    "\nThis database's schema (version {schema_version}) is newer than this \
                     build of ArchiveFS supports (version {}). Upgrade ArchiveFS, or remove \
                     the database file to rebuild it with this version.\n",
                    view.latest_known_schema_version
                ));
            } else {
                output.push_str(
                    "\nThis database's schema is outdated. Run: archivefs-cli library-scan \
                     to upgrade it.\n",
                );
            }
        }
        return output;
    }

    output.push_str("\nArchive counts:\n");
    match &view.stats {
        Some(stats) => {
            output.push_str(&format!("  Total: {}\n", stats.total_archives));
            output.push_str(&format!("  Present: {}\n", stats.present_archives));
            output.push_str(&format!("  Missing: {}\n", stats.missing_archives));
            output.push_str(&format!(
                "  Detected platform: {}\n",
                stats.archives_with_platform
            ));
            output.push_str(&format!(
                "  Unknown platform: {}\n",
                stats.archives_unknown_platform
            ));
        }
        None => output.push_str("  unavailable\n"),
    }

    output.push_str("\nLast completed scan:\n");
    match &view.last_completed_scan {
        Some(scan) => {
            output.push_str(&format!("  Started: {}\n", scan.started_at));
            output.push_str(&format!(
                "  Finished: {}\n",
                scan.finished_at.as_deref().unwrap_or("unknown")
            ));
            output.push_str(&format!("  Triggered by: {}\n", scan.triggered_by));
            output.push_str(&format!(
                "  Source folders scanned: {}\n",
                scan.source_folders_scanned
            ));
            output.push_str(&format!("  Archives seen: {}\n", scan.archives_seen));
            output.push_str(&format!("  Archives added: {}\n", scan.archives_added));
            output.push_str(&format!("  Archives updated: {}\n", scan.archives_updated));
            output.push_str(&format!("  Archives missing: {}\n", scan.archives_missing));
            output.push_str(&format!("  Errors: {}\n", scan.errors_count));
            if let Some(message) = &scan.error_message {
                output.push_str(&format!("  Error details: {message}\n"));
            }
        }
        None => output.push_str("  none yet - run: archivefs-cli library-scan\n"),
    }

    output
}

/// `health` prints [`CatalogueHealthReport`] - a new, stable JSON shape
/// (never a field on any existing serialized struct), so no existing
/// field is renamed or removed. Read-only: `catalogue_health_report`
/// classifies already-loaded catalogue rows only - this command never
/// scans, mounts, unmounts, or writes to the database. Its report is
/// necessarily a catalogue-only subset of the GUI Health Dashboard's
/// report - it can never report `AwaitingValidation`/`CachedOnly`,
/// retryable/terminal mount failures, or recovery-offer availability,
/// since all of those require a live GUI session this command
/// deliberately never has (see `ArchivePresence`'s doc comment in
/// archivefs-core).
fn print_health_report(report: &CatalogueHealthReport) {
    print!("{}", format_health_report(report));
}

fn print_health_report_json(report: &CatalogueHealthReport) -> Result<(), serde_json::Error> {
    println!("{}", format_health_report_json(report)?);
    Ok(())
}

fn format_health_report_json(report: &CatalogueHealthReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_health_report(report: &CatalogueHealthReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Health Report\n\n");
    output.push_str(&format!("Archives checked: {}\n", report.archives_checked));
    output.push_str(&format!("Missing: {}\n", report.missing_count));
    output.push_str(&format!(
        "Unknown platform: {}\n",
        report.unknown_platform_count
    ));

    if report.issues.is_empty() {
        output.push_str("\nNo archive health issues were found.\n");
        return output;
    }

    output.push_str("\nIssues:\n");
    for issue in &report.issues {
        output.push_str(&format!(
            "  [{}] {} ({}) - {}\n",
            issue.category.label(),
            issue.path.display(),
            issue.platform.as_deref().unwrap_or("Unknown"),
            issue.reason
        ));
    }
    output
}

/// A `library-scan` result, reshaped from [`ScanPersistSummary`] into
/// names that read clearly on their own (`source_folders_attempted` etc.)
/// rather than requiring the reader to know this crate's internal
/// `ScanRunCounts` field names.
#[derive(Debug, Clone, Serialize)]
struct LibraryScanReport {
    scan_run_id: i64,
    source_folders_attempted: i64,
    source_folders_succeeded: i64,
    source_folders_failed: i64,
    archives_new: i64,
    archives_changed: i64,
    archives_restored: i64,
    archives_unchanged: i64,
    archives_missing: i64,
    folder_errors: Vec<FolderErrorView>,
}

#[derive(Debug, Clone, Serialize)]
struct FolderErrorView {
    path: PathBuf,
    error: String,
}

impl From<&ScanPersistSummary> for LibraryScanReport {
    fn from(summary: &ScanPersistSummary) -> Self {
        let succeeded = summary.counts.source_folders_scanned;
        let failed = summary.folder_errors.len() as i64;
        Self {
            scan_run_id: summary.scan_run_id,
            source_folders_attempted: succeeded + failed,
            source_folders_succeeded: succeeded,
            source_folders_failed: failed,
            archives_new: summary.counts.archives_added,
            archives_changed: summary.counts.archives_changed,
            archives_restored: summary.counts.archives_restored,
            archives_unchanged: summary.counts.archives_unchanged,
            archives_missing: summary.counts.archives_missing,
            folder_errors: summary
                .folder_errors
                .iter()
                .map(|(path, error)| FolderErrorView {
                    path: path.clone(),
                    error: error.clone(),
                })
                .collect(),
        }
    }
}

/// Opens (creating if needed) the database at `database_path`, runs
/// [`scan_and_persist`] against `config`, and reshapes the result. A
/// database or config problem propagates as `Err` (a non-zero exit code
/// from `main`); one or more failed source folders within an otherwise
/// successful run does not - it shows up in the returned report's
/// `folder_errors` instead. See `docs/DATABASE_DESIGN.md` section 5: this
/// never touches mount or unmount state.
fn run_library_scan(
    config: &Config,
    database_path: &Path,
    triggered_by: &str,
) -> Result<LibraryScanReport, Box<dyn std::error::Error>> {
    let mut database = Database::open_or_create(database_path)?;
    let summary = scan_and_persist(&mut database, config, triggered_by)?;
    Ok(LibraryScanReport::from(&summary))
}

fn print_library_scan(report: &LibraryScanReport) {
    print!("{}", format_library_scan(report));
}

fn print_library_scan_json(report: &LibraryScanReport) -> Result<(), serde_json::Error> {
    println!("{}", format_library_scan_json(report)?);
    Ok(())
}

fn format_library_scan_json(report: &LibraryScanReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_library_scan(report: &LibraryScanReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Library Scan\n\n");
    output.push_str("Source folders:\n");
    output.push_str(&format!(
        "  Attempted: {}\n",
        report.source_folders_attempted
    ));
    output.push_str(&format!(
        "  Succeeded: {}\n",
        report.source_folders_succeeded
    ));
    output.push_str(&format!("  Failed: {}\n", report.source_folders_failed));
    output.push_str("\nArchives:\n");
    output.push_str(&format!("  New: {}\n", report.archives_new));
    output.push_str(&format!("  Changed: {}\n", report.archives_changed));
    output.push_str(&format!("  Restored: {}\n", report.archives_restored));
    output.push_str(&format!("  Unchanged: {}\n", report.archives_unchanged));
    output.push_str(&format!("  Missing: {}\n", report.archives_missing));
    output.push_str("\nErrors:\n");
    if report.folder_errors.is_empty() {
        output.push_str("  none\n");
    } else {
        for error in &report.folder_errors {
            output.push_str(&format!("  {}: {}\n", error.path.display(), error.error));
        }
    }
    output
}

/// Resolves a `sources`/`source` sub-command's `<id-or-path>` argument
/// against the current config and database state - shared by every
/// per-source CLI sub-command so the numeric-id-vs-path resolution
/// (`resolve_source_folder_identifier`) is called exactly the same way
/// everywhere.
fn resolve_source_identifier(identifier: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let sources = load_source_folder_configs_default().map_err(|error| error.to_string())?;
    let database_path = default_database_path()?;
    let database = Database::open_or_create(&database_path)?;
    let records = database.list_source_folders()?;
    resolve_source_folder_identifier(identifier, &sources, &records)
        .map_err(|error| error.to_string().into())
}

fn format_source_availability(availability: SourceAvailability) -> &'static str {
    match availability {
        SourceAvailability::Available => "Available",
        SourceAvailability::Unavailable => "Unavailable",
        SourceAvailability::PermissionDenied => "Permission denied",
        SourceAvailability::Disabled => "Disabled",
        SourceAvailability::ScanFailed => "Scan failed",
    }
}

fn print_source_folder_views(views: &[SourceFolderView]) {
    print!("{}", format_source_folder_views(views));
}

fn format_source_folder_views(views: &[SourceFolderView]) -> String {
    let mut output = String::from("ArchiveFS Sources\n\n");
    if views.is_empty() {
        output.push_str("No source folders are configured.\n");
        output.push_str("Add one with: archivefs-cli source add <path>\n");
        return output;
    }
    for view in views {
        output.push_str(&format!("{}\n", view.path.display()));
        output.push_str(&format!(
            "  Id:               {}\n",
            view.id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ));
        output.push_str(&format!(
            "  Enabled:          {}\n",
            if view.enabled { "yes" } else { "no" }
        ));
        output.push_str(&format!(
            "  Availability:     {}\n",
            format_source_availability(view.availability)
        ));
        output.push_str(&format!(
            "  Last archive count: {}\n",
            view.last_archive_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "never scanned".to_string())
        ));
        output.push_str(&format!(
            "  Last scan:        {}\n",
            view.last_scan_at.as_deref().unwrap_or("never")
        ));
        if let Some(error) = &view.last_scan_error {
            output.push_str(&format!("  Last scan error:  {error}\n"));
        }
        output.push('\n');
    }
    output
}

/// `view preview --json` output: the view alongside its plan, both already
/// `Serialize` on the core types themselves (including their non-UTF-8-safe
/// path encoding) - no CLI-local reshaping needed, unlike
/// `RemoveSourceFolderReport` below.
#[derive(Debug, Serialize)]
struct LibraryViewPreviewJson<'a> {
    view: &'a LibraryViewConfig,
    plan: &'a LibraryViewPlan,
}

/// `view apply`/`view repair`/`view remove --json` output: the view
/// alongside the resulting report.
#[derive(Debug, Serialize)]
struct LibraryViewApplyJson<'a> {
    view: &'a LibraryViewConfig,
    report: &'a LibraryViewApplyReport,
}

fn print_library_views(views: &[LibraryViewConfig]) {
    println!("ArchiveFS Library Views");
    println!();
    if views.is_empty() {
        println!("No library views are configured.");
        return;
    }
    for view in views {
        println!("{} ({})", view.name, view.id);
        println!(
            "  Enabled:        {}",
            if view.enabled { "yes" } else { "no" }
        );
        println!("  Destination:    {}", view.destination_root.display());
        println!(
            "  Source folders: {}",
            if view.source_folders.is_empty() {
                "all configured sources".to_string()
            } else {
                view.source_folders
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        println!(
            "  Platforms:      {}",
            if view.platforms.is_empty() {
                "all known platforms".to_string()
            } else {
                view.platforms.join(", ")
            }
        );
        println!("  Layout:         {}", view.layout_template.label());
        println!();
    }
}

fn print_library_view_plan(view: &LibraryViewConfig, plan: &LibraryViewPlan) {
    println!("Library View: {} ({})", view.name, view.id);
    println!("Destination:  {}", plan.destination_root.display());
    if let Some(error) = &plan.unsafe_root_error {
        println!();
        println!("UNSAFE - Apply is refused: {error}");
        return;
    }
    println!();
    println!("Create:    {}", plan.counts.create);
    println!("Correct:   {}", plan.counts.correct);
    println!("Repair:    {}", plan.counts.repair);
    println!("Remove:    {}", plan.counts.remove);
    println!("Collision: {}", plan.counts.collision);
    println!("Skip:      {}", plan.counts.skip);

    let interesting: Vec<&LibraryViewPlanEntry> = plan
        .entries
        .iter()
        .filter(|entry| entry.action != LibraryViewPlanAction::AlreadyCorrect)
        .collect();
    if interesting.is_empty() {
        return;
    }
    println!();
    println!("Details:");
    for entry in interesting {
        let path = entry
            .destination_path
            .as_ref()
            .or(entry.archive_path.as_ref())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "?".to_string());
        print!("  [{:?}] {path}", entry.action);
        if let Some(reason) = &entry.reason {
            print!(" - {reason}");
        }
        println!();
    }
}

fn print_library_view_apply_report(
    view: &LibraryViewConfig,
    report: &LibraryViewApplyReport,
    verb: &str,
) {
    println!("Library View: {} ({})", view.name, view.id);
    println!("{verb} complete:");
    println!("  Created:   {}", report.created);
    println!("  Repaired:  {}", report.repaired);
    println!("  Removed:   {}", report.removed);
    println!("  Unchanged: {}", report.unchanged);
    println!("  Failed:    {}", report.failed);
    if report.failed > 0 {
        println!();
        println!("Failures:");
        for result in &report.results {
            if result.outcome == LibraryViewApplyOutcome::Failed {
                println!(
                    "  {} - {}",
                    result.relative_link_path.display(),
                    result.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }
}

fn print_library_view_remove_report(
    view: &LibraryViewConfig,
    report: &LibraryViewApplyReport,
    kept_definition: bool,
) {
    println!("Library View: {} ({})", view.name, view.id);
    println!("Removed {} managed symlink(s).", report.removed);
    println!("ArchiveFS did not delete any original archive files.");
    if kept_definition {
        println!("The view definition was kept - Preview/Apply will recreate its symlinks.");
    } else {
        println!("The view definition was also removed from configuration.");
    }
    let left_untouched: Vec<_> = report
        .results
        .iter()
        .filter(|result| result.outcome == LibraryViewApplyOutcome::LeftUnchanged)
        .collect();
    if !left_untouched.is_empty() {
        println!();
        println!("Left untouched (changed since the last apply):");
        for result in left_untouched {
            println!("  {}", result.relative_link_path.display());
        }
    }
}

/// `source remove`'s `--json` output - a small display-ready reshaping of
/// [`archivefs_core::RemoveSourceFolderOutcome`], matching the same
/// "CLI-local report type" convention `LibraryScanReport` already
/// establishes rather than deriving `Serialize` on the core outcome type
/// directly.
#[derive(Debug, Clone, Serialize)]
struct RemoveSourceFolderReport {
    #[serde(serialize_with = "serialize_path_display")]
    removed_path: PathBuf,
    catalogue_rows_removed: Option<usize>,
}

/// One archive as shown by `library-list`/`library-find`: a display-ready
/// reshaping of [`PersistedArchive`] with just the fields those commands
/// (and `library-set-platform`/`library-clear-platform`'s query
/// resolution - see `select_one_library_entry`) need, not the full
/// persisted row (normalized name, cached health, ...).
///
/// `id` is the archive's stable persisted database id: the identity
/// `library-set-platform --id`/`library-clear-platform --id` target
/// directly, and the exact selection a query is required to narrow down
/// to before either command acts (see `resolve_library_target`) - never
/// a lossy display string.
///
/// `path` serializes via `Path::display` (see `serialize_path_display`)
/// rather than `PathBuf`'s own `Serialize` impl, which requires valid
/// Unicode and would otherwise make `--json` output fail entirely for the
/// whole list just because one archive's path is not valid UTF-8. Exact
/// path bytes remain safely preserved in the database (see
/// `PersistedArchive`/`archives.relative_path`) - this is purely a display
/// concern for a view type, matching the same "display-safe path text"
/// this crate already uses for `library-find`'s search matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LibraryArchiveView {
    id: i64,
    #[serde(serialize_with = "serialize_path_display")]
    path: PathBuf,
    platform: Option<String>,
    platform_source: Option<String>,
    present: bool,
    size_bytes: Option<u64>,
    modified_time_unix_seconds: Option<i64>,
}

fn serialize_path_display<S: serde::Serializer>(
    path: &std::path::Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.display().to_string())
}

/// Formats a platform assignment for human display as `"<platform>
/// (<provenance>)"`, or `"Unknown"` when there is none. `platform` and
/// `platform_source` are `None` together or not at all (see
/// [`PersistedArchive::platform_source`]) - `(None, None)` and any
/// otherwise-inconsistent combination both fall back to `"Unknown"`
/// rather than panicking or fabricating a value.
fn format_platform_and_source(platform: Option<&str>, source: Option<&str>) -> String {
    match (platform, source) {
        (Some(platform), Some(source)) => format!("{platform} ({source})"),
        _ => "Unknown".to_string(),
    }
}

/// Matches `input` against [`canonical_platform_names`] case-insensitively,
/// returning the one canonical spelling to actually store (never
/// whatever casing the user typed) - so `xbox360`, `Xbox360`, and
/// `XBOX360` all resolve to the same stored value, and the database
/// never accumulates casing variants of the same platform. `None` means
/// no canonical platform matches at all (the `--custom` escape hatch is
/// the only way to store such a value).
fn resolve_canonical_platform_spelling(input: &str) -> Option<&'static str> {
    canonical_platform_names()
        .into_iter()
        .find(|canonical| canonical.eq_ignore_ascii_case(input))
}

/// `library-set-platform`'s platform-argument resolution, factored out
/// so it is directly testable (mirrors this file's existing convention
/// of factoring `run()` match-arm logic into a plain function rather
/// than testing `run()` itself - see `resolve_library_target`,
/// `resolve_target_selector`). `--custom` stores `platform` exactly as
/// typed; otherwise it must case-insensitively match a canonical name,
/// and the canonical spelling (not the user's casing) is what gets
/// stored.
fn resolve_platform_argument(
    platform: String,
    custom: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    if custom {
        return Ok(platform);
    }
    resolve_canonical_platform_spelling(&platform)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "unsupported platform '{platform}'. Must be one of: {}. Pass --custom to assign free-form platform text.",
                canonical_platform_names().join(", ")
            )
            .into()
        })
}

impl From<&PersistedArchive> for LibraryArchiveView {
    fn from(archive: &PersistedArchive) -> Self {
        Self {
            id: archive.id,
            path: archive.absolute_path.clone(),
            platform: archive.platform.clone(),
            platform_source: archive.platform_source.clone(),
            present: archive.last_verified_missing_at.is_none(),
            size_bytes: archive.size_bytes,
            modified_time_unix_seconds: archive.modified_time_unix_seconds,
        }
    }
}

/// Loads every persisted archive for `library-list`/`library-find`. If no
/// database file exists yet, this is an empty catalogue (`Ok(vec![])`),
/// not an error - `print_library_entries` distinguishes "no database yet"
/// from "database exists but is empty" for the human-readable message.
/// Never rescans - reads the existing database only.
///
/// `unknown_only` filters to archives whose *effective* platform is
/// unknown (see [`persisted_archive_has_unknown_platform`] - the same
/// canonical definition the GUI's unknown-platform count/filter uses),
/// applied here at the [`PersistedArchive`] stage before the
/// [`LibraryArchiveView`] conversion so `library-list`/`library-find`
/// share one filtering path rather than each re-deriving "unknown" from
/// the view type. Includes both present and missing archives - nothing
/// else currently controls state filtering for these two commands, so
/// there is no other option to interact with. The JSON output shape is
/// unaffected either way: the same array of the same object shape, just
/// fewer elements when this is set.
fn build_library_entries(
    database_path: &Path,
    unknown_only: bool,
) -> Result<Vec<LibraryArchiveView>, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Ok(Vec::new());
    }
    let database = Database::open_or_create(database_path)?;
    Ok(database
        .load_archives()?
        .iter()
        .filter(|archive| !unknown_only || persisted_archive_has_unknown_platform(archive))
        .map(LibraryArchiveView::from)
        .collect())
}

/// Case-insensitive match against each entry's display-safe path text
/// (`Path::display`, the same lossy-for-display-only conversion used
/// throughout this CLI - never the entry's identity) and detected
/// platform, mirroring `find_archive_index_entries`'s existing matching
/// style for the JSON index.
fn filter_library_entries(entries: &[LibraryArchiveView], query: &str) -> Vec<LibraryArchiveView> {
    let needle = query.to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            entry
                .path
                .display()
                .to_string()
                .to_lowercase()
                .contains(&needle)
                || entry
                    .platform
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
        })
        .cloned()
        .collect()
}

fn print_library_entries(database_path: &Path, entries: &[LibraryArchiveView]) {
    if entries.is_empty() {
        if database_path.exists() {
            println!("No archives in the library catalogue yet.");
        } else {
            println!(
                "No library database found at {}. Run: archivefs-cli library-scan",
                database_path.display()
            );
        }
        return;
    }

    println!("ArchiveFS Library List\n");
    print!("{}", format_library_entries(entries));
}

fn print_library_find_results(query: &str, entries: &[LibraryArchiveView]) {
    if entries.is_empty() {
        println!("No library matches found for '{query}'.");
        return;
    }

    println!("ArchiveFS Library Find");
    println!("Query: {query}\n");
    print!("{}", format_library_entries(entries));
}

fn print_library_entries_json(entries: &[LibraryArchiveView]) -> Result<(), serde_json::Error> {
    println!("{}", format_library_entries_json(entries)?);
    Ok(())
}

fn format_library_entries_json(
    entries: &[LibraryArchiveView],
) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(entries)
}

/// Loads every persisted custom platform alias for `platform-alias-list`.
/// If no database file exists yet, this is an empty list (`Ok(vec![])`),
/// not an error - mirroring `build_library_entries`'s existing "no
/// database yet is not a failure" convention for a read-only listing
/// command. Never creates the database and never scans.
fn list_platform_aliases_or_empty(
    database_path: &Path,
) -> Result<Vec<PlatformAlias>, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Ok(Vec::new());
    }
    let database = Database::open_or_create(database_path)?;
    Ok(database.list_platform_aliases()?)
}

fn print_platform_aliases(aliases: &[PlatformAlias]) {
    if aliases.is_empty() {
        println!("No custom platform aliases defined.");
        return;
    }

    println!("ArchiveFS Platform Aliases\n");
    for alias in aliases {
        println!("  Alias: {}", alias.alias);
        println!("  Platform: {}", alias.platform);
        println!();
    }
}

fn print_platform_alias_saved(alias: &PlatformAlias) {
    println!(
        "Saved platform alias '{}' -> {}.",
        alias.alias, alias.platform
    );
    println!("Run a library scan to apply this change.");
}

/// `platform-alias-add`'s argument parsing, factored out so it is
/// directly testable (mirrors this file's existing convention of
/// factoring `run()` match-arm logic into a plain function rather than
/// testing `run()` itself - see `resolve_platform_argument`,
/// `resolve_target_selector`). Exactly two positional arguments
/// (alias, platform) are required after `--json` is extracted; anything
/// else (zero, one, or three or more) is a clear error.
fn parse_platform_alias_add_args(
    mut args: Vec<String>,
) -> Result<(bool, String, String), Box<dyn std::error::Error>> {
    let json = extract_flag(&mut args, "--json");
    match args.as_slice() {
        [alias, platform] => Ok((json, alias.clone(), platform.clone())),
        _ => Err(
            "platform-alias-add requires exactly an alias and a platform, e.g. \
             archivefs-cli platform-alias-add gc GameCube"
                .into(),
        ),
    }
}

fn format_library_entries(entries: &[LibraryArchiveView]) -> String {
    let mut output = String::new();
    for entry in entries {
        output.push_str(&format!("  Id: {}\n", entry.id));
        output.push_str(&format!("  Path: {}\n", entry.path.display()));
        output.push_str(&format!(
            "  Platform: {}\n",
            format_platform_and_source(entry.platform.as_deref(), entry.platform_source.as_deref())
        ));
        output.push_str(&format!(
            "  State: {}\n",
            if entry.present { "Present" } else { "Missing" }
        ));
        output.push_str(&format!(
            "  Size: {}\n",
            entry
                .size_bytes
                .map(human_size)
                .unwrap_or_else(|| "unknown".to_string())
        ));
        output.push_str(&format!(
            "  Modified: {}\n",
            entry
                .modified_time_unix_seconds
                .map(|seconds| format_unix_timestamp(seconds.max(0) as u64))
                .unwrap_or_else(|| "unknown".to_string())
        ));
        output.push('\n');
    }
    output
}

/// The result of one `library-set-platform`/`library-clear-platform`
/// call: the archive's display path plus [`PlatformAssignmentChange`]'s
/// old/new platform and provenance, exactly what requirement 3 asks
/// these commands to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LibraryPlatformChangeView {
    #[serde(serialize_with = "serialize_path_display")]
    path: PathBuf,
    old_platform: Option<String>,
    old_source: Option<String>,
    new_platform: Option<String>,
    new_source: Option<String>,
}

impl LibraryPlatformChangeView {
    fn new(path: PathBuf, change: PlatformAssignmentChange) -> Self {
        Self {
            path,
            old_platform: change.old_platform,
            old_source: change.old_source,
            new_platform: change.new_platform,
            new_source: change.new_source,
        }
    }
}

/// How `library-set-platform`/`library-clear-platform` select the one
/// archive to act on - see [`resolve_library_target`]. Exactly one of
/// these three ways is used per invocation, never a combination (see
/// [`resolve_target_selector`], which enforces this from the parsed
/// command-line flags).
enum LibraryTargetSelector {
    /// The stable persisted archive id - unambiguous by construction
    /// (requirement 2: "a stable persisted archive ID where safer").
    Id(i64),
    /// The archive's exact absolute path ([`PersistedArchive::absolute_path`]),
    /// compared exactly as parsed, never a lossy display string
    /// (requirement 2: "the existing exact database identity").
    Path(PathBuf),
    /// A free-text query, matched exactly like `library-find`
    /// ([`filter_library_entries`]) - see [`select_one_library_entry`]
    /// for how more than one match is handled.
    Query(String),
}

/// Resolves exactly one archive for `library-set-platform`/
/// `library-clear-platform` to act on, and the open [`Database`] handle
/// to act with. Every database write these two commands perform goes
/// through the returned entry's `id`, never a lossy display string.
fn resolve_library_target(
    database_path: &Path,
    selector: &LibraryTargetSelector,
) -> Result<(Database, LibraryArchiveView), Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Err(format!(
            "No library database found at {}. Run: archivefs-cli library-scan",
            database_path.display()
        )
        .into());
    }
    let database = Database::open_or_create(database_path)?;
    let entries: Vec<LibraryArchiveView> = database
        .load_archives()?
        .iter()
        .map(LibraryArchiveView::from)
        .collect();

    let target = match selector {
        LibraryTargetSelector::Id(id) => entries
            .into_iter()
            .find(|entry| entry.id == *id)
            .ok_or_else(|| format!("no archive found with id {id}"))?,
        LibraryTargetSelector::Path(path) => entries
            .into_iter()
            .find(|entry| &entry.path == path)
            .ok_or_else(|| format!("no archive found with exact path {}", path.display()))?,
        LibraryTargetSelector::Query(query) => select_one_library_entry(&entries, query)?,
    };
    Ok((database, target))
}

/// Builds the one [`LibraryTargetSelector`] a `library-set-platform`/
/// `library-clear-platform` invocation uses, from its already-extracted
/// `--id`/`--path` flags and whatever positional query words remain.
/// Exactly one selection method is required: giving both `--id` and
/// `--path`, or giving a flag plus leftover query words, is a clear
/// error rather than a silent "first one wins" guess.
fn resolve_target_selector(
    command: &str,
    id: Option<i64>,
    path: Option<PathBuf>,
    remaining_query_args: Vec<String>,
) -> Result<LibraryTargetSelector, Box<dyn std::error::Error>> {
    match (id, path) {
        (Some(_), Some(_)) => Err("--id and --path cannot both be given".into()),
        (Some(id), None) => {
            if remaining_query_args.is_empty() {
                Ok(LibraryTargetSelector::Id(id))
            } else {
                Err(format!("{command} --id <id> takes no additional query arguments").into())
            }
        }
        (None, Some(path)) => {
            if remaining_query_args.is_empty() {
                Ok(LibraryTargetSelector::Path(path))
            } else {
                Err(format!("{command} --path <path> takes no additional query arguments").into())
            }
        }
        (None, None) => {
            if remaining_query_args.is_empty() {
                Err(format!("{command} requires a query, --id <id>, or --path <path>").into())
            } else {
                Ok(LibraryTargetSelector::Query(remaining_query_args.join(" ")))
            }
        }
    }
}

/// Matches `query` against `entries` exactly like `library-find`
/// ([`filter_library_entries`]), then requires the result to be exactly
/// one archive: zero matches and more than one match are both clear
/// errors (never a silent guess), so `library-set-platform`/
/// `library-clear-platform` can never act on the wrong archive or more
/// than one archive from an imprecise query. An ambiguous match lists
/// every candidate with its id, so the caller can immediately retry with
/// `--id <id>`.
fn select_one_library_entry(
    entries: &[LibraryArchiveView],
    query: &str,
) -> Result<LibraryArchiveView, Box<dyn std::error::Error>> {
    let mut matches = filter_library_entries(entries, query);
    match matches.len() {
        0 => Err(format!("no archive matched '{query}'").into()),
        1 => Ok(matches.remove(0)),
        _ => {
            let mut message = format!("multiple archives matched '{query}':\n");
            for entry in &matches {
                message.push_str(&format!("  [id {}] {}\n", entry.id, entry.path.display()));
            }
            message.push_str("Re-run with --id <id> to select exactly one archive.");
            Err(message.into())
        }
    }
}

/// `library-set-platform`'s testable core: resolves the target archive,
/// then sets `platform` as its manual assignment. See
/// [`resolve_library_target`] for how `selector` picks the archive, and
/// [`Database::set_manual_platform`] for the precedence this assignment
/// gets over automatic detection.
fn run_library_set_platform(
    database_path: &Path,
    selector: &LibraryTargetSelector,
    platform: &str,
) -> Result<LibraryPlatformChangeView, Box<dyn std::error::Error>> {
    let (mut database, target) = resolve_library_target(database_path, selector)?;
    let change = database.set_manual_platform(target.id, platform)?;
    Ok(LibraryPlatformChangeView::new(target.path, change))
}

/// `library-clear-platform`'s testable core: resolves the target archive,
/// then clears its manual assignment, if it has one - see
/// [`Database::clear_manual_platform`] for the no-op behavior when it
/// does not, and for how the latest automatic result becomes current
/// again immediately (no rescan needed).
fn run_library_clear_platform(
    database_path: &Path,
    selector: &LibraryTargetSelector,
) -> Result<LibraryPlatformChangeView, Box<dyn std::error::Error>> {
    let (mut database, target) = resolve_library_target(database_path, selector)?;
    let change = database.clear_manual_platform(target.id)?;
    Ok(LibraryPlatformChangeView::new(target.path, change))
}

/// Removes every occurrence of `flag` from `args`, returning whether it
/// was present. Shared by `--json`, and `--custom`.
fn extract_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let had_flag = args.iter().any(|arg| arg == flag);
    args.retain(|arg| arg != flag);
    had_flag
}

/// `source remove` deliberately never defaults to a destructive choice -
/// the caller must always pass exactly one of `--keep-catalogue` or
/// `--remove-catalogue` explicitly. Returns `true` (keep) or `false`
/// (remove); errors clearly on neither or both.
fn resolve_keep_catalogue_flag(
    keep_catalogue: bool,
    remove_catalogue: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    match (keep_catalogue, remove_catalogue) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        (false, false) => Err(
            "source remove requires exactly one of --keep-catalogue or --remove-catalogue".into(),
        ),
        (true, true) => {
            Err("source remove cannot take both --keep-catalogue and --remove-catalogue".into())
        }
    }
}

/// Removes `--id <value>` from `args` if present, returning the parsed
/// id - the stable persisted archive id `library-set-platform`/
/// `library-clear-platform` accept as an unambiguous alternative to a
/// text query (see requirement 2: "or a stable persisted archive ID
/// where safer").
fn extract_id_flag(args: &mut Vec<String>) -> Result<Option<i64>, Box<dyn std::error::Error>> {
    let Some(position) = args.iter().position(|arg| arg == "--id") else {
        return Ok(None);
    };
    if position + 1 >= args.len() {
        return Err("--id requires a value".into());
    }
    let value = args.remove(position + 1);
    args.remove(position);
    value
        .parse::<i64>()
        .map(Some)
        .map_err(|_| format!("--id value '{value}' is not a valid archive id").into())
}

/// Removes `--path <value>` from `args` if present, returning the parsed
/// path unchanged (exact bytes, no normalization or lossy conversion) -
/// the exact-path alternative to `--id`/a text query (requirement 2).
fn extract_path_flag(
    args: &mut Vec<String>,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let Some(position) = args.iter().position(|arg| arg == "--path") else {
        return Ok(None);
    };
    if position + 1 >= args.len() {
        return Err("--path requires a value".into());
    }
    let value = args.remove(position + 1);
    args.remove(position);
    Ok(Some(PathBuf::from(value)))
}

/// Removes every `--id <value>` occurrence from `args`, returning the
/// parsed ids in the order given - the bulk counterpart to
/// [`extract_id_flag`], which only ever handles (and requires) a single
/// occurrence. Used by `library-set-platform-bulk`/
/// `library-clear-platform-bulk`, where repeating `--id` is the normal,
/// expected way to select more than one archive.
fn extract_repeated_id_flags(
    args: &mut Vec<String>,
) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let mut ids = Vec::new();
    while let Some(position) = args.iter().position(|arg| arg == "--id") {
        if position + 1 >= args.len() {
            return Err("--id requires a value".into());
        }
        let value = args.remove(position + 1);
        args.remove(position);
        let id = value
            .parse::<i64>()
            .map_err(|_| format!("--id value '{value}' is not a valid archive id"))?;
        ids.push(id);
    }
    Ok(ids)
}

/// Removes every `--path <value>` occurrence from `args`, returning the
/// parsed paths unchanged (exact bytes) in the order given - the bulk
/// counterpart to [`extract_path_flag`]. See [`extract_repeated_id_flags`].
fn extract_repeated_path_flags(
    args: &mut Vec<String>,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    while let Some(position) = args.iter().position(|arg| arg == "--path") {
        if position + 1 >= args.len() {
            return Err("--path requires a value".into());
        }
        let value = args.remove(position + 1);
        args.remove(position);
        paths.push(PathBuf::from(value));
    }
    Ok(paths)
}

/// Resolves the archive ids `library-set-platform-bulk`/
/// `library-clear-platform-bulk` should act on: `ids` are passed through
/// as-is (an id that does not exist is reported in the database bulk
/// call's own `missing` list, not rejected here - see
/// `Database::set_manual_platform_for_archives`'s missing-id policy);
/// each `paths` entry is resolved to its exact archive id via
/// `Database::find_archive_id_by_absolute_path` and must resolve - an
/// unresolvable path is an immediate, hard error, mirroring
/// `library-set-platform --path`'s existing exact-path behavior (a bad
/// path is a caller mistake at the CLI layer, not a "missing id" the
/// database layer should have to reason about). The two lists are simply
/// concatenated, not deduplicated here - `Database::set_manual_platform_for_archives`/
/// `clear_manual_platform_for_archives` already deduplicate by id, so a
/// path and an `--id` that happen to name the same archive are still
/// only ever processed once.
fn resolve_bulk_target_ids(
    database: &Database,
    ids: &[i64],
    paths: &[PathBuf],
) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    let mut resolved: Vec<i64> = ids.to_vec();
    for path in paths {
        let id = database
            .find_archive_id_by_absolute_path(path)?
            .ok_or_else(|| format!("no archive found with exact path {}", path.display()))?;
        resolved.push(id);
    }
    Ok(resolved)
}

/// Requires at least one `--id`/`--path` for `command` - the bulk
/// counterpart to [`resolve_target_selector`]'s "no selector given"
/// check, factored out the same way for direct testability. Bulk
/// commands never accept a free-text query, so an empty `ids` and
/// `paths` together is the only ambiguity to guard against here.
fn require_at_least_one_bulk_selector(
    command: &str,
    ids: &[i64],
    paths: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    if ids.is_empty() && paths.is_empty() {
        Err(format!("{command} requires at least one --id or --path").into())
    } else {
        Ok(())
    }
}

/// `library-set-platform-bulk`'s testable core: resolves every target id
/// (see [`resolve_bulk_target_ids`]), then sets `platform` as their
/// manual assignment in one transaction - see
/// [`Database::set_manual_platform_for_archives`].
fn run_library_set_platform_bulk(
    database_path: &Path,
    ids: &[i64],
    paths: &[PathBuf],
    platform: &str,
) -> Result<BulkPlatformAssignmentResult, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Err(format!(
            "No library database found at {}. Run: archivefs-cli library-scan",
            database_path.display()
        )
        .into());
    }
    let mut database = Database::open_or_create(database_path)?;
    let target_ids = resolve_bulk_target_ids(&database, ids, paths)?;
    Ok(database.set_manual_platform_for_archives(&target_ids, platform)?)
}

/// `library-clear-platform-bulk`'s testable core - see
/// [`run_library_set_platform_bulk`] and
/// [`Database::clear_manual_platform_for_archives`].
fn run_library_clear_platform_bulk(
    database_path: &Path,
    ids: &[i64],
    paths: &[PathBuf],
) -> Result<BulkPlatformAssignmentResult, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Err(format!(
            "No library database found at {}. Run: archivefs-cli library-scan",
            database_path.display()
        )
        .into());
    }
    let mut database = Database::open_or_create(database_path)?;
    let target_ids = resolve_bulk_target_ids(&database, ids, paths)?;
    Ok(database.clear_manual_platform_for_archives(&target_ids)?)
}

/// Removes catalogue rows selected only by exact persisted id/path. All paths
/// are resolved before the database transaction begins; the core API then
/// validates that every deduplicated id exists and is currently missing before
/// deleting anything. This never scans and never accesses an archive file.
fn run_library_remove_missing(
    database_path: &Path,
    ids: &[i64],
    paths: &[PathBuf],
) -> Result<MissingArchiveRemovalResult, Box<dyn std::error::Error>> {
    if !database_path.exists() {
        return Err(format!(
            "No library database found at {}. Run: archivefs-cli library-scan",
            database_path.display()
        )
        .into());
    }
    let mut database = Database::open_or_create(database_path)?;
    let target_ids = resolve_bulk_target_ids(&database, ids, paths)?;
    Ok(database.remove_missing_archives(&target_ids)?)
}

fn format_missing_removal(result: &MissingArchiveRemovalResult) -> String {
    format!(
        "ArchiveFS Library Remove Missing\nRemoved: {} missing catalogue entr{}.\nNo archive files or mounted contents were deleted.\n",
        result.removed,
        if result.removed == 1 { "y" } else { "ies" }
    )
}

fn print_bulk_platform_change(action: &str, summary: &BulkPlatformAssignmentResult) {
    println!("ArchiveFS Library {action}");
    println!("Requested: {}", summary.requested);
    println!("Changed: {}", summary.changed);
    println!("Unchanged: {}", summary.unchanged);
    if summary.missing.is_empty() {
        println!("Missing: none");
    } else {
        let missing = summary
            .missing
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!("Missing: {missing}");
    }
}

fn print_library_platform_change(action: &str, change: &LibraryPlatformChangeView) {
    println!("ArchiveFS Library {action}");
    println!("Path: {}", change.path.display());
    println!(
        "Old platform: {}",
        format_platform_and_source(change.old_platform.as_deref(), change.old_source.as_deref())
    );
    println!(
        "New platform: {}",
        format_platform_and_source(change.new_platform.as_deref(), change.new_source.as_deref())
    );
}

fn print_library_platform_change_json(
    change: &LibraryPlatformChangeView,
) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(change)?);
    Ok(())
}

fn print_cleaned_dirs(paths: &[std::path::PathBuf]) {
    for path in paths {
        println!("Removed: {}", path.display());
    }
    println!("Removed {} empty directories.", paths.len());
}

fn print_index_find_results(query: &str, entries: &[ArchiveIndexEntry]) {
    if entries.is_empty() {
        println!("No index matches found for '{query}'.");
        return;
    }

    println!("ArchiveFS Index Find");
    println!("Query: {query}");
    println!();
    println!("Matches:");
    for entry in entries {
        println!(
            "  Platform: {}",
            entry.platform.as_deref().unwrap_or("Unknown")
        );
        println!("  Display: {}", entry.display_name);
        println!("  Archive: {}", entry.archive_path.display());
        println!("  Mount: {}", entry.mount_path.display());
        println!("  Health: {}", entry.health);
        println!("  State: {}", entry.mount_state);
        println!();
    }
}

fn print_index_summary(summary: &ArchiveIndexSummary) {
    println!("ArchiveFS Index");
    println!();
    println!("Summary:");
    println!("  Total archives: {}", summary.archives_count);
    println!("  Mounted: {}", summary.mounted_count);
    println!("  Pending: {}", summary.pending_count);
    println!();
    println!("Platforms:");
    if summary.platform_counts.is_empty() {
        println!("  none");
    } else {
        for (platform, count) in &summary.platform_counts {
            println!("  {platform}: {count}");
        }
    }
}

fn print_mount_one(plan: &MountPlan) {
    println!("Mounted:");
    println!("  Archive: {}", plan.archive.path.display());
    println!("  Mount:   {}", plan.mount_path.display());
}

fn print_unmount_one(plan: &MountPlan) {
    println!("Unmounted:");
    println!("  Archive: {}", plan.archive.path.display());
    println!("  Mount:   {}", plan.mount_path.display());
}

fn print_doctor_report(report: &DoctorReport) {
    print!("{}", format_doctor_report(report));
}

fn print_doctor_report_json(report: &DoctorReport) -> Result<(), serde_json::Error> {
    println!("{}", format_doctor_report_json(report)?);
    Ok(())
}

fn format_doctor_report_json(report: &DoctorReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_doctor_report(report: &DoctorReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Doctor\n");
    output.push_str(&format!("Config: {}\n", report.config_path.display()));
    output.push_str("\nChecks:\n");
    for check in &report.checks {
        output.push_str(&format!(
            "  [{:<4}] {:<16} {}\n",
            check.status, check.name, check.detail
        ));
    }
    output.push_str("\nSummary:\n");
    output.push_str(&format!("  Archives found: {}\n", report.archives_found));
    output.push_str(&format!(
        "  Archives with detected platform: {}\n",
        report.archives_with_platform
    ));
    output.push_str(&format!(
        "  Archives with unknown platform: {}\n",
        report.archives_unknown_platform
    ));
    output.push_str(&format!(
        "  Pending archives: {}\n",
        report.pending_archives
    ));
    output.push_str(&format!(
        "  Mounted archives: {}\n",
        report.mounted_archives
    ));
    output.push_str(&format!(
        "  Ready: {}\n",
        if report.is_ready() { "yes" } else { "no" }
    ));
    output.push_str("\nPlatforms:\n");
    if report.platform_counts.is_empty() {
        output.push_str("  none\n");
    } else {
        for (platform, count) in &report.platform_counts {
            output.push_str(&format!("  {platform}: {count}\n"));
        }
    }
    output.push_str("\nUnknown platform examples:\n");
    if report.unknown_platform_examples.is_empty() {
        output.push_str("  none\n");
    } else {
        for path in &report.unknown_platform_examples {
            output.push_str(&format!("  {}\n", path.display()));
        }
    }
    output
}

fn print_duplicate_report(report: &DuplicateReport) {
    print!("{}", format_duplicate_report(report));
}

fn build_duplicate_report(config: &Config) -> Result<DuplicateReport, ArchiveFsError> {
    let scanner = ArchiveScanner::new(config);
    let records = scanner.archive_records()?;
    FilenameDuplicateDetector.detect_duplicates(&records)
}

fn print_duplicate_report_json(report: &DuplicateReport) -> Result<(), serde_json::Error> {
    println!("{}", format_duplicate_report_json(report)?);
    Ok(())
}

fn format_duplicate_report_json(report: &DuplicateReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn format_duplicate_report(report: &DuplicateReport) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Duplicates\n\n");
    output.push_str("Summary:\n");
    output.push_str(&format!("  Records checked: {}\n", report.archives_checked));
    output.push_str(&format!(
        "  Duplicate groups found: {}\n",
        report.entries.len()
    ));

    if report.entries.is_empty() {
        output.push_str("\nNo duplicate candidates found.\n");
        return output;
    }

    output.push_str("\nDuplicate groups:\n");
    for (index, entry) in report.entries.iter().enumerate() {
        push_duplicate_entry(&mut output, index + 1, entry);
    }
    output
}

fn push_duplicate_entry(output: &mut String, index: usize, entry: &DuplicateEntry) {
    output.push_str(&format!("  Group {index}:\n"));
    output.push_str(&format!("    Platform: {}\n", entry.platform));
    output.push_str(&format!("    Severity: {}\n", entry.severity));
    output.push_str(&format!("    Reason: {}\n", entry.reason));
    output.push_str("    Archives:\n");
    for archive_path in &entry.archive_paths {
        output.push_str(&format!("      {}\n", archive_path.display()));
    }
}

fn print_archive_info(info: &ArchiveInfo) {
    print!("{}", format_archive_info(info));
}

fn print_archive_info_json(info: &ArchiveInfo) -> Result<(), serde_json::Error> {
    println!("{}", format_archive_info_json(info)?);
    Ok(())
}

fn format_archive_info_json(info: &ArchiveInfo) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(info)
}

fn format_archive_info(info: &ArchiveInfo) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Info\n\n");
    output.push_str("Details:\n");
    output.push_str(&format!("  Title: {}\n", info.title));
    output.push_str(&format!(
        "  Platform: {}\n",
        info.platform.as_deref().unwrap_or("Unknown")
    ));
    output.push_str(&format!(
        "  Archive path: {}\n",
        info.archive_path.display()
    ));
    output.push_str(&format!("  Mount path: {}\n", info.mount_path.display()));
    output.push_str(&format!("  Extension: {}\n", info.extension));
    output.push_str(&format!(
        "  Archive size: {}\n",
        info.size_bytes
            .map(human_size)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!(
        "  Last modified: {}\n",
        info.modified_time
            .map(format_system_time)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!("  Health: {}\n", info.health));
    output.push_str(&format!("  Mount state: {}\n", info.mount_state));
    output.push_str(&format!(
        "  Metadata provider: {}\n",
        info.metadata_provider
    ));
    output.push_str(&format!("  Health provider: {}\n", info.health_provider));
    output
}

fn print_archive_stats(stats: &ArchiveStats) {
    print!("{}", format_archive_stats(stats));
}

fn print_archive_stats_json(stats: &ArchiveStats) -> Result<(), serde_json::Error> {
    println!("{}", format_archive_stats_json(stats)?);
    Ok(())
}

fn format_archive_stats_json(stats: &ArchiveStats) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(stats)
}

fn format_archive_stats(stats: &ArchiveStats) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS Stats\n\n");
    output.push_str("Summary:\n");
    output.push_str(&format!("  Total archives: {}\n", stats.total_archives));
    output.push_str(&format!("  Mounted: {}\n", stats.mounted_count));
    output.push_str(&format!("  Pending: {}\n", stats.pending_count));
    output.push_str(&format!(
        "  Total archive size: {}\n",
        human_size(stats.total_size_bytes)
    ));
    output.push_str("\nPlatforms:\n");
    push_counts(&mut output, &stats.platform_counts);
    output.push_str("\nArchive extensions:\n");
    push_counts(&mut output, &stats.extension_counts);
    output.push_str("\nLargest archive:\n");
    push_archive_size(&mut output, stats.largest_archive.as_ref());
    output.push_str("\nSmallest archive:\n");
    push_archive_size(&mut output, stats.smallest_archive.as_ref());
    output
}

fn push_counts(output: &mut String, counts: &[(String, usize)]) {
    if counts.is_empty() {
        output.push_str("  none\n");
    } else {
        for (name, count) in counts {
            output.push_str(&format!("  {name}: {count}\n"));
        }
    }
}

fn push_archive_size(output: &mut String, archive: Option<&archivefs_core::ArchiveSizeSummary>) {
    if let Some(archive) = archive {
        output.push_str(&format!(
            "  {} ({})\n",
            archive.archive_path.display(),
            human_size(archive.size_bytes)
        ));
    } else {
        output.push_str("  none\n");
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn format_system_time(time: std::time::SystemTime) -> String {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format_unix_timestamp(duration.as_secs()),
        Err(error) => format!("before UNIX epoch by {}s", error.duration().as_secs()),
    }
}

fn format_unix_timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn print_statuses(statuses: &[ArchiveStatus]) {
    print!("{}", format_statuses(statuses));
}

fn format_statuses(statuses: &[ArchiveStatus]) -> String {
    let mut output = format!("{:<48}  {:<48}  State\n", "Archive", "Mount");
    for status in statuses {
        output.push_str(&format!(
            "{:<48}  {:<48}  {}\n",
            status.archive_path.display(),
            status.mount_path.display(),
            status.state
        ));
    }
    output
}

fn print_statuses_json(statuses: &[ArchiveStatus]) -> Result<(), serde_json::Error> {
    println!("{}", format_statuses_json(statuses)?);
    Ok(())
}

fn format_statuses_json(statuses: &[ArchiveStatus]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(statuses)
}

/// The exact one-line text `--version`/`-V` print - `env!("CARGO_PKG_VERSION")`
/// is a compile-time constant Cargo derives from this crate's resolved
/// `Cargo.toml` version (workspace inheritance included), so this never
/// invokes git, parses tags, reads a file at runtime, or duplicates the
/// version as a separate literal anywhere in this source.
fn version_line() -> String {
    format!("archivefs-cli {}", env!("CARGO_PKG_VERSION"))
}

fn print_version() {
    println!("{}", version_line());
}

fn print_help() {
    println!("archivefs [--verbose|-v] [--debug] <command>");
    println!();
    println!("Global flags:");
    println!("  -v, --verbose  Show operational logs");
    println!("  --debug        Show diagnostic logs");
    println!();
    println!("Commands:");
    println!("  scan           List supported archives from configured source folders");
    println!("  doctor         Check whether ArchiveFS is ready to run");
    println!("  config-check   Validate ArchiveFS configuration");
    println!("  pcsx2-patch-preview  Fetch and preview official PCSX2 patch metadata (read-only)");
    println!("  status         Show archive paths, mount paths, and mount states");
    println!("  stats          Show archive library counts and sizes");
    println!("  duplicates     Show filename-based duplicate candidates");
    println!("  info           Show details for one archive by path or name");
    println!("  mount          Mount scanned archives with ratarmount");
    println!("  mount-one      Mount one archive by path or name");
    println!("  unmount        Unmount ArchiveFS mountpoints under mount_root");
    println!("  unmount-one    Unmount one archive by path or name");
    println!("  clean          Remove empty directories under mount_root");
    println!("  watch          Watch source folders and refresh the JSON index");
    println!("  index-build    Build the JSON archive index");
    println!("  index-show     Show a summary of the JSON archive index");
    println!("  index-find     Find entries in the JSON archive index");
    println!("  library-status Show the persistent library database's health and counts");
    println!(
        "  health         Show catalogue archive health (missing, unknown platform) - no scan, mount, or unmount"
    );
    println!("  library-scan   Scan configured source folders into the library database");
    println!("  library-list   List archives from the library database (no rescan)");
    println!("  library-find   Search the library database by path or platform");
    println!(
        "  library-set-platform   Manually assign an archive's platform (outranks automatic detection)"
    );
    println!("  library-clear-platform Clear a manual platform assignment");
    println!(
        "  library-set-platform-bulk   Manually assign a platform to several archives at once"
    );
    println!(
        "  library-clear-platform-bulk Clear manual platform assignments from several archives at once"
    );
    println!(
        "  library-remove-missing Remove missing catalogue entries by exact id/path (never deletes files)"
    );
    println!("  platform-alias-list    List persistent custom folder-name platform aliases");
    println!(
        "  platform-alias-add     Add a custom folder-name platform alias (applies on next scan)"
    );
    println!("  platform-alias-remove   Remove a custom folder-name platform alias");
    println!("  sources                List configured source folders and their status");
    println!("  sources scan-all       Scan every enabled source folder independently");
    println!("  source add             Add a new source folder (validated, never auto-scanned)");
    println!("  source enable          Enable a source folder (scans it automatically)");
    println!("  source disable         Disable a source folder (catalogue preserved, no scan)");
    println!("  source scan            Scan one source folder by id or path");
    println!(
        "  source remove          Remove a source folder from configuration (--keep-catalogue or --remove-catalogue)"
    );
    println!("  view list              List configured Library Views");
    println!("  view preview           Show a Library View's plan without changing anything");
    println!("  view apply             Create/repair a Library View's managed symlinks");
    println!("  view repair            Same as apply - fixes drift and creates anything missing");
    println!(
        "  view remove            Remove a Library View's managed symlinks (--keep-definition to keep its config)"
    );
    println!();
    println!("Examples:");
    println!("  archivefs --version");
    println!("  archivefs doctor");
    println!("  archivefs config-check");
    println!("  archivefs pcsx2-patch-preview");
    println!("  archivefs pcsx2-patch-preview --json");
    println!("  archivefs status --json");
    println!("  archivefs stats");
    println!("  archivefs library-status");
    println!("  archivefs library-scan");
    println!("  archivefs library-list");
    println!("  archivefs library-list --unknown-only");
    println!("  archivefs library-find \"007 Legends\"");
    println!("  archivefs library-find --unknown-only n64");
    println!("  archivefs library-set-platform \"Luigi's Mansion\" GameCube");
    println!("  archivefs library-set-platform --id 42 GameCube");
    println!("  archivefs library-set-platform --path /roms/n64/Luigis_Mansion.zip GameCube");
    println!("  archivefs library-clear-platform \"Luigi's Mansion\"");
    println!("  archivefs library-set-platform-bulk --id 1 --id 2 --id 3 GameCube");
    println!(
        "  archivefs library-set-platform-bulk --path /roms/n64/a.zip --path /roms/n64/b.zip N64"
    );
    println!("  archivefs library-clear-platform-bulk --id 1 --id 2 --id 3");
    println!("  archivefs library-remove-missing --id 12");
    println!("  archivefs library-remove-missing --path /roms/missing.zip --id 13 --json");
    println!("  archivefs platform-alias-list");
    println!("  archivefs platform-alias-add gc GameCube");
    println!("  archivefs platform-alias-remove gc");
    println!("  archivefs sources");
    println!("  archivefs sources --json");
    println!("  archivefs sources scan-all");
    println!("  archivefs source add /mnt/usbdrive/retro");
    println!("  archivefs source enable /mnt/usbdrive/retro");
    println!("  archivefs source disable 3");
    println!("  archivefs source scan 3");
    println!("  archivefs source remove 3 --keep-catalogue");
    println!("  archivefs view list");
    println!("  archivefs view preview retrodeck");
    println!("  archivefs view apply retrodeck");
    println!("  archivefs view repair retrodeck --json");
    println!("  archivefs view remove retrodeck --keep-definition");
    println!("  archivefs stats --json");
    println!("  archivefs info \"007 Legends\"");
    println!("  archivefs mount-one \"007 Legends\"");
    println!("  archivefs unmount-one \"007 Legends\"");
    println!("  archivefs watch");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example config:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}

#[cfg(test)]
mod tests {
    use super::*;
    use archivefs_core::MANUAL_PLATFORM_SOURCE;
    use archivefs_core::patch_manager::{
        AdvisoryDisposition, AdvisoryPlanEntry, AdvisoryPlanSummary, DiscoveryConfidence,
        GameMatch, HypotheticalDestination, InstallationCandidate, MatchConfidence,
        PatchMetadataRecord, SourceSnapshot, VerificationLevel,
    };

    fn example_statuses() -> Vec<ArchiveStatus> {
        vec![
            ArchiveStatus {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
                state: archivefs_core::MountState::Mounted,
            },
            ArchiveStatus {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                mount_path: std::path::PathBuf::from("/mnt/archivefs/Unknown/Mystery"),
                state: archivefs_core::MountState::Pending,
            },
        ]
    }

    #[test]
    fn format_statuses_preserves_existing_human_output_exactly() {
        let output = format_statuses(&example_statuses());

        assert_eq!(
            output,
            concat!(
                "Archive                                           Mount                                             State\n",
                "/roms/Halo.zip                                    /mnt/archivefs/Xbox/Halo                          Mounted\n",
                "/roms/Mystery.7z                                  /mnt/archivefs/Unknown/Mystery                    Pending\n",
            )
        );
    }

    #[test]
    fn format_statuses_json_outputs_valid_pretty_json_with_expected_fields() {
        let output = format_statuses_json(&example_statuses()).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();
        let statuses = json.as_array().unwrap();

        assert!(output.starts_with("[\n"));
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0]["archive_path"], "/roms/Halo.zip");
        assert_eq!(statuses[0]["mount_path"], "/mnt/archivefs/Xbox/Halo");
        assert_eq!(statuses[0]["state"], "Mounted");
        assert_eq!(statuses[1]["state"], "Pending");
    }

    #[test]
    fn format_statuses_json_contains_no_human_heading() {
        let output = format_statuses_json(&example_statuses()).unwrap();

        assert!(!output.contains("Archive                                           Mount"));
        assert!(!output.contains("State\n"));
    }

    #[test]
    fn advisory_patch_preview_format_is_explicitly_non_executable_and_hypothetical() {
        let plan = AdvisoryPatchPlan {
            format_version: 1,
            plan_id: "plan-hash".to_string(),
            executable: false,
            source: SourceSnapshot {
                id: "source",
                display_name: "PCSX2 metadata",
                endpoint: "https://example.invalid/compiled-in",
                provenance: "fixture provenance",
                license_notice: "fixture license notice",
                metadata_schema: "fixture-schema-v1",
                source_version: "revision".to_string(),
                metadata_sha256: "metadata-hash".to_string(),
                verification: VerificationLevel::TransportOnly,
                verification_explanation: VerificationLevel::TransportOnly.explanation(),
                freshness_explanation: "No authenticated timestamp or monotonic version; first-seen replay cannot be detected",
            },
            installation_candidates: vec![InstallationCandidate {
                adapter_id: "pcsx2",
                kind: "Native".to_string(),
                data_root: PathBuf::from("/readonly/PCSX2"),
                provenance: "fixture root",
                discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
                detected_version: None,
                mutation_readiness: "NotEvaluated",
            }],
            entries: vec![AdvisoryPlanEntry {
                record: PatchMetadataRecord {
                    record_id: "patches/12345678.pnach".to_string(),
                    repository_path: "patches/12345678.pnach".to_string(),
                    patch_blob_id: "blob".to_string(),
                    title: None,
                    platform: "PS2".to_string(),
                    region: None,
                    serial: None,
                    executable_crc: Some("12345678".to_string()),
                    metadata_kind: "metadata".to_string(),
                },
                disposition: AdvisoryDisposition::MissingGame,
                game_match: GameMatch {
                    confidence: MatchConfidence::NoMatch,
                    catalogue_archive_ids: Vec::new(),
                    reasons: vec!["no compatible catalogue identity evidence".to_string()],
                },
                hypothetical_destinations: vec![HypotheticalDestination {
                    candidate_kind: "Native".to_string(),
                    relative_path: "patches/12345678.pnach".to_string(),
                    display_path: "/readonly/PCSX2/patches/12345678.pnach".to_string(),
                    hypothetical: true,
                }],
                reasons: vec!["metadata preview only; no PNACH content was downloaded".to_string()],
            }],
            summary: AdvisoryPlanSummary {
                metadata_records: 1,
                exact_matches: 0,
                probable_matches: 0,
                uncertain_matches: 0,
                ambiguous_matches: 0,
                missing_games: 1,
            },
        };

        let output = format_advisory_patch_plan(&plan);
        assert!(output.contains("Advisory only: yes (executable: false)"));
        assert!(output.contains("TransportOnly"));
        assert!(output.contains("not signed content"));
        assert!(output.contains("first-seen replay cannot be detected"));
        assert!(output.contains("match confidence: NoMatch"));
        assert!(output.contains("hypothetical PNACH destination (Native, not created)"));

        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["format_version"], 1);
        assert_eq!(json["executable"], false);
        assert_eq!(
            json["entries"][0]["hypothetical_destinations"][0]["hypothetical"],
            true
        );
    }

    fn golden_two_candidate_plan() -> AdvisoryPatchPlan {
        AdvisoryPatchPlan {
            format_version: 1,
            plan_id: "golden-plan-id".to_string(),
            executable: false,
            source: SourceSnapshot {
                id: "golden-source",
                display_name: "Golden PCSX2 metadata",
                endpoint: "https://golden.invalid/endpoint",
                provenance: "golden provenance",
                license_notice: "golden license notice",
                metadata_schema: "golden-schema-v1",
                source_version: "golden-revision".to_string(),
                metadata_sha256: "golden-metadata-hash".to_string(),
                verification: VerificationLevel::TransportOnly,
                verification_explanation: VerificationLevel::TransportOnly.explanation(),
                freshness_explanation:
                    "No authenticated timestamp or monotonic version; first-seen replay cannot be detected",
            },
            installation_candidates: vec![
                InstallationCandidate {
                    adapter_id: "pcsx2",
                    kind: "Native".to_string(),
                    data_root: PathBuf::from("/golden/home/.config/PCSX2"),
                    provenance: "golden native provenance",
                    discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
                    detected_version: None,
                    mutation_readiness: "NotEvaluated",
                },
                InstallationCandidate {
                    adapter_id: "pcsx2",
                    kind: "Flatpak".to_string(),
                    data_root: PathBuf::from("/golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2"),
                    provenance: "golden flatpak provenance",
                    discovery_confidence: DiscoveryConfidence::StandardPathCandidate,
                    detected_version: None,
                    mutation_readiness: "NotEvaluated",
                },
            ],
            entries: vec![AdvisoryPlanEntry {
                record: PatchMetadataRecord {
                    record_id: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
                    repository_path: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
                    patch_blob_id: "golden-blob-id".to_string(),
                    title: None,
                    platform: "PS2".to_string(),
                    region: None,
                    serial: Some("GOLD-00001".to_string()),
                    executable_crc: Some("DEADBEEF".to_string()),
                    metadata_kind: "golden fixture".to_string(),
                },
                disposition: AdvisoryDisposition::AmbiguousInstallationCandidates,
                game_match: GameMatch {
                    confidence: MatchConfidence::NoMatch,
                    catalogue_archive_ids: Vec::new(),
                    reasons: vec!["no compatible catalogue identity evidence".to_string()],
                },
                hypothetical_destinations: vec![
                    HypotheticalDestination {
                        candidate_kind: "Native".to_string(),
                        relative_path: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
                        display_path: "/golden/home/.config/PCSX2/patches/GOLD-00001_DEADBEEF.pnach"
                            .to_string(),
                        hypothetical: true,
                    },
                    HypotheticalDestination {
                        candidate_kind: "Flatpak".to_string(),
                        relative_path: "patches/GOLD-00001_DEADBEEF.pnach".to_string(),
                        display_path:
                            "/golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2/patches/GOLD-00001_DEADBEEF.pnach"
                                .to_string(),
                        hypothetical: true,
                    },
                ],
                reasons: vec![
                    "no compatible catalogue identity evidence".to_string(),
                    "metadata preview only; no PNACH content was downloaded".to_string(),
                    "multiple standard-path PCSX2 candidates were found; none was selected"
                        .to_string(),
                ],
            }],
            summary: AdvisoryPlanSummary {
                metadata_records: 1,
                exact_matches: 0,
                probable_matches: 0,
                uncertain_matches: 0,
                ambiguous_matches: 0,
                missing_games: 1,
            },
        }
    }

    #[test]
    fn advisory_patch_preview_exact_human_output_for_a_fixed_native_and_flatpak_plan() {
        let output = format_advisory_patch_plan(&golden_two_candidate_plan());
        assert_eq!(
            output,
            concat!(
                "ArchiveFS PCSX2 Patch Metadata Preview\n",
                "Advisory only: yes (executable: false)\n",
                "Plan format: 1\n",
                "Plan ID: golden-plan-id\n",
                "Source: Golden PCSX2 metadata\n",
                "Endpoint: https://golden.invalid/endpoint\n",
                "Provenance: golden provenance\n",
                "License: golden license notice\n",
                "Metadata schema: golden-schema-v1\n",
                "Source version: golden-revision\n",
                "Metadata SHA-256: golden-metadata-hash\n",
                "Verification: TransportOnly\n",
                "Verification detail: HTTPS transport verified; downloaded metadata is not signed content\n",
                "Freshness: No authenticated timestamp or monotonic version; first-seen replay cannot be detected\n",
                "\n",
                "PCSX2 candidates: 2\n",
                "  Native: /golden/home/.config/PCSX2 (golden native provenance, confidence: StandardPathCandidate, version: not inspected, mutation readiness: NotEvaluated)\n",
                "  Flatpak: /golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2 (golden flatpak provenance, confidence: StandardPathCandidate, version: not inspected, mutation readiness: NotEvaluated)\n",
                "\n",
                "Records: 1 | exact: 0 | probable: 0 | uncertain: 0 | ambiguous: 0 | no match: 1\n",
                "\n",
                "patches/GOLD-00001_DEADBEEF.pnach\n",
                "  disposition: AmbiguousInstallationCandidates\n",
                "  match confidence: NoMatch\n",
                "  catalogue archive IDs: []\n",
                "  identity: serial=GOLD-00001 executable_crc=DEADBEEF\n",
                "  reason: no compatible catalogue identity evidence\n",
                "  reason: metadata preview only; no PNACH content was downloaded\n",
                "  reason: multiple standard-path PCSX2 candidates were found; none was selected\n",
                "  hypothetical PNACH destination (Native, not created): /golden/home/.config/PCSX2/patches/GOLD-00001_DEADBEEF.pnach\n",
                "  hypothetical PNACH destination (Flatpak, not created): /golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2/patches/GOLD-00001_DEADBEEF.pnach\n",
            )
        );

        // Native listed (and its hypothetical destination rendered) before
        // Flatpak, matching the candidate order this fixed plan was given.
        let native_index = output.find("Native: /golden").unwrap();
        let flatpak_index = output.find("Flatpak: /golden").unwrap();
        assert!(native_index < flatpak_index);
    }

    #[test]
    fn advisory_patch_preview_json_candidate_object_has_the_complete_v1_shape() {
        let plan = golden_two_candidate_plan();
        let json = serde_json::to_value(&plan).unwrap();

        assert_eq!(
            json["installation_candidates"],
            serde_json::json!([
                {
                    "kind": "Native",
                    "data_root": "/golden/home/.config/PCSX2",
                    "provenance": "golden native provenance",
                    "discovery_confidence": "StandardPathCandidate",
                    "detected_version": null,
                    "mutation_readiness": "NotEvaluated",
                },
                {
                    "kind": "Flatpak",
                    "data_root": "/golden/home/.var/app/net.pcsx2.PCSX2/config/PCSX2",
                    "provenance": "golden flatpak provenance",
                    "discovery_confidence": "StandardPathCandidate",
                    "detected_version": null,
                    "mutation_readiness": "NotEvaluated",
                },
            ]),
            "the complete format_version=1 installation_candidates shape - field \
             names, field count, string values, and order - must match exactly"
        );
    }

    #[test]
    fn format_doctor_report_preserves_human_output_shape() {
        let report = DoctorReport {
            config_path: std::path::PathBuf::from("/home/user/.config/archivefs/config.toml"),
            checks: vec![archivefs_core::DoctorCheck {
                name: "config".to_string(),
                status: archivefs_core::DoctorStatus::Pass,
                detail: "configuration loaded".to_string(),
            }],
            archives_found: 3,
            archives_with_platform: 2,
            archives_unknown_platform: 1,
            unknown_platform_examples: vec![std::path::PathBuf::from("/roms/Unknown.zip")],
            platform_counts: vec![("Xbox360".to_string(), 2)],
            pending_archives: 2,
            mounted_archives: 1,
        };

        let output = format_doctor_report(&report);

        assert!(output.contains("ArchiveFS Doctor"));
        assert!(output.contains("Config: /home/user/.config/archivefs/config.toml"));
        assert!(output.contains("Checks:"));
        assert!(output.contains("[PASS] config"));
        assert!(output.contains("Summary:"));
        assert!(output.contains("Archives found: 3"));
        assert!(output.contains("Ready: yes"));
        assert!(output.contains("Platforms:"));
        assert!(output.contains("Xbox360: 2"));
        assert!(output.contains("Unknown platform examples:"));
        assert!(output.contains("/roms/Unknown.zip"));
    }

    #[test]
    fn format_doctor_report_json_outputs_pretty_json_only() {
        let report = DoctorReport {
            config_path: std::path::PathBuf::from("/home/user/.config/archivefs/config.toml"),
            checks: vec![archivefs_core::DoctorCheck {
                name: "config".to_string(),
                status: archivefs_core::DoctorStatus::Warn,
                detail: "configuration has warnings".to_string(),
            }],
            archives_found: 3,
            archives_with_platform: 2,
            archives_unknown_platform: 1,
            unknown_platform_examples: vec![std::path::PathBuf::from("/roms/Unknown.zip")],
            platform_counts: vec![("Xbox360".to_string(), 2)],
            pending_archives: 2,
            mounted_archives: 1,
        };

        let output = format_doctor_report_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Doctor"));
        assert!(!output.contains("Summary:"));
        assert_eq!(
            json["config_path"],
            "/home/user/.config/archivefs/config.toml"
        );
        assert_eq!(json["checks"][0]["name"], "config");
        assert_eq!(json["checks"][0]["status"], "Warn");
        assert_eq!(json["archives_found"], 3);
        assert_eq!(json["archives_with_platform"], 2);
        assert_eq!(json["archives_unknown_platform"], 1);
        assert_eq!(json["unknown_platform_examples"][0], "/roms/Unknown.zip");
        assert_eq!(json["platform_counts"][0][0], "Xbox360");
        assert_eq!(json["platform_counts"][0][1], 2);
        assert_eq!(json["pending_archives"], 2);
        assert_eq!(json["mounted_archives"], 1);
    }

    #[test]
    fn format_duplicate_report_shows_friendly_empty_message() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: Vec::new(),
        };

        let output = format_duplicate_report(&report);

        assert!(output.contains("ArchiveFS Duplicates"));
        assert!(output.contains("Records checked: 2"));
        assert!(output.contains("Duplicate groups found: 0"));
        assert!(output.contains("No duplicate candidates found."));
    }

    #[test]
    fn format_duplicate_report_json_outputs_pretty_json_only() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name '007_legends' on platform 'Xbox360'"
                    .to_string(),
                archive_paths: vec![
                    std::path::PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    std::path::PathBuf::from("/roms/imports/007 Legends.7z"),
                ],
            }],
        };

        let output = format_duplicate_report_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Duplicates"));
        assert!(!output.contains("Summary:"));
        assert_eq!(json["detector"], "filename");
        assert_eq!(json["archives_checked"], 2);
        assert_eq!(json["entries"].as_array().unwrap().len(), 1);
        assert_eq!(json["entries"][0]["platform"], "Xbox360");
        assert_eq!(json["entries"][0]["severity"], "Warning");
        assert_eq!(
            json["entries"][0]["archive_paths"][0],
            "/roms/xbox360/007 Legends.zip"
        );
        assert_eq!(
            json["entries"][0]["archive_paths"][1],
            "/roms/imports/007 Legends.7z"
        );
    }

    #[test]
    fn format_duplicate_report_shows_group_details() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name '007_legends' on platform 'Xbox360'"
                    .to_string(),
                archive_paths: vec![
                    std::path::PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    std::path::PathBuf::from("/roms/imports/007 Legends.7z"),
                ],
            }],
        };

        let output = format_duplicate_report(&report);

        assert!(output.contains("Records checked: 2"));
        assert!(output.contains("Duplicate groups found: 1"));
        assert!(output.contains("Group 1:"));
        assert!(output.contains("Platform: Xbox360"));
        assert!(output.contains("Severity: Warning"));
        assert!(output.contains("007_legends"));
        assert!(output.contains("/roms/xbox360/007 Legends.zip"));
        assert!(output.contains("/roms/imports/007 Legends.7z"));
    }

    #[test]
    fn duplicate_human_output_remains_exactly_compatible() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name '007_legends' on platform 'Xbox360'"
                    .to_string(),
                archive_paths: vec![
                    PathBuf::from("/roms/xbox360/007 Legends.zip"),
                    PathBuf::from("/roms/imports/007 Legends.7z"),
                ],
            }],
        };

        assert_eq!(
            format_duplicate_report(&report),
            concat!(
                "ArchiveFS Duplicates\n\n",
                "Summary:\n",
                "  Records checked: 2\n",
                "  Duplicate groups found: 1\n",
                "\nDuplicate groups:\n",
                "  Group 1:\n",
                "    Platform: Xbox360\n",
                "    Severity: Warning\n",
                "    Reason: same normalized archive name '007_legends' on platform 'Xbox360'\n",
                "    Archives:\n",
                "      /roms/xbox360/007 Legends.zip\n",
                "      /roms/imports/007 Legends.7z\n",
            )
        );
    }

    #[test]
    fn duplicate_json_keeps_the_existing_top_level_and_entry_fields() {
        let report = DuplicateReport {
            detector: "filename".to_string(),
            archives_checked: 2,
            entries: vec![DuplicateEntry {
                platform: "Xbox360".to_string(),
                severity: archivefs_core::DuplicateSeverity::Warning,
                reason: "same normalized archive name 'game' on platform 'Xbox360'".to_string(),
                archive_paths: vec![PathBuf::from("/a/Game.zip"), PathBuf::from("/b/Game.7z")],
            }],
        };
        let json: serde_json::Value =
            serde_json::from_str(&format_duplicate_report_json(&report).unwrap()).unwrap();
        let mut top_level = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        top_level.sort();
        let mut entry = json["entries"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        entry.sort();

        assert_eq!(top_level, ["archives_checked", "detector", "entries"]);
        assert_eq!(entry, ["archive_paths", "platform", "reason", "severity"]);
    }

    #[test]
    fn duplicate_report_reads_archives_without_writing_files_or_catalogue() {
        let root = temp_dir("duplicates-read-only");
        let source = root.join("source");
        let mount = root.join("mount");
        let first = write_archive_file(&source, "Game.zip", b"first archive");
        let second = write_archive_file(&source, "Game.7z", b"second archive");
        let config = config_for(&source, &mount);
        let before = [
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap(),
        ];

        let report = build_duplicate_report(&config).unwrap();

        assert_eq!(report.archives_checked, 2);
        assert_eq!(std::fs::read(&first).unwrap(), before[0]);
        assert_eq!(std::fs::read(&second).unwrap(), before[1]);
        assert!(!root.join("library.sqlite3").exists());
        assert!(!mount.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn format_archive_info_includes_all_display_fields() {
        let info = ArchiveInfo {
            title: "Halo".to_string(),
            platform: Some("Xbox".to_string()),
            archive_path: std::path::PathBuf::from("/roms/xbox/Halo.zip"),
            mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
            extension: "zip".to_string(),
            size_bytes: Some(2048),
            modified_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
            health: archivefs_core::ArchiveHealth::Pending,
            mount_state: archivefs_core::MountState::Mounted,
            metadata_provider: "FilenameMetadataProvider".to_string(),
            health_provider: "FilesystemHealthProvider".to_string(),
        };

        let output = format_archive_info(&info);

        assert!(output.contains("Title: Halo"));
        assert!(output.contains("Platform: Xbox"));
        assert!(output.contains("Archive path: /roms/xbox/Halo.zip"));
        assert!(output.contains("Mount path: /mnt/archivefs/Xbox/Halo"));
        assert!(output.contains("Extension: zip"));
        assert!(output.contains("Archive size: 2.0 KiB"));
        assert!(output.contains("Last modified: 1970-01-02 00:00:00 UTC"));
        assert!(output.contains("Health: Pending"));
        assert!(output.contains("Mount state: Mounted"));
        assert!(output.contains("Metadata provider: FilenameMetadataProvider"));
        assert!(output.contains("Health provider: FilesystemHealthProvider"));
    }

    #[test]
    fn format_archive_info_json_outputs_expected_fields_without_headings() {
        let info = ArchiveInfo {
            title: "Halo".to_string(),
            platform: Some("Xbox".to_string()),
            archive_path: std::path::PathBuf::from("/roms/xbox/Halo.zip"),
            mount_path: std::path::PathBuf::from("/mnt/archivefs/Xbox/Halo"),
            extension: "zip".to_string(),
            size_bytes: Some(2048),
            modified_time: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
            health: archivefs_core::ArchiveHealth::Pending,
            mount_state: archivefs_core::MountState::Mounted,
            metadata_provider: "FilenameMetadataProvider".to_string(),
            health_provider: "FilesystemHealthProvider".to_string(),
        };

        let output = format_archive_info_json(&info).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Info"));
        assert!(!output.contains("Details:"));
        assert_eq!(json["title"], "Halo");
        assert_eq!(json["platform"], "Xbox");
        assert_eq!(json["archive_path"], "/roms/xbox/Halo.zip");
        assert_eq!(json["mount_path"], "/mnt/archivefs/Xbox/Halo");
        assert_eq!(json["extension"], "zip");
        assert_eq!(json["size_bytes"], 2048);
        assert_eq!(json["modified_time"], 86_400);
        assert_eq!(json["health"], "Pending");
        assert_eq!(json["mount_state"], "Mounted");
        assert_eq!(json["metadata_provider"], "FilenameMetadataProvider");
        assert_eq!(json["health_provider"], "FilesystemHealthProvider");
    }

    #[test]
    fn format_archive_stats_json_outputs_pretty_json_only() {
        let stats = ArchiveStats {
            total_archives: 2,
            mounted_count: 1,
            pending_count: 1,
            platform_counts: vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 1)],
            extension_counts: vec![("7z".to_string(), 1), ("zip".to_string(), 1)],
            largest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                size_bytes: 2048,
            }),
            smallest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                size_bytes: 512,
            }),
            total_size_bytes: 2560,
        };

        let output = format_archive_stats_json(&stats).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert!(!output.contains("ArchiveFS Stats"));
        assert_eq!(json["total_archives"], 2);
        assert_eq!(json["mounted_count"], 1);
        assert_eq!(json["pending_count"], 1);
        assert_eq!(json["total_size_bytes"], 2560);
        assert_eq!(json["platform_counts"]["Unknown"], 1);
        assert_eq!(json["platform_counts"]["Xbox360"], 1);
        assert_eq!(json["extension_counts"]["7z"], 1);
        assert_eq!(json["extension_counts"]["zip"], 1);
        assert_eq!(json["largest_archive"]["archive_path"], "/roms/Halo.zip");
        assert_eq!(json["smallest_archive"]["size_bytes"], 512);
    }

    #[test]
    fn format_archive_stats_includes_counts_and_sizes() {
        let stats = ArchiveStats {
            total_archives: 2,
            mounted_count: 1,
            pending_count: 1,
            platform_counts: vec![("Unknown".to_string(), 1), ("Xbox360".to_string(), 1)],
            extension_counts: vec![("7z".to_string(), 1), ("zip".to_string(), 1)],
            largest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Halo.zip"),
                size_bytes: 2048,
            }),
            smallest_archive: Some(archivefs_core::ArchiveSizeSummary {
                archive_path: std::path::PathBuf::from("/roms/Mystery.7z"),
                size_bytes: 512,
            }),
            total_size_bytes: 2560,
        };

        let output = format_archive_stats(&stats);

        assert!(output.contains("Total archives: 2"));
        assert!(output.contains("Mounted: 1"));
        assert!(output.contains("Pending: 1"));
        assert!(output.contains("Total archive size: 2.5 KiB"));
        assert!(output.contains("  Xbox360: 1"));
        assert!(output.contains("  zip: 1"));
        assert!(output.contains("/roms/Halo.zip (2.0 KiB)"));
        assert!(output.contains("/roms/Mystery.7z (512 B)"));
    }

    #[test]
    fn parse_cli_args_defaults_to_quiet_help() {
        let args = parse_cli_args(Vec::<String>::new());

        assert_eq!(args.log_level, log::LevelFilter::Off);
        assert_eq!(args.command, "help");
        assert!(args.args.is_empty());
    }

    #[test]
    fn parse_cli_args_accepts_verbose_flag() {
        let args = parse_cli_args(["-v", "scan"].into_iter().map(str::to_string));

        assert_eq!(args.log_level, log::LevelFilter::Info);
        assert_eq!(args.command, "scan");
    }

    #[test]
    fn parse_cli_args_accepts_debug_flag_and_preserves_command_args() {
        let args = parse_cli_args(
            ["--debug", "mount-one", "Test", "Game"]
                .into_iter()
                .map(str::to_string),
        );

        assert_eq!(args.log_level, log::LevelFilter::Debug);
        assert_eq!(args.command, "mount-one");
        assert_eq!(args.args, vec!["Test".to_string(), "Game".to_string()]);
    }

    // -------------------------------------------------------------
    // --version / -V
    // -------------------------------------------------------------

    #[test]
    fn parse_cli_args_recognizes_long_version_flag() {
        let args = parse_cli_args(["--version"].into_iter().map(str::to_string));

        assert_eq!(args.command, "--version");
        assert!(args.args.is_empty());
    }

    #[test]
    fn parse_cli_args_recognizes_short_version_flag() {
        let args = parse_cli_args(["-V"].into_iter().map(str::to_string));

        assert_eq!(args.command, "-V");
        assert!(args.args.is_empty());
    }

    #[test]
    fn parse_cli_args_still_recognizes_help_flags() {
        // Unaffected by adding --version/-V: parse_cli_args itself was
        // not changed, only a new match arm in run().
        assert_eq!(
            parse_cli_args(["--help"].into_iter().map(str::to_string)).command,
            "--help"
        );
        assert_eq!(
            parse_cli_args(["-h"].into_iter().map(str::to_string)).command,
            "-h"
        );
    }

    #[test]
    fn parse_cli_args_leaves_ordinary_commands_unaffected() {
        let args = parse_cli_args(["scan"].into_iter().map(str::to_string));

        assert_eq!(args.command, "scan");
        assert!(args.args.is_empty());
    }

    #[test]
    fn version_flag_trailing_extra_command_is_ignored_deterministically() {
        // "version wins and exits": documented at the run() match arm.
        // --version is the command token, and the trailing "library-list"
        // is simply never read - print_version never touches cli.args -
        // exactly like --help already behaves with trailing garbage
        // today. The parse itself is deterministic either way.
        let args = parse_cli_args(
            ["--version", "library-list"]
                .into_iter()
                .map(str::to_string),
        );

        assert_eq!(args.command, "--version");
        assert_eq!(args.args, vec!["library-list".to_string()]);
    }

    #[test]
    fn version_flag_after_a_command_is_not_treated_as_a_global_request() {
        // --version/-V are recognised only in the command position - the
        // same scope --help/-h already have. Here "library-list" is the
        // command, and "--version" is just an (unused-by-library-list)
        // trailing argument, not a version request.
        let args = parse_cli_args(
            ["library-list", "--version"]
                .into_iter()
                .map(str::to_string),
        );

        assert_eq!(args.command, "library-list");
        assert_eq!(args.args, vec!["--version".to_string()]);
    }

    #[test]
    fn version_line_contains_the_cargo_package_version() {
        assert!(version_line().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn version_line_is_exactly_one_concise_line() {
        let line = version_line();

        assert!(!line.contains('\n'));
        assert_eq!(line, format!("archivefs-cli {}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn version_output_requires_no_config_or_database_access() {
        // version_line is a pure function of compile-time constants only
        // - no Config::load_default, no default_database_path, no
        // filesystem or database I/O. Every other command's tests in
        // this file set up a temp_dir/config_for/database_path first;
        // this one deliberately does not, proving by construction that
        // none of that is needed here.
        assert!(!version_line().is_empty());
    }

    #[test]
    fn all_workspace_crates_resolve_to_the_same_version() {
        // Asks Cargo itself, rather than parsing Cargo.toml text: `cargo
        // metadata` performs the same workspace-inheritance resolution
        // (`version.workspace = true`) that a real build uses, so this
        // can't be fooled by formatting/whitespace and needs no ad-hoc
        // TOML string matching. This shells out to `cargo` from a test
        // (not from the shipped binary), so it doesn't conflict with
        // version_output_requires_no_config_or_database_access above or
        // with print_version() avoiding runtime Cargo.toml/git access.
        let workspace_manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");

        let output = std::process::Command::new(env!("CARGO"))
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .arg("--manifest-path")
            .arg(&workspace_manifest)
            .output()
            .expect("failed to run `cargo metadata`");
        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let metadata: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("cargo metadata did not print JSON");

        let mut versions = std::collections::BTreeMap::new();
        for pkg in metadata["packages"]
            .as_array()
            .expect("metadata.packages should be an array")
        {
            let name = pkg["name"].as_str().expect("package.name");
            if matches!(name, "archivefs-core" | "archivefs-cli" | "archivefs-gui") {
                let version = pkg["version"]
                    .as_str()
                    .expect("package.version")
                    .to_string();
                versions.insert(name.to_string(), version);
            }
        }

        assert_eq!(
            versions.len(),
            3,
            "expected archivefs-core, archivefs-cli and archivefs-gui in `cargo metadata` output, got: {versions:?}"
        );

        let distinct: std::collections::BTreeSet<&String> = versions.values().collect();
        assert_eq!(
            distinct.len(),
            1,
            "workspace crates report different versions: {versions:?}"
        );
    }

    // -------------------------------------------------------------
    // library-status / library-scan / library-list / library-find
    //
    // All of these call the testable core functions
    // (build_library_status_view / run_library_scan / build_library_entries
    // / filter_library_entries) directly with explicit temp paths, exactly
    // like archivefs_core's own database tests - never Config::load_default
    // or default_database_path, so nothing here touches the real $HOME or
    // races other tests over process-wide environment variables.
    // -------------------------------------------------------------

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("archivefs-cli-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_archive_file(dir: &Path, relative_path: &str, content: &[u8]) -> PathBuf {
        let full_path = dir.join(relative_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full_path, content).unwrap();
        full_path
    }

    fn config_for(source_dir: &Path, mount_dir: &Path) -> Config {
        Config {
            source_folders: vec![source_dir.to_path_buf()],
            mount_root: mount_dir.to_path_buf(),
            ratarmount_bin: "ratarmount".to_string(),
        }
    }

    #[test]
    fn library_status_reports_no_database_before_any_scan() {
        let root = temp_dir("status-no-database");
        let database_path = root.join("library.sqlite3");

        let view = build_library_status_view(&database_path);

        assert!(!view.health.database_exists);
        assert!(!view.health.database_opens);
        assert!(view.stats.is_none());
        assert!(view.last_completed_scan.is_none());
        assert!(
            !database_path.exists(),
            "a status check must never create the database"
        );

        let output = format_library_status(&view);
        assert!(output.contains("Exists: no"));
        assert!(output.contains("No library database yet. Run: archivefs-cli library-scan"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_status_reports_counts_after_a_successful_scan() {
        let root = temp_dir("status-after-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);

        run_library_scan(&config, &database_path, "test").unwrap();
        let view = build_library_status_view(&database_path);

        assert!(view.health.database_exists);
        assert!(view.health.database_opens);
        assert!(view.health.migrations_current);
        assert!(view.health.foreign_keys_enabled);
        let stats = view
            .stats
            .as_ref()
            .expect("stats must be present once migrations are current");
        assert_eq!(stats.total_archives, 1);
        assert_eq!(stats.present_archives, 1);
        assert_eq!(stats.archives_with_platform, 1);
        let scan = view
            .last_completed_scan
            .as_ref()
            .expect("a completed scan must be reported");
        assert_eq!(scan.archives_added, 1);

        let output = format_library_status(&view);
        assert!(output.contains("Total: 1"));
        assert!(output.contains("Present: 1"));
        assert!(output.contains("Detected platform: 1"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_status_json_parses_and_contains_expected_fields() {
        let root = temp_dir("status-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let view = build_library_status_view(&database_path);
        let output = format_library_status_json(&view).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert_eq!(json["database_exists"], true);
        assert_eq!(json["schema_version"], latest_schema_version());
        assert_eq!(json["migrations_current"], true);
        assert_eq!(json["stats"]["total_archives"], 1);
        assert_eq!(json["last_completed_scan"]["archives_added"], 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_creates_the_database() {
        let root = temp_dir("scan-creates-database");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        assert!(!database_path.exists());

        let report = run_library_scan(&config, &database_path, "test").unwrap();

        assert!(database_path.exists());
        assert_eq!(report.archives_new, 1);
        assert_eq!(report.source_folders_attempted, 1);
        assert_eq!(report.source_folders_succeeded, 1);
        assert_eq!(report.source_folders_failed, 0);
        assert!(report.folder_errors.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_reports_partial_source_folder_failure() {
        let root = temp_dir("scan-partial-failure");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source_a, "a.zip", b"a");
        write_archive_file(&source_b, "b.zip", b"b");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        run_library_scan(&config, &database_path, "test").unwrap();

        std::fs::remove_dir_all(&source_a).unwrap();
        let report = run_library_scan(&config, &database_path, "test").unwrap();

        assert_eq!(report.source_folders_attempted, 2);
        assert_eq!(report.source_folders_succeeded, 1);
        assert_eq!(report.source_folders_failed, 1);
        assert_eq!(report.folder_errors.len(), 1);
        assert_eq!(report.folder_errors[0].path, source_a);

        let output = format_library_scan(&report);
        assert!(output.contains("Attempted: 2"));
        assert!(output.contains("Succeeded: 1"));
        assert!(output.contains("Failed: 1"));
        assert!(output.contains(&source_a.display().to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_scan_json_parses_and_contains_expected_fields() {
        let root = temp_dir("scan-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);

        let report = run_library_scan(&config, &database_path, "test").unwrap();
        let output = format_library_scan_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("{\n"));
        assert_eq!(json["archives_new"], 1);
        assert_eq!(json["source_folders_succeeded"], 1);
        assert_eq!(json["folder_errors"].as_array().unwrap().len(), 0);
        let object = json.as_object().unwrap();
        for field in [
            "scan_run_id",
            "source_folders_attempted",
            "source_folders_succeeded",
            "source_folders_failed",
            "archives_new",
            "archives_changed",
            "archives_restored",
            "archives_unchanged",
            "archives_missing",
            "folder_errors",
        ] {
            assert!(
                object.contains_key(field),
                "existing JSON field {field} must remain"
            );
        }
        assert_eq!(object.len(), 10, "scan JSON compatibility surface changed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_shows_present_and_missing_rows() {
        let root = temp_dir("list-present-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "keep.zip", b"a");
        let doomed = write_archive_file(&source, "gone.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        std::fs::remove_file(&doomed).unwrap();
        run_library_scan(&config, &database_path, "test").unwrap();

        let entries = build_library_entries(&database_path, false).unwrap();

        assert_eq!(entries.len(), 2);
        let keep = entries
            .iter()
            .find(|entry| entry.path.ends_with("keep.zip"))
            .unwrap();
        let gone = entries
            .iter()
            .find(|entry| entry.path.ends_with("gone.zip"))
            .unwrap();
        assert!(keep.present);
        assert!(!gone.present);

        let output = format_library_entries(&entries);
        assert!(output.contains("State: Present"));
        assert!(output.contains("State: Missing"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_with_no_database_is_an_empty_but_successful_result() {
        let root = temp_dir("list-no-database");
        let database_path = root.join("library.sqlite3");

        let entries = build_library_entries(&database_path, false).unwrap();

        assert!(entries.is_empty());
        assert!(!database_path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_unknown_only_returns_only_unknown_rows() {
        let root = temp_dir("list-unknown-only");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"a"); // unknown
        write_archive_file(&source, "msx2/game.zip", b"b"); // automatic MSX2
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let all_entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(all_entries.len(), 2, "sanity check: both archives present");

        let unknown_entries = build_library_entries(&database_path, true).unwrap();

        assert_eq!(unknown_entries.len(), 1);
        assert!(unknown_entries[0].path.ends_with("mystery.zip"));
        assert!(unknown_entries[0].platform.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_unknown_only_excludes_known_manual_and_automatic_rows() {
        let root = temp_dir("list-unknown-only-excludes-known");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"a");
        write_archive_file(&source, "msx2/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let mystery_id = build_library_entries(&database_path, false)
            .unwrap()
            .iter()
            .find(|entry| entry.path.ends_with("mystery.zip"))
            .unwrap()
            .id;
        run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Id(mystery_id),
            "GameCube",
        )
        .unwrap();

        let unknown_entries = build_library_entries(&database_path, true).unwrap();

        assert!(
            unknown_entries.is_empty(),
            "both rows are now known (one manual, one automatic) - neither should appear"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_unknown_only_includes_missing_rows() {
        let root = temp_dir("list-unknown-only-includes-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let doomed = write_archive_file(&source, "mystery.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        std::fs::remove_file(&doomed).unwrap();
        run_library_scan(&config, &database_path, "test").unwrap();

        let unknown_entries = build_library_entries(&database_path, true).unwrap();

        assert_eq!(unknown_entries.len(), 1);
        assert!(
            !unknown_entries[0].present,
            "missing unknown rows must still be included"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_unknown_only_with_no_database_is_an_empty_successful_result() {
        let root = temp_dir("list-unknown-only-no-database");
        let database_path = root.join("library.sqlite3");

        let entries = build_library_entries(&database_path, true).unwrap();

        assert!(entries.is_empty());
        assert!(!database_path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_list_unknown_only_json_output_shape_matches_normal_output() {
        let root = temp_dir("list-unknown-only-json-shape");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"a");
        write_archive_file(&source, "msx2/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let unknown_entries = build_library_entries(&database_path, true).unwrap();
        let output = format_library_entries_json(&unknown_entries).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("[\n"));
        assert_eq!(json.as_array().unwrap().len(), 1);
        let entry = &json[0];
        assert!(entry.get("id").is_some());
        assert!(entry.get("path").is_some());
        assert!(entry.get("platform").is_some());
        assert!(entry.get("platform_source").is_some());
        assert!(entry.get("present").is_some());
        assert!(entry.get("size_bytes").is_some());
        assert!(entry.get("modified_time_unix_seconds").is_some());
        assert_eq!(entry["platform"], serde_json::Value::Null);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_unknown_only_combines_with_the_text_query() {
        let root = temp_dir("find-unknown-only");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery-game.zip", b"a"); // no folder hint: unknown
        write_archive_file(&source, "msx2/mystery-game.zip", b"b"); // automatic MSX2: known
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let entries = build_library_entries(&database_path, true).unwrap();
        let matches = filter_library_entries(&entries, "mystery-game");

        assert_eq!(matches.len(), 1);
        assert!(
            matches[0].path.ends_with("mystery-game.zip")
                && !matches[0].path.ends_with("msx2/mystery-game.zip")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_matches_case_insensitively_on_path() {
        let root = temp_dir("find-path-match");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Halo.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();

        let matches = filter_library_entries(&entries, "HALO");

        assert_eq!(matches.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_matches_on_platform() {
        let root = temp_dir("find-platform-match");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();

        let matches = filter_library_entries(&entries, "xbox360");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].platform.as_deref(), Some("Xbox360"));

        let output = print_library_find_results_for_test("xbox360", &matches);
        assert!(output.contains("Query: xbox360"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_find_returns_no_results_without_erroring() {
        let entries: Vec<LibraryArchiveView> = Vec::new();
        let matches = filter_library_entries(&entries, "nothing-will-match-this");

        assert!(matches.is_empty());
    }

    #[test]
    fn library_find_json_parses_and_round_trips_expected_fields() {
        let root = temp_dir("find-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let matches = filter_library_entries(&entries, "game");

        let output = format_library_entries_json(&matches).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();

        assert!(output.starts_with("[\n"));
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["present"], true);
        assert_eq!(json[0]["platform"], "Xbox360");
        assert!(
            json[0]["path"]
                .as_str()
                .unwrap()
                .ends_with("Xbox360/game.zip")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_platform_is_shown_as_unknown_not_a_stored_sentinel() {
        let root = temp_dir("unknown-platform-cli");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].platform, None,
            "an undetected platform must round-trip as None, not a sentinel string"
        );

        let output = format_library_entries(&entries);
        assert!(output.contains("Platform: Unknown"));

        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // v0.4.3-alpha: `health` / `health --json` - catalogue-only,
    // never-scans health report.
    // -----------------------------------------------------------------

    #[test]
    fn health_report_is_empty_for_a_freshly_scanned_healthy_library() {
        let root = temp_dir("health-empty");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let database = Database::open_or_create(&database_path).unwrap();
        let archives = database.load_archives().unwrap();
        let report = catalogue_health_report(&archives);

        assert_eq!(report.archives_checked, 1);
        assert_eq!(report.missing_count, 0);
        assert_eq!(report.unknown_platform_count, 0);
        assert!(report.issues.is_empty());
        assert!(format_health_report(&report).contains("No archive health issues were found."));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn health_report_json_round_trips_missing_and_unknown_platform() {
        let root = temp_dir("health-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let disappearing = write_archive_file(&source, "Xbox360/game.zip", b"contents");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        std::fs::remove_file(&disappearing).unwrap();
        run_library_scan(&config, &database_path, "test").unwrap();

        let database = Database::open_or_create(&database_path).unwrap();
        let archives = database.load_archives().unwrap();
        let report = catalogue_health_report(&archives);

        assert_eq!(report.archives_checked, 2);
        assert_eq!(report.missing_count, 1);
        assert_eq!(report.unknown_platform_count, 1);
        assert_eq!(report.issues.len(), 2);

        let output = format_health_report_json(&report).unwrap();
        let json = serde_json::from_str::<serde_json::Value>(&output).unwrap();
        assert_eq!(json["missing_count"], 1);
        assert_eq!(json["unknown_platform_count"], 1);
        assert_eq!(json["issues"].as_array().unwrap().len(), 2);
        assert!(
            json["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["category"] == "Missing")
        );
        assert!(
            json["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|issue| issue["category"] == "UnknownPlatform")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn health_report_never_triggers_a_scan_itself() {
        // Scan once while the archive is present, then delete it from
        // disk without ever scanning again. `catalogue_health_report`
        // must read only what the last completed scan already persisted
        // (`last_verified_missing_at` still unset) - if it instead
        // performed its own live check, this archive would incorrectly
        // show up as Missing.
        let root = temp_dir("health-no-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let archive_path = write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        std::fs::remove_file(&archive_path).unwrap();

        let database = Database::open_or_create(&database_path).unwrap();
        let archives = database.load_archives().unwrap();
        let report = catalogue_health_report(&archives);

        assert_eq!(
            report.missing_count, 0,
            "reading the health report must never itself run a scan that would notice the \
             archive is now gone"
        );
        assert!(report.issues.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_formats_without_panicking() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let non_utf8_name =
            OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p']);
        let entry = LibraryArchiveView {
            id: 1,
            path: PathBuf::from("/roms").join(&non_utf8_name),
            platform: Some("Unknown".to_string()),
            platform_source: Some("heuristic-path-detector".to_string()),
            present: true,
            size_bytes: Some(10),
            modified_time_unix_seconds: Some(0),
        };

        // Human output uses Path::display, which is lossy-but-safe and
        // must not panic on a non-UTF-8 path.
        let human = format_library_entries(std::slice::from_ref(&entry));
        assert!(human.contains("Path: "));

        // JSON output uses the same display-safe conversion (see
        // serialize_path_display) rather than PathBuf's own Serialize
        // impl (which requires valid Unicode and would otherwise fail the
        // whole list's --json output over one oddly-named archive) - it
        // must succeed and produce valid, parseable JSON, not panic or
        // error out.
        let json = format_library_entries_json(std::slice::from_ref(&entry)).unwrap();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert!(parsed[0]["path"].as_str().unwrap().contains("fo"));
    }

    #[test]
    fn database_failure_does_not_affect_mount_planning_in_the_cli_layer() {
        // Mirrors the equivalent test in archivefs_core::database: force a
        // database failure here, in the CLI's own test suite, then confirm
        // real (unrelated) core mount-planning logic still behaves
        // normally in the same test. mount/mount-one/unmount/unmount-one
        // command handlers in `run()` never call any library-* function.
        let root = temp_dir("cli-database-failure-mount-safety");
        let occupied_by_a_file = root.join("not-a-directory");
        std::fs::write(&occupied_by_a_file, b"not a directory").unwrap();
        let impossible_db_path = occupied_by_a_file.join("library.sqlite3");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);

        let result = run_library_scan(&config, &impossible_db_path, "test");
        assert!(result.is_err());

        let scanner = ArchiveScanner::new(&config);
        let archives = scanner.scan_archives().unwrap();
        let plans = archivefs_core::plan_mounts(&archives, &config.mount_root);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].state, archivefs_core::MountState::Pending);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Small helper so `library_find_matches_on_platform` can check the
    /// heading text without duplicating `print_library_find_results`'s
    /// stdout-writing shape.
    fn print_library_find_results_for_test(query: &str, entries: &[LibraryArchiveView]) -> String {
        let mut output = format!("ArchiveFS Library Find\nQuery: {query}\n\n");
        output.push_str(&format_library_entries(entries));
        output
    }

    // -------------------------------------------------------------
    // library-set-platform / library-clear-platform
    // -------------------------------------------------------------

    #[test]
    fn library_set_platform_assigns_by_query() {
        let root = temp_dir("set-platform-by-query");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let change = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("mystery".to_string()),
            "GameCube",
        )
        .unwrap();

        assert_eq!(change.old_platform, None);
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));
        assert!(change.path.display().to_string().ends_with("mystery.zip"));

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(entries[0].platform.as_deref(), Some("GameCube"));
        assert_eq!(
            entries[0].platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_assigns_by_exact_id_and_touches_only_that_row() {
        let root = temp_dir("set-platform-by-id");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "collection-a/game.zip", b"a");
        write_archive_file(&source, "collection-b/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let target_id = entries
            .iter()
            .find(|entry| entry.path.display().to_string().contains("collection-a"))
            .unwrap()
            .id;

        run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Id(target_id),
            "GameCube",
        )
        .unwrap();

        let entries = build_library_entries(&database_path, false).unwrap();
        let changed = entries.iter().find(|entry| entry.id == target_id).unwrap();
        let untouched = entries.iter().find(|entry| entry.id != target_id).unwrap();
        assert_eq!(changed.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            untouched.platform, None,
            "only the exactly-selected archive may change"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_assigns_by_exact_path_and_touches_only_that_row() {
        let root = temp_dir("set-platform-by-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let target_path = write_archive_file(&source, "collection-a/game.zip", b"a");
        write_archive_file(&source, "collection-b/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Path(target_path.clone()),
            "GameCube",
        )
        .unwrap();

        let entries = build_library_entries(&database_path, false).unwrap();
        let changed = entries
            .iter()
            .find(|entry| entry.path == target_path)
            .unwrap();
        let untouched = entries
            .iter()
            .find(|entry| entry.path != target_path)
            .unwrap();
        assert_eq!(changed.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            untouched.platform, None,
            "only the exactly-selected archive may change"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_unknown_path_is_a_clear_error() {
        let root = temp_dir("set-platform-unknown-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let error = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Path(PathBuf::from("/nowhere/does-not-exist.zip")),
            "GameCube",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("no archive found with exact path")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_ambiguous_query_changes_nothing() {
        let root = temp_dir("set-platform-ambiguous");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "collection-a/game.zip", b"a");
        write_archive_file(&source, "collection-b/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let error = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("game".to_string()),
            "GameCube",
        )
        .unwrap_err();

        assert!(error.to_string().contains("multiple archives matched"));
        assert!(error.to_string().contains("--id"));

        let entries = build_library_entries(&database_path, false).unwrap();
        assert!(
            entries.iter().all(|entry| entry.platform.is_none()),
            "an ambiguous query must leave every candidate untouched"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_no_match_is_a_clear_error() {
        let root = temp_dir("set-platform-no-match");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let error = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("nothing-will-match-this".to_string()),
            "GameCube",
        )
        .unwrap_err();

        assert!(error.to_string().contains("no archive matched"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_missing_database_is_a_clear_error() {
        let root = temp_dir("set-platform-missing-database");
        let database_path = root.join("library.sqlite3");

        let error = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("anything".to_string()),
            "GameCube",
        )
        .unwrap_err();

        assert!(error.to_string().contains("No library database found"));
        assert!(error.to_string().contains("library-scan"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_unknown_id_is_a_clear_error() {
        let root = temp_dir("set-platform-unknown-id");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let error = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Id(999_999),
            "GameCube",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("no archive found with id 999999")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_clear_platform_retires_a_manual_assignment() {
        let root = temp_dir("clear-platform-retires");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("mystery".to_string()),
            "GameCube",
        )
        .unwrap();

        let change = run_library_clear_platform(
            &database_path,
            &LibraryTargetSelector::Query("mystery".to_string()),
        )
        .unwrap();

        assert_eq!(change.old_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.old_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));
        assert_eq!(
            change.new_platform, None,
            "mystery.zip never had an automatic detection to fall back to"
        );

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(entries[0].platform, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_clear_platform_immediately_exposes_the_automatic_result() {
        let root = temp_dir("clear-platform-exposes-automatic");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let target_id = build_library_entries(&database_path, false).unwrap()[0].id;
        run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Id(target_id),
            "GameCube",
        )
        .unwrap();

        let change =
            run_library_clear_platform(&database_path, &LibraryTargetSelector::Id(target_id))
                .unwrap();

        // No rescan anywhere in this test.
        assert_eq!(change.old_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_platform.as_deref(), Some("MSX2"));
        assert_eq!(change.new_source.as_deref(), Some("folder_alias"));
        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(entries[0].platform.as_deref(), Some("MSX2"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_clear_platform_by_id_is_a_no_op_when_not_manual() {
        let root = temp_dir("clear-platform-not-manual");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let target_id = build_library_entries(&database_path, false).unwrap()[0].id;

        let change =
            run_library_clear_platform(&database_path, &LibraryTargetSelector::Id(target_id))
                .unwrap();

        assert_eq!(change.old_platform.as_deref(), Some("MSX2"));
        assert_eq!(change.old_source.as_deref(), Some("folder_alias"));
        assert_eq!(change.new_platform, change.old_platform);

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(
            entries[0].platform.as_deref(),
            Some("MSX2"),
            "a non-manual assignment must be untouched by clear"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // -------------------------------------------------------------
    // library-set-platform-bulk / library-clear-platform-bulk
    // -------------------------------------------------------------

    #[test]
    fn extract_repeated_id_flags_collects_every_occurrence_in_order() {
        let mut args = vec![
            "--id".to_string(),
            "1".to_string(),
            "GameCube".to_string(),
            "--id".to_string(),
            "2".to_string(),
            "--id".to_string(),
            "3".to_string(),
        ];

        let ids = extract_repeated_id_flags(&mut args).unwrap();

        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(args, vec!["GameCube".to_string()]);
    }

    #[test]
    fn extract_repeated_id_flags_rejects_an_invalid_value() {
        let mut args = vec!["--id".to_string(), "not-a-number".to_string()];
        assert!(extract_repeated_id_flags(&mut args).is_err());
    }

    #[test]
    fn extract_repeated_id_flags_requires_a_value() {
        let mut args = vec!["--id".to_string()];
        assert!(extract_repeated_id_flags(&mut args).is_err());
    }

    #[test]
    fn extract_repeated_path_flags_collects_every_occurrence_in_order() {
        let mut args = vec![
            "--path".to_string(),
            "/roms/a.zip".to_string(),
            "--path".to_string(),
            "/roms/b.zip".to_string(),
        ];

        let paths = extract_repeated_path_flags(&mut args).unwrap();

        assert_eq!(
            paths,
            vec![PathBuf::from("/roms/a.zip"), PathBuf::from("/roms/b.zip")]
        );
        assert!(args.is_empty());
    }

    #[test]
    fn library_set_platform_bulk_changes_every_selected_archive() {
        let root = temp_dir("bulk-set-changes-every-archive");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        write_archive_file(&source, "b.zip", b"b");
        write_archive_file(&source, "c.zip", b"c");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let ids: Vec<i64> = entries.iter().map(|entry| entry.id).collect();

        let summary = run_library_set_platform_bulk(&database_path, &ids, &[], "GameCube").unwrap();

        assert_eq!(summary.requested, 3);
        assert_eq!(summary.changed, 3);
        assert_eq!(summary.unchanged, 0);
        assert!(summary.missing.is_empty());
        let entries = build_library_entries(&database_path, false).unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry.platform.as_deref() == Some("GameCube"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_bulk_by_repeated_path_resolves_exact_archives() {
        let root = temp_dir("bulk-set-by-repeated-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let path_a = write_archive_file(&source, "collection-a/game.zip", b"a");
        let path_b = write_archive_file(&source, "collection-b/game.zip", b"b");
        write_archive_file(&source, "collection-c/game.zip", b"c");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let summary = run_library_set_platform_bulk(
            &database_path,
            &[],
            &[path_a.clone(), path_b.clone()],
            "GameCube",
        )
        .unwrap();

        assert_eq!(summary.requested, 2);
        assert_eq!(summary.changed, 2);
        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|e| e.path == path_a)
                .unwrap()
                .platform
                .as_deref(),
            Some("GameCube")
        );
        assert_eq!(
            entries
                .iter()
                .find(|e| e.path == path_b)
                .unwrap()
                .platform
                .as_deref(),
            Some("GameCube")
        );
        assert_eq!(
            entries
                .iter()
                .find(|e| e.path.to_string_lossy().contains("collection-c"))
                .unwrap()
                .platform,
            None,
            "an archive not named by --id or --path must be untouched"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_bulk_combines_id_and_path_selectors_deterministically() {
        let root = temp_dir("bulk-set-mixed-selectors");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        let path_b = write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let id_a = entries
            .iter()
            .find(|e| e.path.to_string_lossy().ends_with("a.zip"))
            .unwrap()
            .id;

        let summary =
            run_library_set_platform_bulk(&database_path, &[id_a], &[path_b], "GameCube").unwrap();

        assert_eq!(summary.requested, 2);
        assert_eq!(summary.changed, 2);
        let entries = build_library_entries(&database_path, false).unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry.platform.as_deref() == Some("GameCube")),
            "an --id and a --path naming different archives must both be applied"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_bulk_deduplicates_an_id_and_path_naming_the_same_archive() {
        let root = temp_dir("bulk-set-dedup-id-and-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let path_a = write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let id_a = build_library_entries(&database_path, false).unwrap()[0].id;

        let summary =
            run_library_set_platform_bulk(&database_path, &[id_a], &[path_a], "GameCube").unwrap();

        assert_eq!(
            summary.requested, 1,
            "an --id and a --path naming the same archive must resolve to one request"
        );
        assert_eq!(summary.changed, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_bulk_reports_missing_ids_without_failing() {
        let root = temp_dir("bulk-set-missing-id");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let real_id = build_library_entries(&database_path, false).unwrap()[0].id;
        let missing_id = real_id + 999_999;

        let summary =
            run_library_set_platform_bulk(&database_path, &[real_id, missing_id], &[], "GameCube")
                .unwrap();

        assert_eq!(summary.changed, 1);
        assert_eq!(summary.missing, vec![missing_id]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_set_platform_bulk_by_unresolvable_path_is_a_clear_error() {
        let root = temp_dir("bulk-set-unresolvable-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let error = run_library_set_platform_bulk(
            &database_path,
            &[],
            &[PathBuf::from("/does/not/exist.zip")],
            "GameCube",
        )
        .unwrap_err();

        assert!(error.to_string().contains("no archive found"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_clear_platform_bulk_restores_each_archives_own_fallback() {
        let root = temp_dir("bulk-clear-restores-fallback");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "msx2/game.zip", b"a");
        write_archive_file(&source, "neogeo/game.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let ids: Vec<i64> = entries.iter().map(|entry| entry.id).collect();
        run_library_set_platform_bulk(&database_path, &ids, &[], "GameCube").unwrap();

        let summary = run_library_clear_platform_bulk(&database_path, &ids, &[]).unwrap();

        assert_eq!(summary.changed, 2);
        let entries = build_library_entries(&database_path, false).unwrap();
        let msx2 = entries
            .iter()
            .find(|e| e.path.to_string_lossy().contains("msx2"))
            .unwrap();
        assert_eq!(msx2.platform.as_deref(), Some("MSX2"));
        let neogeo = entries
            .iter()
            .find(|e| e.path.to_string_lossy().contains("neogeo"))
            .unwrap();
        assert_eq!(neogeo.platform.as_deref(), Some("NeoGeo"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_platform_commands_never_trigger_a_scan() {
        let root = temp_dir("bulk-no-scan-side-effect");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let id_a = build_library_entries(&database_path, false).unwrap()[0].id;

        // A newly-appearing archive on disk must remain invisible to the
        // library database after bulk set/clear - proof neither command
        // walks the filesystem or calls scan_and_persist.
        write_archive_file(&source, "new-archive.zip", b"new");

        run_library_set_platform_bulk(&database_path, &[id_a], &[], "GameCube").unwrap();
        run_library_clear_platform_bulk(&database_path, &[id_a], &[]).unwrap();

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "bulk platform commands must never discover new archives via a scan"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_json_summary_has_the_expected_shape() {
        let root = temp_dir("bulk-json-shape");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();
        let id_a = build_library_entries(&database_path, false).unwrap()[0].id;
        let missing_id = id_a + 999_999;

        let summary =
            run_library_set_platform_bulk(&database_path, &[id_a, missing_id], &[], "GameCube")
                .unwrap();
        let json = serde_json::to_value(&summary).unwrap();

        assert_eq!(json["requested"], 2);
        assert_eq!(json["changed"], 1);
        assert_eq!(json["unchanged"], 0);
        assert_eq!(json["missing"], serde_json::json!([missing_id]));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_platform_argument_still_validates_canonical_names_for_bulk() {
        // library-set-platform-bulk reuses resolve_platform_argument
        // directly - no separate, independently-drifting validation.
        assert!(resolve_platform_argument("NotARealPlatform".to_string(), false).is_err());
        assert_eq!(
            resolve_platform_argument("gamecube".to_string(), false).unwrap(),
            "GameCube"
        );
        assert_eq!(
            resolve_platform_argument("AnythingAtAll".to_string(), true).unwrap(),
            "AnythingAtAll"
        );
    }

    #[test]
    fn require_at_least_one_bulk_selector_rejects_an_empty_selection() {
        let error =
            require_at_least_one_bulk_selector("library-set-platform-bulk", &[], &[]).unwrap_err();
        assert!(error.to_string().contains("at least one --id or --path"));
    }

    #[test]
    fn require_at_least_one_bulk_selector_accepts_an_id_alone() {
        assert!(require_at_least_one_bulk_selector("library-set-platform-bulk", &[1], &[]).is_ok());
    }

    #[test]
    fn require_at_least_one_bulk_selector_accepts_a_path_alone() {
        assert!(
            require_at_least_one_bulk_selector(
                "library-set-platform-bulk",
                &[],
                &[PathBuf::from("/roms/a.zip")]
            )
            .is_ok()
        );
    }

    // -------------------------------------------------------------
    // platform-alias-list / platform-alias-add / platform-alias-remove
    // -------------------------------------------------------------

    #[test]
    fn platform_alias_add_args_parsing_requires_exactly_two_positional_args() {
        let (json, alias, platform) = parse_platform_alias_add_args(
            ["gc", "GameCube"].into_iter().map(str::to_string).collect(),
        )
        .unwrap();
        assert!(!json);
        assert_eq!(alias, "gc");
        assert_eq!(platform, "GameCube");

        let (json, alias, platform) = parse_platform_alias_add_args(
            ["--json", "gc", "GameCube"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        )
        .unwrap();
        assert!(json);
        assert_eq!(alias, "gc");
        assert_eq!(platform, "GameCube");

        assert!(parse_platform_alias_add_args(vec!["gc".to_string()]).is_err());
        assert!(parse_platform_alias_add_args(Vec::new()).is_err());
        assert!(
            parse_platform_alias_add_args(
                ["gc", "GameCube", "extra"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
            .is_err()
        );
    }

    #[test]
    fn platform_alias_list_is_empty_and_successful_when_no_database_exists_yet() {
        let root = temp_dir("platform-alias-list-no-database");
        let database_path = root.join("does-not-exist.sqlite3");

        let aliases = list_platform_aliases_or_empty(&database_path).unwrap();
        assert!(aliases.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_add_list_remove_round_trip() {
        let root = temp_dir("platform-alias-cli-round-trip");
        let database_path = root.join("library.sqlite3");

        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            database.add_platform_alias("gc", "GameCube").unwrap();
        }

        let listed = list_platform_aliases_or_empty(&database_path).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].alias, "gc");
        assert_eq!(listed[0].platform, "GameCube");

        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            assert!(database.remove_platform_alias("gc").unwrap());
        }
        assert!(
            list_platform_aliases_or_empty(&database_path)
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_list_json_shape_round_trips_through_serde() {
        let root = temp_dir("platform-alias-json-shape");
        let database_path = root.join("library.sqlite3");
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            database.add_platform_alias("gc", "GameCube").unwrap();
        }

        let aliases = list_platform_aliases_or_empty(&database_path).unwrap();
        let json = serde_json::to_string_pretty(&aliases).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["alias"], "gc");
        assert_eq!(parsed[0]["normalized_alias"], "gc");
        assert_eq!(parsed[0]["platform"], "GameCube");
        assert!(parsed[0]["id"].is_number());
        assert!(parsed[0]["created_at"].is_string());
        assert!(parsed[0]["updated_at"].is_string());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_add_duplicate_normalized_alias_is_a_clear_error() {
        let root = temp_dir("platform-alias-cli-duplicate");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();

        let error = database.add_platform_alias("GC", "Wii").unwrap_err();
        assert!(error.to_string().contains("already exists"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_remove_unknown_alias_is_a_clear_error() {
        let root = temp_dir("platform-alias-cli-remove-unknown");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        // `run()`'s "platform-alias-remove" match arm turns this `false`
        // into `Err(format!("no platform alias matches '{alias}'"))` -
        // exercised here at the level this file's other command handlers
        // are tested at (the underlying, directly callable operation).
        assert!(!database.remove_platform_alias("does-not-exist").unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_add_invalid_alias_is_a_clear_error() {
        let root = temp_dir("platform-alias-cli-invalid-alias");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let empty = database.add_platform_alias("---", "GameCube").unwrap_err();
        assert!(empty.to_string().contains("letter or digit"));

        let path_like = database
            .add_platform_alias("gc/extra", "GameCube")
            .unwrap_err();
        assert!(path_like.to_string().contains('/'));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_add_validates_against_canonical_platform_names() {
        let root = temp_dir("platform-alias-cli-canonical-validation");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let error = database
            .add_platform_alias("gc", "NotARealPlatform")
            .unwrap_err();
        assert!(error.to_string().contains("not a known platform"));

        let saved = database.add_platform_alias("wii", "wii").unwrap();
        assert_eq!(
            saved.platform, "Wii",
            "canonical spelling must be stored regardless of the caller's casing"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_add_has_no_automatic_scan_side_effect() {
        let root = temp_dir("platform-alias-cli-no-auto-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "n64/game.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial-scan").unwrap();
        assert_eq!(
            build_library_entries(&database_path, false).unwrap()[0]
                .platform
                .as_deref(),
            Some("N64"),
            "sanity check: the built-in folder alias detects N64 before any custom alias exists"
        );

        // Adding a custom alias for "n64" -> GameCube must not itself
        // rescan/re-detect anything - the already-persisted archive's
        // platform must stay exactly as the last scan left it until a
        // new library-scan is run.
        {
            let mut database = Database::open_or_create(&database_path).unwrap();
            database.add_platform_alias("n64", "GameCube").unwrap();
        }

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(
            entries[0].platform.as_deref(),
            Some("N64"),
            "platform-alias-add must not trigger an automatic rescan"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_starter_config(config_path: &Path, mount_root: &Path) {
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n",
                mount_root.display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn resolve_keep_catalogue_flag_requires_exactly_one_flag() {
        assert!(resolve_keep_catalogue_flag(true, false).unwrap());
        assert!(!resolve_keep_catalogue_flag(false, true).unwrap());
        assert!(resolve_keep_catalogue_flag(false, false).is_err());
        assert!(resolve_keep_catalogue_flag(true, true).is_err());
    }

    #[test]
    fn format_source_folder_views_reports_an_empty_configuration_truthfully() {
        let output = format_source_folder_views(&[]);
        assert!(output.contains("No source folders are configured."));
        assert!(output.contains("archivefs-cli source add"));
    }

    #[test]
    fn cli_source_lifecycle_uses_the_same_core_functions_as_the_gui() {
        // Exercises the exact core functions the CLI's "source" and
        // "sources" match arms call (add/list/scan/enable/disable/remove)
        // through the same `_at` entry points, proving the CLI never
        // duplicates core logic - it only formats what these functions
        // already return.
        let root = temp_dir("cli-source-lifecycle");
        let config_path = root.join("config.toml");
        let database_path = root.join("library.sqlite3");
        let mount_root = root.join("mounts");
        write_starter_config(&config_path, &mount_root);
        let source_a = root.join("source-a");
        std::fs::create_dir_all(&source_a).unwrap();
        std::fs::write(source_a.join("a.zip"), b"a").unwrap();

        let added =
            archivefs_core::add_source_folder_at(&config_path, &database_path, &source_a).unwrap();
        assert!(added.enabled);

        let views =
            archivefs_core::list_source_folder_views_at(&config_path, &database_path).unwrap();
        assert_eq!(views.len(), 1);
        let formatted = format_source_folder_views(&views);
        assert!(formatted.contains("source-a"));
        assert!(formatted.contains("Available"));

        let summary =
            archivefs_core::scan_source_folder_at(&config_path, &database_path, &source_a, "test")
                .unwrap();
        assert_eq!(summary.counts.archives_added, 1);

        let disabled = archivefs_core::set_source_folder_enabled_at(
            &config_path,
            &database_path,
            &source_a,
            false,
        )
        .unwrap();
        assert!(!disabled.source.enabled);
        assert!(disabled.scan.is_none());

        let removed =
            archivefs_core::remove_source_folder_at(&config_path, &database_path, &source_a, true)
                .unwrap();
        assert_eq!(removed.catalogue_rows_removed, None);
        assert!(
            source_a.exists(),
            "the CLI path must never touch the filesystem source"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_source_folder_report_serializes_a_display_safe_path() {
        let report = RemoveSourceFolderReport {
            removed_path: PathBuf::from("/mnt/usbdrive/retro"),
            catalogue_rows_removed: Some(3),
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("/mnt/usbdrive/retro"));
        assert!(json.contains("\"catalogue_rows_removed\":3"));
    }

    #[test]
    fn resolve_target_selector_requires_exactly_one_selection_method() {
        assert!(matches!(
            resolve_target_selector("library-set-platform", Some(7), None, Vec::new()).unwrap(),
            LibraryTargetSelector::Id(7)
        ));
        assert!(matches!(
            resolve_target_selector(
                "library-set-platform",
                None,
                Some(PathBuf::from("/a.zip")),
                Vec::new()
            )
            .unwrap(),
            LibraryTargetSelector::Path(path) if path == Path::new("/a.zip")
        ));
        assert!(matches!(
            resolve_target_selector(
                "library-set-platform",
                None,
                None,
                vec!["query".to_string()]
            )
            .unwrap(),
            LibraryTargetSelector::Query(query) if query == "query"
        ));

        assert!(
            resolve_target_selector(
                "library-set-platform",
                Some(7),
                Some(PathBuf::from("/a.zip")),
                Vec::new()
            )
            .is_err(),
            "--id and --path together must be rejected"
        );
        assert!(
            resolve_target_selector("library-set-platform", None, None, Vec::new()).is_err(),
            "no selector at all must be rejected"
        );
        assert!(
            resolve_target_selector(
                "library-set-platform",
                Some(7),
                None,
                vec!["extra".to_string()]
            )
            .is_err(),
            "--id plus leftover query words must be rejected"
        );
    }

    #[test]
    fn unsupported_platform_text_is_rejected_without_custom() {
        let error = resolve_platform_argument("NotARealPlatform".to_string(), false).unwrap_err();
        assert!(error.to_string().contains("unsupported platform"));
        assert!(error.to_string().contains("--custom"));
    }

    #[test]
    fn custom_flag_stores_unsupported_platform_text_exactly_as_typed() {
        assert_eq!(
            resolve_platform_argument("NotARealPlatform".to_string(), true).unwrap(),
            "NotARealPlatform"
        );
    }

    #[test]
    fn platform_matching_is_case_insensitive_but_stores_one_canonical_spelling() {
        for typed in ["gamecube", "GAMECUBE", "GameCube", "gAmEcUbE"] {
            assert_eq!(
                resolve_platform_argument(typed.to_string(), false).unwrap(),
                "GameCube",
                "{typed:?} must resolve to the canonical spelling"
            );
        }
        // --custom bypasses canonical matching entirely, so casing is
        // preserved exactly as typed.
        assert_eq!(
            resolve_platform_argument("gamecube".to_string(), true).unwrap(),
            "gamecube"
        );
    }

    #[test]
    fn extract_id_flag_parses_removes_and_rejects_invalid_values() {
        let mut args = vec!["--id".to_string(), "42".to_string(), "GameCube".to_string()];
        assert_eq!(extract_id_flag(&mut args).unwrap(), Some(42));
        assert_eq!(args, vec!["GameCube".to_string()]);

        let mut args = vec!["GameCube".to_string()];
        assert_eq!(extract_id_flag(&mut args).unwrap(), None);

        let mut args = vec!["--id".to_string(), "not-a-number".to_string()];
        assert!(extract_id_flag(&mut args).is_err());

        let mut args = vec!["--id".to_string()];
        assert!(extract_id_flag(&mut args).is_err());
    }

    #[test]
    fn extract_path_flag_parses_removes_and_requires_a_value() {
        let mut args = vec![
            "--path".to_string(),
            "/roms/game.zip".to_string(),
            "GameCube".to_string(),
        ];
        assert_eq!(
            extract_path_flag(&mut args).unwrap(),
            Some(PathBuf::from("/roms/game.zip"))
        );
        assert_eq!(args, vec!["GameCube".to_string()]);

        let mut args = vec!["GameCube".to_string()];
        assert_eq!(extract_path_flag(&mut args).unwrap(), None);

        let mut args = vec!["--path".to_string()];
        assert!(extract_path_flag(&mut args).is_err());
    }

    #[test]
    fn print_library_platform_change_shows_old_new_and_provenance() {
        let change = LibraryPlatformChangeView {
            path: PathBuf::from("/roms/n64/Luigis_Mansion.zip"),
            old_platform: Some("N64".to_string()),
            old_source: Some("folder_alias".to_string()),
            new_platform: Some("GameCube".to_string()),
            new_source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
        };

        assert_eq!(
            format_platform_and_source(
                change.old_platform.as_deref(),
                change.old_source.as_deref()
            ),
            "N64 (folder_alias)"
        );
        assert_eq!(
            format_platform_and_source(
                change.new_platform.as_deref(),
                change.new_source.as_deref()
            ),
            "GameCube (manual)"
        );
    }

    #[test]
    fn library_set_platform_json_round_trips_expected_fields() {
        let root = temp_dir("set-platform-json");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "test").unwrap();

        let change = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("mystery".to_string()),
            "GameCube",
        )
        .unwrap();
        let json = serde_json::to_string_pretty(&change).unwrap();
        let parsed = serde_json::from_str::<serde_json::Value>(&json).unwrap();

        assert!(parsed["path"].as_str().unwrap().ends_with("mystery.zip"));
        assert_eq!(parsed["old_platform"], serde_json::Value::Null);
        assert_eq!(parsed["new_platform"], "GameCube");
        assert_eq!(parsed["new_source"], MANUAL_PLATFORM_SOURCE);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_platform_survives_a_rescan_via_the_cli_layer() {
        // A CLI-layer confirmation of the same guarantee proven in depth
        // in archivefs_core::database - manual assignment precedence is
        // not something the CLI reimplements, only exposes.
        let root = temp_dir("cli-manual-survives-rescan");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "n64/Luigis_Mansion_[hexrom.com].zip", b"contents");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial-scan").unwrap();

        let change = run_library_set_platform(
            &database_path,
            &LibraryTargetSelector::Query("Luigis_Mansion".to_string()),
            "GameCube",
        )
        .unwrap();
        assert_eq!(change.old_platform.as_deref(), Some("N64"));
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));

        run_library_scan(&config, &database_path, "rescan").unwrap();

        let entries = build_library_entries(&database_path, false).unwrap();
        assert_eq!(entries[0].platform.as_deref(), Some("GameCube"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_remove_missing_accepts_exact_id_and_exact_path_selectors() {
        let root = temp_dir("cli-remove-missing-id-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let gone_by_id = write_archive_file(&source, "gone-id.zip", b"id");
        let gone_by_path = write_archive_file(&source, "gone-path.zip", b"path");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let id = entries
            .iter()
            .find(|entry| entry.path.ends_with("gone-id.zip"))
            .unwrap()
            .id;
        std::fs::remove_file(&gone_by_id).unwrap();
        std::fs::remove_file(&gone_by_path).unwrap();
        run_library_scan(&config, &database_path, "missing").unwrap();

        let by_id = run_library_remove_missing(&database_path, &[id], &[]).unwrap();
        let by_path =
            run_library_remove_missing(&database_path, &[], std::slice::from_ref(&gone_by_path))
                .unwrap();

        assert_eq!(by_id.removed, 1);
        assert_eq!(by_path.removed, 1);
        assert!(
            build_library_entries(&database_path, false)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_remove_missing_bulk_deduplicates_combined_selectors() {
        let root = temp_dir("cli-remove-missing-bulk");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let gone_a = write_archive_file(&source, "a.zip", b"a");
        let gone_b = write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let id_a = entries
            .iter()
            .find(|entry| entry.path == gone_a)
            .unwrap()
            .id;
        let id_b = entries
            .iter()
            .find(|entry| entry.path == gone_b)
            .unwrap()
            .id;
        std::fs::remove_file(&gone_a).unwrap();
        std::fs::remove_file(&gone_b).unwrap();
        run_library_scan(&config, &database_path, "missing").unwrap();

        let result = run_library_remove_missing(
            &database_path,
            &[id_a, id_a, id_b],
            std::slice::from_ref(&gone_b),
        )
        .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.removed, 2);
        assert!(
            build_library_entries(&database_path, false)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_remove_missing_rejects_present_and_mixed_selections_atomically() {
        let root = temp_dir("cli-remove-missing-reject-present");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let gone = write_archive_file(&source, "gone.zip", b"gone");
        write_archive_file(&source, "present.zip", b"present");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial").unwrap();
        let entries = build_library_entries(&database_path, false).unwrap();
        let gone_id = entries
            .iter()
            .find(|entry| entry.path.ends_with("gone.zip"))
            .unwrap()
            .id;
        let present_id = entries
            .iter()
            .find(|entry| entry.path.ends_with("present.zip"))
            .unwrap()
            .id;
        std::fs::remove_file(gone).unwrap();
        run_library_scan(&config, &database_path, "missing").unwrap();

        assert!(run_library_remove_missing(&database_path, &[present_id], &[]).is_err());
        let mixed =
            run_library_remove_missing(&database_path, &[gone_id, present_id], &[]).unwrap_err();

        assert!(mixed.to_string().contains("currently present"));
        assert_eq!(
            build_library_entries(&database_path, false).unwrap().len(),
            2
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_remove_missing_reports_unknown_ids_and_paths_without_removing_valid_rows() {
        let root = temp_dir("cli-remove-missing-unknown");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let gone = write_archive_file(&source, "gone.zip", b"gone");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial").unwrap();
        let id = build_library_entries(&database_path, false).unwrap()[0].id;
        std::fs::remove_file(gone).unwrap();
        run_library_scan(&config, &database_path, "missing").unwrap();

        let unknown_id =
            run_library_remove_missing(&database_path, &[id, i64::MAX], &[]).unwrap_err();
        let unknown_path =
            run_library_remove_missing(&database_path, &[], &[source.join("never-scanned.zip")])
                .unwrap_err();

        assert!(unknown_id.to_string().contains("not found"));
        assert!(
            unknown_path
                .to_string()
                .contains("no archive found with exact path")
        );
        assert_eq!(
            build_library_entries(&database_path, false).unwrap().len(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_remove_missing_json_and_human_output_have_safe_compatible_shapes() {
        let result = MissingArchiveRemovalResult {
            requested: 2,
            removed: 2,
            archive_ids: vec![12, 13],
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["requested"], 2);
        assert_eq!(json["removed"], 2);
        assert_eq!(json["archive_ids"], serde_json::json!([12, 13]));
        assert_eq!(json.as_object().unwrap().len(), 3);
        let human = format_missing_removal(&result);
        assert!(human.contains("Removed: 2 missing catalogue entries."));
        assert!(human.contains("No archive files or mounted contents were deleted."));
    }

    #[test]
    fn library_remove_missing_never_scans_or_alters_an_archive_file() {
        let root = temp_dir("cli-remove-missing-no-filesystem");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        let archive_path = write_archive_file(&source, "game.zip", b"initial");
        let config = config_for(&source, &mount);
        run_library_scan(&config, &database_path, "initial").unwrap();
        let id = build_library_entries(&database_path, false).unwrap()[0].id;
        std::fs::remove_file(&archive_path).unwrap();
        run_library_scan(&config, &database_path, "missing").unwrap();
        std::fs::write(&archive_path, b"reappeared").unwrap();
        let database = Database::open_or_create(&database_path).unwrap();
        let scan_id_before = database
            .latest_completed_scan()
            .unwrap()
            .unwrap()
            .scan_run_id;
        database.close().unwrap();

        run_library_remove_missing(&database_path, &[id], &[]).unwrap();

        assert_eq!(std::fs::read(&archive_path).unwrap(), b"reappeared");
        let database = Database::open_or_create(&database_path).unwrap();
        assert_eq!(
            database
                .latest_completed_scan()
                .unwrap()
                .unwrap()
                .scan_run_id,
            scan_id_before
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
