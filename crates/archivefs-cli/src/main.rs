use std::env;
use std::process::ExitCode;

use archivefs_core::{
    ArchiveStatus, Config, current_statuses, mount_archives, scan_archives, unmount_archives,
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
                println!("{}", archive.display());
            }
        }
        "mount" => {
            let config = Config::load_default()?;
            print_statuses(&mount_archives(&config)?);
        }
        "unmount" => {
            let config = Config::load_default()?;
            print_statuses(&unmount_archives(&config)?);
        }
        "status" => {
            let config = Config::load_default()?;
            print_statuses(&current_statuses(&config)?);
        }
        "help" | "-h" | "--help" => print_help(),
        unknown => {
            print_help();
            return Err(format!("unknown command '{unknown}'").into());
        }
    }

    Ok(())
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
    println!("  unmount   unmount archivefs mountpoints under configured mount_root");
    println!("  status    show archive path, mount path, and state");
    println!();
    println!("Config: ~/.config/archivefs/config.toml");
    println!("Example:");
    println!("  source_folders = [\"/data/archives\"]");
    println!("  mount_root = \"/mnt/archivefs\"");
    println!("  ratarmount_bin = \"ratarmount\"");
}
