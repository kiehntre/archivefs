//! Library Views: named, symlink-based organised folder trees pointing at
//! existing archive files - never a copy, move, rename, or modification of
//! any original archive.
//!
//! # Safety model
//!
//! - A view's destination root may never be inside a configured source
//!   folder, and no configured source folder may be inside a destination
//!   root (`validate_library_view_destination`).
//! - Every symlink EmuWiz creates is recorded in a per-view manifest
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
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// Which frontend this view is nominally aimed at, and the (currently
    /// mostly-default) policy governing its output shape. Added by the
    /// Frontend Profiles milestone - `#[serde(default)]` so every
    /// `library_views.json` written before this field existed keeps loading
    /// unchanged, always resolving to `FrontendProfile::default()` (the
    /// `Generic` kind, whose behaviour is byte-for-byte the pre-existing
    /// `PlatformFilename` behaviour). `Romm` plans real paths; `EsDe` is
    /// still vocabulary only - see `FrontendProfileKind`'s doc comment.
    #[serde(default)]
    pub profile: FrontendProfile,
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

// ---------------------------------------------------------------------
// Frontend profiles. Stage 1/2 of the Frontend Profiles milestone added the
// vocabulary; this stage makes `Romm` plan real `roms/<slug>/<filename>`
// paths (see `generate_relative_link_path`, `resolve_romm_platform_slug`) -
// still only ever a managed *symlink* to the source archive, never a copy,
// hardlink, `.m3u`, `gamelist.xml`, or RomM rescan API call. `EsDe` remains
// vocabulary only and still fails closed - no ES-DE system mapping exists
// yet; see this module's top-of-file doc comment and the milestone notes.
// This is deliberately kept on the read-only-of-master "view" axis and must
// never be confused with `RepairProfile::Romm` (in `crate::repair::library`),
// which lives on the separate, mutating master-organisation axis - there is
// no call path from anything in this module into that one.
// ---------------------------------------------------------------------

/// Which frontend a Library View is nominally shaped for. `Generic` is
/// byte-for-byte the pre-existing `PlatformFilename` behaviour. `Romm` plans
/// real `roms/<resolved-slug>/<filename>` paths once every selected
/// archive's platform resolves to a RomM slug (see
/// `resolve_romm_platform_slug`) - an archive whose platform does not
/// resolve is refused individually (`SkipInvalidPath`), never silently
/// planned under a `Generic`-shaped path. `EsDe` exists so the schema does
/// not need to churn again when its real materialization is implemented in
/// a later milestone, but selecting it today *fails closed* entirely:
/// planning an `EsDe` view produces a clear refusal
/// (`LibraryViewPlan::profile_error`, and `generate_relative_link_path`
/// itself also refuses directly), never a silent fallback to Generic's
/// `{platform}/{filename}` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FrontendProfileKind {
    /// The existing, generic `PlatformFilename` behaviour.
    #[default]
    Generic,
    /// Plans real `roms/<resolved-romm-slug>/<filename>` symlink paths - see
    /// this section's doc comment and `resolve_romm_platform_slug`. Still no
    /// RomM rescan API call, no `.m3u`/`gamelist.xml`, no copy/hardlink -
    /// only the existing managed-symlink materialization path.
    Romm,
    /// Vocabulary only - see this section's doc comment. No ES-DE system
    /// mapping, no `.m3u`/`gamelist.xml` generation exists yet -
    /// planning/applying a view with this kind fails closed rather than
    /// silently behaving like `Generic`.
    EsDe,
}

/// How a profile would eventually pick a title/region/language/variant when
/// more than one archive could represent "the same" release - deliberately
/// minimal today. Only `CanonicalArchiveFilename` (today's actual behaviour)
/// exists; a `PreferredPerTitle` policy, a variant-index database, DAT
/// re-audits, and multidisc grouping are explicitly out of scope for this
/// milestone (see the milestone notes) and are left for later variants to
/// add without needing to reshape this enum's existing case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TitleSelectionPolicy {
    /// Use the archive's own filename verbatim (after sanitisation) - the
    /// only behaviour this milestone implements.
    #[default]
    CanonicalArchiveFilename,
}

/// How a profile would eventually treat multiple variants (regions,
/// revisions, dumps) of what is otherwise the same title. Only `KeepAll`
/// (today's actual behaviour - every archive that plans to a distinct
/// destination is planned) exists in this milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VariantHandlingPolicy {
    #[default]
    KeepAll,
}

/// How a profile would eventually group multi-disc releases. Only
/// `Ungrouped` (today's actual behaviour - every archive is planned
/// independently) exists in this milestone; multidisc grouping itself is
/// explicitly out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MultidiscHandlingPolicy {
    #[default]
    Ungrouped,
}

/// The policy knobs a `FrontendProfile` carries. Every field defaults to
/// exactly what today's `PlatformFilename` behaviour already does, so
/// constructing `FrontendProfilePolicy::default()` (or loading an old
/// config, via `#[serde(default)]`) never changes existing output. Kept
/// deliberately small - this milestone adds only the shape needed to avoid
/// future schema churn, not the policies themselves (no `PreferredPerTitle`,
/// no variant-index database, no DAT re-audits, no multidisc grouping - see
/// the milestone notes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FrontendProfilePolicy {
    #[serde(default)]
    pub title_selection: TitleSelectionPolicy,
    /// Ordered most-preferred-first; empty means "no preference expressed"
    /// (today's actual behaviour - region is never consulted).
    #[serde(default)]
    pub region_preference: Vec<String>,
    /// Ordered most-preferred-first; empty means "no preference expressed".
    #[serde(default)]
    pub language_preference: Vec<String>,
    #[serde(default)]
    pub variant_handling: VariantHandlingPolicy,
    #[serde(default)]
    pub multidisc_handling: MultidiscHandlingPolicy,
    /// Overrides the catalogue platform name used for planning purposes.
    /// Empty means "no overrides" (today's actual behaviour). See
    /// `FrontendPlatformMapping`'s own doc comment for why this is a named
    /// type rather than a bare map field.
    #[serde(default)]
    pub platform_mapping_overrides: FrontendPlatformMapping,
    /// How a future materialization step would compute a managed symlink's
    /// target path. Vocabulary only in this milestone - `apply_library_view`
    /// always uses the archive's absolute host path today, regardless of
    /// this field's value; see `SymlinkTargetStrategy`'s own doc comment for
    /// why the seam exists now anyway.
    #[serde(default)]
    pub symlink_target_strategy: SymlinkTargetStrategy,
}

/// A profile's catalogue-platform-name overrides, keyed by the catalogue's
/// own platform string. A dedicated (if today minimal) type rather than a
/// bare `HashMap`/`BTreeMap` field, so RomM/ES-DE platform mapping has one
/// principled place to grow into later (e.g. per-frontend-kind mapping
/// rules, wildcard/prefix rules, a reverse lookup) without another schema
/// migration. Backed by a `BTreeMap` so serialization/hashing
/// (`compute_view_profile_fingerprint`) is always key-order-stable
/// regardless of insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FrontendPlatformMapping {
    #[serde(default)]
    overrides: BTreeMap<String, String>,
}

impl FrontendPlatformMapping {
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    pub fn get(&self, catalogue_platform: &str) -> Option<&str> {
        self.overrides.get(catalogue_platform).map(String::as_str)
    }

    /// Returns the previous override for `catalogue_platform`, if any -
    /// mirrors `BTreeMap::insert`'s own return shape.
    pub fn insert(
        &mut self,
        catalogue_platform: String,
        mapped_platform: String,
    ) -> Option<String> {
        self.overrides.insert(catalogue_platform, mapped_platform)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.overrides
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// How a managed symlink's target path would be computed. Vocabulary only
/// in this milestone (see `FrontendProfilePolicy::symlink_target_strategy`'s
/// doc comment) - added now, ahead of any real use, because RomM commonly
/// runs in a container with a different mount path than the host: a plain
/// absolute host path (`AbsoluteSourcePath`, today's only real behaviour)
/// works for Generic's own host-side symlinks, but breaks once something
/// *inside* the container tries to resolve the same symlink against its own
/// (different) view of the filesystem. A relative-to-shared-ancestor target
/// survives that kind of remount because it never encodes an absolute host
/// path at all. Adding this enum now - even though nothing but
/// `AbsoluteSourcePath` is implemented - means a later milestone can wire in
/// the Docker-safe computation without another manifest/config migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SymlinkTargetStrategy {
    /// Today's only real behaviour: the target is the archive's absolute
    /// host path, exactly as `apply_library_view` already writes it.
    #[default]
    AbsoluteSourcePath,
    /// Not implemented yet - vocabulary only. Would compute the target as a
    /// path relative to the nearest shared ancestor of the destination and
    /// the source archive, so the symlink stays valid across a container
    /// remount (e.g. RomM's Docker deployment) that gives the destination
    /// and source different absolute paths on each side of the mount.
    RelativeToSharedAncestor,
    /// Not implemented yet - vocabulary only. Explicitly requests an
    /// absolute target - identical output to `AbsoluteSourcePath` today,
    /// but named explicitly so a profile can pin this behaviour even after
    /// a different strategy becomes the default.
    Absolute,
}

/// A Library View's frontend identity: which frontend it is nominally
/// shaped for, plus the (mostly-default) policy governing its output shape.
/// `#[serde(default)]` on `LibraryViewConfig::profile` means an old config
/// with no `profile` key at all deserializes to `FrontendProfile::default()`
/// (`Generic` kind, default policy), which is defined to behave exactly
/// like the pre-existing, profile-less code did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FrontendProfile {
    #[serde(default)]
    pub kind: FrontendProfileKind,
    #[serde(default)]
    pub policy: FrontendProfilePolicy,
}

/// What kind of filesystem object a planned Library View entry ultimately
/// is (or will become). Only `Symlink` is ever actually produced by
/// `plan_library_view`/`apply_library_view` in this milestone - `GeneratedFile`
/// and `Directory` exist so later milestones (RomM/ES-DE materialization)
/// can extend the planner and manifest without another backward-compatibility
/// migration. See `plan_generated_file`/`classify_library_view_object` for
/// the planning-only seams that use the non-`Symlink` cases today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LibraryViewObjectKind {
    /// A managed symlink pointing at a source archive - the only kind this
    /// milestone's `apply_library_view` ever creates.
    #[default]
    Symlink,
    /// A managed regular file whose content EmuWiz itself would generate
    /// (e.g. a future `.m3u`/`gamelist.xml`) - never a copy or hardlink of a
    /// source archive. Not created by this milestone.
    GeneratedFile,
    /// A managed directory EmuWiz itself created (as opposed to a
    /// pre-existing directory it merely wrote into) - see
    /// `LibraryViewManifest::created_directories`. Not created as a
    /// standalone planned object by this milestone.
    Directory,
}

/// Derives the filename a Library View would display for `archive_path`
/// under `profile`, without ever renaming or otherwise touching the source
/// archive. A pure name-deriving primitive: it does not itself decide
/// *whether* a profile is allowed to plan at all (that fail-closed gate is
/// `generate_relative_link_path`'s - see its doc comment - so this function
/// is in practice only ever reached for `Generic` today). In this milestone
/// every profile kind still resolves to exactly the archive's own filename
/// (sanitised) if called directly, so a later profile-driven rename policy
/// has one obvious place to plug into instead of every call site
/// re-deriving a filename by hand.
///
/// Rejects (never invents a fallback for): an absent filename, an empty
/// name, `.`/`..`, any path separator, and anything else that is not a
/// single safe path component - the exact same rules
/// `sanitize_path_component_os` already enforces for the pre-existing
/// generic behaviour, so `Generic`-profile output is provably unchanged.
pub fn derive_view_filename(profile: &FrontendProfile, archive_path: &Path) -> Result<PathBuf> {
    let source_filename = archive_path.file_name().ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "{} has no filename to use in a Library View",
            archive_path.display()
        ))
    })?;
    let derived: OsString = match profile.kind {
        // Stage 1/2: every kind derives the same filename as the source
        // archive - see the function doc comment for why.
        FrontendProfileKind::Generic | FrontendProfileKind::Romm | FrontendProfileKind::EsDe => {
            source_filename.to_os_string()
        }
    };
    sanitize_path_component_os(&derived)
}

/// Resolves the config-y "list of views" file: `library_views.json` under the
/// effective config directory (EmuWiz's `~/.config/emuwiz`, or the legacy
/// `~/.config/archivefs`), alongside `config.toml`/`source_folders`. JSON
/// rather than another hand-rolled `[[block]]` format: each view's
/// `source_folders`/`platforms` are list fields, which the existing
/// line-based TOML parser (`parse_config_fields`) has no support for nesting
/// inside a block - `serde_json` (already an archivefs-cli dependency)
/// avoids inventing that parser just for this.
pub fn default_library_views_config_path() -> Result<PathBuf> {
    crate::app_dirs::config_path("library_views.json")
}

/// Resolves the per-view manifests directory: `library_views/` under the
/// effective data directory, alongside `library.sqlite3`/`index.json`,
/// deliberately never inside a user's source folder.
pub fn default_library_views_data_dir() -> Result<PathBuf> {
    Ok(crate::app_dirs::data_dir()?.join("library_views"))
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

/// One managed symlink EmuWiz created, as recorded in a view's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewManifestEntry {
    /// Relative to the view's `destination_root` at the time this entry
    /// was written - never an absolute path, so a manifest stays valid if
    /// the whole destination tree is ever relocated by the user outside
    /// EmuWiz (repair would then simply report every entry as broken).
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
    /// What kind of filesystem object this entry owns. `#[serde(default)]`
    /// resolves every entry written before this field existed to `Symlink`,
    /// which is exactly what every such entry actually is, since Symlink
    /// was the only kind `apply_library_view` could ever produce before
    /// this milestone.
    #[serde(default)]
    pub object_kind: LibraryViewObjectKind,
    /// The content hash a future `GeneratedFile` entry's managed content
    /// would be identified by. Always `None` in this milestone - no entry's
    /// `object_kind` is ever `GeneratedFile` yet, since nothing creates one.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// The version of the (future) rendering rules used to produce this
    /// entry's content, for a `GeneratedFile` entry. Always `None` in this
    /// milestone, for the same reason as `content_hash`.
    #[serde(default)]
    pub rendering_version: Option<u32>,
}

/// # Schema evolution / backward compatibility
///
/// Every field the Frontend Profiles milestone added to this struct and to
/// `LibraryViewManifestEntry` (`object_kind`, `content_hash`,
/// `rendering_version`, `view_fingerprint`, `profile_version`,
/// `created_directories`) carries `#[serde(default)]`, so a manifest JSON
/// file written before this milestone existed - one with none of these keys
/// at all - deserializes exactly as it always did, with:
///
/// - `object_kind` resolving to `Symlink` on every existing entry, which is
///   exactly correct: `Symlink` was the only kind any manifest-writing code
///   could ever have produced before this milestone.
/// - `content_hash`/`rendering_version` resolving to `None` - meaningless
///   for a `Symlink` entry, and no `GeneratedFile` entry existed yet to have
///   had them.
/// - `view_fingerprint` resolving to `None` - deliberately distinct from
///   "recorded and matches the current profile": `plan_library_view` only
///   ever reports `LibraryViewPlan::fingerprint_conflict` when a fingerprint
///   *was* recorded and it disagrees, so an old manifest predating the field
///   is never treated as incompatible merely for predating it.
/// - `profile_version` resolving to `0` - a write-format counter, not a
///   per-edit revision; `apply_library_view` always writes `1` from this
///   milestone onward.
/// - `created_directories` resolving to an empty `Vec` - correct, since no
///   apply before this milestone ever recorded directory ownership; the
///   field is not yet populated by this milestone's `apply_library_view`
///   either (see the field's own doc comment), so it round-trips whatever a
///   later milestone starts writing into it without this one clobbering it.
///
/// Every one of these defaults was chosen to be the value that describes
/// what *already happened* under the pre-Frontend-Profiles code, never a
/// value that would silently change interpretation of old data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewManifest {
    pub view_id: String,
    #[serde(with = "path_json")]
    pub destination_root: PathBuf,
    pub entries: Vec<LibraryViewManifestEntry>,
    /// The `compute_view_profile_fingerprint` value of the view's
    /// configuration at the time this manifest was last written by
    /// `apply_library_view`/`repair_library_view`. `#[serde(default)]`
    /// resolves a pre-Frontend-Profiles manifest to `None` - deliberately
    /// distinct from "recorded and matches", so `plan_library_view` never
    /// treats an old manifest as fingerprint-incompatible merely for
    /// predating this field (see `LibraryViewPlan::fingerprint_conflict`).
    #[serde(default)]
    pub view_fingerprint: Option<String>,
    /// A simple write-format counter (not a per-edit revision number):
    /// `0` for any manifest written before this milestone's
    /// fingerprint-aware apply path existed (`#[serde(default)]`), `1` for
    /// every manifest `apply_library_view` writes now.
    #[serde(default)]
    pub profile_version: u32,
    /// Directories `apply_library_view` itself created under
    /// `destination_root` (never a directory that already existed) - the
    /// same "only record what EmuWiz itself created" ownership rule
    /// `LibraryViewManifestEntry` already applies to symlinks. Not yet
    /// populated or consulted by this milestone's `apply_library_view`
    /// (which does not create standalone managed directories or generated
    /// files - see the milestone notes); the field exists purely so a
    /// pre-existing manifest keeps loading once a later milestone starts
    /// writing into it, without another migration.
    #[serde(default, with = "vec_path_json")]
    pub created_directories: Vec<PathBuf>,
}

impl LibraryViewManifest {
    fn empty(view_id: &str, destination_root: &Path) -> Self {
        Self {
            view_id: view_id.to_string(),
            destination_root: destination_root.to_path_buf(),
            entries: Vec::new(),
            view_fingerprint: None,
            profile_version: 0,
            created_directories: Vec::new(),
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

/// Confirms a planned symlink target (`archive_path`) actually resolves
/// inside `source_root` - the same canonicalize-then-`starts_with`
/// containment check `validate_library_view_destination` already applies to
/// a view's *destination*, applied here to the *source* side instead:
/// `plan_library_view` must never plan a symlink whose target has escaped
/// the trusted source root the catalogue says it came from (e.g. via a
/// symlink planted inside a source folder that itself points somewhere
/// untrusted, discovered after the catalogue record was written). A
/// read-only check (`fs::canonicalize`/`fs::metadata` only, via
/// `canonical_or_nearest_existing_ancestor`) - consistent with planning
/// never mutating anything.
fn validate_symlink_target_within_source(archive_path: &Path, source_root: &Path) -> Result<()> {
    let archive_canonical = canonical_or_nearest_existing_ancestor(archive_path)?;
    let source_canonical =
        fs::canonicalize(source_root).unwrap_or_else(|_| source_root.to_path_buf());
    if archive_canonical.starts_with(&source_canonical) {
        Ok(())
    } else {
        Err(ArchiveFsError::Config(format!(
            "{} does not resolve inside its configured source folder {} - refusing to plan a \
             symlink target that could escape the trusted source root",
            archive_path.display(),
            source_root.display()
        )))
    }
}

// ---------------------------------------------------------------------
// Layout / path generation.
// ---------------------------------------------------------------------

/// Builds the relative link path for one archive under `template`,
/// rejecting anything that could escape the destination root (milestone
/// requirement: "reject path traversal through generated names"). The
/// filename component always goes through `derive_view_filename`, so a
/// non-UTF-8 archive filename is preserved exactly rather than mangled or
/// rejected outright.
///
/// The *directory* shape depends on `profile.kind`:
/// - `Generic`: `{platform}/{filename}` - the catalogue's own platform
///   string, sanitised, exactly as before this milestone.
/// - `Romm`: `roms/{resolved-romm-slug}/{filename}` - see
///   `resolve_romm_platform_slug` for how the slug is resolved (never
///   invented from the catalogue platform string or from a display name).
///   `romm_identity_cache` is threaded through only for this tier.
/// - `EsDe`: always refused - no real materialization exists yet (see
///   `FrontendProfileKind`'s doc comment); this is defense in depth
///   alongside `plan_library_view`'s own `profile_error` check, so any other
///   caller of this function gets the same refusal rather than a silent
///   `Generic`-shaped path.
pub fn generate_relative_link_path(
    template: LibraryViewLayoutTemplate,
    profile: &FrontendProfile,
    platform: &str,
    archive_path: &Path,
    romm_identity_cache: Option<&crate::identity_source::cache::IdentityCache>,
) -> Result<PathBuf> {
    let LibraryViewLayoutTemplate::PlatformFilename = template;
    let filename_component = derive_view_filename(profile, archive_path)?;
    match profile.kind {
        FrontendProfileKind::Generic => {
            let platform_component = sanitize_path_component_str(platform)?;
            Ok(PathBuf::from(platform_component).join(filename_component))
        }
        FrontendProfileKind::Romm => {
            let slug = resolve_romm_platform_slug(
                platform,
                &profile.policy.platform_mapping_overrides,
                romm_identity_cache,
            )
            .ok_or_else(|| {
                ArchiveFsError::Config(format!(
                    "no RomM platform slug could be resolved for catalogue platform \
                     {platform:?} - refusing to plan a RomM path rather than guessing one \
                     (add an explicit platform_mapping_overrides entry, or import this \
                     platform from a connected RomM instance first)"
                ))
            })?;
            let slug_component = sanitize_path_component_str(&slug)?;
            Ok(PathBuf::from("roms")
                .join(slug_component)
                .join(filename_component))
        }
        FrontendProfileKind::EsDe => Err(ArchiveFsError::Config(
            "the EsDe frontend profile does not implement real Library View materialization \
             yet in this milestone - refusing to plan a path rather than silently falling back \
             to Generic behaviour"
                .to_string(),
        )),
    }
}

/// Resolves the RomM platform slug `catalogue_platform` (the catalogue's own
/// canonical platform id - never changed or re-derived by this function) maps
/// to, trying each tier in strict precedence order and stopping at the first
/// that answers:
///
/// 1. `overrides` - an explicit user override
///    (`FrontendProfilePolicy::platform_mapping_overrides`).
/// 2. `identity_cache` - a locally published, previously imported RomM
///    instance's own reported slug for this platform
///    (`IdentityCache::romm_slug_for_platform`) - read entirely offline, no
///    network request. `None` when no cache was loaded/available, which is
///    simply "this tier has nothing to say", not a refusal.
///
/// Returns `None` when neither tier answers - callers must fail closed
/// rather than inventing a slug (e.g. lower-casing or otherwise sanitising
/// the canonical platform id and assuming RomM accepts it).
///
/// # No bundled/default static table
///
/// This was evaluated and deliberately rejected: the repo's only existing
/// RomM-slug table (`identity_source::romm::normalise::canonical_platform_for_romm_slug`'s
/// `ROMM_SLUG_ALIASES`) resolves *inbound* provider slugs to a canonical
/// platform, and several of its entries are intentionally approximate,
/// many-to-one associations for that purpose (e.g. `fds` -> `NES`, `pc-fx` ->
/// `PC Engine`, `xboxone` -> `Xbox` - see that table's own doc comment).
/// Inverting it to pick a *default output* slug for a canonical platform
/// would silently produce a wrong-but-plausible-looking slug for exactly
/// those platforms (`NES`'s real RomM slug is `nes`, never `fds`) - which is
/// worse than failing closed, not safer. If a genuinely 1:1, vetted forward
/// table is added later, it belongs here as a third tier below the two
/// above; guessing one now was out of scope.
pub fn resolve_romm_platform_slug(
    catalogue_platform: &str,
    overrides: &FrontendPlatformMapping,
    identity_cache: Option<&crate::identity_source::cache::IdentityCache>,
) -> Option<String> {
    if let Some(slug) = overrides.get(catalogue_platform) {
        return Some(slug.to_string());
    }
    if let Some(slug) =
        identity_cache.and_then(|cache| cache.romm_slug_for_platform(catalogue_platform))
    {
        return Some(slug.to_string());
    }
    None
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
    /// The fingerprint (`compute_view_profile_fingerprint`) of `view`'s
    /// *current* configuration, as planned just now.
    pub profile_fingerprint: String,
    /// `Some` when the manifest this plan was computed against already
    /// recorded a `view_fingerprint` (i.e. it was written by a
    /// fingerprint-aware apply) and that recorded fingerprint does not match
    /// `profile_fingerprint` - meaning the view's profile/layout changed in
    /// an output-affecting way since the manifest was last written. The
    /// GUI/CLI must refuse Apply while this is set: this milestone never
    /// automatically destroys or rebuilds an existing view under a changed
    /// profile, it only surfaces the conflict for review. `None` (not a
    /// conflict) whenever the manifest recorded no fingerprint at all - an
    /// old, pre-Frontend-Profiles manifest is never treated as "incompatible"
    /// merely for predating this field.
    pub fingerprint_conflict: Option<String>,
    /// `Some` when `view.profile.kind` is not `Generic` - `RomM`/`EsDe`
    /// materialization is not implemented in this milestone, so planning
    /// fails closed with a clear refusal here rather than silently
    /// producing `Generic`-shaped output (see `FrontendProfileKind`'s doc
    /// comment). The GUI/CLI must refuse Apply while this is set, exactly
    /// like `unsafe_root_error`/`fingerprint_conflict`.
    pub profile_error: Option<String>,
}

impl LibraryViewPlan {
    pub fn is_safe_to_apply(&self) -> bool {
        self.unsafe_root_error.is_none()
            && self.fingerprint_conflict.is_none()
            && self.profile_error.is_none()
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
    romm_identity_cache: Option<&crate::identity_source::cache::IdentityCache>,
) -> LibraryViewPlan {
    let mut counts = LibraryViewPlanCounts::default();
    let mut entries = Vec::new();

    let all_source_paths: Vec<PathBuf> = source_folders.iter().map(|s| s.path.clone()).collect();
    let unsafe_root_error =
        match validate_library_view_destination(&view.destination_root, &all_source_paths) {
            Ok(_) => None,
            Err(error) => Some(error.to_string()),
        };
    // Fail closed for a not-yet-implemented frontend kind - see
    // `FrontendProfileKind`'s doc comment. Computed once here (rather than
    // only inside `generate_relative_link_path`'s own per-record refusal)
    // so the plan carries one clear, top-level reason Apply is refused,
    // in addition to every individual candidate also reporting
    // `SkipInvalidPath` with the same underlying cause.
    // `Romm` is no longer blanket-refused here: it plans real
    // `roms/<slug>/<filename>` paths when every selected archive's platform
    // resolves to a RomM slug (see `resolve_romm_platform_slug`), and each
    // archive whose platform does *not* resolve is refused individually as
    // `SkipInvalidPath` by `generate_relative_link_path` below - reusing the
    // exact same per-record refusal path `Generic` already uses for an
    // unsafe derived filename, rather than a new plan-wide field. `EsDe`
    // still has no real materialization at all, so it keeps the blanket,
    // plan-wide refusal.
    let profile_error = match view.profile.kind {
        FrontendProfileKind::Generic | FrontendProfileKind::Romm => None,
        FrontendProfileKind::EsDe => Some(format!(
            "the {:?} frontend profile does not implement real Library View materialization \
             yet in this milestone - refusing to plan/apply rather than silently falling back \
             to Generic behaviour",
            view.profile.kind
        )),
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
    // Only ever populated for a `Romm` profile, and only with platforms this
    // plan actually resolved successfully (never a guessed/unresolved
    // value) - folded into `profile_fingerprint` below via
    // `compute_view_profile_fingerprint_with_resolved_romm_mapping` so a
    // resolved-mapping change (e.g. a re-imported RomM cache reporting a
    // different slug) is visible as a fingerprint change, not silently
    // invisible drift. A `BTreeMap` so its key order - and therefore the
    // fingerprint - never depends on `records`' own iteration order.
    let mut resolved_romm_mapping: BTreeMap<String, String> = BTreeMap::new();
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
            &view.profile,
            &platform,
            &record.absolute_path,
            romm_identity_cache,
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
        if view.profile.kind == FrontendProfileKind::Romm
            && let Some(slug) = resolve_romm_platform_slug(
                &platform,
                &view.profile.policy.platform_mapping_overrides,
                romm_identity_cache,
            )
        {
            resolved_romm_mapping.insert(platform.clone(), slug);
        }
        // Source-side containment: the planned symlink target must itself
        // resolve inside the trusted source root the catalogue says it
        // came from - never touched by ordinary scans (which only ever
        // discover archives under a source folder), but this refuses to
        // plan a symlink target that has since been made to escape that
        // root (e.g. a symlink planted inside the source folder pointing
        // somewhere untrusted). See `validate_symlink_target_within_source`.
        if let Err(error) =
            validate_symlink_target_within_source(&record.absolute_path, &source.path)
        {
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

    // Deterministic output: `wanted` above is a `HashMap`, so its iteration
    // order (and therefore the order entries were pushed in passes 1-3) is
    // not guaranteed stable across runs/platforms. Sort once, at the very
    // end, by `(view_relative_path, source_path)` - repeated planning from
    // identical inputs must always produce an identical plan vector. Entries
    // with no relative/archive path (the two catalogue-only skip reasons)
    // sort together at the front under the empty-path key, ordered by
    // archive path as the tiebreaker; that is still fully deterministic,
    // just not alphabetically interleaved with path-bearing entries.
    entries.sort_by(|a, b| {
        let a_key = (
            a.relative_link_path.clone().unwrap_or_default(),
            a.archive_path.clone().unwrap_or_default(),
        );
        let b_key = (
            b.relative_link_path.clone().unwrap_or_default(),
            b.archive_path.clone().unwrap_or_default(),
        );
        a_key.cmp(&b_key)
    });

    let profile_fingerprint =
        compute_view_profile_fingerprint_with_resolved_romm_mapping(view, resolved_romm_mapping);
    let fingerprint_conflict = manifest.view_fingerprint.as_ref().and_then(|recorded| {
        if *recorded == profile_fingerprint {
            None
        } else {
            Some(format!(
                "this view's existing manifest was written under a different frontend profile \
                 fingerprint ({recorded}) than its current configuration ({profile_fingerprint}) \
                 produces - refusing to apply automatically; review the profile change (or the \
                 manifest) before re-applying"
            ))
        }
    });

    LibraryViewPlan {
        view_id: view.id.clone(),
        destination_root: view.destination_root.clone(),
        counts,
        entries,
        unsafe_root_error,
        profile_fingerprint,
        fingerprint_conflict,
        profile_error,
    }
}

/// A deterministic fingerprint of everything about `view` that affects the
/// *shape* of its planned output - the layout template and the frontend
/// profile (kind + policy). Two views with the same fingerprint are
/// guaranteed to plan identical relative link paths for the same catalogue
/// input; two views that differ only in fields that do not affect output
/// shape (`id`, `name`, `enabled`, `source_folders`, `platforms`,
/// `destination_root`) are *not* guaranteed to differ.
///
/// Deliberately excludes anything time-based, random, or HashMap-ordered:
/// `FrontendPlatformMapping` is backed by a `BTreeMap` specifically so this
/// fingerprint's input JSON always serializes its keys in the same (sorted)
/// order regardless of insertion order. Uses `sha2` (already a dependency -
/// see `Cargo.toml`) rather than adding a new hash crate just for this.
pub fn compute_view_profile_fingerprint(view: &LibraryViewConfig) -> String {
    compute_view_profile_fingerprint_with_resolved_romm_mapping(view, BTreeMap::new())
}

/// Like `compute_view_profile_fingerprint`, but also folds in the exact
/// canonical-platform -> RomM-slug mappings a `Romm`-profile plan actually
/// resolved and used (see `resolve_romm_platform_slug`) - a `BTreeMap` for
/// deterministic (canonical-platform-ordered), insertion-order-independent
/// serialization, exactly like `FrontendPlatformMapping`'s own backing.
///
/// This closes a real drift gap: RomM output depends on a resolved mapping
/// that can itself change independently of `view` (e.g. a locally cached
/// RomM instance is re-imported with a different slug for the same
/// platform) - `layout_template`/`profile` alone cannot see that. Only the
/// mappings *actually used by this plan* are hashed (never every platform
/// the cache happens to know about), so an unrelated platform changing in
/// the cache never perturbs a plan that never touched it. An unresolved
/// platform is never represented here at all (never a guessed value) -
/// it stays visible only as that record's own `SkipInvalidPath` entry.
///
/// `#[serde(skip_serializing_if = "...")]` means an empty map serializes to
/// exactly the same JSON `compute_view_profile_fingerprint` already
/// produced before this existed - so a `Generic`/`EsDe` view's fingerprint
/// (and a `Romm` view that resolved nothing at all) is provably byte-for-byte
/// unchanged, and no fingerprint recorded by the already-committed Stage 1/2
/// code is ever invalidated by this addition. (No previously *applied*
/// `Romm` manifest could exist to invalidate in any case: Stage 1/2's `Romm`
/// was blanket fail-closed, so `apply_library_view` could never have
/// succeeded and written one.)
fn compute_view_profile_fingerprint_with_resolved_romm_mapping(
    view: &LibraryViewConfig,
    resolved_romm_mapping: BTreeMap<String, String>,
) -> String {
    #[derive(Serialize)]
    struct FingerprintInput<'a> {
        layout_template: LibraryViewLayoutTemplate,
        profile: &'a FrontendProfile,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        resolved_romm_mapping: BTreeMap<String, String>,
    }
    let input = FingerprintInput {
        layout_template: view.layout_template,
        profile: &view.profile,
        resolved_romm_mapping,
    };
    let canonical = serde_json::to_string(&input)
        .expect("FingerprintInput has no non-serializable field (no maps/floats/NaN)");
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    encode_hex(&hasher.finalize())
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
// Stage 2 ownership model: generalizes `classify_existing_path`'s
// Symlink-specific classification to the full `LibraryViewObjectKind`
// vocabulary, for planning/dry-run use by a later milestone's `GeneratedFile`/
// `Directory` materialization. Read-only, exactly like `classify_existing_path`
// - `fs::symlink_metadata`/`fs::read_link` only, never a mutation. Not yet
// wired into `plan_library_view` itself (which only ever plans `Symlink`
// entries in this milestone); exists so the ownership rules below are
// already implemented, tested, and stable before anything calls them from
// the main planning pass.
// ---------------------------------------------------------------------

/// What a path on disk turned out to be, classified against what a
/// (possibly hypothetical, not-yet-real) managed object of `expected_kind`
/// would look like there. Never assigns `Owned*` merely because a path's
/// *name* matches something the manifest recorded - ownership additionally
/// requires the recorded entry to exist for that exact relative path
/// (`is_owned_by_manifest`) *and* the actual filesystem object to be
/// consistent with what that entry says it should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryViewObjectClassification {
    /// Nothing exists at this path yet.
    Missing,
    /// A manifest-owned object of the expected kind, matching what the
    /// manifest recorded for it (for `Symlink`: pointing at the recorded
    /// target).
    OwnedCorrect,
    /// A manifest-owned object of the expected kind, but not matching what
    /// the manifest recorded (for `Symlink`: pointing somewhere else) -
    /// drift since the last apply, safe to repair.
    OwnedStale,
    /// A real (non-symlink) file or directory exists here, and it is not
    /// recorded as manifest-owned - never touched.
    ForeignRealFile,
    /// A symlink exists here, and it is not recorded as manifest-owned -
    /// never touched, even if it happens to point at the same target a
    /// managed symlink would.
    ForeignSymlink,
    /// Something is recorded as manifest-owned at this exact relative path,
    /// but what is actually on disk is not the kind of object the manifest
    /// says it should be (e.g. the manifest says `Symlink` but a real
    /// directory sits there now) - never touched; this is a refusal state,
    /// not something to be silently reinterpreted.
    WrongObjectKind,
}

/// Classifies whatever is at `path` against a managed object of
/// `expected_kind` - `expected_target` is only meaningful for
/// `LibraryViewObjectKind::Symlink` (the exact symlink target the manifest
/// recorded); `is_owned_by_manifest` must be computed by the caller from an
/// actual manifest lookup (e.g. "does some entry's `relative_link_path`
/// equal this path's relative form"), never inferred from the path's name
/// alone - see `LibraryViewObjectClassification`'s doc comment.
pub fn classify_library_view_object(
    path: &Path,
    expected_kind: LibraryViewObjectKind,
    expected_target: Option<&Path>,
    is_owned_by_manifest: bool,
) -> LibraryViewObjectClassification {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return LibraryViewObjectClassification::Missing,
    };
    let file_type = metadata.file_type();

    match expected_kind {
        LibraryViewObjectKind::Symlink => {
            if !file_type.is_symlink() {
                return if is_owned_by_manifest {
                    LibraryViewObjectClassification::WrongObjectKind
                } else {
                    LibraryViewObjectClassification::ForeignRealFile
                };
            }
            match fs::read_link(path) {
                Ok(actual_target) if Some(actual_target.as_path()) == expected_target => {
                    LibraryViewObjectClassification::OwnedCorrect
                }
                _ if is_owned_by_manifest => LibraryViewObjectClassification::OwnedStale,
                _ => LibraryViewObjectClassification::ForeignSymlink,
            }
        }
        LibraryViewObjectKind::GeneratedFile => {
            if file_type.is_symlink() {
                return if is_owned_by_manifest {
                    LibraryViewObjectClassification::WrongObjectKind
                } else {
                    LibraryViewObjectClassification::ForeignSymlink
                };
            }
            if !file_type.is_file() {
                return LibraryViewObjectClassification::WrongObjectKind;
            }
            if is_owned_by_manifest {
                // Content-hash comparison (the real "still correct?" check
                // for a GeneratedFile) is future work - nothing creates a
                // GeneratedFile yet, so there is no real content to compare
                // against. Conservatively `OwnedStale` rather than
                // `OwnedCorrect`: never claim a not-yet-implemented check
                // passed.
                LibraryViewObjectClassification::OwnedStale
            } else {
                LibraryViewObjectClassification::ForeignRealFile
            }
        }
        LibraryViewObjectKind::Directory => {
            if !file_type.is_dir() {
                return if is_owned_by_manifest {
                    LibraryViewObjectClassification::WrongObjectKind
                } else {
                    LibraryViewObjectClassification::ForeignRealFile
                };
            }
            if is_owned_by_manifest {
                LibraryViewObjectClassification::OwnedCorrect
            } else {
                LibraryViewObjectClassification::ForeignRealFile
            }
        }
    }
}

/// A planned (never written) future `GeneratedFile` entry: the relative
/// path it would occupy, and the content hash its intended content would
/// have. Model/planning only - nothing in this milestone ever writes the
/// file `intended_content_hash` describes; see `plan_generated_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryViewGeneratedFilePlan {
    #[serde(with = "path_json")]
    pub relative_path: PathBuf,
    pub intended_content_hash: String,
    /// What `classify_library_view_object` reports for this path against
    /// the view's current manifest/filesystem state - i.e. what applying
    /// this (still-hypothetical) `GeneratedFile` entry would actually do,
    /// without doing it.
    pub classification: LibraryViewObjectClassification,
}

/// Computes what a future `GeneratedFile` entry at `relative_path` (under
/// `destination_root`) would contain (`intended_content_hash`, via `sha2` -
/// already a dependency, see `Cargo.toml`) and what applying it would find
/// on disk right now (`classification`) - entirely a dry-run read: this
/// never creates `content`, `relative_path`, or any directory. The single
/// seam a later milestone's real `GeneratedFile` materialization (e.g. a
/// RomM `.m3u` or an ES-DE `gamelist.xml`) can build on without this
/// milestone having guessed at its content format.
pub fn plan_generated_file(
    destination_root: &Path,
    relative_path: PathBuf,
    content: &[u8],
    manifest: &LibraryViewManifest,
) -> LibraryViewGeneratedFilePlan {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let intended_content_hash = encode_hex(&hasher.finalize());

    let is_owned_by_manifest = manifest.entries.iter().any(|entry| {
        entry.relative_link_path == relative_path
            && entry.object_kind == LibraryViewObjectKind::GeneratedFile
    });
    let destination_path = destination_root.join(&relative_path);
    let classification = classify_library_view_object(
        &destination_path,
        LibraryViewObjectKind::GeneratedFile,
        None,
        is_owned_by_manifest,
    );

    LibraryViewGeneratedFilePlan {
        relative_path,
        intended_content_hash,
        classification,
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
    /// as intended ("never remove anything EmuWiz did not record as
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
            plan.unsafe_root_error
                .clone()
                .or_else(|| plan.profile_error.clone())
                .or_else(|| plan.fingerprint_conflict.clone())
                .unwrap_or_else(|| {
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
                        object_kind: LibraryViewObjectKind::Symlink,
                        content_hash: None,
                        rendering_version: None,
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
                                object_kind: LibraryViewObjectKind::Symlink,
                                content_hash: None,
                                rendering_version: None,
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

    let mut new_entries: Vec<LibraryViewManifestEntry> = entries_by_path.into_values().collect();
    // Deterministic output, same reasoning as `plan_library_view`'s final
    // sort: `entries_by_path` is a `HashMap`, so its iteration order is not
    // guaranteed stable.
    new_entries.sort_by(|a, b| a.relative_link_path.cmp(&b.relative_link_path));

    let new_manifest = LibraryViewManifest {
        view_id: view.id.clone(),
        destination_root: view.destination_root.clone(),
        entries: new_entries,
        // Reuses the fingerprint `plan_library_view` already computed
        // (rather than recomputing it from `view` alone) so the recorded
        // fingerprint always matches exactly what was planned - including,
        // for a `Romm` profile, the resolved canonical-platform -> RomM-slug
        // mappings this plan actually used (see
        // `compute_view_profile_fingerprint_with_resolved_romm_mapping`).
        // Recomputing from `view` here would silently drop that resolved-
        // mapping component and let a later plan's fingerprint mismatch one
        // this very apply just wrote.
        view_fingerprint: Some(plan.profile_fingerprint.clone()),
        profile_version: 1,
        // Not yet populated by this milestone's apply (see the field's own
        // doc comment) - carried forward unchanged rather than reset, so a
        // later milestone that does start populating it is never silently
        // wiped by an apply from this one.
        created_directories: manifest.created_directories.clone(),
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
        // Removal does not re-plan or re-apply a profile - it only drops
        // symlinks the previous manifest already owned - so the previous
        // fingerprint/version/directory-ownership bookkeeping is carried
        // forward unchanged rather than reset or recomputed here.
        view_fingerprint: manifest.view_fingerprint.clone(),
        profile_version: manifest.profile_version,
        created_directories: manifest.created_directories.clone(),
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

/// Loads the locally published RomM identity cache for `resolve_romm_platform_slug`'s
/// tier 2, if one exists and is usable - entirely offline (`load_cache`'s own
/// doc comment: "makes no network request of any kind"). Any reason the
/// cache cannot be used (missing, unreadable, wrong format version, from a
/// different server than expected) simply means this tier has nothing to
/// say for this plan - it is not escalated to a planning error, since a
/// `Generic` or `EsDe` view (or a `Romm` view whose platforms are all
/// resolved via an explicit override) never needed it in the first place.
/// Whether `IdentityCache.server_id` could be checked against the currently
/// configured RomM source without adding network access, before trusting
/// the cache for planning: investigated and deliberately **not done**.
///
/// `IdentityCache.server_id` is stamped from `ValidatedRommSource::server_id()`
/// (`self.endpoint.origin()` - the *validated, normalised* endpoint), not
/// from the raw `RommSourceConfig.url` string `config.json` stores. Building
/// a `ValidatedRommSource` to reproduce that exact value requires
/// `ValidatedRommSource::validate`, which needs a `HostResolver` and a
/// loaded token - not a pure local-file read, and not obviously free of
/// resolver-level network activity (DNS) depending on the resolver
/// implementation passed in. Comparing against the raw, unvalidated `url`
/// string instead would risk a false mismatch against the normalised origin
/// the cache was actually stamped with (scheme/port/trailing-slash
/// differences) - a *second*, subtly different notion of "the configured
/// server" that could disagree with the one `server_id()` already defines.
/// That would be worse than not checking at all: a spurious refusal is a
/// working feature reported broken, and a spurious pass is what it exists
/// to prevent.
///
/// So this is deferred rather than invented: `load_cache` below is called
/// with `expected_server: None`, meaning "accept a cache from any server" -
/// exactly the same acceptance behaviour Stage 1/2 already had for RomM
/// (which never read a cache at all). Revisit if `ValidatedRommSource`
/// grows a way to reconstruct/compare its `server_id` from `config.json`
/// alone.
fn load_romm_identity_cache_for_default_planning()
-> Option<crate::identity_source::cache::IdentityCache> {
    let identity_root = crate::identity_source::settings::default_identity_root().ok()?;
    let location = crate::identity_source::cache::IdentityCacheLocation::new(
        &identity_root,
        crate::identity_source::model::IdentityProvider::Romm,
    );
    crate::identity_source::cache::load_cache(&location, None).ok()
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
    // Only a `Romm` profile ever consults this tier - loading it otherwise
    // would be a needless read with no planning effect.
    let romm_identity_cache = if view.profile.kind == FrontendProfileKind::Romm {
        load_romm_identity_cache_for_default_planning()
    } else {
        None
    };
    let plan = plan_library_view(
        &view,
        &archives,
        &source_folders,
        &manifest,
        romm_identity_cache.as_ref(),
    );
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
    profile: FrontendProfile,
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
        profile,
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
    profile: FrontendProfile,
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
        view.profile = profile;
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
/// EmuWiz did not record as managed" even when the manifest is stale
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
/// now-empty directory EmuWiz created under `destination_root` - never
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
            assigned_platform: None,
            unknown_archive_count: 0,
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
            identity_report: None,
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
            profile: FrontendProfile::default(),
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

        let _plan = plan_library_view(&view, &[archive], &[source], &manifest, None);

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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);

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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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
            None,
        );
        let report1 = apply_library_view(&view, &plan1, &manifest1, &data_dir).unwrap();
        assert_eq!(report1.created, 1);

        let manifest2 = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        let plan2 = plan_library_view(&view, &[archive], &[source], &manifest2, None);
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
        // created by EmuWiz, but already points exactly where EmuWiz
        // would put it.
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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
                object_kind: LibraryViewObjectKind::Symlink,
                content_hash: None,
                rendering_version: None,
            }],
            view_fingerprint: None,
            profile_version: 0,
            created_directories: Vec::new(),
        };

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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
            None,
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
        let plan = plan_library_view(
            &view,
            &[archive],
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        let manifest_after_first = load_library_view_manifest_at(&data_dir, &view.id).unwrap();

        // Second plan against an empty catalogue: the managed link becomes
        // stale and should be the only thing removed.
        let plan2 = plan_library_view(&view, &[], &[source], &manifest_after_first, None);
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
                object_kind: LibraryViewObjectKind::Symlink,
                content_hash: None,
                rendering_version: None,
            }],
            view_fingerprint: None,
            profile_version: 0,
            created_directories: Vec::new(),
        };

        // The path the manifest thinks is a managed symlink has since been
        // replaced by a real file - e.g. by the user, outside EmuWiz.
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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
            None,
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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
                object_kind: LibraryViewObjectKind::Symlink,
                content_hash: None,
                rendering_version: None,
            }],
            view_fingerprint: None,
            profile_version: 0,
            created_directories: Vec::new(),
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
            profile_fingerprint: compute_view_profile_fingerprint(&view),
            fingerprint_conflict: None,
            profile_error: None,
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

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
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

        let plan = plan_library_view(
            &view,
            &[record_ok, record_unknown],
            &[source],
            &manifest,
            None,
        );

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

    // -------------------------------------------------------------------
    // Frontend Profiles milestone - Stage 1.
    // -------------------------------------------------------------------

    #[test]
    fn old_config_without_profile_field_deserializes_unchanged() {
        let json = r#"[
            {
                "id": "view-1",
                "name": "Old View",
                "destination_root": "/tmp/old-dest",
                "enabled": true,
                "source_folders": [],
                "platforms": [],
                "layout_template": "PlatformFilename"
            }
        ]"#;
        let views = parse_library_view_configs(json).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, "view-1");
        assert_eq!(
            views[0].profile,
            FrontendProfile::default(),
            "a config predating the profile field must resolve to the default profile"
        );
        assert_eq!(views[0].profile.kind, FrontendProfileKind::Generic);
    }

    #[test]
    fn new_profile_default_preserves_platform_filename_behavior() {
        let root = temp_dir("profile-defaults-preserve-behavior");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Super Mario Bros. (USA) (Rev 1) [!].zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));

        // `FrontendProfile::default()` (Generic, default policy) - what
        // `make_view` already constructs - must plan exactly the
        // pre-existing `PlatformFilename` output.
        let view = make_view("view-generic", &destination, vec![], vec![]);
        assert_eq!(view.profile.kind, FrontendProfileKind::Generic);
        let manifest = empty_manifest(&view.id, &destination);
        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);

        assert_eq!(plan.profile_error, None);
        assert_eq!(
            plan.entries[0].relative_link_path,
            Some(PathBuf::from("NES/Super Mario Bros. (USA) (Rev 1) [!].zip"))
        );
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
    }

    #[test]
    fn esde_profile_kind_fails_closed_never_silently_falls_back_to_generic() {
        let root = temp_dir("esde-fail-closed");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Super Mario Bros. (USA) (Rev 1) [!].zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));

        let mut view = make_view("view-esde", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::EsDe;
        let manifest = empty_manifest(&view.id, &destination);
        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert!(
            plan.profile_error.is_some(),
            "EsDe must surface a clear planning refusal, not a silent fallback"
        );
        assert!(!plan.is_safe_to_apply());
        // Never silently degrades to Generic's Create entry - either no
        // entry is produced for this candidate, or it is explicitly
        // reported as skipped, but it must never be `Create`.
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action != LibraryViewPlanAction::Create),
            "EsDe must never silently produce a Generic-shaped Create entry"
        );

        let result = apply_library_view(&view, &plan, &manifest, &data_dir);
        assert!(
            result.is_err(),
            "EsDe apply must be refused, not silently applied"
        );

        // Defense in depth: the lower-level path generator refuses directly
        // too, for any caller that bypasses plan_library_view.
        assert!(
            generate_relative_link_path(
                view.layout_template,
                &view.profile,
                "NES",
                &archive_path,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn romm_profile_kind_fails_closed_per_entry_for_an_unresolved_platform() {
        // "3DO" has no RomM slug in any tier used here (no override, no
        // cache, and it is not one of `ROMM_SLUG_ALIASES`'s targets) - see
        // `resolve_romm_platform_slug`.
        let root = temp_dir("romm-unresolved-platform");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("3DO"));
        let mut view = make_view("view-romm-unresolved", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        // Romm is not blanket-refused at the plan level - only EsDe is.
        assert_eq!(plan.profile_error, None);
        // But the one candidate, whose platform cannot be resolved to a
        // RomM slug, must be refused individually and never silently
        // produce a Generic-shaped Create entry.
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipInvalidPath
        );
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action != LibraryViewPlanAction::Create),
            "an unresolved RomM platform must never silently produce a Generic-shaped Create entry"
        );

        // Apply never creates anything for a Skip entry (the same rule
        // every other Skip* reason already relies on).
        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        assert_eq!(report.created, 0);
        assert!(!destination.join("roms").exists());

        // Defense in depth: the lower-level path generator refuses directly
        // too, for any caller that bypasses plan_library_view.
        assert!(
            generate_relative_link_path(
                view.layout_template,
                &view.profile,
                "3DO",
                &archive_path,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn plan_is_deterministic_across_repeated_calls_and_input_order() {
        let root = temp_dir("plan-determinism");
        let source_dir = root.join("source");
        let mut archives = Vec::new();
        for (index, (platform, name)) in [
            ("NES", "Alpha.zip"),
            ("NES", "Beta.zip"),
            ("SNES", "Charlie.zip"),
            ("SNES", "Delta.zip"),
            ("GBA", "Echo.zip"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = source_dir.join(name);
            write_file(&path, b"zip-bytes");
            archives.push(make_archive(index as i64 + 1, 1, &path, Some(platform)));
        }
        let destination = root.join("dest");
        let source = make_source(1, &source_dir);
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan_a = plan_library_view(
            &view,
            &archives,
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        // Re-plan from a differently-ordered (reversed) input Vec - a real
        // HashMap-ordering hazard would show up as a different entries
        // order here.
        let mut reversed = archives.clone();
        reversed.reverse();
        let plan_b = plan_library_view(
            &view,
            &reversed,
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let plan_c = plan_library_view(&view, &archives, &[source], &manifest, None);

        assert_eq!(
            plan_a.entries, plan_b.entries,
            "planning from a reordered input must still produce an identically ordered plan"
        );
        assert_eq!(
            plan_a.entries, plan_c.entries,
            "repeated planning from identical inputs must produce identical plan vectors"
        );

        // The stated ordering rule: (view_relative_path, source_path),
        // non-decreasing.
        let keys: Vec<(PathBuf, PathBuf)> = plan_a
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.relative_link_path.clone().unwrap_or_default(),
                    entry.archive_path.clone().unwrap_or_default(),
                )
            })
            .collect();
        let mut sorted_keys = keys.clone();
        sorted_keys.sort();
        assert_eq!(
            keys, sorted_keys,
            "plan entries must be sorted by (view_relative_path, source_path)"
        );
    }

    #[test]
    fn derived_filename_sanitisation_rejects_unsafe_components() {
        assert!(
            sanitize_path_component_str("").is_err(),
            "empty must be rejected"
        );
        assert!(
            sanitize_path_component_str(".").is_err(),
            "'.' must be rejected"
        );
        assert!(
            sanitize_path_component_str("..").is_err(),
            "'..' must be rejected"
        );
        assert!(
            sanitize_path_component_str("a/b").is_err(),
            "an embedded path separator must be rejected"
        );
        assert!(
            sanitize_path_component_str("/etc/passwd").is_err(),
            "an absolute path must be rejected"
        );
        assert!(
            sanitize_path_component_str("../../etc/passwd").is_err(),
            "a traversal attempt must be rejected"
        );
        assert!(sanitize_path_component_str("Game.zip").is_ok());

        assert!(sanitize_path_component_os(OsStr::new("")).is_err());
        assert!(sanitize_path_component_os(OsStr::new("..")).is_err());
        assert!(sanitize_path_component_os(OsStr::new("a/b")).is_err());
        assert!(sanitize_path_component_os(OsStr::new("Game.zip")).is_ok());
    }

    #[test]
    fn derive_view_filename_rejects_archive_path_with_no_filename() {
        let profile = FrontendProfile::default();
        assert!(derive_view_filename(&profile, Path::new("/")).is_err());
        assert!(derive_view_filename(&profile, Path::new(".")).is_err());
    }

    #[test]
    fn derive_view_filename_never_invents_a_fallback_and_never_renames_source() {
        let profile = FrontendProfile::default();
        let archive_path = Path::new("/roms/nes/Game (USA).zip");
        let derived = derive_view_filename(&profile, archive_path).unwrap();
        assert_eq!(
            derived,
            PathBuf::from("Game (USA).zip"),
            "Stage 1 default behaviour must equal the source filename exactly"
        );
        // The archive itself is never touched by deriving its view filename.
        assert_eq!(archive_path.file_name().unwrap(), "Game (USA).zip");
    }

    #[test]
    fn duplicate_derived_destination_is_reported_as_collision_not_silently_disambiguated() {
        let root = temp_dir("derived-destination-collision");
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
            None,
        );

        assert_eq!(plan.counts.collision, 2);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action != LibraryViewPlanAction::Create),
            "two archives that derive the same destination must never be silently disambiguated"
        );
    }

    #[test]
    fn planning_never_mutates_the_source_archive_under_any_profile_kind() {
        let root = temp_dir("no-mutation-any-profile");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"original-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-1", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        let _plan = plan_library_view(&view, &[archive], &[source], &manifest, None);

        assert_eq!(fs::read(&archive_path).unwrap(), b"original-bytes");
        assert!(
            !destination.exists(),
            "planning must never create the destination directory either"
        );
    }

    #[test]
    fn frontend_profile_kinds_serialize_and_deserialize_safely() {
        for kind in [
            FrontendProfileKind::Generic,
            FrontendProfileKind::Romm,
            FrontendProfileKind::EsDe,
        ] {
            let profile = FrontendProfile {
                kind,
                policy: FrontendProfilePolicy::default(),
            };
            let json = serde_json::to_string(&profile).unwrap();
            let round_tripped: FrontendProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(profile, round_tripped);
        }
    }

    // -------------------------------------------------------------------
    // Frontend Profiles milestone - Stage 2.
    // -------------------------------------------------------------------

    #[test]
    fn old_manifest_without_new_fields_deserializes_unchanged() {
        let json = r#"{
            "view_id": "view-1",
            "destination_root": "/tmp/dest",
            "entries": [
                {
                    "relative_link_path": "NES/Game.zip",
                    "target_path": "/source/Game.zip",
                    "archive_identity": null,
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z",
                    "platform": "NES",
                    "source_folder_path": "/source"
                }
            ]
        }"#;
        let manifest: LibraryViewManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(
            manifest.entries[0].object_kind,
            LibraryViewObjectKind::Symlink
        );
        assert_eq!(manifest.entries[0].content_hash, None);
        assert_eq!(manifest.entries[0].rendering_version, None);
        assert_eq!(manifest.view_fingerprint, None);
        assert_eq!(manifest.profile_version, 0);
        assert_eq!(manifest.created_directories, Vec::<PathBuf>::new());
    }

    #[test]
    fn new_manifest_fields_serde_default_correctly() {
        let manifest = LibraryViewManifest::empty("view-1", Path::new("/tmp/dest"));
        let json = serde_json::to_string(&manifest).unwrap();
        let round_tripped: LibraryViewManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, round_tripped);
        assert_eq!(round_tripped.view_fingerprint, None);
        assert_eq!(round_tripped.profile_version, 0);
        assert!(round_tripped.created_directories.is_empty());
    }

    #[test]
    fn object_kind_round_trips_through_json_for_every_variant() {
        for kind in [
            LibraryViewObjectKind::Symlink,
            LibraryViewObjectKind::GeneratedFile,
            LibraryViewObjectKind::Directory,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let round_tripped: LibraryViewObjectKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, round_tripped);
        }
    }

    #[test]
    fn profile_fingerprint_is_deterministic() {
        let view = make_view("view-1", Path::new("/tmp/dest"), vec![], vec![]);
        let a = compute_view_profile_fingerprint(&view);
        let b = compute_view_profile_fingerprint(&view);
        assert_eq!(a, b);

        let other = make_view("view-2", Path::new("/tmp/dest"), vec![], vec![]);
        assert_eq!(
            a,
            compute_view_profile_fingerprint(&other),
            "fingerprint depends only on layout_template/profile, not id/name/destination"
        );
    }

    #[test]
    fn profile_fingerprint_changes_for_output_affecting_policy() {
        let base = make_view("view-1", Path::new("/tmp/dest"), vec![], vec![]);
        let base_fingerprint = compute_view_profile_fingerprint(&base);

        let mut kind_changed = base.clone();
        kind_changed.profile.kind = FrontendProfileKind::Romm;
        assert_ne!(
            base_fingerprint,
            compute_view_profile_fingerprint(&kind_changed)
        );

        let mut policy_changed = base.clone();
        policy_changed.profile.policy.region_preference = vec!["USA".to_string()];
        assert_ne!(
            base_fingerprint,
            compute_view_profile_fingerprint(&policy_changed)
        );

        let mut platform_override_changed = base.clone();
        platform_override_changed
            .profile
            .policy
            .platform_mapping_overrides
            .insert("NES".to_string(), "Nintendo".to_string());
        assert_ne!(
            base_fingerprint,
            compute_view_profile_fingerprint(&platform_override_changed)
        );

        // Fields that do not affect output shape must never change the
        // fingerprint.
        let mut cosmetic_changed = base.clone();
        cosmetic_changed.name = "Renamed".to_string();
        cosmetic_changed.enabled = false;
        cosmetic_changed.source_folders = vec![PathBuf::from("/somewhere")];
        cosmetic_changed.platforms = vec!["NES".to_string()];
        cosmetic_changed.destination_root = PathBuf::from("/elsewhere");
        assert_eq!(
            base_fingerprint,
            compute_view_profile_fingerprint(&cosmetic_changed),
            "id/name/enabled/source_folders/platforms/destination_root must not affect the fingerprint"
        );
    }

    #[test]
    fn incompatible_manifest_fingerprint_produces_refusal_never_silent_overwrite() {
        let root = temp_dir("fingerprint-conflict-refusal");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);

        // A manifest recorded under some other (unrelated) fingerprint -
        // simulating a profile change since the manifest was last written.
        let mut existing_manifest = empty_manifest(&view.id, &destination);
        existing_manifest.view_fingerprint = Some("not-the-current-fingerprint".to_string());
        save_library_view_manifest_at(&data_dir, &existing_manifest).unwrap();
        let manifest_path = library_view_manifest_path(&data_dir, &view.id);
        let before = fs::read_to_string(&manifest_path).unwrap();

        let plan = plan_library_view(&view, &[archive], &[source], &existing_manifest, None);
        assert!(
            plan.fingerprint_conflict.is_some(),
            "a manifest recorded under a different fingerprint must surface a conflict"
        );
        assert!(!plan.is_safe_to_apply());

        let result = apply_library_view(&view, &plan, &existing_manifest, &data_dir);
        assert!(
            result.is_err(),
            "apply must refuse rather than silently overwrite"
        );

        let after = fs::read_to_string(&manifest_path).unwrap();
        assert_eq!(
            before, after,
            "a refused apply must never touch the previous manifest"
        );
    }

    #[test]
    fn no_fingerprint_recorded_is_never_treated_as_a_conflict() {
        // An empty/pre-Frontend-Profiles manifest never records a
        // fingerprint at all - that must never be treated as
        // "incompatible", only an actual mismatch should be.
        let root = temp_dir("no-fingerprint-not-a-conflict");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);
        assert_eq!(manifest.view_fingerprint, None);

        let plan = plan_library_view(&view, &[archive], &[source], &manifest, None);
        assert_eq!(plan.fingerprint_conflict, None);
        assert!(plan.is_safe_to_apply());
    }

    #[test]
    fn classify_foreign_real_file_is_preserved_not_owned() {
        let root = temp_dir("classify-foreign-real-file");
        let path = root.join("Game.zip");
        write_file(&path, b"a real file");
        let classification = classify_library_view_object(
            &path,
            LibraryViewObjectKind::Symlink,
            Some(Path::new("/somewhere/Game.zip")),
            false,
        );
        assert_eq!(
            classification,
            LibraryViewObjectClassification::ForeignRealFile
        );
    }

    #[test]
    fn classify_foreign_symlink_is_preserved_not_owned() {
        let root = temp_dir("classify-foreign-symlink");
        let target = root.join("elsewhere.zip");
        write_file(&target, b"x");
        let path = root.join("Game.zip");
        symlink(&target, &path).unwrap();
        let classification = classify_library_view_object(
            &path,
            LibraryViewObjectKind::Symlink,
            Some(Path::new("/somewhere/Game.zip")),
            false,
        );
        assert_eq!(
            classification,
            LibraryViewObjectClassification::ForeignSymlink
        );
    }

    #[test]
    fn classify_wrong_object_kind_is_detected_for_every_expected_kind() {
        let root = temp_dir("classify-wrong-object-kind");

        let real_file = root.join("owned-as-symlink.zip");
        write_file(&real_file, b"x");
        assert_eq!(
            classify_library_view_object(&real_file, LibraryViewObjectKind::Symlink, None, true),
            LibraryViewObjectClassification::WrongObjectKind
        );

        let a_directory = root.join("owned-as-generated-file");
        fs::create_dir_all(&a_directory).unwrap();
        assert_eq!(
            classify_library_view_object(
                &a_directory,
                LibraryViewObjectKind::GeneratedFile,
                None,
                true
            ),
            LibraryViewObjectClassification::WrongObjectKind
        );

        let a_file = root.join("owned-as-directory");
        write_file(&a_file, b"x");
        assert_eq!(
            classify_library_view_object(&a_file, LibraryViewObjectKind::Directory, None, true),
            LibraryViewObjectClassification::WrongObjectKind
        );
    }

    #[test]
    fn classify_stale_owned_symlink_behavior_is_unchanged() {
        let root = temp_dir("classify-stale-owned-symlink");
        let wrong_target = root.join("wrong.zip");
        write_file(&wrong_target, b"x");
        let path = root.join("Game.zip");
        symlink(&wrong_target, &path).unwrap();

        let classification = classify_library_view_object(
            &path,
            LibraryViewObjectKind::Symlink,
            Some(Path::new("/expected/Game.zip")),
            true,
        );
        assert_eq!(classification, LibraryViewObjectClassification::OwnedStale);
    }

    #[test]
    fn classify_missing_when_nothing_exists() {
        let root = temp_dir("classify-missing");
        let path = root.join("does-not-exist.zip");
        assert_eq!(
            classify_library_view_object(&path, LibraryViewObjectKind::Symlink, None, true),
            LibraryViewObjectClassification::Missing
        );
    }

    #[test]
    fn generated_file_content_hash_planning_never_writes_a_file() {
        let root = temp_dir("generated-file-planning-no-write");
        let destination = root.join("dest");
        fs::create_dir_all(&destination).unwrap();
        let manifest = empty_manifest("view-1", &destination);
        let content = b"<gamelist/>";

        let planned = plan_generated_file(
            &destination,
            PathBuf::from("NES/gamelist.xml"),
            content,
            &manifest,
        );

        let mut hasher = Sha256::new();
        hasher.update(content);
        let expected_hash = encode_hex(&hasher.finalize());
        assert_eq!(planned.intended_content_hash, expected_hash);
        assert_eq!(
            planned.classification,
            LibraryViewObjectClassification::Missing
        );
        assert!(
            !destination.join("NES/gamelist.xml").exists(),
            "planning a GeneratedFile entry must never write it to disk"
        );
    }

    #[test]
    fn created_directories_field_is_backwards_compatible_and_round_trips() {
        let json_without_field = r#"{
            "view_id": "view-1",
            "destination_root": "/tmp/dest",
            "entries": []
        }"#;
        let manifest: LibraryViewManifest = serde_json::from_str(json_without_field).unwrap();
        assert!(manifest.created_directories.is_empty());

        let mut populated = manifest.clone();
        populated.created_directories = vec![PathBuf::from("NES"), PathBuf::from("SNES/discs")];
        let json = serde_json::to_string(&populated).unwrap();
        let round_tripped: LibraryViewManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            round_tripped.created_directories,
            populated.created_directories
        );
    }

    #[test]
    fn applied_manifest_entries_are_written_in_deterministic_order() {
        let root = temp_dir("deterministic-manifest-order");
        let source_dir = root.join("source");
        let mut archives = Vec::new();
        for (index, (platform, name)) in [
            ("NES", "Zulu.zip"),
            ("NES", "Alpha.zip"),
            ("SNES", "Mike.zip"),
            ("GBA", "Bravo.zip"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = source_dir.join(name);
            write_file(&path, b"zip-bytes");
            archives.push(make_archive(index as i64 + 1, 1, &path, Some(platform)));
        }
        let destination = root.join("dest");
        let data_dir = root.join("data");
        let source = make_source(1, &source_dir);
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &archives, &[source], &manifest, None);
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        let written = load_library_view_manifest_at(&data_dir, &view.id).unwrap();

        let mut expected = written.entries.clone();
        expected.sort_by(|a, b| a.relative_link_path.cmp(&b.relative_link_path));
        assert_eq!(
            written.entries, expected,
            "manifest entries must be written in a deterministic (sorted) order"
        );
    }

    // -------------------------------------------------------------------
    // RomM real planning slice.
    // -------------------------------------------------------------------

    fn romm_view_with_override(
        id: &str,
        destination: &Path,
        catalogue_platform: &str,
        slug: &str,
    ) -> LibraryViewConfig {
        let mut view = make_view(id, destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        view.profile
            .policy
            .platform_mapping_overrides
            .insert(catalogue_platform.to_string(), slug.to_string());
        view
    }

    fn identity_cache_with_romm_slug(
        catalogue_platform: &str,
        slug: &str,
    ) -> crate::identity_source::cache::IdentityCache {
        crate::identity_source::cache::IdentityCache {
            format_version: crate::identity_source::cache::CACHE_FORMAT_VERSION,
            provider: crate::identity_source::model::IdentityProvider::Romm,
            server_id: "test-server".to_string(),
            server_version: None,
            source_fingerprint: "test-fingerprint".to_string(),
            imported_at_unix_seconds: 0,
            platforms: vec![
                crate::identity_source::romm::normalise::NormalisedPlatform {
                    provider_platform_id: None,
                    provider_slug: slug.to_string(),
                    provider_name: None,
                    canonical: Some(catalogue_platform.to_string()),
                    rom_count: None,
                },
            ],
            records: Vec::new(),
            rejected_hashes: Vec::new(),
            unknown_platforms: Vec::new(),
            server_reported_total: None,
        }
    }

    /// Builds an identity cache whose `platforms` list is exactly the given
    /// `(provider_slug, canonical)` pairs, so a test can exercise the
    /// ambiguous-mapping case (two distinct slugs for one canonical platform).
    fn identity_cache_with_romm_platforms(
        platforms: Vec<(&str, &str)>,
    ) -> crate::identity_source::cache::IdentityCache {
        crate::identity_source::cache::IdentityCache {
            format_version: crate::identity_source::cache::CACHE_FORMAT_VERSION,
            provider: crate::identity_source::model::IdentityProvider::Romm,
            server_id: "test-server".to_string(),
            server_version: None,
            source_fingerprint: "test-fingerprint".to_string(),
            imported_at_unix_seconds: 0,
            platforms: platforms
                .into_iter()
                .enumerate()
                .map(|(index, (slug, canonical))| {
                    crate::identity_source::romm::normalise::NormalisedPlatform {
                        provider_platform_id: Some(index.to_string()),
                        provider_slug: slug.to_string(),
                        provider_name: None,
                        canonical: Some(canonical.to_string()),
                        rom_count: None,
                    }
                })
                .collect(),
            records: Vec::new(),
            rejected_hashes: Vec::new(),
            unknown_platforms: Vec::new(),
            server_reported_total: None,
        }
    }

    /// Test A: a known canonical platform ID maps to the expected
    /// `roms/<slug>/<filename>` path.
    #[test]
    fn romm_profile_maps_a_known_platform_to_the_expected_roms_slug_path() {
        let root = temp_dir("romm-known-platform-path");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Super Mario Bros. (USA) (Rev 1) [!].zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.profile_error, None);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(
            plan.entries[0].relative_link_path,
            Some(PathBuf::from(
                "roms/nes/Super Mario Bros. (USA) (Rev 1) [!].zip"
            ))
        );
        assert_eq!(
            plan.entries[0].destination_path,
            Some(
                destination
                    .join("roms")
                    .join("nes")
                    .join("Super Mario Bros. (USA) (Rev 1) [!].zip")
            )
        );
    }

    /// Test B: an explicit user override wins over a locally cached
    /// instance's own reported slug.
    #[test]
    fn romm_explicit_override_wins_over_the_local_identity_cache() {
        let root = temp_dir("romm-override-beats-cache");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        // Override says "nes-override"; the cache (a lower-precedence tier)
        // disagrees and says "nes-from-cache" - the override must win.
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes-override");
        let cache = identity_cache_with_romm_slug("NES", "nes-from-cache");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache),
        );

        assert_eq!(
            plan.entries[0].relative_link_path,
            Some(PathBuf::from("roms/nes-override/Game.zip"))
        );

        // And directly at the resolver level, for good measure.
        assert_eq!(
            resolve_romm_platform_slug(
                "NES",
                &view.profile.policy.platform_mapping_overrides,
                Some(&cache)
            ),
            Some("nes-override".to_string())
        );
    }

    /// The local identity cache tier works on its own too (no override
    /// present), confirming tier 2 is actually consulted, not just
    /// shadowed by tier 1 in the test above.
    #[test]
    fn romm_local_identity_cache_resolves_a_platform_with_no_override() {
        let root = temp_dir("romm-cache-only");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let cache = identity_cache_with_romm_slug("NES", "nes-from-cache");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache),
        );

        assert_eq!(
            plan.entries[0].relative_link_path,
            Some(PathBuf::from("roms/nes-from-cache/Game.zip"))
        );
    }

    /// An identity cache reporting *two distinct* provider slugs for the
    /// same canonical platform (e.g. a RomM instance with both an `fds` and
    /// an `nes` filesystem folder that both normalise to `NES`) is
    /// ambiguous - `IdentityCache::romm_slug_for_platform` now fails closed
    /// (`None`) rather than silently picking the lexicographically-first
    /// one, since a plausible-looking but arbitrary directory choice is
    /// exactly the kind of guess this milestone must never make. Planning
    /// must therefore refuse that entry individually (`SkipInvalidPath`),
    /// the same as any other unresolved platform - never fall back to a
    /// Generic-shaped path, and never guess between the two candidates.
    #[test]
    fn romm_local_identity_cache_ambiguous_mapping_fails_closed_not_a_silent_winner() {
        let root = temp_dir("romm-cache-ambiguous");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let cache = identity_cache_with_romm_platforms(vec![("fds", "NES"), ("nes", "NES")]);
        let manifest = empty_manifest(&view.id, &destination);

        // The resolver itself already refuses ambiguity.
        assert_eq!(
            resolve_romm_platform_slug(
                "NES",
                &view.profile.policy.platform_mapping_overrides,
                Some(&cache)
            ),
            None
        );

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache),
        );

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipInvalidPath
        );
        assert_eq!(plan.entries[0].relative_link_path, None);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action != LibraryViewPlanAction::Create),
            "an ambiguous cache mapping must never silently pick a winner or fall back to Generic"
        );

        // An explicit override still takes precedence over an ambiguous
        // cache and resolves the entry normally.
        let mut overridden = view.clone();
        overridden
            .profile
            .policy
            .platform_mapping_overrides
            .insert("NES".to_string(), "nes-pinned".to_string());
        let plan_overridden = plan_library_view(
            &overridden,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache),
        );
        assert_eq!(
            plan_overridden.entries[0].relative_link_path,
            Some(PathBuf::from("roms/nes-pinned/Game.zip"))
        );
    }

    /// Test C (see also `romm_profile_kind_fails_closed_per_entry_for_an_unresolved_platform`
    /// above): an unresolved RomM platform fails closed and never produces a
    /// Generic-shaped path, even when other archives in the same plan do
    /// resolve.
    #[test]
    fn romm_unresolved_platform_never_produces_a_generic_path_while_other_entries_stay_safe() {
        let root = temp_dir("romm-partial-unresolved");
        let source_dir = root.join("source");
        let resolved_path = source_dir.join("Resolved.zip");
        let unresolved_path = source_dir.join("Unresolved.zip");
        write_file(&resolved_path, b"a");
        write_file(&unresolved_path, b"b");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let resolved_archive = make_archive(1, 1, &resolved_path, Some("NES"));
        let unresolved_archive = make_archive(2, 1, &unresolved_path, Some("3DO"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[resolved_archive, unresolved_archive],
            &[source],
            &manifest,
            None,
        );

        assert_eq!(plan.profile_error, None, "Romm is not blanket-refused");
        assert_eq!(
            plan.counts.create, 1,
            "the resolved NES archive stays previewable"
        );
        assert_eq!(
            plan.counts.skip, 1,
            "the unresolved 3DO archive is refused individually"
        );
        let unresolved_entry = plan
            .entries
            .iter()
            .find(|entry| entry.archive_path.as_deref() == Some(unresolved_path.as_path()))
            .unwrap();
        assert_eq!(
            unresolved_entry.action,
            LibraryViewPlanAction::SkipInvalidPath
        );
        assert_eq!(unresolved_entry.relative_link_path, None);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.relative_link_path != Some(PathBuf::from("3DO/Unresolved.zip"))),
            "must never silently fall back to a Generic-shaped path for the unresolved archive"
        );
    }

    /// Test E: changing the RomM mapping changes the fingerprint.
    #[test]
    fn changing_romm_mapping_override_changes_the_fingerprint() {
        let destination = Path::new("/tmp/dest-romm-fingerprint");
        let unmapped = make_view("view-romm", destination, vec![], vec![]);
        let mut unmapped = unmapped;
        unmapped.profile.kind = FrontendProfileKind::Romm;
        let mapped = romm_view_with_override("view-romm", destination, "NES", "nes");

        assert_ne!(
            compute_view_profile_fingerprint(&unmapped),
            compute_view_profile_fingerprint(&mapped),
            "adding a RomM platform_mapping_overrides entry must change the fingerprint"
        );

        let mapped_differently =
            romm_view_with_override("view-romm", destination, "NES", "nintendo");
        assert_ne!(
            compute_view_profile_fingerprint(&mapped),
            compute_view_profile_fingerprint(&mapped_differently),
            "changing the mapped slug for the same platform must change the fingerprint"
        );
    }

    /// Test F: mapping insertion order must not affect the fingerprint or
    /// the plan - `FrontendPlatformMapping` is `BTreeMap`-backed.
    #[test]
    fn romm_mapping_insertion_order_never_affects_fingerprint_or_plan() {
        let root = temp_dir("romm-insertion-order");
        let source_dir = root.join("source");
        let nes_path = source_dir.join("Nes.zip");
        let snes_path = source_dir.join("Snes.zip");
        write_file(&nes_path, b"a");
        write_file(&snes_path, b"b");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let nes_archive = make_archive(1, 1, &nes_path, Some("NES"));
        let snes_archive = make_archive(2, 1, &snes_path, Some("SNES"));

        let mut view_a = make_view("view-romm", &destination, vec![], vec![]);
        view_a.profile.kind = FrontendProfileKind::Romm;
        view_a
            .profile
            .policy
            .platform_mapping_overrides
            .insert("NES".to_string(), "nes".to_string());
        view_a
            .profile
            .policy
            .platform_mapping_overrides
            .insert("SNES".to_string(), "snes".to_string());

        let mut view_b = make_view("view-romm", &destination, vec![], vec![]);
        view_b.profile.kind = FrontendProfileKind::Romm;
        // Same two entries, inserted in the opposite order.
        view_b
            .profile
            .policy
            .platform_mapping_overrides
            .insert("SNES".to_string(), "snes".to_string());
        view_b
            .profile
            .policy
            .platform_mapping_overrides
            .insert("NES".to_string(), "nes".to_string());

        assert_eq!(
            compute_view_profile_fingerprint(&view_a),
            compute_view_profile_fingerprint(&view_b),
            "BTreeMap-backed platform_mapping_overrides must serialize identically regardless \
             of insertion order"
        );

        let manifest = empty_manifest("view-romm", &destination);
        let plan_a = plan_library_view(
            &view_a,
            &[nes_archive.clone(), snes_archive.clone()],
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let plan_b = plan_library_view(
            &view_b,
            &[nes_archive, snes_archive],
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        assert_eq!(plan_a.entries, plan_b.entries);
    }

    /// Test G: a duplicate RomM destination is a Collision, never a silent
    /// winner.
    #[test]
    fn romm_duplicate_destination_is_collision_never_a_silent_winner() {
        let root = temp_dir("romm-duplicate-destination");
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
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_a, record_b],
            &[source_a, source_b],
            &manifest,
            None,
        );

        assert_eq!(plan.counts.collision, 2);
        assert_eq!(plan.counts.create, 0);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action != LibraryViewPlanAction::Create),
            "two archives resolving to the same RomM destination must never be silently \
             disambiguated"
        );
    }

    /// Test H: a foreign real file already at the RomM destination is
    /// preserved, never overwritten.
    #[test]
    fn romm_foreign_real_file_at_destination_is_preserved() {
        let root = temp_dir("romm-foreign-real-file");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let foreign_path = destination.join("roms").join("nes").join("Game.zip");
        write_file(&foreign_path, b"a real, unmanaged file");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Collision);
        assert_eq!(
            fs::read(&foreign_path).unwrap(),
            b"a real, unmanaged file",
            "a foreign real file must never be overwritten by planning or apply"
        );
    }

    /// Test I: a foreign symlink already at the RomM destination is
    /// preserved, never overwritten.
    #[test]
    fn romm_foreign_symlink_at_destination_is_preserved() {
        let root = temp_dir("romm-foreign-symlink");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let unrelated_target = root.join("unrelated.zip");
        write_file(&unrelated_target, b"unrelated");
        let foreign_link = destination.join("roms").join("nes").join("Game.zip");
        fs::create_dir_all(foreign_link.parent().unwrap()).unwrap();
        symlink(&unrelated_target, &foreign_link).unwrap();

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Collision);
        let target = fs::read_link(&foreign_link).unwrap();
        assert_eq!(
            target, unrelated_target,
            "a foreign symlink must never be repointed by planning or apply"
        );
    }

    /// Test J: source-side containment still rejects a symlink target that
    /// escapes its declared source folder, under the RomM profile too.
    #[test]
    fn romm_source_side_containment_still_rejects_escape() {
        let root = temp_dir("romm-source-containment-escape");
        let source_dir = root.join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let outside_dir = root.join("outside");
        let real_target = outside_dir.join("Elsewhere.zip");
        write_file(&real_target, b"outside-bytes");
        let escaping_symlink = source_dir.join("Escape.zip");
        symlink(&real_target, &escaping_symlink).unwrap();
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &escaping_symlink, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipInvalidPath
        );
        assert_eq!(plan.counts.create, 0);
    }

    /// Test K: planning a resolved RomM view never mutates the source
    /// archive and never creates the destination directory.
    #[test]
    fn romm_planning_with_resolved_mapping_never_mutates_source_or_creates_destination() {
        let root = temp_dir("romm-resolved-no-mutation");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"original-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(fs::read(&archive_path).unwrap(), b"original-bytes");
        assert!(
            !destination.exists(),
            "planning must never create the destination directory either"
        );
    }

    /// Test L: apply creates only a symlink under `destination_root` for a
    /// resolved RomM entry - never a regular file, never anything outside
    /// `destination_root`.
    #[test]
    fn romm_apply_creates_only_a_symlink_under_destination_root() {
        let root = temp_dir("romm-apply-symlink-only");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let report = apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        assert_eq!(report.created, 1);

        let created_path = destination.join("roms").join("nes").join("Game.zip");
        let metadata = fs::symlink_metadata(&created_path).unwrap();
        assert!(
            metadata.file_type().is_symlink(),
            "apply must create a symlink, not a regular file"
        );
        assert_eq!(fs::read_link(&created_path).unwrap(), archive_path);

        let written = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        assert_eq!(written.entries.len(), 1);
        assert_eq!(
            written.entries[0].object_kind,
            LibraryViewObjectKind::Symlink
        );
    }

    /// Test N: a RomM apply never produces a manifest entry claiming a
    /// `GeneratedFile`/`Directory` object kind - the only materialization
    /// path this stage exercises is the pre-existing managed-symlink one.
    #[test]
    fn romm_apply_never_produces_a_generated_file_or_directory_object_kind() {
        let root = temp_dir("romm-no-generated-file-or-directory");
        let source_dir = root.join("source");
        let nes_path = source_dir.join("Nes.zip");
        let snes_path = source_dir.join("Snes.zip");
        write_file(&nes_path, b"a");
        write_file(&snes_path, b"b");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let nes_archive = make_archive(1, 1, &nes_path, Some("NES"));
        let snes_archive = make_archive(2, 1, &snes_path, Some("SNES"));
        let mut view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        view.profile
            .policy
            .platform_mapping_overrides
            .insert("SNES".to_string(), "snes".to_string());
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[nes_archive, snes_archive],
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        apply_library_view(&view, &plan, &manifest, &data_dir).unwrap();
        let written = load_library_view_manifest_at(&data_dir, &view.id).unwrap();

        assert_eq!(written.entries.len(), 2);
        assert!(
            written
                .entries
                .iter()
                .all(|entry| entry.object_kind == LibraryViewObjectKind::Symlink),
            "this stage must never produce a GeneratedFile or Directory manifest entry"
        );
    }

    // -------------------------------------------------------------------
    // Resolved RomM-mapping fingerprint drift fix.
    // -------------------------------------------------------------------

    /// Test 1: identical profile and identical resolved RomM mapping ->
    /// identical fingerprint, across independent plan calls.
    #[test]
    fn resolved_romm_mapping_fingerprint_is_identical_for_identical_resolution() {
        let root = temp_dir("romm-fingerprint-identical");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let plan_a = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let plan_b = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan_a.profile_fingerprint, plan_b.profile_fingerprint);
    }

    /// Test 2: same profile, but the resolved slug for the same platform
    /// changes (via the cache tier, not the config itself) -> the
    /// fingerprint changes.
    #[test]
    fn resolved_romm_mapping_fingerprint_changes_when_resolved_slug_changes() {
        let root = temp_dir("romm-fingerprint-changed-slug");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        // No override at all - the *only* thing that differs between the two
        // plans below is what the identity cache reports.
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        let cache_a = identity_cache_with_romm_slug("NES", "nes");
        let cache_b = identity_cache_with_romm_slug("NES", "nes-renamed");

        let plan_a = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_a),
        );
        let plan_b = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_b),
        );

        assert_ne!(
            plan_a.profile_fingerprint, plan_b.profile_fingerprint,
            "the view's own config did not change, but the resolved output did - the \
             fingerprint must reflect the resolved mapping, not just the declared profile"
        );
    }

    /// Test 3: a manifest fingerprinted against one resolved cache mapping,
    /// re-planned after the cache changes what it reports for the same
    /// platform, must surface a fingerprint conflict rather than silently
    /// treating the new resolution as equivalent.
    #[test]
    fn changed_cache_mapping_against_existing_manifest_produces_fingerprint_conflict() {
        let root = temp_dir("romm-cache-drift-conflict");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        // First apply, resolved via the cache reporting "nes".
        let cache_before = identity_cache_with_romm_slug("NES", "nes");
        let plan_before = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_before),
        );
        assert!(plan_before.is_safe_to_apply());
        let report = apply_library_view(&view, &plan_before, &manifest, &data_dir).unwrap();
        assert_eq!(report.created, 1);
        let applied_manifest = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        assert_eq!(
            applied_manifest.view_fingerprint,
            Some(plan_before.profile_fingerprint.clone())
        );

        // The cache is re-imported and now reports a different slug for the
        // very same platform - simulating a RomM instance renaming its
        // filesystem platform folder between imports.
        let cache_after = identity_cache_with_romm_slug("NES", "nintendo-nes");
        let plan_after = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &applied_manifest,
            Some(&cache_after),
        );

        assert!(
            plan_after.fingerprint_conflict.is_some(),
            "a resolved-mapping change against an already-applied manifest must be a conflict"
        );
        assert!(!plan_after.is_safe_to_apply());
    }

    /// Test 4: that conflict actually blocks `apply_library_view` outright -
    /// before any stale link is removed or any new link is created, not
    /// merely reported and then ignored.
    #[test]
    fn fingerprint_conflict_blocks_apply_before_any_link_mutation() {
        let root = temp_dir("romm-cache-drift-blocks-apply");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let data_dir = root.join("data");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        let cache_before = identity_cache_with_romm_slug("NES", "nes");
        let plan_before = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_before),
        );
        apply_library_view(&view, &plan_before, &manifest, &data_dir).unwrap();
        let applied_manifest = load_library_view_manifest_at(&data_dir, &view.id).unwrap();
        let manifest_path = library_view_manifest_path(&data_dir, &view.id);
        let manifest_bytes_before = fs::read(&manifest_path).unwrap();
        let old_link = destination.join("roms").join("nes").join("Game.zip");
        assert!(fs::symlink_metadata(&old_link).is_ok());

        let cache_after = identity_cache_with_romm_slug("NES", "nintendo-nes");
        let plan_after = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &applied_manifest,
            Some(&cache_after),
        );
        assert!(!plan_after.is_safe_to_apply());

        let result = apply_library_view(&view, &plan_after, &applied_manifest, &data_dir);
        assert!(
            result.is_err(),
            "apply must be refused outright, not partially performed"
        );

        // Nothing was mutated: the old symlink is exactly as it was, the new
        // `roms/nintendo-nes/` path was never created, and the manifest file
        // on disk is byte-for-byte unchanged.
        assert!(
            fs::symlink_metadata(&old_link).is_ok(),
            "the previously managed symlink must not have been removed"
        );
        assert!(
            !destination.join("roms").join("nintendo-nes").exists(),
            "no new path may be created for a refused apply"
        );
        let manifest_bytes_after = fs::read(&manifest_path).unwrap();
        assert_eq!(
            manifest_bytes_before, manifest_bytes_after,
            "a refused apply must never touch the manifest file"
        );
    }

    /// Test 5: a platform this plan never touches changing in the cache must
    /// never perturb the fingerprint - only mappings *actually used by this
    /// plan* are hashed.
    #[test]
    fn unrelated_cache_platform_change_does_not_alter_fingerprint_for_an_unused_platform() {
        let root = temp_dir("romm-unrelated-cache-change");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        // The plan's only record is NES; SNES is never referenced by any
        // record in this plan at all.
        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let manifest = empty_manifest(&view.id, &destination);

        let mut cache_a = identity_cache_with_romm_slug("NES", "nes-from-cache-unused-tier");
        cache_a.platforms.push(
            crate::identity_source::romm::normalise::NormalisedPlatform {
                provider_platform_id: None,
                provider_slug: "snes".to_string(),
                provider_name: None,
                canonical: Some("SNES".to_string()),
                rom_count: None,
            },
        );
        let mut cache_b = cache_a.clone();
        // Only the SNES entry (never touched by this plan, and shadowed by
        // the explicit NES override anyway) changes between the two caches.
        cache_b
            .platforms
            .retain(|platform| platform.canonical.as_deref() != Some("SNES"));
        cache_b.platforms.push(
            crate::identity_source::romm::normalise::NormalisedPlatform {
                provider_platform_id: None,
                provider_slug: "super-nintendo".to_string(),
                provider_name: None,
                canonical: Some("SNES".to_string()),
                rom_count: None,
            },
        );

        let plan_a = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_a),
        );
        let plan_b = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_b),
        );

        assert_eq!(
            plan_a.profile_fingerprint, plan_b.profile_fingerprint,
            "a cache change for a platform this plan never resolves must not affect the \
             fingerprint"
        );
    }

    /// Test 6: the resolved-mapping component of the fingerprint reflects
    /// only the actual *resolved* output string, normalised the same way
    /// regardless of which tier produced it - not, say, provider metadata
    /// that happens to differ between two caches that both resolve to the
    /// same slug. Holds the declared profile/policy identical between the
    /// two plans (neither has an explicit override) so the only variable is
    /// the resolved mapping itself, not a confounding config difference.
    #[test]
    fn romm_fingerprint_reflects_resolved_output_regardless_of_origin_tier() {
        let root = temp_dir("romm-fingerprint-resolved-output");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");
        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let mut view = make_view("view-romm", &destination, vec![], vec![]);
        view.profile.kind = FrontendProfileKind::Romm;
        let manifest = empty_manifest(&view.id, &destination);

        // Two different caches that both resolve NES to the same final
        // "nes" slug, but via different provider metadata (id/name/rom
        // count) - none of which is part of the resolved-mapping
        // representation that gets hashed.
        let cache_a = identity_cache_with_romm_slug("NES", "nes");
        let mut cache_b = cache_a.clone();
        cache_b.platforms[0].provider_platform_id = Some("different-id".to_string());
        cache_b.platforms[0].provider_name = Some("A Different Display Name".to_string());
        cache_b.platforms[0].rom_count = Some(9999);
        cache_b.server_id = "a-totally-different-server".to_string();

        let plan_a = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_a),
        );
        let plan_b = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_b),
        );

        assert_eq!(
            plan_a.profile_fingerprint, plan_b.profile_fingerprint,
            "the same resolved slug must fingerprint identically regardless of unrelated \
             provider metadata differing between the two caches that produced it"
        );

        // An explicit user override resolving to that exact same slug must
        // *also* still be a real, distinct configuration - it legitimately
        // fingerprints differently from "no override, resolved via cache",
        // because the declared profile itself differs (a real override is
        // present). This is not a contradiction: the resolved-mapping
        // component alone is tier-agnostic (proven above); the full
        // fingerprint also covers the declared profile, which correctly
        // still distinguishes "explicitly pinned" from "resolved from the
        // environment".
        let via_override = romm_view_with_override("view-romm", &destination, "NES", "nes");
        let plan_via_override = plan_library_view(
            &via_override,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        assert_ne!(
            plan_a.profile_fingerprint,
            plan_via_override.profile_fingerprint
        );

        // But a different final slug (still via the cache tier, profile
        // held constant) must fingerprint differently from `plan_a`.
        let cache_different_slug = identity_cache_with_romm_slug("NES", "different-slug");
        let plan_different = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache_different_slug),
        );
        assert_ne!(
            plan_a.profile_fingerprint,
            plan_different.profile_fingerprint
        );
    }

    /// Test 7: a `Generic` profile's fingerprint is unaffected by the
    /// presence or contents of a RomM identity cache - it never consults
    /// one at all.
    #[test]
    fn generic_profile_fingerprint_unaffected_by_romm_cache_presence_or_contents() {
        let root = temp_dir("generic-fingerprint-ignores-romm-cache");
        let source_dir = root.join("source");
        let archive_path = source_dir.join("Game.zip");
        write_file(&archive_path, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &source_dir);
        let archive = make_archive(1, 1, &archive_path, Some("NES"));
        let view = make_view("view-generic", &destination, vec![], vec![]);
        assert_eq!(view.profile.kind, FrontendProfileKind::Generic);
        let manifest = empty_manifest(&view.id, &destination);
        let cache = identity_cache_with_romm_slug("NES", "nes-should-never-matter-here");

        let plan_without_cache = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let plan_with_cache = plan_library_view(
            &view,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&source),
            &manifest,
            Some(&cache),
        );

        assert_eq!(
            plan_without_cache.profile_fingerprint, plan_with_cache.profile_fingerprint,
            "a Generic profile must never be influenced by a RomM identity cache"
        );
        // And it must equal exactly what the pre-Stage-3 algorithm already
        // produced for this view, proving no existing Generic fingerprint is
        // invalidated by this addition.
        assert_eq!(
            plan_without_cache.profile_fingerprint,
            compute_view_profile_fingerprint(&view)
        );
    }

    // -------------------------------------------------------------------
    // Source-folder filtering regression suite (2026-08-17 smoke-test
    // incident: a view scoped to `/mnt/games/roms/sms` appeared to plan
    // archives from `/mnt/local/downloads/jdownloader2/output`). Root cause
    // was a stale cached GUI plan, not this planner - these tests pin down
    // that `plan_library_view`'s own source-folder filtering is, and stays,
    // correct in isolation, independent of the GUI fix.
    // -------------------------------------------------------------------

    /// Test A: a view whose `source_folders` names only source A must never
    /// plan (as *any* action - Create, Skip, or Collision) an archive that
    /// belongs to a different, unselected source B, even when B is present
    /// in the same catalogue read and has archives with the exact same
    /// platform/filename shape.
    #[test]
    fn selected_source_never_plans_an_archive_from_an_unselected_source() {
        let root = temp_dir("source-filter-a-never-b");
        let source_a_dir = root.join("games/roms/sms");
        let source_b_dir = root.join("local/downloads/jdownloader2/output");
        let archive_a = source_a_dir.join("Alex Kidd.zip");
        let archive_b = source_b_dir.join("Agatha Christie.zip");
        write_file(&archive_a, b"sms-bytes");
        write_file(&archive_b, b"jdownloader-bytes");
        let destination = root.join("dest");

        let source_a = make_source(9, &source_a_dir);
        let source_b = make_source(1, &source_b_dir);
        // Archive B has no assigned platform - exactly the
        // `SkipUnknownPlatform` shape the incident reported - so this test
        // also proves an unselected source's unknown-platform archives are
        // not merely re-bucketed as Skip, they are not planned *at all*.
        let record_a = make_archive(1, 9, &archive_a, Some("MasterSystem"));
        let record_b = make_archive(2, 1, &archive_b, None);

        let view = make_view(
            "view-1",
            &destination,
            vec![source_a_dir.clone()],
            vec!["MasterSystem".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_a, record_b],
            &[source_a, source_b],
            &manifest,
            None,
        );

        assert_eq!(
            plan.entries.len(),
            1,
            "an unselected source's archive must produce zero plan entries of any kind"
        );
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(plan.entries[0].archive_path, Some(archive_a));
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.archive_path.as_deref() != Some(archive_b.as_path())),
            "source B's archive must never appear in the plan, in any action bucket"
        );
        assert_eq!(plan.counts.skip, 0);
        assert_eq!(plan.counts.create, 1);
    }

    /// Test B: a configured source folder is matched against the
    /// catalogue's `SourceFolderRecord` by its registered identity (exact
    /// `path` string, via the `source_folder_id` a record actually belongs
    /// to) - never by textual path *containment*. A newly registered,
    /// narrower source folder that happens to be a sub-path of an older,
    /// already-scanned, broader source folder does not "inherit" that
    /// broader source's archives - this is exactly the incident's
    /// environment shape (a fresh, unscanned `/mnt/games/roms/sms`
    /// registration sitting underneath an already-scanned
    /// `/mnt/games/roms`): the view must see zero entries from the broader
    /// source, not a leaked subset of it.
    #[test]
    fn selected_source_path_resolves_by_registered_identity_not_containment() {
        let root = temp_dir("source-filter-containment");
        let broad_dir = root.join("games/roms");
        let narrow_dir = broad_dir.join("sms");
        let archive_under_broad = narrow_dir.join("Alex Kidd.zip");
        write_file(&archive_under_broad, b"sms-bytes");
        let destination = root.join("dest");

        // The archive is catalogued under the *broad* source's id (5, as in
        // the real incident), even though it physically lives under what is
        // now also a separately-registered narrower source (9). The
        // catalogue ties every record to the source_folder_id it was
        // scanned under - never re-derived from the path at plan time.
        let broad_source = make_source(5, &broad_dir);
        let narrow_source = make_source(9, &narrow_dir);
        let record = make_archive(1, 5, &archive_under_broad, Some("MasterSystem"));

        let view = make_view(
            "view-1",
            &destination,
            vec![narrow_dir.clone()],
            vec!["MasterSystem".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record],
            &[broad_source, narrow_source],
            &manifest,
            None,
        );

        assert!(
            plan.entries.is_empty(),
            "a view scoped to the narrow (unscanned) source must not see the broad source's \
             archives just because the narrow path is textually inside the broad one - \
             got {:?}",
            plan.entries
        );
        assert_eq!(plan.counts.create, 0);
    }

    /// Test C: editing a view's selected source (A -> B) replaces the
    /// effective filter outright - the new plan reflects only B, never a
    /// union of A and B, and never still-A.
    #[test]
    fn editing_selected_source_replaces_the_effective_filter() {
        let root = temp_dir("source-filter-edit-replaces");
        let source_a_dir = root.join("source-a");
        let source_b_dir = root.join("source-b");
        let archive_a = source_a_dir.join("Game.zip");
        let archive_b = source_b_dir.join("Game.zip");
        write_file(&archive_a, b"a");
        write_file(&archive_b, b"b");
        let destination = root.join("dest");

        let source_a = make_source(1, &source_a_dir);
        let source_b = make_source(2, &source_b_dir);
        let record_a = make_archive(1, 1, &archive_a, Some("NES"));
        let record_b = make_archive(2, 2, &archive_b, Some("NES"));
        let records = [record_a, record_b];
        let sources = [source_a, source_b];

        let mut view = make_view("view-1", &destination, vec![source_a_dir.clone()], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan_before = plan_library_view(&view, &records, &sources, &manifest, None);
        assert_eq!(plan_before.entries.len(), 1);
        assert_eq!(plan_before.entries[0].archive_path, Some(archive_a.clone()));

        // The edit: source_folders now names B instead of A.
        view.source_folders = vec![source_b_dir.clone()];
        let plan_after = plan_library_view(&view, &records, &sources, &manifest, None);
        assert_eq!(plan_after.entries.len(), 1);
        assert_eq!(plan_after.entries[0].archive_path, Some(archive_b));
    }

    /// Test D: a configured source folder that matches no registered
    /// `SourceFolderRecord` at all (e.g. it was removed, or was never a
    /// real source) must never be treated as "no filter" / "all sources" -
    /// it must produce zero entries, the same as any other selection that
    /// matches nothing.
    #[test]
    fn unresolvable_selected_source_never_degrades_to_all_sources() {
        let root = temp_dir("source-filter-unresolvable");
        let known_dir = root.join("known-source");
        let archive = known_dir.join("Game.zip");
        write_file(&archive, b"bytes");
        let destination = root.join("dest");

        let known_source = make_source(1, &known_dir);
        let record = make_archive(1, 1, &archive, Some("NES"));

        let unresolvable_path = root.join("this-path-was-never-registered-as-a-source");
        let view = make_view("view-1", &destination, vec![unresolvable_path], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[record], &[known_source], &manifest, None);

        assert!(
            plan.entries.is_empty(),
            "an unresolvable source selection must plan nothing, never silently fall back to \
             every configured source - got {:?}",
            plan.entries
        );
    }

    /// Test E: the documented "empty `source_folders` means every
    /// configured source is included" semantics still hold exactly - not
    /// affected by the source-identity matching used when the list is
    /// non-empty.
    #[test]
    fn empty_source_folders_still_means_every_configured_source() {
        let root = temp_dir("source-filter-empty-means-all");
        let source_a_dir = root.join("source-a");
        let source_b_dir = root.join("source-b");
        let archive_a = source_a_dir.join("A.zip");
        let archive_b = source_b_dir.join("B.zip");
        write_file(&archive_a, b"a");
        write_file(&archive_b, b"b");
        let destination = root.join("dest");

        let source_a = make_source(1, &source_a_dir);
        let source_b = make_source(2, &source_b_dir);
        let record_a = make_archive(1, 1, &archive_a, Some("NES"));
        let record_b = make_archive(2, 2, &archive_b, Some("NES"));

        // No source_folders selected at all.
        let view = make_view("view-1", &destination, vec![], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_a, record_b],
            &[source_a, source_b],
            &manifest,
            None,
        );

        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.counts.create, 2);
    }

    /// Test G: `Generic` and `Romm` apply byte-identical source-folder
    /// filtering - the profile kind only changes the *output path shape*
    /// for an already-selected archive, never which archives are selected
    /// in the first place.
    #[test]
    fn generic_and_romm_profiles_apply_identical_source_filtering() {
        let root = temp_dir("source-filter-generic-vs-romm");
        let source_a_dir = root.join("source-a");
        let source_b_dir = root.join("source-b");
        let archive_a = source_a_dir.join("Game.zip");
        let archive_b = source_b_dir.join("Other.zip");
        write_file(&archive_a, b"a");
        write_file(&archive_b, b"b");
        let destination = root.join("dest");

        let source_a = make_source(1, &source_a_dir);
        let source_b = make_source(2, &source_b_dir);
        let record_a = make_archive(1, 1, &archive_a, Some("NES"));
        let record_b = make_archive(2, 2, &archive_b, Some("NES"));
        let records = [record_a, record_b];
        let sources = [source_a, source_b];

        let generic_view = make_view("view-1", &destination, vec![source_a_dir.clone()], vec![]);
        let manifest = empty_manifest(&generic_view.id, &destination);
        let generic_plan = plan_library_view(&generic_view, &records, &sources, &manifest, None);

        let mut romm_view = romm_view_with_override("view-1", &destination, "NES", "nes");
        romm_view.source_folders = vec![source_a_dir.clone()];
        let romm_plan = plan_library_view(&romm_view, &records, &sources, &manifest, None);

        // Same archives selected (only A's), same skip/collision shape -
        // only the destination shape differs (Generic's `{platform}/{filename}`
        // vs RomM's `roms/{slug}/{filename}`).
        assert_eq!(generic_plan.entries.len(), 1);
        assert_eq!(romm_plan.entries.len(), 1);
        assert_eq!(
            generic_plan.entries[0].archive_path,
            Some(archive_a.clone())
        );
        assert_eq!(romm_plan.entries[0].archive_path, Some(archive_a));
        assert_eq!(
            generic_plan.entries[0].action,
            LibraryViewPlanAction::Create
        );
        assert_eq!(romm_plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(generic_plan.counts.create, romm_plan.counts.create);
    }

    /// Test H: source filtering and platform filtering compose correctly -
    /// an archive is only planned when it matches *both*; matching only one
    /// of the two must silently exclude it (never a reportable Skip, per
    /// the existing "ordinary filter" semantics), and an unselected
    /// source's archive is excluded regardless of platform, even an
    /// otherwise-matching one.
    #[test]
    fn source_and_platform_filters_compose_correctly() {
        let root = temp_dir("source-and-platform-compose");
        let selected_dir = root.join("selected-source");
        let other_dir = root.join("other-source");
        let matching = selected_dir.join("Matches.zip");
        let wrong_platform = selected_dir.join("WrongPlatform.zip");
        let wrong_source = other_dir.join("WrongSource.zip");
        write_file(&matching, b"a");
        write_file(&wrong_platform, b"b");
        write_file(&wrong_source, b"c");
        let destination = root.join("dest");

        let selected_source = make_source(1, &selected_dir);
        let other_source = make_source(2, &other_dir);
        let record_matching = make_archive(1, 1, &matching, Some("MasterSystem"));
        let record_wrong_platform = make_archive(2, 1, &wrong_platform, Some("SNES"));
        // Same platform as the target, but from the unselected source.
        let record_wrong_source = make_archive(3, 2, &wrong_source, Some("MasterSystem"));

        let view = make_view(
            "view-1",
            &destination,
            vec![selected_dir.clone()],
            vec!["MasterSystem".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_matching, record_wrong_platform, record_wrong_source],
            &[selected_source, other_source],
            &manifest,
            None,
        );

        assert_eq!(
            plan.entries.len(),
            1,
            "only the archive matching both filters is planned"
        );
        assert_eq!(plan.entries[0].archive_path, Some(matching));
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(plan.counts.create, 1);
        assert_eq!(plan.counts.skip, 0);
    }

    // -------------------------------------------------------------------
    // Loose game-media (2026-08-17 C128 smoke case): once the catalogue
    // contains a `.d64`/`.g64` `PersistedArchive` row (see
    // `archive_kind` in `lib.rs`, which is what actually gained the new
    // recognition - nothing here changed), `plan_library_view` needs zero
    // changes to plan it: it already never reads `archive_kind` at all,
    // only `platform`/`source_folder_id`/`absolute_path`/
    // `last_verified_missing_at`. These tests prove that directly, using
    // the same `make_archive` fixture helper every other test already
    // uses, just with a `.d64`/`.g64` filename instead of `.zip`.
    // -------------------------------------------------------------------

    /// Test 4: a view restricted to the loose-media source must never plan
    /// an archive belonging to a different, unselected source - identical
    /// isolation guarantee as archive-backed media (see
    /// `selected_source_never_plans_an_archive_from_an_unselected_source`).
    #[test]
    fn loose_media_selected_source_isolation_matches_archive_backed_behavior() {
        let root = temp_dir("loose-media-source-isolation");
        let c128_dir = root.join("c128");
        let other_dir = root.join("other-source");
        let d64 = c128_dir.join("BurgerWhop!.d64");
        let other_zip = other_dir.join("Unrelated.zip");
        write_file(&d64, b"d64-bytes");
        write_file(&other_zip, b"zip-bytes");
        let destination = root.join("dest");

        let c128_source = make_source(1, &c128_dir);
        let other_source = make_source(2, &other_dir);
        let d64_record = make_archive(1, 1, &d64, Some("Commodore 128"));
        let other_record = make_archive(2, 2, &other_zip, Some("Commodore 128"));

        let view = make_view(
            "view-1",
            &destination,
            vec![c128_dir.clone()],
            vec!["Commodore 128".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[d64_record, other_record],
            &[c128_source, other_source],
            &manifest,
            None,
        );

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].archive_path, Some(d64));
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
    }

    /// Test 5: platform filtering composes with a loose-media candidate
    /// exactly as it does for an archive-backed one.
    #[test]
    fn loose_media_platform_filter_composition_matches_archive_backed_behavior() {
        let root = temp_dir("loose-media-platform-filter");
        let c128_dir = root.join("c128");
        let matching = c128_dir.join("BurgerWhop!.d64");
        let wrong_platform = c128_dir.join("SomeAmiga.adf");
        write_file(&matching, b"a");
        write_file(&wrong_platform, b"b");
        let destination = root.join("dest");

        let source = make_source(1, &c128_dir);
        let record_matching = make_archive(1, 1, &matching, Some("Commodore 128"));
        let record_wrong_platform = make_archive(2, 1, &wrong_platform, Some("Amiga"));

        let view = make_view(
            "view-1",
            &destination,
            vec![c128_dir.clone()],
            vec!["Commodore 128".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            &[record_matching, record_wrong_platform],
            &[source],
            &manifest,
            None,
        );

        assert_eq!(
            plan.entries.len(),
            1,
            "the wrong-platform loose file is silently excluded"
        );
        assert_eq!(plan.entries[0].archive_path, Some(matching));
        assert_eq!(plan.counts.create, 1);
        assert_eq!(plan.counts.skip, 0);
    }

    /// Test 6: a loose-media archive the catalogue's last scan reported
    /// missing must be `SkipMissingSourceArchive`, never `Create` -
    /// identical rule to an archive-backed file.
    #[test]
    fn loose_media_missing_source_file_is_reported_not_created() {
        let root = temp_dir("loose-media-missing");
        let c128_dir = root.join("c128");
        let g64 = c128_dir.join("Ultima V (Disk 1).g64");
        write_file(&g64, b"g64-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &c128_dir);
        let mut record = make_archive(1, 1, &g64, Some("Commodore 128"));
        record.last_verified_missing_at = Some("2026-08-17T00:00:00Z".to_string());

        let view = make_view("view-1", &destination, vec![c128_dir], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[record], &[source], &manifest, None);

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(
            plan.entries[0].action,
            LibraryViewPlanAction::SkipMissingSourceArchive
        );
        assert_eq!(
            plan.counts.create, 0,
            "a missing loose-media source file must never be Create"
        );
    }

    /// Test 7: archive-backed (`.zip`) planning behaviour is byte-for-byte
    /// unchanged - a mixed catalogue of loose media and zip archives plans
    /// each according to the exact same rules, with no cross-influence.
    #[test]
    fn archive_backed_behavior_is_unchanged_alongside_loose_media() {
        let root = temp_dir("mixed-loose-and-archive");
        let c128_dir = root.join("c128");
        let d64 = c128_dir.join("BurgerWhop!.d64");
        let zip = c128_dir.join("SomeGame.zip");
        write_file(&d64, b"d64-bytes");
        write_file(&zip, b"zip-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &c128_dir);
        let d64_record = make_archive(1, 1, &d64, Some("Commodore 128"));
        let zip_record = make_archive(2, 1, &zip, Some("Commodore 128"));

        let view = make_view(
            "view-1",
            &destination,
            vec![c128_dir],
            vec!["Commodore 128".to_string()],
        );
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(&view, &[d64_record, zip_record], &[source], &manifest, None);

        assert_eq!(plan.entries.len(), 2);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.action == LibraryViewPlanAction::Create)
        );
        assert_eq!(plan.counts.create, 2);
    }

    /// Test 8: planning a loose-media catalogue is exactly as deterministic
    /// as an archive-backed one - repeated calls, and calls with reordered
    /// input, produce identical plan vectors.
    #[test]
    fn loose_media_planning_is_deterministic() {
        let root = temp_dir("loose-media-deterministic");
        let c128_dir = root.join("c128");
        let mut records = Vec::new();
        for (index, name) in [
            "BurgerWhop!.d64",
            "Ultima V (Disk 1).g64",
            "Ultima V (Disk 2).g64",
            "Yahtzee.d64",
        ]
        .into_iter()
        .enumerate()
        {
            let path = c128_dir.join(name);
            write_file(&path, b"bytes");
            records.push(make_archive(
                index as i64 + 1,
                1,
                &path,
                Some("Commodore 128"),
            ));
        }
        let destination = root.join("dest");
        let source = make_source(1, &c128_dir);
        let view = make_view("view-1", &destination, vec![c128_dir], vec![]);
        let manifest = empty_manifest(&view.id, &destination);

        let plan_a = plan_library_view(
            &view,
            &records,
            std::slice::from_ref(&source),
            &manifest,
            None,
        );
        let mut reversed = records.clone();
        reversed.reverse();
        let plan_b = plan_library_view(&view, &reversed, &[source], &manifest, None);

        assert_eq!(plan_a.entries, plan_b.entries);
        let keys: Vec<PathBuf> = plan_a
            .entries
            .iter()
            .map(|entry| entry.relative_link_path.clone().unwrap_or_default())
            .collect();
        let mut sorted_keys = keys.clone();
        sorted_keys.sort();
        assert_eq!(keys, sorted_keys);
    }

    /// Test 9: the RomM profile plans a real `roms/<slug>/<filename>` path
    /// for a loose-media candidate exactly as it does for an archive-backed
    /// one - the planner never renamed/wrapped/copied the `.d64` file, and
    /// slug resolution follows the same override -> cache -> fail-closed
    /// precedence.
    #[test]
    fn romm_profile_plans_a_real_path_for_a_loose_media_candidate() {
        let root = temp_dir("loose-media-romm-path");
        let c128_dir = root.join("c128");
        let d64 = c128_dir.join("BurgerWhop!.d64");
        write_file(&d64, b"d64-bytes");
        let destination = root.join("dest");

        let source = make_source(1, &c128_dir);
        let record = make_archive(1, 1, &d64, Some("Commodore 128"));
        let view = romm_view_with_override("view-1", &destination, "Commodore 128", "c128");
        let manifest = empty_manifest(&view.id, &destination);

        let plan = plan_library_view(
            &view,
            std::slice::from_ref(&record),
            std::slice::from_ref(&source),
            &manifest,
            None,
        );

        assert_eq!(plan.profile_error, None);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].action, LibraryViewPlanAction::Create);
        assert_eq!(
            plan.entries[0].relative_link_path,
            Some(PathBuf::from("roms/c128/BurgerWhop!.d64"))
        );
        // The source file itself is untouched - no rename, no wrapper.
        assert_eq!(fs::read(&d64).unwrap(), b"d64-bytes");
    }
}
