//! EmuWiz rename: clean-install migration tests over an isolated HOME.
//!
//! These spawn the real CLI binary against an isolated `$HOME` so the
//! EmuWiz-first / legacy-ArchiveFS-fallback directory resolution is
//! exercised end to end. No real user data is ever touched: every test
//! builds its own temporary home and removes it afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

fn isolated_home(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "emuwiz-clean-install-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run_cli(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_archivefs-cli"))
        .args(args)
        .env("HOME", home)
        .env_remove("USERPROFILE")
        .output()
        .expect("the CLI must run")
}

fn combined(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr)
}

#[test]
fn a_fresh_emuwiz_user_uses_the_emuwiz_directories() {
    let home = isolated_home("fresh");
    let text = combined(&run_cli(&home, &["config-check"]));
    assert!(
        text.contains(home.join(".config/emuwiz/config.toml").to_str().unwrap()),
        "a fresh install must resolve the EmuWiz config path, got:\n{text}"
    );
    assert!(
        !text.contains(home.join(".config/archivefs").to_str().unwrap()),
        "a fresh install must not use the legacy ArchiveFS path:\n{text}"
    );

    // Resolution itself writes nothing.
    assert!(!home.join(".config").exists());
    assert!(!home.join(".local").exists());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_legacy_archivefs_only_user_keeps_using_legacy_directories() {
    let home = isolated_home("legacy-only");
    std::fs::create_dir_all(home.join(".config/archivefs")).unwrap();
    std::fs::create_dir_all(home.join(".local/share/archivefs")).unwrap();

    let text = combined(&run_cli(&home, &["config-check"]));
    assert!(
        text.contains(home.join(".config/archivefs/config.toml").to_str().unwrap()),
        "an existing legacy config dir must be reused, got:\n{text}"
    );

    // The legacy data directory keeps serving the database.
    let status = combined(&run_cli(&home, &["library-status"]));
    assert!(
        status.contains(
            home.join(".local/share/archivefs/library.sqlite3")
                .to_str()
                .unwrap()
        ),
        "the legacy data dir must keep serving the database, got:\n{status}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn when_both_exist_emuwiz_wins_and_nothing_is_overwritten() {
    let home = isolated_home("both");
    std::fs::create_dir_all(home.join(".config/archivefs")).unwrap();
    std::fs::create_dir_all(home.join(".config/emuwiz")).unwrap();
    std::fs::create_dir_all(home.join(".local/share/archivefs")).unwrap();
    std::fs::create_dir_all(home.join(".local/share/emuwiz")).unwrap();

    let text = combined(&run_cli(&home, &["config-check"]));
    assert!(
        text.contains(home.join(".config/emuwiz/config.toml").to_str().unwrap()),
        "EmuWiz must take precedence when both exist, got:\n{text}"
    );

    // Resolution never overwrites or moves conflicting destination data.
    assert!(home.join(".config/archivefs").exists());
    assert!(home.join(".local/share/archivefs").exists());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_partial_legacy_state_is_still_served() {
    // Only the legacy config directory exists; no legacy data directory.
    let home = isolated_home("partial");
    std::fs::create_dir_all(home.join(".config/archivefs")).unwrap();
    let text = combined(&run_cli(&home, &["config-check"]));
    assert!(
        text.contains(home.join(".config/archivefs/config.toml").to_str().unwrap()),
        "a legacy config dir must be reused even without legacy data, got:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_existing_legacy_config_still_loads() {
    let home = isolated_home("legacy-config-loads");
    let config_dir = home.join(".config/archivefs");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
source_folders = ["/roms"]
mount_root = "/mnt/archivefs"
ratarmount_bin = "ratarmount"
"#,
    )
    .unwrap();

    let text = combined(&run_cli(&home, &["config-check"]));
    assert!(
        text.contains(home.join(".config/archivefs/config.toml").to_str().unwrap()),
        "the legacy config must be the one that loads, got:\n{text}"
    );
    assert!(
        !text.contains("could not be read"),
        "a valid legacy config must load cleanly:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
