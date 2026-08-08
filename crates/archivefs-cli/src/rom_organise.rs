//! Canonical ROM organisation CLI: configuring the master ROM root.
//!
//! Planning and applying organisation is a GUI workflow (it needs the
//! resolved platform identity and the imported RomM slug cache); the CLI
//! here manages the configuration that workflow consumes.

use std::path::PathBuf;

use archivefs_core::{
    Config, clear_master_rom_root_default, default_config_path, set_master_rom_root_default,
};

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(command) = args.first().cloned() else {
        return Err("rom-organise requires a command".into());
    };
    let rest = &args[1..];
    match command.as_str() {
        "set-master-root" => {
            let Some(path) = rest.first() else {
                return Err("rom-organise set-master-root requires a path".into());
            };
            if rest.len() > 1 {
                return Err("rom-organise set-master-root accepts one path".into());
            }
            let path = PathBuf::from(path);
            let previous = set_master_rom_root_default(&path)?;
            println!(
                "Master ROM root set to {}{}",
                path.display(),
                previous
                    .map(|old| format!(" (was {})", old.display()))
                    .unwrap_or_default()
            );
            println!("{}", config_hint());
        }
        "clear-master-root" => {
            if !rest.is_empty() {
                return Err("rom-organise clear-master-root accepts no arguments".into());
            }
            match clear_master_rom_root_default()? {
                Some(removed) => println!("Master ROM root cleared (was {}).", removed.display()),
                None => println!("No master ROM root was configured."),
            }
        }
        "show" => {
            if !rest.is_empty() {
                return Err("rom-organise show accepts no arguments".into());
            }
            match Config::load_default() {
                Ok(config) => match &config.master_rom_root {
                    Some(root) => println!("Master ROM root: {}", root.display()),
                    None => println!("No master ROM root is configured."),
                },
                // A fresh install with no config.toml yet simply has no root.
                Err(_) => println!("No master ROM root is configured."),
            }
        }
        _ => {
            return Err(format!(
                "unknown rom-organise command: {command} (expected set-master-root | \
                 clear-master-root | show)"
            )
            .into());
        }
    }
    Ok(())
}

fn config_hint() -> String {
    format!(
        "Preferences saved to {}",
        default_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use archivefs_core::set_master_rom_root_to;
    use std::path::{Path, PathBuf};

    fn temp_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("mount_root = \"{}\"\n", dir.path().join("mounts").display()),
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn a_relative_master_root_is_rejected() {
        let (_d, config_path) = temp_config();
        let error = set_master_rom_root_to(&config_path, Path::new("relative/roms")).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
    }

    #[test]
    fn set_and_clear_round_trip() {
        let (_d, config_path) = temp_config();
        let roms = config_path.parent().unwrap().join("roms");
        set_master_rom_root_to(&config_path, &roms).unwrap();
        assert_eq!(
            Config::load_from(&config_path).unwrap().master_rom_root,
            Some(roms.clone())
        );
        archivefs_core::clear_master_rom_root_to(&config_path).unwrap();
        assert_eq!(
            Config::load_from(&config_path).unwrap().master_rom_root,
            None
        );
    }
}
