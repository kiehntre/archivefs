use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::sync::OnceLock;

use archivefs_core::{
    ArchiveIndex, ArchiveIndexEntry, ArchiveIndexFreshness, ArchiveIndexSummary, ArchiveInfo,
    ArchiveScanner, ArchiveStats, ArchiveStatus, Config, ConfigCheckReport, ConfigCheckStatus,
    DoctorReport, MountPlan, WatchRebuildSummary, build_and_write_archive_index,
    check_archive_index_freshness, clean_mount_root, cleanup_selected_mount_dir,
    current_archive_info, current_archive_stats, current_statuses, default_index_path,
    find_archive_index_entries, mount_archives, mount_one_archive, read_default_archive_index,
    run_config_check_default, run_doctor_default, summarize_archive_index, unmount_archives,
    unmount_one_archive, watch_archive_index,
};

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
            let config = Config::load_default()?;
            print_statuses(&current_statuses(&config)?);
        }
        "stats" => {
            let config = Config::load_default()?;
            print_archive_stats(&current_archive_stats(&config)?);
        }
        "info" => {
            let Some(first) = args.next() else {
                return Err("info requires an archive path or name".into());
            };
            let input = std::iter::once(first)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            let config = Config::load_default()?;
            print_archive_info(&current_archive_info(&config, &input)?);
        }
        "doctor" => {
            print_doctor_report(&run_doctor_default());
        }
        "config-check" => {
            print_config_check_report(&run_config_check_default());
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
        "watch" => {
            let config = Config::load_default()?;
            watch_archive_index(
                &config,
                || println!("Watching configured source folders for archive changes."),
                print_watch_rebuild,
            )?;
        }
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
}

fn print_config_check_report(report: &ConfigCheckReport) {
    println!("ArchiveFS config-check");
    println!("Config: {}", report.config_path.display());
    println!();
    println!("Checklist:");
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
    if report.is_ok() {
        println!("Configuration OK");
    } else {
        println!(
            "Configuration has {} error(s), {} warning(s)",
            report.error_count(),
            report.warning_count()
        );
    }
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

fn print_archive_info(info: &ArchiveInfo) {
    print!("{}", format_archive_info(info));
}

fn format_archive_info(info: &ArchiveInfo) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS info\n");
    output.push_str(&format!("Title: {}\n", info.title));
    output.push_str(&format!(
        "Platform: {}\n",
        info.platform.as_deref().unwrap_or("Unknown")
    ));
    output.push_str(&format!("Archive path: {}\n", info.archive_path.display()));
    output.push_str(&format!("Mount path: {}\n", info.mount_path.display()));
    output.push_str(&format!("Extension: {}\n", info.extension));
    output.push_str(&format!(
        "Archive size: {}\n",
        info.size_bytes
            .map(human_size)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!(
        "Last modified: {}\n",
        info.modified_time
            .map(format_system_time)
            .unwrap_or_else(|| "unknown".to_string())
    ));
    output.push_str(&format!("Health: {}\n", info.health));
    output.push_str(&format!("Mount state: {}\n", info.mount_state));
    output.push_str(&format!("Metadata provider: {}\n", info.metadata_provider));
    output.push_str(&format!("Health provider: {}\n", info.health_provider));
    output
}

fn print_archive_stats(stats: &ArchiveStats) {
    print!("{}", format_archive_stats(stats));
}

fn format_archive_stats(stats: &ArchiveStats) -> String {
    let mut output = String::new();
    output.push_str("ArchiveFS stats\n");
    output.push_str(&format!("Total archives: {}\n", stats.total_archives));
    output.push_str(&format!("Mounted: {}\n", stats.mounted_count));
    output.push_str(&format!("Pending: {}\n", stats.pending_count));
    output.push_str(&format!(
        "Total archive size: {}\n",
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
    println!("archivefs [--verbose|-v] [--debug] <command>");
    println!();
    println!("Global flags:");
    println!("  -v, --verbose print operational details");
    println!("  --debug      print diagnostic details");
    println!();
    println!("Commands:");
    println!("  scan      list supported archives from configured source folders");
    println!("  mount     mount scanned archives with ratarmount");
    println!("  mount-one mount one archive by path or name");
    println!("  unmount   unmount archivefs mountpoints under configured mount_root");
    println!("  unmount-one unmount one archive by path or name");
    println!("  status    show archive path, mount path, and state");
    println!("  stats     show archive library counts and sizes");
    println!("  info      show details for one archive by path or name");
    println!("  doctor    diagnose whether ArchiveFS is ready to run safely");
    println!("  config-check validate ArchiveFS configuration");
    println!("  index-build build the JSON archive index");
    println!("  index-show show a summary of the JSON archive index");
    println!("  index-find find entries in the JSON archive index");
    println!("  clean     remove empty directories under mount_root");
    println!("  watch     watch source folders and refresh the JSON index");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
