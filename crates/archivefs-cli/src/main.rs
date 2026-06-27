use std::env;
use std::process::ExitCode;

use archivefs_core::{
    ArchiveStatus, Config, DoctorReport, MountPlan, current_statuses, mount_archives,
    mount_one_archive, run_doctor_default, scan_archives, unmount_archives,
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
        }
        "unmount" => {
            let config = Config::load_default()?;
            print_statuses(&unmount_archives(&config)?);
        }
        "status" => {
            let config = Config::load_default()?;
            print_statuses(&current_statuses(&config)?);
        }
        "doctor" => {
            print_doctor_report(&run_doctor_default());
        }
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
}

fn print_mount_one(plan: &MountPlan) {
    println!("Mounted:");
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
    println!("  status    show archive path, mount path, and state");
    println!("  doctor    diagnose whether ArchiveFS is ready to run safely");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}
