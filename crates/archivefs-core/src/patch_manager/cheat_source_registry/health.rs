//! Read-only health snapshot for a single cheat source, plus a best-effort
//! probe that derives that snapshot from each source's persisted cache state.
//!
//! The snapshot itself is a lightweight status record: it performs no network
//! or filesystem access on its own. The [`probe_cheat_source_health`] family
//! in this module is where persisted state is turned into a snapshot, and it is
//! strictly read-only - it never creates, locks, or modifies a file, and never
//! touches the network.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::CheatProviderSourceState;
use super::super::bsfree::{BSFREE_PROVIDER_ID, BsFreePaths, inspect_bsfree_source};
use super::super::dolphin_cheat_catalogue::DOLPHIN_CATALOGUE_PROVIDER_ID;
use super::super::dolphin_gecko_provider::DOLPHIN_UPSTREAM_PROVIDER_ID;
use super::super::gamehacking_provider::GAMEHACKING_PROVIDER_ID;
use super::super::xenia_provider::XENIA_PROVIDER_ID;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CheatSourceHealth {
    pub state: CheatProviderSourceState,
    pub last_checked_unix_seconds: Option<u64>,
    pub last_error: Option<String>,
    pub entry_count: Option<u64>,
    pub freshness_seconds: Option<u64>,
}

impl CheatSourceHealth {
    pub const fn unknown() -> Self {
        Self {
            state: CheatProviderSourceState::NotInstalled,
            last_checked_unix_seconds: None,
            last_error: None,
            entry_count: None,
            freshness_seconds: None,
        }
    }

    pub fn ready(entry_count: u64) -> Self {
        Self {
            state: CheatProviderSourceState::Ready,
            last_checked_unix_seconds: Some(now_unix()),
            last_error: None,
            entry_count: Some(entry_count),
            freshness_seconds: None,
        }
    }

    pub fn error(state: CheatProviderSourceState, message: String) -> Self {
        Self {
            state,
            last_checked_unix_seconds: Some(now_unix()),
            last_error: Some(message),
            ..Self::unknown()
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Best-effort read-only health probing
// ---------------------------------------------------------------------------

/// The directory under which every cheat source keeps its persisted cache
/// state: the parent of the ArchiveFS database.
pub fn default_cheat_source_data_root() -> Option<PathBuf> {
    crate::default_database_path()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// Probes the persisted cache state of one registered source, read-only.
///
/// Returns `Some` whenever the source has an on-disk state worth reporting -
/// installed, explicitly not installed, or an error - and `None` only for
/// sources that keep no persistent cache (their state is resolved at fetch
/// time) or for an id this build does not know. Best-effort: a malformed or
/// unreadable cache is reported as an error health, never as a panic or a
/// failed probe.
///
/// This is the single probe entry point used by the CLI, the Cheat Sources
/// page and Doctor, so a source's health is always described the same way.
pub fn probe_cheat_source_health(source_id: &str, data_root: &Path) -> Option<CheatSourceHealth> {
    let retroarch = super::super::cheat_sources::trusted_retroarch_cheat_sources();
    let retroarch_id = retroarch
        .first()
        .map(|definition| definition.source_id.as_str())
        .unwrap_or("libretro-buildbot-cheats");
    match source_id {
        id if id == retroarch_id => probe_retroarch_metadata(id, data_root),
        id if id == BSFREE_PROVIDER_ID => probe_bsfree(data_root),
        id if id == super::GAMEHACKING_PS2_REGISTRY_ID => {
            probe_gamehacking(data_root, PS2_CATALOGUE_FILE)
        }
        id if id == super::GAMEHACKING_GAMECUBE_REGISTRY_ID => {
            probe_gamehacking(data_root, GAMECUBE_CATALOGUE_FILE)
        }
        id if id == super::GAMEHACKING_WII_REGISTRY_ID => {
            probe_gamehacking(data_root, WII_CATALOGUE_FILE)
        }
        id if id == DOLPHIN_CATALOGUE_PROVIDER_ID => probe_dolphin_catalogue(data_root),
        id if id == DOLPHIN_UPSTREAM_PROVIDER_ID => probe_dolphin_gecko_cache(data_root),
        id if id == XENIA_PROVIDER_ID => probe_xenia_index(data_root),
        // pcsx2-official-patches-tree resolves its metadata at fetch time and
        // persists nothing, so there is no on-disk state to probe.
        _ => None,
    }
}

/// Catalogue filenames under `cache/gamehacking/`, matching each provider's
/// own constant so the probe never drifts from what the provider writes.
const PS2_CATALOGUE_FILE: &str = "ps2-catalogue.json";
const GAMECUBE_CATALOGUE_FILE: &str = "gamecube-catalogue.json";
const WII_CATALOGUE_FILE: &str = "wii-catalogue.json";

/// A concise, actionable label for a cache file that exists but cannot be
/// read. Deliberately never includes the path: the reason travels with a
/// per-source description, and dumping private cache paths into a status line
/// would leak layout for no benefit.
fn cache_read_error_reason(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::NotFound => "no longer present",
        std::io::ErrorKind::IsADirectory => "is a directory, not a cache file",
        std::io::ErrorKind::NotADirectory => "is not a directory",
        _ => "unreadable",
    }
}

/// Minimal projection of the libretro cache metadata, enough for a health
/// snapshot without pulling in the provider's full manifest (which carries a
/// large per-file list this probe does not need).
#[derive(Deserialize)]
struct RetroArchMetadataProbe {
    source_id: String,
    current_snapshot: Option<String>,
    manifest: Option<RetroArchManifestProbe>,
    #[serde(default)]
    last_fetch_succeeded: bool,
    #[serde(default)]
    last_error: Option<RetroArchErrorProbe>,
    #[serde(default)]
    last_error_at_unix_seconds: Option<u64>,
}

#[derive(Deserialize)]
struct RetroArchManifestProbe {
    fetched_at_unix_seconds: u64,
    #[serde(default)]
    valid_cheat_count: usize,
}

#[derive(Deserialize)]
struct RetroArchErrorProbe {
    #[serde(default)]
    message: String,
}

/// The libretro source persists one `metadata.json` per source under
/// `cheat-sources/`, written by its own fetch pipeline. `None` = metadata
/// absent (the source was never fetched); the state comes from that record.
fn probe_retroarch_metadata(source_id: &str, data_root: &Path) -> Option<CheatSourceHealth> {
    let metadata_path = data_root
        .join("cheat-sources")
        .join(source_id)
        .join("metadata.json");
    // A missing file means "never fetched". A file that exists but cannot be
    // read is a real error and must not collapse into "not checked": matching
    // directly on `read` also closes the race where the file disappears
    // between an `exists` check and the read.
    let bytes = match std::fs::read(&metadata_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(CheatSourceHealth::unknown());
        }
        Err(error) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some(format!(
                    "libretro cache exists but could not be read: {}",
                    cache_read_error_reason(&error)
                )),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let metadata: RetroArchMetadataProbe = match serde_json::from_slice(&bytes) {
        Ok(parsed) => parsed,
        Err(_) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some("libretro cache metadata is unreadable".to_string()),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    if metadata.source_id != source_id {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::Invalid,
            last_checked_unix_seconds: metadata.last_error_at_unix_seconds,
            last_error: Some("libretro cache metadata is bound to another source".to_string()),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    if !metadata.last_fetch_succeeded {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::DownloadFailed,
            last_checked_unix_seconds: metadata.last_error_at_unix_seconds,
            last_error: metadata
                .last_error
                .map(|error| error.message)
                .or_else(|| Some("the last fetch did not complete".to_string())),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    let Some(snapshot) = metadata.current_snapshot else {
        return Some(CheatSourceHealth::unknown());
    };
    // The snapshot directory must still exist; the manifest record alone is
    // not proof the data is on disk. This is a cheap directory check - it does
    // not re-hash the catalogue.
    let snapshot_dir = data_root
        .join("cheat-sources")
        .join(source_id)
        .join("snapshots")
        .join(snapshot);
    if !snapshot_dir.is_dir() {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::Invalid,
            last_checked_unix_seconds: metadata.last_error_at_unix_seconds,
            last_error: Some("libretro snapshot directory is missing".to_string()),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    let manifest = metadata.manifest.as_ref()?;
    Some(CheatSourceHealth {
        state: CheatProviderSourceState::Ready,
        last_checked_unix_seconds: Some(manifest.fetched_at_unix_seconds),
        last_error: None,
        entry_count: Some(manifest.valid_cheat_count as u64),
        freshness_seconds: Some(now_unix().saturating_sub(manifest.fetched_at_unix_seconds)),
    })
}

/// The BSFree source keeps its own `source.json` (state, last error) plus a
/// `last-validation.json` (counts, validation time). `inspect_bsfree_source`
/// is read-only and is exactly the state the source reports about itself.
fn probe_bsfree(data_root: &Path) -> Option<CheatSourceHealth> {
    let root = data_root.join("cheat-sources").join("bsfree");
    let paths = BsFreePaths::at(root);
    let status = match inspect_bsfree_source(&paths) {
        Ok(status) => status,
        Err(_) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some("BSFree source state is unreadable".to_string()),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let entry_count = status
        .validation
        .as_ref()
        .map(|validation| validation.counts.codes);
    let last_checked = status
        .validation
        .as_ref()
        .map(|validation| validation.result.validated_at_unix_seconds);
    let freshness_seconds = last_checked.map(|checked| now_unix().saturating_sub(checked));
    let last_error = status.last_error.as_ref().map(ToString::to_string);
    let state = if status.usable {
        CheatProviderSourceState::Ready
    } else {
        status.state
    };
    Some(CheatSourceHealth {
        state,
        last_checked_unix_seconds: last_checked,
        last_error,
        entry_count,
        freshness_seconds,
    })
}

/// Minimal projection of one GameHacking platform catalogue, mirroring the
/// metadata the provider's own loader validates, without deserialising the
/// full per-game record shape this probe does not need.
#[derive(Deserialize)]
struct GameHackingCatalogueProbe {
    schema_version: u32,
    provider: String,
    system: String,
    retrieved_at_unix_seconds: u64,
    games: Vec<serde_json::Value>,
}

/// One GameHacking platform catalogue under `cache/gamehacking/`. Presence is
/// checked against the provider's own validation (schema, provider, system)
/// so "present but wrong shape" is an error, not a false ready.
fn probe_gamehacking(data_root: &Path, file: &str) -> Option<CheatSourceHealth> {
    let path = data_root.join("cache").join("gamehacking").join(file);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(CheatSourceHealth::unknown());
        }
        Err(error) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some(format!(
                    "GameHacking cache exists but could not be read: {}",
                    cache_read_error_reason(&error)
                )),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let catalogue: GameHackingCatalogueProbe = match serde_json::from_slice(&bytes) {
        Ok(catalogue) => catalogue,
        Err(_) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some("GameHacking catalogue is unreadable".to_string()),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let expected_system = match file {
        PS2_CATALOGUE_FILE => "PlayStation 2",
        GAMECUBE_CATALOGUE_FILE => "GameCube",
        WII_CATALOGUE_FILE => "Wii",
        _ => return None,
    };
    if catalogue.schema_version != 1
        || catalogue.provider != GAMEHACKING_PROVIDER_ID
        || !catalogue.system.eq_ignore_ascii_case(expected_system)
    {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::Invalid,
            last_checked_unix_seconds: None,
            last_error: Some("GameHacking catalogue metadata is unsupported".to_string()),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    let retrieved_at = catalogue.retrieved_at_unix_seconds;
    Some(CheatSourceHealth {
        state: CheatProviderSourceState::Ready,
        last_checked_unix_seconds: Some(retrieved_at),
        last_error: None,
        entry_count: Some(catalogue.games.len() as u64),
        freshness_seconds: Some(now_unix().saturating_sub(retrieved_at)),
    })
}

/// Minimal projection of the Dolphin catalogue, which the provider persists as
/// one `catalogue.json` alongside its metadata.
#[derive(Deserialize)]
struct DolphinCatalogueProbe {
    metadata: DolphinCatalogueMetadataProbe,
    games: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct DolphinCatalogueMetadataProbe {
    schema_version: u32,
    fetched_at_unix_seconds: u64,
}

/// The Dolphin catalogue is read directly (never through the provider's cache
/// lock), keeping this probe strictly read-only.
fn probe_dolphin_catalogue(data_root: &Path) -> Option<CheatSourceHealth> {
    let catalogue_path = data_root
        .join("dolphin-cheat-catalogue")
        .join("catalogue.json");
    let bytes = match std::fs::read(&catalogue_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(CheatSourceHealth::unknown());
        }
        Err(error) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some(format!(
                    "Dolphin catalogue exists but could not be read: {}",
                    cache_read_error_reason(&error)
                )),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let catalogue: DolphinCatalogueProbe = match serde_json::from_slice(&bytes) {
        Ok(catalogue) => catalogue,
        Err(_) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some("Dolphin catalogue is unreadable".to_string()),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    if catalogue.metadata.schema_version != 1 {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::Invalid,
            last_checked_unix_seconds: None,
            last_error: Some("Dolphin catalogue schema is unsupported".to_string()),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    let fetched_at = catalogue.metadata.fetched_at_unix_seconds;
    Some(CheatSourceHealth {
        state: CheatProviderSourceState::Ready,
        last_checked_unix_seconds: Some(fetched_at),
        last_error: None,
        entry_count: Some(catalogue.games.len() as u64),
        freshness_seconds: Some(now_unix().saturating_sub(fetched_at)),
    })
}

/// The per-game Gecko cache is a flat directory of `{gameid}-r{rev}.json`
/// files. Health = "N single-game results cached", dated by the newest file.
fn probe_dolphin_gecko_cache(data_root: &Path) -> Option<CheatSourceHealth> {
    let cache_root = data_root.join("gecko-provider-cache");
    let mut count = 0u64;
    let mut newest = None;
    let entries = match std::fs::read_dir(&cache_root) {
        Ok(entries) => entries,
        // A missing directory means "never fetched"; a directory that exists
        // but cannot be read is a real error, not "not checked".
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(CheatSourceHealth::unknown());
        }
        Err(error) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some(format!(
                    "Gecko cache exists but could not be read: {}",
                    cache_read_error_reason(&error)
                )),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        count += 1;
        let modified = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        newest = newest.max(modified);
    }
    if count == 0 {
        return Some(CheatSourceHealth::unknown());
    }
    Some(CheatSourceHealth {
        state: CheatProviderSourceState::Ready,
        last_checked_unix_seconds: newest,
        last_error: None,
        entry_count: Some(count),
        freshness_seconds: newest.map(|newest| now_unix().saturating_sub(newest)),
    })
}

/// Minimal projection of the Xenia index cache.
#[derive(Deserialize)]
struct XeniaIndexProbe {
    commit: String,
    #[serde(default)]
    paths: Vec<String>,
}

/// The Xenia source persists one `index.json` naming the patches for the
/// pinned commit it downloaded. `None` = never fetched.
fn probe_xenia_index(data_root: &Path) -> Option<CheatSourceHealth> {
    let index_path = data_root.join("xenia-provider-cache").join("index.json");
    let bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(CheatSourceHealth::unknown());
        }
        Err(error) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some(format!(
                    "Xenia index exists but could not be read: {}",
                    cache_read_error_reason(&error)
                )),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    let index: XeniaIndexProbe = match serde_json::from_slice(&bytes) {
        Ok(index) => index,
        Err(_) => {
            return Some(CheatSourceHealth {
                state: CheatProviderSourceState::Invalid,
                last_checked_unix_seconds: None,
                last_error: Some("Xenia index cache is unreadable".to_string()),
                entry_count: None,
                freshness_seconds: None,
            });
        }
    };
    if index.commit.is_empty() {
        return Some(CheatSourceHealth {
            state: CheatProviderSourceState::Invalid,
            last_checked_unix_seconds: None,
            last_error: Some("Xenia index cache carries no commit".to_string()),
            entry_count: None,
            freshness_seconds: None,
        });
    }
    let last_checked = std::fs::metadata(&index_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Some(CheatSourceHealth {
        state: CheatProviderSourceState::Ready,
        last_checked_unix_seconds: last_checked,
        last_error: None,
        entry_count: Some(index.paths.len() as u64),
        freshness_seconds: last_checked.map(|checked| now_unix().saturating_sub(checked)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_installed() {
        let h = CheatSourceHealth::unknown();
        assert_eq!(h.state, CheatProviderSourceState::NotInstalled);
        assert!(h.last_checked_unix_seconds.is_none());
        assert!(h.last_error.is_none());
    }

    #[test]
    fn ready_has_timestamp() {
        let h = CheatSourceHealth::ready(42);
        assert_eq!(h.state, CheatProviderSourceState::Ready);
        assert_eq!(h.entry_count, Some(42));
        assert!(h.last_checked_unix_seconds.is_some());
        assert!(h.last_error.is_none());
    }

    #[test]
    fn error_carries_message() {
        let h = CheatSourceHealth::error(
            CheatProviderSourceState::DownloadFailed,
            "timeout".to_string(),
        );
        assert_eq!(h.state, CheatProviderSourceState::DownloadFailed);
        assert_eq!(h.last_error.as_deref(), Some("timeout"));
    }

    // ------------------------------------------------------------------
    // Read-only health probe
    // ------------------------------------------------------------------

    fn data_root(dir: &Path) -> PathBuf {
        let root = dir.join("data");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_json(path: &Path, value: serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn an_empty_data_root_reports_every_cached_source_as_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        for entry in crate::patch_manager::build_default_registry().entries() {
            let health = probe_cheat_source_health(&entry.spec.id, &root);
            if entry.spec.id == crate::patch_manager::BUILT_IN_SOURCE_ID {
                assert!(
                    health.is_none(),
                    "{} keeps no persisted state, so health is unknown",
                    entry.spec.id
                );
            } else {
                let health = health.expect("every cached source reports a health");
                assert_eq!(
                    health.state,
                    CheatProviderSourceState::NotInstalled,
                    "{} should read as not installed on an empty data root",
                    entry.spec.id
                );
            }
        }
    }

    #[test]
    fn an_unknown_source_id_has_no_health() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        assert!(probe_cheat_source_health("no-such-source", &root).is_none());
    }

    #[test]
    fn retroarch_ready_reports_entry_count_and_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        let source_root = root.join("cheat-sources").join("libretro-buildbot-cheats");
        std::fs::create_dir_all(source_root.join("snapshots").join("abc123")).unwrap();
        write_json(
            &source_root.join("metadata.json"),
            serde_json::json!({
                "format_version": 1,
                "source_id": "libretro-buildbot-cheats",
                "current_snapshot": "abc123",
                "manifest": {
                    "fetched_at_unix_seconds": 1000,
                    "valid_cheat_count": 42,
                    "catalogue_file_count": 5
                },
                "last_fetch_succeeded": true,
                "last_error": null
            }),
        );
        let health = probe_cheat_source_health("libretro-buildbot-cheats", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Ready);
        assert_eq!(health.entry_count, Some(42));
        assert_eq!(health.last_checked_unix_seconds, Some(1000));
        assert!(health.last_error.is_none());
        assert!(health.freshness_seconds.is_some());
    }

    #[test]
    fn retroarch_failed_fetch_reports_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        let source_root = root.join("cheat-sources").join("libretro-buildbot-cheats");
        write_json(
            &source_root.join("metadata.json"),
            serde_json::json!({
                "format_version": 1,
                "source_id": "libretro-buildbot-cheats",
                "current_snapshot": null,
                "manifest": null,
                "last_fetch_succeeded": false,
                "last_error": {
                    "schema_version": 1,
                    "stage": "download",
                    "code": "network_timeout",
                    "message": "network unreachable"
                },
                "last_error_at_unix_seconds": 2000
            }),
        );
        let health = probe_cheat_source_health("libretro-buildbot-cheats", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::DownloadFailed);
        assert_eq!(health.last_checked_unix_seconds, Some(2000));
        assert!(health.last_error.is_some());
        assert!(
            health
                .last_error
                .as_deref()
                .unwrap()
                .contains("network unreachable"),
            "{:?}",
            health.last_error
        );
    }

    #[test]
    fn retroarch_missing_snapshot_directory_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        let source_root = root.join("cheat-sources").join("libretro-buildbot-cheats");
        // The manifest claims a snapshot that does not exist on disk.
        write_json(
            &source_root.join("metadata.json"),
            serde_json::json!({
                "format_version": 1,
                "source_id": "libretro-buildbot-cheats",
                "current_snapshot": "deadbeef",
                "manifest": {
                    "fetched_at_unix_seconds": 1000,
                    "valid_cheat_count": 42
                },
                "last_fetch_succeeded": true,
                "last_error": null
            }),
        );
        let health = probe_cheat_source_health("libretro-buildbot-cheats", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Invalid);
        assert!(health.last_error.is_some());
    }

    #[test]
    fn gamehacking_catalogue_reports_game_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        write_json(
            &root
                .join("cache")
                .join("gamehacking")
                .join("ps2-catalogue.json"),
            serde_json::json!({
                "schema_version": 1,
                "provider": "gamehacking.org",
                "system": "PlayStation 2",
                "source_url": "https://example.test",
                "retrieved_at_unix_seconds": 1700000000,
                "pages": [],
                "games": [
                    {"game_id": 1, "title": "Game"},
                    {"game_id": 2, "title": "Game 2"}
                ]
            }),
        );
        let health = probe_cheat_source_health("gamehacking.org-ps2", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Ready);
        assert_eq!(health.entry_count, Some(2));
        assert_eq!(health.last_checked_unix_seconds, Some(1700000000));
    }

    #[test]
    fn gamehacking_wrong_system_is_invalid_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        write_json(
            &root
                .join("cache")
                .join("gamehacking")
                .join("wii-catalogue.json"),
            serde_json::json!({
                "schema_version": 1,
                "provider": "gamehacking.org",
                "system": "GameCube",
                "source_url": "https://example.test",
                "retrieved_at_unix_seconds": 1700000000,
                "pages": [],
                "games": []
            }),
        );
        let health = probe_cheat_source_health("gamehacking.org-wii", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Invalid);
        assert!(health.last_error.is_some());
    }

    #[test]
    fn dolphin_catalogue_reports_game_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        write_json(
            &root.join("dolphin-cheat-catalogue").join("catalogue.json"),
            serde_json::json!({
                "metadata": {
                    "schema_version": 1,
                    "fetched_at_unix_seconds": 1700000000
                },
                "games": [
                    {"game_id": "GAME1"},
                    {"game_id": "GAME2"},
                    {"game_id": "GAME3"}
                ]
            }),
        );
        let health = probe_cheat_source_health("dolphin_upstream_catalogue", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Ready);
        assert_eq!(health.entry_count, Some(3));
        assert_eq!(health.last_checked_unix_seconds, Some(1700000000));
    }

    #[test]
    fn dolphin_gecko_cache_reports_cached_game_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        let cache_root = root.join("gecko-provider-cache");
        std::fs::create_dir_all(&cache_root).unwrap();
        std::fs::write(cache_root.join("GAME1-r0.json"), b"{}").unwrap();
        std::fs::write(cache_root.join("GAME2-r0.json"), b"{}").unwrap();
        std::fs::write(cache_root.join("unrelated.txt"), b"ignored").unwrap();
        let health = probe_cheat_source_health("dolphin_upstream_gamesettings", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Ready);
        assert_eq!(health.entry_count, Some(2));
        assert!(health.last_checked_unix_seconds.is_some());
    }

    #[test]
    fn xenia_index_reports_patch_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        write_json(
            &root.join("xenia-provider-cache").join("index.json"),
            serde_json::json!({
                "commit": "abc123",
                "paths": ["patches/a.toml", "patches/b.toml"]
            }),
        );
        let health = probe_cheat_source_health("xenia_canary_game_patches", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Ready);
        assert_eq!(health.entry_count, Some(2));
        assert!(health.last_checked_unix_seconds.is_some());
    }

    #[test]
    fn bsfree_reports_its_own_disabled_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        write_json(
            &root
                .join("cheat-sources")
                .join("bsfree")
                .join("source.json"),
            serde_json::json!({
                "format_version": 1,
                "provider_id": "bsfree-archive",
                "enabled": false,
                "state": "not_installed",
                "last_operation_at_unix_seconds": 1000,
                "validation": null,
                "last_error": null
            }),
        );
        let health = probe_cheat_source_health("bsfree-archive", &root).unwrap();
        assert_eq!(health.state, CheatProviderSourceState::Disabled);
    }

    #[test]
    fn registry_probe_health_populates_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        let mut registry = crate::patch_manager::build_default_registry();
        assert!(
            registry
                .entries()
                .iter()
                .all(|entry| entry.health.is_none()),
            "health starts unprobed"
        );
        registry.probe_health(&root);
        for entry in registry.entries() {
            if entry.spec.id == crate::patch_manager::BUILT_IN_SOURCE_ID {
                assert!(entry.health.is_none());
            } else {
                assert!(
                    entry.health.is_some(),
                    "{} should carry a probed health",
                    entry.spec.id
                );
            }
        }
    }

    /// Makes `std::fs::read(path)` fail deterministically, including when the
    /// test runs as root, by putting a directory where the probe expects a
    /// regular file (EISDIR). The parent is created first.
    fn make_unreadable_file(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::create_dir(path).unwrap();
    }

    #[test]
    fn an_existing_but_unreadable_cache_is_invalid_never_not_checked() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());

        make_unreadable_file(
            &root
                .join("cheat-sources")
                .join("libretro-buildbot-cheats")
                .join("metadata.json"),
        );
        make_unreadable_file(
            &root
                .join("cache")
                .join("gamehacking")
                .join("ps2-catalogue.json"),
        );
        make_unreadable_file(&root.join("dolphin-cheat-catalogue").join("catalogue.json"));
        make_unreadable_file(&root.join("xenia-provider-cache").join("index.json"));
        // The Gecko cache is read as a directory; a regular file where the
        // directory belongs makes `read_dir` fail (ENOTDIR).
        std::fs::write(root.join("gecko-provider-cache"), b"not a directory").unwrap();

        for source_id in [
            "libretro-buildbot-cheats",
            "gamehacking.org-ps2",
            "dolphin_upstream_catalogue",
            "dolphin_upstream_gamesettings",
            "xenia_canary_game_patches",
        ] {
            let health = probe_cheat_source_health(source_id, &root)
                .unwrap_or_else(|| panic!("{source_id} must report a health, never None"));
            assert_eq!(
                health.state,
                CheatProviderSourceState::Invalid,
                "{source_id} has an existing-but-unreadable cache and must be Invalid"
            );
            let reason = health.last_error.as_deref().unwrap_or_default();
            assert!(!reason.is_empty(), "{source_id} must carry a reason");
            assert!(
                !reason.contains("data/") && !reason.contains("archivefs"),
                "{source_id} reason must not leak private cache paths: {reason}"
            );
        }
    }

    #[test]
    fn a_malformed_existing_cache_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        std::fs::create_dir_all(root.join("cheat-sources").join("libretro-buildbot-cheats"))
            .unwrap();
        std::fs::write(
            root.join("cheat-sources")
                .join("libretro-buildbot-cheats")
                .join("metadata.json"),
            b"not json",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("cache").join("gamehacking")).unwrap();
        std::fs::write(
            root.join("cache")
                .join("gamehacking")
                .join("ps2-catalogue.json"),
            b"not json",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("dolphin-cheat-catalogue")).unwrap();
        std::fs::write(
            root.join("dolphin-cheat-catalogue").join("catalogue.json"),
            b"not json",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("xenia-provider-cache")).unwrap();
        std::fs::write(
            root.join("xenia-provider-cache").join("index.json"),
            b"not json",
        )
        .unwrap();

        for source_id in [
            "libretro-buildbot-cheats",
            "gamehacking.org-ps2",
            "dolphin_upstream_catalogue",
            "xenia_canary_game_patches",
        ] {
            let health = probe_cheat_source_health(source_id, &root)
                .unwrap_or_else(|| panic!("{source_id} must report a health, never None"));
            assert_eq!(
                health.state,
                CheatProviderSourceState::Invalid,
                "{source_id} has a malformed cache and must be Invalid"
            );
            assert!(health.last_error.is_some(), "{source_id} must explain");
        }
    }

    #[test]
    fn registry_probe_health_retains_an_invalid_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = data_root(dir.path());
        make_unreadable_file(
            &root
                .join("cache")
                .join("gamehacking")
                .join("ps2-catalogue.json"),
        );

        let mut registry = crate::patch_manager::build_default_registry();
        registry.probe_health(&root);
        let ps2 = registry.get("gamehacking.org-ps2").expect("entry");
        let health = ps2.health.as_ref().expect("health is probed, not dropped");
        assert_eq!(health.state, CheatProviderSourceState::Invalid);
        assert!(health.last_error.is_some());
    }
}
