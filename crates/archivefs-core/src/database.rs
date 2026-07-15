//! Persistent library catalogue database foundation.
//!
//! Stage 1: resolving the default database path, opening/creating the
//! SQLite database, applying forward-only migrations, and reporting
//! structured health.
//!
//! Stage 2 (this addition): persisting `ArchiveScanner` results into the
//! schema - registering source folders, recording scan runs, upserting
//! archives, logging observations, and tracking platform assignments with
//! provenance. Still not called from any mount, unmount, CLI, or GUI code
//! path - only [`scan_and_persist`] (and its own tests) ever calls into
//! the scanner and this module together. See `docs/DATABASE_DESIGN.md` and
//! `docs/adr/0001-persistent-library-database.md` for the design this
//! implements and why mount/unmount safety never depends on it.
//!
//! `rusqlite::Connection` (and `rusqlite::Transaction`) are intentionally
//! never exposed outside this module. Every other part of the crate that
//! eventually needs the database goes through [`Database`]'s narrow API,
//! the standalone [`check_database_health`] report, or [`scan_and_persist`].
//!
//! This module uses `std::os::unix::ffi` to store and read back exact path
//! bytes, so it - like the rest of this Linux-first project - is Unix-only.

use std::collections::HashSet;
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    Archive, ArchiveFsError, ArchiveKind, ArchiveScanner, Config, PlatformProvenance, Result,
    canonical_platform_names, normalize_path_segment,
};

/// Resolves the default library database path:
/// `~/.local/share/archivefs/library.sqlite3`, alongside the existing JSON
/// index (`default_index_path`) in the same XDG data directory. Performs no
/// filesystem I/O and creates nothing - resolving the path and creating the
/// database are deliberately separate operations (see [`Database::open_or_create`]).
pub fn default_database_path() -> Result<PathBuf> {
    resolve_database_path(env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")))
}

/// The logic behind [`default_database_path`], taking the already-resolved
/// `HOME`/`USERPROFILE` value as a parameter instead of reading the
/// environment itself. This lets tests exercise the "home directory is
/// unresolved" case by passing `None` directly, rather than mutating real
/// process environment variables (which would race with other tests
/// running in parallel in the same process).
fn resolve_database_path(home: Option<OsString>) -> Result<PathBuf> {
    let home = home.ok_or_else(|| ArchiveFsError::Database("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("archivefs")
        .join("library.sqlite3"))
}

/// One forward-only schema migration. Applied in ascending `version` order;
/// there are no down-migrations.
struct Migration {
    version: i64,
    description: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "initial schema: schema_migrations, source_folders, archives, scan_runs, archive_scan_observations, platform_assignments",
        sql: include_str!("migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        description: "add platform_aliases: persistent custom folder-name -> canonical platform mappings",
        sql: include_str!("migrations/0002_platform_aliases.sql"),
    },
];

fn latest_known_version(migrations: &[Migration]) -> i64 {
    migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0)
}

/// An open connection to the ArchiveFS library database, with all pending
/// migrations already applied. The inner `rusqlite::Connection` is private;
/// nothing outside this module touches it directly.
#[derive(Debug)]
pub struct Database {
    connection: Connection,
    path: PathBuf,
}

impl Database {
    /// Opens the database at `path`, creating the file and its parent
    /// directory if needed, and applying any pending migrations inside
    /// transactions. Fails clearly, without mutating the file, if its
    /// schema version is newer than this build of ArchiveFS understands.
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|source| ArchiveFsError::io(parent.to_path_buf(), source))?;
        }

        let mut connection = open_connection(path)?;
        apply_migrations(&mut connection, MIGRATIONS)?;

        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
    }

    /// Opens the database at [`default_database_path`], creating it (and
    /// its parent directory) if needed.
    pub fn open_or_create_default() -> Result<Self> {
        Self::open_or_create(default_database_path()?)
    }

    /// The path this database was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current schema version (`PRAGMA user_version`), which equals
    /// the highest applied migration's version.
    pub fn schema_version(&self) -> Result<i64> {
        schema_version(&self.connection)
    }

    /// True if `PRAGMA foreign_keys` reports enabled on this connection.
    /// `open_connection` always enables it, so this should only ever
    /// report `false` if the SQLite build in use cannot honor it - the
    /// value is read back rather than assumed, per the design's "enable
    /// and verify" requirement.
    pub fn foreign_keys_enabled(&self) -> Result<bool> {
        foreign_keys_enabled(&self.connection)
    }

    /// Closes the database, surfacing any error SQLite reports on close
    /// (for example an unfinalized statement) instead of silently
    /// dropping it. Simply letting a `Database` go out of scope is also
    /// always safe by normal Rust ownership - this exists only for
    /// callers that want to observe a close-time error explicitly.
    pub fn close(self) -> Result<()> {
        self.connection
            .close()
            .map_err(|(_, error)| ArchiveFsError::Database(error.to_string()))
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).map_err(|error| {
        ArchiveFsError::Database(format!("failed to open {}: {error}", path.display()))
    })?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| {
            ArchiveFsError::Database(format!("failed to enable foreign keys: {error}"))
        })?;
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<i64> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| {
            ArchiveFsError::Database(format!("failed to read schema version: {error}"))
        })
}

fn foreign_keys_enabled(connection: &Connection) -> Result<bool> {
    connection
        .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
        .map(|value: i64| value != 0)
        .map_err(|error| {
            ArchiveFsError::Database(format!("failed to read foreign_keys pragma: {error}"))
        })
}

/// Applies every migration in `migrations` with a version greater than the
/// database's current `PRAGMA user_version`, in order. Each migration's DDL
/// and its `schema_migrations` bookkeeping row are written in one
/// transaction, committed only after both succeed - so a migration that
/// fails partway through (bad SQL, or the bookkeeping insert itself
/// failing) leaves the database exactly as it was before that migration
/// started, via the transaction's automatic rollback on drop. A database
/// whose recorded version is higher than any migration this build knows
/// about is rejected before anything is touched.
fn apply_migrations(connection: &mut Connection, migrations: &[Migration]) -> Result<()> {
    let current_version = schema_version(connection)?;
    let target_version = latest_known_version(migrations);

    if current_version > target_version {
        return Err(ArchiveFsError::Database(format!(
            "database schema version {current_version} is newer than the highest version \
             ({target_version}) this build of ArchiveFS understands; refusing to open it \
             automatically"
        )));
    }

    for migration in migrations {
        if migration.version <= current_version {
            continue;
        }

        let tx = connection.transaction().map_err(|error| {
            ArchiveFsError::Database(format!("failed to start migration transaction: {error}"))
        })?;

        tx.execute_batch(migration.sql).map_err(|error| {
            ArchiveFsError::Database(format!(
                "migration {} ({}) failed: {error}",
                migration.version, migration.description
            ))
        })?;

        tx.execute(
            "INSERT INTO schema_migrations (version, description, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![migration.version, migration.description, now_utc_string()],
        )
        .map_err(|error| {
            ArchiveFsError::Database(format!(
                "failed to record migration {}: {error}",
                migration.version
            ))
        })?;

        tx.pragma_update(None, "user_version", migration.version)
            .map_err(|error| {
                ArchiveFsError::Database(format!(
                    "failed to update schema version to {}: {error}",
                    migration.version
                ))
            })?;

        tx.commit().map_err(|error| {
            ArchiveFsError::Database(format!(
                "failed to commit migration {}: {error}",
                migration.version
            ))
        })?;
    }

    Ok(())
}

/// A structured, non-panicking report on the database at a given path.
/// Never mutates the database: if it does not exist yet, that is
/// reported, not created; if migrations are pending, that is reported,
/// not applied. Field names deliberately stay technical (this is core,
/// not a GUI layer) - `error` carries whatever went wrong as plain text
/// for a caller to display however it likes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseHealth {
    pub resolved_path: PathBuf,
    pub database_exists: bool,
    pub database_opens: bool,
    pub schema_version: Option<i64>,
    pub migrations_current: bool,
    pub foreign_keys_enabled: bool,
    pub error: Option<String>,
}

/// Reports on the database at `path` without mutating it in any way.
pub fn check_database_health(path: impl AsRef<Path>) -> DatabaseHealth {
    let path = path.as_ref().to_path_buf();

    if !path.exists() {
        return DatabaseHealth {
            resolved_path: path,
            database_exists: false,
            database_opens: false,
            schema_version: None,
            migrations_current: false,
            foreign_keys_enabled: false,
            error: None,
        };
    }

    match open_connection(&path) {
        Ok(connection) => {
            // Connection::open is lazy - SQLite does not validate the file
            // header until the first real read, so a corrupt or
            // non-database file still opens successfully here and only
            // fails once schema_version below actually reads page 1. That
            // failure must not be silently swallowed (as `.ok()` alone
            // would do): a corrupt database and a database that merely has
            // pending migrations both leave `schema_version` as `None`/
            // stale, and a caller cannot tell them apart without this
            // error being carried through.
            let schema_version_result = schema_version(&connection);
            let schema_version_value = schema_version_result.as_ref().ok().copied();
            let foreign_keys_value = foreign_keys_enabled(&connection).unwrap_or(false);
            let migrations_current = schema_version_value == Some(latest_known_version(MIGRATIONS));

            DatabaseHealth {
                resolved_path: path,
                database_exists: true,
                database_opens: true,
                schema_version: schema_version_value,
                migrations_current,
                foreign_keys_enabled: foreign_keys_value,
                error: schema_version_result.err().map(|error| error.to_string()),
            }
        }
        Err(error) => DatabaseHealth {
            resolved_path: path,
            database_exists: true,
            database_opens: false,
            schema_version: None,
            migrations_current: false,
            foreign_keys_enabled: false,
            error: Some(error.to_string()),
        },
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn now_utc_string() -> String {
    format_unix_timestamp_utc(now_unix_seconds())
}

/// Formats a Unix timestamp (seconds since epoch, UTC) as
/// `YYYY-MM-DDTHH:MM:SSZ`. Hand-rolled instead of adding a date/time
/// dependency: this only ever needs UTC (no timezone conversion, no
/// locale handling), and this project already prefers small hand-written
/// parsing/formatting over a general-purpose crate for a narrow,
/// well-defined need (see the config parser in `lib.rs`).
fn format_unix_timestamp_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts a day count since the Unix epoch (1970-01-01) into a
/// `(year, month, day)` proleptic Gregorian calendar date. Adapted from
/// Howard Hinnant's public-domain `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

// ---------------------------------------------------------------------
// Stage 2: archive scan persistence.
// ---------------------------------------------------------------------

/// A `source_folders` row after [`Database::register_source_folders`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredSourceFolder {
    pub id: i64,
    pub path: PathBuf,
}

/// What happened to one `archives` row during [`Database::upsert_archive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveChangeKind {
    /// No existing row for this `(source_folder_id, relative_path)`.
    New,
    /// An existing row was previously marked missing and has been seen
    /// again; its `last_verified_missing_at` is cleared.
    Restored,
    /// An existing, not-missing row was seen with the same size and
    /// modified time as last recorded.
    Unchanged,
    /// An existing, not-missing row was seen with a different size or
    /// modified time; any previously computed hash columns are cleared,
    /// since a hash computed against the old content no longer applies.
    Changed,
}

/// The result of one [`Database::upsert_archive`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveUpsertOutcome {
    pub archive_id: i64,
    pub change: ArchiveChangeKind,
}

/// One `archive_scan_observations.observation` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveObservationKind {
    Added,
    Unchanged,
    Changed,
    Missing,
    Restored,
}

impl ArchiveObservationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
            Self::Missing => "missing",
            Self::Restored => "restored",
        }
    }
}

impl From<ArchiveChangeKind> for ArchiveObservationKind {
    fn from(change: ArchiveChangeKind) -> Self {
        match change {
            ArchiveChangeKind::New => Self::Added,
            ArchiveChangeKind::Restored => Self::Restored,
            ArchiveChangeKind::Unchanged => Self::Unchanged,
            ArchiveChangeKind::Changed => Self::Changed,
        }
    }
}

/// Counters for one `scan_runs` row, filled in as a scan proceeds and
/// written by [`Database::complete_scan_run`].
///
/// `archives_updated` is the sum of `archives_changed` and
/// `archives_restored` - it is what actually gets written to the
/// `scan_runs.archives_updated` column (that column predates this finer
/// breakdown; splitting it out here needed no schema change, since these
/// are plain Rust counters computed fresh each run, not columns of their
/// own). `archives_unchanged` is likewise not persisted to any column
/// today - it exists so callers (the CLI's `library-scan`) can report it
/// without a schema change either.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanRunCounts {
    pub source_folders_scanned: i64,
    pub archives_seen: i64,
    pub archives_added: i64,
    pub archives_changed: i64,
    pub archives_restored: i64,
    pub archives_unchanged: i64,
    pub archives_updated: i64,
    pub archives_missing: i64,
    pub errors_count: i64,
}

/// The outcome of one [`scan_and_persist`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPersistSummary {
    pub scan_run_id: i64,
    pub counts: ScanRunCounts,
    /// `(source folder path, error)` for every source folder whose scan
    /// attempt failed this run. A non-empty list here does not mean the
    /// whole run failed - other source folders may have scanned
    /// successfully - but archives under a listed folder are guaranteed
    /// untouched by this run (see [`scan_and_persist`]).
    pub folder_errors: Vec<(PathBuf, String)>,
}

/// A persisted `archives` row joined with its current platform (if any),
/// reconstructed with exact path bytes. Used by tests today; the natural
/// read path for future CLI/GUI callers once they are wired up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedArchive {
    pub id: i64,
    pub source_folder_id: i64,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub archive_kind: String,
    pub display_name: String,
    pub normalized_name: String,
    pub size_bytes: Option<u64>,
    pub modified_time_unix_seconds: Option<i64>,
    /// The archive's current platform assignment, if any (`NULL` in the
    /// database - i.e. `None` here - represents "unknown", not a
    /// sentinel string; see `platform_assignments` in the migration).
    pub platform: Option<String>,
    /// The current platform assignment's provenance
    /// (`platform_assignments.source`: `"heuristic-path-detector"`,
    /// `"folder_alias"`, or [`MANUAL_PLATFORM_SOURCE`]), if any. `None`
    /// exactly when `platform` is `None`.
    pub platform_source: Option<String>,
    pub last_known_health: String,
    pub last_verified_missing_at: Option<String>,
}

/// Whether `archive`'s *effective* platform (manual assignment if one is
/// active, otherwise the latest automatic detection - see
/// `provenance_priority`) is unknown: there is no current platform
/// assignment at all. The single canonical definition of "unknown"
/// shared by every caller (CLI `library-list --unknown-only`, the GUI's
/// unknown-platform count/filter) - never re-derived from display text
/// like the literal string `"Unknown"`, and never computed from raw
/// automatic-detection fields directly, so a manual assignment is never
/// misclassified as unknown just because automatic detection found
/// nothing (`archive.platform` already reflects the outcome of that
/// precedence, not the automatic guess alone).
pub fn persisted_archive_has_unknown_platform(archive: &PersistedArchive) -> bool {
    archive.platform.is_none()
}

/// The result of one [`Database::set_manual_platform`] or
/// [`Database::clear_manual_platform`] call: the platform assignment
/// immediately before and after, so callers (CLI/GUI) can show exactly
/// what changed - old platform, new platform, and provenance - without a
/// second query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformAssignmentChange {
    pub old_platform: Option<String>,
    pub old_source: Option<String>,
    pub new_platform: Option<String>,
    pub new_source: Option<String>,
}

/// One persisted custom platform folder alias (`platform_aliases` table -
/// see [`Database::add_platform_alias`]). `alias` is the user's original
/// typed text (trimmed), kept only for display; matching and uniqueness
/// always go through `normalized_alias`. `platform` is always a
/// canonical name from `canonical_platform_names()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlatformAlias {
    pub id: i64,
    pub alias: String,
    pub normalized_alias: String,
    pub platform: String,
    pub created_at: String,
    pub updated_at: String,
}

fn platform_alias_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlatformAlias> {
    Ok(PlatformAlias {
        id: row.get(0)?,
        alias: row.get(1)?,
        normalized_alias: row.get(2)?,
        platform: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn split_assignment(assignment: Option<(String, String)>) -> (Option<String>, Option<String>) {
    match assignment {
        Some((platform, source)) => (Some(platform), Some(source)),
        None => (None, None),
    }
}

fn archive_kind_str(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Zip => "zip",
        ArchiveKind::SevenZip => "sevenzip",
        ArchiveKind::Rar => "rar",
    }
}

fn system_time_to_unix_seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn db_error(context: &str, error: rusqlite::Error) -> ArchiveFsError {
    ArchiveFsError::Database(format!("{context}: {error}"))
}

/// The `platform_assignments.source` value for a manual (user-chosen)
/// platform assignment - the highest-priority provenance recognised by
/// [`provenance_priority`], so it can never be silently overwritten by a
/// later scan's automatic detection. Set by [`Database::set_manual_platform`]
/// and retired (without a replacement) by [`Database::clear_manual_platform`].
pub const MANUAL_PLATFORM_SOURCE: &str = "manual";

/// The `platform_assignments.source` value for a match against a
/// persisted, user-defined `platform_aliases` row - see
/// [`Database::add_platform_alias`]. Ranked above the built-in
/// `"folder_alias"` table and the filename/path heuristic (a deliberate
/// user-configured mapping is more specific than either), but below
/// [`MANUAL_PLATFORM_SOURCE`] (a single archive's explicit override still
/// wins) - see [`provenance_priority`].
pub const CUSTOM_FOLDER_ALIAS_SOURCE: &str = "custom_folder_alias";

/// Relative strength of a `platform_assignments.source` value, used by
/// [`Database::assign_platform`] to decide whether a new assignment may
/// replace the current one, in four tiers (weakest to strongest):
///
/// 1. `"folder_alias"` (the generic, code-shipped folder-name fallback in
///    `crate::detect_platform_with_provenance`) - weakest, since it is
///    the least specific signal.
/// 2. Everything else this build does not specifically recognize,
///    including `"heuristic-path-detector"` - the "automatic detection"
///    tier. Two assignments from this tier can still freely replace each
///    other (matching the existing heuristic-vs-heuristic reassignment
///    behavior from before more tiers existed).
/// 3. [`CUSTOM_FOLDER_ALIAS_SOURCE`] - a persisted, user-defined folder
///    alias. Outranks both the built-in folder alias table and the
///    filename/path heuristic (required precedence: manual > custom
///    alias > heuristic > built-in alias), but never a manual assignment.
///    Unlike the two tiers below it, this source's evidence can change
///    out from under a scan (an alias can be added, edited, or removed
///    between scans) - see [`Database::retire_stale_custom_alias_assignment`]
///    for how a stale current assignment sourced from a since-removed
///    alias is un-stuck, which `provenance_priority` alone cannot do.
/// 4. [`MANUAL_PLATFORM_SOURCE`] - strongest: a deliberate user choice.
///    Nothing in tiers 1-3 can ever silently replace it; only another
///    manual assignment (via [`Database::set_manual_platform`]) can.
fn provenance_priority(source: &str) -> u8 {
    match source {
        "folder_alias" => 0,
        CUSTOM_FOLDER_ALIAS_SOURCE => 2,
        MANUAL_PLATFORM_SOURCE => 3,
        _ => 1,
    }
}

/// The subset of an existing `archives` row read by
/// [`Database::upsert_archive`] to decide which of [`ArchiveChangeKind`]'s
/// four outcomes applies.
struct ExistingArchiveRow {
    archive_id: i64,
    size_bytes: Option<i64>,
    modified_time_unix_seconds: Option<i64>,
    last_verified_missing_at: Option<String>,
}

impl Database {
    /// Registers every path in `source_folders` (typically
    /// `Config.source_folders`) as an active, currently-configured source
    /// folder: inserting rows that do not exist yet, and refreshing
    /// `last_seen_in_config_at` (and clearing any stale
    /// `removed_from_config_at`) for ones that already do. Any
    /// previously-registered source folder *not* present in
    /// `source_folders` is marked `removed_from_config_at` if it is not
    /// already - its archives and their history are left untouched, only
    /// excluded from future scans. Returns the id assigned to each
    /// currently-configured path, in the same order, for the caller to
    /// associate discovered archives with.
    pub fn register_source_folders(
        &mut self,
        source_folders: &[PathBuf],
    ) -> Result<Vec<RegisteredSourceFolder>> {
        let now = now_utc_string();
        let tx = self.connection.transaction().map_err(|error| {
            db_error("failed to start register_source_folders transaction", error)
        })?;
        let mut registered = Vec::with_capacity(source_folders.len());

        for path in source_folders {
            let path_bytes = path.as_os_str().as_bytes();
            let existing_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM source_folders WHERE path = ?1",
                    params![path_bytes],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| db_error("failed to look up source folder", error))?;

            let id = match existing_id {
                Some(id) => {
                    tx.execute(
                        "UPDATE source_folders SET last_seen_in_config_at = ?2, removed_from_config_at = NULL WHERE id = ?1",
                        params![id, now],
                    )
                    .map_err(|error| db_error("failed to refresh source folder", error))?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO source_folders (path, first_seen_at, last_seen_in_config_at) VALUES (?1, ?2, ?2)",
                        params![path_bytes, now],
                    )
                    .map_err(|error| db_error("failed to insert source folder", error))?;
                    tx.last_insert_rowid()
                }
            };
            registered.push(RegisteredSourceFolder {
                id,
                path: path.clone(),
            });
        }

        let configured: Vec<&[u8]> = source_folders
            .iter()
            .map(|path| path.as_os_str().as_bytes())
            .collect();
        let mut still_active_stmt = tx
            .prepare("SELECT id, path FROM source_folders WHERE removed_from_config_at IS NULL")
            .map_err(|error| db_error("failed to prepare source folder scan", error))?;
        let still_active: Vec<(i64, Vec<u8>)> = still_active_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|error| db_error("failed to list active source folders", error))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|error| db_error("failed to read active source folders", error))?;
        drop(still_active_stmt);

        for (id, path_bytes) in still_active {
            if !configured
                .iter()
                .any(|configured| **configured == *path_bytes)
            {
                tx.execute(
                    "UPDATE source_folders SET removed_from_config_at = ?2 WHERE id = ?1",
                    params![id, now],
                )
                .map_err(|error| db_error("failed to mark source folder removed", error))?;
            }
        }

        tx.commit()
            .map_err(|error| db_error("failed to commit register_source_folders", error))?;
        Ok(registered)
    }

    /// Starts a new `scan_runs` row with `status = 'running'` and returns
    /// its id. Committed immediately in its own transaction, so it is
    /// durably visible even if the process dies moments later - this is
    /// what makes [`Database::mark_interrupted_scan_runs`] able to detect
    /// a scan that never finished.
    pub fn start_scan_run(
        &mut self,
        triggered_by: &str,
        config_content_digest: Option<[u8; 32]>,
    ) -> Result<i64> {
        self.connection
            .execute(
                "INSERT INTO scan_runs (started_at, triggered_by, config_content_digest, status) VALUES (?1, ?2, ?3, 'running')",
                params![
                    now_utc_string(),
                    triggered_by,
                    config_content_digest.map(|digest| digest.to_vec())
                ],
            )
            .map_err(|error| db_error("failed to start scan run", error))?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Marks every `scan_runs` row still `status = 'running'` as
    /// `'interrupted'`. Scanning is synchronous and single-process today,
    /// so any such row at the start of a new process can only be left
    /// over from a previous process that did not exit cleanly. Returns
    /// how many rows were fixed.
    pub fn mark_interrupted_scan_runs(&mut self) -> Result<usize> {
        self.connection
            .execute(
                "UPDATE scan_runs SET status = 'interrupted', finished_at = ?1 WHERE status = 'running'",
                params![now_utc_string()],
            )
            .map_err(|error| db_error("failed to mark interrupted scan runs", error))
    }

    /// Inserts or updates the `archives` row for `archive`, identified by
    /// `(source_folder_id, relative_path)` - `relative_path` is computed
    /// here as `archive.path` stripped of `source_folder_path`'s prefix,
    /// preserving exact path bytes throughout (no lossy UTF-8 conversion
    /// at any point). See [`ArchiveChangeKind`] for what each outcome
    /// means.
    pub fn upsert_archive(
        &mut self,
        source_folder_id: i64,
        source_folder_path: &Path,
        archive: &Archive,
    ) -> Result<ArchiveUpsertOutcome> {
        let relative_path = archive.path.strip_prefix(source_folder_path).map_err(|_| {
            ArchiveFsError::Database(format!(
                "{} is not under source folder {}",
                archive.path.display(),
                source_folder_path.display()
            ))
        })?;
        let relative_path_bytes = relative_path.as_os_str().as_bytes();
        let absolute_path_bytes = archive.path.as_os_str().as_bytes();
        let file_name_bytes = archive
            .path
            .file_name()
            .map(|name| name.as_bytes())
            .unwrap_or_default();
        let archive_kind = archive_kind_str(archive.kind);
        let size_bytes = archive.identity.size_bytes.map(|value| value as i64);
        let modified_time_unix_seconds = archive
            .identity
            .modified_time
            .and_then(system_time_to_unix_seconds);
        let now = now_utc_string();

        let tx = self
            .connection
            .transaction()
            .map_err(|error| db_error("failed to start upsert_archive transaction", error))?;

        let existing: Option<ExistingArchiveRow> = tx
            .query_row(
                "SELECT id, size_bytes, modified_time_unix_seconds, last_verified_missing_at \
                 FROM archives WHERE source_folder_id = ?1 AND relative_path = ?2",
                params![source_folder_id, relative_path_bytes],
                |row| {
                    Ok(ExistingArchiveRow {
                        archive_id: row.get(0)?,
                        size_bytes: row.get(1)?,
                        modified_time_unix_seconds: row.get(2)?,
                        last_verified_missing_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|error| db_error("failed to look up archive", error))?;

        let outcome = match existing {
            None => {
                tx.execute(
                    "INSERT INTO archives (\
                         source_folder_id, relative_path, absolute_path_cached, file_name_cached, \
                         archive_kind, display_name, normalized_name, size_bytes, \
                         modified_time_unix_seconds, first_seen_at, last_seen_at, created_at, updated_at\
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10, ?10)",
                    params![
                        source_folder_id,
                        relative_path_bytes,
                        absolute_path_bytes,
                        file_name_bytes,
                        archive_kind,
                        archive.identity.display_name,
                        archive.identity.normalized_name,
                        size_bytes,
                        modified_time_unix_seconds,
                        now,
                    ],
                )
                .map_err(|error| db_error("failed to insert archive", error))?;
                ArchiveUpsertOutcome {
                    archive_id: tx.last_insert_rowid(),
                    change: ArchiveChangeKind::New,
                }
            }
            Some(ExistingArchiveRow {
                archive_id,
                size_bytes: old_size,
                modified_time_unix_seconds: old_modified_time,
                last_verified_missing_at,
            }) => {
                if last_verified_missing_at.is_some() {
                    tx.execute(
                        "UPDATE archives SET absolute_path_cached = ?2, file_name_cached = ?3, \
                         size_bytes = ?4, modified_time_unix_seconds = ?5, \
                         last_verified_missing_at = NULL, last_seen_at = ?6, updated_at = ?6 \
                         WHERE id = ?1",
                        params![
                            archive_id,
                            absolute_path_bytes,
                            file_name_bytes,
                            size_bytes,
                            modified_time_unix_seconds,
                            now,
                        ],
                    )
                    .map_err(|error| db_error("failed to restore archive", error))?;
                    ArchiveUpsertOutcome {
                        archive_id,
                        change: ArchiveChangeKind::Restored,
                    }
                } else if old_size != size_bytes || old_modified_time != modified_time_unix_seconds
                {
                    tx.execute(
                        "UPDATE archives SET absolute_path_cached = ?2, file_name_cached = ?3, \
                         size_bytes = ?4, modified_time_unix_seconds = ?5, content_hash = NULL, \
                         archive_hash = NULL, internal_listing_hash = NULL, last_seen_at = ?6, \
                         updated_at = ?6 WHERE id = ?1",
                        params![
                            archive_id,
                            absolute_path_bytes,
                            file_name_bytes,
                            size_bytes,
                            modified_time_unix_seconds,
                            now,
                        ],
                    )
                    .map_err(|error| db_error("failed to update changed archive", error))?;
                    ArchiveUpsertOutcome {
                        archive_id,
                        change: ArchiveChangeKind::Changed,
                    }
                } else {
                    tx.execute(
                        "UPDATE archives SET last_seen_at = ?2, updated_at = ?2 WHERE id = ?1",
                        params![archive_id, now],
                    )
                    .map_err(|error| db_error("failed to touch unchanged archive", error))?;
                    ArchiveUpsertOutcome {
                        archive_id,
                        change: ArchiveChangeKind::Unchanged,
                    }
                }
            }
        };

        tx.commit()
            .map_err(|error| db_error("failed to commit upsert_archive", error))?;
        Ok(outcome)
    }

    /// Appends one row to the append-only `archive_scan_observations` log.
    pub fn record_observation(
        &mut self,
        scan_run_id: i64,
        archive_id: i64,
        observation: ArchiveObservationKind,
        size_bytes: Option<u64>,
        modified_time_unix_seconds: Option<i64>,
    ) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO archive_scan_observations (\
                     scan_run_id, archive_id, observation, size_bytes, modified_time_unix_seconds, observed_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    scan_run_id,
                    archive_id,
                    observation.as_str(),
                    size_bytes.map(|value| value as i64),
                    modified_time_unix_seconds,
                    now_utc_string(),
                ],
            )
            .map_err(|error| db_error("failed to record observation", error))?;
        Ok(())
    }

    /// Records `platform` as the current platform assignment for
    /// `archive_id`, with `source` as its provenance (for example
    /// `"heuristic-path-detector"` or `"folder_alias"`, matching
    /// `detect_platform_with_provenance`'s `PlatformProvenance::as_source_str`
    /// output). `platform: None` (no platform detected) is a no-op - it
    /// never overwrites or removes an existing assignment, since a scan
    /// not detecting a platform this time is not evidence a previous
    /// detection was wrong. If `platform` already matches the current
    /// assignment, this is also a no-op, to avoid growing history with
    /// every scan re-confirming the same guess.
    ///
    /// A new assignment whose `source` is *weaker* than the current
    /// assignment's `source` (see [`provenance_priority`]'s three tiers)
    /// never becomes current: a `"folder_alias"` guess can never replace
    /// an existing `"heuristic-path-detector"` or [`MANUAL_PLATFORM_SOURCE`]
    /// assignment, and neither `"folder_alias"` nor
    /// `"heuristic-path-detector"` can ever replace a manual assignment -
    /// only [`Self::set_manual_platform`] can knowingly replace one
    /// manual choice with another.
    ///
    /// While the current assignment is manual specifically, any automatic
    /// result (`source` is not [`MANUAL_PLATFORM_SOURCE`]) is always
    /// *recorded*, as a non-current row, rather than silently discarded -
    /// checked first, unconditionally, before the "unchanged platform" or
    /// priority checks below, so this applies even when `platform`
    /// happens to already equal the current manual platform's text.
    /// Discarding it instead would lose the true latest automatic
    /// detection, and [`Self::clear_manual_platform`] would restore a
    /// stale pre-manual result instead of it once the manual override is
    /// removed. Recording is itself a no-op if it exactly matches the
    /// most recent existing automatic row (by row id, not `assigned_at` -
    /// a stable, collision-proof tiebreaker even when two rows share a
    /// timestamp), so repeated scans reporting the same unchanged
    /// automatic result never grow history. This shadow row is inserted
    /// with `is_current = 0` directly - it never becomes current while
    /// manual is active, even momentarily. Blocked results that lose to a
    /// *non-manual* current assignment (`"folder_alias"` losing to
    /// `"heuristic-path-detector"`, for example) are still a plain no-op:
    /// nothing currently reads shadowed history for that case, and
    /// recording every losing automatic guess there would grow history
    /// unboundedly for no consumer.
    ///
    /// If `platform` already matches the current assignment, this is a
    /// no-op, to avoid growing history with every scan re-confirming the
    /// same guess. Otherwise (the new source is not weaker than the
    /// current one) the previous current row (if any) is flipped to
    /// not-current and a new row is inserted, preserving full history.
    /// This method is what every scan calls (via [`scan_and_persist`]) -
    /// a deliberate user override goes through
    /// [`Self::set_manual_platform`]/[`Self::clear_manual_platform`]
    /// instead, never this one directly.
    pub fn assign_platform(
        &mut self,
        archive_id: i64,
        platform: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let Some(platform) = platform else {
            return Ok(());
        };

        let current: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT platform, source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                params![archive_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| db_error("failed to look up current platform assignment", error))?;

        if let Some((current_platform, current_source)) = &current {
            // Checked first, and unconditionally (even if `platform`
            // happens to equal `current_platform`): while manual is
            // current, any automatic result must be shadow-recorded so
            // `clear_manual_platform` always exposes the true latest
            // automatic state, never a stale pre-manual one - including
            // the case where the latest automatic result now
            // coincidentally matches the manual platform's text, which
            // would otherwise hit the "no-op if unchanged" check below
            // and never get recorded at all.
            if current_source == MANUAL_PLATFORM_SOURCE && source != MANUAL_PLATFORM_SOURCE {
                return self.record_shadow_automatic_assignment(archive_id, platform, source);
            }
            if current_platform == platform {
                return Ok(());
            }
            if provenance_priority(current_source) > provenance_priority(source) {
                return Ok(());
            }
        }

        let tx = self
            .connection
            .transaction()
            .map_err(|error| db_error("failed to start assign_platform transaction", error))?;
        tx.execute(
            "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
            params![archive_id],
        )
        .map_err(|error| db_error("failed to retire previous platform assignment", error))?;
        tx.execute(
            "INSERT INTO platform_assignments (archive_id, platform, source, is_current, assigned_at) \
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![archive_id, platform, source, now_utc_string()],
        )
        .map_err(|error| db_error("failed to insert platform assignment", error))?;
        tx.commit()
            .map_err(|error| db_error("failed to commit assign_platform", error))?;
        Ok(())
    }

    /// Records `platform`/`source` as a non-current historical row for
    /// `archive_id` - the "remember the true latest automatic result even
    /// though a manual assignment currently outranks it" step of
    /// [`Self::assign_platform`]. A no-op if it exactly matches the most
    /// recent existing non-manual row already (ordered by row id, the
    /// stable tiebreaker), so this never grows history for an unchanged
    /// repeated scan result. Never touches `is_current` for any row - the
    /// currently-active manual assignment is left completely alone.
    fn record_shadow_automatic_assignment(
        &mut self,
        archive_id: i64,
        platform: &str,
        source: &str,
    ) -> Result<()> {
        let latest_automatic: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT platform, source FROM platform_assignments \
                 WHERE archive_id = ?1 AND source != ?2 \
                 ORDER BY id DESC LIMIT 1",
                params![archive_id, MANUAL_PLATFORM_SOURCE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| {
                db_error(
                    "failed to look up latest shadow automatic assignment",
                    error,
                )
            })?;
        if latest_automatic
            .as_ref()
            .is_some_and(|(latest_platform, latest_source)| {
                latest_platform == platform && latest_source == source
            })
        {
            return Ok(());
        }

        self.connection
            .execute(
                "INSERT INTO platform_assignments (archive_id, platform, source, is_current, assigned_at) \
                 VALUES (?1, ?2, ?3, 0, ?4)",
                params![archive_id, platform, source, now_utc_string()],
            )
            .map_err(|error| db_error("failed to record shadow automatic assignment", error))?;
        Ok(())
    }

    /// Reads `archive_id`'s current (`is_current = 1`) platform assignment,
    /// if any - the shared read step behind [`Self::set_manual_platform`]
    /// and [`Self::clear_manual_platform`].
    fn current_platform_assignment(&self, archive_id: i64) -> Result<Option<(String, String)>> {
        self.connection
            .query_row(
                "SELECT platform, source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                params![archive_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| db_error("failed to look up current platform assignment", error))
    }

    /// Looks up the current `archives.id` for the archive whose
    /// `absolute_path_cached` exactly matches `path` (exact bytes - never
    /// a lossy display string; see `Path::as_os_str`/`OsStrExt::as_bytes`).
    /// `absolute_path_cached` is not itself the identity SQLite enforces
    /// uniqueness on (that is `(source_folder_id, relative_path)` - see
    /// the migration), but is derived from it and kept in sync by every
    /// [`Self::upsert_archive`] call, so in practice it is unique too;
    /// this still defensively errors rather than guessing if more than
    /// one row somehow matches, instead of silently acting on the wrong
    /// archive. `Ok(None)` means no persisted archive has this path -
    /// not yet scanned into the library database, for example.
    pub fn find_archive_id_by_absolute_path(&self, path: &Path) -> Result<Option<i64>> {
        let path_bytes = path.as_os_str().as_bytes();
        let mut stmt = self
            .connection
            .prepare("SELECT id FROM archives WHERE absolute_path_cached = ?1")
            .map_err(|error| db_error("failed to prepare archive path lookup", error))?;
        let ids: Vec<i64> = stmt
            .query_map(params![path_bytes], |row| row.get(0))
            .map_err(|error| db_error("failed to look up archive by path", error))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|error| db_error("failed to read archive path lookup", error))?;

        match ids.len() {
            0 => Ok(None),
            1 => Ok(Some(ids[0])),
            _ => Err(ArchiveFsError::Database(format!(
                "{} matches more than one archive row - this indicates a data inconsistency",
                path.display()
            ))),
        }
    }

    /// Sets `platform` as a manual, user-chosen platform assignment for
    /// `archive_id` - the highest-priority provenance
    /// ([`MANUAL_PLATFORM_SOURCE`], see [`provenance_priority`]), so a
    /// later scan's automatic detection (`"heuristic-path-detector"` or
    /// `"folder_alias"`, via [`Self::assign_platform`]) can never
    /// silently overwrite it. Unlike `assign_platform`, this is a direct,
    /// deliberate user action: it is never blocked by provenance priority
    /// (manual is already the highest tier, and this call is itself how a
    /// user replaces one manual choice with another) - it only no-ops
    /// when `platform` already matches the current manual assignment
    /// exactly, to avoid growing history with a redundant re-confirmation.
    ///
    /// Returns the assignment immediately before and after this call, so
    /// callers (CLI/GUI) can show exactly what changed without a second
    /// query.
    ///
    /// Rejects `platform` if it is empty or whitespace-only (after
    /// trimming) - the exact stored spelling is not otherwise normalized
    /// (`--custom`/free-text callers get exactly what they typed), but an
    /// effectively-blank value is never a meaningful manual choice and
    /// would render as indistinguishable from "no assignment" everywhere
    /// this is displayed. Enforced here, not just in the CLI/GUI layers
    /// that currently call this, so every future caller gets the same
    /// guarantee for free.
    pub fn set_manual_platform(
        &mut self,
        archive_id: i64,
        platform: &str,
    ) -> Result<PlatformAssignmentChange> {
        if platform.trim().is_empty() {
            return Err(ArchiveFsError::Database(
                "manual platform must not be empty or whitespace-only".to_string(),
            ));
        }

        let current = self.current_platform_assignment(archive_id)?;
        let (old_platform, old_source) = split_assignment(current);

        if old_platform.as_deref() == Some(platform)
            && old_source.as_deref() == Some(MANUAL_PLATFORM_SOURCE)
        {
            return Ok(PlatformAssignmentChange {
                old_platform: old_platform.clone(),
                old_source: old_source.clone(),
                new_platform: old_platform,
                new_source: old_source,
            });
        }

        let tx = self
            .connection
            .transaction()
            .map_err(|error| db_error("failed to start set_manual_platform transaction", error))?;
        tx.execute(
            "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
            params![archive_id],
        )
        .map_err(|error| db_error("failed to retire previous platform assignment", error))?;
        tx.execute(
            "INSERT INTO platform_assignments (archive_id, platform, source, is_current, assigned_at) \
             VALUES (?1, ?2, ?3, 1, ?4)",
            params![archive_id, platform, MANUAL_PLATFORM_SOURCE, now_utc_string()],
        )
        .map_err(|error| db_error("failed to insert manual platform assignment", error))?;
        tx.commit()
            .map_err(|error| db_error("failed to commit set_manual_platform", error))?;

        Ok(PlatformAssignmentChange {
            old_platform,
            old_source,
            new_platform: Some(platform.to_string()),
            new_source: Some(MANUAL_PLATFORM_SOURCE.to_string()),
        })
    }

    /// Clears a manual platform assignment for `archive_id`: retires the
    /// current (manual) row and, in the same transaction, immediately
    /// restores the most recent automatic assignment still in history
    /// (any row whose `source` is not [`MANUAL_PLATFORM_SOURCE`]) as
    /// current again - the exact same historical row, not a fresh
    /// duplicate, so history does not grow just from toggling manual on
    /// and off. This is what makes the latest automatic result available
    /// right away, without waiting for the next scan: a scan is only
    /// ever needed to *discover a change*, never to make an
    /// already-known automatic result current again.
    ///
    /// If no automatic assignment was ever recorded for this archive,
    /// there is nothing to restore - it becomes current-less (`Unknown`)
    /// until the next scan finds one, exactly as before.
    ///
    /// A no-op if there is no current assignment, or the current
    /// assignment is not manual: this never touches an assignment made a
    /// different way, mirroring `assign_platform`'s "never silently
    /// overwrite or remove a non-matching provenance" philosophy. In
    /// both the no-op and the actual-clear case, the returned
    /// [`PlatformAssignmentChange`] reflects exactly what happened -
    /// compare `old_source` to `Some(MANUAL_PLATFORM_SOURCE)` to tell
    /// them apart.
    pub fn clear_manual_platform(&mut self, archive_id: i64) -> Result<PlatformAssignmentChange> {
        let current = self.current_platform_assignment(archive_id)?;
        let (old_platform, old_source) = split_assignment(current);

        if old_source.as_deref() != Some(MANUAL_PLATFORM_SOURCE) {
            return Ok(PlatformAssignmentChange {
                old_platform: old_platform.clone(),
                old_source: old_source.clone(),
                new_platform: old_platform,
                new_source: old_source,
            });
        }

        let latest_automatic: Option<(i64, String, String)> = self
            .connection
            .query_row(
                "SELECT id, platform, source FROM platform_assignments \
                 WHERE archive_id = ?1 AND source != ?2 \
                 ORDER BY id DESC LIMIT 1",
                params![archive_id, MANUAL_PLATFORM_SOURCE],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| {
                db_error(
                    "failed to look up latest automatic platform assignment",
                    error,
                )
            })?;

        let tx = self.connection.transaction().map_err(|error| {
            db_error("failed to start clear_manual_platform transaction", error)
        })?;
        tx.execute(
            "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
            params![archive_id],
        )
        .map_err(|error| db_error("failed to clear manual platform assignment", error))?;

        let (new_platform, new_source) = match &latest_automatic {
            Some((row_id, platform, source)) => {
                tx.execute(
                    "UPDATE platform_assignments SET is_current = 1 WHERE id = ?1",
                    params![row_id],
                )
                .map_err(|error| {
                    db_error(
                        "failed to restore latest automatic platform assignment",
                        error,
                    )
                })?;
                (Some(platform.clone()), Some(source.clone()))
            }
            None => (None, None),
        };
        tx.commit()
            .map_err(|error| db_error("failed to commit clear_manual_platform", error))?;

        Ok(PlatformAssignmentChange {
            old_platform,
            old_source,
            new_platform,
            new_source,
        })
    }

    /// Adds a persistent custom platform folder alias: `alias` is a
    /// single folder name (never a path), and `platform` must
    /// case-insensitively match one of [`canonical_platform_names`] - the
    /// canonical spelling is what actually gets stored, never the
    /// caller's casing, matching `library-set-platform`'s existing
    /// canonical-matching convention. Unlike manual platform assignment,
    /// there is no free-form `--custom` escape hatch here: a mistyped
    /// alias platform would silently misclassify every archive under a
    /// matching folder, so this first version keeps aliases limited to
    /// known platforms.
    ///
    /// `alias` is normalized with [`normalize_path_segment`] - the same
    /// ASCII-alphanumeric-only, lowercased normalization the built-in
    /// folder alias table already uses, so `"GC"`, `"gc"`, `"g-c"`, and
    /// `"g_c"` all key to the same stored alias. Rejected (returning
    /// `Err`, nothing written) if `alias` contains a `/` (a folder alias
    /// must name exactly one folder, never a path) or normalizes to an
    /// empty string (for example `"---"` or whitespace-only input).
    ///
    /// Deterministic duplicate behaviour: if an alias already exists with
    /// the same *normalized* form, this call returns a clear `Err`
    /// rather than silently overwriting it - a caller who wants to
    /// change an existing alias's platform must
    /// [`Self::remove_platform_alias`] it first, then add the
    /// replacement. This is the CLI-visible contract
    /// (`platform-alias-add` on an already-known alias is a clear
    /// duplicate error, never a silent update).
    ///
    /// Never triggers a rescan - see `crate::database::scan_and_persist`
    /// for where a newly added alias actually takes effect.
    pub fn add_platform_alias(&mut self, alias: &str, platform: &str) -> Result<PlatformAlias> {
        if alias.contains('/') {
            return Err(ArchiveFsError::Database(
                "platform alias must be a single folder name, not a path (it must not contain '/')"
                    .to_string(),
            ));
        }

        let normalized_alias = normalize_path_segment(alias);
        if normalized_alias.is_empty() {
            return Err(ArchiveFsError::Database(
                "platform alias must contain at least one letter or digit".to_string(),
            ));
        }

        let canonical_platform = canonical_platform_names()
            .into_iter()
            .find(|canonical| canonical.eq_ignore_ascii_case(platform))
            .ok_or_else(|| {
                ArchiveFsError::Database(format!(
                    "'{platform}' is not a known platform; known platforms: {}",
                    canonical_platform_names().join(", ")
                ))
            })?;

        let already_exists: bool = self
            .connection
            .query_row(
                "SELECT 1 FROM platform_aliases WHERE normalized_alias = ?1",
                params![normalized_alias],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| db_error("failed to look up existing platform alias", error))?
            .is_some();
        if already_exists {
            return Err(ArchiveFsError::Database(format!(
                "a platform alias for '{normalized_alias}' already exists; remove it first \
                 (remove_platform_alias/platform-alias-remove) before adding a different mapping"
            )));
        }

        let display_alias = alias.trim();
        let now = now_utc_string();
        self.connection
            .execute(
                "INSERT INTO platform_aliases (alias, normalized_alias, platform, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![display_alias, normalized_alias, canonical_platform, now],
            )
            .map_err(|error| db_error("failed to insert platform alias", error))?;

        self.read_platform_alias(self.connection.last_insert_rowid())
    }

    fn read_platform_alias(&self, id: i64) -> Result<PlatformAlias> {
        self.connection
            .query_row(
                "SELECT id, alias, normalized_alias, platform, created_at, updated_at \
                 FROM platform_aliases WHERE id = ?1",
                params![id],
                platform_alias_from_row,
            )
            .map_err(|error| db_error("failed to read back platform alias", error))
    }

    /// Every persisted custom platform alias, ordered by `normalized_alias`
    /// for a stable, deterministic listing - SQLite does not otherwise
    /// guarantee row order without an explicit `ORDER BY`.
    pub fn list_platform_aliases(&self) -> Result<Vec<PlatformAlias>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id, alias, normalized_alias, platform, created_at, updated_at \
                 FROM platform_aliases ORDER BY normalized_alias",
            )
            .map_err(|error| db_error("failed to prepare platform alias listing", error))?;
        stmt.query_map([], platform_alias_from_row)
            .map_err(|error| db_error("failed to list platform aliases", error))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| db_error("failed to read platform alias rows", error))
    }

    /// Removes the custom platform alias whose normalized form exactly
    /// matches `alias` (run through the same [`normalize_path_segment`]
    /// normalization [`Self::add_platform_alias`] uses). Returns whether
    /// a row was actually removed: `Ok(false)` for "no such alias" is not
    /// an error - matching [`Self::clear_manual_platform`]'s "no-op, not
    /// a failure" treatment of an already-absent state - so a caller
    /// wanting a hard error for "unknown alias" (the CLI, for a clear
    /// user-facing message) checks the returned bool itself.
    ///
    /// Never triggers a rescan: any archive whose current assignment came
    /// from this alias keeps that assignment until a later scan
    /// recomputes it (see `crate::database::scan_and_persist` and
    /// [`Self::retire_stale_custom_alias_assignment`]).
    pub fn remove_platform_alias(&mut self, alias: &str) -> Result<bool> {
        let normalized_alias = normalize_path_segment(alias);
        let removed = self
            .connection
            .execute(
                "DELETE FROM platform_aliases WHERE normalized_alias = ?1",
                params![normalized_alias],
            )
            .map_err(|error| db_error("failed to remove platform alias", error))?;
        Ok(removed > 0)
    }

    /// Looks up the canonical platform for one already-lossy-stringified
    /// folder name (a single path component, never a full path) against
    /// the persisted custom alias table, normalizing with the same
    /// [`normalize_path_segment`] [`Self::add_platform_alias`] and the
    /// built-in folder alias table both use. `None` means no custom
    /// alias matches this component - callers still need to fall back
    /// through the existing heuristic/built-in-alias tiers themselves
    /// (see [`provenance_priority`]). Never mutates, and never triggers a
    /// scan.
    pub fn lookup_custom_platform_alias(&self, folder_component: &str) -> Result<Option<String>> {
        let normalized = normalize_path_segment(folder_component);
        self.connection
            .query_row(
                "SELECT platform FROM platform_aliases WHERE normalized_alias = ?1",
                params![normalized],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| db_error("failed to look up custom platform alias", error))
    }

    /// Un-sticks a stale [`CUSTOM_FOLDER_ALIAS_SOURCE`] current
    /// assignment whose alias no longer matches this scan. Called by
    /// `persist_one_folder` immediately before [`Self::assign_platform`],
    /// only when this scan's custom-alias lookup for the archive found
    /// nothing.
    ///
    /// `assign_platform`'s general provenance-priority blocking (a
    /// lower-tier automatic source can never silently replace a
    /// higher-tier one - see [`provenance_priority`]) is correct for the
    /// built-in `"folder_alias"`/`"heuristic-path-detector"` tiers, which
    /// are derived from fixed, code-shipped tables that cannot themselves
    /// change between scans - a scan finding a different (or no) result
    /// there really is just noise, not evidence the previous detection
    /// was wrong. A [`CUSTOM_FOLDER_ALIAS_SOURCE`] result is different:
    /// it is derived from user-editable, removable data, so "this scan's
    /// custom-alias lookup no longer matches" is real evidence the
    /// previous result is stale.
    ///
    /// If the archive's current assignment is [`CUSTOM_FOLDER_ALIAS_SOURCE`],
    /// retires that current row (without inserting a replacement), so the
    /// immediately-following `assign_platform` call sees no current
    /// assignment and freely sets this scan's heuristic/built-in-alias/
    /// Unknown result instead - including Unknown, if nothing else
    /// matches either, since `assign_platform` never itself clears a
    /// current assignment down to nothing. A no-op for every other
    /// current source, including manual: this never touches a manual
    /// assignment - shadow-recording the latest automatic fallback while
    /// manual is active is handled entirely by `assign_platform`'s
    /// existing logic, unaffected by this method.
    fn retire_stale_custom_alias_assignment(&mut self, archive_id: i64) -> Result<()> {
        let current = self.current_platform_assignment(archive_id)?;
        if current.as_ref().map(|(_, source)| source.as_str()) != Some(CUSTOM_FOLDER_ALIAS_SOURCE) {
            return Ok(());
        }
        self.connection
            .execute(
                "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
                params![archive_id],
            )
            .map_err(|error| {
                db_error(
                    "failed to retire stale custom platform alias assignment",
                    error,
                )
            })?;
        Ok(())
    }

    /// Marks every `archives` row under `source_folder_id` that is not in
    /// `seen_archive_ids` and not already missing as missing
    /// (`last_verified_missing_at` set to now), recording a `missing`
    /// observation for each. Callers must only invoke this for a source
    /// folder whose scan attempt this run fully succeeded - never after a
    /// failed or partial scan of that folder, and never for a folder that
    /// was not scanned at all this run (see [`scan_and_persist`], which
    /// enforces this). Returns how many archives were marked missing.
    pub fn mark_unseen_archives_missing(
        &mut self,
        scan_run_id: i64,
        source_folder_id: i64,
        seen_archive_ids: &[i64],
    ) -> Result<i64> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT id FROM archives WHERE source_folder_id = ?1 AND last_verified_missing_at IS NULL",
            )
            .map_err(|error| db_error("failed to prepare missing-archive scan", error))?;
        let candidates: Vec<i64> = stmt
            .query_map(params![source_folder_id], |row| row.get(0))
            .map_err(|error| db_error("failed to list archives for missing check", error))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|error| db_error("failed to read archives for missing check", error))?;
        drop(stmt);

        let seen: HashSet<i64> = seen_archive_ids.iter().copied().collect();
        let missing: Vec<i64> = candidates
            .into_iter()
            .filter(|id| !seen.contains(id))
            .collect();
        if missing.is_empty() {
            return Ok(0);
        }

        let now = now_utc_string();
        let tx = self
            .connection
            .transaction()
            .map_err(|error| db_error("failed to start mark-missing transaction", error))?;
        for archive_id in &missing {
            tx.execute(
                "UPDATE archives SET last_verified_missing_at = ?2, updated_at = ?2 WHERE id = ?1",
                params![archive_id, now],
            )
            .map_err(|error| db_error("failed to mark archive missing", error))?;
            tx.execute(
                "INSERT INTO archive_scan_observations (scan_run_id, archive_id, observation, observed_at) \
                 VALUES (?1, ?2, 'missing', ?3)",
                params![scan_run_id, archive_id, now],
            )
            .map_err(|error| db_error("failed to record missing observation", error))?;
        }
        tx.commit()
            .map_err(|error| db_error("failed to commit mark-missing", error))?;
        Ok(missing.len() as i64)
    }

    /// Completes `scan_run_id` successfully: sets `finished_at`,
    /// `status = 'completed'`, and the final counters. `error_message` may
    /// still be set on an otherwise-completed run to summarize non-fatal,
    /// per-source-folder errors (see [`ScanPersistSummary::folder_errors`]) -
    /// completing successfully is about the run finishing, not about every
    /// source folder having scanned without issue.
    pub fn complete_scan_run(
        &mut self,
        scan_run_id: i64,
        counts: &ScanRunCounts,
        error_message: Option<&str>,
    ) -> Result<()> {
        self.connection
            .execute(
                "UPDATE scan_runs SET finished_at = ?2, status = 'completed', \
                 source_folders_scanned = ?3, archives_seen = ?4, archives_added = ?5, \
                 archives_updated = ?6, archives_missing = ?7, errors_count = ?8, error_message = ?9 \
                 WHERE id = ?1",
                params![
                    scan_run_id,
                    now_utc_string(),
                    counts.source_folders_scanned,
                    counts.archives_seen,
                    counts.archives_added,
                    counts.archives_updated,
                    counts.archives_missing,
                    counts.errors_count,
                    error_message,
                ],
            )
            .map_err(|error| db_error("failed to complete scan run", error))?;
        Ok(())
    }

    /// Marks `scan_run_id` as `'failed'` with `error_message`, for a
    /// fatal error that stopped the run before it could complete. Prior
    /// catalogue state (archives, observations, platform assignments
    /// already committed earlier in this run) is left exactly as it was -
    /// a failed run never marks anything missing, because
    /// [`Database::mark_unseen_archives_missing`] is only ever reached
    /// after [`Database::complete_scan_run`] in [`scan_and_persist`]'s
    /// control flow.
    pub fn fail_scan_run(&mut self, scan_run_id: i64, error_message: &str) -> Result<()> {
        self.connection
            .execute(
                "UPDATE scan_runs SET finished_at = ?2, status = 'failed', error_message = ?3 \
                 WHERE id = ?1",
                params![scan_run_id, now_utc_string(), error_message],
            )
            .map_err(|error| db_error("failed to record failed scan run", error))?;
        Ok(())
    }

    /// Loads every persisted archive, joined with its current platform
    /// assignment if any, with exact path bytes reconstructed into
    /// `PathBuf`s. For tests and future callers (CLI/GUI integration is a
    /// later stage).
    pub fn load_archives(&self) -> Result<Vec<PersistedArchive>> {
        let mut stmt = self
            .connection
            .prepare(
                "SELECT a.id, a.source_folder_id, a.relative_path, a.absolute_path_cached, \
                 a.archive_kind, a.display_name, a.normalized_name, a.size_bytes, \
                 a.modified_time_unix_seconds, p.platform, p.source, a.last_known_health, \
                 a.last_verified_missing_at \
                 FROM archives a \
                 LEFT JOIN platform_assignments p ON p.archive_id = a.id AND p.is_current = 1 \
                 ORDER BY a.id",
            )
            .map_err(|error| db_error("failed to prepare load_archives", error))?;

        let rows = stmt
            .query_map([], |row| {
                let relative_path_bytes: Vec<u8> = row.get(2)?;
                let absolute_path_bytes: Vec<u8> = row.get(3)?;
                let size_bytes: Option<i64> = row.get(7)?;
                Ok(PersistedArchive {
                    id: row.get(0)?,
                    source_folder_id: row.get(1)?,
                    relative_path: PathBuf::from(OsString::from_vec(relative_path_bytes)),
                    absolute_path: PathBuf::from(OsString::from_vec(absolute_path_bytes)),
                    archive_kind: row.get(4)?,
                    display_name: row.get(5)?,
                    normalized_name: row.get(6)?,
                    size_bytes: size_bytes.map(|value| value as u64),
                    modified_time_unix_seconds: row.get(8)?,
                    platform: row.get(9)?,
                    platform_source: row.get(10)?,
                    last_known_health: row.get(11)?,
                    last_verified_missing_at: row.get(12)?,
                })
            })
            .map_err(|error| db_error("failed to query archives", error))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| db_error("failed to read archives", error))
    }

    /// Aggregate counts over the whole catalogue: total archives, how many
    /// are currently present vs. missing, and how many have a current
    /// platform assignment vs. none. Computed with a single SQL aggregate
    /// query rather than loading every row, for
    /// [`crate::CatalogueStats`]-shaped callers like `library-status`.
    pub fn catalogue_stats(&self) -> Result<CatalogueStats> {
        self.connection
            .query_row(
                "SELECT \
                     COUNT(*), \
                     COUNT(*) FILTER (WHERE a.last_verified_missing_at IS NULL), \
                     COUNT(*) FILTER (WHERE a.last_verified_missing_at IS NOT NULL), \
                     COUNT(DISTINCT CASE WHEN p.archive_id IS NOT NULL THEN a.id END) \
                 FROM archives a \
                 LEFT JOIN platform_assignments p ON p.archive_id = a.id AND p.is_current = 1",
                [],
                |row| {
                    let total_archives: i64 = row.get(0)?;
                    let present_archives: i64 = row.get(1)?;
                    let missing_archives: i64 = row.get(2)?;
                    let archives_with_platform: i64 = row.get(3)?;
                    Ok(CatalogueStats {
                        total_archives,
                        present_archives,
                        missing_archives,
                        archives_with_platform,
                        archives_unknown_platform: total_archives - archives_with_platform,
                    })
                },
            )
            .map_err(|error| db_error("failed to compute catalogue stats", error))
    }

    /// The most recently completed (`status = 'completed'`) scan run, if
    /// any. Never returns a `'running'`, `'interrupted'`, or `'failed'`
    /// run - callers that specifically want the outcome of the most
    /// recent attempt regardless of status are not served by this method
    /// today (not needed by anything in this stage).
    pub fn latest_completed_scan(&self) -> Result<Option<CompletedScanSummary>> {
        self.connection
            .query_row(
                "SELECT id, started_at, finished_at, triggered_by, source_folders_scanned, \
                 archives_seen, archives_added, archives_updated, archives_missing, \
                 errors_count, error_message \
                 FROM scan_runs WHERE status = 'completed' ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok(CompletedScanSummary {
                        scan_run_id: row.get(0)?,
                        started_at: row.get(1)?,
                        finished_at: row.get(2)?,
                        triggered_by: row.get(3)?,
                        source_folders_scanned: row.get(4)?,
                        archives_seen: row.get(5)?,
                        archives_added: row.get(6)?,
                        archives_updated: row.get(7)?,
                        archives_missing: row.get(8)?,
                        errors_count: row.get(9)?,
                        error_message: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(|error| db_error("failed to load latest completed scan", error))
    }
}

/// Aggregate counts over the whole persisted catalogue - see
/// [`Database::catalogue_stats`]. `archives_unknown_platform` is derived as
/// `total_archives - archives_with_platform`, matching how "unknown" is
/// represented everywhere else in this schema: by the absence of a current
/// `platform_assignments` row, never a sentinel string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CatalogueStats {
    pub total_archives: i64,
    pub present_archives: i64,
    pub missing_archives: i64,
    pub archives_with_platform: i64,
    pub archives_unknown_platform: i64,
}

/// A snapshot of one completed `scan_runs` row - see
/// [`Database::latest_completed_scan`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CompletedScanSummary {
    pub scan_run_id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub triggered_by: String,
    pub source_folders_scanned: i64,
    pub archives_seen: i64,
    pub archives_added: i64,
    pub archives_updated: i64,
    pub archives_missing: i64,
    pub errors_count: i64,
    pub error_message: Option<String>,
}

/// The highest schema version this build of ArchiveFS understands - the
/// same value [`Database::open_or_create`] refuses to open a newer
/// database than. Exposed so callers (the CLI's `library-status`) can
/// distinguish "this database's schema is newer than this build
/// supports" from "this database just needs a migration" without
/// duplicating the migration list.
pub fn latest_schema_version() -> i64 {
    latest_known_version(MIGRATIONS)
}

/// Scans every folder in `config.source_folders` with the existing
/// [`ArchiveScanner`] (unmodified - one scan per folder, so a single
/// unreachable folder cannot poison the whole run) and persists the
/// results into `database`: registers source folders, starts a
/// `scan_runs` row, upserts each discovered archive with its observation
/// and platform assignment, and - only for folders whose scan succeeded -
/// marks previously-known archives no longer seen as missing. Always
/// completes the scan run (`status = 'completed'`) as long as it could
/// start one at all; per-folder failures are recorded in
/// [`ScanPersistSummary::folder_errors`] and `counts.errors_count`
/// without failing the run or touching that folder's archives.
///
/// Returns `Err` only for a failure that prevents the run from starting
/// or finishing at all (for example the database itself being
/// unreachable) - in that case [`Database::fail_scan_run`] is attempted
/// on a best-effort basis before the error is returned. This never
/// touches mount or unmount state; see `docs/DATABASE_DESIGN.md` section 5.
pub fn scan_and_persist(
    database: &mut Database,
    config: &Config,
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    database.mark_interrupted_scan_runs()?;
    let registered_folders = database.register_source_folders(&config.source_folders)?;
    let scan_run_id = database.start_scan_run(triggered_by, None)?;

    let mut counts = ScanRunCounts::default();
    let mut folder_errors = Vec::new();

    for folder in &registered_folders {
        let folder_config = Config {
            source_folders: vec![folder.path.clone()],
            mount_root: config.mount_root.clone(),
            ratarmount_bin: config.ratarmount_bin.clone(),
        };

        let archives = match ArchiveScanner::new(&folder_config).scan_archives() {
            Ok(archives) => archives,
            Err(error) => {
                counts.errors_count += 1;
                folder_errors.push((folder.path.clone(), error.to_string()));
                continue;
            }
        };

        match persist_one_folder(database, scan_run_id, folder, &archives) {
            Ok(folder_counts) => {
                counts.source_folders_scanned += 1;
                counts.archives_seen += folder_counts.archives_seen;
                counts.archives_added += folder_counts.archives_added;
                counts.archives_changed += folder_counts.archives_changed;
                counts.archives_restored += folder_counts.archives_restored;
                counts.archives_unchanged += folder_counts.archives_unchanged;
                counts.archives_updated += folder_counts.archives_updated;
                counts.archives_missing += folder_counts.archives_missing;
            }
            Err(error) => {
                counts.errors_count += 1;
                folder_errors.push((folder.path.clone(), error.to_string()));
            }
        }
    }

    let error_message = if folder_errors.is_empty() {
        None
    } else {
        Some(
            folder_errors
                .iter()
                .map(|(path, error)| format!("{}: {error}", path.display()))
                .collect::<Vec<_>>()
                .join("; "),
        )
    };

    if let Err(error) = database.complete_scan_run(scan_run_id, &counts, error_message.as_deref()) {
        let _ = database.fail_scan_run(scan_run_id, &error.to_string());
        return Err(error);
    }

    Ok(ScanPersistSummary {
        scan_run_id,
        counts,
        folder_errors,
    })
}

/// Upserts every archive discovered under one already-successfully-scanned
/// source folder, records its observation and platform assignment, and
/// marks any archive under that folder not seen this pass as missing.
/// Only called from [`scan_and_persist`] after that folder's
/// `ArchiveScanner::scan_archives` call already succeeded - a folder whose
/// scan itself failed never reaches this function, so its archives are
/// never touched.
fn persist_one_folder(
    database: &mut Database,
    scan_run_id: i64,
    folder: &RegisteredSourceFolder,
    archives: &[Archive],
) -> Result<ScanRunCounts> {
    let mut counts = ScanRunCounts::default();
    let mut seen_archive_ids = Vec::with_capacity(archives.len());

    for archive in archives {
        let outcome = database.upsert_archive(folder.id, &folder.path, archive)?;
        seen_archive_ids.push(outcome.archive_id);
        counts.archives_seen += 1;
        match outcome.change {
            ArchiveChangeKind::New => counts.archives_added += 1,
            ArchiveChangeKind::Restored => {
                counts.archives_restored += 1;
                counts.archives_updated += 1;
            }
            ArchiveChangeKind::Changed => {
                counts.archives_changed += 1;
                counts.archives_updated += 1;
            }
            ArchiveChangeKind::Unchanged => counts.archives_unchanged += 1,
        }

        database.record_observation(
            scan_run_id,
            outcome.archive_id,
            outcome.change.into(),
            archive.identity.size_bytes,
            archive
                .identity
                .modified_time
                .and_then(system_time_to_unix_seconds),
        )?;

        // Required precedence: manual > custom folder alias > heuristic >
        // built-in folder alias > unknown. `archive.identity.platform`/
        // `platform_provenance` already resolved the heuristic/built-in
        // tiers (in `detect_platform_with_provenance`, which has no
        // database access and so cannot itself see custom aliases) - a
        // custom alias match here unconditionally outranks whatever that
        // already found. `assign_platform` still has the final say via
        // `provenance_priority` (for example, never silently replacing a
        // manual assignment).
        let custom_alias_platform =
            find_custom_platform_alias(database, &archive.path, &archive.identity.source_root)?;
        let (platform, source): (Option<String>, &str) = match &custom_alias_platform {
            Some(platform) => (Some(platform.clone()), CUSTOM_FOLDER_ALIAS_SOURCE),
            None => {
                database.retire_stale_custom_alias_assignment(outcome.archive_id)?;
                (
                    archive.identity.platform.clone(),
                    archive
                        .identity
                        .platform_provenance
                        .map(PlatformProvenance::as_source_str)
                        .unwrap_or("heuristic-path-detector"),
                )
            }
        };
        database.assign_platform(outcome.archive_id, platform.as_deref(), source)?;
    }

    counts.archives_missing =
        database.mark_unseen_archives_missing(scan_run_id, folder.id, &seen_archive_ids)?;

    Ok(counts)
}

/// Finds the nearest custom platform alias match for `path` (an archive
/// discovered under `source_root`), walking directory components from
/// the archive's nearest containing folder upward to (but never beyond)
/// `source_root` - the nearest matching parent wins, mirroring the
/// built-in folder alias walk (`detect_platform_from_folder_alias`)
/// exactly, but consulting the persisted `platform_aliases` table (via
/// [`Database::lookup_custom_platform_alias`]) instead of the built-in
/// `FOLDER_PLATFORM_ALIASES` table. The archive's own filename is
/// excluded, same as the built-in walk.
fn find_custom_platform_alias(
    database: &Database,
    path: &Path,
    source_root: &Path,
) -> Result<Option<String>> {
    let Ok(relative) = path.strip_prefix(source_root) else {
        return Ok(None);
    };
    let mut components: Vec<_> = relative.components().collect();
    components.pop(); // the archive's own filename never counts as a folder.

    for component in components.iter().rev() {
        let segment = component.as_os_str().to_string_lossy();
        if let Some(platform) = database.lookup_custom_platform_alias(&segment)? {
            return Ok(Some(platform));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "archivefs-database-test-{name}-{}-{}",
            std::process::id(),
            now_unix_seconds()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a file at `dir/relative_path` (creating any parent
    /// directories) so `ArchiveScanner` has something real to discover.
    fn write_archive_file(dir: &Path, relative_path: impl AsRef<Path>, content: &[u8]) -> PathBuf {
        let full_path = dir.join(relative_path.as_ref());
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, content).unwrap();
        full_path
    }

    fn config_for(source_dir: &Path, mount_dir: &Path) -> Config {
        Config {
            source_folders: vec![source_dir.to_path_buf()],
            mount_root: mount_dir.to_path_buf(),
            ratarmount_bin: "ratarmount".to_string(),
        }
    }

    fn find_archive<'a>(
        archives: &'a [PersistedArchive],
        relative_path: &str,
    ) -> &'a PersistedArchive {
        archives
            .iter()
            .find(|archive| archive.relative_path == Path::new(relative_path))
            .unwrap_or_else(|| panic!("no persisted archive with relative_path {relative_path}"))
    }

    // -----------------------------------------------------------------
    // Unknown-platform classification (`persisted_archive_has_unknown_platform`).
    // -----------------------------------------------------------------

    #[test]
    fn no_effective_platform_is_classified_unknown() {
        let root = temp_dir("unknown-classification-none");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(archive.platform, None, "sanity check: nothing detected");
        assert!(persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn automatic_platform_is_not_unknown() {
        let root = temp_dir("unknown-classification-automatic");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("MSX2"));
        assert!(!persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_platform_is_not_unknown_even_without_any_automatic_detection() {
        // The crux of requirement 6: a manual assignment on an archive
        // automatic detection never had an opinion about must not be
        // classified as unknown just because there was no automatic
        // signal underneath it.
        let root = temp_dir("unknown-classification-manual");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );
        assert!(!persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_archive_with_no_platform_remains_unknown() {
        let root = temp_dir("unknown-classification-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "rescan").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert!(
            archive.last_verified_missing_at.is_some(),
            "sanity check: marked missing"
        );
        assert!(persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_archive_with_a_manual_platform_is_not_unknown() {
        // Requirement 7's cache-only/missing coverage combined with
        // requirement 6's manual-outranks-unknown rule: a manually
        // classified archive that later goes missing must not revert to
        // "unknown" - the manual assignment is keyed by the stable
        // archive id, untouched by presence/absence.
        let root = temp_dir("unknown-classification-missing-manual");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();
        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "rescan").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert!(archive.last_verified_missing_at.is_some());
        assert!(!persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn civil_from_days_matches_known_reference_dates() {
        assert_eq!(format_unix_timestamp_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_timestamp_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(format_unix_timestamp_utc(86_400), "1970-01-02T00:00:00Z");
        // 2000-03-01T00:00:00Z, a date chosen specifically to cross a
        // leap-year February in the civil_from_days month/day math.
        assert_eq!(
            format_unix_timestamp_utc(951_868_800),
            "2000-03-01T00:00:00Z"
        );
    }

    #[test]
    fn default_database_path_resolves_under_home() {
        let path = resolve_database_path(Some(OsStr::new("/home/example").to_os_string()))
            .expect("HOME is present, so this must resolve");

        assert_eq!(
            path,
            PathBuf::from("/home/example/.local/share/archivefs/library.sqlite3")
        );
    }

    #[test]
    fn unresolved_home_directory_is_a_clear_error_not_a_placeholder_path() {
        let error = resolve_database_path(None).unwrap_err();

        assert!(error.to_string().contains("HOME is not set"));
    }

    #[test]
    fn path_resolution_alone_performs_no_filesystem_writes() {
        let home = temp_dir("path-resolution-no-writes");
        let data_dir = home.join(".local").join("share").join("archivefs");

        let resolved = resolve_database_path(Some(home.clone().into_os_string())).unwrap();

        assert_eq!(resolved, data_dir.join("library.sqlite3"));
        assert!(
            !data_dir.exists(),
            "resolving the default path must not create ~/.local/share/archivefs"
        );
        assert!(
            !resolved.exists(),
            "resolving the default path must not create the database file"
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn open_or_create_creates_missing_parent_directories() {
        let root = temp_dir("parent-dir-creation");
        let db_path = root
            .join("nested")
            .join("does")
            .join("not")
            .join("exist")
            .join("library.sqlite3");
        assert!(!db_path.parent().unwrap().exists());

        let database = Database::open_or_create(&db_path).expect("open_or_create should succeed");

        assert!(db_path.exists());
        assert_eq!(database.path(), db_path.as_path());
        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fresh_database_creation_applies_first_migration() {
        let root = temp_dir("fresh-database");
        let db_path = root.join("library.sqlite3");

        let database = Database::open_or_create(&db_path).unwrap();

        let table_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
                 ('schema_migrations', 'source_folders', 'archives', 'scan_runs', \
                 'archive_scan_observations', 'platform_assignments')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 6);

        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reopen_is_idempotent() {
        let root = temp_dir("reopen-idempotent");
        let db_path = root.join("library.sqlite3");

        let first = Database::open_or_create(&db_path).unwrap();
        assert_eq!(first.schema_version().unwrap(), latest_schema_version());
        first.close().unwrap();

        // Reopening a database that is already at the latest version must
        // not try to re-run any migration's CREATE TABLE statements
        // (which would fail with "table already exists" if it did).
        let second = Database::open_or_create(&db_path).expect("reopening must be idempotent");
        assert_eq!(second.schema_version().unwrap(), latest_schema_version());

        let migration_row_count: i64 = second
            .connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            migration_row_count,
            MIGRATIONS.len() as i64,
            "every migration must be recorded exactly once, not once per open"
        );

        second.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_version_is_reported_after_migration() {
        let root = temp_dir("schema-version-reporting");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert_eq!(database.schema_version().unwrap(), latest_schema_version());

        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn foreign_keys_are_enabled_on_open() {
        let root = temp_dir("foreign-keys-enabled");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert!(database.foreign_keys_enabled().unwrap());

        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn foreign_keys_are_actually_enforced() {
        let root = temp_dir("foreign-keys-enforced");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let result = database.connection.execute(
            "INSERT INTO archives (source_folder_id, relative_path, absolute_path_cached, \
             file_name_cached, archive_kind, display_name, normalized_name, last_known_health, \
             first_seen_at, last_seen_at, created_at, updated_at) \
             VALUES (?1, X'612e7a6970', X'612e7a6970', X'612e7a6970', 'zip', 'a', 'a', \
             'Pending', 'x', 'x', 'x', 'x')",
            [999_i64],
        );

        assert!(
            result.is_err(),
            "inserting an archive under a nonexistent source_folder_id must fail"
        );

        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn future_schema_version_is_rejected_clearly() {
        let root = temp_dir("future-schema-version");
        let db_path = root.join("library.sqlite3");

        {
            let connection = Connection::open(&db_path).unwrap();
            connection
                .pragma_update(None, "user_version", 999_i64)
                .unwrap();
        }

        let error = Database::open_or_create(&db_path).unwrap_err();

        assert!(error.to_string().contains("999"));
        assert!(error.to_string().contains("newer"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_migration_rolls_back_completely() {
        let root = temp_dir("failed-migration-rollback");
        let db_path = root.join("library.sqlite3");
        let mut connection = open_connection(&db_path).unwrap();

        let bookkeeping_migration = Migration {
            version: 1,
            description: "bookkeeping table only, for this test",
            sql: "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, description TEXT NOT NULL, applied_at TEXT NOT NULL);",
        };
        apply_migrations(
            &mut connection,
            std::slice::from_ref(&bookkeeping_migration),
        )
        .unwrap();

        let broken_migration = Migration {
            version: 2,
            description: "deliberately broken for this test",
            sql: "CREATE TABLE ok_table (id INTEGER); CREATE TBLE this_is_not_valid_sql (id INTEGER);",
        };
        let migrations = [bookkeeping_migration, broken_migration];
        let result = apply_migrations(&mut connection, &migrations);
        assert!(result.is_err());

        // Version 1's effects must survive - only the failed migration 2
        // rolls back, not everything.
        assert_eq!(schema_version(&connection).unwrap(), 1);
        let migration_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migration_rows, 1);

        // ok_table must not exist: the batch that created it was rolled
        // back along with the rest of migration 2's failed transaction.
        let ok_table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'ok_table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ok_table_count, 0);

        // Retrying with only the valid migrations must still work
        // afterward - a failed migration must not leave the database
        // permanently stuck.
        apply_migrations(&mut connection, std::slice::from_ref(&migrations[0])).unwrap();
        assert_eq!(schema_version(&connection).unwrap(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_path_round_trips_exactly_through_a_blob_column() {
        let root = temp_dir("non-utf8-blob-roundtrip");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        // "fo<invalid byte>o" - 0x80 alone is never valid UTF-8, so this
        // OsString cannot round-trip through a TEXT/String column without
        // lossy conversion. It must survive a BLOB column byte-for-byte.
        let non_utf8_bytes: Vec<u8> = vec![0x66, 0x6f, 0x80, 0x6f];
        let non_utf8_path = PathBuf::from(OsString::from_vec(non_utf8_bytes.clone()));
        assert!(
            non_utf8_path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );

        let now = now_utc_string();
        database
            .connection
            .execute(
                "INSERT INTO source_folders (path, first_seen_at, last_seen_in_config_at) \
                 VALUES (?1, ?2, ?2)",
                rusqlite::params![non_utf8_path.as_os_str().as_bytes(), now],
            )
            .unwrap();

        let read_back: Vec<u8> = database
            .connection
            .query_row("SELECT path FROM source_folders LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(read_back, non_utf8_bytes);
        let read_back_path = PathBuf::from(OsString::from_vec(read_back));
        assert_eq!(read_back_path, non_utf8_path);

        database.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn check_database_health_reports_missing_database_without_creating_it() {
        let root = temp_dir("health-missing-database");
        let db_path = root.join("library.sqlite3");

        let health = check_database_health(&db_path);

        assert_eq!(health.resolved_path, db_path);
        assert!(!health.database_exists);
        assert!(!health.database_opens);
        assert_eq!(health.schema_version, None);
        assert!(!health.migrations_current);
        assert!(health.error.is_none());
        assert!(
            !db_path.exists(),
            "a health check must never create the database file"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn check_database_health_reports_a_current_database() {
        let root = temp_dir("health-current-database");
        let db_path = root.join("library.sqlite3");
        Database::open_or_create(&db_path).unwrap().close().unwrap();

        let health = check_database_health(&db_path);

        assert!(health.database_exists);
        assert!(health.database_opens);
        assert_eq!(health.schema_version, Some(latest_schema_version()));
        assert!(health.migrations_current);
        assert!(health.foreign_keys_enabled);
        assert!(health.error.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_failure_does_not_affect_existing_mount_planning_behavior() {
        // A database failure here must have zero effect on unrelated core
        // behavior - mount/unmount code never depends on this module, so
        // demonstrate that concretely: force a database open failure,
        // then exercise real (unrelated) mount-planning logic in the same
        // test and confirm it behaves exactly as it does with no database
        // involved at all.
        let root = temp_dir("database-failure-mount-safety");
        let occupied_by_a_file = root.join("not-a-directory");
        fs::write(&occupied_by_a_file, b"not a directory").unwrap();
        let impossible_db_path = occupied_by_a_file.join("library.sqlite3");

        let database_result = Database::open_or_create(&impossible_db_path);
        assert!(
            database_result.is_err(),
            "opening a database under a path that is actually a file must fail"
        );

        let archive = crate::Archive::from_path_in_root("game.zip", root.join("source"))
            .expect("game.zip has a supported extension and does not need to exist on disk");
        let plans = crate::plan_mounts(&[archive], root.join("mounts"));

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].state, crate::MountState::Pending);
        assert_eq!(
            plans[0].mount_path,
            root.join("mounts").join("Unknown").join("game")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn first_scan_inserts_archives() {
        let root = temp_dir("first-scan-inserts");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.counts.archives_seen, 1);
        assert_eq!(summary.counts.archives_added, 1);
        assert_eq!(summary.counts.archives_updated, 0);
        assert_eq!(summary.counts.archives_missing, 0);
        assert!(summary.folder_errors.is_empty());

        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 1);
        let archive = find_archive(&archives, "game.zip");
        assert_eq!(archive.display_name, "game");
        assert_eq!(archive.archive_kind, "zip");
        assert_eq!(archive.size_bytes, Some(8));
        assert!(archive.last_verified_missing_at.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repeat_scan_is_idempotent() {
        let root = temp_dir("repeat-scan-idempotent");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();
        let second = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(second.counts.archives_seen, 1);
        assert_eq!(second.counts.archives_added, 0);
        assert_eq!(second.counts.archives_updated, 0);
        assert_eq!(second.counts.archives_missing, 0);

        let archives = database.load_archives().unwrap();
        assert_eq!(
            archives.len(),
            1,
            "a repeat scan must not duplicate the archive row"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn changed_archive_updates_size_and_modified_time() {
        let root = temp_dir("changed-archive");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let new_contents = b"different, longer contents";
        write_archive_file(&source, "game.zip", new_contents);
        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.counts.archives_added, 0);
        assert_eq!(summary.counts.archives_updated, 1);

        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(
            find_archive(&archives, "game.zip").size_bytes,
            Some(new_contents.len() as u64)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn successful_scan_marks_disappeared_archive_missing() {
        let root = temp_dir("disappeared-archive");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "keep.zip", b"a");
        let doomed_path = write_archive_file(&source, "gone.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        fs::remove_file(&doomed_path).unwrap();
        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.counts.archives_missing, 1);
        assert!(summary.folder_errors.is_empty());

        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 2, "the missing archive's row must survive");
        assert!(
            find_archive(&archives, "gone.zip")
                .last_verified_missing_at
                .is_some()
        );
        assert!(
            find_archive(&archives, "keep.zip")
                .last_verified_missing_at
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_platform_survives_the_archive_going_missing_and_reappearing() {
        // platform_assignments is keyed by the stable archives.id, never
        // touched by mark_unseen_archives_missing or upsert_archive's
        // Restored path - going missing and reappearing must not affect
        // a manual assignment at all, since database identity
        // (source_folder_id, relative_path) never changes.
        let root = temp_dir("manual-survives-missing-and-restored");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "scan-while-missing").unwrap();
        let archives = database.load_archives().unwrap();
        let missing_archive = find_archive(&archives, "mystery.zip");
        assert!(missing_archive.last_verified_missing_at.is_some());
        assert_eq!(missing_archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            missing_archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        fs::write(&archive_path, b"contents").unwrap();
        scan_and_persist(&mut database, &config, "scan-after-restore").unwrap();
        let archives = database.load_archives().unwrap();
        let restored_archive = find_archive(&archives, "mystery.zip");
        assert!(restored_archive.last_verified_missing_at.is_none());
        assert_eq!(
            restored_archive.id, archive_id,
            "database identity must be unchanged"
        );
        assert_eq!(restored_archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            restored_archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fail_scan_run_does_not_touch_archives() {
        let root = temp_dir("fail-scan-run-isolated");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let before = database.load_archives().unwrap();

        let scan_run_id = database.start_scan_run("test", None).unwrap();
        database
            .fail_scan_run(scan_run_id, "simulated fatal error")
            .unwrap();

        let after = database.load_archives().unwrap();
        assert_eq!(
            before, after,
            "recording a failed scan run must not alter any archives row"
        );

        let status: String = database
            .connection
            .query_row(
                "SELECT status FROM scan_runs WHERE id = ?1",
                params![scan_run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unavailable_source_folder_does_not_destroy_prior_catalogue_state() {
        let root = temp_dir("unavailable-source-folder");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let mount = root.join("mount");
        write_archive_file(&source_a, "a.zip", b"a");
        write_archive_file(&source_b, "b.zip", b"b");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        let first = scan_and_persist(&mut database, &config, "test").unwrap();
        assert_eq!(first.counts.archives_added, 2);
        assert!(first.folder_errors.is_empty());

        // source_a becomes entirely unreachable (deleted, or e.g. an
        // unmounted drive in real usage) between scans.
        fs::remove_dir_all(&source_a).unwrap();
        let second = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(
            second.folder_errors.len(),
            1,
            "exactly one source folder's scan should have failed"
        );
        assert_eq!(second.folder_errors[0].0, source_a);
        // source_b still scanned normally and reported no changes.
        assert_eq!(second.counts.source_folders_scanned, 1);
        assert_eq!(second.counts.archives_missing, 0);

        let archives = database.load_archives().unwrap();
        assert_eq!(
            archives.len(),
            2,
            "both archives must still exist after one source folder became unavailable"
        );
        assert!(
            find_archive(&archives, "a.zip")
                .last_verified_missing_at
                .is_none(),
            "a.zip must not be marked missing - its source folder's scan failed, it was never checked"
        );
        assert!(
            find_archive(&archives, "b.zip")
                .last_verified_missing_at
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_relative_path_in_two_source_folders_remains_distinct() {
        let root = temp_dir("same-relative-path-two-folders");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let mount = root.join("mount");
        write_archive_file(&source_a, "game.zip", b"from a");
        write_archive_file(&source_b, "game.zip", b"from b, different size");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.counts.archives_added, 2);
        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 2);
        let distinct_source_folders: HashSet<i64> = archives
            .iter()
            .map(|archive| archive.source_folder_id)
            .collect();
        assert_eq!(
            distinct_source_folders.len(),
            2,
            "the same relative_path under two source folders must produce two distinct archives"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_behaves_as_old_missing_plus_new_present() {
        let root = temp_dir("rename-old-missing-new-present");
        let source = root.join("source");
        let mount = root.join("mount");
        let old_path = write_archive_file(&source, "old_name.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        fs::rename(&old_path, source.join("new_name.zip")).unwrap();
        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.counts.archives_added, 1);
        assert_eq!(summary.counts.archives_missing, 1);

        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 2, "both the old and new rows must exist");
        assert!(
            find_archive(&archives, "old_name.zip")
                .last_verified_missing_at
                .is_some()
        );
        assert!(
            find_archive(&archives, "new_name.zip")
                .last_verified_missing_at
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detected_platform_is_persisted_with_provenance() {
        let root = temp_dir("platform-provenance");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "Xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "Xbox360/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("Xbox360"));

        let source_column: String = database
            .connection
            .query_row(
                "SELECT source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                params![archive.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_column, "heuristic-path-detector");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_platform_remains_representable() {
        let root = temp_dir("unknown-platform");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(
            archive.platform, None,
            "an undetected platform is represented by the absence of a value, not a sentinel string"
        );

        let assignment_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(assignment_count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_alias_detection_is_persisted_with_folder_alias_provenance() {
        let root = temp_dir("folder-alias-provenance");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("MSX2"));

        let source_column: String = database
            .connection
            .query_row(
                "SELECT source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                params![archive.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_column, "folder_alias");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stronger_platform_assignment_is_not_overwritten_by_a_weaker_folder_guess() {
        let root = temp_dir("provenance-priority-protects-stronger");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let archive_id = find_archive(&archives, "msx2/game.zip").id;

        // A manual assignment is exactly the kind of assignment
        // provenance_priority must never let a folder guess quietly
        // replace.
        database
            .set_manual_platform(archive_id, "CustomPlatform")
            .unwrap();

        // A later folder_alias guess - even one that disagrees - must be
        // a no-op against the stronger assignment above.
        database
            .assign_platform(archive_id, Some("MSX2"), "folder_alias")
            .unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("CustomPlatform"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stronger_source_can_still_replace_an_existing_folder_alias_guess() {
        let root = temp_dir("provenance-priority-stronger-replaces-weaker");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let archive_id = find_archive(&archives, "msx2/game.zip").id;
        assert_eq!(
            find_archive(&archives, "msx2/game.zip").platform.as_deref(),
            Some("MSX2")
        );

        // Unlike the reverse direction, a stronger source is always
        // allowed to replace an existing weaker (folder_alias) guess.
        database
            .assign_platform(archive_id, Some("Corrected"), "heuristic-path-detector")
            .unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("Corrected"));

        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // Manual platform assignment.
    // -----------------------------------------------------------------

    #[test]
    fn set_manual_platform_creates_a_new_manual_assignment() {
        let root = temp_dir("set-manual-platform-new");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;

        let change = database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        assert_eq!(change.old_platform, None);
        assert_eq!(change.old_source, None);
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_manual_platform_rejects_empty_or_whitespace_only_text() {
        let root = temp_dir("set-manual-platform-empty");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;

        assert!(database.set_manual_platform(archive_id, "").is_err());
        assert!(database.set_manual_platform(archive_id, "   ").is_err());
        assert!(database.set_manual_platform(archive_id, "\t\n").is_err());

        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "mystery.zip").platform,
            None,
            "a rejected empty/whitespace platform must not be stored"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_manual_platform_replaces_one_manual_assignment_with_another() {
        let root = temp_dir("set-manual-platform-replace");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database.set_manual_platform(archive_id, "N64").unwrap();

        let change = database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        assert_eq!(change.old_platform.as_deref(), Some("N64"));
        assert_eq!(change.old_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));

        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 2,
            "both manual assignments must remain in history, only one current"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn set_manual_platform_with_the_same_value_is_a_no_op() {
        let root = temp_dir("set-manual-platform-idempotent");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 1,
            "re-confirming the same manual platform must not grow history"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_manual_platform_retires_it_cleanly() {
        let root = temp_dir("clear-manual-platform");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        let change = database.clear_manual_platform(archive_id).unwrap();

        assert_eq!(change.old_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.old_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));
        assert_eq!(change.new_platform, None);
        assert_eq!(change.new_source, None);

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(
            archive.platform, None,
            "clearing manual must leave no current assignment, not a sentinel"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_manual_immediately_restores_the_latest_automatic_result_without_a_rescan() {
        let root = temp_dir("clear-manual-immediate-restore");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "msx2/game.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "msx2/game.zip")
                .platform
                .as_deref(),
            Some("MSX2")
        );
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        let change = database.clear_manual_platform(archive_id).unwrap();

        // No `scan_and_persist` call anywhere in this test - the automatic
        // result must be current immediately from the clear call alone.
        assert_eq!(change.new_platform.as_deref(), Some("MSX2"));
        assert_eq!(change.new_source.as_deref(), Some("folder_alias"));
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("MSX2"));
        assert_eq!(archive.platform_source.as_deref(), Some("folder_alias"));

        // The restored row is the original history entry, not a fresh
        // duplicate - only 2 rows total (folder_alias + manual).
        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_manual_platform_is_a_no_op_when_current_assignment_is_not_manual() {
        let root = temp_dir("clear-manual-platform-not-manual");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "msx2/game.zip").id;

        let change = database.clear_manual_platform(archive_id).unwrap();

        assert_eq!(change.old_platform.as_deref(), Some("MSX2"));
        assert_eq!(change.old_source.as_deref(), Some("folder_alias"));
        assert_eq!(
            change.new_platform, change.old_platform,
            "a no-op clear must report the unchanged state as both old and new"
        );
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(
            archive.platform.as_deref(),
            Some("MSX2"),
            "the existing folder_alias assignment must be untouched"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manual_assignment_is_not_overwritten_by_automatic_heuristic_detection() {
        let root = temp_dir("manual-outranks-heuristic");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "007 Legends.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "007 Legends.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "007 Legends.zip")
                .platform
                .as_deref(),
            Some("Xbox360"),
            "sanity check: the title heuristic detects Xbox360 before any manual correction"
        );

        database.set_manual_platform(archive_id, "PC").unwrap();

        // Before this task, "heuristic-path-detector" and "manual" shared
        // the same priority tier, so this call would have silently won -
        // it must now be blocked exactly like a folder_alias guess is.
        database
            .assign_platform(archive_id, Some("Xbox360"), "heuristic-path-detector")
            .unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "007 Legends.zip");
        assert_eq!(archive.platform.as_deref(), Some("PC"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rescan_with_no_detected_platform_still_preserves_manual_platform() {
        // An archive with no filename hint and no platform folder - a
        // scan never detects anything for it (`platform: None`), which
        // `assign_platform` treats as "nothing to say", not "clear the
        // existing assignment" - manual must survive every such rescan.
        let root = temp_dir("manual-survives-undetected-rescan");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "mystery.zip").platform,
            None,
            "sanity check: nothing is auto-detected for a filename with no hints"
        );

        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();
        scan_and_persist(&mut database, &config, "rescan-1").unwrap();
        scan_and_persist(&mut database, &config, "rescan-2").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_archive_id_by_absolute_path_matches_exact_bytes_and_rejects_no_match() {
        let root = temp_dir("find-archive-id-by-path");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let expected_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;

        assert_eq!(
            database
                .find_archive_id_by_absolute_path(&archive_path)
                .unwrap(),
            Some(expected_id)
        );
        assert_eq!(
            database
                .find_archive_id_by_absolute_path(&source.join("does-not-exist.zip"))
                .unwrap(),
            None
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_archive_path_can_be_manually_assigned_on_unix() {
        let root = temp_dir("manual-platform-non-utf8");
        let source = root.join("source");
        let mount = root.join("mount");
        fs::create_dir_all(&source).unwrap();
        // "fo<invalid byte>o.zip" - never valid UTF-8 on its own.
        let mut invalid_name = b"fo".to_vec();
        invalid_name.push(0x80);
        invalid_name.extend_from_slice(b"o.zip");
        let archive_path = source.join(OsString::from_vec(invalid_name));
        assert!(
            archive_path.to_str().is_none(),
            "test path must actually be invalid UTF-8"
        );
        fs::write(&archive_path, b"contents").unwrap();
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let archive_id = database
            .find_archive_id_by_absolute_path(&archive_path)
            .unwrap()
            .expect("the non-UTF-8 archive must have been scanned and found by exact path bytes");
        let change = database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));

        let archives = database.load_archives().unwrap();
        let archive = archives
            .iter()
            .find(|archive| archive.id == archive_id)
            .unwrap();
        assert_eq!(archive.absolute_path, archive_path);
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_manual_exposes_the_latest_automatic_result_even_when_it_changed_while_manual_was_active()
     {
        // Reproduction for the reported stale-automatic-detection flaw:
        // 1. automatic A ("Xbox360", via a title heuristic).
        // 2. manual M ("GameCube").
        // 3. the *same* archive identity's automatic detection changes to
        //    B ("Corrected") - simulated the same way the existing
        //    `a_stronger_source_can_still_replace_an_existing_folder_alias_guess`
        //    test simulates a later scan's detector disagreeing with an
        //    earlier one, via a direct `assign_platform` call (real
        //    end-to-end rescans cannot change platform detection for a
        //    fixed identity, since path *is* the identity here - see
        //    `rename_behaves_as_old_missing_plus_new_present`). While
        //    manual is still current, B must never become current, but it
        //    must still be recorded so it is not lost.
        // 4. manual M must remain the effective/current platform.
        // 5. clear the manual assignment.
        // 6. the effective platform must become B ("Corrected"), not
        //    stale A ("Xbox360").
        let root = temp_dir("stale-automatic-clear");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "007 Legends.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "007 Legends.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "007 Legends.zip")
                .platform
                .as_deref(),
            Some("Xbox360"),
            "step 1: sanity check - automatic A is Xbox360"
        );

        // step 2: manual M.
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        // step 3: the automatic detector now reports B ("Corrected") for
        // this same archive - blocked from becoming current by manual,
        // but must still be recorded, not silently discarded.
        database
            .assign_platform(archive_id, Some("Corrected"), "heuristic-path-detector")
            .unwrap();

        // step 4: manual M must remain the effective/current platform -
        // the automatic B must never become current while manual is active.
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "007 Legends.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        // step 5: clear the manual assignment.
        let change = database.clear_manual_platform(archive_id).unwrap();

        // step 6: the effective platform must become B ("Corrected"), not
        // stale A ("Xbox360") - this is the crux of the reported flaw.
        assert_eq!(
            change.new_platform.as_deref(),
            Some("Corrected"),
            "clearing manual must expose the LATEST automatic result (B), \
             not a stale pre-manual automatic result (A)"
        );
        assert_eq!(
            change.new_source.as_deref(),
            Some("heuristic-path-detector")
        );
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "007 Legends.zip");
        assert_eq!(archive.platform.as_deref(), Some("Corrected"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repeated_scans_with_the_same_shadow_automatic_value_do_not_duplicate_history() {
        let root = temp_dir("stale-automatic-no-duplicate-history");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "n64/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "n64/game.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        // Three rescans in a row while manual is active, all reporting
        // the same unchanged automatic guess (N64, from the folder) -
        // must not grow history on every scan.
        scan_and_persist(&mut database, &config, "rescan-1").unwrap();
        scan_and_persist(&mut database, &config, "rescan-2").unwrap();
        scan_and_persist(&mut database, &config, "rescan-3").unwrap();

        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 2,
            "folder_alias (N64, from the initial scan) + manual (GameCube) - \
             three repeat scans reporting the same N64 guess must not add rows"
        );

        let change = database.clear_manual_platform(archive_id).unwrap();
        assert_eq!(change.new_platform.as_deref(), Some("N64"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn automatic_result_coinciding_with_the_manual_platform_text_is_still_shadow_recorded() {
        // A subtler variant of the same stale-automatic flaw: if the
        // latest automatic result happens to share the same *text* as
        // the current manual platform, the "no-op if platform is
        // unchanged" fast path must not suppress recording it - clearing
        // manual afterward must expose that coincidental match, not a
        // stale earlier automatic guess.
        let root = temp_dir("stale-automatic-coincidental-match");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery.zip").id;
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        // The automatic detector now reports "GameCube" too - the exact
        // same text as the active manual assignment.
        database
            .assign_platform(archive_id, Some("GameCube"), "heuristic-path-detector")
            .unwrap();

        let change = database.clear_manual_platform(archive_id).unwrap();

        assert_eq!(
            change.new_platform.as_deref(),
            Some("GameCube"),
            "the coincidentally-matching automatic result must have been recorded, \
             not silently dropped by the unchanged-platform fast path"
        );
        assert_eq!(
            change.new_source.as_deref(),
            Some("heuristic-path-detector")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_manual_falls_back_to_the_last_known_automatic_result_when_a_rescan_detects_nothing()
    {
        // 1. automatic A.
        // 2. manual M.
        // 3. a rescan of this same identity reports no detected platform
        //    at all (not even a weaker guess) - `assign_platform`'s
        //    existing "`platform: None` is a no-op" rule (it never
        //    overwrites or removes an existing assignment) already means
        //    nothing at all is recorded for this scan.
        // 4. clear manual.
        // 5. the fallback is explicitly the last known automatic result
        //    (A) - there is no newer automatic information to prefer.
        let root = temp_dir("stale-automatic-nothing-detected");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "n64/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "n64/game.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "n64/game.zip")
                .platform
                .as_deref(),
            Some("N64"),
            "sanity check: automatic A is N64"
        );
        database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();

        // step 3: the automatic detector reports nothing for this scan.
        database
            .assign_platform(archive_id, None, "heuristic-path-detector")
            .unwrap();
        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "n64/game.zip").platform.as_deref(),
            Some("GameCube"),
            "manual must remain current through a scan that detects nothing"
        );

        let change = database.clear_manual_platform(archive_id).unwrap();

        assert_eq!(
            change.new_platform.as_deref(),
            Some("N64"),
            "with nothing new detected, the explicit fallback is the last known automatic result"
        );
        assert_eq!(change.new_source.as_deref(), Some("folder_alias"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_gamecube_assignment_for_luigis_mansion_survives_rescan_and_automatic_detection() {
        // Reproduces the exact scenario in requirement 6: a genuinely
        // GameCube game misfiled under an "n64" folder (which the
        // folder-alias fallback would otherwise confidently, and
        // wrongly, detect as N64 on every scan).
        let root = temp_dir("manual-platform-survives-rescan");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "n64/Luigis_Mansion_[hexrom.com].zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archives = database.load_archives().unwrap();
        let archive_id = find_archive(&archives, "n64/Luigis_Mansion_[hexrom.com].zip").id;
        assert_eq!(
            find_archive(&archives, "n64/Luigis_Mansion_[hexrom.com].zip")
                .platform
                .as_deref(),
            Some("N64"),
            "sanity check: folder_alias detects N64 before any manual correction"
        );

        let change = database
            .set_manual_platform(archive_id, "GameCube")
            .unwrap();
        assert_eq!(change.old_platform.as_deref(), Some("N64"));
        assert_eq!(change.old_source.as_deref(), Some("folder_alias"));
        assert_eq!(change.new_platform.as_deref(), Some("GameCube"));
        assert_eq!(change.new_source.as_deref(), Some(MANUAL_PLATFORM_SOURCE));

        // Rescan repeatedly - the folder still says n64/, so automatic
        // detection keeps trying to reassign "N64" every time, and must
        // keep losing to the manual assignment (proves both "folder_alias
        // cannot overwrite manual" and "manual survives rescan").
        scan_and_persist(&mut database, &config, "rescan-1").unwrap();
        scan_and_persist(&mut database, &config, "rescan-2").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "n64/Luigis_Mansion_[hexrom.com].zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        // Assignment history remains intact: the original folder_alias
        // row from the initial scan is still present, only not current.
        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 2,
            "the original folder_alias row must remain in history, not be deleted"
        );

        // Clearing manual immediately restores the last known automatic
        // result (N64, from the initial scan) - no rescan required.
        let clear_change = database.clear_manual_platform(archive_id).unwrap();
        assert_eq!(
            clear_change.old_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );
        assert_eq!(clear_change.new_platform.as_deref(), Some("N64"));
        assert_eq!(clear_change.new_source.as_deref(), Some("folder_alias"));
        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "n64/Luigis_Mansion_[hexrom.com].zip")
                .platform
                .as_deref(),
            Some("N64"),
            "the automatic result must be current immediately, before any rescan"
        );

        scan_and_persist(&mut database, &config, "rescan-after-clear").unwrap();
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "n64/Luigis_Mansion_[hexrom.com].zip");
        assert_eq!(
            archive.platform.as_deref(),
            Some("N64"),
            "clearing manual must let automatic detection resume"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nobara_like_layout_persists_the_expected_platform_per_console_folder() {
        // A small reproduction of the real-world layout that motivated
        // folder-alias detection: msx2/neogeo/intellivision folders full
        // of archives with no filename hints at all - only the folder
        // name distinguishes them. Uses temporary directories throughout;
        // never touches a real user library.
        let root = temp_dir("nobara-like-layout");
        let source = root.join("Archives");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/Game1.zip", b"a");
        write_archive_file(&source, "neogeo/Game2.zip", b"b");
        write_archive_file(&source, "intellivision/Game3.zip", b"c");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let summary = scan_and_persist(&mut database, &config, "test").unwrap();
        assert_eq!(summary.counts.archives_added, 3);
        assert!(summary.folder_errors.is_empty());

        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "msx2/Game1.zip")
                .platform
                .as_deref(),
            Some("MSX2")
        );
        assert_eq!(
            find_archive(&archives, "neogeo/Game2.zip")
                .platform
                .as_deref(),
            Some("NeoGeo")
        );
        assert_eq!(
            find_archive(&archives, "intellivision/Game3.zip")
                .platform
                .as_deref(),
            Some("Intellivision")
        );

        let stats = database.catalogue_stats().unwrap();
        assert_eq!(stats.total_archives, 3);
        assert_eq!(stats.archives_with_platform, 3);
        assert_eq!(stats.archives_unknown_platform, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_relative_path_round_trips_through_a_scan() {
        let root = temp_dir("non-utf8-relative-path-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        fs::create_dir_all(&source).unwrap();

        // "fo<invalid byte>o.zip" - archive_kind's lossy substring check
        // still sees the trailing ".zip", but the stored path must keep
        // the real, non-UTF-8 byte exactly.
        let non_utf8_name =
            OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p']);
        assert!(non_utf8_name.to_str().is_none());
        let file_path = source.join(&non_utf8_name);
        fs::write(&file_path, b"contents").unwrap();

        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        let summary = scan_and_persist(&mut database, &config, "test").unwrap();
        assert_eq!(summary.counts.archives_added, 1);
        assert!(summary.folder_errors.is_empty());

        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].relative_path, PathBuf::from(&non_utf8_name));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupted_upsert_transaction_rolls_back_safely() {
        let root = temp_dir("interrupted-upsert-rollback");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        // A source_folder_id that was never registered violates the
        // archives.source_folder_id foreign key, forcing the INSERT
        // inside upsert_archive's transaction to fail.
        let archive = Archive::from_path_in_root("game.zip", root.join("source"))
            .expect("game.zip has a supported extension and does not need to exist on disk");
        let result = database.upsert_archive(999_999, &root.join("source"), &archive);
        assert!(result.is_err());

        let archive_count: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM archives", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            archive_count, 0,
            "a failed upsert_archive transaction must leave no partial row behind"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn catalogue_stats_counts_present_missing_and_platform() {
        let root = temp_dir("catalogue-stats");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "Xbox360/known.zip", b"a");
        write_archive_file(&source, "mystery.zip", b"b");
        let doomed = write_archive_file(&source, "gone.zip", b"c");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        fs::remove_file(&doomed).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let stats = database.catalogue_stats().unwrap();

        assert_eq!(stats.total_archives, 3);
        assert_eq!(stats.present_archives, 2);
        assert_eq!(stats.missing_archives, 1);
        assert_eq!(stats.archives_with_platform, 1);
        assert_eq!(stats.archives_unknown_platform, 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn latest_completed_scan_reports_the_most_recent_run() {
        let root = temp_dir("latest-completed-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert!(database.latest_completed_scan().unwrap().is_none());

        let summary = scan_and_persist(&mut database, &config, "test-trigger").unwrap();
        let latest = database.latest_completed_scan().unwrap().unwrap();

        assert_eq!(latest.scan_run_id, summary.scan_run_id);
        assert_eq!(latest.triggered_by, "test-trigger");
        assert_eq!(latest.archives_seen, 1);
        assert_eq!(latest.archives_added, 1);
        assert!(latest.finished_at.is_some());
        assert!(latest.error_message.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn latest_schema_version_matches_the_migrated_database() {
        let root = temp_dir("latest-schema-version");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert_eq!(database.schema_version().unwrap(), latest_schema_version());

        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // Custom platform folder aliases: migration/CRUD
    // -----------------------------------------------------------------

    #[test]
    fn migration_to_platform_aliases_preserves_existing_rows() {
        // A database created and populated at schema version 1 (before
        // platform_aliases existed) must upgrade to version 2 without
        // losing anything already in it.
        let root = temp_dir("platform-aliases-migration-preserves-rows");
        let db_path = root.join("library.sqlite3");
        {
            let mut connection = open_connection(&db_path).unwrap();
            apply_migrations(&mut connection, &MIGRATIONS[..1]).unwrap();
            connection
                .execute(
                    "INSERT INTO source_folders (path, first_seen_at, last_seen_in_config_at) \
                     VALUES (?1, ?2, ?2)",
                    params![b"/roms".as_slice(), now_utc_string()],
                )
                .unwrap();
        }
        assert_eq!(
            schema_version(&open_connection(&db_path).unwrap()).unwrap(),
            1
        );

        let database = Database::open_or_create(&db_path).unwrap();
        assert_eq!(database.schema_version().unwrap(), latest_schema_version());
        let source_folder_count: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM source_folders", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            source_folder_count, 1,
            "the pre-migration source_folders row must survive the upgrade to platform_aliases"
        );
        assert!(database.list_platform_aliases().unwrap().is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn add_list_remove_platform_alias_round_trip() {
        let root = temp_dir("platform-alias-round-trip");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let added = database.add_platform_alias("gc", "GameCube").unwrap();
        assert_eq!(added.alias, "gc");
        assert_eq!(added.normalized_alias, "gc");
        assert_eq!(added.platform, "GameCube");

        let listed = database.list_platform_aliases().unwrap();
        assert_eq!(listed, vec![added]);

        assert!(database.remove_platform_alias("gc").unwrap());
        assert!(database.list_platform_aliases().unwrap().is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn normalized_alias_is_unique_regardless_of_original_casing_or_separators() {
        let root = temp_dir("platform-alias-normalization-uniqueness");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let first = database.add_platform_alias("GC", "GameCube").unwrap();
        assert_eq!(first.normalized_alias, "gc");

        // Every other spelling variant of the same folder name must
        // collide with the row above as a duplicate, not silently create
        // a second row or update it - normalized_alias carries this
        // table's uniqueness constraint.
        for spelling in ["gc", "g-c", "g_c"] {
            let error = database
                .add_platform_alias(spelling, "GameCube")
                .unwrap_err();
            assert!(
                error.to_string().contains("already exists"),
                "{spelling} must be rejected as a duplicate of 'gc', got: {error}"
            );
        }

        let listed = database.list_platform_aliases().unwrap();
        assert_eq!(listed, vec![first]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_normalized_alias_is_rejected() {
        let root = temp_dir("platform-alias-empty-normalized");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let error = database.add_platform_alias("---", "GameCube").unwrap_err();
        assert!(error.to_string().contains("letter or digit"));
        assert!(database.list_platform_aliases().unwrap().is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn alias_containing_a_path_separator_is_rejected() {
        let root = temp_dir("platform-alias-path-separator");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let error = database
            .add_platform_alias("gc/extra", "GameCube")
            .unwrap_err();
        assert!(error.to_string().contains('/'));
        assert!(database.list_platform_aliases().unwrap().is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_argument_is_matched_case_insensitively_and_canonical_spelling_is_stored() {
        let root = temp_dir("platform-alias-canonical-spelling");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let alias = database.add_platform_alias("gc", "gamecube").unwrap();
        assert_eq!(alias.platform, "GameCube");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_platform_argument_is_a_clear_error() {
        let root = temp_dir("platform-alias-unknown-platform");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let error = database
            .add_platform_alias("gc", "NotARealPlatform")
            .unwrap_err();
        assert!(error.to_string().contains("not a known platform"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn adding_an_existing_normalized_alias_is_a_clear_deterministic_duplicate_error() {
        let root = temp_dir("platform-alias-deterministic-duplicate");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        let first = database.add_platform_alias("gc", "GameCube").unwrap();
        let error = database.add_platform_alias("GC", "Wii").unwrap_err();

        assert!(error.to_string().contains("already exists"));
        // The original row is left completely untouched by the failed
        // duplicate add - not partially updated, not duplicated.
        assert_eq!(database.list_platform_aliases().unwrap(), vec![first]);

        // Removing it first and re-adding is the documented way to
        // change an alias's platform.
        assert!(database.remove_platform_alias("gc").unwrap());
        let replaced = database.add_platform_alias("GC", "Wii").unwrap();
        assert_eq!(replaced.platform, "Wii");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn platform_alias_list_order_is_stable_and_sorted_by_normalized_alias() {
        let root = temp_dir("platform-alias-list-order");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        database.add_platform_alias("wii", "Wii").unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();
        database.add_platform_alias("n64", "N64").unwrap();

        let first_listing = database.list_platform_aliases().unwrap();
        let second_listing = database.list_platform_aliases().unwrap();
        assert_eq!(first_listing, second_listing);
        let normalized: Vec<&str> = first_listing
            .iter()
            .map(|alias| alias.normalized_alias.as_str())
            .collect();
        assert_eq!(normalized, vec!["gc", "n64", "wii"]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_an_unknown_alias_is_a_clear_no_op_result() {
        let root = temp_dir("platform-alias-remove-unknown");
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert!(!database.remove_platform_alias("does-not-exist").unwrap());

        let _ = fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // Custom platform folder aliases: detection precedence
    // -----------------------------------------------------------------

    #[test]
    fn custom_alias_detects_platform_during_a_scan() {
        let root = temp_dir("custom-alias-detects-platform");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "gc/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "gc/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_alias_case_and_separator_normalization_matches_the_folder_name() {
        let root = temp_dir("custom-alias-normalization");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "g_c/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("G-C", "GameCube").unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "g_c/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nearest_matching_custom_alias_parent_wins() {
        let root = temp_dir("custom-alias-nearest-parent");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "outer/inner/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("outer", "Wii").unwrap();
        database.add_platform_alias("inner", "WiiU").unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "outer/inner/game.zip");
        assert_eq!(
            archive.platform.as_deref(),
            Some("WiiU"),
            "the nearer 'inner' alias must win over the farther 'outer' one"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_alias_outranks_the_built_in_folder_alias_for_the_same_folder_name() {
        // "n64" is already a built-in FOLDER_PLATFORM_ALIASES key (-> N64).
        // A custom alias for the exact same folder name must win instead.
        let root = temp_dir("custom-alias-outranks-built-in");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "n64/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("n64", "GameCube").unwrap();

        scan_and_persist(&mut database, &config, "test").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "n64/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_alias_outranks_the_existing_filename_path_heuristic() {
        // A folder literally named "xbox360" makes
        // detect_platform_from_known_heuristics report Xbox360. A custom
        // alias for that same folder name must still win, per the
        // required precedence (custom alias ranks above the heuristic).
        let root = temp_dir("custom-alias-outranks-heuristic");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "xbox360/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        // Sanity check: without a custom alias, the heuristic wins.
        scan_and_persist(&mut database, &config, "before-alias").unwrap();
        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "xbox360/game.zip")
                .platform
                .as_deref(),
            Some("Xbox360"),
            "sanity check: the heuristic detects Xbox360 before any custom alias exists"
        );

        database.add_platform_alias("xbox360", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "after-alias").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "xbox360/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_assignment_still_outranks_a_matching_custom_alias() {
        let root = temp_dir("custom-alias-manual-outranks");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "gc/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "gc/game.zip").id;

        database.set_manual_platform(archive_id, "Wii").unwrap();
        scan_and_persist(&mut database, &config, "rescan-with-manual-active").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "gc/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("Wii"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_while_manual_is_active_shadow_records_the_custom_alias_fallback() {
        let root = temp_dir("custom-alias-shadow-while-manual");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery/game.zip").id;
        database.set_manual_platform(archive_id, "Wii").unwrap();

        // The alias did not exist yet when manual was set - it only
        // starts affecting this archive on the next scan, which must
        // shadow-record it without disturbing the current manual value.
        database.add_platform_alias("mystery", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "rescan-with-manual-active").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery/game.zip");
        assert_eq!(
            archive.platform.as_deref(),
            Some("Wii"),
            "manual must still be current"
        );

        let change = database.clear_manual_platform(archive_id).unwrap();
        assert_eq!(
            change.new_platform.as_deref(),
            Some("GameCube"),
            "clearing manual must expose the shadow-recorded custom-alias fallback"
        );
        assert_eq!(
            change.new_source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_alias_and_rescanning_restores_the_built_in_alias_fallback() {
        let root = temp_dir("custom-alias-removal-restores-built-in");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "n64/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("n64", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "with-alias").unwrap();
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "n64/game.zip")
                .platform
                .as_deref(),
            Some("GameCube")
        );

        assert!(database.remove_platform_alias("n64").unwrap());
        scan_and_persist(&mut database, &config, "after-removal").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "n64/game.zip");
        assert_eq!(
            archive.platform.as_deref(),
            Some("N64"),
            "removing the alias and rescanning must restore the built-in folder alias fallback"
        );
        assert_eq!(archive.platform_source.as_deref(), Some("folder_alias"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_alias_and_rescanning_restores_unknown_when_nothing_else_matches() {
        let root = temp_dir("custom-alias-removal-restores-unknown");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("mystery", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "with-alias").unwrap();
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "mystery/game.zip")
                .platform
                .as_deref(),
            Some("GameCube")
        );

        assert!(database.remove_platform_alias("mystery").unwrap());
        scan_and_persist(&mut database, &config, "after-removal").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "mystery/game.zip");
        assert_eq!(
            archive.platform, None,
            "with no built-in alias or heuristic match either, removal must restore Unknown"
        );
        assert!(persisted_archive_has_unknown_platform(archive));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repeated_scans_with_an_unchanged_custom_alias_do_not_duplicate_history() {
        let root = temp_dir("custom-alias-no-duplicate-history");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "gc/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();

        scan_and_persist(&mut database, &config, "scan-1").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "gc/game.zip").id;
        scan_and_persist(&mut database, &config, "scan-2").unwrap();
        scan_and_persist(&mut database, &config, "scan-3").unwrap();

        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 1,
            "three scans reporting the same unchanged custom-alias result must not add rows"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
