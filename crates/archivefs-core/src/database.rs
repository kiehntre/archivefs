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

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    description: "initial schema: schema_migrations, source_folders, archives, scan_runs, archive_scan_observations, platform_assignments",
    sql: include_str!("migrations/0001_initial.sql"),
}];

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
    pub last_known_health: String,
    pub last_verified_missing_at: Option<String>,
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

/// Relative strength of a `platform_assignments.source` value, used by
/// [`Database::assign_platform`] to decide whether a new assignment may
/// replace the current one. Only `"folder_alias"` (the generic
/// folder-name fallback in `crate::detect_platform_with_provenance`) is
/// weak: everything else - `"heuristic-path-detector"`, and any source
/// string this build does not specifically recognize (a future
/// `"manual"`/user-override source, for example) - is treated as at
/// least as strong, so a folder guess can never quietly replace a
/// stronger or manual assignment, while two assignments from the same
/// tier can still freely replace each other (matching the existing
/// heuristic-vs-heuristic reassignment behavior from before this
/// function existed).
fn provenance_priority(source: &str) -> u8 {
    match source {
        "folder_alias" => 0,
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
    /// assignment's `source` (see [`provenance_priority`]) is also a
    /// no-op: today this means a `"folder_alias"` guess can never replace
    /// an existing assignment made any other way (including a future
    /// manual/user-override source this build does not specifically
    /// recognize - `provenance_priority` treats every unrecognized source
    /// as strong, not weak, so it is never silently overwritten by a
    /// folder guess either). Otherwise the previous current row (if any)
    /// is flipped to not-current and a new row is inserted, preserving
    /// full history.
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
                 a.modified_time_unix_seconds, p.platform, a.last_known_health, \
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
                    last_known_health: row.get(10)?,
                    last_verified_missing_at: row.get(11)?,
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
        database.assign_platform(
            outcome.archive_id,
            archive.identity.platform.as_deref(),
            archive
                .identity
                .platform_provenance
                .map(PlatformProvenance::as_source_str)
                .unwrap_or("heuristic-path-detector"),
        )?;
    }

    counts.archives_missing =
        database.mark_unseen_archives_missing(scan_run_id, folder.id, &seen_archive_ids)?;

    Ok(counts)
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
        assert_eq!(first.schema_version().unwrap(), 1);
        first.close().unwrap();

        // Reopening a database that is already at the latest version must
        // not try to re-run migration 1's CREATE TABLE statements (which
        // would fail with "table already exists" if it did).
        let second = Database::open_or_create(&db_path).expect("reopening must be idempotent");
        assert_eq!(second.schema_version().unwrap(), 1);

        let migration_row_count: i64 = second
            .connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            migration_row_count, 1,
            "migration 1 must be recorded exactly once, not once per open"
        );

        second.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn schema_version_is_reported_after_migration() {
        let root = temp_dir("schema-version-reporting");
        let database = Database::open_or_create(root.join("library.sqlite3")).unwrap();

        assert_eq!(database.schema_version().unwrap(), 1);

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
        assert_eq!(health.schema_version, Some(1));
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

        // Simulate a stronger correction - a hypothetical future "manual"
        // source is exactly the kind of assignment provenance_priority
        // must never let a folder guess quietly replace.
        database
            .assign_platform(archive_id, Some("CustomPlatform"), "manual")
            .unwrap();

        // A later folder_alias guess - even one that disagrees - must be
        // a no-op against the stronger assignment above.
        database
            .assign_platform(archive_id, Some("MSX2"), "folder_alias")
            .unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("CustomPlatform"));

        let source_column: String = database
            .connection
            .query_row(
                "SELECT source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                params![archive.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_column, "manual");

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
}
