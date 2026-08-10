//! App-directory resolution with legacy ArchiveFS compatibility.
//!
//! EmuWiz prefers its own XDG directories but transparently reuses the
//! legacy `archivefs` directories when they exist. This is what lets a
//! pre-rename user's config, database, journals, caches and history keep
//! working without any data being moved or overwritten:
//!
//! 1. Look for the EmuWiz path first.
//! 2. If absent, detect the legacy ArchiveFS path and reuse it.
//! 3. If neither exists (a fresh EmuWiz install), use the EmuWiz path.
//!
//! No files are ever copied or renamed by resolution, so the strategy is
//! idempotent and can never overwrite conflicting destination data. A future
//! migration pass may move legacy data into the EmuWiz directories; until
//! then the legacy directories remain the active ones for existing users.
//!
//! The XDG base-directory layout mirrors what EmuWiz already used
//! (`~/.config/archivefs`, `~/.local/share/archivefs`) rather than adopting
//! the XDG environment variables, so an existing user's data is found at the
//! exact same place it has always been.

use std::env;
use std::path::{Path, PathBuf};

use crate::{ArchiveFsError, Result};

/// The EmuWiz config directory name under `~/.config`.
pub const CONFIG_DIR_NAME: &str = "emuwiz";
/// The legacy ArchiveFS config directory name under `~/.config`.
pub const LEGACY_CONFIG_DIR_NAME: &str = "archivefs";
/// The EmuWiz data directory name under `~/.local/share`.
pub const DATA_DIR_NAME: &str = "emuwiz";
/// The legacy ArchiveFS data directory name under `~/.local/share`.
pub const LEGACY_DATA_DIR_NAME: &str = "archivefs";

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))
}

/// Picks the primary directory when it exists, otherwise the legacy directory
/// when it exists, otherwise the primary directory. Resolution is at the
/// directory level (not per-file) so every file a user writes lands in one
/// consistent place: an existing legacy `archivefs` directory keeps all of
/// its files, and a fresh EmuWiz install writes to the EmuWiz directory.
/// Read-only: it never creates, copies or moves anything.
fn choose_dir(primary: &Path, legacy: &Path) -> PathBuf {
    if path_is_present(primary) {
        primary.to_path_buf()
    } else if path_is_present(legacy) {
        legacy.to_path_buf()
    } else {
        primary.to_path_buf()
    }
}

/// Treat any directory entry, including a broken symlink, as present. An I/O
/// error other than `NotFound` also keeps the primary path selected: falling
/// back after a permissions or transient filesystem error could make EmuWiz
/// read or write a different profile unexpectedly.
fn path_is_present(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

pub(crate) fn config_dir_in(home: &Path) -> PathBuf {
    choose_dir(
        &home.join(".config").join(CONFIG_DIR_NAME),
        &home.join(".config").join(LEGACY_CONFIG_DIR_NAME),
    )
}

pub(crate) fn data_dir_in(home: &Path) -> PathBuf {
    choose_dir(
        &home.join(".local").join("share").join(DATA_DIR_NAME),
        &home.join(".local").join("share").join(LEGACY_DATA_DIR_NAME),
    )
}

#[cfg(test)]
pub(crate) fn config_path_in(home: &Path, leaf: &str) -> PathBuf {
    config_dir_in(home).join(leaf)
}

pub(crate) fn data_path_in(home: &Path, leaf: &str) -> PathBuf {
    data_dir_in(home).join(leaf)
}

/// The config directory root for the current user, resolving the EmuWiz
/// directory first with the legacy ArchiveFS directory as fallback.
pub fn config_dir() -> Result<PathBuf> {
    let home = home()?;
    Ok(config_dir_in(&home))
}

/// The data directory root for the current user, resolving the EmuWiz
/// directory first with the legacy ArchiveFS directory as fallback.
pub fn data_dir() -> Result<PathBuf> {
    let home = home()?;
    Ok(data_dir_in(&home))
}

/// The path to one config file (e.g. `config.toml`) under the effective
/// config directory.
pub fn config_path(leaf: &str) -> Result<PathBuf> {
    Ok(config_dir()?.join(leaf))
}

/// The path to one data file (e.g. `library.sqlite3`) under the effective
/// data directory.
pub fn data_path(leaf: &str) -> Result<PathBuf> {
    Ok(data_dir()?.join(leaf))
}

/// Whether the legacy `archivefs` config directory exists for the current
/// user. Used by diagnostics and migration tooling.
pub fn legacy_config_dir_exists() -> bool {
    home()
        .map(|home| path_is_present(&home.join(".config").join(LEGACY_CONFIG_DIR_NAME)))
        .unwrap_or(false)
}

/// Whether the legacy `archivefs` data directory exists for the current user.
pub fn legacy_data_dir_exists() -> bool {
    home()
        .map(|home| path_is_present(&home.join(".local").join("share").join(LEGACY_DATA_DIR_NAME)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private temp HOME for one test, with the given legacy/primary
    /// directory trees already created.
    fn home_with(name: &str, dirs: &[&[&str]]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("archivefs-app-dirs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for parts in dirs {
            let mut path = root.clone();
            for part in *parts {
                path = path.join(part);
            }
            std::fs::create_dir_all(&path).unwrap();
        }
        root
    }

    #[test]
    fn fresh_install_uses_emuwiz_paths() {
        let home = home_with("fresh", &[]);
        assert_eq!(
            config_path_in(&home, "config.toml"),
            home.join(".config/emuwiz/config.toml")
        );
        assert_eq!(
            data_path_in(&home, "library.sqlite3"),
            home.join(".local/share/emuwiz/library.sqlite3")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_only_install_transparently_reuses_archivefs_paths() {
        let home = home_with(
            "legacy",
            &[&[".config", "archivefs"], &[".local", "share", "archivefs"]],
        );
        assert_eq!(
            config_path_in(&home, "config.toml"),
            home.join(".config/archivefs/config.toml")
        );
        assert_eq!(
            data_path_in(&home, "library.sqlite3"),
            home.join(".local/share/archivefs/library.sqlite3")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn emuwiz_wins_when_both_paths_exist() {
        let home = home_with(
            "both",
            &[
                &[".config", "archivefs"],
                &[".config", "emuwiz"],
                &[".local", "share", "archivefs"],
                &[".local", "share", "emuwiz"],
            ],
        );
        assert_eq!(
            config_path_in(&home, "config.toml"),
            home.join(".config/emuwiz/config.toml")
        );
        assert_eq!(
            data_path_in(&home, "library.sqlite3"),
            home.join(".local/share/emuwiz/library.sqlite3")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn config_and_data_directories_resolve_independently_mixed_states() {
        // EmuWiz config dir exists, but the database lives in legacy.
        let home = home_with(
            "mixed",
            &[&[".config", "emuwiz"], &[".local", "share", "archivefs"]],
        );
        assert_eq!(
            config_path_in(&home, "config.toml"),
            home.join(".config/emuwiz/config.toml")
        );
        assert_eq!(
            data_path_in(&home, "library.sqlite3"),
            home.join(".local/share/archivefs/library.sqlite3")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolution_never_creates_anything() {
        let home = home_with("nowrites", &[]);
        let config = config_path_in(&home, "config.toml");
        let data = data_path_in(&home, "library.sqlite3");
        assert!(!config.exists());
        assert!(!data.exists());
        assert!(!home.join(".config").exists());
        assert!(!home.join(".local").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_primary_symlink_still_has_precedence_over_legacy() {
        use std::os::unix::fs::symlink;

        let home = home_with("broken-primary", &[&[".config", "archivefs"]]);
        let primary = home.join(".config/emuwiz");
        symlink(home.join("missing-target"), &primary).unwrap();

        assert_eq!(config_dir_in(&home), primary);
        let _ = std::fs::remove_dir_all(&home);
    }
}
