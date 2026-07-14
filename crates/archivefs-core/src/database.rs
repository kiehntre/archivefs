//! Persistent library catalogue database foundation.
//!
//! Stage 1 only: resolving the default database path, opening/creating the
//! SQLite database, applying forward-only migrations, and reporting
//! structured health. Nothing in this module is called from any scan,
//! mount, unmount, CLI, or GUI code path yet - see
//! `docs/DATABASE_DESIGN.md` and
//! `docs/adr/0001-persistent-library-database.md` for the design this
//! implements and why mount/unmount safety never depends on it.
//!
//! `rusqlite::Connection` is intentionally never exposed outside this
//! module. Every other part of the crate that eventually needs the
//! database goes through [`Database`]'s narrow API, or the standalone
//! [`check_database_health`] report.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};

use rusqlite::Connection;

use crate::{ArchiveFsError, Result};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
            let schema_version_value = schema_version(&connection).ok();
            let foreign_keys_value = foreign_keys_enabled(&connection).unwrap_or(false);
            let migrations_current = schema_version_value == Some(latest_known_version(MIGRATIONS));

            DatabaseHealth {
                resolved_path: path,
                database_exists: true,
                database_opens: true,
                schema_version: schema_version_value,
                migrations_current,
                foreign_keys_enabled: foreign_keys_value,
                error: None,
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
}
