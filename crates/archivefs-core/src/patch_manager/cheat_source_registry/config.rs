//! Persistent per-user cheat-source preferences.
//!
//! The configuration file lives at `~/.config/archivefs/cheat_sources.toml`.
//! When the file is absent every built-in provider is enabled at its default
//! priority. Only user overrides are persisted; entries that match defaults
//! are omitted from save output.
//!
//! # Nothing the user wrote is thrown away
//!
//! The file is `deny_unknown_fields`, so an unrecognised *key* is a parse
//! error and never reaches this layer. What does reach it is an entry whose
//! shape is valid but whose *subject* this build does not know: a
//! `[[providers]]` naming a source that is not in the registry, or a
//! `[[platform_overrides]]` naming a platform that does not canonicalise.
//! Those arise from a typo, from a provider that a newer build adds, or from
//! one an older build had.
//!
//! Such an entry must survive a load/edit/save cycle. It previously did not:
//! `to_config` rebuilt the provider list from live registry entries alone, so
//! saving any unrelated change deleted the line. See
//! [`super::CheatSourceRegistry::apply_config`] for where they are retained
//! and [`super::CheatSourceRegistry::unresolved_preferences`] for how they are
//! surfaced.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ArchiveFsError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct CheatSourcesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<ProviderConfigEntry>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_overrides: Option<Vec<PlatformOverrideEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigEntry {
    pub id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlatformOverrideEntry {
    pub platform: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_providers: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_overrides: Option<Vec<ProviderPriorityOverride>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderPriorityOverride {
    pub id: String,
    pub priority: u32,
}

pub fn default_cheat_sources_config_path() -> Result<PathBuf, ArchiveFsError> {
    cheat_sources_config_path_in(
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
}

/// The preferences path under `home`, or an error when there is no home.
///
/// Split out so the no-home case can be tested without removing `HOME` from
/// the process: these tests run in parallel with every other test in the
/// crate, and several of those read `HOME`, so mutating it made unrelated
/// tests fail depending on scheduling.
fn cheat_sources_config_path_in(home: Option<OsString>) -> Result<PathBuf, ArchiveFsError> {
    let home = home.ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("cheat_sources.toml"))
}

pub fn load_cheat_sources_config_default() -> Result<CheatSourcesConfig, ArchiveFsError> {
    load_cheat_sources_config_from(default_cheat_sources_config_path()?)
}

pub fn load_cheat_sources_config_from(
    path: impl AsRef<Path>,
) -> Result<CheatSourcesConfig, ArchiveFsError> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CheatSourcesConfig::default());
        }
        Err(source) => {
            return Err(ArchiveFsError::io(path.to_path_buf(), source));
        }
    };
    if text.trim().is_empty() {
        return Ok(CheatSourcesConfig::default());
    }
    let config: CheatSourcesConfig = toml::from_str(&text)
        .map_err(|e| ArchiveFsError::Config(format!("failed to parse {}: {e}", path.display())))?;
    Ok(config)
}

pub fn save_cheat_sources_config_default(
    config: &CheatSourcesConfig,
) -> Result<(), ArchiveFsError> {
    save_cheat_sources_config_to(default_cheat_sources_config_path()?, config)
}

pub fn save_cheat_sources_config_to(
    path: impl AsRef<Path>,
    config: &CheatSourcesConfig,
) -> Result<(), ArchiveFsError> {
    let path = path.as_ref();
    let header =
        "# ArchiveFS cheat source preferences\n# Only non-default values are recorded.\n\n";
    let body = toml::to_string_pretty(config).map_err(|e| {
        ArchiveFsError::Config(format!("failed to serialize cheat source config: {e}"))
    })?;
    let contents = format!("{header}{body}");
    crate::atomic_write_text(path, &contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "archivefs-cheat-sources-config-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn missing_file_returns_default() {
        let cfg =
            load_cheat_sources_config_from(test_root("missing").join("nonexistent.toml")).unwrap();
        assert_eq!(cfg, CheatSourcesConfig::default());
    }

    #[test]
    fn empty_file_returns_default() {
        let root = test_root("empty");
        let path = root.join("cheat_sources.toml");
        fs::write(&path, "").unwrap();
        let cfg = load_cheat_sources_config_from(&path).unwrap();
        assert_eq!(cfg, CheatSourcesConfig::default());
    }

    #[test]
    fn whitespace_only_file_returns_default() {
        let root = test_root("whitespace");
        let path = root.join("cheat_sources.toml");
        fs::write(&path, "  \n  \t  \n").unwrap();
        let cfg = load_cheat_sources_config_from(&path).unwrap();
        assert_eq!(cfg, CheatSourcesConfig::default());
    }

    #[test]
    fn round_trip_preserves_providers() {
        let root = test_root("roundtrip");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: Some(vec![
                ProviderConfigEntry {
                    id: "bsfree-archive".to_string(),
                    enabled: Some(false),
                    priority: None,
                },
                ProviderConfigEntry {
                    id: "libretro-buildbot-cheats".to_string(),
                    enabled: None,
                    priority: Some(50),
                },
            ]),
            platform_overrides: None,
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        let reloaded = load_cheat_sources_config_from(&path).unwrap();
        assert_eq!(reloaded, cfg);
    }

    #[test]
    fn serialized_output_includes_header() {
        let root = test_root("header");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig::default();
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("ArchiveFS cheat source preferences"));
    }

    #[test]
    fn zero_priority_is_preserved() {
        let root = test_root("zeropri");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: "test".to_string(),
                enabled: None,
                priority: Some(0),
            }]),
            platform_overrides: None,
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        let reloaded = load_cheat_sources_config_from(&path).unwrap();
        assert_eq!(reloaded.providers.unwrap()[0].priority, Some(0));
    }

    #[test]
    fn deny_unknown_fields_rejects_extra_keys() {
        let toml_str = r#"
[[providers]]
id = "test"
enabled = false
extra_field = "bad"
"#;
        let result: Result<CheatSourcesConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn platform_overrides_round_trip() {
        let root = test_root("platform");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "PlayStation2".to_string(),
                disabled_providers: Some(vec!["dolphin_upstream_gamesettings".to_string()]),
                priority_overrides: None,
            }]),
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        let reloaded = load_cheat_sources_config_from(&path).unwrap();
        assert_eq!(reloaded, cfg);
    }

    #[test]
    fn malformed_toml_returns_error() {
        let root = test_root("malformed");
        let path = root.join("cheat_sources.toml");
        fs::write(&path, "not valid toml {{[").unwrap();
        let result = load_cheat_sources_config_from(&path);
        assert!(result.is_err());
    }

    #[test]
    fn default_config_path_lives_in_config_dir() {
        let path = default_cheat_sources_config_path().unwrap();
        let s = path.to_string_lossy().into_owned();
        assert!(s.contains(".config/archivefs/cheat_sources.toml"));
    }

    #[test]
    fn save_creates_parent_directories() {
        let root = test_root("nested");
        let path = root.join("deep/nest/cheat_sources.toml");
        let cfg = CheatSourcesConfig::default();
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn an_unknown_provider_id_round_trips_through_the_file() {
        // The whole-file property: what comes back out must carry what went in,
        // including a provider no build in this workspace defines.
        let root = test_root("unknown-provider-file");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: Some(vec![
                ProviderConfigEntry {
                    id: "from-a-newer-build".to_string(),
                    enabled: Some(false),
                    priority: Some(123),
                },
                ProviderConfigEntry {
                    id: "bsfree-archive".to_string(),
                    enabled: Some(false),
                    priority: None,
                },
            ]),
            platform_overrides: None,
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        assert_eq!(load_cheat_sources_config_from(&path).unwrap(), cfg);
    }

    #[test]
    fn an_unresolvable_platform_round_trips_through_the_file() {
        let root = test_root("unknown-platform-file");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "SomePlatformThisBuildLacks".to_string(),
                disabled_providers: Some(vec!["whoever".to_string()]),
                priority_overrides: Some(vec![ProviderPriorityOverride {
                    id: "whoever".to_string(),
                    priority: 9,
                }]),
            }]),
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        assert_eq!(load_cheat_sources_config_from(&path).unwrap(), cfg);
    }

    #[test]
    fn a_second_save_of_reloaded_content_is_byte_identical() {
        // Once written, the representation must be a fixed point. A format
        // that drifts on every save would keep rewriting the user's file and
        // make "did anything change?" impossible to answer.
        let root = test_root("fixed-point");
        let path = root.join("cheat_sources.toml");
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: "unknown-to-this-build".to_string(),
                enabled: Some(false),
                priority: Some(77),
            }]),
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "AlsoUnknown".to_string(),
                disabled_providers: Some(vec!["x".to_string()]),
                priority_overrides: None,
            }]),
        };
        save_cheat_sources_config_to(&path, &cfg).unwrap();
        let first = fs::read_to_string(&path).unwrap();

        let reloaded = load_cheat_sources_config_from(&path).unwrap();
        save_cheat_sources_config_to(&path, &reloaded).unwrap();
        let second = fs::read_to_string(&path).unwrap();

        assert_eq!(first, second, "saving reloaded content must not drift");
    }

    // ---- Durable write --------------------------------------------------

    #[test]
    fn a_save_leaves_no_temp_file_behind() {
        let root = test_root("no-temp-residue");
        let path = root.join("cheat_sources.toml");
        save_cheat_sources_config_to(&path, &CheatSourcesConfig::default()).unwrap();

        let strays: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "cheat_sources.toml")
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    #[test]
    fn a_failed_save_leaves_the_previous_file_intact() {
        // The failure that matters: a write that cannot complete must never
        // truncate or remove what was already there. Here the target path is
        // a directory, so the rename cannot succeed.
        let root = test_root("failed-save");
        let good_path = root.join("cheat_sources.toml");
        let original = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: "bsfree-archive".to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            platform_overrides: None,
        };
        save_cheat_sources_config_to(&good_path, &original).unwrap();
        let before = fs::read_to_string(&good_path).unwrap();

        let blocked = root.join("a-directory-not-a-file");
        fs::create_dir_all(&blocked).unwrap();
        let result = save_cheat_sources_config_to(&blocked, &CheatSourcesConfig::default());
        assert!(result.is_err(), "writing over a directory must fail");

        assert_eq!(
            fs::read_to_string(&good_path).unwrap(),
            before,
            "an unrelated failure must not disturb a valid file"
        );
        assert_eq!(
            load_cheat_sources_config_from(&good_path).unwrap(),
            original
        );
    }

    #[test]
    fn overwriting_replaces_content_without_leaving_a_tail() {
        // rename-based replacement, not truncate-and-write: a shorter document
        // must not leave the tail of a longer one behind.
        let root = test_root("no-tail");
        let path = root.join("cheat_sources.toml");
        let long = CheatSourcesConfig {
            providers: Some(vec![
                ProviderConfigEntry {
                    id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                    enabled: Some(false),
                    priority: Some(11),
                },
                ProviderConfigEntry {
                    id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                    enabled: Some(false),
                    priority: Some(22),
                },
            ]),
            platform_overrides: None,
        };
        save_cheat_sources_config_to(&path, &long).unwrap();
        save_cheat_sources_config_to(&path, &CheatSourcesConfig::default()).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("aaaaaaaa"), "stale tail remained: {text}");
        assert_eq!(
            load_cheat_sources_config_from(&path).unwrap(),
            CheatSourcesConfig::default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_files_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root("perms");
        let path = root.join("cheat_sources.toml");
        save_cheat_sources_config_to(&path, &CheatSourcesConfig::default()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        save_cheat_sources_config_to(
            &path,
            &CheatSourcesConfig {
                providers: Some(vec![ProviderConfigEntry {
                    id: "bsfree-archive".to_string(),
                    enabled: Some(false),
                    priority: None,
                }]),
                platform_overrides: None,
            },
        )
        .unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "a tightened config must not be widened by a save"
        );
    }

    #[test]
    fn the_written_format_gains_no_new_keys() {
        // The compatibility floor for Milestone 1: `CheatSourcesConfig` is
        // `deny_unknown_fields`, so any key added here makes the file
        // unreadable by every already-released build. This asserts the written
        // vocabulary is exactly the shipped one - in particular that no
        // `format_version` has crept in.
        let root = test_root("format-stability");
        let path = root.join("cheat_sources.toml");
        save_cheat_sources_config_to(
            &path,
            &CheatSourcesConfig {
                providers: Some(vec![ProviderConfigEntry {
                    id: "bsfree-archive".to_string(),
                    enabled: Some(false),
                    priority: Some(42),
                }]),
                platform_overrides: Some(vec![PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec!["x".to_string()]),
                    priority_overrides: Some(vec![ProviderPriorityOverride {
                        id: "x".to_string(),
                        priority: 5,
                    }]),
                }]),
            },
        )
        .unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let keys: std::collections::BTreeSet<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('['))
            .filter_map(|line| line.split('=').next())
            .map(str::trim)
            .collect();

        let permitted: std::collections::BTreeSet<&str> = [
            "id",
            "enabled",
            "priority",
            "platform",
            "disabled_providers",
        ]
        .into_iter()
        .collect();
        let unexpected: Vec<&&str> = keys.difference(&permitted).collect();
        assert!(
            unexpected.is_empty(),
            "a new key would break every released build that reads this file: {unexpected:?}"
        );
        assert!(
            !text.contains("format_version"),
            "no version field may be written in Milestone 1"
        );
    }

    #[test]
    fn an_absent_home_yields_an_error_not_a_relative_path() {
        // Tested through the seam rather than by removing HOME from the
        // process: these tests run in parallel with others that read it, and
        // mutating it made unrelated tests fail depending on scheduling.
        let result = cheat_sources_config_path_in(None);
        assert!(result.is_err(), "an absent HOME must not resolve to a path");
    }

    #[test]
    fn a_home_yields_the_documented_path() {
        let path = cheat_sources_config_path_in(Some(OsString::from("/home/example")))
            .expect("a home resolves");
        assert_eq!(
            path,
            PathBuf::from("/home/example/.config/archivefs/cheat_sources.toml")
        );
    }
}
