//! Read-only discovery and matching for external RetroArch cheat catalogue
//! sources - local only, no network, no install/enable/apply operation.
//!
//! This is a third, independent read-only preview alongside `pcsx2` and
//! `retroarch`: it does not implement [`super::adapter::EmulatorAdapter`]
//! and does not produce an [`super::AdvisoryPatchPlan`], for the same
//! reason `retroarch.rs` does not (see that module's doc comment) - PCSX2
//! patches and RetroArch cheats are shaped too differently for one shared
//! trait to be worth forcing yet. Nothing in `mod.rs`, `adapter.rs`,
//! `matching.rs`, `pcsx2.rs`, `retrieval.rs`, or `retroarch.rs`/
//! `retroarch_inventory.rs` is changed to add this module.
//!
//! ## What this module reuses instead of rebuilding
//!
//! - **Identity evidence for "is this catalogue game already in my
//!   library?"** comes from [`super::CatalogueGameEvidence`] - the same
//!   type PCSX2 matching already consumes - passed in by the caller
//!   (already loaded via the existing, unmodified
//!   [`super::load_catalogue_evidence_read_only`]). This module never
//!   opens a database itself.
//! - **Playlist-identity evidence** (tier 3 below) comes from an already
//!   -built [`super::RetroArchAdvisoryPlan`] (`entries[].profile_outcomes[]
//!   .playlist_evidence`), produced by the existing, unmodified
//!   [`super::preview_retroarch_patch_and_cheat_destinations`]. Passing
//!   `None` for that plan still allows title/platform/region/filename
//!   matching (tiers 4-6); only tier 3 and installed-state reporting need
//!   it.
//! - **Installed-state** cross-references the same plan's
//!   `artifact_inventory` (already-built [`super::RetroArchArtifactInventory`]
//!   /[`super::RetroArchArtifactDestination`]) rather than re-scanning
//!   RetroArch's cheat directories a second time.
//! - **Bounded, symlink-safe filesystem access** reuses
//!   [`crate::emulator_environment::ReadOnlyHostFilesystem`] - the exact
//!   same no-write, final-component-no-follow trait `retroarch_inventory`
//!   uses - rather than a second filesystem abstraction.
//!
//! ## What is new here
//!
//! - Reading an arbitrary **local** catalogue root (a `.cht` directory tree
//!   or a bounded JSON manifest) that is *not* one of RetroArch's own
//!   configured cheat directories.
//! - A `.cht` text parser that keeps per-cheat descriptions and
//!   enabled-by-default flags ([`CheatDefinition`]), rather than
//!   `retroarch_inventory::CheatFileSummary`'s aggregate-only counts. Cheat
//!   *code* bodies (the numeric/hex value lines) are still never parsed or
//!   stored, matching that module's existing precedent.
//! - Game-identity matching against a catalogue source name/platform/
//!   region/serial/content-hash instead of a PNACH filename or playlist
//!   content path.
//! - Byte-hash comparison (SHA-256, computed only over content already
//!   bounded-read for parsing) for the "is this exact cheat file already
//!   installed under this or another filename?" question - the existing
//!   artifact inventory deliberately never hashes (see
//!   `docs/RETROARCH_ARTIFACT_INVENTORY.md`'s Non-goals), so this module
//!   performs its own additional bounded read of the installed candidate
//!   when one exists, still never writing, executing, or following a
//!   symlink.
//!
//! ## Non-goals (this milestone)
//!
//! No network access, no download, no install/copy/rename/delete of a
//! cheat, no enabling/disabling a cheat, no RetroArch launch, no emulator
//! configuration change, no live-database write, no migration, no scan.
//! See `docs/RETROARCH_CHEAT_CATALOGUE.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::emulator_environment::{
    BoundedListResult, BoundedReadResult, EncodedPath, FsProbe, ReadOnlyHostFilesystem,
    os_str_bytes,
};

use super::retroarch::{PlaylistMatchConfidence, RetroArchAdvisoryPlan};
use super::{
    ArtifactConflictState, ArtifactDiagnosticSeverity, ArtifactKind, CatalogueGameEvidence,
    RetroArchArtifactDestination,
};
use crate::canonical_platform_for_alias;

pub const CHEAT_CATALOGUE_FORMAT_VERSION: u32 = 1;
/// Mirrors `retroarch_inventory::MAX_CHEAT_FILE_BYTES` - one catalogue
/// `.cht` file or the JSON manifest body, bounded-read.
pub const MAX_CATALOGUE_FILE_BYTES: usize = 2 * 1024 * 1024;
/// A JSON manifest describes many games in one file; it gets its own,
/// larger bound distinct from a single `.cht` file's bound.
pub const MAX_CATALOGUE_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CATALOGUE_FILES: usize = 50_000;
pub const MAX_CATALOGUE_DIRECTORIES: usize = 50_000;
pub const MAX_ARTIFACTS_PER_DIRECTORY: usize = 8_192;
/// Mirrors `retroarch_inventory::MAX_CHEAT_ENTRIES_PER_FILE`.
pub const MAX_CHEATS_PER_GAME: usize = 16_384;
pub const MAX_GAME_RECORDS: usize = 100_000;
pub const MAX_CATALOGUE_DIAGNOSTICS: usize = 2_048;
pub const MAX_CATALOGUE_STRING_BYTES: usize = 4 * 1024;

/// Which local format a [`CheatGameRecord`] was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatCatalogueFormat {
    /// A directory tree of RetroArch/libretro `.cht` files, matched the
    /// same way `retroarch_inventory` recognizes them (case-insensitive
    /// `.cht` extension).
    RetroarchChtDirectory,
    /// A single bounded JSON document listing games and cheats -
    /// deterministic fixtures, and the only format able to declare a
    /// serial/content-hash/region/revision today, since real `.cht` files
    /// carry no such fields.
    JsonManifest,
}

/// One cheat's bounded metadata - never the code body itself.
#[derive(Debug, Clone, Serialize)]
pub struct CheatDefinition {
    pub description: Option<String>,
    pub enabled_by_default: bool,
    /// The declared `cheatN_*` index for a `.cht` source, or the manifest
    /// array position for a JSON source.
    pub declared_index: Option<u32>,
}

/// One catalogue-supplied structured diagnostic - never free-text-only,
/// matching `retroarch_inventory::ArtifactDiagnostic`'s convention.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogueDiagnostic {
    pub code: &'static str,
    pub severity: ArtifactDiagnosticSeverity,
    pub path: Option<EncodedPath>,
}

/// One game's cheat availability as declared by one local catalogue file.
/// Two files that both describe "the same" game (by any matching tier)
/// each get their own `CheatGameRecord` - this module never merges them,
/// so "multiple cheat files for one game" stays visible instead of being
/// silently collapsed.
#[derive(Debug, Clone, Serialize)]
pub struct CheatGameRecord {
    pub source_game_name: String,
    pub source_platform: Option<String>,
    pub source_region: Option<String>,
    /// A `(Rev N)`/`(Revision N)`-style token, if the source declares one.
    /// Only a JSON manifest can declare this; a `.cht` filename carries no
    /// such field.
    pub source_revision: Option<String>,
    /// Serial/product code, e.g. `"SLUS-12345"`. JSON-manifest only.
    pub source_identifier: Option<String>,
    /// A hash of the *target ROM/content* this cheat set is for (what a
    /// real cheat database calls a game's CRC), distinct from
    /// `source_file_hash` below. JSON-manifest only.
    pub source_content_hash: Option<String>,
    pub target_emulator: Option<String>,
    pub cheat_count: usize,
    pub cheats: Vec<CheatDefinition>,
    pub enabled_by_default_count: usize,
    pub source_file_path: EncodedPath,
    /// SHA-256 hex digest of the exact bytes parsed for this record.
    /// `None` only if the bytes could not be bounded-read at all (should
    /// not happen for a record that exists, but kept optional rather than
    /// asserted).
    pub source_file_hash: Option<String>,
    pub format: CheatCatalogueFormat,
    /// `false` if any parsing diagnostic was emitted for this record
    /// (malformed line, declared/parsed count mismatch, entry index past
    /// [`MAX_CHEATS_PER_GAME`], ...). Mirrors
    /// `retroarch_inventory::CheatFileSummary::complete`.
    pub parsing_complete: bool,
    pub parsing_diagnostics: Vec<CatalogueDiagnostic>,
}

/// A bounded, read-only snapshot of one local catalogue root - the
/// [`CheatCatalogueSource`] output.
#[derive(Debug, Clone, Serialize)]
pub struct CheatCatalogueSnapshot {
    pub format_version: u32,
    pub source_name: String,
    pub source_root: EncodedPath,
    pub read_only: bool,
    /// `false` if any bound (file count, directory count, manifest size,
    /// game count) was reached, or a top-level read failed. Partial
    /// results are never presented as a complete catalogue - mirrors
    /// `RetroArchArtifactInventory::complete`.
    pub complete: bool,
    pub games: Vec<CheatGameRecord>,
    pub diagnostics: Vec<CatalogueDiagnostic>,
}

/// A read-only local cheat catalogue adapter boundary. Only two
/// implementations exist today ([`RetroarchChtDirectorySource`],
/// [`JsonManifestSource`]); a third local format is addable by adding a
/// third implementation, never by growing a hard-coded match in matching
/// code. No implementation of this trait may access the network, write,
/// install, or execute anything - see the module doc comment.
pub trait CheatCatalogueSource {
    fn format(&self) -> CheatCatalogueFormat;
    fn source_name(&self) -> &str;
    fn load(&self, filesystem: &dyn ReadOnlyHostFilesystem, root: &Path) -> CheatCatalogueSnapshot;
}

/// Loads a local catalogue root, auto-selecting the format the same way a
/// user would describe it: an existing directory is read as a `.cht` tree,
/// an existing regular file is read as a JSON manifest. Neither this
/// function nor either source ever searches for a root - the exact path
/// given is the exact path probed, matching the milestone's "no automatic
/// home-directory search" requirement.
pub fn load_cheat_catalogue_snapshot(
    filesystem: &dyn ReadOnlyHostFilesystem,
    source_name: &str,
    root: &Path,
) -> CheatCatalogueSnapshot {
    match filesystem.probe(root) {
        FsProbe::PresentDirectory => {
            RetroarchChtDirectorySource::new(source_name).load(filesystem, root)
        }
        FsProbe::PresentFile => JsonManifestSource::new(source_name).load(filesystem, root),
        probe => empty_snapshot_for_unusable_root(source_name, root, probe),
    }
}

fn empty_snapshot_for_unusable_root(
    source_name: &str,
    root: &Path,
    probe: FsProbe,
) -> CheatCatalogueSnapshot {
    let code = match probe {
        FsProbe::Missing => "catalogue_root_missing",
        FsProbe::Symlink => "catalogue_root_symlink_not_followed",
        FsProbe::WrongType => "catalogue_root_wrong_type",
        FsProbe::Inaccessible => "catalogue_root_inaccessible",
        FsProbe::IoError => "catalogue_root_io_error",
        FsProbe::PresentDirectory | FsProbe::PresentFile => unreachable!(),
    };
    CheatCatalogueSnapshot {
        format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
        source_name: source_name.to_string(),
        source_root: EncodedPath::from_path(root),
        read_only: true,
        complete: false,
        games: Vec::new(),
        diagnostics: vec![CatalogueDiagnostic {
            code,
            severity: ArtifactDiagnosticSeverity::Error,
            path: Some(EncodedPath::from_path(root)),
        }],
    }
}

// ---------------------------------------------------------------------
// RetroArch/libretro `.cht` directory tree source
// ---------------------------------------------------------------------

pub struct RetroarchChtDirectorySource {
    source_name: String,
}

impl RetroarchChtDirectorySource {
    pub fn new(source_name: &str) -> Self {
        Self {
            source_name: source_name.to_string(),
        }
    }
}

impl CheatCatalogueSource for RetroarchChtDirectorySource {
    fn format(&self) -> CheatCatalogueFormat {
        CheatCatalogueFormat::RetroarchChtDirectory
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn load(&self, filesystem: &dyn ReadOnlyHostFilesystem, root: &Path) -> CheatCatalogueSnapshot {
        let mut games = Vec::new();
        let mut diagnostics = Vec::new();
        let mut complete = true;
        let mut total_files = 0usize;

        if filesystem.probe(root) != FsProbe::PresentDirectory {
            return empty_snapshot_for_unusable_root(
                &self.source_name,
                root,
                filesystem.probe(root),
            );
        }

        let mut pending: Vec<(PathBuf, Option<String>)> = vec![(root.to_path_buf(), None)];
        let mut visited = BTreeSet::<PathBuf>::new();
        while let Some((directory, platform_hint)) = pending.pop() {
            if !visited.insert(directory.clone()) {
                continue;
            }
            if visited.len() > MAX_CATALOGUE_DIRECTORIES {
                complete = false;
                diagnostics.push(CatalogueDiagnostic {
                    code: "catalogue_directory_limit_reached",
                    severity: ArtifactDiagnosticSeverity::Warning,
                    path: Some(EncodedPath::from_path(&directory)),
                });
                break;
            }
            match filesystem.list_dir_bounded(&directory, MAX_ARTIFACTS_PER_DIRECTORY) {
                BoundedListResult::Ok(mut entries) => {
                    entries.sort_by(|left, right| {
                        os_str_bytes(&left.file_name).cmp(os_str_bytes(&right.file_name))
                    });
                    for entry in entries {
                        let path = directory.join(&entry.file_name);
                        if entry.probe == FsProbe::PresentDirectory {
                            // Only a *non-symlink* directory is ever queued -
                            // `entry.probe` comes from `symlink_metadata`, so
                            // a symlinked subdirectory reports `Symlink`
                            // here, not `PresentDirectory`, and is silently
                            // never traversed. This is the same no-follow
                            // pattern `retroarch_inventory::scan_cheat_directories`
                            // uses, and it is what keeps a catalogue root
                            // from escaping itself via a symlinked child
                            // directory.
                            // A `.cht` tree has no metadata field for
                            // platform. As a narrow, explicit heuristic,
                            // the immediate child directory of the
                            // catalogue root is offered as a platform
                            // hint for every `.cht` file nested under it -
                            // e.g. `<root>/Super Nintendo Entertainment
                            // System/Game.cht`. Files directly under
                            // `root` get no hint. This is deliberately
                            // shallow: it is not re-derived at deeper
                            // levels, so a deeper nested layout simply
                            // carries the same top-level hint down.
                            let hint = platform_hint.clone().or_else(|| {
                                (directory == *root)
                                    .then(|| entry.file_name.to_string_lossy().into_owned())
                            });
                            pending.push((path, hint));
                            continue;
                        }
                        if total_files >= MAX_CATALOGUE_FILES {
                            complete = false;
                            continue;
                        }
                        let Some(name) = entry.file_name.to_str() else {
                            complete = false;
                            diagnostics.push(CatalogueDiagnostic {
                                code: "catalogue_file_non_utf8_name_skipped",
                                severity: ArtifactDiagnosticSeverity::Warning,
                                path: Some(EncodedPath::from_path(&path)),
                            });
                            continue;
                        };
                        if !name.to_ascii_lowercase().ends_with(".cht") {
                            continue;
                        }
                        total_files += 1;
                        if let Some(record) = load_cht_record(
                            filesystem,
                            &path,
                            platform_hint.as_deref(),
                            &mut diagnostics,
                        ) {
                            games.push(record);
                        } else {
                            complete = false;
                        }
                    }
                    pending.sort_by(|(left, _), (right, _)| {
                        os_str_bytes(right.as_os_str()).cmp(os_str_bytes(left.as_os_str()))
                    });
                }
                result => {
                    complete = false;
                    diagnostics.push(list_diagnostic(&directory, result));
                }
            }
            if games.len() >= MAX_GAME_RECORDS {
                complete = false;
                break;
            }
        }

        games.sort_by(|left, right| {
            left.source_file_path
                .display
                .cmp(&right.source_file_path.display)
        });
        truncate_diagnostics(&mut diagnostics, &mut complete);

        CheatCatalogueSnapshot {
            format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
            source_name: self.source_name.clone(),
            source_root: EncodedPath::from_path(root),
            read_only: true,
            complete,
            games,
            diagnostics,
        }
    }
}

fn list_diagnostic(path: &Path, result: BoundedListResult) -> CatalogueDiagnostic {
    let code = match result {
        BoundedListResult::TooLarge => "catalogue_directory_listing_too_large",
        BoundedListResult::NotFound => "catalogue_directory_missing",
        BoundedListResult::WrongType => "catalogue_directory_wrong_type",
        BoundedListResult::Symlink => "catalogue_directory_symlink_not_followed",
        BoundedListResult::Inaccessible => "catalogue_directory_inaccessible",
        BoundedListResult::IoError => "catalogue_directory_io_error",
        BoundedListResult::Ok(_) => unreachable!(),
    };
    CatalogueDiagnostic {
        code,
        severity: ArtifactDiagnosticSeverity::Warning,
        path: Some(EncodedPath::from_path(path)),
    }
}

fn load_cht_record(
    filesystem: &dyn ReadOnlyHostFilesystem,
    path: &Path,
    platform_hint: Option<&str>,
    diagnostics: &mut Vec<CatalogueDiagnostic>,
) -> Option<CheatGameRecord> {
    let probe = filesystem.probe(path);
    if probe == FsProbe::Symlink {
        diagnostics.push(CatalogueDiagnostic {
            code: "catalogue_file_symlink_not_followed",
            severity: ArtifactDiagnosticSeverity::Warning,
            path: Some(EncodedPath::from_path(path)),
        });
        return None;
    }
    let bytes = match filesystem.read_bounded(path, MAX_CATALOGUE_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) => bytes,
        result => {
            let code = match result {
                BoundedReadResult::TooLarge => "catalogue_file_too_large",
                BoundedReadResult::NotFound => "catalogue_file_disappeared",
                BoundedReadResult::WrongType => "catalogue_file_wrong_type",
                BoundedReadResult::Symlink => "catalogue_file_symlink_not_followed",
                BoundedReadResult::Inaccessible => "catalogue_file_inaccessible",
                BoundedReadResult::IoError => "catalogue_file_io_error",
                BoundedReadResult::Ok(_) => unreachable!(),
            };
            diagnostics.push(CatalogueDiagnostic {
                code,
                severity: ArtifactDiagnosticSeverity::Warning,
                path: Some(EncodedPath::from_path(path)),
            });
            return None;
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        diagnostics.push(CatalogueDiagnostic {
            code: "catalogue_file_invalid_utf8",
            severity: ArtifactDiagnosticSeverity::Warning,
            path: Some(EncodedPath::from_path(path)),
        });
        return None;
    };
    let hash = hex_sha256(&bytes);
    let (cheats, parsing_complete, mut file_diagnostics) = parse_cht_cheats(text, path);
    diagnostics.append(&mut file_diagnostics);

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_string();
    let enabled_by_default_count = cheats
        .iter()
        .filter(|cheat| cheat.enabled_by_default)
        .count();

    Some(CheatGameRecord {
        source_game_name: stem,
        source_platform: platform_hint.map(str::to_string),
        source_region: None,
        source_revision: None,
        source_identifier: None,
        source_content_hash: None,
        target_emulator: Some("retroarch".to_string()),
        cheat_count: cheats.len(),
        cheats,
        enabled_by_default_count,
        source_file_path: EncodedPath::from_path(path),
        source_file_hash: Some(hash),
        format: CheatCatalogueFormat::RetroarchChtDirectory,
        parsing_complete,
        parsing_diagnostics: Vec::new(),
    })
}

/// Parses the same `cheatN_*`/`cheats = N` key-value text format as
/// `retroarch_inventory::parse_cheat_summary`, but keeps one
/// [`CheatDefinition`] per entry index instead of only aggregate counts.
/// Deliberately re-implemented rather than importing that function:
/// `retroarch_inventory` exposes no `pub(crate)` parser today, and
/// widening its visibility is out of scope for this milestone's file-level
/// boundary (see the module doc comment). Cheat *code* lines
/// (`cheatN_code`, `cheatN_code_type`, ...) are read only far enough to
/// confirm the key exists; their values are never stored anywhere in this
/// module's output.
fn parse_cht_cheats(
    text: &str,
    path: &Path,
) -> (Vec<CheatDefinition>, bool, Vec<CatalogueDiagnostic>) {
    use std::collections::BTreeMap;

    let mut declared_cheat_count = None;
    let mut descriptions = BTreeMap::<u32, String>::new();
    let mut enabled = BTreeSet::<u32>::new();
    let mut seen_indices = BTreeSet::<u32>::new();
    let mut diagnostics = Vec::new();
    let mut complete = true;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            complete = false;
            diagnostics.push(malformed_line_diagnostic(path, line_number));
            continue;
        };
        let key = raw_key.trim();
        let value = unquote_cht_value(raw_value.trim());
        if key == "cheats" {
            match value.parse::<u32>() {
                Ok(count) => declared_cheat_count = Some(count),
                Err(_) => {
                    complete = false;
                    diagnostics.push(malformed_line_diagnostic(path, line_number));
                }
            }
            continue;
        }
        let Some(remainder) = key.strip_prefix("cheat") else {
            continue;
        };
        let digit_count = remainder.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 || !remainder[digit_count..].starts_with('_') {
            complete = false;
            diagnostics.push(malformed_line_diagnostic(path, line_number));
            continue;
        }
        let Ok(entry_index) = remainder[..digit_count].parse::<u32>() else {
            complete = false;
            diagnostics.push(malformed_line_diagnostic(path, line_number));
            continue;
        };
        if entry_index as usize >= MAX_CHEATS_PER_GAME {
            complete = false;
            continue;
        }
        seen_indices.insert(entry_index);
        let field = &remainder[digit_count + 1..];
        if field == "desc" && !value.is_empty() {
            descriptions
                .entry(entry_index)
                .or_insert_with(|| value.to_string());
        }
        if field == "enable" && value.eq_ignore_ascii_case("true") {
            enabled.insert(entry_index);
        }
        // `cheatN_code`, `cheatN_code_type`, `cheatN_memory_search_size`,
        // and any other field are intentionally not matched above - their
        // values are read into `value` for the length of this loop
        // iteration only and then dropped.
    }

    if declared_cheat_count.is_some_and(|count| count as usize != seen_indices.len()) {
        complete = false;
    }

    let cheats = seen_indices
        .into_iter()
        .map(|index| CheatDefinition {
            description: descriptions.remove(&index),
            enabled_by_default: enabled.contains(&index),
            declared_index: Some(index),
        })
        .collect();
    (cheats, complete, diagnostics)
}

fn malformed_line_diagnostic(path: &Path, line: u32) -> CatalogueDiagnostic {
    CatalogueDiagnostic {
        code: "catalogue_cht_malformed_line",
        severity: ArtifactDiagnosticSeverity::Warning,
        path: Some(EncodedPath {
            display: format!("{}:{line}", EncodedPath::from_path(path).display),
            lossy: false,
        }),
    }
}

fn unquote_cht_value(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

// ---------------------------------------------------------------------
// Bounded JSON manifest source
// ---------------------------------------------------------------------

pub struct JsonManifestSource {
    source_name: String,
}

impl JsonManifestSource {
    pub fn new(source_name: &str) -> Self {
        Self {
            source_name: source_name.to_string(),
        }
    }
}

impl CheatCatalogueSource for JsonManifestSource {
    fn format(&self) -> CheatCatalogueFormat {
        CheatCatalogueFormat::JsonManifest
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn load(&self, filesystem: &dyn ReadOnlyHostFilesystem, root: &Path) -> CheatCatalogueSnapshot {
        let probe = filesystem.probe(root);
        if probe != FsProbe::PresentFile {
            return empty_snapshot_for_unusable_root(&self.source_name, root, probe);
        }
        let bytes = match filesystem.read_bounded(root, MAX_CATALOGUE_MANIFEST_BYTES) {
            BoundedReadResult::Ok(bytes) => bytes,
            result => {
                let code = match result {
                    BoundedReadResult::TooLarge => "catalogue_manifest_too_large",
                    BoundedReadResult::NotFound => "catalogue_manifest_disappeared",
                    BoundedReadResult::WrongType => "catalogue_manifest_wrong_type",
                    BoundedReadResult::Symlink => "catalogue_manifest_symlink_not_followed",
                    BoundedReadResult::Inaccessible => "catalogue_manifest_inaccessible",
                    BoundedReadResult::IoError => "catalogue_manifest_io_error",
                    BoundedReadResult::Ok(_) => unreachable!(),
                };
                return CheatCatalogueSnapshot {
                    format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
                    source_name: self.source_name.clone(),
                    source_root: EncodedPath::from_path(root),
                    read_only: true,
                    complete: false,
                    games: Vec::new(),
                    diagnostics: vec![CatalogueDiagnostic {
                        code,
                        severity: ArtifactDiagnosticSeverity::Error,
                        path: Some(EncodedPath::from_path(root)),
                    }],
                };
            }
        };
        let hash = hex_sha256(&bytes);
        let mut diagnostics = Vec::new();
        let mut complete = true;

        let document: ManifestDocument = match serde_json::from_slice(&bytes) {
            Ok(document) => document,
            Err(_error) => {
                return CheatCatalogueSnapshot {
                    format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
                    source_name: self.source_name.clone(),
                    source_root: EncodedPath::from_path(root),
                    read_only: true,
                    complete: false,
                    games: Vec::new(),
                    diagnostics: vec![CatalogueDiagnostic {
                        code: "catalogue_manifest_malformed_json",
                        severity: ArtifactDiagnosticSeverity::Error,
                        path: Some(EncodedPath::from_path(root)),
                    }],
                };
            }
        };

        let source_name = document
            .source_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.source_name.clone());

        let mut games = Vec::new();
        for (index, entry) in document.games.into_iter().enumerate() {
            if games.len() >= MAX_GAME_RECORDS {
                complete = false;
                break;
            }
            match build_manifest_record(entry, root, &hash, index) {
                Ok(record) => games.push(record),
                Err(diagnostic) => {
                    complete = false;
                    diagnostics.push(diagnostic);
                }
            }
        }

        games.sort_by(|left, right| {
            left.source_game_name
                .cmp(&right.source_game_name)
                .then_with(|| left.source_platform.cmp(&right.source_platform))
        });
        truncate_diagnostics(&mut diagnostics, &mut complete);

        CheatCatalogueSnapshot {
            format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
            source_name,
            source_root: EncodedPath::from_path(root),
            read_only: true,
            complete,
            games,
            diagnostics,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ManifestDocument {
    #[serde(default)]
    source_name: Option<String>,
    #[serde(default)]
    games: Vec<ManifestGame>,
}

#[derive(Debug, Deserialize)]
struct ManifestGame {
    game_name: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    content_hash: Option<String>,
    #[serde(default)]
    target_emulator: Option<String>,
    #[serde(default)]
    cheats: Vec<ManifestCheat>,
}

#[derive(Debug, Deserialize)]
struct ManifestCheat {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled_by_default: bool,
}

fn build_manifest_record(
    entry: ManifestGame,
    root: &Path,
    file_hash: &str,
    index: usize,
) -> Result<CheatGameRecord, CatalogueDiagnostic> {
    validate_manifest_string("game_name", &entry.game_name, root, index)?;
    if entry.game_name.trim().is_empty() {
        return Err(manifest_diagnostic(
            "catalogue_manifest_empty_game_name",
            root,
            index,
        ));
    }
    if let Some(platform) = &entry.platform {
        validate_manifest_string("platform", platform, root, index)?;
    }
    if let Some(region) = &entry.region {
        validate_manifest_string("region", region, root, index)?;
    }
    if let Some(revision) = &entry.revision {
        validate_manifest_string("revision", revision, root, index)?;
    }
    if let Some(serial) = &entry.serial {
        validate_manifest_string("serial", serial, root, index)?;
    }
    if let Some(hash) = &entry.content_hash {
        validate_manifest_string("content_hash", hash, root, index)?;
    }
    if entry.cheats.len() > MAX_CHEATS_PER_GAME {
        return Err(manifest_diagnostic(
            "catalogue_manifest_cheat_count_limit_reached",
            root,
            index,
        ));
    }

    let mut cheats = Vec::with_capacity(entry.cheats.len());
    for (cheat_index, cheat) in entry.cheats.into_iter().enumerate() {
        if let Some(description) = &cheat.description {
            validate_manifest_string("cheat description", description, root, index)?;
        }
        cheats.push(CheatDefinition {
            description: cheat.description.filter(|value| !value.trim().is_empty()),
            enabled_by_default: cheat.enabled_by_default,
            declared_index: u32::try_from(cheat_index).ok(),
        });
    }
    let enabled_by_default_count = cheats
        .iter()
        .filter(|cheat| cheat.enabled_by_default)
        .count();

    Ok(CheatGameRecord {
        source_game_name: entry.game_name,
        source_platform: entry.platform,
        source_region: entry.region,
        source_revision: entry.revision,
        source_identifier: entry.serial,
        source_content_hash: entry.content_hash,
        target_emulator: entry.target_emulator,
        cheat_count: cheats.len(),
        cheats,
        enabled_by_default_count,
        source_file_path: EncodedPath::from_path(root),
        source_file_hash: Some(file_hash.to_string()),
        format: CheatCatalogueFormat::JsonManifest,
        parsing_complete: true,
        parsing_diagnostics: Vec::new(),
    })
}

fn validate_manifest_string(
    field: &'static str,
    value: &str,
    root: &Path,
    index: usize,
) -> Result<(), CatalogueDiagnostic> {
    if value.len() > MAX_CATALOGUE_STRING_BYTES || value.contains('\0') {
        let _ = field;
        return Err(manifest_diagnostic(
            "catalogue_manifest_string_rejected",
            root,
            index,
        ));
    }
    Ok(())
}

fn manifest_diagnostic(code: &'static str, root: &Path, index: usize) -> CatalogueDiagnostic {
    CatalogueDiagnostic {
        code,
        severity: ArtifactDiagnosticSeverity::Warning,
        path: Some(EncodedPath {
            display: format!("{}#games[{index}]", EncodedPath::from_path(root).display),
            lossy: false,
        }),
    }
}

fn truncate_diagnostics(diagnostics: &mut Vec<CatalogueDiagnostic>, complete: &mut bool) {
    if diagnostics.len() > MAX_CATALOGUE_DIAGNOSTICS {
        diagnostics.truncate(MAX_CATALOGUE_DIAGNOSTICS);
        *complete = false;
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatMatchConfidence {
    Unsupported,
    Ambiguous,
    Weak,
    Strong,
    Exact,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatMatchEvidence {
    /// A stable, fixed identifier for which tier produced this evidence -
    /// never free-text prose. One of `"exact_serial"`,
    /// `"exact_content_hash"`, `"exact_playlist_identity"`,
    /// `"exact_title_platform_region"`, `"exact_title_platform"`,
    /// `"filename_only"`, `"region_mismatch"`, or `"revision_mismatch"`.
    pub tier: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatMatchCandidate {
    pub archive_id: i64,
    pub display_name: String,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatGameMatch {
    pub confidence: CheatMatchConfidence,
    pub evidence: Vec<CheatMatchEvidence>,
    /// More than one only when `confidence == Ambiguous` - a tie is always
    /// shown, never silently resolved to one game.
    pub candidates: Vec<CheatMatchCandidate>,
}

fn normalize_for_matching(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut last_was_space = true; // trims leading separators
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            normalized.extend(ch.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            normalized.push(' ');
            last_was_space = true;
        }
    }
    normalized.truncate(normalized.trim_end().len());
    normalized
}

fn normalize_identifier(text: &str) -> String {
    text.trim().to_ascii_uppercase()
}

/// Strips a `(...)` segment that contains "rev" (case-insensitive) - e.g.
/// `"Chrono Quest (Rev 2)"` -> `"Chrono Quest "` - before title
/// normalization, so a revision-only difference does not by itself make
/// two titles fail to match at all. The stripped-out text is not lost:
/// [`extract_revision_marker`] is separately run on the *original* text so
/// a real revision difference still surfaces as visible evidence rather
/// than being silently treated as identical.
fn title_for_matching(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < text.len() {
        if bytes[index] == b'('
            && let Some(offset) = text[index..].find(')')
        {
            let segment = &text[index..index + offset + 1];
            if segment.to_ascii_lowercase().contains("rev") {
                index += offset + 1;
                continue;
            }
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index is a char boundary");
        result.push(ch);
        index += ch.len_utf8();
    }
    normalize_for_matching(&result)
}

fn extract_revision_marker(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let position = lower.find("rev")?;
    let after = &text[position + 3..];
    let token: String = after
        .trim_start_matches(|character: char| character == '.' || character.is_whitespace())
        .chars()
        .take_while(|character| character.is_alphanumeric())
        .collect();
    (!token.is_empty()).then_some(token.to_ascii_uppercase())
}

struct CandidateGame<'a> {
    archive_id: i64,
    display_name: &'a str,
    normalized_title: String,
    platform: Option<&'a str>,
    region: Option<&'a str>,
    serial: Option<&'a str>,
    content_hash: Option<&'a str>,
    exact_playlist_identity: bool,
}

fn candidate_games<'a>(
    catalogue_games: &'a [CatalogueGameEvidence],
    advisory_plan: Option<&RetroArchAdvisoryPlan>,
) -> Vec<CandidateGame<'a>> {
    let exact_playlist_archive_ids: BTreeSet<i64> = advisory_plan
        .map(|plan| {
            plan.entries
                .iter()
                .filter(|entry| {
                    entry.profile_outcomes.iter().any(|outcome| {
                        outcome
                            .playlist_evidence
                            .iter()
                            .any(|evidence| evidence.confidence == PlaylistMatchConfidence::Exact)
                    })
                })
                .map(|entry| entry.archive_id)
                .collect()
        })
        .unwrap_or_default();

    catalogue_games
        .iter()
        .filter(|game| game.is_present)
        .map(|game| CandidateGame {
            archive_id: game.archive_id,
            display_name: &game.display_name,
            normalized_title: title_for_matching(&game.display_name),
            platform: game.platform.as_deref(),
            region: game.region.as_deref(),
            serial: game.serial.as_deref(),
            content_hash: game.executable_crc.as_deref(),
            exact_playlist_identity: exact_playlist_archive_ids.contains(&game.archive_id),
        })
        .collect()
}

/// Matches one catalogue game record against already-loaded evidence,
/// using the conservative tiers documented in
/// `docs/RETROARCH_CHEAT_CATALOGUE.md`. Never mutates anything; pure
/// function of its inputs. `advisory_plan` is optional - passing `None`
/// still evaluates every tier except playlist identity.
pub fn match_cheat_game_record(
    record: &CheatGameRecord,
    catalogue_games: &[CatalogueGameEvidence],
    advisory_plan: Option<&RetroArchAdvisoryPlan>,
) -> CheatGameMatch {
    let candidates = candidate_games(catalogue_games, advisory_plan);
    let record_title = title_for_matching(&record.source_game_name);
    let canonical_source_platform = record.source_platform.as_deref().and_then(|source| {
        canonical_platform_for_alias(source).map(|canonical| (source, canonical))
    });
    let record_revision = record
        .source_revision
        .as_deref()
        .map(str::to_string)
        .or_else(|| extract_revision_marker(&record.source_game_name));

    // Tier 1: exact serial/product code.
    if let Some(identifier) = record.source_identifier.as_deref() {
        let needle = normalize_identifier(identifier);
        let hits = candidates
            .iter()
            .filter(|candidate| candidate.serial.map(normalize_identifier) == Some(needle.clone()))
            .collect::<Vec<_>>();
        if let Some(outcome) =
            exact_or_ambiguous(&hits, "exact_serial", format!("serial {identifier}"))
        {
            return outcome;
        }
    }

    // Tier 2: exact known content hash.
    if let Some(hash) = record.source_content_hash.as_deref() {
        let needle = normalize_identifier(hash);
        let hits = candidates
            .iter()
            .filter(|candidate| {
                candidate.content_hash.map(normalize_identifier) == Some(needle.clone())
            })
            .collect::<Vec<_>>();
        if let Some(outcome) =
            exact_or_ambiguous(&hits, "exact_content_hash", format!("content hash {hash}"))
        {
            return outcome;
        }
    }

    // Tier 3: exact playlist identity - title+platform must also agree, so
    // a playlist-exact archive for an unrelated game can never be pulled
    // in purely because *some* playlist entry elsewhere was exact.
    if let Some((_, platform)) = canonical_source_platform {
        let hits = candidates
            .iter()
            .filter(|candidate| candidate.exact_playlist_identity)
            .filter(|candidate| candidate.normalized_title == record_title)
            .filter(|candidate| {
                candidate
                    .platform
                    .and_then(canonical_platform_for_alias)
                    .is_some_and(|value| value == platform)
            })
            .collect::<Vec<_>>();
        if let Some(outcome) = exact_or_ambiguous(
            &hits,
            "exact_playlist_identity",
            "exact playlist content-path match".to_string(),
        ) {
            return outcome;
        }
    }

    // Tier 4: exact normalized title + platform + region.
    if let (Some((source_platform, platform)), Some(region)) =
        (canonical_source_platform, record.source_region.as_deref())
    {
        let normalized_region = normalize_for_matching(region);
        let hits = candidates
            .iter()
            .filter(|candidate| candidate.normalized_title == record_title)
            .filter(|candidate| {
                candidate
                    .platform
                    .and_then(canonical_platform_for_alias)
                    .is_some_and(|value| value == platform)
            })
            .filter(|candidate| {
                candidate
                    .region
                    .map(normalize_for_matching)
                    .is_some_and(|value| value == normalized_region)
            })
            .collect::<Vec<_>>();
        if let Some(mut outcome) = exact_or_ambiguous(
            &hits,
            "exact_title_platform_region",
            format!(
                "normalized title, canonical platform, and region match ({source_platform} -> {platform}, {region})"
            ),
        ) {
            if outcome.confidence == CheatMatchConfidence::Exact {
                outcome.confidence = CheatMatchConfidence::Strong;
            }
            return outcome;
        }
    }

    // Tier 5: exact normalized title + platform (region ignored, but a
    // declared-and-differing region on both sides stays visible as an
    // extra evidence entry rather than being silently dropped).
    if let Some((source_platform, platform)) = canonical_source_platform {
        let hits = candidates
            .iter()
            .filter(|candidate| candidate.normalized_title == record_title)
            .filter(|candidate| {
                candidate
                    .platform
                    .and_then(canonical_platform_for_alias)
                    .is_some_and(|value| value == platform)
            })
            .collect::<Vec<_>>();
        if let Some(mut outcome) = exact_or_ambiguous(
            &hits,
            "exact_title_platform",
            format!(
                "normalized title and canonical platform match ({source_platform} -> {platform})"
            ),
        ) {
            if outcome.confidence == CheatMatchConfidence::Exact {
                outcome.confidence = CheatMatchConfidence::Strong;
            }
            if hits.len() == 1 {
                if let Some(region) = record.source_region.as_deref()
                    && let Some(candidate_region) = hits[0].region
                    && normalize_for_matching(region) != normalize_for_matching(candidate_region)
                {
                    outcome.evidence.push(CheatMatchEvidence {
                        tier: "region_mismatch",
                        detail: format!(
                            "catalogue declares region {region}, matched archive declares {candidate_region}"
                        ),
                    });
                }
                if let Some(revision) = record_revision.as_deref()
                    && let Some(candidate_revision) = extract_revision_marker(hits[0].display_name)
                    && revision != candidate_revision
                {
                    outcome.evidence.push(CheatMatchEvidence {
                        tier: "revision_mismatch",
                        detail: format!(
                            "catalogue declares revision {revision}, matched archive declares {candidate_revision}"
                        ),
                    });
                }
            }
            return outcome;
        }
    }

    // Tier 6: filename-only evidence (normalized title alone, no platform
    // corroboration).
    let hits = candidates
        .iter()
        .filter(|candidate| candidate.normalized_title == record_title)
        .collect::<Vec<_>>();
    if let Some(mut outcome) = exact_or_ambiguous(
        &hits,
        "filename_only",
        "normalized title match only".to_string(),
    ) {
        if outcome.confidence == CheatMatchConfidence::Exact {
            outcome.confidence = CheatMatchConfidence::Weak;
        }
        return outcome;
    }

    CheatGameMatch {
        confidence: CheatMatchConfidence::Unsupported,
        evidence: Vec::new(),
        candidates: Vec::new(),
    }
}

/// Shared "one hit is exact, more than one is ambiguous, zero falls
/// through" rule used by every tier above - mirrors
/// `retroarch_inventory::unique_game_count`'s tie-detection, generalized
/// to also carry the winning/tied evidence text.
fn exact_or_ambiguous(
    hits: &[&CandidateGame<'_>],
    tier: &'static str,
    detail: String,
) -> Option<CheatGameMatch> {
    let unique_ids: BTreeSet<i64> = hits.iter().map(|candidate| candidate.archive_id).collect();
    match unique_ids.len() {
        0 => None,
        1 => Some(CheatGameMatch {
            confidence: CheatMatchConfidence::Exact,
            evidence: vec![CheatMatchEvidence { tier, detail }],
            candidates: vec![to_match_candidate(hits[0])],
        }),
        _ => {
            let mut candidates: Vec<CheatMatchCandidate> =
                hits.iter().map(|hit| to_match_candidate(hit)).collect();
            candidates.sort_by_key(|candidate| candidate.archive_id);
            candidates.dedup_by_key(|candidate| candidate.archive_id);
            Some(CheatGameMatch {
                confidence: CheatMatchConfidence::Ambiguous,
                evidence: vec![CheatMatchEvidence {
                    tier,
                    detail: format!("{detail} (tied across {} games)", candidates.len()),
                }],
                candidates,
            })
        }
    }
}

fn to_match_candidate(candidate: &CandidateGame<'_>) -> CheatMatchCandidate {
    CheatMatchCandidate {
        archive_id: candidate.archive_id,
        display_name: candidate.display_name.to_string(),
        platform: candidate.platform.map(str::to_string),
    }
}

// ---------------------------------------------------------------------
// Installed-state integration
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatInstalledState {
    /// No matched archive, no advisory plan, or the expected destination
    /// does not exist.
    NotInstalled,
    /// The expected per-game cheat destination exists and its SHA-256
    /// matches this catalogue record's own file hash exactly.
    ExactFilePresent,
    /// Some other `.cht` finding for the same matched game has the same
    /// SHA-256 as this catalogue record, but under a different filename
    /// than the expected destination.
    SameSetDifferentFilename,
    /// The expected destination exists as a regular file whose hash
    /// differs from this catalogue record.
    DestinationOccupiedDifferentContent,
    /// More than one installed `.cht` finding associates with the matched
    /// game - never picked between silently.
    MultipleInstalledCandidates,
    /// The installed file at the expected destination parsed with
    /// diagnostics (`retroarch_inventory::CheatFileSummary::complete ==
    /// false`).
    InstalledFileMalformed,
    /// The expected destination's final path component is itself a
    /// symlink - never followed, never hashed.
    DestinationSymlink,
    /// The expected destination could not be read (permission denied or an
    /// I/O error).
    InaccessibleDestination,
    /// No game match, or no advisory plan/artifact inventory was supplied,
    /// so installed-state cannot be evaluated at all.
    Unknown,
}

fn resolve_installed_state(
    filesystem: &dyn ReadOnlyHostFilesystem,
    record: &CheatGameRecord,
    matched_archive_id: Option<i64>,
    advisory_plan: Option<&RetroArchAdvisoryPlan>,
) -> (CheatInstalledState, Vec<String>) {
    let (Some(archive_id), Some(plan)) = (matched_archive_id, advisory_plan) else {
        return (CheatInstalledState::Unknown, Vec::new());
    };

    let destinations: Vec<&RetroArchArtifactDestination> = plan
        .artifact_inventory
        .destinations
        .iter()
        .filter(|destination| {
            destination.artifact_kind == ArtifactKind::Cheat
                && destination
                    .catalogue_games
                    .iter()
                    .any(|game| game.archive_id == archive_id)
        })
        .collect();

    if destinations.is_empty() {
        return (CheatInstalledState::Unknown, Vec::new());
    }
    if destinations.len() > 1 {
        return (
            CheatInstalledState::MultipleInstalledCandidates,
            destinations
                .iter()
                .map(|destination| destination.path.display.clone())
                .collect(),
        );
    }
    let destination = destinations[0];
    let mut detail = vec![format!(
        "expected destination: {}",
        destination.path.display
    )];

    match destination.state {
        ArtifactConflictState::Empty => return (CheatInstalledState::NotInstalled, detail),
        ArtifactConflictState::Ambiguous => {
            return (CheatInstalledState::MultipleInstalledCandidates, detail);
        }
        _ => {}
    }

    match destination.probe {
        FsProbe::Missing => return (CheatInstalledState::NotInstalled, detail),
        FsProbe::Symlink => return (CheatInstalledState::DestinationSymlink, detail),
        FsProbe::Inaccessible | FsProbe::IoError => {
            return (CheatInstalledState::InaccessibleDestination, detail);
        }
        FsProbe::PresentDirectory | FsProbe::WrongType => {
            return (
                CheatInstalledState::DestinationOccupiedDifferentContent,
                detail,
            );
        }
        FsProbe::PresentFile => {}
    }

    let installed_finding = plan.artifact_inventory.findings.iter().find(|finding| {
        finding.artifact_kind == ArtifactKind::Cheat && finding.path == destination.path
    });
    if let Some(finding) = installed_finding
        && let Some(summary) = &finding.cheat_summary
        && !summary.complete
    {
        detail.push("installed cheat file parsed with diagnostics".to_string());
        return (CheatInstalledState::InstalledFileMalformed, detail);
    }

    let destination_path = Path::new(&destination.path.display);
    match filesystem.read_bounded(destination_path, MAX_CATALOGUE_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) => {
            let installed_hash = hex_sha256(&bytes);
            match &record.source_file_hash {
                Some(catalogue_hash) if *catalogue_hash == installed_hash => {
                    (CheatInstalledState::ExactFilePresent, detail)
                }
                _ => {
                    detail.push("installed content hash differs from catalogue file".to_string());
                    (
                        CheatInstalledState::DestinationOccupiedDifferentContent,
                        detail,
                    )
                }
            }
        }
        _ => {
            detail.push("installed file could not be re-read for hash comparison".to_string());
            (
                CheatInstalledState::DestinationOccupiedDifferentContent,
                detail,
            )
        }
    }
}

// ---------------------------------------------------------------------
// Availability report
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CheatAvailabilityEntry {
    pub game: CheatGameRecord,
    pub game_match: CheatGameMatch,
    pub installed_state: CheatInstalledState,
    pub installed_state_detail: Vec<String>,
    /// `true` only when the match is `exact` or `strong`, the record
    /// parsed with no diagnostics, and the installed state is one where
    /// staging would not silently overwrite unrelated content
    /// (`not_installed`, `exact_file_present`, or
    /// `same_set_different_filename`). This field never causes any
    /// install/copy/write - it is advisory metadata only, consistent with
    /// this milestone's read-only scope.
    pub staging_candidate: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CheatAvailabilitySummary {
    pub games_in_catalogue: usize,
    pub exact_matches: usize,
    pub strong_matches: usize,
    pub weak_matches: usize,
    pub ambiguous_matches: usize,
    pub unsupported_matches: usize,
    pub not_installed: usize,
    pub already_installed: usize,
    pub conflicts: usize,
    pub staging_candidates: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatAvailabilityReport {
    pub format_version: u32,
    pub read_only: bool,
    pub complete: bool,
    pub source_name: String,
    pub source_root: EncodedPath,
    pub entries: Vec<CheatAvailabilityEntry>,
    pub summary: CheatAvailabilitySummary,
    pub diagnostics: Vec<CatalogueDiagnostic>,
}

/// Builds the full availability report: matches every game in `snapshot`
/// against `catalogue_games` (and, optionally, `advisory_plan` for
/// playlist-identity evidence and installed-state), then resolves
/// installed-state for whichever single archive each record matched.
/// Ambiguous/unsupported matches always get `Unknown` installed-state -
/// this function never guesses which of several tied candidates a cheat
/// file "belongs to" in order to report on it.
///
/// The only I/O this function performs is the same bounded, no-follow
/// `read_bounded` the rest of this module already uses, to hash an
/// already-`Occupied`/`Matched` destination for the exact-file-present
/// comparison. Nothing is written, created, renamed, deleted, executed, or
/// requested over the network.
pub fn build_cheat_availability_report(
    filesystem: &dyn ReadOnlyHostFilesystem,
    snapshot: &CheatCatalogueSnapshot,
    catalogue_games: &[CatalogueGameEvidence],
    advisory_plan: Option<&RetroArchAdvisoryPlan>,
) -> CheatAvailabilityReport {
    let mut entries = Vec::with_capacity(snapshot.games.len());
    let mut summary = CheatAvailabilitySummary {
        games_in_catalogue: snapshot.games.len(),
        ..Default::default()
    };

    for game in &snapshot.games {
        let game_match = match_cheat_game_record(game, catalogue_games, advisory_plan);
        match game_match.confidence {
            CheatMatchConfidence::Exact => summary.exact_matches += 1,
            CheatMatchConfidence::Strong => summary.strong_matches += 1,
            CheatMatchConfidence::Weak => summary.weak_matches += 1,
            CheatMatchConfidence::Ambiguous => summary.ambiguous_matches += 1,
            CheatMatchConfidence::Unsupported => summary.unsupported_matches += 1,
        }

        let single_candidate = (game_match.candidates.len() == 1
            && matches!(
                game_match.confidence,
                CheatMatchConfidence::Exact
                    | CheatMatchConfidence::Strong
                    | CheatMatchConfidence::Weak
            ))
        .then(|| game_match.candidates[0].archive_id);

        let (installed_state, installed_state_detail) =
            resolve_installed_state(filesystem, game, single_candidate, advisory_plan);

        match installed_state {
            CheatInstalledState::NotInstalled => summary.not_installed += 1,
            CheatInstalledState::ExactFilePresent
            | CheatInstalledState::SameSetDifferentFilename => {
                summary.already_installed += 1;
            }
            CheatInstalledState::DestinationOccupiedDifferentContent
            | CheatInstalledState::MultipleInstalledCandidates
            | CheatInstalledState::InstalledFileMalformed
            | CheatInstalledState::DestinationSymlink
            | CheatInstalledState::InaccessibleDestination => summary.conflicts += 1,
            CheatInstalledState::Unknown => {}
        }

        let staging_candidate = game.parsing_complete
            && matches!(
                game_match.confidence,
                CheatMatchConfidence::Exact | CheatMatchConfidence::Strong
            )
            && matches!(
                installed_state,
                CheatInstalledState::NotInstalled
                    | CheatInstalledState::ExactFilePresent
                    | CheatInstalledState::SameSetDifferentFilename
            );
        if staging_candidate {
            summary.staging_candidates += 1;
        }

        entries.push(CheatAvailabilityEntry {
            game: game.clone(),
            game_match,
            installed_state,
            installed_state_detail,
            staging_candidate,
        });
    }

    entries.sort_by(|left, right| {
        left.game
            .source_file_path
            .display
            .cmp(&right.game.source_file_path.display)
    });

    CheatAvailabilityReport {
        format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
        read_only: true,
        complete: snapshot.complete,
        source_name: snapshot.source_name.clone(),
        source_root: snapshot.source_root.clone(),
        entries,
        summary,
        diagnostics: snapshot.diagnostics.clone(),
    }
}

#[cfg(test)]
mod tests;
