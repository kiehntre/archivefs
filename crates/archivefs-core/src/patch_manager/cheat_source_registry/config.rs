//! Persistent per-user cheat-source preferences.
//!
//! The configuration file lives at `~/.config/archivefs/cheat_sources.toml`.
//! When the file is absent every built-in provider is enabled at its default
//! priority. Only user overrides are persisted; entries that match defaults
//! are omitted from save output.

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
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
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
}
