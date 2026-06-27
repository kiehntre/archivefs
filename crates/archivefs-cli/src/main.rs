use std::env;
use std::path::Path;
use std::process::ExitCode;

use archivefs_core::{
    ArchiveIndex, ArchiveIndexEntry, ArchiveIndexFreshness, ArchiveIndexSummary, ArchiveStatus,
    Config, DoctorReport, MountPlan, build_and_write_archive_index, check_archive_index_freshness,
    clean_mount_root, current_statuses, default_index_path, find_archive_index_entries,
    mount_archives, mount_one_archive, read_default_archive_index, run_doctor_default,
    scan_archives, summarize_archive_index, unmount_archives, unmount_one_archive,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("archivefs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_string());

    match command.as_str() {
        "scan" => {
            let config = Config::load_default()?;
            for archive in scan_archives(&config)? {
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
            print_unmount_one(&unmount_one_archive(&config, &input)?);
            warn_if_index_refresh_failed(&config);
        }
        "status" => {
            let config = Config::load_default()?;
            print_statuses(&current_statuses(&config)?);
        }
        "doctor" => {
            print_doctor_report(&run_doctor_default());
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
        "clean" => {
            let config = Config::load_default()?;
            print_cleaned_dirs(&clean_mount_root(&config)?);
        }
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
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

    println!("Index matches for '{query}':");
    for entry in entries {
        println!(
            "  Platform: {}",
            entry.platform.as_deref().unwrap_or("Unknown")
        );
        println!("  Display:  {}", entry.display_name);
        println!("  Archive:  {}", entry.archive_path.display());
        println!("  Mount:    {}", entry.mount_path.display());
        println!("  Health:   {}", entry.health);
        println!("  State:    {}", entry.mount_state);
        println!();
    }
}

fn print_index_summary(summary: &ArchiveIndexSummary) {
    println!("ArchiveFS index");
    println!("Archives: {}", summary.archives_count);
    println!("Mounted: {}", summary.mounted_count);
    println!("Pending: {}", summary.pending_count);
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
    println!("ArchiveFS doctor");
    println!("Config: {}", report.config_path.display());
    println!();
    println!("Checks:");
    for check in &report.checks {
        println!(
            "  [{:<4}] {:<16} {}",
            check.status, check.name, check.detail
        );
    }
    println!();
    println!("Summary:");
    println!("  Archives found: {}", report.archives_found);
    println!(
        "  Archives with detected platform: {}",
        report.archives_with_platform
    );
    println!(
        "  Archives with unknown platform: {}",
        report.archives_unknown_platform
    );
    println!("  Pending archives: {}", report.pending_archives);
    println!("  Mounted archives: {}", report.mounted_archives);
    println!();
    println!("Platform summary:");
    if report.platform_counts.is_empty() {
        println!("  none detected");
    } else {
        for (platform, count) in &report.platform_counts {
            println!("  {platform}: {count}");
        }
    }
    println!();
    println!("Unknown platform examples:");
    if report.unknown_platform_examples.is_empty() {
        println!("  none");
    } else {
        for path in &report.unknown_platform_examples {
            println!("  {}", path.display());
        }
    }
    println!();
    println!("  Ready: {}", if report.is_ready() { "yes" } else { "no" });
}

fn print_statuses(statuses: &[ArchiveStatus]) {
    println!("{:<48}  {:<48}  State", "Archive", "Mount");
    for status in statuses {
        println!(
            "{:<48}  {:<48}  {}",
            status.archive_path.display(),
            status.mount_path.display(),
            status.state
        );
    }
}

fn print_help() {
    println!("archivefs <command>");
    println!();
    println!("Commands:");
    println!("  scan      list supported archives from configured source folders");
    println!("  mount     mount scanned archives with ratarmount");
    println!("  mount-one mount one archive by path or name");
    println!("  unmount   unmount archivefs mountpoints under configured mount_root");
    println!("  unmount-one unmount one archive by path or name");
    println!("  status    show archive path, mount path, and state");
    println!("  doctor    diagnose whether ArchiveFS is ready to run safely");
    println!("  index-build build the JSON archive index");
    println!("  index-show show a summary of the JSON archive index");
    println!("  index-find find entries in the JSON archive index");
    println!("  clean     remove empty directories under mount_root");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}
