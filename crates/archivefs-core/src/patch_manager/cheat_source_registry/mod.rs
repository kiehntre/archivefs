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

/// Per-platform participation for one source, as the GUI edits it.
///
/// A source can be off for one platform while still enabled everywhere else;
/// that is `disabled_providers`, and it is deliberately distinct from the
/// source-level `enabled` flag. Source-level off wins: a platform block can
/// subtract participation, never add it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformParticipation {
    /// False when a `disabled_providers` entry names this source.
    pub participating: bool,
    /// True when the source is off at source level, which no platform block
    /// can override. The GUI shows the platform control as inactive then.
    pub overridden_by_source_level: bool,
}

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

impl CheatSourceSpec {
    /// Whether this source covers `platform_id` at all.
    ///
    /// An empty `platforms` list means "every platform" - that is how the
    /// cross-platform sources are registered - so it is not the same as
    /// "covers nothing", and the two must not be conflated in a display.
    pub fn covers_platform(&self, platform_id: &str) -> bool {
        if self.platforms.is_empty() {
            return true;
        }
        let normalized = crate::canonical_platform_for_alias(platform_id).unwrap_or(platform_id);
        self.platforms.iter().any(|p| p == normalized)
    }

    /// Platform coverage for display: the listed platforms, or `None` when the
    /// source is not platform-specific.
    pub fn platform_coverage(&self) -> Option<&[String]> {
        if self.platforms.is_empty() {
            None
        } else {
            Some(&self.platforms)
        }
    }
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

/// A preferences entry this build cannot act on, kept exactly as written.
///
/// Not an error and not a warning about the file being wrong: the usual cause
/// is a provider a different ArchiveFS build knows about, or a typo the user
/// can fix. Either way the entry is inert - it never affects resolution - and
/// it is never rewritten or dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedPreference {
    pub kind: UnresolvedPreferenceKind,
    /// The identifier as the user wrote it, for display and for correcting.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedPreferenceKind {
    /// A `[[providers]]` entry naming a source that is not in the registry.
    UnknownProvider,
    /// A `[[platform_overrides]]` entry whose platform does not canonicalise,
    /// so it can never match. Its whole block is inert.
    UnresolvedPlatform,
    /// A `priority_overrides` line naming a source that is not in the
    /// registry, inside an otherwise resolvable platform block.
    UnknownPriorityOverride { platform: String },
    /// A `disabled_providers` entry naming a source that is not in the
    /// registry, inside an otherwise resolvable platform block.
    ///
    /// Kept separate from [`Self::UnknownPriorityOverride`] because the two
    /// send the user to different lines of their file: telling someone to look
    /// for a priority override that is not there wastes their time.
    UnknownDisabledProvider { platform: String },
}

impl UnresolvedPreference {
    /// One line of plain language, for a GUI list or a CLI note.
    pub fn describe(&self) -> String {
        match &self.kind {
            UnresolvedPreferenceKind::UnknownProvider => format!(
                "Provider '{}' is not one this build knows about. Kept as written.",
                self.detail
            ),
            UnresolvedPreferenceKind::UnresolvedPlatform => format!(
                "Platform '{}' was not recognised, so its overrides do nothing. Kept as written.",
                self.detail
            ),
            UnresolvedPreferenceKind::UnknownPriorityOverride { platform } => format!(
                "Priority override for unknown provider '{}' under platform '{platform}'. Kept as written.",
                self.detail
            ),
            UnresolvedPreferenceKind::UnknownDisabledProvider { platform } => format!(
                "Disabled entry for unknown provider '{}' under platform '{platform}'. Kept as written.",
                self.detail
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheatSourceRegistry {
    entries: Vec<CheatSourceEntry>,
    by_id: BTreeMap<String, usize>,
    platform_overrides: Vec<PlatformOverrideEntry>,
    /// `[[providers]]` entries whose ID matched no registry source, retained
    /// verbatim and in file order so [`Self::to_config`] can re-emit them.
    ///
    /// Without this the provider list was rebuilt from live entries only, and
    /// an unknown ID vanished the first time anything else was saved.
    unknown_providers: Vec<ProviderConfigEntry>,
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
            unknown_providers: Vec::new(),
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

    /// Applies user preferences, keeping every entry this build cannot act on.
    ///
    /// An entry naming an unknown provider is moved to `unknown_providers`
    /// rather than skipped, so [`Self::to_config`] can write it back
    /// unchanged. Platform overrides are already retained wholesale, including
    /// ones whose platform never canonicalises; they stay inert but present.
    pub fn apply_config(&mut self, cfg: &CheatSourcesConfig) {
        self.unknown_providers.clear();
        for provider_cfg in cfg.providers.iter().flatten() {
            match self.by_id.get(provider_cfg.id.as_str()).copied() {
                Some(idx) => {
                    let entry = &mut self.entries[idx];
                    if let Some(enabled) = provider_cfg.enabled {
                        entry.enabled = enabled;
                    }
                    if let Some(priority) = provider_cfg.priority {
                        entry.priority = priority.clamp(1, 999);
                    }
                }
                None => self.unknown_providers.push(provider_cfg.clone()),
            }
        }
        self.platform_overrides = cfg.platform_overrides.clone().unwrap_or_default();
    }

    /// Serialises current state back to the preferences shape.
    ///
    /// Known sources contribute a line only when they differ from their
    /// compiled-in default, which is what keeps an untouched file empty.
    /// Entries for unknown providers are appended verbatim: they have no
    /// default to compare against, so "differs from default" cannot be asked
    /// of them, and dropping them was the data loss this fixes.
    pub fn to_config(&self) -> CheatSourcesConfig {
        let mut providers: Vec<config::ProviderConfigEntry> = self
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
        providers.extend(self.unknown_providers.iter().cloned());

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

    /// `[[providers]]` entries retained because no registry source claims them.
    pub fn unknown_providers(&self) -> &[ProviderConfigEntry] {
        &self.unknown_providers
    }

    /// The platform-override blocks, exactly as loaded.
    pub fn platform_overrides(&self) -> &[PlatformOverrideEntry] {
        &self.platform_overrides
    }

    /// Replaces the platform-override blocks.
    ///
    /// Whole-list replacement rather than per-field edits: unresolved blocks
    /// are carried through by the caller passing them back untouched, which
    /// keeps "preserve what you did not edit" a property of one call site
    /// instead of a rule every mutation has to remember.
    pub fn set_platform_overrides(&mut self, overrides: Vec<PlatformOverrideEntry>) {
        self.platform_overrides = overrides;
    }

    /// Whether `source_id` currently participates for `platform_id`.
    ///
    /// Answers only the *policy* question. Whether the source's own
    /// `spec.platforms` covers the platform is a separate, non-editable fact;
    /// callers pair this with [`CheatSourceSpec::covers_platform`].
    pub fn platform_participation(
        &self,
        source_id: &str,
        platform_id: &str,
    ) -> PlatformParticipation {
        let overridden_by_source_level = self.get(source_id).map(|e| !e.enabled).unwrap_or(false);
        let normalized = crate::canonical_platform_for_alias(platform_id).unwrap_or(platform_id);
        let participating = !self
            .find_platform_override(normalized)
            .and_then(|block| block.disabled_providers.as_ref())
            .map(|ids| ids.iter().any(|id| id == source_id))
            .unwrap_or(false);
        PlatformParticipation {
            participating,
            overridden_by_source_level,
        }
    }

    /// Turns per-platform participation for one source on or off.
    ///
    /// Edits only the `disabled_providers` lists, leaving every block's other
    /// fields untouched - including blocks whose platform does not resolve. A
    /// block emptied of all content is removed so toggling a setting on and
    /// then off again does not leave an inert stub behind in the user's file.
    ///
    /// # Duplicate platform blocks
    ///
    /// A file may name one platform in several blocks (see
    /// `duplicate_platform_overrides_last_wins`). Resolution reads the *last*
    /// match, so the write has to agree with it or a toggle edits a block
    /// nobody is reading:
    ///
    /// - Turning participation **off** records it in the last matching block,
    ///   which is the one [`Self::find_platform_override`] will read back.
    /// - Turning it **on** clears the source from *every* matching block.
    ///   Removing it from only one would leave a later block still disabling
    ///   it, so the control would appear to do nothing.
    ///
    /// This makes the operation idempotent and makes the state the user sees
    /// after it the state that actually resolves.
    pub fn set_platform_participation(
        &mut self,
        source_id: &str,
        platform_id: &str,
        participating: bool,
    ) {
        let normalized = crate::canonical_platform_for_alias(platform_id)
            .unwrap_or(platform_id)
            .to_string();
        let matches_platform = |block: &PlatformOverrideEntry| {
            crate::canonical_platform_for_alias(&block.platform)
                .map(|canon| canon == normalized)
                .unwrap_or(false)
        };

        if participating {
            for block in self
                .platform_overrides
                .iter_mut()
                .filter(|block| matches_platform(block))
            {
                if let Some(disabled) = block.disabled_providers.as_mut() {
                    disabled.retain(|id| id != source_id);
                    if disabled.is_empty() {
                        block.disabled_providers = None;
                    }
                }
            }
            self.platform_overrides.retain(|block| {
                block.disabled_providers.is_some() || block.priority_overrides.is_some()
            });
            return;
        }

        // Last match, so the read sees what was just written.
        let existing = self.platform_overrides.iter().rposition(matches_platform);

        let idx = match existing {
            Some(idx) => idx,
            None => {
                self.platform_overrides.push(PlatformOverrideEntry {
                    platform: normalized,
                    disabled_providers: None,
                    priority_overrides: None,
                });
                self.platform_overrides.len() - 1
            }
        };

        let block = &mut self.platform_overrides[idx];
        let mut disabled = block.disabled_providers.take().unwrap_or_default();
        if !disabled.iter().any(|id| id == source_id) {
            disabled.push(source_id.to_string());
        }
        block.disabled_providers = if disabled.is_empty() {
            None
        } else {
            Some(disabled)
        };

        if block.disabled_providers.is_none() && block.priority_overrides.is_none() {
            self.platform_overrides.remove(idx);
        }
    }

    /// Everything in the loaded preferences this build cannot act on.
    ///
    /// Deterministic order: unknown providers in file order, then platform
    /// blocks in file order, each followed by its unknown priority overrides.
    pub fn unresolved_preferences(&self) -> Vec<UnresolvedPreference> {
        let mut out: Vec<UnresolvedPreference> = self
            .unknown_providers
            .iter()
            .map(|entry| UnresolvedPreference {
                kind: UnresolvedPreferenceKind::UnknownProvider,
                detail: entry.id.clone(),
            })
            .collect();

        for block in &self.platform_overrides {
            if crate::canonical_platform_for_alias(&block.platform).is_none() {
                // The whole block is inert, so its inner IDs are not reported
                // separately - one clear cause beats a cascade of symptoms.
                out.push(UnresolvedPreference {
                    kind: UnresolvedPreferenceKind::UnresolvedPlatform,
                    detail: block.platform.clone(),
                });
                continue;
            }
            for over in block.priority_overrides.iter().flatten() {
                if !self.by_id.contains_key(over.id.as_str()) {
                    out.push(UnresolvedPreference {
                        kind: UnresolvedPreferenceKind::UnknownPriorityOverride {
                            platform: block.platform.clone(),
                        },
                        detail: over.id.clone(),
                    });
                }
            }
            for id in block.disabled_providers.iter().flatten() {
                if !self.by_id.contains_key(id.as_str()) {
                    out.push(UnresolvedPreference {
                        kind: UnresolvedPreferenceKind::UnknownDisabledProvider {
                            platform: block.platform.clone(),
                        },
                        detail: id.clone(),
                    });
                }
            }
        }
        out
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

    // ---- Lossless round-trip -------------------------------------------
    //
    // The property under test throughout: loading preferences, changing
    // something this build understands, and saving must not delete anything
    // it does not understand. Before this, `to_config` rebuilt the provider
    // list from live registry entries alone, so an unknown ID disappeared the
    // first time any unrelated setting was saved.

    const UNKNOWN_ID: &str = "a-provider-from-some-other-build";
    const KNOWN_ID: &str = "bsfree-archive";

    #[test]
    fn an_unknown_provider_survives_load_edit_save() {
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: Some(42),
            }]),
            platform_overrides: None,
        };

        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        // Edit something entirely unrelated, the way a user would.
        registry.get_mut(KNOWN_ID).expect("known source").enabled = false;
        let saved = registry.to_config();

        let kept = saved
            .providers
            .expect("providers")
            .into_iter()
            .find(|p| p.id == UNKNOWN_ID)
            .expect("the unknown provider must survive the save");
        assert_eq!(kept.enabled, Some(false), "its value must be unchanged");
        assert_eq!(kept.priority, Some(42), "its value must be unchanged");
    }

    #[test]
    fn an_unknown_provider_does_not_affect_resolution() {
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: Some(1),
            }]),
            platform_overrides: None,
        };
        let mut registry = build_default_registry();
        let before: Vec<String> = registry
            .sorted_all()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        registry.apply_config(&cfg);
        let after: Vec<String> = registry
            .sorted_all()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        assert_eq!(before, after, "a retained unknown entry must stay inert");
    }

    #[test]
    fn an_unresolvable_platform_block_survives_load_edit_save() {
        let block = PlatformOverrideEntry {
            platform: "NotAPlatformThisBuildKnows".to_string(),
            disabled_providers: Some(vec![KNOWN_ID.to_string()]),
            priority_overrides: Some(vec![ProviderPriorityOverride {
                id: KNOWN_ID.to_string(),
                priority: 7,
            }]),
        };
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![block.clone()]),
        };

        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        registry.get_mut(KNOWN_ID).expect("known source").priority = 55;
        let saved = registry.to_config();

        assert_eq!(
            saved.platform_overrides.expect("platform overrides"),
            vec![block],
            "an unresolvable platform block must be re-emitted verbatim"
        );
    }

    #[test]
    fn an_unknown_priority_override_inside_a_known_platform_survives() {
        let block = PlatformOverrideEntry {
            platform: "PS2".to_string(),
            disabled_providers: None,
            priority_overrides: Some(vec![ProviderPriorityOverride {
                id: UNKNOWN_ID.to_string(),
                priority: 3,
            }]),
        };
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![block.clone()]),
        };

        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        registry.get_mut(KNOWN_ID).expect("known source").enabled = false;

        assert_eq!(
            registry.to_config().platform_overrides.expect("overrides"),
            vec![block]
        );
    }

    #[test]
    fn every_unresolvable_entry_is_reported_not_hidden() {
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            platform_overrides: Some(vec![
                PlatformOverrideEntry {
                    platform: "NotAPlatform".to_string(),
                    disabled_providers: None,
                    priority_overrides: None,
                },
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: None,
                    priority_overrides: Some(vec![ProviderPriorityOverride {
                        id: "another-unknown".to_string(),
                        priority: 5,
                    }]),
                },
            ]),
        };

        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        let unresolved = registry.unresolved_preferences();

        assert_eq!(unresolved.len(), 3, "got {unresolved:?}");
        assert!(
            unresolved
                .iter()
                .any(|u| u.kind == UnresolvedPreferenceKind::UnknownProvider
                    && u.detail == UNKNOWN_ID)
        );
        assert!(
            unresolved
                .iter()
                .any(|u| u.kind == UnresolvedPreferenceKind::UnresolvedPlatform
                    && u.detail == "NotAPlatform")
        );
        assert!(unresolved.iter().any(|u| matches!(
            &u.kind,
            UnresolvedPreferenceKind::UnknownPriorityOverride { platform } if platform == "PS2"
        ) && u.detail == "another-unknown"));

        for entry in &unresolved {
            assert!(
                entry.describe().contains("Kept as written"),
                "the wording must tell the user nothing was lost: {}",
                entry.describe()
            );
        }
    }

    #[test]
    fn an_unknown_disabled_entry_is_not_described_as_a_priority_override() {
        // Both kinds were reported as UnknownPriorityOverride, so the note sent
        // the user looking for a priority_overrides line that does not exist in
        // their file. The two live on different lines and must read differently.
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "PS2".to_string(),
                disabled_providers: Some(vec!["ghost-source".to_string()]),
                priority_overrides: None,
            }]),
        });

        let unresolved = registry.unresolved_preferences();
        assert_eq!(unresolved.len(), 1, "got {unresolved:?}");
        assert!(
            matches!(
                &unresolved[0].kind,
                UnresolvedPreferenceKind::UnknownDisabledProvider { platform } if platform == "PS2"
            ),
            "got {:?}",
            unresolved[0].kind
        );

        let described = unresolved[0].describe();
        assert!(
            !described.contains("Priority override"),
            "a disabled entry must not be called a priority override: {described}"
        );
        assert!(described.contains("ghost-source") && described.contains("Kept as written"));
    }

    #[test]
    fn an_unresolvable_platform_block_reports_one_cause_not_a_cascade() {
        // The block never matches, so its inner IDs are moot. Reporting each
        // of them as separately broken would bury the single real cause.
        let cfg = CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "NotAPlatform".to_string(),
                disabled_providers: Some(vec!["unknown-a".to_string()]),
                priority_overrides: Some(vec![ProviderPriorityOverride {
                    id: "unknown-b".to_string(),
                    priority: 5,
                }]),
            }]),
        };
        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        let unresolved = registry.unresolved_preferences();
        assert_eq!(unresolved.len(), 1, "got {unresolved:?}");
        assert_eq!(
            unresolved[0].kind,
            UnresolvedPreferenceKind::UnresolvedPlatform
        );
    }

    #[test]
    fn a_clean_registry_reports_nothing_unresolved() {
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig::default());
        assert!(registry.unresolved_preferences().is_empty());
        assert!(registry.unknown_providers().is_empty());
    }

    #[test]
    fn reapplying_a_config_does_not_accumulate_unknown_entries() {
        let cfg = CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: None,
            }]),
            platform_overrides: None,
        };
        let mut registry = build_default_registry();
        registry.apply_config(&cfg);
        registry.apply_config(&cfg);
        registry.apply_config(&cfg);
        assert_eq!(registry.unknown_providers().len(), 1);
    }

    #[test]
    fn an_untouched_registry_still_saves_an_empty_config() {
        // The compatibility floor: never having opened the GUI must not
        // start writing preferences that were previously absent.
        let registry = build_default_registry();
        let cfg = registry.to_config();
        assert_eq!(cfg, CheatSourcesConfig::default());
        assert!(cfg.providers.is_none() && cfg.platform_overrides.is_none());
    }

    // ---- Per-platform participation ------------------------------------

    #[test]
    fn participation_defaults_to_on_and_records_nothing() {
        let mut registry = build_default_registry();
        assert!(
            registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating
        );
        registry.set_platform_participation(KNOWN_ID, "PS2", true);
        assert!(
            registry.to_config().platform_overrides.is_none(),
            "the default must not be written out"
        );
    }

    #[test]
    fn participation_can_be_turned_off_and_back_on_without_residue() {
        let mut registry = build_default_registry();
        registry.set_platform_participation(KNOWN_ID, "PS2", false);
        assert!(
            !registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating
        );
        assert!(registry.to_config().platform_overrides.is_some());

        registry.set_platform_participation(KNOWN_ID, "PS2", true);
        assert!(
            registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating
        );
        assert!(
            registry.to_config().platform_overrides.is_none(),
            "toggling back must leave no inert stub in the user's file"
        );
    }

    #[test]
    fn participation_edits_do_not_disturb_other_blocks() {
        let foreign = PlatformOverrideEntry {
            platform: "NotAPlatform".to_string(),
            disabled_providers: Some(vec!["unknown".to_string()]),
            priority_overrides: None,
        };
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![foreign.clone()]),
        });

        registry.set_platform_participation(KNOWN_ID, "PS2", false);
        let saved = registry.to_config().platform_overrides.expect("overrides");

        assert!(
            saved.contains(&foreign),
            "an unrelated, unresolvable block must survive an edit elsewhere"
        );
        assert!(saved.iter().any(|b| b.platform == "PS2"));
    }

    #[test]
    fn source_level_disable_is_reported_as_overriding_the_platform_control() {
        let mut registry = build_default_registry();
        registry.get_mut(KNOWN_ID).expect("known source").enabled = false;
        let participation = registry.platform_participation(KNOWN_ID, "PS2");
        assert!(
            participation.overridden_by_source_level,
            "the GUI needs to know the platform toggle cannot help here"
        );
    }

    #[test]
    fn platform_participation_survives_an_alias() {
        // Stored values and typed values may spell a platform differently;
        // both must reach the same block rather than creating a second one.
        let mut registry = build_default_registry();
        registry.set_platform_participation(KNOWN_ID, "PS2", false);
        let blocks = registry.to_config().platform_overrides.expect("overrides");
        assert_eq!(blocks.len(), 1, "an alias must not create a second block");
        assert!(
            !registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating
        );
    }

    #[test]
    fn empty_platform_list_means_every_platform() {
        let registry = build_default_registry();
        let cross_platform = registry.get(KNOWN_ID).expect("bsfree is cross-platform");
        assert!(cross_platform.spec.platforms.is_empty());
        assert!(cross_platform.spec.covers_platform("PS2"));
        assert!(cross_platform.spec.covers_platform("Wii"));
        assert!(
            cross_platform.spec.platform_coverage().is_none(),
            "no list must not be displayed as covering nothing"
        );
    }

    #[test]
    fn re_enabling_clears_every_duplicate_block_for_that_platform() {
        // A file may name one platform in several blocks, and resolution reads
        // the last match. Removing the source from only the first left a later
        // block still disabling it, so the toggle silently did nothing: the
        // control moved, the resolved state did not, and the GUI redrew it as
        // still disabled.
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
            ]),
        });
        assert!(
            !registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating
        );

        registry.set_platform_participation(KNOWN_ID, "PS2", true);

        assert!(
            registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating,
            "re-enabling must actually take effect, not be masked by a later block"
        );
        assert!(
            registry
                .to_config()
                .platform_overrides
                .unwrap_or_default()
                .iter()
                .all(|block| block
                    .disabled_providers
                    .iter()
                    .flatten()
                    .all(|id| id != KNOWN_ID)),
            "no block may still name the source after re-enabling"
        );
    }

    #[test]
    fn disabling_records_into_the_block_resolution_reads() {
        // The mirror of the above: the write must land in the block that will
        // be read back, or the new setting is invisible to resolution.
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
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
                    priority_overrides: Some(vec![ProviderPriorityOverride {
                        id: "gamehacking.org-ps2".to_string(),
                        priority: 5,
                    }]),
                },
            ]),
        });

        registry.set_platform_participation(KNOWN_ID, "PS2", false);

        assert!(
            !registry
                .platform_participation(KNOWN_ID, "PS2")
                .participating,
            "the new exception must be visible to resolution immediately"
        );
        assert!(
            !registry
                .sorted_enabled_for_platform("PS2")
                .iter()
                .any(|e| e.spec.id == KNOWN_ID),
            "and must actually remove the source from that platform's results"
        );
    }

    #[test]
    fn re_enabling_preserves_unrelated_and_unresolvable_blocks() {
        // Cleanup must never reach past the platform being edited.
        let unresolvable = PlatformOverrideEntry {
            platform: "NotAPlatform".to_string(),
            disabled_providers: Some(vec!["someone-else".to_string()]),
            priority_overrides: None,
        };
        let other_platform = PlatformOverrideEntry {
            platform: "Wii".to_string(),
            disabled_providers: Some(vec![KNOWN_ID.to_string()]),
            priority_overrides: None,
        };
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
            providers: Some(vec![ProviderConfigEntry {
                id: UNKNOWN_ID.to_string(),
                enabled: Some(false),
                priority: Some(42),
            }]),
            platform_overrides: Some(vec![
                unresolvable.clone(),
                other_platform.clone(),
                PlatformOverrideEntry {
                    platform: "PS2".to_string(),
                    disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                    priority_overrides: None,
                },
            ]),
        });

        registry.set_platform_participation(KNOWN_ID, "PS2", true);
        let saved = registry.to_config();
        let blocks = saved.platform_overrides.expect("overrides");

        assert!(
            blocks.contains(&unresolvable),
            "an unresolvable block must survive an unrelated removal: {blocks:?}"
        );
        assert!(
            blocks.contains(&other_platform),
            "another platform's exception must survive: {blocks:?}"
        );
        assert!(
            !blocks.iter().any(|b| b.platform == "PS2"),
            "the emptied PS2 block should be gone: {blocks:?}"
        );
        assert_eq!(
            saved
                .providers
                .expect("providers")
                .iter()
                .filter(|p| p.id == UNKNOWN_ID)
                .count(),
            1,
            "the unknown provider must survive a platform edit"
        );
    }

    #[test]
    fn a_block_kept_only_for_its_priority_overrides_is_not_deleted() {
        // Cleanup removes blocks with nothing left in them. A block whose
        // priority_overrides are still present has content, even if the
        // disabled list just emptied.
        let mut registry = build_default_registry();
        registry.apply_config(&CheatSourcesConfig {
            providers: None,
            platform_overrides: Some(vec![PlatformOverrideEntry {
                platform: "PS2".to_string(),
                disabled_providers: Some(vec![KNOWN_ID.to_string()]),
                priority_overrides: Some(vec![ProviderPriorityOverride {
                    id: "gamehacking.org-ps2".to_string(),
                    priority: 5,
                }]),
            }]),
        });

        registry.set_platform_participation(KNOWN_ID, "PS2", true);
        let blocks = registry
            .to_config()
            .platform_overrides
            .expect("the block must remain for its priority overrides");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].disabled_providers.is_none());
        assert!(blocks[0].priority_overrides.is_some());
    }

    #[test]
    fn a_platform_specific_source_reports_its_coverage() {
        let registry = build_default_registry();
        let ps2 = registry.get("gamehacking.org-ps2").expect("entry");
        assert!(ps2.spec.covers_platform("PS2"));
        assert!(!ps2.spec.covers_platform("Wii"));
        assert_eq!(ps2.spec.platform_coverage(), Some(&["PS2".to_string()][..]));
    }
}
