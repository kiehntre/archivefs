//! Library Views: named, symlink-based organised folder trees pointing at
//! existing archive files - never a copy, move, rename, or modification of
//! any original archive.
//!
//! # Safety model
//!
//! - A view's destination root may never be inside a configured source
//!   folder, and no configured source folder may be inside a destination
//!   root (`validate_library_view_destination`).
//! - Every symlink ArchiveFS creates is recorded in a per-view manifest
//!   (`LibraryViewManifest`); cleanup (`remove_library_view_symlinks`) only
//!   ever removes a path that is *still* a symlink pointing at the *exact*
//!   target the manifest recorded for it - never a path that has since
//!   become a real file or been repointed by something else.
//! - Planning (`plan_library_view`) performs no filesystem mutation at
//!   all - only reads (`fs::symlink_metadata`) to classify what already
//!   exists.
//! - Generated relative link paths are rejected outright if they contain a
//!   `..` component, an absolute path, or any component that would place
//!   the final destination outside the view's destination root.
//! - Two archives that would generate the same destination path are
//!   reported as a collision and neither is linked - this milestone never
//!   invents an automatic disambiguating suffix.
//! - Config/manifest writes are atomic (`crate::atomic_write_text`, plus
//!   the same temp-file-then-rename shape applied directly to symlink
//!   creation in `apply_library_view`).

use crate::{
    ArchiveFsError, Database, PersistedArchive, Result, SourceFolderRecord, atomic_write_text,
    default_database_path,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A `PathBuf` that survives a JSON round-trip byte-for-byte even when it
/// is not valid UTF-8. JSON strings must be valid Unicode, but archive
/// filenames come straight off the filesystem and are never guaranteed to
/// be - a manifest that simply refused to serialize such a path (the
/// default derived behaviour for `PathBuf`) would mean a single
/// non-UTF-8 archive could break an entire view's manifest write.
///
/// The common, valid-UTF-8 case still serializes as a plain, readable,
/// diffable JSON string. The rare invalid case falls back to a small JSON
/// *object* carrying the exact raw bytes hex-encoded - deliberately a
/// different JSON type (object, not string) so it can never be confused
/// with a normal path string on the way back in.
///
/// Used only via the `path_json`/`option_path_json`/`vec_path_json`
/// `serde(with = ...)` helper modules below, so every public field stays a
/// plain `PathBuf`/`Option<PathBuf>`/`Vec<PathBuf>` - this wrapper is purely
/// a (de)serialization detail.
struct PathJson(PathBuf);

impl Serialize for PathJson {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.0.to_str() {
            Some(text) => serializer.serialize_str(text),
            None => {
                let mut object = serde_json::Map::new();
                object.insert(
                    "invalid_utf8_hex".to_string(),
                    serde_json::Value::String(encode_hex(self.0.as_os_str().as_bytes())),
                );
                serde_json::Value::Object(object).serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for PathJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(text) => Ok(PathJson(PathBuf::from(text))),
            serde_json::Value::Object(object) => {
                let hex = object
                    .get("invalid_utf8_hex")
                    .and_then(|field| field.as_str())
                    .ok_or_else(|| {
                        serde::de::Error::custom("expected an invalid_utf8_hex field")
                    })?;
                let bytes = decode_hex(hex).map_err(serde::de::Error::custom)?;
                Ok(PathJson(PathBuf::from(OsString::from_vec(bytes))))
            }
            _ => Err(serde::de::Error::custom(
                "expected a path string or an invalid-utf8 object",
            )),
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(hex: &str) -> std::result::Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16).map_err(|error| error.to_string())
        })
        .collect()
}

/// `serde(with = "path_json")` for a plain `PathBuf` field.
mod path_json {
    use super::PathJson;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};

    pub fn serialize<S: serde::Serializer>(
        path: &Path,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        PathJson(path.to_path_buf()).serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<PathBuf, D::Error> {
        PathJson::deserialize(deserializer).map(|wrapped| wrapped.0)
    }
}

/// `serde(with = "option_path_json")` for an `Option<PathBuf>` field.
mod option_path_json {
    use super::PathJson;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    pub fn serialize<S: serde::Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        path.as_ref()
            .map(|inner| PathJson(inner.clone()))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<PathBuf>, D::Error> {
        Option::<PathJson>::deserialize(deserializer).map(|option| option.map(|wrapped| wrapped.0))
    }
}

/// `serde(with = "vec_path_json")` for a `Vec<PathBuf>` field.
mod vec_path_json {
    use super::PathJson;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    pub fn serialize<S: serde::Serializer>(
        paths: &[PathBuf],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        paths
            .iter()
            .map(|path| PathJson(path.clone()))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<PathBuf>, D::Error> {
        Vec::<PathJson>::deserialize(deserializer)
            .map(|paths| paths.into_iter().map(|wrapped| wrapped.0).collect())
    }
}

/// A named, symlink-based organised view of the catalogue. Mirrors
/// `SourceFolderConfig`'s "load the full list, mutate in memory, save back
/// atomically" shape (see `load_library_view_configs_from`/
/// `save_library_view_configs_to`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewConfig {
    /// Stable identity, independent of `name` (which the user may rename)
    /// - generated once by `generate_library_view_id` and never reused.
    pub id: String,
    pub name: String,
    #[serde(with = "path_json")]
    pub destination_root: PathBuf,
    pub enabled: bool,
    /// Every configured source folder is included when this is empty.
    #[serde(with = "vec_path_json")]
    pub source_folders: Vec<PathBuf>,
    /// Every known (non-Unknown) platform is included when this is empty -
    /// an Unknown-platform archive is always skipped regardless (see
    /// `plan_library_view`'s doc comment).
    pub platforms: Vec<String>,
    pub layout_template: LibraryViewLayoutTemplate,
}

/// The only layout template this milestone supports - see the milestone's
/// explicit scope note. Deliberately an enum (not a free-form string
/// template) so an invalid/unsupported template can never be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryViewLayoutTemplate {
    /// `{platform}/{filename}`.
    PlatformFilename,
}

impl LibraryViewLayoutTemplate {
    pub fn label(self) -> &'static str {
        match self {
            Self::PlatformFilename => "{platform}/{filename}",
        }
    }
}

/// Resolves `~/.config/archivefs/library_views.json` - the config-y "list
/// of views" file, alongside `config.toml`/`source_folders` in the same
/// `.config/archivefs` directory. JSON rather than another hand-rolled
/// `[[block]]` format: each view's `source_folders`/`platforms` are list
/// fields, which the existing line-based TOML parser (`parse_config_fields`)
/// has no support for nesting inside a block - `serde_json` (already an
/// archivefs-cli dependency) avoids inventing that parser just for this.
pub fn default_library_views_config_path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("library_views.json"))
}

/// Resolves `~/.local/share/archivefs/library_views/` - per-view manifests
/// live here (see `library_view_manifest_path`), alongside
/// `library.sqlite3`/`index.json` in the same `.local/share/archivefs`
/// application-data directory, deliberately never inside a user's source
/// folder.
pub fn default_library_views_data_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("archivefs")
        .join("library_views"))
}

/// The exact manifest path for one view - `{data_dir}/{id}.manifest.json`.
/// Keyed by `id`, never `name`, so renaming a view never orphans its
/// manifest.
pub fn library_view_manifest_path(data_dir: &Path, view_id: &str) -> PathBuf {
    data_dir.join(format!("{view_id}.manifest.json"))
}

/// A short, unique-enough identifier: process id + a monotonic counter +
/// wall-clock nanoseconds, hex-encoded - the same "no external `uuid`
/// dependency, PID plus an atomic sequence" shape `atomic_write_text`
/// already uses for its temp-file names, applied here to a stable,
/// permanent identity instead of a throwaway one.
pub fn generate_library_view_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{:x}-{:x}-{:x}",
        nanos,
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn load_library_view_configs_default() -> Result<Vec<LibraryViewConfig>> {
    load_library_view_configs_from(default_library_views_config_path()?)
}

/// A missing file is treated as "no views configured yet", not an error -
/// exactly like a first-run config, so a fresh install never needs an
/// explicit initialization step for this feature.
pub fn load_library_view_configs_from(path: impl AsRef<Path>) -> Result<Vec<LibraryViewConfig>> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(contents) => parse_library_view_configs(&contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(ArchiveFsError::io(path.to_path_buf(), error)),
    }
}

fn parse_library_view_configs(contents: &str) -> Result<Vec<LibraryViewConfig>> {
    serde_json::from_str(contents).map_err(|error| {
        ArchiveFsError::Config(format!("library views config is invalid: {error}"))
    })
}

pub fn save_library_view_configs_default(views: &[LibraryViewConfig]) -> Result<()> {
    save_library_view_configs_to(default_library_views_config_path()?, views)
}

pub fn save_library_view_configs_to(
    path: impl AsRef<Path>,
    views: &[LibraryViewConfig],
) -> Result<()> {
    let contents = serde_json::to_string_pretty(views).map_err(|error| {
        ArchiveFsError::Config(format!("cannot serialize library views: {error}"))
    })?;
    atomic_write_text(path.as_ref(), &contents)
}

/// One managed symlink ArchiveFS created, as recorded in a view's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewManifestEntry {
    /// Relative to the view's `destination_root` at the time this entry
    /// was written - never an absolute path, so a manifest stays valid if
    /// the whole destination tree is ever relocated by the user outside
    /// ArchiveFS (repair would then simply report every entry as broken).
    #[serde(with = "path_json")]
    pub relative_link_path: PathBuf,
    /// The exact symlink target - never a lossy display string, so a
    /// non-UTF-8 archive path round-trips exactly (requirement: "preserve
    /// exact underlying target paths even when display strings are
    /// lossy").
    #[serde(with = "path_json")]
    pub target_path: PathBuf,
    /// A lightweight drift indicator (`"{size}:{modified_unix_seconds}"`),
    /// not a content hash - Library Views map names to paths, they do not
    /// verify archive integrity. `None` when the source archive's
    /// size/modified time were not available at write time.
    pub archive_identity: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub platform: String,
    #[serde(with = "path_json")]
    pub source_folder_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewManifest {
    pub view_id: String,
    #[serde(with = "path_json")]
    pub destination_root: PathBuf,
    pub entries: Vec<LibraryViewManifestEntry>,
}

impl LibraryViewManifest {
    fn empty(view_id: &str, destination_root: &Path) -> Self {
        Self {
            view_id: view_id.to_string(),
            destination_root: destination_root.to_path_buf(),
            entries: Vec::new(),
        }
    }
}

pub fn load_library_view_manifest_default(view_id: &str) -> Result<LibraryViewManifest> {
    load_library_view_manifest_at(&default_library_views_data_dir()?, view_id)
}

/// A missing manifest file means "this view has never been applied yet" -
/// returns an empty manifest rather than an error, exactly like a missing
/// config file.
pub fn load_library_view_manifest_at(
    data_dir: &Path,
    view_id: &str,
) -> Result<LibraryViewManifest> {
    let path = library_view_manifest_path(data_dir, view_id);
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
            ArchiveFsError::Config(format!(
                "manifest for library view {view_id} is invalid: {error}"
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(LibraryViewManifest::empty(view_id, Path::new("")))
        }
        Err(error) => Err(ArchiveFsError::io(path, error)),
    }
}

fn save_library_view_manifest_at(data_dir: &Path, manifest: &LibraryViewManifest) -> Result<()> {
    let path = library_view_manifest_path(data_dir, &manifest.view_id);
    let contents = serde_json::to_string_pretty(manifest).map_err(|error| {
        ArchiveFsError::Config(format!("cannot serialize library view manifest: {error}"))
    })?;
    atomic_write_text(&path, &contents)
}

fn now_utc_string() -> String {
    crate::format_unix_timestamp_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------
// Safety validation.
// ---------------------------------------------------------------------

/// Validates that `destination_root` is safe for a Library View: it must
/// not be inside any `source_folders` entry, and no `source_folders` entry
/// may be inside it. Mirrors `validate_new_source_folder`'s exact
/// canonicalize-then-`starts_with` containment check, generalized to check
/// both directions at once (a destination need not exist yet, unlike a
/// source folder, so its own side of the check walks up to the nearest
/// existing ancestor first - see `canonical_or_nearest_existing_ancestor`).
pub fn validate_library_view_destination(
    destination_root: &Path,
    source_folders: &[PathBuf],
) -> Result<PathBuf> {
    let normalized: PathBuf = destination_root.components().collect();
    if normalized.as_os_str().is_empty() {
        return Err(ArchiveFsError::Config(
            "a Library View destination folder is required".to_string(),
        ));
    }
    let destination_canonical = canonical_or_nearest_existing_ancestor(&normalized)?;

    for source in source_folders {
        let source_canonical = fs::canonicalize(source).unwrap_or_else(|_| source.clone());
        if destination_canonical == source_canonical {
            return Err(ArchiveFsError::Config(format!(
                "{} is a configured source folder - a Library View's destination must be a \
                 separate directory",
                normalized.display()
            )));
        }
        if destination_canonical.starts_with(&source_canonical) {
            return Err(ArchiveFsError::Config(format!(
                "{} is inside the configured source folder {} - a Library View's destination \
                 must never be inside a source folder",
                normalized.display(),
                source.display()
            )));
        }
        if source_canonical.starts_with(&destination_canonical) {
            return Err(ArchiveFsError::Config(format!(
                "the configured source folder {} is inside {} - a source folder must never be \
                 inside a Library View's destination",
                source.display(),
                normalized.display()
            )));
        }
    }

    Ok(normalized)
}

/// Canonicalizes `path` if it exists; otherwise walks up to the nearest
/// existing ancestor, canonicalizes *that*, and rejoins the non-existent
/// suffix - the same "resolve a not-yet-created path safely" shape
/// `resolved_mount_target` already uses for mount targets, applied here so
/// a symlinked ancestor directory can never be used to smuggle a Library
/// View's real destination outside of what the user typed.
fn canonical_or_nearest_existing_ancestor(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source));
    }
    let mut existing_parent = path.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "cannot resolve a safe parent for {}",
            path.display()
        ))
    })?;
    while !existing_parent.exists() {
        existing_parent = existing_parent.parent().ok_or_else(|| {
            ArchiveFsError::Config(format!(
                "cannot resolve a safe parent for {}",
                path.display()
            ))
        })?;
    }
    let canonical_parent = fs::canonicalize(existing_parent)
        .map_err(|source| ArchiveFsError::io(existing_parent.to_path_buf(), source))?;
    let suffix = path.strip_prefix(existing_parent).map_err(|_| {
        ArchiveFsError::Config(format!(
            "cannot resolve {} from {}",
            path.display(),
            existing_parent.display()
        ))
    })?;
    Ok(canonical_parent.join(suffix))
}

// ---------------------------------------------------------------------
// Layout / path generation.
// ---------------------------------------------------------------------

/// Builds the relative link path for one archive under `template`,
/// rejecting anything that could escape the destination root (milestone
/// requirement: "reject path traversal through generated names"). The
/// filename is taken directly from `archive_path.file_name()` (an
/// `OsStr`, never lossily converted), so a non-UTF-8 archive filename is
/// preserved exactly rather than mangled or rejected outright.
pub fn generate_relative_link_path(
    template: LibraryViewLayoutTemplate,
    platform: &str,
    archive_path: &Path,
) -> Result<PathBuf> {
    let LibraryViewLayoutTemplate::PlatformFilename = template;
    let platform_component = sanitize_path_component_str(platform)?;
    let filename = archive_path.file_name().ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "{} has no filename to use in a Library View",
            archive_path.display()
        ))
    })?;
    let filename_component = sanitize_path_component_os(filename)?;
    Ok(PathBuf::from(platform_component).join(filename_component))
}

/// Rejects an empty string, `.`/`..`, or anything containing a path
/// separator - `PathBuf::join` alone would happily accept `"../../etc"` as
/// a single string and produce exactly the traversal this must reject.
fn sanitize_path_component_str(raw: &str) -> Result<String> {
    if raw.is_empty() {
        return Err(ArchiveFsError::Config(
            "a Library View path component cannot be empty".to_string(),
        ));
    }
    let as_path = Path::new(raw);
    let mut components = as_path.components();
    let is_single_normal_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !is_single_normal_component {
        return Err(ArchiveFsError::Config(format!(
            "{raw:?} is not a safe Library View path component"
        )));
    }
    Ok(raw.to_string())
}

/// Same rejection rules as `sanitize_path_component_str`, but over an
/// `OsStr` so a non-UTF-8 filename is validated (and preserved) without
/// ever being lossily converted to `str` first.
fn sanitize_path_component_os(raw: &OsStr) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(ArchiveFsError::Config(
            "a Library View path component cannot be empty".to_string(),
        ));
    }
    let as_path = Path::new(raw);
    let mut components = as_path.components();
    let is_single_normal_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !is_single_normal_component {
        return Err(ArchiveFsError::Config(
            "a Library View filename is not a safe path component".to_string(),
        ));
    }
    Ok(PathBuf::from(raw))
}

// ---------------------------------------------------------------------
// Plan types.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryViewPlanAction {
    Create,
    AlreadyCorrect,
    Repair,
    RemoveStale,
    Collision,
    SkipUnknownPlatform,
    SkipMissingSourceArchive,
    SkipInvalidPath,
}

impl LibraryViewPlanAction {
    /// Which of the GUI's six summary buckets (Create / Correct / Repair /
    /// Remove / Collision / Skip) this action counts toward - the three
    /// `Skip*` reasons are distinct in the entry table but collapse into
    /// one "Skip" total, matching the milestone's exact summary spec.
    fn count_bucket(self) -> LibraryViewCountBucket {
        match self {
            Self::Create => LibraryViewCountBucket::Create,
            Self::AlreadyCorrect => LibraryViewCountBucket::Correct,
            Self::Repair => LibraryViewCountBucket::Repair,
            Self::RemoveStale => LibraryViewCountBucket::Remove,
            Self::Collision => LibraryViewCountBucket::Collision,
            Self::SkipUnknownPlatform | Self::SkipMissingSourceArchive | Self::SkipInvalidPath => {
                LibraryViewCountBucket::Skip
            }
        }
    }
}

enum LibraryViewCountBucket {
    Create,
    Correct,
    Repair,
    Remove,
    Collision,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewPlanEntry {
    pub action: LibraryViewPlanAction,
    #[serde(with = "option_path_json")]
    pub archive_path: Option<PathBuf>,
    #[serde(with = "option_path_json")]
    pub relative_link_path: Option<PathBuf>,
    #[serde(with = "option_path_json")]
    pub destination_path: Option<PathBuf>,
    pub platform: Option<String>,
    pub reason: Option<String>,
    /// For `Collision` only: the *other* archive path that would produce
    /// the same destination, if the collision is between two archives
    /// (rather than an existing unrelated file/symlink).
    #[serde(with = "option_path_json")]
    pub colliding_with: Option<PathBuf>,
    /// Populated only for `Create`/`AlreadyCorrect`/`Repair` - what
    /// `apply_library_view` writes into the manifest entry, computed once
    /// here rather than re-derived during apply.
    #[serde(with = "option_path_json")]
    pub source_folder_path: Option<PathBuf>,
    pub archive_identity: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewPlanCounts {
    pub create: usize,
    pub correct: usize,
    pub repair: usize,
    pub remove: usize,
    pub collision: usize,
    pub skip: usize,
}

impl LibraryViewPlanCounts {
    fn add(&mut self, bucket: LibraryViewCountBucket) {
        match bucket {
            LibraryViewCountBucket::Create => self.create += 1,
            LibraryViewCountBucket::Correct => self.correct += 1,
            LibraryViewCountBucket::Repair => self.repair += 1,
            LibraryViewCountBucket::Remove => self.remove += 1,
            LibraryViewCountBucket::Collision => self.collision += 1,
            LibraryViewCountBucket::Skip => self.skip += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewPlan {
    pub view_id: String,
    #[serde(with = "path_json")]
    pub destination_root: PathBuf,
    pub counts: LibraryViewPlanCounts,
    pub entries: Vec<LibraryViewPlanEntry>,
    /// `Some` when the destination root itself is unsafe (inside a source
    /// folder, or contains one, or cannot be resolved at all) - the GUI/
    /// CLI must refuse Apply while this is set, regardless of how clean
    /// the individual entries look (milestone requirement: "no Apply
    /// button until planning succeeds without unsafe-root errors").
    pub unsafe_root_error: Option<String>,
}

impl LibraryViewPlan {
    pub fn is_safe_to_apply(&self) -> bool {
        self.unsafe_root_error.is_none()
    }
}

// ---------------------------------------------------------------------
// Planning (dry-run - no filesystem mutation).
// ---------------------------------------------------------------------

struct LibraryViewCandidate<'a> {
    archive_path: &'a Path,
    platform: String,
    relative_link_path: PathBuf,
    destination_path: PathBuf,
    source_folder_path: PathBuf,
    archive_identity: Option<String>,
}

/// Produces a full `LibraryViewPlan` for `view` against the current
/// catalogue (`records`/`source_folders`) and the view's last-applied
/// `manifest` - performs no filesystem mutation, only reads
/// (`fs::symlink_metadata`/`fs::read_link`) to classify what already
/// exists at each planned destination. Safe to call as often as needed
/// (a "Preview" button, or before every Apply/Repair) with no side effect.
///
/// Platform filtering: an archive with no catalogue platform (`None`) is
/// always reported as `SkipUnknownPlatform`, regardless of `view.platforms`,
/// since Library Views never guesses a platform on the catalogue's behalf.
/// When `view.platforms` is non-empty, an archive whose platform is not in
/// that list is simply excluded from the plan entirely (an ordinary
/// filter, not a reportable skip) - the same distinction `view.source_folders`
/// draws for included-vs-excluded sources.
pub fn plan_library_view(
    view: &LibraryViewConfig,
    records: &[PersistedArchive],
    source_folders: &[SourceFolderRecord],
    manifest: &LibraryViewManifest,
) -> LibraryViewPlan {
    let mut counts = LibraryViewPlanCounts::default();
    let mut entries = Vec::new();

    let all_source_paths: Vec<PathBuf> = source_folders.iter().map(|s| s.path.clone()).collect();
    let unsafe_root_error =
        match validate_library_view_destination(&view.destination_root, &all_source_paths) {
            Ok(_) => None,
            Err(error) => Some(error.to_string()),
        };

    let source_by_id: HashMap<i64, &SourceFolderRecord> = source_folders
        .iter()
        .map(|source| (source.id, source))
        .collect();
    let included_sources: Option<HashSet<&Path>> = if view.source_folders.is_empty() {
        None
    } else {
        Some(view.source_folders.iter().map(PathBuf::as_path).collect())
    };
    let included_platforms: Option<HashSet<&str>> = if view.platforms.is_empty() {
        None
    } else {
        Some(view.platforms.iter().map(String::as_str).collect())
    };

    // Pass 1: for every catalogue-included archive, either report why it
    // is skipped or compute the one destination path it wants.
    let mut wanted: HashMap<PathBuf, Vec<LibraryViewCandidate<'_>>> = HashMap::new();
    for record in records {
        let Some(source) = source_by_id.get(&record.source_folder_id) else {
            continue;
        };
        if let Some(included) = &included_sources
            && !included.contains(source.path.as_path())
        {
            continue;
        }
        let Some(platform) = record.platform.clone() else {
            entries.push(LibraryViewPlanEntry {
                action: LibraryViewPlanAction::SkipUnknownPlatform,
                archive_path: Some(record.absolute_path.clone()),
                relative_link_path: None,
                destination_path: None,
                platform: None,
                reason: Some("archive has no assigned platform".to_string()),
                colliding_with: None,
                source_folder_path: None,
                archive_identity: None,
            });
            counts.add(LibraryViewPlanAction::SkipUnknownPlatform.count_bucket());
            continue;
        };
        if let Some(included) = &included_platforms
            && !included.contains(platform.as_str())
        {
            continue;
        }
        if record.last_verified_missing_at.is_some() {
            entries.push(LibraryViewPlanEntry {
                action: LibraryViewPlanAction::SkipMissingSourceArchive,
                archive_path: Some(record.absolute_path.clone()),
                relative_link_path: None,
                destination_path: None,
                platform: Some(platform),
                reason: Some(
                    "the catalogue's last successful scan reported this archive missing"
                        .to_string(),
                ),
                colliding_with: None,
                source_folder_path: None,
                archive_identity: None,
            });
            counts.add(LibraryViewPlanAction::SkipMissingSourceArchive.count_bucket());
            continue;
        }

        let relative_link_path = match generate_relative_link_path(
            view.layout_template,
            &platform,
            &record.absolute_path,
        ) {
            Ok(path) => path,
            Err(error) => {
                entries.push(LibraryViewPlanEntry {
                    action: LibraryViewPlanAction::SkipInvalidPath,
                    archive_path: Some(record.absolute_path.clone()),
                    relative_link_path: None,
                    destination_path: None,
                    platform: Some(platform),
                    reason: Some(error.to_string()),
                    colliding_with: None,
                    source_folder_path: None,
                    archive_identity: None,
                });
                counts.add(LibraryViewPlanAction::SkipInvalidPath.count_bucket());
                continue;
            }
        };
        let destination_path = view.destination_root.join(&relative_link_path);
        let archive_identity = record
            .size_bytes
            .map(|size| format!("{size}:{}", record.modified_time_unix_seconds.unwrap_or(0)));

        wanted
            .entry(destination_path.clone())
            .or_default()
            .push(LibraryViewCandidate {
                archive_path: &record.absolute_path,
                platform,
                relative_link_path,
                destination_path,
                source_folder_path: source.path.clone(),
                archive_identity,
            });
    }

    // Pass 2: classify each destination - a collision if more than one
    // archive wants it, otherwise Create/AlreadyCorrect/Repair against
    // whatever is actually on disk right now.
    let mut still_wanted_relative_paths: HashSet<PathBuf> = HashSet::new();
    for (_destination, mut candidates) in wanted {
        if candidates.len() > 1 {
            candidates.sort_by(|a, b| a.archive_path.cmp(b.archive_path));
            for index in 0..candidates.len() {
                let other = if index == 0 { 1 } else { 0 };
                entries.push(LibraryViewPlanEntry {
                    action: LibraryViewPlanAction::Collision,
                    archive_path: Some(candidates[index].archive_path.to_path_buf()),
                    relative_link_path: Some(candidates[index].relative_link_path.clone()),
                    destination_path: Some(candidates[index].destination_path.clone()),
                    platform: Some(candidates[index].platform.clone()),
                    reason: Some(
                        "another archive already maps to this exact destination path".to_string(),
                    ),
                    colliding_with: Some(candidates[other].archive_path.to_path_buf()),
                    source_folder_path: None,
                    archive_identity: None,
                });
                counts.add(LibraryViewPlanAction::Collision.count_bucket());
            }
            continue;
        }
        let candidate = candidates.into_iter().next().expect("non-empty group");
        still_wanted_relative_paths.insert(candidate.relative_link_path.clone());

        let (action, reason) = classify_existing_path(
            &candidate.destination_path,
            candidate.archive_path,
            &candidate.relative_link_path,
            manifest,
        );
        counts.add(action.count_bucket());
        entries.push(LibraryViewPlanEntry {
            action,
            archive_path: Some(candidate.archive_path.to_path_buf()),
            relative_link_path: Some(candidate.relative_link_path),
            destination_path: Some(candidate.destination_path),
            platform: Some(candidate.platform),
            reason,
            colliding_with: None,
            source_folder_path: Some(candidate.source_folder_path),
            archive_identity: candidate.archive_identity,
        });
    }

    // Pass 3: any manifest entry no longer wanted is stale and must be
    // reported for removal - never silently dropped.
    for manifest_entry in &manifest.entries {
        if still_wanted_relative_paths.contains(&manifest_entry.relative_link_path) {
            continue;
        }
        entries.push(LibraryViewPlanEntry {
            action: LibraryViewPlanAction::RemoveStale,
            archive_path: None,
            relative_link_path: Some(manifest_entry.relative_link_path.clone()),
            destination_path: Some(
                view.destination_root
                    .join(&manifest_entry.relative_link_path),
            ),
            platform: Some(manifest_entry.platform.clone()),
            reason: Some(
                "no longer produced by the current catalogue/filters - was previously managed"
                    .to_string(),
            ),
            colliding_with: None,
            source_folder_path: Some(manifest_entry.source_folder_path.clone()),
            archive_identity: manifest_entry.archive_identity.clone(),
        });
        counts.add(LibraryViewPlanAction::RemoveStale.count_bucket());
    }

    LibraryViewPlan {
        view_id: view.id.clone(),
        destination_root: view.destination_root.clone(),
        counts,
        entries,
        unsafe_root_error,
    }
}

/// Classifies what already exists at `destination_path` against what the
/// candidate wants there:
/// - nothing there yet -> `Create`.
/// - a real file (or directory) -> `Collision` (never overwritten).
/// - a symlink already pointing at `expected_target` -> `AlreadyCorrect`
///   (adopted/preserved, never re-created).
/// - a symlink pointing elsewhere, recorded in `manifest` as ours ->
///   `Repair`.
/// - a symlink pointing elsewhere, *not* recorded in `manifest` -> a
///   `Collision` (an unrelated symlink is never overwritten either).
fn classify_existing_path(
    destination_path: &Path,
    expected_target: &Path,
    relative_link_path: &Path,
    manifest: &LibraryViewManifest,
) -> (LibraryViewPlanAction, Option<String>) {
    let owned_by_manifest = manifest
        .entries
        .iter()
        .any(|entry| entry.relative_link_path == relative_link_path);

    match fs::symlink_metadata(destination_path) {
        Err(_) => (LibraryViewPlanAction::Create, None),
        Ok(metadata) if !metadata.file_type().is_symlink() => (
            LibraryViewPlanAction::Collision,
            Some("a real file or directory already exists at this destination".to_string()),
        ),
        Ok(_) => match fs::read_link(destination_path) {
            Ok(actual_target) if actual_target == expected_target => {
                (LibraryViewPlanAction::AlreadyCorrect, None)
            }
            Ok(_) if owned_by_manifest => (LibraryViewPlanAction::Repair, None),
            Ok(_) => (
                LibraryViewPlanAction::Collision,
                Some(
                    "an existing symlink at this path is not managed by this view and points \
                     elsewhere"
                        .to_string(),
                ),
            ),
            Err(_) if owned_by_manifest => (LibraryViewPlanAction::Repair, None),
            Err(_) => (
                LibraryViewPlanAction::Collision,
                Some("an existing symlink at this path could not be read".to_string()),
            ),
        },
    }
}

// ---------------------------------------------------------------------
// Apply / repair / remove - the only functions in this module that ever
// touch the filesystem beyond a read.
// ---------------------------------------------------------------------

/// What actually happened to one plan entry during `apply_library_view` or
/// `remove_library_view_symlinks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryViewApplyOutcome {
    Created,
    AlreadyCorrect,
    Repaired,
    Removed,
    /// A stale managed symlink was *supposed* to be removed, but the path
    /// no longer matches what the manifest recorded (already gone, replaced
    /// by a real file, or repointed by something else since planning) - so
    /// nothing was touched. Not an error: this is the safety model working
    /// as intended ("never remove anything ArchiveFS did not record as
    /// managed", re-checked at the moment of removal, not just at plan
    /// time).
    LeftUnchanged,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewApplyEntryResult {
    pub relative_link_path: PathBuf,
    pub outcome: LibraryViewApplyOutcome,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewApplyReport {
    pub view_id: String,
    pub created: usize,
    pub repaired: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub results: Vec<LibraryViewApplyEntryResult>,
}

/// Applies a previously computed `plan` for `view`: creates/repairs managed
/// symlinks, removes stale managed symlinks, and writes the updated
/// manifest atomically.
///
/// Refuses outright - before touching the filesystem or the manifest at
/// all - if `plan.is_safe_to_apply()` is false, so a failed apply always
/// leaves the previous manifest file completely untouched (it is never
/// even opened for writing in that case). A per-entry failure (a single
/// symlink creation erroring) is recorded in the returned report instead of
/// aborting the whole apply; the new manifest reflects whatever *did*
/// succeed, since leaving a successfully-created symlink unmanaged would be
/// worse than recording it.
///
/// `Collision`/`Skip*` entries are informational only and are never acted
/// on here - resolving them means changing the view's configuration
/// (source/platform filters, or a future disambiguation policy) and
/// re-planning, not something Apply does implicitly.
pub fn apply_library_view(
    view: &LibraryViewConfig,
    plan: &LibraryViewPlan,
    manifest: &LibraryViewManifest,
    data_dir: &Path,
) -> Result<LibraryViewApplyReport> {
    if !plan.is_safe_to_apply() {
        return Err(ArchiveFsError::Config(
            plan.unsafe_root_error.clone().unwrap_or_else(|| {
                "this library view's destination is unsafe to apply".to_string()
            }),
        ));
    }
    fs::create_dir_all(&view.destination_root)
        .map_err(|source| ArchiveFsError::io(view.destination_root.clone(), source))?;

    let mut entries_by_path: HashMap<PathBuf, LibraryViewManifestEntry> = manifest
        .entries
        .iter()
        .cloned()
        .map(|entry| (entry.relative_link_path.clone(), entry))
        .collect();

    let mut report = LibraryViewApplyReport {
        view_id: view.id.clone(),
        created: 0,
        repaired: 0,
        removed: 0,
        unchanged: 0,
        failed: 0,
        results: Vec::new(),
    };
    let now = now_utc_string();

    for entry in &plan.entries {
        match entry.action {
            LibraryViewPlanAction::AlreadyCorrect => {
                let (Some(relative_link_path), Some(target_path)) =
                    (entry.relative_link_path.clone(), entry.archive_path.clone())
                else {
                    continue;
                };
                let created_at = entries_by_path
                    .get(&relative_link_path)
                    .map(|existing| existing.created_at.clone())
                    .unwrap_or_else(|| now.clone());
                entries_by_path.insert(
                    relative_link_path.clone(),
                    LibraryViewManifestEntry {
                        relative_link_path: relative_link_path.clone(),
                        target_path,
                        archive_identity: entry.archive_identity.clone(),
                        created_at,
                        updated_at: now.clone(),
                        platform: entry.platform.clone().unwrap_or_default(),
                        source_folder_path: entry.source_folder_path.clone().unwrap_or_default(),
                    },
                );
                report.unchanged += 1;
                report.results.push(LibraryViewApplyEntryResult {
                    relative_link_path,
                    outcome: LibraryViewApplyOutcome::AlreadyCorrect,
                    error: None,
                });
            }
            LibraryViewPlanAction::Create | LibraryViewPlanAction::Repair => {
                let (Some(relative_link_path), Some(destination_path), Some(archive_path)) = (
                    entry.relative_link_path.clone(),
                    entry.destination_path.clone(),
                    entry.archive_path.clone(),
                ) else {
                    continue;
                };
                let is_repair = entry.action == LibraryViewPlanAction::Repair;
                match create_or_repair_symlink(&destination_path, &archive_path) {
                    Ok(()) => {
                        let created_at = if is_repair {
                            entries_by_path
                                .get(&relative_link_path)
                                .map(|existing| existing.created_at.clone())
                                .unwrap_or_else(|| now.clone())
                        } else {
                            now.clone()
                        };
                        entries_by_path.insert(
                            relative_link_path.clone(),
                            LibraryViewManifestEntry {
                                relative_link_path: relative_link_path.clone(),
                                target_path: archive_path,
                                archive_identity: entry.archive_identity.clone(),
                                created_at,
                                updated_at: now.clone(),
                                platform: entry.platform.clone().unwrap_or_default(),
                                source_folder_path: entry
                                    .source_folder_path
                                    .clone()
                                    .unwrap_or_default(),
                            },
                        );
                        if is_repair {
                            report.repaired += 1;
                        } else {
                            report.created += 1;
                        }
                        report.results.push(LibraryViewApplyEntryResult {
                            relative_link_path,
                            outcome: if is_repair {
                                LibraryViewApplyOutcome::Repaired
                            } else {
                                LibraryViewApplyOutcome::Created
                            },
                            error: None,
                        });
                    }
                    Err(error) => {
                        report.failed += 1;
                        report.results.push(LibraryViewApplyEntryResult {
                            relative_link_path,
                            outcome: LibraryViewApplyOutcome::Failed,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            LibraryViewPlanAction::RemoveStale => {
                let (Some(relative_link_path), Some(destination_path)) = (
                    entry.relative_link_path.clone(),
                    entry.destination_path.clone(),
                ) else {
                    continue;
                };
                let Some(recorded) = manifest
                    .entries
                    .iter()
                    .find(|existing| existing.relative_link_path == relative_link_path)
                else {
                    continue;
                };
                match remove_managed_symlink(&destination_path, &recorded.target_path) {
                    Ok(true) => {
                        entries_by_path.remove(&relative_link_path);
                        report.removed += 1;
                        report.results.push(LibraryViewApplyEntryResult {
                            relative_link_path,
                            outcome: LibraryViewApplyOutcome::Removed,
                            error: None,
                        });
                    }
                    Ok(false) => {
                        report.results.push(LibraryViewApplyEntryResult {
                            relative_link_path,
                            outcome: LibraryViewApplyOutcome::LeftUnchanged,
                            error: Some(
                                "left untouched - this path no longer matches the symlink \
                                 recorded in the manifest"
                                    .to_string(),
                            ),
                        });
                    }
                    Err(error) => {
                        report.failed += 1;
                        report.results.push(LibraryViewApplyEntryResult {
                            relative_link_path,
                            outcome: LibraryViewApplyOutcome::Failed,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            LibraryViewPlanAction::Collision
            | LibraryViewPlanAction::SkipUnknownPlatform
            | LibraryViewPlanAction::SkipMissingSourceArchive
            | LibraryViewPlanAction::SkipInvalidPath => {
                // Informational only - Apply never acts on these.
            }
        }
    }

    let new_manifest = LibraryViewManifest {
        view_id: view.id.clone(),
        destination_root: view.destination_root.clone(),
        entries: entries_by_path.into_values().collect(),
    };
    save_library_view_manifest_at(data_dir, &new_manifest)?;
    maybe_remove_empty_managed_directories(&view.destination_root, &new_manifest);

    Ok(report)
}

/// Repairs `view`: identical to `apply_library_view`. Re-running the full
/// plan against the current catalogue and filesystem state already fixes
/// drift (`Repair` entries) as well as creating anything newly missing, so
/// "Repair" is not a narrower operation than "Apply" here - keeping them as
/// one code path means the two can never silently diverge.
pub fn repair_library_view(
    view: &LibraryViewConfig,
    plan: &LibraryViewPlan,
    manifest: &LibraryViewManifest,
    data_dir: &Path,
) -> Result<LibraryViewApplyReport> {
    apply_library_view(view, plan, manifest, data_dir)
}

/// Removes every symlink recorded in `manifest` for `view` (verify-then-
/// remove, the same safety check `remove_managed_symlink` applies during a
/// normal apply), and writes back a manifest containing only the entries
/// that could *not* be safely removed - never forced to empty, so a
/// partially-completed removal (one entry changed underneath us) stays
/// visible on the next Preview rather than being silently forgotten.
///
/// Never touches `view`'s own definition (the config list) - the caller
/// decides separately whether to also drop it (CLI: `--keep-definition`;
/// GUI: keeps the definition by default, per the milestone's Remove View
/// requirement).
pub fn remove_library_view_symlinks(
    view: &LibraryViewConfig,
    manifest: &LibraryViewManifest,
    data_dir: &Path,
) -> Result<LibraryViewApplyReport> {
    let mut report = LibraryViewApplyReport {
        view_id: view.id.clone(),
        created: 0,
        repaired: 0,
        removed: 0,
        unchanged: 0,
        failed: 0,
        results: Vec::new(),
    };
    let mut remaining: Vec<LibraryViewManifestEntry> = Vec::new();

    for entry in &manifest.entries {
        let destination = view.destination_root.join(&entry.relative_link_path);
        match remove_managed_symlink(&destination, &entry.target_path) {
            Ok(true) => {
                report.removed += 1;
                report.results.push(LibraryViewApplyEntryResult {
                    relative_link_path: entry.relative_link_path.clone(),
                    outcome: LibraryViewApplyOutcome::Removed,
                    error: None,
                });
            }
            Ok(false) => {
                remaining.push(entry.clone());
                report.results.push(LibraryViewApplyEntryResult {
                    relative_link_path: entry.relative_link_path.clone(),
                    outcome: LibraryViewApplyOutcome::LeftUnchanged,
                    error: Some(
                        "left untouched - this path no longer matches the symlink recorded in \
                         the manifest"
                            .to_string(),
                    ),
                });
            }
            Err(error) => {
                remaining.push(entry.clone());
                report.failed += 1;
                report.results.push(LibraryViewApplyEntryResult {
                    relative_link_path: entry.relative_link_path.clone(),
                    outcome: LibraryViewApplyOutcome::Failed,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let new_manifest = LibraryViewManifest {
        view_id: view.id.clone(),
        destination_root: view.destination_root.clone(),
        entries: remaining,
    };
    save_library_view_manifest_at(data_dir, &new_manifest)?;
    maybe_remove_empty_managed_directories(&view.destination_root, &new_manifest);

    Ok(report)
}

// ---------------------------------------------------------------------
// Default-wired orchestration: the single implementation shared by the
// CLI's `view` subcommands and the GUI's Library Views page, so planning/
// apply logic is never duplicated between the two (milestone requirement).
// ---------------------------------------------------------------------

/// Resolves `identifier` against `views` - an exact `id` match first, else
/// an exact `name` match. Mirrors `resolve_source_folder_identifier`'s "id
/// first, then a direct match" shape; unlike a source folder's path,
/// though, a view's `name` is not guaranteed unique, so an identifier that
/// matches more than one view by name is rejected rather than silently
/// picking one.
pub fn resolve_library_view_identifier(
    identifier: &str,
    views: &[LibraryViewConfig],
) -> Result<LibraryViewConfig> {
    if let Some(view) = views.iter().find(|view| view.id == identifier) {
        return Ok(view.clone());
    }
    let matches: Vec<&LibraryViewConfig> = views
        .iter()
        .filter(|view| view.name == identifier)
        .collect();
    match matches.as_slice() {
        [] => Err(ArchiveFsError::Config(format!(
            "no library view matches '{identifier}'"
        ))),
        [only] => Ok((*only).clone()),
        _ => Err(ArchiveFsError::Config(format!(
            "'{identifier}' matches more than one library view by name - use its id instead"
        ))),
    }
}

/// Loads the catalogue (every `PersistedArchive` row, joined with its
/// current platform, plus every `SourceFolderRecord`) needed to plan any
/// view, from the default database path - mirrors the CLI `health`
/// command's `default_database_path` + `Database::open_read_only` +
/// `load_archives`/`list_source_folders` shape.
fn load_catalogue_for_planning() -> Result<(Vec<PersistedArchive>, Vec<SourceFolderRecord>)> {
    load_catalogue_for_planning_at(&default_database_path()?)
}

fn load_catalogue_for_planning_at(
    database_path: &Path,
) -> Result<(Vec<PersistedArchive>, Vec<SourceFolderRecord>)> {
    let database = Database::open_read_only(database_path)?;
    let archives = database.load_archives()?;
    let source_folders = database.list_source_folders()?;
    Ok((archives, source_folders))
}

/// Builds a fresh `LibraryViewPlan` for the view identified by
/// `identifier` against the current catalogue and the view's
/// last-applied manifest - performs no filesystem mutation. The single
/// "Preview" implementation shared by the CLI's `view preview` and the
/// GUI's Library Views page.
pub fn preview_library_view_default(
    identifier: &str,
) -> Result<(LibraryViewConfig, LibraryViewPlan)> {
    let views = load_library_view_configs_default()?;
    let view = resolve_library_view_identifier(identifier, &views)?;
    let (archives, source_folders) = load_catalogue_for_planning()?;
    let manifest = load_library_view_manifest_default(&view.id)?;
    let plan = plan_library_view(&view, &archives, &source_folders, &manifest);
    Ok((view, plan))
}

/// Plans and applies the view identified by `identifier` in one step - the
/// shared implementation behind the CLI's `view apply` and the GUI's
/// Apply button.
pub fn apply_library_view_default(
    identifier: &str,
) -> Result<(LibraryViewConfig, LibraryViewApplyReport)> {
    let (view, plan) = preview_library_view_default(identifier)?;
    let manifest = load_library_view_manifest_default(&view.id)?;
    let data_dir = default_library_views_data_dir()?;
    let report = apply_library_view(&view, &plan, &manifest, &data_dir)?;
    Ok((view, report))
}

/// Plans and repairs the view identified by `identifier` - identical to
/// `apply_library_view_default` (see `repair_library_view`'s own doc
/// comment for why Repair is not a narrower operation than Apply here).
pub fn repair_library_view_default(
    identifier: &str,
) -> Result<(LibraryViewConfig, LibraryViewApplyReport)> {
    apply_library_view_default(identifier)
}

/// Removes every managed symlink for the view identified by `identifier`,
/// and - unless `keep_definition` is set - also drops the view's own
/// definition from the configured list. The definition is only removed
/// after the symlink removal has been written, so a failure removing
/// symlinks never also loses the view's configuration. Never deletes
/// original archive files - only the managed symlinks
/// `remove_library_view_symlinks` itself is already restricted to.
pub fn remove_library_view_default(
    identifier: &str,
    keep_definition: bool,
) -> Result<(LibraryViewConfig, LibraryViewApplyReport)> {
    let mut views = load_library_view_configs_default()?;
    let view = resolve_library_view_identifier(identifier, &views)?;
    let manifest = load_library_view_manifest_default(&view.id)?;
    let data_dir = default_library_views_data_dir()?;
    let report = remove_library_view_symlinks(&view, &manifest, &data_dir)?;

    if !keep_definition {
        views.retain(|candidate| candidate.id != view.id);
        save_library_view_configs_default(&views)?;
    }

    Ok((view, report))
}

/// Creates a new Library View: validates `destination_root` against every
/// currently configured source folder (never inside one, and containing
/// none of them - `validate_library_view_destination`), generates a fresh
/// stable id, appends it to the configured list, and saves atomically.
/// Returns the created view.
pub fn add_library_view_default(
    name: String,
    destination_root: PathBuf,
    source_folders: Vec<PathBuf>,
    platforms: Vec<String>,
    layout_template: LibraryViewLayoutTemplate,
) -> Result<LibraryViewConfig> {
    let (_, all_source_folders) = load_catalogue_for_planning()?;
    let all_source_paths: Vec<PathBuf> = all_source_folders
        .iter()
        .map(|source| source.path.clone())
        .collect();
    let destination_root = validate_library_view_destination(&destination_root, &all_source_paths)?;

    let view = LibraryViewConfig {
        id: generate_library_view_id(),
        name,
        destination_root,
        enabled: true,
        source_folders,
        platforms,
        layout_template,
    };

    let mut views = load_library_view_configs_default()?;
    views.push(view.clone());
    save_library_view_configs_default(&views)?;
    Ok(view)
}

/// Loads the configured list, applies `mutate` to the view identified by
/// `identifier`, and saves the result back atomically - the same "load the
/// full list, mutate one entry in memory, save back" shape
/// `SourceFolderConfig`'s enable/disable already uses.
fn update_library_view_default(
    identifier: &str,
    mutate: impl FnOnce(&mut LibraryViewConfig),
) -> Result<LibraryViewConfig> {
    let mut views = load_library_view_configs_default()?;
    let resolved = resolve_library_view_identifier(identifier, &views)?;
    let Some(existing) = views
        .iter_mut()
        .find(|candidate| candidate.id == resolved.id)
    else {
        return Err(ArchiveFsError::Config(format!(
            "no library view matches '{identifier}'"
        )));
    };
    mutate(existing);
    let updated = existing.clone();
    save_library_view_configs_default(&views)?;
    Ok(updated)
}

/// Enables or disables the view identified by `identifier` without
/// touching its manifest or any symlink - a disabled view is simply never
/// offered for Preview/Apply/Repair by the GUI/CLI going forward; existing
/// managed symlinks are left exactly as they are until an explicit Remove.
pub fn set_library_view_enabled_default(
    identifier: &str,
    enabled: bool,
) -> Result<LibraryViewConfig> {
    update_library_view_default(identifier, |view| view.enabled = enabled)
}

/// Edits the name/destination/source-folder-filter/platform-filter of the
/// view identified by `identifier`. The new destination is validated
/// exactly as `add_library_view_default` validates a new one - editing a
/// view can never relax the destination-safety guarantee.
pub fn edit_library_view_default(
    identifier: &str,
    name: String,
    destination_root: PathBuf,
    source_folders: Vec<PathBuf>,
    platforms: Vec<String>,
) -> Result<LibraryViewConfig> {
    let (_, all_source_folders) = load_catalogue_for_planning()?;
    let all_source_paths: Vec<PathBuf> = all_source_folders
        .iter()
        .map(|source| source.path.clone())
        .collect();
    let destination_root = validate_library_view_destination(&destination_root, &all_source_paths)?;
    update_library_view_default(identifier, move |view| {
        view.name = name;
        view.destination_root = destination_root;
        view.source_folders = source_folders;
        view.platforms = platforms;
    })
}

/// Creates (or replaces) a managed symlink at `destination` pointing to
/// `target`, atomically: the new symlink is first created under a
/// temporary name in the same directory, then renamed into place
/// (`fs::rename` is atomic on POSIX and replaces whatever - file or
/// symlink - currently sits at `destination` in one step). Mirrors
/// `atomic_write_text`'s temp-file-then-rename shape, applied to a symlink
/// instead of a regular file.
///
/// Only ever called for `Create`/`Repair` entries, which `plan_library_view`
/// has already proven safe to write: either nothing real is at
/// `destination`, or what is there is a symlink this view already owns.
fn create_or_repair_symlink(destination: &Path, target: &Path) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        ArchiveFsError::Config(format!("{} has no parent directory", destination.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temp_path = parent.join(format!(
        ".archivefs-link-{:x}-{:x}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    // Best-effort: a leftover temp path from a previous crash must never
    // block this attempt.
    let _ = fs::remove_file(&temp_path);

    symlink(target, &temp_path).map_err(|source| ArchiveFsError::io(temp_path.clone(), source))?;
    fs::rename(&temp_path, destination).map_err(|source| {
        let _ = fs::remove_file(&temp_path);
        ArchiveFsError::io(destination.to_path_buf(), source)
    })
}

/// Removes the symlink at `destination` - but only if it is still *exactly*
/// what the manifest recorded: a symlink (never a real file or directory)
/// pointing at `recorded_target`. Returns `Ok(true)` if it was removed,
/// `Ok(false)` if `destination` no longer matches (already gone, replaced
/// by a real file, or repointed by something else) - in the `Ok(false)`
/// case nothing is touched at all, satisfying "never remove anything
/// ArchiveFS did not record as managed" even when the manifest is stale
/// relative to the filesystem.
fn remove_managed_symlink(destination: &Path, recorded_target: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ArchiveFsError::io(destination.to_path_buf(), error)),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }
    match fs::read_link(destination) {
        Ok(actual_target) if actual_target == recorded_target => {
            fs::remove_file(destination)
                .map_err(|source| ArchiveFsError::io(destination.to_path_buf(), source))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Best-effort cleanup: after removing managed symlinks, removes any
/// now-empty directory ArchiveFS created under `destination_root` - never
/// `destination_root` itself (milestone requirement: "never treat the
/// destination directory itself as removable"), and never anything outside
/// it. `fs::remove_dir` on a non-empty directory simply fails and is
/// ignored here - this never forces a removal.
fn maybe_remove_empty_managed_directories(destination_root: &Path, manifest: &LibraryViewManifest) {
    let mut candidate_dirs: HashSet<PathBuf> = HashSet::new();
    for entry in &manifest.entries {
        let mut current = entry.relative_link_path.parent();
        while let Some(relative_dir) = current {
            if relative_dir.as_os_str().is_empty() {
                break;
            }
            candidate_dirs.insert(destination_root.join(relative_dir));
            current = relative_dir.parent();
        }
    }
    // Also sweep one level deep under `destination_root` directly, so a
    // directory left with zero remaining manifest entries (everything
    // under it just got removed) is still considered even though the loop
    // above no longer has any entry to derive it from.
    if let Ok(read_dir) = fs::read_dir(destination_root) {
        for dir_entry in read_dir.flatten() {
            if dir_entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
            {
                candidate_dirs.insert(dir_entry.path());
            }
        }
    }

    let mut ordered: Vec<PathBuf> = candidate_dirs.into_iter().collect();
    // Deepest first, so a now-empty parent is only attempted after its
    // (already-removed) child.
    ordered.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in ordered {
        if dir == destination_root || !dir.starts_with(destination_root) {
            continue;
        }
        let _ = fs::remove_dir(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "archivefs-core-library-views-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn catalogue_planning_load_is_strictly_read_only() {
        let root = temp_dir("catalogue-planning-read-only");
        let database_path = root.join("library.sqlite3");
        Database::open_or_create(&database_path)
            .unwrap()
            .close()
            .unwrap();
        let before = fs::read(&database_path).unwrap();
        let before_modified = fs::metadata(&database_path).unwrap().modified().unwrap();

        let (archives, sources) = load_catalogue_for_planning_at(&database_path).unwrap();

        assert!(archives.is_empty());
        assert!(sources.is_empty());
        assert_eq!(fs::read(&database_path).unwrap(), before);
        assert_eq!(
            fs::metadata(&database_path).unwrap().modified().unwrap(),
            before_modified
        );
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut sidecar = database_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            assert!(!PathBuf::from(sidecar).exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    fn make_source(id: i64, path: &Path) -> SourceFolderRecord {
        SourceFolderRecord {
            id,
            path: path.to_path_buf(),
            first_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_scan_status: None,
            last_scan_error: None,
            last_scan_at: None,
            last_successful_scan_at: None,
            last_archive_count: None,
        }
    }

    fn make_archive(
        id: i64,
        source_folder_id: i64,
        absolute_path: &Path,
        platform: Option<&str>,
    ) -> PersistedArchive {
        let file_name = absolute_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        PersistedArchive {
            id,
            source_folder_id,
            relative_path: PathBuf::from(&file_name),
            absolute_path: absolute_path.to_path_buf(),
            archive_kind: "zip".to_string(),
            display_name: file_name.clone(),
            normalized_name: file_name.to_lowercase(),
            size_bytes: Some(1234),
            modified_time_unix_seconds: Some(1_700_000_000),
            platform: platform.map(|p| p.to_string()),
            platform_source: platform.map(|_| "heuristic-path-detector".to_string()),
            last_known_health: "ok".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_verified_missing_at: None,
        }
    }

    fn make_view(
        id: &str,
        destination_root: &Path,
        source_folders: Vec<PathBuf>,
        platforms: Vec<String>,
    ) -> LibraryViewConfig {
        LibraryViewConfig {
            id: id.to_string(),
            name: id.to_string(),
            destination_root: destination_root.to_path_buf(),
            enabled: true,
            source_folders,
            platforms,
            layout_template: LibraryViewLayoutTemplate::PlatformFilename,
        }
    }

    fn empty_manifest(view_id: &str, destination_root: &Path) -> LibraryViewManifest {
        LibraryViewManifest::empty(view_id, destination_root)
    }

    #[test]
    fn plan_does_not_touch_filesystem() {
        let root = temp_dir("no-mutation");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let _plan = plan_library_view(&view, &[archive], &[source], &manifest);

        assert!(
            !destination.exists(),
            "preview must never create the destination root"
        );
    }

    #[test]
    fn plan_single_archive_produces_create_entry() {
        let root = temp_dir("single-create");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);

        assert_eq!(plan.counts.create, 1);
        assert_eq!(plan.entries.len(), 1);
        let entry = &plan.entries[0];
        assert_eq!(entry.action, LibraryViewPlanAction::Create);
        assert_eq!(
            entry.relative_link_path.as_deref(),
            Some(Path::new("NES/Game.zip"))
        );
        assert_eq!(
            entry.destination_path.as_deref(),
            Some(destination.join("NES/Game.zip").as_path())
        );
    }

    #[test]
    fn apply_creates_symlink_with_correct_target() {
        let root = temp_dir("apply-create");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();

        assert_eq!(report.created, 1);
        let link_path = destination.join("NES/Game.zip");
        let target = fs::read_link(&link_path).unwrap();
        assert_eq!(target, archive_path);
    }

    #[test]
    fn apply_twice_is_idempotent() {
        let root = temp_dir("apply-idempotent");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);

        let manifest1 = empty_manifest(&view.id, &destination);
        let plan1 = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest1,
        );
        let report1 = apply_library_view(&view, &plan1, &manifest1, &data_dir).unwrap();
        assert_eq!(report1.created, 1);

        let manifest2 = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        let plan2 = plan_library_view(&view, &[archive], &[source], &manifest2);
        assert_eq!(plan2.counts.create, 0);
        assert_eq!(plan2.counts.correct, 1);
        let report2 = apply_library_view(&view, &plan2, &manifest2, &data_dir).unwrap();
        assert_eq!(report2.created, 0);
        assert_eq!(report2.unchanged, 1);
    }

    #[test]
    fn already_correct_symlink_is_preserved_not_recreated() {
        let root = temp_dir("already-correct");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let link_path = destination.join("NES").join("Game.zip");
        fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        symlink(&archive_path, &link_path).unwrap();
        let ino_before = fs::symlink_metadata(&link_path).unwrap().ino();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        // Not previously recorded in any manifest - this symlink was not
        // created by ArchiveFS, but already points exactly where ArchiveFS
        // would put it.
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::AlreadyCorrect
        );

        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.created, 0);

        let ino_after = fs::symlink_metadata(&link_path).unwrap().ino();
        assert_eq!(
            ino_before, ino_after,
            "an already-correct symlink must never be recreated"
        );

        let new_manifest = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        assert_eq!(
            new_manifest.entries.len(),
            1,
            "an adopted correct symlink must be recorded"
        );
    }

    #[test]
    fn broken_managed_symlink_is_repaired() {
        let root = temp_dir("repair");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        let wrong_target = root.join("wrong-target.zip");
        write_file(&archive_path, b"zip-bytes");
        write_file(&wrong_target, b"other-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let link_path = destination.join("NES").join("Game.zip");
        fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        symlink(&wrong_target, &link_path).unwrap();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);

        // Recorded in the manifest as ours, pointing at the wrong target -
        // simulating drift since the last apply.
        let manifest = LibraryViewManifest {
            view_id: view.id.clone(),
            destination_root: destination.clone(),
            entries: vec![LibraryViewManifestEntry {
                relative_link_path: PathBuf::from("NES/Game.zip"),
                target_path: wrong_target.clone(),
                archive_identity: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                platform: "NES".to_string(),
                source_folder_path: source_dir.clone(),
            }],
        };

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Repair);

        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        assert_eq!(report.repaired, 1);
        let target = fs::read_link(&link_path).unwrap();
        assert_eq!(target, archive_path);
    }

    #[test]
    fn unrelated_real_file_collision_is_never_overwritten() {
        let root = temp_dir("real-file-collision");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let link_path = destination.join("NES").join("Game.zip");
        write_file(&link_path, b"unrelated real file contents");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Collision);
        assert_eq!(plan.counts.collision, 1);

        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();

        assert!(
            !fs::symlink_metadata(&link_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(&link_path).unwrap(),
            b"unrelated real file contents"
        );
    }

    #[test]
    fn unrelated_symlink_is_never_overwritten() {
        let root = temp_dir("symlink-collision");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        let elsewhere = root.join("elsewhere.zip");
        write_file(&archive_path, b"zip-bytes");
        write_file(&elsewhere, b"elsewhere-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let link_path = destination.join("NES").join("Game.zip");
        fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        symlink(&elsewhere, &link_path).unwrap();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination); // not managed by us

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Collision);

        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();

        let target = fs::read_link(&link_path).unwrap();
        assert_eq!(target, elsewhere);
    }

    #[test]
    fn two_archives_generating_the_same_destination_become_a_collision() {
        let root = temp_dir("collision-two-archives");
        let source_dir_a = root.join("source-a");
        let source_dir_b = root.join("source-b");
        let archive_a = source_dir_a.join("Game.zip");
        let archive_b = source_dir_b.join("Game.zip");
        write_file(&archive_a, b"a");
        write_file(&archive_b, b"b");
        let destination = root.join("dest");

        let source_a = make_source(1, &source_dir_a);
        let source_b = make_source(2, &source_dir_b);
        let record_a = make_archive(1, 1, &archive_a, Some("NES"));
        let record_b = make_archive(2, 2, &archive_b, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_a, record_b],
            &[source_a, source_b],
            &manifest,
        );

        assert_eq!(plan.counts.collision, 2);
        assert_eq!(plan.counts.create, 0);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action == LibraryViewPlanAction::Collision)
        );
        for entry in &plan.entries {
            assert!(entry.colliding_with.is_some());
        }
    }

    #[test]
    fn destination_inside_a_source_is_rejected() {
        let root = temp_dir("dest-inside-source");
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let destination = source_dir.join("nested-dest");

        let result =
            validate_library_view_destination(&destination, std::slice::from_ref(&source_dir));
        assert!(result.is_err());
    }

    #[test]
    fn source_inside_destination_is_rejected() {
        let root = temp_dir("source-inside-dest");
        let destination = root.join("dest");
        fs::create_dir_all(&destination).unwrap();
        let source_dir = destination.join("nested-source");
        fs::create_dir_all(&source_dir).unwrap();

        let result = validate_library_view_destination(&destination, &[source_dir]);
        assert!(result.is_err());
    }

    #[test]
    fn traversal_through_generated_names_is_rejected() {
        assert!(sanitize_path_component_str("..").is_err());
        assert!(sanitize_path_component_str(".").is_err());
        assert!(sanitize_path_component_str("").is_err());
        assert!(sanitize_path_component_str("a/b").is_err());
        assert!(sanitize_path_component_str("../../etc").is_err());
        assert!(sanitize_path_component_str("NES").is_ok());

        assert!(sanitize_path_component_os(OsStr::new("..")).is_err());
        assert!(sanitize_path_component_os(OsStr::new("a/b")).is_err());
        assert!(sanitize_path_component_os(OsStr::new("Game.zip")).is_ok());
    }

    #[test]
    fn cleanup_removes_only_manifest_owned_symlinks() {
        let root = temp_dir("cleanup-owned-only");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        // A symlink that is NOT recorded in any manifest, sitting right
        // next to where a managed one will go.
        let unmanaged_link = destination.join("NES").join("Other.zip");
        let unmanaged_target = root.join("unmanaged-target.zip");
        write_file(&unmanaged_target, b"unmanaged");
        fs::create_dir_all(unmanaged_link.parent().unwrap()).unwrap();
        symlink(&unmanaged_target, &unmanaged_link).unwrap();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        // First apply: creates the managed link, manifest now owns it.
        let plan = plan_library_view(&view, &[archive], std::slice::from_ref(&source), &manifest);
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        let manifest_after_first = load_library_view_manifest_at(&data_dir, &view.id).unwrap();

        // Second plan against an empty catalogue: the managed link becomes
        // stale and should be the only thing removed.
        let plan2 = plan_library_view(&view, &[], &[source], &manifest_after_first);
        assert_eq!(plan2.counts.remove, 1);
        let report2 = apply_library_view(&view, &plan2, &manifest_after_first, &data_dir).unwrap();
        assert_eq!(report2.removed, 1);

        assert!(!destination.join("NES").join("Game.zip").exists());
        assert!(
            fs::symlink_metadata(&unmanaged_link).is_ok(),
            "unmanaged symlink must be left alone"
        );
    }

    #[test]
    fn changed_or_replaced_managed_path_is_left_untouched() {
        let root = temp_dir("changed-managed-path");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let link_path = destination.join("NES").join("Game.zip");

        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = LibraryViewManifest {
            view_id: view.id.clone(),
            destination_root: destination.clone(),
            entries: vec![LibraryViewManifestEntry {
                relative_link_path: PathBuf::from("NES/Game.zip"),
                target_path: archive_path.clone(),
                archive_identity: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                platform: "NES".to_string(),
                source_folder_path: source_dir.clone(),
            }],
        };

        // The path the manifest thinks is a managed symlink has since been
        // replaced by a real file - e.g. by the user, outside ArchiveFS.
        write_file(&link_path, b"a real file now sits here");

        let removed = remove_managed_symlink(&link_path, &archive_path).unwrap();
        assert!(!removed);
        assert_eq!(fs::read(&link_path).unwrap(), b"a real file now sits here");

        let report = remove_library_view_symlinks(&view, &manifest, &data_dir).unwrap();
        assert_eq!(report.removed, 0);
        let new_manifest = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        assert_eq!(
            new_manifest.entries.len(),
            1,
            "an entry that could not be safely removed must stay recorded"
        );
    }

    #[test]
    fn original_archive_bytes_remain_unchanged() {
        let root = temp_dir("archive-bytes-unchanged");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"original-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();

        assert_eq!(fs::read(&archive_path).unwrap(), b"original-bytes");
        assert!(
            !fs::symlink_metadata(&archive_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn missing_source_archive_is_reported_not_removed() {
        let root = temp_dir("missing-source");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip"); // never actually created
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let mut archive = make_archive(1, 1, &archive_path, Some("NES"));
        archive.last_verified_missing_at = Some("2026-01-01T00:00:00Z".to_string());
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipMissingSourceArchive
        );
        assert_eq!(plan.counts.skip, 1);
    }

    #[test]
    fn unknown_platform_is_skipped_truthfully() {
        let root = temp_dir("unknown-platform");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, None);
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipUnknownPlatform
        );
        assert_eq!(plan.counts.skip, 1);
    }

    #[test]
    fn disabled_source_and_platform_filters_are_respected() {
        let root = temp_dir("filters");
        let source_dir_included = root.join("source-included");
        let source_dir_excluded = root.join("source-excluded");
        let archive_included = source_dir_included.join("Included.zip");
        let archive_excluded_by_source = source_dir_excluded.join("ExcludedSource.zip");
        let archive_excluded_by_platform = source_dir_included.join("ExcludedPlatform.zip");
        write_file(&archive_included, b"a");
        write_file(&archive_excluded_by_source, b"b");
        write_file(&archive_excluded_by_platform, b"c");
        let destination = root.join("dest");

        let source_included = make_source(1, &source_dir_included);
        let source_excluded = make_source(2, &source_dir_excluded);
        let record_included = make_archive(1, 1, &archive_included, Some("NES"));
        let record_excluded_by_source =
            make_archive(2, 2, &archive_excluded_by_source, Some("NES"));
        let record_excluded_by_platform =
            make_archive(3, 1, &archive_excluded_by_platform, Some("SNES"));

        let view = make_view(
            "view-1",
            &destination,
            vec![source_dir_included.clone()],
            vec!["NES".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[
                record_included,
                record_excluded_by_source,
                record_excluded_by_platform,
            ],
            &[source_included, source_excluded],
            &manifest,
        );

        assert_eq!(
            plan.entries.len(),
            1,
            "excluded source/platform archives are silently omitted, not reported"
        );
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(
            plan.entries[0].relative_link_path.as_deref(),
            Some(Path::new("NES/Included.zip"))
        );
    }

    #[test]
    fn non_utf8_paths_do_not_panic() {
        let root = temp_dir("non-utf8");
        let source_dir = root.join("source");
        let bytes: &[u8] = b"Invalid-\xFF\xFE-Name.zip";
        let os_str = OsStr::from_bytes(bytes);
        let archive_path = source_dir.join(os_str);
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);

        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        assert_eq!(report.created, 1);

        let link_path = plan.entries[0].destination_path.clone().unwrap();
        let target = fs::read_link(&link_path).unwrap();
        assert_eq!(target, archive_path);
    }

    #[test]
    fn manifest_writes_are_atomic_no_stray_temp_files() {
        let root = temp_dir("atomic-manifest");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();

        let leftover_temp_files: Vec<_> = fs::read_dir(&data_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftover_temp_files.is_empty(),
            "atomic writes must never leave a temp file behind"
        );
    }

    #[test]
    fn failed_apply_leaves_the_previous_manifest_intact() {
        let root = temp_dir("failed-apply-manifest-intact");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let view = make_view("view-1", &destination, vec![], vec![]);
        let existing_manifest = LibraryViewManifest {
            view_id: view.id.clone(),
            destination_root: destination.clone(),
            entries: vec![LibraryViewManifestEntry {
                relative_link_path: PathBuf::from("NES/Game.zip"),
                target_path: PathBuf::from("/somewhere/Game.zip"),
                archive_identity: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                platform: "NES".to_string(),
                source_folder_path: PathBuf::from("/somewhere"),
            }],
        };
        save_library_view_manifest_at(&data_dir, &existing_manifest).unwrap();
        let manifest_path = library_view_manifest_path(&data_dir, &view.id);
        let before = fs::read_to_string(&manifest_path).unwrap();

        let unsafe_plan = LibraryViewPlan {
            view_id: view.id.clone(),
            destination_root: destination.clone(),
            counts: LibraryViewPlanCounts::default(),
            entries: vec![],
            unsafe_root_error: Some("destination is inside a source folder".to_string()),
        };

        let result = apply_library_view(&view, &unsafe_plan, &existing_manifest, &data_dir);
        assert!(result.is_err());

        let after = fs::read_to_string(&manifest_path).unwrap();
        assert_eq!(
            before, after,
            "a rejected apply must never touch the previous manifest"
        );
    }

    #[test]
    fn remove_view_never_deletes_original_archives() {
        let root = temp_dir("remove-view-keeps-archives");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"original-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest);
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        let manifest_after = load_library_view_manifest_at(&data_dir, &view.id).unwrap();

        let report = remove_library_view_symlinks(&view, &manifest_after, &data_dir).unwrap();
        assert_eq!(report.removed, 1);

        assert!(archive_path.exists());
        assert_eq!(fs::read(&archive_path).unwrap(), b"original-bytes");
        assert!(!destination.join("NES").join("Game.zip").exists());
    }

    #[test]
    fn plan_counts_match_entry_action_totals() {
        let root = temp_dir("counts-consistency");
        let source_dir = root.join("source");
        let archive_ok = source_dir.join("Ok.zip");
        write_file(&archive_ok, b"a");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let record_ok = make_archive(1, 1, &archive_ok, Some("NES"));
        let record_unknown = make_archive(2, 1, &source_dir.join("Unknown.zip"), None);
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[record_ok, record_unknown], &[source], &manifest);

        let recomputed =
            plan.entries
                .iter()
                .fold(LibraryViewPlanCounts::default(), |mut counts, entry| {
                    match entry.action {
                        LibraryViewPlanAction::Create => counts.create += 1,
                        LibraryViewPlanAction::AlreadyCorrect => counts.correct += 1,
                        LibraryViewPlanAction::Repair => counts.repair += 1,
                        LibraryViewPlanAction::RemoveStale => counts.remove += 1,
                        LibraryViewPlanAction::Collision => counts.collision += 1,
                        LibraryViewPlanAction::SkipUnknownPlatform
                        | LibraryViewPlanAction::SkipMissingSourceArchive
                        | LibraryViewPlanAction::SkipInvalidPath => counts.skip += 1,
                    }
                    counts
                });

        assert_eq!(
            plan.counts, recomputed,
            "CLI and GUI both read plan.counts directly - it must always match the entries list exactly"
        );
    }
}
