//! Read-only registry of all known cheat sources.
//!
//! The registry is a flat list of `CheatSourceEntry` values that describe
//! every built-in provider and (in future stages) every user-configured
//! custom source. It wraps and describes providers; it does not mutate them.
//!
//! ## Provider counting
//!
//! Nine registry entries covering six distinct upstream projects and eight
//! logical data sources:
//!
//! - libretro-buildbot-cheats (libretro/libretro-database)
//! - pcsx2-official-patches-tree (PCSX2/pcsx2_patches)
//! - gamehacking.org split into three platform-specific registry entries
//!   (PS2, GameCube, Wii) because the upstream treats them as separate
//!   platforms with different matching, caching, and scraping paths
//! - dolphin_upstream_gamesettings + dolphin_upstream_catalogue
//!   (two distinct Dolphin sources sharing the same upstream repository)
//! - xenia_canary_game_patches (xenia-canary/game-patches)
//! - bsfree-archive (Andrew Mackrodt's BSFree Archive)
//!
//! The three figures differ for different reasons. `upstream_project` names a
//! repository, and gamehacking.org accounts for three entries while
//! dolphin-emu/dolphin accounts for two, so nine entries name only six
//! repositories between them - that is the number
//! `the_registry_covers_six_distinct_upstream_projects` derives from the entries
//! and pins. "Logical data sources" counts the distinct bodies of data rather
//! than the repositories or the entries; it is an editorial figure with no field
//! behind it, so nothing asserts it.

pub mod capabilities;
pub mod config;
pub mod health;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use capabilities::CheatSourceCapabilities;
pub use config::{
    CheatSourcesConfig, PlatformOverrideEntry, ProviderConfigEntry, ProviderPriorityOverride,
    default_cheat_sources_config_path, load_cheat_sources_config_default,
    load_cheat_sources_config_from, save_cheat_sources_config_default,
    save_cheat_sources_config_to,
};
pub use health::CheatSourceHealth;

use super::bsfree::{BSFREE_PROVIDER_ID, BSFREE_UPSTREAM_PROJECT};
use super::cheat_sources::trusted_retroarch_cheat_sources;
use super::dolphin_cheat_catalogue::{DOLPHIN_CATALOGUE_PROVIDER_ID, DOLPHIN_CATALOGUE_REPOSITORY};
use super::dolphin_gecko_provider::{
    DOLPHIN_UPSTREAM_PROVIDER_ID, DOLPHIN_UPSTREAM_PROVIDER_NAME, DOLPHIN_UPSTREAM_REPOSITORY,
};
const GAMEHACKING_PS2_REGISTRY_ID: &str = "gamehacking.org-ps2";
const GAMEHACKING_GAMECUBE_REGISTRY_ID: &str = "gamehacking.org-gamecube";
const GAMEHACKING_WII_REGISTRY_ID: &str = "gamehacking.org-wii";
use super::BUILT_IN_SOURCE_ID;
use super::xenia_provider::XENIA_PROVIDER_ID;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatSourceSpec {
    pub id: String,
    pub display_name: String,
    pub emulator: String,
    pub platforms: Vec<String>,
    pub capabilities: CheatSourceCapabilities,
    pub upstream_project: String,
    pub default_priority: u32,
    pub description: String,
}

/// A source's runtime state.
///
/// `health` is `None` until a health check has been performed.
/// `None` means "not yet checked" — distinct from a known
/// `CheatSourceHealth` with a concrete state. Callers that need a
/// health display should treat `None` as "unknown".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatSourceEntry {
    pub spec: CheatSourceSpec,
    pub enabled: bool,
    pub priority: u32,
    /// `None` = health not yet checked. Runtime health probing is deferred
    /// to a future stage. The registry never populates this field; it only
    /// carries whatever a caller sets.
    pub health: Option<CheatSourceHealth>,
}

impl CheatSourceEntry {
    pub fn from_spec(spec: CheatSourceSpec) -> Self {
        Self {
            priority: spec.default_priority,
            enabled: true,
            health: None,
            spec,
        }
    }
}

/// Two registry entries claimed the same source ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateSourceId {
    pub id: String,
    /// Where the ID was first seen, and where it was seen again.
    pub first_index: usize,
    pub duplicate_index: usize,
}

impl std::fmt::Display for DuplicateSourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "duplicate cheat source ID '{}': entries {} and {} both claim it",
            self.id, self.first_index, self.duplicate_index
        )
    }
}

impl std::error::Error for DuplicateSourceId {}

#[derive(Debug, Clone)]
pub struct CheatSourceRegistry {
    entries: Vec<CheatSourceEntry>,
    by_id: BTreeMap<String, usize>,
    platform_overrides: Vec<PlatformOverrideEntry>,
}

impl CheatSourceRegistry {
    /// Builds a registry, refusing two entries that claim the same ID.
    ///
    /// The ID is what every lookup, every preference and every CLI argument
    /// names a source by. Letting a later entry overwrite an earlier one left the
    /// first source present in `entries` but unreachable through `get`, so it
    /// could still be listed and still be counted while nothing could enable,
    /// disable or re-prioritise it. Today's built-in IDs are unique; the point of
    /// refusing is that a custom source added later cannot quietly displace a
    /// built-in one.
    pub fn new(entries: Vec<CheatSourceEntry>) -> Result<Self, DuplicateSourceId> {
        let mut by_id = BTreeMap::new();
        for (idx, entry) in entries.iter().enumerate() {
            if let Some(&first) = by_id.get(&entry.spec.id) {
                return Err(DuplicateSourceId {
                    id: entry.spec.id.clone(),
                    first_index: first,
                    duplicate_index: idx,
                });
            }
            by_id.insert(entry.spec.id.clone(), idx);
        }
        Ok(Self {
            entries,
            by_id,
            platform_overrides: Vec::new(),
        })
    }

    /// Every registered source, enabled or not, in the order `list` shows them.
    ///
    /// Sorted the same way as [`Self::sorted_enabled`], so enabling a source does
    /// not move it: it is already in the position it will occupy.
    pub fn sorted_all(&self) -> Vec<&CheatSourceEntry> {
        let mut all: Vec<&CheatSourceEntry> = self.entries.iter().collect();
        all.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.spec.id.cmp(&b.spec.id))
        });
        all
    }

    pub fn entries(&self) -> &[CheatSourceEntry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&CheatSourceEntry> {
        self.by_id.get(id).map(|&idx| &self.entries[idx])
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut CheatSourceEntry> {
        self.by_id
            .get(id)
            .copied()
            .map(|idx| &mut self.entries[idx])
    }

    pub fn sorted_enabled(&self) -> Vec<&CheatSourceEntry> {
        let mut enabled: Vec<&CheatSourceEntry> =
            self.entries.iter().filter(|e| e.enabled).collect();
        enabled.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.spec.id.cmp(&b.spec.id))
        });
        enabled
    }

    pub fn sorted_enabled_for_platform(&self, platform_id: &str) -> Vec<&CheatSourceEntry> {
        let normalized = crate::canonical_platform_for_alias(platform_id).unwrap_or(platform_id);

        let overrides = self.find_platform_override(normalized);

        let disabled_set: BTreeMap<&str, bool> = overrides
            .and_then(|o| o.disabled_providers.as_ref())
            .map(|ids| ids.iter().map(|id| (id.as_str(), true)).collect())
            .unwrap_or_default();

        let priority_overrides: BTreeMap<&str, u32> = overrides
            .and_then(|o| o.priority_overrides.as_ref())
            .map(|pos| {
                pos.iter()
                    .filter_map(|po| {
                        if self.by_id.contains_key(po.id.as_str()) {
                            Some((po.id.as_str(), po.priority.clamp(1, 999)))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut entries: Vec<&CheatSourceEntry> = self
            .entries
            .iter()
            .filter(|e| {
                if !e.enabled {
                    return false;
                }
                if disabled_set.contains_key(e.spec.id.as_str()) {
                    return false;
                }
                if e.spec.platforms.is_empty() {
                    return true;
                }
                e.spec.platforms.iter().any(|p| p == normalized)
            })
            .collect();

        entries.sort_by(|a, b| {
            let a_pri = priority_overrides
                .get(a.spec.id.as_str())
                .copied()
                .unwrap_or(a.priority);
            let b_pri = priority_overrides
                .get(b.spec.id.as_str())
                .copied()
                .unwrap_or(b.priority);
            a_pri.cmp(&b_pri).then_with(|| a.spec.id.cmp(&b.spec.id))
        });
        entries
    }

    fn find_platform_override(&self, normalized_platform: &str) -> Option<&PlatformOverrideEntry> {
        self.platform_overrides.iter().rev().find(|o| {
            if let Some(canon) = crate::canonical_platform_for_alias(&o.platform) {
                canon == normalized_platform
            } else {
                false
            }
        })
    }

    pub fn apply_config(&mut self, cfg: &CheatSourcesConfig) {
        for provider_cfg in cfg.providers.iter().flatten() {
            if let Some(entry) = self.get_mut(&provider_cfg.id) {
                if let Some(enabled) = provider_cfg.enabled {
                    entry.enabled = enabled;
                }
                if let Some(priority) = provider_cfg.priority {
                    entry.priority = priority.clamp(1, 999);
                }
            }
        }
        self.platform_overrides = cfg.platform_overrides.clone().unwrap_or_default();
    }

    pub fn to_config(&self) -> CheatSourcesConfig {
        let providers: Vec<config::ProviderConfigEntry> = self
            .entries
            .iter()
            .filter(|e| !e.enabled || e.priority != e.spec.default_priority)
            .map(|e| config::ProviderConfigEntry {
                id: e.spec.id.clone(),
                enabled: if e.enabled { None } else { Some(false) },
                priority: if e.priority == e.spec.default_priority {
                    None
                } else {
                    Some(e.priority)
                },
            })
            .collect();
        let providers = if providers.is_empty() {
            None
        } else {
            Some(providers)
        };
        let platform_overrides = if self.platform_overrides.is_empty() {
            None
        } else {
            Some(self.platform_overrides.clone())
        };
        CheatSourcesConfig {
            providers,
            platform_overrides,
        }
    }
}

pub fn build_default_registry() -> CheatSourceRegistry {
    let mut entries = Vec::new();

    // 1. libretro-buildbot-cheats
    {
        let sources = trusted_retroarch_cheat_sources();
        if let Some(def) = sources.first() {
            entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
                id: def.source_id.clone(),
                display_name: def.display_name.clone(),
                emulator: "RetroArch".to_string(),
                platforms: vec![],
                capabilities: CheatSourceCapabilities::remote_download_and_install(),
                upstream_project: "libretro/libretro-database".to_string(),
                default_priority: 10,
                description: "Official Libretro cheat database; resolves master to pinned commit, downloads immutable ZIP snapshots with SHA-256 verification".to_string(),
            }));
        }
    }

    // 2. pcsx2-official-patches-tree
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: BUILT_IN_SOURCE_ID.to_string(),
        display_name: "PCSX2 official patch repository metadata".to_string(),
        emulator: "PCSX2".to_string(),
        platforms: vec!["PS2".to_string()],
        capabilities: CheatSourceCapabilities::local_read_only(),
        upstream_project: "PCSX2/pcsx2_patches".to_string(),
        default_priority: 20,
        description:
            "Read-only catalogue of official PCSX2 patches matched by CRC and serial identity"
                .to_string(),
    }));

    // 3. gamehacking.org (PS2)
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: GAMEHACKING_PS2_REGISTRY_ID.to_string(),
        display_name: "GameHacking.org (PS2)".to_string(),
        emulator: "PCSX2".to_string(),
        platforms: vec!["PS2".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_and_install(),
        upstream_project: "gamehacking.org".to_string(),
        default_priority: 30,
        description: "GameHacking.org PS2 cheat database, fetched over HTTPS with Cloudflare-detection cooldown".to_string(),
    }));

    // 4. gamehacking.org (GameCube)
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: GAMEHACKING_GAMECUBE_REGISTRY_ID.to_string(),
        display_name: "GameHacking.org (GameCube)".to_string(),
        emulator: "Dolphin".to_string(),
        platforms: vec!["GameCube".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_and_install(),
        upstream_project: "gamehacking.org".to_string(),
        default_priority: 40,
        description: "GameHacking.org GameCube cheat database, matched by Dolphin Game ID with code-format auditing".to_string(),
    }));

    // 5. gamehacking.org (Wii)
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: GAMEHACKING_WII_REGISTRY_ID.to_string(),
        display_name: "GameHacking.org (Wii)".to_string(),
        emulator: "Dolphin".to_string(),
        platforms: vec!["Wii".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_and_install(),
        upstream_project: "gamehacking.org".to_string(),
        default_priority: 50,
        description:
            "GameHacking.org Wii cheat database, matched by Wii Game ID with safety classification"
                .to_string(),
    }));

    // 6. dolphin_upstream_gamesettings
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: DOLPHIN_UPSTREAM_PROVIDER_ID.to_string(),
        display_name: DOLPHIN_UPSTREAM_PROVIDER_NAME.to_string(),
        emulator: "Dolphin".to_string(),
        platforms: vec!["GameCube".to_string(), "Wii".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_and_install(),
        upstream_project: DOLPHIN_UPSTREAM_REPOSITORY.to_string(),
        default_priority: 60,
        description:
            "Per-game Gecko and ActionReplay codes fetched from Dolphin upstream GameSettings on master"
                .to_string(),
    }));

    // 7. dolphin_upstream_catalogue
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: DOLPHIN_CATALOGUE_PROVIDER_ID.to_string(),
        display_name: "Dolphin upstream catalogue".to_string(),
        emulator: "Dolphin".to_string(),
        platforms: vec!["GameCube".to_string(), "Wii".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_read_only(),
        upstream_project: DOLPHIN_CATALOGUE_REPOSITORY.to_string(),
        default_priority: 65,
        description: "Offline indexed catalogue of the entire Dolphin upstream GameSettings tree, pinned to a resolved commit".to_string(),
    }));

    // 8. xenia_canary_game_patches
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: XENIA_PROVIDER_ID.to_string(),
        display_name: "Xenia Canary game-patches".to_string(),
        emulator: "Xenia".to_string(),
        platforms: vec!["Xbox360".to_string()],
        capabilities: CheatSourceCapabilities::remote_download_and_install(),
        upstream_project: "xenia-canary/game-patches".to_string(),
        default_priority: 70,
        description: "Xenia Canary .patch.toml files fetched from upstream repository, matched by Title ID and Media ID".to_string(),
    }));

    // 9. bsfree-archive
    entries.push(CheatSourceEntry::from_spec(CheatSourceSpec {
        id: BSFREE_PROVIDER_ID.to_string(),
        display_name: "BSFree Archive".to_string(),
        emulator: "Multi".to_string(),
        platforms: vec![],
        capabilities: CheatSourceCapabilities::read_only_browse(),
        upstream_project: BSFREE_UPSTREAM_PROJECT.to_string(),
        default_priority: 100,
        description:
            "Andrew Mackrodt's BSFree Archive: a cross-platform read-only SQLite cheat database"
                .to_string(),
    }));

    CheatSourceRegistry::new(entries)
        .expect("the built-in registry is a fixed list with unique IDs; a duplicate here is a bug")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_entries_claiming_one_id_are_rejected() {
        // Silently keeping the last one left the first present in `entries` but
        // unreachable through `get`, so it could be listed and counted while
        // nothing could enable, disable or re-prioritise it.
        let registry = build_default_registry();
        let mut entries: Vec<CheatSourceEntry> = registry.entries().to_vec();
        let mut clone = entries[0].clone();
        clone.spec.display_name = "An impostor claiming a taken ID".to_string();
        let taken = clone.spec.id.clone();
        entries.push(clone);

        let error = CheatSourceRegistry::new(entries).expect_err("a duplicate ID must be refused");
        assert_eq!(error.id, taken);
        assert!(
            error.to_string().contains(&taken),
            "the message must name the duplicate ID, got {error}"
        );
        assert_ne!(
            error.first_index, error.duplicate_index,
            "the error should point at both entries"
        );
    }

    #[test]
    fn unique_ids_are_accepted() {
        let entries: Vec<CheatSourceEntry> = build_default_registry().entries().to_vec();
        let count = entries.len();
        let registry = CheatSourceRegistry::new(entries).expect("unique IDs are fine");
        assert_eq!(registry.entries().len(), count);
    }

    #[test]
    fn sorted_all_includes_disabled_entries_in_the_same_order() {
        // What `cheat-source list` relies on: disabling a source must not remove
        // it from the listing, nor move anything else.
        let mut registry = build_default_registry();
        let before: Vec<String> = registry
            .sorted_all()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        let victim = before[0].clone();
        registry.get_mut(&victim).expect("entry").enabled = false;

        let after: Vec<String> = registry
            .sorted_all()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        assert_eq!(before, after, "disabling a source reordered the listing");
        assert_eq!(
            registry.sorted_enabled().len(),
            after.len() - 1,
            "exactly one source should have left the enabled set"
        );
    }

    #[test]
    fn the_registry_covers_six_distinct_upstream_projects() {
        // The module doc gave two different numbers for this - "six" in one
        // sentence and "Eight" in another. Six is what the registry produces:
        // three entries share gamehacking.org and two share dolphin-emu/dolphin.
        // Deriving it from the entries is what stops the prose drifting again.
        let registry = build_default_registry();
        let upstreams: std::collections::BTreeSet<&str> = registry
            .entries
            .iter()
            .map(|entry| entry.spec.upstream_project.as_str())
            .collect();
        assert_eq!(
            upstreams.len(),
            6,
            "expected six distinct upstream projects, got {upstreams:?}"
        );
        // Nine entries over those six repositories.
        assert_eq!(registry.entries.len(), 9);
    }

    #[test]
    fn default_registry_contains_nine_entries() {
        let registry = build_default_registry();
        assert_eq!(registry.entries.len(), 9);
    }

    #[test]
    fn default_registry_ids_are_unique() {
        let registry = build_default_registry();
        let mut ids: Vec<&str> = registry
            .entries
            .iter()
            .map(|e| e.spec.id.as_str())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 9);
    }

    #[test]
    fn default_registry_has_expected_ids() {
        let registry = build_default_registry();
        assert!(registry.get("libretro-buildbot-cheats").is_some());
        assert!(registry.get("pcsx2-official-patches-tree").is_some());
        assert!(registry.get("gamehacking.org-ps2").is_some());
        assert!(registry.get("gamehacking.org-gamecube").is_some());
        assert!(registry.get("gamehacking.org-wii").is_some());
        assert!(registry.get("dolphin_upstream_gamesettings").is_some());
        assert!(registry.get("dolphin_upstream_catalogue").is_some());
        assert!(registry.get("xenia_canary_game_patches").is_some());
        assert!(registry.get("bsfree-archive").is_some());
    }

    #[test]
    fn sorted_enabled_respects_priority_then_id() {
        let mut registry = build_default_registry();
        registry.get_mut("bsfree-archive").unwrap().priority = 5;
        let sorted = registry.sorted_enabled();
        assert_eq!(sorted[0].spec.id, "bsfree-archive");
        assert_eq!(sorted[1].spec.id, "libretro-buildbot-cheats");
    }

    #[test]
    fn sorted_enabled_for_platform_filters_ps2() {
        let registry = build_default_registry();
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        let ids: Vec<&str> = ps2.iter().map(|e| e.spec.id.as_str()).collect();
        assert!(ids.contains(&"pcsx2-official-patches-tree"));
        assert!(ids.contains(&"gamehacking.org-ps2"));
        assert!(!ids.contains(&"xenia_canary_game_patches"));
        assert!(!ids.contains(&"dolphin_upstream_gamesettings"));
    }

    #[test]
    fn sorted_enabled_for_platform_includes_empty_platforms() {
        let registry = build_default_registry();
        let all = registry.sorted_enabled_for_platform("PS2");
        let ids: Vec<&str> = all.iter().map(|e| e.spec.id.as_str()).collect();
        assert!(ids.contains(&"libretro-buildbot-cheats"));
        assert!(ids.contains(&"bsfree-archive"));
    }

    #[test]
    fn apply_config_disables_provider() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: Some(vec![config::ProviderConfigEntry {
                id: "bsfree-archive".to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            ..CheatSourcesConfig::default()
        };
        registry.apply_config(&cfg);
        assert!(!registry.get("bsfree-archive").unwrap().enabled);
        assert!(registry.get("libretro-buildbot-cheats").unwrap().enabled);
    }

    #[test]
    fn apply_config_changes_priority() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: Some(vec![config::ProviderConfigEntry {
                id: "libretro-buildbot-cheats".to_string(),
                enabled: None,
                priority: Some(50),
            }]),
            ..CheatSourcesConfig::default()
        };
        registry.apply_config(&cfg);
        assert_eq!(
            registry.get("libretro-buildbot-cheats").unwrap().priority,
            50
        );
    }

    #[test]
    fn apply_config_ignores_unknown_provider_id() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: Some(vec![config::ProviderConfigEntry {
                id: "non-existent-provider".to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            ..CheatSourcesConfig::default()
        };
        registry.apply_config(&cfg);
        assert_eq!(registry.entries.len(), 9);
    }

    #[test]
    fn apply_config_clamps_priority() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: Some(vec![config::ProviderConfigEntry {
                id: "bsfree-archive".to_string(),
                enabled: None,
                priority: Some(0),
            }]),
            ..CheatSourcesConfig::default()
        };
        registry.apply_config(&cfg);
        assert_eq!(registry.get("bsfree-archive").unwrap().priority, 1);
    }

    #[test]
    fn to_config_only_writes_non_default_values() {
        let registry = build_default_registry();
        let cfg = registry.to_config();
        assert!(cfg.providers.is_none());
        assert!(cfg.platform_overrides.is_none());
    }

    #[test]
    fn to_config_writes_disabled_provider() {
        let mut registry = build_default_registry();
        registry.get_mut("bsfree-archive").unwrap().enabled = false;
        let cfg = registry.to_config();
        let providers = cfg.providers.unwrap();
        let bsfree = providers.iter().find(|p| p.id == "bsfree-archive").unwrap();
        assert_eq!(bsfree.enabled, Some(false));
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let registry = build_default_registry();
        assert!(registry.get("not-a-provider").is_none());
    }

    #[test]
    fn sorted_enabled_excludes_disabled() {
        let mut registry = build_default_registry();
        registry
            .get_mut("libretro-buildbot-cheats")
            .unwrap()
            .enabled = false;
        let enabled = registry.sorted_enabled();
        let ids: Vec<&str> = enabled.iter().map(|e| e.spec.id.as_str()).collect();
        assert!(!ids.contains(&"libretro-buildbot-cheats"));
        assert!(ids.contains(&"pcsx2-official-patches-tree"));
    }

    #[test]
    fn priority_ties_broken_by_id() {
        let mut registry = build_default_registry();
        registry
            .get_mut("dolphin_upstream_gamesettings")
            .unwrap()
            .priority = 10;
        registry
            .get_mut("pcsx2-official-patches-tree")
            .unwrap()
            .priority = 10;
        registry.get_mut("bsfree-archive").unwrap().priority = 10;
        let sorted = registry.sorted_enabled();
        let ids: Vec<&str> = sorted.iter().map(|e| e.spec.id.as_str()).collect();
        assert_eq!(ids[0], "bsfree-archive");
        assert_eq!(ids[1], "dolphin_upstream_gamesettings");
        assert!(ids[2..].contains(&"pcsx2-official-patches-tree"));
    }

    #[test]
    fn gamehacking_org_appears_three_times_with_distinct_registry_ids() {
        let registry = build_default_registry();
        let gh_entries: Vec<&CheatSourceEntry> = registry
            .entries()
            .iter()
            .filter(|e| e.spec.display_name.starts_with("GameHacking.org"))
            .collect();
        assert_eq!(gh_entries.len(), 3);
        let ids: Vec<&str> = gh_entries.iter().map(|e| e.spec.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "gamehacking.org-ps2",
                "gamehacking.org-gamecube",
                "gamehacking.org-wii"
            ]
        );
    }

    // -----------------------------------------------------------------
    // Platform override tests
    // -----------------------------------------------------------------

    fn platform_override_cfg(
        platform: &str,
        disabled: &[&str],
        priority_overrides: &[(&str, u32)],
    ) -> CheatSourcesConfig {
        CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: platform.to_string(),
                disabled_providers: if disabled.is_empty() {
                    None
                } else {
                    Some(disabled.iter().map(|s| s.to_string()).collect())
                },
                priority_overrides: if priority_overrides.is_empty() {
                    None
                } else {
                    Some(
                        priority_overrides
                            .iter()
                            .map(|(id, pri)| ProviderPriorityOverride {
                                id: id.to_string(),
                                priority: *pri,
                            })
                            .collect(),
                    )
                },
            }]),
        }
    }

    #[test]
    fn globally_enabled_provider_remains_enabled_without_override() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &[], &[]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
    }

    #[test]
    fn globally_disabled_provider_is_not_re_enabled_by_platform_override() {
        let mut registry = build_default_registry();
        registry.get_mut("bsfree-archive").unwrap().enabled = false;
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "PS2".to_string(),
                disabled_providers: None,
                priority_overrides: None,
            }]),
        };
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(!ps2.iter().any(|e| e.spec.id == "bsfree-archive"));
    }

    #[test]
    fn platform_disabled_provider_omitted_for_that_platform_only() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &["gamehacking.org-ps2"], &[]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(!ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
        let all = registry.sorted_enabled();
        assert!(all.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
    }

    #[test]
    fn platform_priority_override_affects_ordering_only_for_that_platform() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &[], &[("libretro-buildbot-cheats", 999)]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        let ps2_ids: Vec<&str> = ps2.iter().map(|e| e.spec.id.as_str()).collect();
        let ps2_libretro_pos = ps2_ids
            .iter()
            .position(|id| *id == "libretro-buildbot-cheats")
            .unwrap();
        assert!(ps2_libretro_pos > 0, "libretro with pri 999 should be late");
        let all = registry.sorted_enabled();
        let all_libretro_pos = all
            .iter()
            .position(|e| e.spec.id == "libretro-buildbot-cheats")
            .unwrap();
        assert!(
            all_libretro_pos < all.len() - 1,
            "libretro should be at default position globally"
        );
        assert_ne!(ps2_libretro_pos, all_libretro_pos);
    }

    #[test]
    fn global_ordering_unchanged_for_non_overridden_platform() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &["gamehacking.org-ps2"], &[]);
        registry.apply_config(&cfg);
        let all = registry.sorted_enabled();
        let all_ids: Vec<&str> = all.iter().map(|e| e.spec.id.as_str()).collect();
        assert!(all_ids.contains(&"gamehacking.org-gamecube"));
        assert!(all_ids.contains(&"gamehacking.org-wii"));
        let gc = registry.sorted_enabled_for_platform("GameCube");
        assert!(gc.iter().any(|e| e.spec.id == "gamehacking.org-gamecube"));
    }

    #[test]
    fn equal_priorities_have_deterministic_tiebreaker() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg(
            "GameCube",
            &[],
            &[
                ("dolphin_upstream_gamesettings", 50),
                ("gamehacking.org-gamecube", 50),
            ],
        );
        registry.apply_config(&cfg);
        let gc = registry.sorted_enabled_for_platform("GameCube");
        let ids: Vec<&str> = gc.iter().map(|e| e.spec.id.as_str()).collect();
        let d_index = ids
            .iter()
            .position(|id| *id == "dolphin_upstream_gamesettings")
            .unwrap();
        let gh_index = ids
            .iter()
            .position(|id| *id == "gamehacking.org-gamecube")
            .unwrap();
        assert_eq!(
            ids[d_index].cmp(ids[gh_index]),
            std::cmp::Ordering::Less,
            "ties broken by ID: dolphin_upstream_gamesettings < gamehacking.org-gamecube"
        );
    }

    #[test]
    fn unknown_provider_ids_in_disabled_list_do_not_affect_valid_providers() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &["non-existent", "another-fake"], &[]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
        assert!(
            ps2.iter()
                .any(|e| e.spec.id == "pcsx2-official-patches-tree")
        );
    }

    #[test]
    fn unknown_provider_ids_in_priority_overrides_do_not_affect_valid_providers() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("PS2", &[], &[("non-existent", 1), ("another-fake", 2)]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
    }

    #[test]
    fn duplicate_platform_overrides_last_wins() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec!["gamehacking.org-ps2".to_string()]),
                    priority_overrides: None,
                },
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: None,
                    priority_overrides: None,
                },
            ]),
        };
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(
            ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"),
            "last override clears disabled list, so provider appears"
        );
    }

    #[test]
    fn duplicate_priority_overrides_last_wins() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "PS2".to_string(),
                disabled_providers: None,
                priority_overrides: Some(vec![
                    ProviderPriorityOverride {
                        id: "libretro-buildbot-cheats".to_string(),
                        priority: 1,
                    },
                    ProviderPriorityOverride {
                        id: "libretro-buildbot-cheats".to_string(),
                        priority: 999,
                    },
                ]),
            }]),
        };
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        let libretro_pos = ps2
            .iter()
            .position(|e| e.spec.id == "libretro-buildbot-cheats")
            .unwrap();
        assert!(
            libretro_pos > 1,
            "libretro with 999 priority should be sorted late"
        );
    }

    #[test]
    fn platform_override_with_unrecognized_platform_name_has_no_effect() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("NotARealPlatform", &["bsfree-archive"], &[]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(ps2.iter().any(|e| e.spec.id == "bsfree-archive"));
    }

    #[test]
    fn platform_normalized_by_alias() {
        let mut registry = build_default_registry();
        let cfg = platform_override_cfg("ps2", &["gamehacking.org-ps2"], &[]);
        registry.apply_config(&cfg);
        let ps2 = registry.sorted_enabled_for_platform("PS2");
        assert!(!ps2.iter().any(|e| e.spec.id == "gamehacking.org-ps2"));
    }

    #[test]
    fn to_config_preserves_platform_overrides_round_trip() {
        let mut registry = build_default_registry();
        let cfg = CheatSourcesConfig {
            providers: Some(vec![config::ProviderConfigEntry {
                id: "bsfree-archive".to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            platform_overrides: Some(vec![
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec!["gamehacking.org-ps2".to_string()]),
                    priority_overrides: Some(vec![ProviderPriorityOverride {
                        id: "libretro-buildbot-cheats".to_string(),
                        priority: 500,
                    }]),
                },
                PlatformOverrideEntry {
                    platform: "GameCube".to_string(),
                    disabled_providers: None,
                    priority_overrides: None,
                },
            ]),
        };
        registry.apply_config(&cfg);
        let out = registry.to_config();
        assert_eq!(out, cfg);
    }

    #[test]
    fn health_is_none_by_default() {
        let registry = build_default_registry();
        for entry in registry.entries() {
            assert!(
                entry.health.is_none(),
                "built-in provider {} has unexpected health",
                entry.spec.id
            );
        }
    }

    #[test]
    fn health_none_means_not_yet_checked() {
        let entry = CheatSourceEntry::from_spec(CheatSourceSpec {
            id: "test".to_string(),
            display_name: "Test".to_string(),
            emulator: "Test".to_string(),
            platforms: vec![],
            capabilities: CheatSourceCapabilities::read_only_browse(),
            upstream_project: "test".to_string(),
            default_priority: 10,
            description: "test".to_string(),
        });
        assert!(entry.health.is_none());
    }
}
