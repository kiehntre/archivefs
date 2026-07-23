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

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params};

use crate::emulator_environment::EncodedPath;

use crate::{
    Archive, ArchiveFsError, ArchiveKind, ArchiveScanner, Config, PlatformProvenance, Result,
    canonical_platform_names, detect_platform_with_details, normalize_path_segment,
    revalidate_archive_for_catalogue, validate_configured_source_roots,
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
    Migration {
        version: 3,
        description: "add source_folders scan-status columns: last_scan_status, last_scan_error, last_scan_at, last_successful_scan_at, last_archive_count",
        sql: include_str!("migrations/0003_source_folder_scan_status.sql"),
    },
    Migration {
        version: 4,
        description: "retain unchanged, unsupported-extension, and ambiguous-platform scan counts",
        sql: include_str!("migrations/0004_scan_skip_counts.sql"),
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
    fn begin_catalogue_refresh(&mut self) -> Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| db_error("failed to acquire catalogue refresh transaction", error))
    }

    fn commit_catalogue_refresh(&mut self) -> Result<()> {
        self.connection
            .execute_batch("COMMIT")
            .map_err(|error| db_error("failed to commit catalogue refresh", error))
    }

    fn rollback_catalogue_refresh(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK");
    }

    fn begin_folder_refresh(&mut self) -> Result<()> {
        self.connection
            .execute_batch("SAVEPOINT archivefs_folder_refresh")
            .map_err(|error| db_error("failed to start source refresh savepoint", error))
    }

    fn commit_folder_refresh(&mut self) -> Result<()> {
        self.connection
            .execute_batch("RELEASE SAVEPOINT archivefs_folder_refresh")
            .map_err(|error| db_error("failed to commit source refresh savepoint", error))
    }

    fn rollback_folder_refresh(&mut self) -> Result<()> {
        self.connection
            .execute_batch(
                "ROLLBACK TO SAVEPOINT archivefs_folder_refresh; \
                 RELEASE SAVEPOINT archivefs_folder_refresh",
            )
            .map_err(|error| db_error("failed to roll back source refresh savepoint", error))
    }

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

    /// Opens an existing, current-schema catalogue with SQLite's read-only
    /// flag. This path never creates parent
    /// directories, creates a database, applies migrations, repairs state, or
    /// obtains a write-capable connection. It is intended for advisory
    /// features whose safety contract requires the catalogue to remain
    /// byte-for-byte unchanged.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(ArchiveFsError::Database(format!(
                "library database does not exist at {}",
                path.display()
            )));
        }

        let connection = open_read_only_connection(path)?;
        let current_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|error| read_only_database_error(path, "read schema version", &error))?;
        let expected_version = latest_known_version(MIGRATIONS);
        if current_version != expected_version {
            return Err(ArchiveFsError::Database(format!(
                "library database schema version {current_version} is not the required current version {expected_version}; refusing to migrate or repair it during a read-only operation"
            )));
        }

        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
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
    /// Normal read-write opens enable it. Explicit read-only opens leave
    /// every pragma untouched and only report SQLite's connection default.
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

/// Stable diagnostic categories. Sidecar presence is evidence only: it is
/// deliberately never classified as corruption or as proof of a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseDiagnosticCode {
    MissingDatabase,
    PermissionDenied,
    DatabaseLocked,
    DatabaseBusy,
    RollbackJournalPresent,
    HotRollbackJournal,
    NonHotRollbackJournal,
    MalformedRollbackJournal,
    RollbackRecoveryRequired,
    WalPresent,
    ShmPresent,
    CorruptDatabase,
    MalformedDatabase,
    IntegrityCheckFailed,
    SchemaVersionUnsupported,
    MigrationFailed,
    IoError,
    SqliteError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseDiagnostic {
    pub code: DatabaseDiagnosticCode,
    pub severity: DatabaseDiagnosticSeverity,
    pub message: String,
    /// SQLite's numeric extended result code, when the evidence came from SQLite.
    pub sqlite_extended_code: Option<i32>,
    /// Unstable presentation detail from SQLite. Consumers must use `code`.
    pub raw_sqlite_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseFileFinding {
    pub path: EncodedPath,
    pub size_bytes: u64,
    pub permissions_mode: Option<u32>,
    pub owner_uid: Option<u32>,
    pub group_gid: Option<u32>,
    pub inode: Option<u64>,
    pub modified_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseSidecarFinding {
    pub kind: DatabaseSidecarKind,
    pub path: EncodedPath,
    pub present: bool,
    pub size_bytes: Option<u64>,
    pub rollback_journal_header: Option<RollbackJournalHeaderState>,
}

/// Bounded evidence from the first eight bytes of a rollback journal.
/// `HotCandidate` means SQLite's journal magic is present; SQLite still
/// decides whether recovery is required after considering locks and the
/// complete header. Zeroed and truncated headers are non-hot. An invalid
/// non-zero header is reported separately as malformed evidence, not as
/// database corruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackJournalHeaderState {
    HotCandidate,
    ZeroedNonHot,
    TruncatedNonHot,
    Malformed,
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseSidecarKind {
    RollbackJournal,
    Wal,
    Shm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseOpenOutcome {
    OpenedReadOnly,
    MissingDatabase,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseCheckStatus {
    Ok,
    Failed,
    Error,
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseCheckOutcome {
    pub status: DatabaseCheckStatus,
    pub messages: Vec<String>,
}

impl DatabaseCheckOutcome {
    fn not_run() -> Self {
        Self {
            status: DatabaseCheckStatus::NotRun,
            messages: Vec::new(),
        }
    }
}

/// Complete, read-only database diagnosis. The JSON field names and enum
/// spellings are an API contract; raw SQLite prose is explicitly secondary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseHealthReport {
    pub format_version: u32,
    pub database_path: EncodedPath,
    pub database_present: bool,
    pub main_file: Option<DatabaseFileFinding>,
    pub sidecars: Vec<DatabaseSidecarFinding>,
    pub open_outcome: DatabaseOpenOutcome,
    pub journal_mode: Option<String>,
    pub quick_check: DatabaseCheckOutcome,
    pub integrity_check: DatabaseCheckOutcome,
    pub schema_version: Option<i64>,
    pub diagnostics: Vec<DatabaseDiagnostic>,
}

/// Inspects an existing catalogue without creating files/directories, running
/// migrations, changing pragmas, checkpointing WAL, or attempting recovery.
pub fn diagnose_database(path: impl AsRef<Path>) -> DatabaseHealthReport {
    let path = path.as_ref();
    let mut diagnostics = Vec::new();
    let (database_present, main_file) = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => (true, Some(file_finding(path, &metadata))),
        Ok(_) => {
            diagnostics.push(diagnostic(
                DatabaseDiagnosticCode::IoError,
                DatabaseDiagnosticSeverity::Error,
                "configured database path is not a regular file",
                None,
            ));
            (true, None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, None),
        Err(error) => {
            diagnostics.push(io_diagnostic("failed to inspect database file", &error));
            // The path may exist but be unstatable. Do not mislabel that as a
            // missing database; the existing I/O diagnostic is the evidence.
            (true, None)
        }
    };
    let sidecars = [
        (
            DatabaseSidecarKind::RollbackJournal,
            sidecar_path(path, "-journal"),
        ),
        (DatabaseSidecarKind::Wal, sidecar_path(path, "-wal")),
        (DatabaseSidecarKind::Shm, sidecar_path(path, "-shm")),
    ]
    .into_iter()
    .map(|(kind, sidecar_path)| {
        let metadata = match fs::symlink_metadata(&sidecar_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                diagnostics.push(io_diagnostic("failed to inspect database sidecar", &error));
                None
            }
        };
        let present = metadata.is_some();
        let regular_file = metadata.as_ref().is_some_and(|value| value.is_file());
        let rollback_journal_header = if present && kind == DatabaseSidecarKind::RollbackJournal {
            Some(match inspect_rollback_journal_header(&sidecar_path, metadata.as_ref().unwrap()) {
                Ok(state) => state,
                Err(error) => {
                    diagnostics.push(io_diagnostic(
                        "failed to read rollback journal header",
                        &error,
                    ));
                    RollbackJournalHeaderState::Unreadable
                }
            })
        } else {
            None
        };
        if present {
            if kind == DatabaseSidecarKind::RollbackJournal {
                diagnostics.push(diagnostic(
                    DatabaseDiagnosticCode::RollbackJournalPresent,
                    DatabaseDiagnosticSeverity::Warning,
                    "rollback journal is present; presence alone does not imply corruption",
                    None,
                ));
            }
            let code = match kind {
                DatabaseSidecarKind::RollbackJournal => match rollback_journal_header {
                    Some(RollbackJournalHeaderState::HotCandidate) => {
                        DatabaseDiagnosticCode::HotRollbackJournal
                    }
                    Some(
                        RollbackJournalHeaderState::ZeroedNonHot
                        | RollbackJournalHeaderState::TruncatedNonHot,
                    ) => {
                        DatabaseDiagnosticCode::NonHotRollbackJournal
                    }
                    Some(RollbackJournalHeaderState::Malformed) => {
                        DatabaseDiagnosticCode::MalformedRollbackJournal
                    }
                    Some(RollbackJournalHeaderState::Unreadable) | None => {
                        DatabaseDiagnosticCode::RollbackJournalPresent
                    }
                },
                DatabaseSidecarKind::Wal => DatabaseDiagnosticCode::WalPresent,
                DatabaseSidecarKind::Shm => DatabaseDiagnosticCode::ShmPresent,
            };
            let message = match rollback_journal_header {
                Some(RollbackJournalHeaderState::HotCandidate) => {
                    "rollback journal has SQLite's hot-journal magic; recovery may be required, subject to SQLite's lock checks"
                }
                Some(RollbackJournalHeaderState::ZeroedNonHot) => {
                    "rollback journal is present with a zeroed non-hot header"
                }
                Some(RollbackJournalHeaderState::TruncatedNonHot) => {
                    "rollback journal is too short to contain a complete SQLite rollback-journal header and is non-hot"
                }
                Some(RollbackJournalHeaderState::Malformed) => {
                    "rollback journal has an unrecognised non-zero header; this does not by itself imply database corruption"
                }
                Some(RollbackJournalHeaderState::Unreadable) => {
                    "rollback journal is present but its header could not be read"
                }
                None => "sidecar file is present; presence alone does not imply corruption",
            };
            if kind != DatabaseSidecarKind::RollbackJournal
                || code != DatabaseDiagnosticCode::RollbackJournalPresent
            {
                diagnostics.push(diagnostic(
                    code,
                    if matches!(
                        rollback_journal_header,
                        Some(
                            RollbackJournalHeaderState::ZeroedNonHot
                                | RollbackJournalHeaderState::TruncatedNonHot
                        )
                    ) {
                        DatabaseDiagnosticSeverity::Info
                    } else {
                        DatabaseDiagnosticSeverity::Warning
                    },
                    message,
                    None,
                ));
            }
        }
        DatabaseSidecarFinding {
            kind,
            path: EncodedPath::from_path(&sidecar_path),
            present,
            size_bytes: regular_file.then(|| metadata.as_ref().unwrap().len()),
            rollback_journal_header,
        }
    })
    .collect();

    let mut report = DatabaseHealthReport {
        format_version: 2,
        database_path: EncodedPath::from_path(path),
        database_present,
        main_file,
        sidecars,
        open_outcome: DatabaseOpenOutcome::MissingDatabase,
        journal_mode: None,
        quick_check: DatabaseCheckOutcome::not_run(),
        integrity_check: DatabaseCheckOutcome::not_run(),
        schema_version: None,
        diagnostics,
    };
    if !database_present {
        report.diagnostics.push(diagnostic(
            DatabaseDiagnosticCode::MissingDatabase,
            DatabaseDiagnosticSeverity::Error,
            "configured database does not exist",
            None,
        ));
        return report;
    }

    let connection = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ) {
        Ok(connection) => connection,
        Err(error) => {
            report.open_outcome = DatabaseOpenOutcome::Failed;
            report.diagnostics.push(sqlite_diagnostic(&error));
            return report;
        }
    };
    // rusqlite defaults to five seconds. A diagnostic should return promptly;
    // this changes only the connection's busy handler, never database bytes.
    if let Err(error) = connection.busy_timeout(Duration::from_millis(250)) {
        push_unique_diagnostic(&mut report.diagnostics, sqlite_diagnostic(&error));
    }
    report.open_outcome = DatabaseOpenOutcome::OpenedReadOnly;

    match connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0)) {
        Ok(mode) => report.journal_mode = Some(mode.to_ascii_lowercase()),
        Err(error) => push_unique_diagnostic(&mut report.diagnostics, sqlite_diagnostic(&error)),
    }
    match connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)) {
        Ok(version) => {
            report.schema_version = Some(version);
            if version > latest_known_version(MIGRATIONS) {
                report.diagnostics.push(diagnostic(
                    DatabaseDiagnosticCode::SchemaVersionUnsupported,
                    DatabaseDiagnosticSeverity::Error,
                    "database schema is newer than this ArchiveFS build supports",
                    None,
                ));
            }
        }
        Err(error) => push_unique_diagnostic(&mut report.diagnostics, sqlite_diagnostic(&error)),
    }
    report.quick_check = run_check(&connection, "PRAGMA quick_check", &mut report.diagnostics);
    if report
        .diagnostics
        .iter()
        .any(|item| item.code == DatabaseDiagnosticCode::RollbackRecoveryRequired)
    {
        // SQLite opens connections lazily. Creating the connection can
        // succeed even though the first real read discovers a hot journal
        // whose rollback requires write access. Report the usable open as
        // failed rather than claiming the database opened read-only.
        report.open_outcome = DatabaseOpenOutcome::Failed;
    }
    let _ = connection.close();
    report
}

fn run_check(
    connection: &Connection,
    sql: &str,
    diagnostics: &mut Vec<DatabaseDiagnostic>,
) -> DatabaseCheckOutcome {
    let result = (|| -> rusqlite::Result<Vec<String>> {
        let mut statement = connection.prepare(sql)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
    })();
    match result {
        Ok(messages) if messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok") => {
            DatabaseCheckOutcome {
                status: DatabaseCheckStatus::Ok,
                messages,
            }
        }
        Ok(messages) => {
            diagnostics.push(diagnostic(
                DatabaseDiagnosticCode::IntegrityCheckFailed,
                DatabaseDiagnosticSeverity::Error,
                "SQLite quick_check reported a consistency failure",
                None,
            ));
            DatabaseCheckOutcome {
                status: DatabaseCheckStatus::Failed,
                messages,
            }
        }
        Err(error) => {
            push_unique_diagnostic(diagnostics, sqlite_diagnostic(&error));
            DatabaseCheckOutcome {
                status: DatabaseCheckStatus::Error,
                messages: Vec::new(),
            }
        }
    }
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

const SQLITE_ROLLBACK_JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

fn inspect_rollback_journal_header(
    path: &Path,
    metadata: &fs::Metadata,
) -> std::io::Result<RollbackJournalHeaderState> {
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "rollback journal path is not a regular file",
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "rollback journal path is not a regular file",
        ));
    }
    if opened_metadata.len() < SQLITE_ROLLBACK_JOURNAL_MAGIC.len() as u64 {
        return Ok(RollbackJournalHeaderState::TruncatedNonHot);
    }
    let mut header = [0_u8; SQLITE_ROLLBACK_JOURNAL_MAGIC.len()];
    file.read_exact(&mut header)?;
    Ok(if opened_metadata.len() <= 512 {
        RollbackJournalHeaderState::TruncatedNonHot
    } else if header == SQLITE_ROLLBACK_JOURNAL_MAGIC {
        RollbackJournalHeaderState::HotCandidate
    } else if header == [0; SQLITE_ROLLBACK_JOURNAL_MAGIC.len()] {
        RollbackJournalHeaderState::ZeroedNonHot
    } else {
        RollbackJournalHeaderState::Malformed
    })
}

fn file_finding(path: &Path, metadata: &fs::Metadata) -> DatabaseFileFinding {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    DatabaseFileFinding {
        path: EncodedPath::from_path(path),
        size_bytes: metadata.len(),
        #[cfg(unix)]
        permissions_mode: Some(metadata.permissions().mode() & 0o7777),
        #[cfg(not(unix))]
        permissions_mode: None,
        #[cfg(unix)]
        owner_uid: Some(metadata.uid()),
        #[cfg(not(unix))]
        owner_uid: None,
        #[cfg(unix)]
        group_gid: Some(metadata.gid()),
        #[cfg(not(unix))]
        group_gid: None,
        #[cfg(unix)]
        inode: Some(metadata.ino()),
        #[cfg(not(unix))]
        inode: None,
        modified_unix_seconds: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64),
    }
}

fn diagnostic(
    code: DatabaseDiagnosticCode,
    severity: DatabaseDiagnosticSeverity,
    message: impl Into<String>,
    raw: Option<String>,
) -> DatabaseDiagnostic {
    DatabaseDiagnostic {
        code,
        severity,
        message: message.into(),
        sqlite_extended_code: None,
        raw_sqlite_message: raw,
    }
}

fn push_unique_diagnostic(
    diagnostics: &mut Vec<DatabaseDiagnostic>,
    diagnostic: DatabaseDiagnostic,
) {
    if !diagnostics.iter().any(|existing| existing == &diagnostic) {
        diagnostics.push(diagnostic);
    }
}

fn io_diagnostic(context: &str, error: &std::io::Error) -> DatabaseDiagnostic {
    let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
        DatabaseDiagnosticCode::PermissionDenied
    } else {
        DatabaseDiagnosticCode::IoError
    };
    diagnostic(
        code,
        DatabaseDiagnosticSeverity::Error,
        context,
        Some(error.to_string()),
    )
}

fn sqlite_diagnostic(error: &rusqlite::Error) -> DatabaseDiagnostic {
    let raw = error.to_string();
    let (code, extended_code) = match error.sqlite_error() {
        Some(sqlite) => {
            let code = if sqlite.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK {
                DatabaseDiagnosticCode::RollbackRecoveryRequired
            } else {
                match sqlite.code {
                    ErrorCode::PermissionDenied => DatabaseDiagnosticCode::PermissionDenied,
                    ErrorCode::CannotOpen
                        if raw.to_ascii_lowercase().contains("permission denied") =>
                    {
                        DatabaseDiagnosticCode::PermissionDenied
                    }
                    ErrorCode::DatabaseLocked => DatabaseDiagnosticCode::DatabaseLocked,
                    ErrorCode::DatabaseBusy => DatabaseDiagnosticCode::DatabaseBusy,
                    ErrorCode::DatabaseCorrupt => DatabaseDiagnosticCode::CorruptDatabase,
                    ErrorCode::NotADatabase => DatabaseDiagnosticCode::MalformedDatabase,
                    ErrorCode::SystemIoFailure => DatabaseDiagnosticCode::IoError,
                    _ => DatabaseDiagnosticCode::SqliteError,
                }
            };
            (code, Some(sqlite.extended_code))
        }
        None => (DatabaseDiagnosticCode::SqliteError, None),
    };
    DatabaseDiagnostic {
        code,
        severity: DatabaseDiagnosticSeverity::Error,
        message: if code == DatabaseDiagnosticCode::RollbackRecoveryRequired {
            "SQLite requires rollback recovery, which a read-only diagnostic must not perform; preserve the database and sidecars, close active users cleanly, and follow the recovery procedure"
                .to_string()
        } else {
            "SQLite operation failed".to_string()
        },
        sqlite_extended_code: extended_code,
        raw_sqlite_message: Some(raw),
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| {
        ArchiveFsError::Database(format!("failed to open {}: {error}", path.display()))
    })?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| {
            ArchiveFsError::Database(format!(
                "failed to configure database busy timeout: {error}"
            ))
        })?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| {
            ArchiveFsError::Database(format!("failed to enable foreign keys: {error}"))
        })?;
    Ok(connection)
}

fn open_read_only_connection(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| read_only_database_error(path, "open database", &error))
}

fn read_only_database_error(
    path: &Path,
    operation: &str,
    error: &rusqlite::Error,
) -> ArchiveFsError {
    if error
        .sqlite_error()
        .is_some_and(|sqlite| sqlite.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK)
    {
        return ArchiveFsError::Database(format!(
            "failed to {operation} at {} read-only because SQLite requires rollback recovery; preserve the database and sidecars, close active users cleanly, and follow the copy-first recovery procedure: {error}",
            path.display()
        ));
    }
    ArchiveFsError::Database(format!(
        "failed to {operation} at {} read-only: {error}",
        path.display()
    ))
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

    match open_read_only_connection(&path) {
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
pub fn format_unix_timestamp_utc(unix_seconds: i64) -> String {
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

/// Whether the most recent scan attempt of one source folder succeeded or
/// failed - see the `source_folders.last_scan_status` column added by
/// migration 0003. Deliberately just these two variants: distinguishing
/// *why* a failure happened (missing path vs. permission denied vs. some
/// other error) is done from `last_scan_error`'s text by
/// `classify_source_availability`, not encoded redundantly here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SourceScanStatus {
    Success,
    Failed,
}

impl SourceScanStatus {
    fn as_db_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }

    fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A full `source_folders` row, including migration 0003's scan-status
/// columns - the multi-source Sources page's per-source display data.
/// Does not include `enabled` (a config-owned fact, not a database one);
/// callers building a full display view join this against
/// `Vec<SourceFolderConfig>` by path (see `lib.rs`'s source-management
/// functions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFolderRecord {
    pub id: i64,
    pub path: PathBuf,
    pub first_seen_at: String,
    pub last_scan_status: Option<SourceScanStatus>,
    pub last_scan_error: Option<String>,
    pub last_scan_at: Option<String>,
    pub last_successful_scan_at: Option<String>,
    pub last_archive_count: Option<i64>,
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
    pub skipped_unsupported_extension: i64,
    pub skipped_ambiguous_platform: i64,
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
    /// Reliable timestamp updated whenever a successful scan observes this
    /// archive. Retained while the row is missing for review display.
    pub last_seen_at: String,
    pub last_verified_missing_at: Option<String>,
}

/// One automatically detected platform and the path evidence retained for a
/// human-readable explanation. `matched_component` is populated only for a
/// custom or built-in folder alias; heuristic detections deliberately leave it
/// empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticPlatformDetails {
    pub platform: String,
    pub source: String,
    pub matched_component: Option<String>,
}

/// Read-time provenance details for one archive. The effective assignment is
/// copied from the current assignment row, while `automatic_fallback` is the
/// latest recorded non-manual assignment: exactly the row
/// [`Database::clear_manual_platform`] would restore. Path evidence is
/// recomputed only to attach a matching alias/folder label, never to invent a
/// different fallback before a scan has recorded it. No schema change is
/// required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformProvenanceDetails {
    pub platform: Option<String>,
    pub source: Option<String>,
    pub matched_component: Option<String>,
    pub automatic_fallback: Option<AutomaticPlatformDetails>,
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

/// The result of one [`Database::set_manual_platform_for_archives`] or
/// [`Database::clear_manual_platform_for_archives`] call.
///
/// `requested` counts *distinct* archive ids after deduplication - a
/// duplicate id in the input never inflates this count and never
/// produces a second history row (see both methods' doc comments).
/// `changed` and `unchanged` count exactly one of the two ways processing
/// a distinct, existing id can go: `changed` means a new
/// `platform_assignments` row became current for that archive; `unchanged`
/// means the archive already had the effective result the caller asked
/// for, so nothing was written (matching `set_manual_platform`/
/// `clear_manual_platform`'s existing per-row no-op behavior exactly -
/// see `Database::set_manual_platform_for_archives` for the precise
/// conditions).
/// `missing` lists every requested id that does not name any archive in
/// this database, in the order first requested - never silently dropped,
/// and never cause the ids that *do* exist to go unprocessed (see the
/// "missing-id policy" note on `set_manual_platform_for_archives`).
/// `requested == changed + unchanged + missing.len()` always holds.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct BulkPlatformAssignmentResult {
    pub requested: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub missing: Vec<i64>,
}

/// Result of atomically removing missing archive records from the catalogue.
/// `requested` counts distinct archive ids after deduplication; every accepted
/// id is removed, so a successful result always has `requested == removed`.
/// Unknown or currently-present ids reject the operation before any delete.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct MissingArchiveRemovalResult {
    pub requested: usize,
    pub removed: usize,
    pub archive_ids: Vec<i64>,
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
        ArchiveKind::MegaDriveRom => "megadrive_rom",
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

fn format_archive_ids(ids: &[i64]) -> String {
    ids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
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

    /// Every source folder not removed from config (`removed_from_config_at
    /// IS NULL`), with its scan-status columns - the multi-source Sources
    /// page's data source. Ordered by `first_seen_at` so the display order
    /// is stable across renders instead of depending on SQLite's
    /// unspecified row order.
    pub fn list_source_folders(&self) -> Result<Vec<SourceFolderRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, path, first_seen_at, last_scan_status, last_scan_error, \
                 last_scan_at, last_successful_scan_at, last_archive_count \
                 FROM source_folders WHERE removed_from_config_at IS NULL \
                 ORDER BY first_seen_at ASC, id ASC",
            )
            .map_err(|error| db_error("failed to prepare source folder listing", error))?;

        let rows = statement
            .query_map([], |row| {
                let path_bytes: Vec<u8> = row.get(1)?;
                let status: Option<String> = row.get(3)?;
                Ok(SourceFolderRecord {
                    id: row.get(0)?,
                    path: PathBuf::from(OsString::from_vec(path_bytes)),
                    first_seen_at: row.get(2)?,
                    last_scan_status: status
                        .and_then(|status| SourceScanStatus::from_db_str(&status)),
                    last_scan_error: row.get(4)?,
                    last_scan_at: row.get(5)?,
                    last_successful_scan_at: row.get(6)?,
                    last_archive_count: row.get(7)?,
                })
            })
            .map_err(|error| db_error("failed to list source folders", error))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| db_error("failed to read source folders", error))?;

        Ok(rows)
    }

    /// Records the outcome of one source folder's scan attempt (success or
    /// failure) into its `source_folders` row - always called exactly
    /// once per folder per scan pass, whether that folder's
    /// `ArchiveScanner::scan_archives` call itself failed or the
    /// filesystem walk succeeded but persistence (`persist_one_folder`)
    /// then failed. `last_successful_scan_at` and `last_archive_count`
    /// only advance on `Success` - a failed attempt never overwrites the
    /// last genuinely known archive count, matching the "preserve
    /// catalogue on an unavailable source" safety requirement.
    fn record_source_scan_result(
        &mut self,
        source_folder_id: i64,
        outcome: SourceScanStatus,
        error: Option<&str>,
        archive_count: Option<i64>,
    ) -> Result<()> {
        let now = now_utc_string();
        match outcome {
            SourceScanStatus::Success => {
                self.connection
                    .execute(
                        "UPDATE source_folders SET last_scan_status = ?2, last_scan_error = NULL, \
                         last_scan_at = ?3, last_successful_scan_at = ?3, last_archive_count = ?4 \
                         WHERE id = ?1",
                        params![
                            source_folder_id,
                            SourceScanStatus::Success.as_db_str(),
                            now,
                            archive_count
                        ],
                    )
                    .map_err(|error| db_error("failed to record source scan success", error))?;
            }
            SourceScanStatus::Failed => {
                self.connection
                    .execute(
                        "UPDATE source_folders SET last_scan_status = ?2, last_scan_error = ?3, \
                         last_scan_at = ?4 WHERE id = ?1",
                        params![
                            source_folder_id,
                            SourceScanStatus::Failed.as_db_str(),
                            error,
                            now
                        ],
                    )
                    .map_err(|error| db_error("failed to record source scan failure", error))?;
            }
        }
        Ok(())
    }

    /// Starts a new `scan_runs` row with `status = 'running'` and returns
    /// its id. Direct callers commit according to their surrounding SQLite
    /// transaction; the catalogue refresh pipeline includes this row in its
    /// all-or-nothing refresh transaction.
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
    /// `'interrupted'`. The refresh pipeline acquires an immediate write
    /// transaction before calling this method, so a concurrent refresh
    /// cannot have a live row misclassified as interrupted. Returns how
    /// many rows were fixed.
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
            .savepoint()
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
            .savepoint()
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

    /// Deduplicates `archive_ids`, preserving the order of first
    /// occurrence - the shared first step of
    /// [`Self::set_manual_platform_for_archives`] and
    /// [`Self::clear_manual_platform_for_archives`], so a duplicate id in
    /// the caller's selection (for example the same archive reachable by
    /// both `--id` and `--path` on the CLI, or a GUI multi-selection that
    /// somehow contains a repeat) is processed - and can produce at most
    /// one history row - exactly once.
    fn deduplicate_archive_ids(archive_ids: &[i64]) -> Vec<i64> {
        let mut seen = HashSet::new();
        archive_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect()
    }

    /// Removes the selected archive records only when every distinct id names
    /// an archive already marked missing. Validation and all deletes happen in
    /// one transaction: an unknown id, a present archive, or any SQLite error
    /// rejects/rolls back the whole operation.
    ///
    /// The initial schema does not cascade archive deletion to its two child
    /// tables, so related `platform_assignments` and
    /// `archive_scan_observations` rows are deleted explicitly before the
    /// parent. `scan_runs`, `platform_aliases`, source folders, and unrelated
    /// archives are never deleted. This method performs database operations
    /// only; it never accesses or mutates an archive path.
    pub fn remove_missing_archives(
        &mut self,
        archive_ids: &[i64],
    ) -> Result<MissingArchiveRemovalResult> {
        let ids = Self::deduplicate_archive_ids(archive_ids);
        if ids.is_empty() {
            return Err(ArchiveFsError::Database(
                "at least one archive id is required".to_string(),
            ));
        }

        let tx = self.connection.transaction().map_err(|error| {
            db_error("failed to start remove_missing_archives transaction", error)
        })?;
        let mut unknown = Vec::new();
        let mut present = Vec::new();
        for archive_id in &ids {
            let missing_at: Option<Option<String>> = tx
                .query_row(
                    "SELECT last_verified_missing_at FROM archives WHERE id = ?1",
                    params![archive_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| db_error("failed to validate missing archive removal", error))?;
            match missing_at {
                None => unknown.push(*archive_id),
                Some(None) => present.push(*archive_id),
                Some(Some(_)) => {}
            }
        }
        if !unknown.is_empty() {
            return Err(ArchiveFsError::Database(format!(
                "archive id(s) not found: {}. No catalogue entries were removed",
                format_archive_ids(&unknown)
            )));
        }
        if !present.is_empty() {
            return Err(ArchiveFsError::Database(format!(
                "archive id(s) are currently present: {}. Only missing catalogue entries can be removed; no catalogue entries were removed",
                format_archive_ids(&present)
            )));
        }

        for archive_id in &ids {
            tx.execute(
                "DELETE FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
            )
            .map_err(|error| {
                db_error(
                    "failed to remove platform history for missing archive",
                    error,
                )
            })?;
            tx.execute(
                "DELETE FROM archive_scan_observations WHERE archive_id = ?1",
                params![archive_id],
            )
            .map_err(|error| {
                db_error(
                    "failed to remove scan observations for missing archive",
                    error,
                )
            })?;
            tx.execute("DELETE FROM archives WHERE id = ?1", params![archive_id])
                .map_err(|error| db_error("failed to remove missing archive", error))?;
        }

        tx.commit()
            .map_err(|error| db_error("failed to commit remove_missing_archives", error))?;
        Ok(MissingArchiveRemovalResult {
            requested: ids.len(),
            removed: ids.len(),
            archive_ids: ids,
        })
    }

    /// The exact number of catalogue rows a
    /// [`Self::remove_source_folder_catalogue`] call for `source_folder_id`
    /// would delete - read-only, for a removal confirmation dialog to show
    /// the precise count *before* the user commits to anything.
    pub fn count_archives_for_source_folder(&self, source_folder_id: i64) -> Result<i64> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM archives WHERE source_folder_id = ?1",
                params![source_folder_id],
                |row| row.get(0),
            )
            .map_err(|error| db_error("failed to count archives for source folder", error))
    }

    /// Deletes every catalogue entry owned by `source_folder_id` - present
    /// or missing alike, unlike [`Self::remove_missing_archives`], since
    /// removing an entire source's catalogue is a deliberate "stop
    /// tracking this source" action, not a missing-entry cleanup. Same
    /// explicit child-then-parent delete order as
    /// `remove_missing_archives` (this schema has no cascading deletes),
    /// all in one transaction: a failure partway through rolls back
    /// completely rather than leaving a source half-cleaned. Only rows
    /// whose `source_folder_id` matches are ever touched - no other
    /// source's archives, and no filesystem content, are affected. Returns
    /// the number of archive rows removed.
    ///
    /// Does not remove the `source_folders` row itself (that stays,
    /// marked `removed_from_config_at` by the caller's config save +
    /// `register_source_folders` cycle) - this only clears its owned
    /// catalogue rows.
    pub fn remove_source_folder_catalogue(&mut self, source_folder_id: i64) -> Result<usize> {
        let tx = self.connection.transaction().map_err(|error| {
            db_error(
                "failed to start remove_source_folder_catalogue transaction",
                error,
            )
        })?;

        let archive_ids: Vec<i64> = {
            let mut statement = tx
                .prepare("SELECT id FROM archives WHERE source_folder_id = ?1")
                .map_err(|error| db_error("failed to prepare source folder archive scan", error))?;
            statement
                .query_map(params![source_folder_id], |row| row.get(0))
                .map_err(|error| db_error("failed to list source folder archives", error))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|error| db_error("failed to read source folder archives", error))?
        };

        for archive_id in &archive_ids {
            tx.execute(
                "DELETE FROM platform_assignments WHERE archive_id = ?1",
                params![archive_id],
            )
            .map_err(|error| {
                db_error(
                    "failed to remove platform history for source folder archive",
                    error,
                )
            })?;
            tx.execute(
                "DELETE FROM archive_scan_observations WHERE archive_id = ?1",
                params![archive_id],
            )
            .map_err(|error| {
                db_error(
                    "failed to remove scan observations for source folder archive",
                    error,
                )
            })?;
        }
        tx.execute(
            "DELETE FROM archives WHERE source_folder_id = ?1",
            params![source_folder_id],
        )
        .map_err(|error| db_error("failed to remove source folder archives", error))?;

        tx.commit()
            .map_err(|error| db_error("failed to commit remove_source_folder_catalogue", error))?;
        Ok(archive_ids.len())
    }

    /// Sets `platform` as a manual, user-chosen platform assignment for
    /// every archive in `archive_ids` in a single transaction - the batch
    /// counterpart to [`Self::set_manual_platform`]. Every existing
    /// caller of the single-row method is unaffected by this one; it does
    /// not call or delegate to it (a loop calling `set_manual_platform`
    /// would open one transaction per archive, which this exists
    /// specifically to avoid).
    ///
    /// Per-archive precedence is identical to `set_manual_platform`: a
    /// manual assignment always wins over automatic detection
    /// (`provenance_priority`'s top tier), and this call itself never
    /// blocks on priority - it *is* how a user replaces one manual choice
    /// with another, in bulk. An archive already carrying this exact
    /// manual platform is counted `unchanged`, not `changed` -
    /// re-confirming the same value never grows history.
    ///
    /// Missing-id policy (deliberately atomic-but-not-all-or-nothing): an
    /// id in `archive_ids` that does not name any archive in this
    /// database is skipped and reported in the result's `missing` list,
    /// rather than aborting the whole call - a stale id must not prevent
    /// every other, valid id in the same batch from being updated. What
    /// *is* atomic is the set of writes actually performed: they all
    /// happen in one transaction, so a genuine failure partway through
    /// (a database error, not a missing id) rolls every one of them back
    /// - see `bulk_set_manual_platform_transaction_rolls_back_on_failure`.
    ///
    /// Rejects `platform` if it is empty or whitespace-only, exactly like
    /// `set_manual_platform` - checked once, up front, before any row is
    /// touched, so an invalid platform never partially applies.
    pub fn set_manual_platform_for_archives(
        &mut self,
        archive_ids: &[i64],
        platform: &str,
    ) -> Result<BulkPlatformAssignmentResult> {
        if platform.trim().is_empty() {
            return Err(ArchiveFsError::Database(
                "manual platform must not be empty or whitespace-only".to_string(),
            ));
        }

        let ids = Self::deduplicate_archive_ids(archive_ids);
        let mut result = BulkPlatformAssignmentResult {
            requested: ids.len(),
            ..BulkPlatformAssignmentResult::default()
        };

        let tx = self.connection.transaction().map_err(|error| {
            db_error(
                "failed to start set_manual_platform_for_archives transaction",
                error,
            )
        })?;
        let now = now_utc_string();

        for archive_id in &ids {
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM archives WHERE id = ?1",
                    params![archive_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| db_error("failed to check archive existence", error))?
                .is_some();
            if !exists {
                result.missing.push(*archive_id);
                continue;
            }

            let current: Option<(String, String)> = tx
                .query_row(
                    "SELECT platform, source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                    params![archive_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| db_error("failed to look up current platform assignment", error))?;

            if current
                .as_ref()
                .is_some_and(|(current_platform, current_source)| {
                    current_platform == platform && current_source == MANUAL_PLATFORM_SOURCE
                })
            {
                result.unchanged += 1;
                continue;
            }

            tx.execute(
                "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
                params![archive_id],
            )
            .map_err(|error| db_error("failed to retire previous platform assignment", error))?;
            tx.execute(
                "INSERT INTO platform_assignments (archive_id, platform, source, is_current, assigned_at) \
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![archive_id, platform, MANUAL_PLATFORM_SOURCE, now],
            )
            .map_err(|error| db_error("failed to insert manual platform assignment", error))?;
            result.changed += 1;
        }

        tx.commit().map_err(|error| {
            db_error("failed to commit set_manual_platform_for_archives", error)
        })?;
        Ok(result)
    }

    /// Clears a manual platform assignment for every archive in
    /// `archive_ids` in a single transaction - the batch counterpart to
    /// [`Self::clear_manual_platform`], which it does not call or
    /// delegate to (see [`Self::set_manual_platform_for_archives`]'s doc
    /// comment for why - the same reasoning applies here).
    ///
    /// Per archive, this is identical to `clear_manual_platform`: an
    /// archive whose current assignment is not manual (no assignment at
    /// all, or an automatic one) is a no-op, counted `unchanged` - never
    /// touches an assignment made a different way. An archive whose
    /// current assignment *is* manual has it retired and, in the same
    /// transaction, the most recent automatic assignment still in this
    /// archive's history (any row whose source is not
    /// [`MANUAL_PLATFORM_SOURCE`] - this already includes
    /// [`CUSTOM_FOLDER_ALIAS_SOURCE`] results, so a custom alias fallback
    /// is restored exactly like any other automatic source) restored as
    /// current immediately, without needing a rescan; if there is no such
    /// row, the archive becomes current-less (`Unknown`), exactly as
    /// `clear_manual_platform` already behaves. Counted `changed`.
    ///
    /// Missing-id policy is identical to
    /// [`Self::set_manual_platform_for_archives`]: a stale id is skipped
    /// and reported in `missing`, never aborting the rest of the batch;
    /// the writes that do happen are still one atomic transaction.
    pub fn clear_manual_platform_for_archives(
        &mut self,
        archive_ids: &[i64],
    ) -> Result<BulkPlatformAssignmentResult> {
        let ids = Self::deduplicate_archive_ids(archive_ids);
        let mut result = BulkPlatformAssignmentResult {
            requested: ids.len(),
            ..BulkPlatformAssignmentResult::default()
        };

        let tx = self.connection.transaction().map_err(|error| {
            db_error(
                "failed to start clear_manual_platform_for_archives transaction",
                error,
            )
        })?;

        for archive_id in &ids {
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM archives WHERE id = ?1",
                    params![archive_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| db_error("failed to check archive existence", error))?
                .is_some();
            if !exists {
                result.missing.push(*archive_id);
                continue;
            }

            let current_source: Option<String> = tx
                .query_row(
                    "SELECT source FROM platform_assignments WHERE archive_id = ?1 AND is_current = 1",
                    params![archive_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| db_error("failed to look up current platform assignment", error))?;

            if current_source.as_deref() != Some(MANUAL_PLATFORM_SOURCE) {
                result.unchanged += 1;
                continue;
            }

            tx.execute(
                "UPDATE platform_assignments SET is_current = 0 WHERE archive_id = ?1 AND is_current = 1",
                params![archive_id],
            )
            .map_err(|error| db_error("failed to clear manual platform assignment", error))?;

            let latest_automatic_row_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM platform_assignments \
                     WHERE archive_id = ?1 AND source != ?2 \
                     ORDER BY id DESC LIMIT 1",
                    params![archive_id, MANUAL_PLATFORM_SOURCE],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| {
                    db_error(
                        "failed to look up latest automatic platform assignment",
                        error,
                    )
                })?;
            if let Some(row_id) = latest_automatic_row_id {
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
            }
            result.changed += 1;
        }

        tx.commit().map_err(|error| {
            db_error("failed to commit clear_manual_platform_for_archives", error)
        })?;
        Ok(result)
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
            .savepoint()
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
                 archives_updated = ?6, archives_missing = ?7, errors_count = ?8, error_message = ?9, \
                 archives_unchanged = ?10, skipped_unsupported_extension = ?11, \
                 skipped_ambiguous_platform = ?12 \
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
                    counts.archives_unchanged,
                    counts.skipped_unsupported_extension,
                    counts.skipped_ambiguous_platform,
                ],
            )
            .map_err(|error| db_error("failed to complete scan run", error))?;
        Ok(())
    }

    /// Marks `scan_run_id` as `'failed'` with `error_message`, for a
    /// fatal error that stopped the run before it could complete. When used
    /// inside the catalogue refresh transaction, a later rollback also
    /// rolls this status update back together with every catalogue mutation.
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
                 a.last_seen_at, a.last_verified_missing_at \
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
                    last_seen_at: row.get(12)?,
                    last_verified_missing_at: row.get(13)?,
                })
            })
            .map_err(|error| db_error("failed to query archives", error))?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| db_error("failed to read archives", error))
    }

    /// Builds display-oriented provenance details for already-loaded archive
    /// rows without changing their effective assignments. Source roots and
    /// custom aliases are loaded once. A manual row's fallback comes from the
    /// same latest automatic history row that `clear_manual_platform`
    /// restores, so display and behavior cannot disagree. Current path and
    /// alias evidence is used only to attach a matched component when it
    /// agrees with that recorded assignment.
    pub fn load_platform_provenance_details(
        &self,
        archives: &[PersistedArchive],
    ) -> Result<HashMap<i64, PlatformProvenanceDetails>> {
        let mut stmt = self
            .connection
            .prepare("SELECT id, path FROM source_folders")
            .map_err(|error| db_error("failed to prepare source folders for provenance", error))?;
        let source_roots = stmt
            .query_map([], |row| {
                let path_bytes: Vec<u8> = row.get(1)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(OsString::from_vec(path_bytes)),
                ))
            })
            .map_err(|error| db_error("failed to query source folders for provenance", error))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(|error| db_error("failed to read source folders for provenance", error))?;
        let aliases = self.list_platform_aliases()?;
        let mut stmt = self
            .connection
            .prepare(
                "SELECT p.archive_id, p.platform, p.source \
                 FROM platform_assignments p \
                 WHERE p.source != ?1 AND p.id = (\
                     SELECT MAX(latest.id) FROM platform_assignments latest \
                     WHERE latest.archive_id = p.archive_id AND latest.source != ?1\
                 )",
            )
            .map_err(|error| {
                db_error(
                    "failed to prepare automatic platform fallbacks for provenance",
                    error,
                )
            })?;
        let latest_automatic = stmt
            .query_map(params![MANUAL_PLATFORM_SOURCE], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                ))
            })
            .map_err(|error| {
                db_error(
                    "failed to query automatic platform fallbacks for provenance",
                    error,
                )
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(|error| {
                db_error(
                    "failed to read automatic platform fallbacks for provenance",
                    error,
                )
            })?;

        let mut details = HashMap::with_capacity(archives.len());
        for archive in archives {
            let detected_now =
                source_roots
                    .get(&archive.source_folder_id)
                    .and_then(|source_root| {
                        automatic_platform_details(&aliases, &archive.absolute_path, source_root)
                    });
            let matched_component = detected_now
                .as_ref()
                .filter(|automatic| {
                    archive.platform.as_deref() == Some(automatic.platform.as_str())
                        && archive.platform_source.as_deref() == Some(automatic.source.as_str())
                })
                .and_then(|automatic| automatic.matched_component.clone());
            let automatic_fallback = latest_automatic.get(&archive.id).map(|(platform, source)| {
                let matched_component = detected_now
                    .as_ref()
                    .filter(|detected| detected.platform == *platform && detected.source == *source)
                    .and_then(|detected| detected.matched_component.clone());
                AutomaticPlatformDetails {
                    platform: platform.clone(),
                    source: source.clone(),
                    matched_component,
                }
            });

            details.insert(
                archive.id,
                PlatformProvenanceDetails {
                    platform: archive.platform.clone(),
                    source: archive.platform_source.clone(),
                    matched_component,
                    automatic_fallback,
                },
            );
        }
        Ok(details)
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
                 errors_count, error_message, archives_unchanged, \
                 skipped_unsupported_extension, skipped_ambiguous_platform \
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
                        archives_unchanged: row.get(11)?,
                        skipped_unsupported_extension: row.get(12)?,
                        skipped_ambiguous_platform: row.get(13)?,
                    })
                },
            )
            .optional()
            .map_err(|error| db_error("failed to load latest completed scan", error))
    }

    /// Persistent additions from the newest completed scan. Partial-success
    /// scans are completed runs and retain their committed additions; failed
    /// transactions never appear here.
    pub fn latest_scan_additions(&self) -> Result<Option<RecentScanAdditions>> {
        const MAX_RECENT_ADDITIONS: usize = 10_000;
        let Some(scan) = self.latest_completed_scan()? else {
            return Ok(None);
        };
        let mut statement = self
            .connection
            .prepare(
                "SELECT archive_id FROM archive_scan_observations \
                 WHERE scan_run_id = ?1 AND observation = 'added' ORDER BY archive_id LIMIT ?2",
            )
            .map_err(|error| db_error("failed to prepare recent additions", error))?;
        let ids = statement
            .query_map(
                params![scan.scan_run_id, (MAX_RECENT_ADDITIONS + 1) as i64],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| db_error("failed to load recent additions", error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| db_error("failed to decode recent additions", error))?;
        let truncated = ids.len() > MAX_RECENT_ADDITIONS;
        let wanted: HashSet<i64> = ids.into_iter().take(MAX_RECENT_ADDITIONS).collect();
        let mut archives = self
            .load_archives()?
            .into_iter()
            .filter(|archive| wanted.contains(&archive.id))
            .collect::<Vec<_>>();
        archives.sort_by(|left, right| left.absolute_path.cmp(&right.absolute_path));
        Ok(Some(RecentScanAdditions {
            scan,
            archives,
            truncated,
        }))
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
    pub archives_unchanged: i64,
    pub skipped_unsupported_extension: i64,
    pub skipped_ambiguous_platform: i64,
    pub errors_count: i64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentScanAdditions {
    pub scan: CompletedScanSummary,
    pub archives: Vec<PersistedArchive>,
    pub truncated: bool,
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
    validate_configured_source_roots(&config.source_folders)?;
    let registered_folders = database.register_source_folders(&config.source_folders)?;
    scan_and_persist_folders(database, &registered_folders, triggered_by)
}

/// The shared scan+persist pipeline both [`scan_and_persist`] (the legacy
/// whole-config entry point) and the multi-source milestone's
/// `scan_source_folder`/`scan_all_enabled_sources` (see `lib.rs`) drive -
/// an explicit, already-registered list of folders, with per-folder
/// isolation: one folder's scanner or persistence failure is recorded in
/// [`ScanPersistSummary::folder_errors`] and that folder's own
/// `source_folders` status columns, without touching any other folder's
/// archives, status, or the overall run's success. This is the single
/// place archive discovery is ever walked and persisted - no caller
/// duplicates this loop.
///
/// Registration itself is deliberately the caller's responsibility, not
/// this function's: `scan_and_persist` only ever knows "enabled" folders
/// (`Config::source_folders`), while multi-source management must also
/// register disabled ones (to keep them "configured" rather than
/// "removed") without asking this function to scan them.
pub(crate) fn scan_and_persist_folders(
    database: &mut Database,
    folders: &[RegisteredSourceFolder],
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    database.begin_catalogue_refresh()?;
    let result = scan_and_persist_folders_transaction(database, folders, triggered_by);
    match result {
        Ok(summary) => {
            if let Err(error) = database.commit_catalogue_refresh() {
                database.rollback_catalogue_refresh();
                return Err(error);
            }
            Ok(summary)
        }
        Err(error) => {
            database.rollback_catalogue_refresh();
            Err(error)
        }
    }
}

fn scan_and_persist_folders_transaction(
    database: &mut Database,
    folders: &[RegisteredSourceFolder],
    triggered_by: &str,
) -> Result<ScanPersistSummary> {
    database.mark_interrupted_scan_runs()?;
    let scan_run_id = database.start_scan_run(triggered_by, None)?;

    let mut counts = ScanRunCounts::default();
    let mut folder_errors = Vec::new();

    for folder in folders {
        let folder_config = Config {
            source_folders: vec![folder.path.clone()],
            mount_root: PathBuf::new(),
            ratarmount_bin: String::new(),
        };

        let discovery = match ArchiveScanner::new(&folder_config).scan_archives_with_summary() {
            Ok(discovery) => discovery,
            Err(error) => {
                counts.errors_count += 1;
                let message = error.to_string();
                database.record_source_scan_result(
                    folder.id,
                    SourceScanStatus::Failed,
                    Some(&message),
                    None,
                )?;
                folder_errors.push((folder.path.clone(), message));
                continue;
            }
        };
        counts.skipped_unsupported_extension += discovery.skipped_unsupported_extension as i64;
        counts.skipped_ambiguous_platform += discovery.skipped_ambiguous_platform as i64;
        let archives = discovery.archives;

        database.begin_folder_refresh()?;
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
                database.record_source_scan_result(
                    folder.id,
                    SourceScanStatus::Success,
                    None,
                    Some(archives.len() as i64),
                )?;
                database.commit_folder_refresh()?;
            }
            Err(error) => {
                database.rollback_folder_refresh()?;
                counts.errors_count += 1;
                let message = error.to_string();
                database.record_source_scan_result(
                    folder.id,
                    SourceScanStatus::Failed,
                    Some(&message),
                    None,
                )?;
                folder_errors.push((folder.path.clone(), message));
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
        revalidate_archive_for_catalogue(archive)?;
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

fn automatic_platform_details(
    aliases: &[PlatformAlias],
    path: &Path,
    source_root: &Path,
) -> Option<AutomaticPlatformDetails> {
    if let Some((platform, matched_alias)) =
        find_custom_platform_alias_in(aliases, path, source_root)
    {
        return Some(AutomaticPlatformDetails {
            platform,
            source: CUSTOM_FOLDER_ALIAS_SOURCE.to_string(),
            matched_component: Some(matched_alias),
        });
    }

    detect_platform_with_details(path, source_root).map(|detection| AutomaticPlatformDetails {
        platform: detection.platform,
        source: detection.provenance.as_source_str().to_string(),
        matched_component: detection.matched_folder,
    })
}

fn find_custom_platform_alias_in(
    aliases: &[PlatformAlias],
    path: &Path,
    source_root: &Path,
) -> Option<(String, String)> {
    let relative = path.strip_prefix(source_root).ok()?;
    let mut components: Vec<_> = relative.components().collect();
    components.pop();

    for component in components.iter().rev() {
        let normalized = normalize_path_segment(&component.as_os_str().to_string_lossy());
        if let Some(alias) = aliases
            .iter()
            .find(|alias| alias.normalized_alias == normalized)
        {
            return Some((alias.platform.clone(), alias.alias.clone()));
        }
    }
    None
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
    fn read_only_open_never_creates_a_missing_database_or_parent() {
        let root = temp_dir("read-only-open-missing");
        let database_path = root.join("missing").join("library.sqlite3");

        let error = Database::open_read_only(&database_path).unwrap_err();

        assert!(error.to_string().contains("does not exist"));
        assert!(!database_path.exists());
        assert!(!database_path.parent().unwrap().exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_open_does_not_migrate_write_or_create_sidecars() {
        let root = temp_dir("read-only-open-no-writes");
        let database_path = root.join("library.sqlite3");
        Database::open_or_create(&database_path)
            .unwrap()
            .close()
            .unwrap();
        let before_bytes = fs::read(&database_path).unwrap();
        let mut before_entries = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        before_entries.sort();

        let database = Database::open_read_only(&database_path).unwrap();
        assert_eq!(database.schema_version().unwrap(), latest_schema_version());
        assert!(database.load_archives().unwrap().is_empty());
        let write_error = database
            .connection
            .execute("DELETE FROM archives", [])
            .unwrap_err();
        assert!(write_error.to_string().contains("readonly"));
        database.close().unwrap();

        let mut after_entries = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        after_entries.sort();
        assert_eq!(after_entries, before_entries);
        assert_eq!(fs::read(&database_path).unwrap(), before_bytes);
        assert!(!root.join("library.sqlite3-wal").exists());
        assert!(!root.join("library.sqlite3-shm").exists());
        assert!(!root.join("library.sqlite3-journal").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_open_rejects_an_outdated_schema_without_migrating_it() {
        let root = temp_dir("read-only-open-outdated");
        let database_path = root.join("library.sqlite3");
        let connection = Connection::open(&database_path).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection.close().unwrap();
        let before_bytes = fs::read(&database_path).unwrap();

        let error = Database::open_read_only(&database_path).unwrap_err();

        assert!(error.to_string().contains("refusing to migrate or repair"));
        assert_eq!(fs::read(&database_path).unwrap(), before_bytes);
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

    #[cfg(unix)]
    #[test]
    fn writable_database_open_refuses_a_symlinked_database_file() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("writable-database-symlink");
        let real_path = root.join("real.sqlite3");
        Database::open_or_create(&real_path)
            .unwrap()
            .close()
            .unwrap();
        let linked_path = root.join("linked.sqlite3");
        symlink(&real_path, &linked_path).unwrap();

        let error = Database::open_or_create(&linked_path).unwrap_err();

        assert!(error.to_string().contains("failed to open"));
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
    fn database_diagnostic_reports_healthy_database_without_writing() {
        let root = temp_dir("database-diagnostic-healthy");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let before = fs::read(&path).unwrap();
        let before_metadata = fs::metadata(&path).unwrap();

        let report = diagnose_database(&path);

        assert_eq!(report.format_version, 2);
        assert!(report.database_present);
        assert_eq!(report.open_outcome, DatabaseOpenOutcome::OpenedReadOnly);
        assert_eq!(report.schema_version, Some(latest_schema_version()));
        assert_eq!(report.quick_check.status, DatabaseCheckStatus::Ok);
        assert_eq!(report.integrity_check.status, DatabaseCheckStatus::NotRun);
        assert!(report.sidecars.iter().all(|sidecar| !sidecar.present));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().len(), before_metadata.len());
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            before_metadata.modified().unwrap()
        );
        assert!(!sidecar_path(&path, "-journal").exists());
        assert!(!sidecar_path(&path, "-wal").exists());
        assert!(!sidecar_path(&path, "-shm").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_missing_database_creates_nothing() {
        let root = temp_dir("database-diagnostic-missing");
        let path = root.join("missing-parent").join("library.sqlite3");

        let report = diagnose_database(&path);

        assert!(!report.database_present);
        assert_eq!(report.open_outcome, DatabaseOpenOutcome::MissingDatabase);
        assert_eq!(report.quick_check.status, DatabaseCheckStatus::NotRun);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::MissingDatabase })
        );
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_reports_sidecars_without_modifying_them() {
        let root = temp_dir("database-diagnostic-sidecars");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let fixtures = [
            ("-journal", b"journal fixture".as_slice()),
            ("-wal", b"wal fixture".as_slice()),
            ("-shm", b"shm fixture".as_slice()),
        ];
        for (suffix, bytes) in fixtures {
            fs::write(sidecar_path(&path, suffix), bytes).unwrap();
        }
        let before = fixtures
            .iter()
            .map(|(suffix, _)| {
                let sidecar = sidecar_path(&path, suffix);
                (
                    fs::read(&sidecar).unwrap(),
                    fs::metadata(&sidecar).unwrap().modified().unwrap(),
                )
            })
            .collect::<Vec<_>>();

        let report = diagnose_database(&path);

        assert!(report.sidecars.iter().all(|sidecar| sidecar.present));
        assert_eq!(
            report
                .sidecars
                .iter()
                .map(|sidecar| sidecar.size_bytes)
                .collect::<Vec<_>>(),
            vec![Some(15), Some(11), Some(11)]
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::RollbackJournalPresent })
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::NonHotRollbackJournal })
        );
        assert_eq!(
            report.sidecars[0].rollback_journal_header,
            Some(RollbackJournalHeaderState::TruncatedNonHot)
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| item.code == DatabaseDiagnosticCode::WalPresent)
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| item.code == DatabaseDiagnosticCode::ShmPresent)
        );
        for (index, (suffix, _)) in fixtures.iter().enumerate() {
            let sidecar = sidecar_path(&path, suffix);
            assert_eq!(fs::read(&sidecar).unwrap(), before[index].0);
            assert_eq!(
                fs::metadata(&sidecar).unwrap().modified().unwrap(),
                before[index].1
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_distinguishes_hot_candidate_journal_header() {
        let root = temp_dir("database-diagnostic-hot-journal-header");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let journal_path = sidecar_path(&path, "-journal");
        let mut journal = vec![0_u8; 8_720];
        journal[..SQLITE_ROLLBACK_JOURNAL_MAGIC.len()]
            .copy_from_slice(&SQLITE_ROLLBACK_JOURNAL_MAGIC);
        fs::write(&journal_path, &journal).unwrap();

        let report = diagnose_database(&path);

        assert_eq!(
            report.sidecars[0].rollback_journal_header,
            Some(RollbackJournalHeaderState::HotCandidate)
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::HotRollbackJournal })
        );
        assert_eq!(fs::read(&journal_path).unwrap(), journal);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_distinguishes_zeroed_and_malformed_journal_headers() {
        let root = temp_dir("database-diagnostic-non-hot-journal-headers");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let journal_path = sidecar_path(&path, "-journal");

        let zeroed = vec![0_u8; 8_720];
        fs::write(&journal_path, &zeroed).unwrap();
        let zeroed_report = diagnose_database(&path);
        assert_eq!(
            zeroed_report.sidecars[0].rollback_journal_header,
            Some(RollbackJournalHeaderState::ZeroedNonHot)
        );
        assert_eq!(fs::read(&journal_path).unwrap(), zeroed);

        let malformed = vec![0x5a_u8; 8_720];
        fs::write(&journal_path, &malformed).unwrap();
        let malformed_report = diagnose_database(&path);
        assert_eq!(
            malformed_report.sidecars[0].rollback_journal_header,
            Some(RollbackJournalHeaderState::Malformed)
        );
        assert!(
            malformed_report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::MalformedRollbackJournal })
        );
        assert_eq!(fs::read(&journal_path).unwrap(), malformed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_classifies_malformed_database() {
        let root = temp_dir("database-diagnostic-malformed");
        let path = root.join("library.sqlite3");
        fs::write(&path, b"this is not sqlite").unwrap();
        let before = fs::read(&path).unwrap();

        let report = diagnose_database(&path);

        assert!(report.diagnostics.iter().any(|item| {
            item.code == DatabaseDiagnosticCode::MalformedDatabase
                || item.code == DatabaseDiagnosticCode::CorruptDatabase
        }));
        assert_eq!(report.quick_check.status, DatabaseCheckStatus::Error);
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_reports_future_schema_without_migrating() {
        let root = temp_dir("database-diagnostic-future-schema");
        let path = root.join("library.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 999).unwrap();
        connection.close().unwrap();
        let before = fs::read(&path).unwrap();

        let report = diagnose_database(&path);

        assert_eq!(report.schema_version, Some(999));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::SchemaVersionUnsupported })
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_error_code_classification_is_stable() {
        let cases = [
            (
                rusqlite::ffi::SQLITE_BUSY,
                DatabaseDiagnosticCode::DatabaseBusy,
            ),
            (
                rusqlite::ffi::SQLITE_LOCKED,
                DatabaseDiagnosticCode::DatabaseLocked,
            ),
            (
                rusqlite::ffi::SQLITE_PERM,
                DatabaseDiagnosticCode::PermissionDenied,
            ),
            (
                rusqlite::ffi::SQLITE_CORRUPT,
                DatabaseDiagnosticCode::CorruptDatabase,
            ),
            (
                rusqlite::ffi::SQLITE_NOTADB,
                DatabaseDiagnosticCode::MalformedDatabase,
            ),
            (rusqlite::ffi::SQLITE_IOERR, DatabaseDiagnosticCode::IoError),
            (
                rusqlite::ffi::SQLITE_READONLY_ROLLBACK,
                DatabaseDiagnosticCode::RollbackRecoveryRequired,
            ),
        ];
        for (sqlite_code, expected) in cases {
            let error = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(sqlite_code),
                Some("fixture".to_string()),
            );
            assert_eq!(sqlite_diagnostic(&error).code, expected);
        }
        let recovery_error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY_ROLLBACK),
            Some("attempt to write a readonly database".to_string()),
        );
        let recovery_diagnostic = sqlite_diagnostic(&recovery_error);
        assert_eq!(recovery_diagnostic.sqlite_extended_code, Some(776));
        assert!(recovery_diagnostic.message.contains("rollback recovery"));
        assert!(recovery_diagnostic.message.contains("preserve"));
        let open_error = read_only_database_error(
            Path::new("/fixture/library.sqlite3"),
            "read schema version",
            &recovery_error,
        );
        let open_message = open_error.to_string();
        assert!(open_message.contains("rollback recovery"));
        assert!(open_message.contains("copy-first"));
    }

    #[test]
    fn database_diagnostic_code_json_names_are_stable_lower_snake_case() {
        let cases = [
            (DatabaseDiagnosticCode::MissingDatabase, "missing_database"),
            (
                DatabaseDiagnosticCode::PermissionDenied,
                "permission_denied",
            ),
            (DatabaseDiagnosticCode::DatabaseLocked, "database_locked"),
            (DatabaseDiagnosticCode::DatabaseBusy, "database_busy"),
            (
                DatabaseDiagnosticCode::RollbackJournalPresent,
                "rollback_journal_present",
            ),
            (
                DatabaseDiagnosticCode::HotRollbackJournal,
                "hot_rollback_journal",
            ),
            (
                DatabaseDiagnosticCode::NonHotRollbackJournal,
                "non_hot_rollback_journal",
            ),
            (
                DatabaseDiagnosticCode::MalformedRollbackJournal,
                "malformed_rollback_journal",
            ),
            (
                DatabaseDiagnosticCode::RollbackRecoveryRequired,
                "rollback_recovery_required",
            ),
            (DatabaseDiagnosticCode::WalPresent, "wal_present"),
            (DatabaseDiagnosticCode::ShmPresent, "shm_present"),
            (DatabaseDiagnosticCode::CorruptDatabase, "corrupt_database"),
            (
                DatabaseDiagnosticCode::MalformedDatabase,
                "malformed_database",
            ),
            (
                DatabaseDiagnosticCode::IntegrityCheckFailed,
                "integrity_check_failed",
            ),
            (
                DatabaseDiagnosticCode::SchemaVersionUnsupported,
                "schema_version_unsupported",
            ),
            (DatabaseDiagnosticCode::MigrationFailed, "migration_failed"),
            (DatabaseDiagnosticCode::IoError, "io_error"),
            (DatabaseDiagnosticCode::SqliteError, "sqlite_error"),
        ];
        for (code, expected) in cases {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn rollback_journal_header_json_names_are_stable_lower_snake_case() {
        let cases = [
            (RollbackJournalHeaderState::HotCandidate, "hot_candidate"),
            (RollbackJournalHeaderState::ZeroedNonHot, "zeroed_non_hot"),
            (
                RollbackJournalHeaderState::TruncatedNonHot,
                "truncated_non_hot",
            ),
            (RollbackJournalHeaderState::Malformed, "malformed"),
            (RollbackJournalHeaderState::Unreadable, "unreadable"),
        ];
        for (state, expected) in cases {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(expected.to_string())
            );
        }
    }

    #[test]
    fn database_diagnostic_returns_promptly_and_classifies_exclusive_lock() {
        let root = temp_dir("database-diagnostic-busy");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let blocker = Connection::open(&path).unwrap();
        blocker
            .execute_batch(
                "BEGIN EXCLUSIVE; UPDATE schema_migrations SET description = description;",
            )
            .unwrap();
        let started = std::time::Instant::now();

        let report = diagnose_database(&path);

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(report.diagnostics.iter().any(|item| {
            matches!(
                item.code,
                DatabaseDiagnosticCode::DatabaseBusy | DatabaseDiagnosticCode::DatabaseLocked
            )
        }));
        blocker.execute_batch("ROLLBACK").unwrap();
        blocker.close().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn database_diagnostic_json_has_exact_top_level_contract() {
        let root = temp_dir("database-diagnostic-json-contract");
        let path = root.join("library.sqlite3");
        Database::open_or_create(&path).unwrap().close().unwrap();
        let value = serde_json::to_value(diagnose_database(&path)).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "database_path",
                "database_present",
                "diagnostics",
                "format_version",
                "integrity_check",
                "journal_mode",
                "main_file",
                "open_outcome",
                "quick_check",
                "schema_version",
                "sidecars",
            ]
        );
        assert_eq!(value["open_outcome"], "opened_read_only");
        assert_eq!(value["quick_check"]["status"], "ok");
        assert_eq!(value["integrity_check"]["status"], "not_run");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn database_diagnostic_json_handles_non_utf8_path() {
        let root = temp_dir("database-diagnostic-non-utf8");
        let mut bytes = b"library-".to_vec();
        bytes.push(0x80);
        bytes.extend_from_slice(b".sqlite3");
        let path = root.join(OsString::from_vec(bytes));
        Database::open_or_create(&path).unwrap().close().unwrap();

        let report = diagnose_database(&path);
        let json = serde_json::to_string(&report).unwrap();

        assert!(report.database_path.lossy);
        assert!(json.contains("\\u{fffd}") || json.contains('�'));
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
        assert_eq!(summary.counts.archives_changed, 0);
        assert_eq!(summary.counts.archives_restored, 0);
        assert_eq!(summary.counts.archives_unchanged, 0);
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
        assert_eq!(second.counts.archives_changed, 0);
        assert_eq!(second.counts.archives_restored, 0);
        assert_eq!(second.counts.archives_unchanged, 1);
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
    fn catalogue_duplicate_groups_remain_stable_after_database_reload() {
        let root = temp_dir("duplicate-groups-stable-reload");
        let source = root.join("source");
        let mount = root.join("mount");
        let database_path = root.join("library.sqlite3");
        write_archive_file(&source, "a/Game.zip", b"first");
        write_archive_file(&source, "b/Game.7z", b"second");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(&database_path).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let before = crate::catalogue_filename_duplicates(&database.load_archives().unwrap());
        drop(database);

        let database = Database::open_or_create(&database_path).unwrap();
        let after = crate::catalogue_filename_duplicates(&database.load_archives().unwrap());

        assert_eq!(before, after);
        assert_eq!(after.groups.len(), 1);
        assert_eq!(after.groups[0].entries.len(), 2);
        assert_ne!(
            after.groups[0].entries[0].path,
            after.groups[0].entries[1].path
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
        assert_eq!(summary.counts.archives_changed, 1);
        assert_eq!(summary.counts.archives_restored, 0);
        assert_eq!(summary.counts.archives_unchanged, 0);
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
        assert_eq!(summary.counts.archives_seen, 1);
        assert_eq!(summary.counts.archives_unchanged, 1);
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
    fn one_missing_archive_can_be_removed_with_its_related_rows() {
        let root = temp_dir("remove-one-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "n64/gone.zip", b"gone");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archive_id = database.load_archives().unwrap()[0].id;
        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();

        let result = database.remove_missing_archives(&[archive_id]).unwrap();

        assert_eq!(result.requested, 1);
        assert_eq!(result.removed, 1);
        assert_eq!(result.archive_ids, vec![archive_id]);
        assert!(database.load_archives().unwrap().is_empty());
        for table in ["platform_assignments", "archive_scan_observations"] {
            let count: i64 = database
                .connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE archive_id = ?1"),
                    params![archive_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table} must not retain an orphan row");
        }
        let foreign_key_errors: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_errors, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_missing_removal_deduplicates_and_preserves_unrelated_catalogue_state() {
        let root = temp_dir("remove-missing-bulk");
        let source = root.join("source");
        let mount = root.join("mount");
        let gone_a = write_archive_file(&source, "n64/a.zip", b"a");
        let gone_b = write_archive_file(&source, "n64/b.zip", b"b");
        write_archive_file(&source, "n64/keep.zip", b"keep");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("custom", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archives = database.load_archives().unwrap();
        let id_a = find_archive(&archives, "n64/a.zip").id;
        let id_b = find_archive(&archives, "n64/b.zip").id;
        let keep_id = find_archive(&archives, "n64/keep.zip").id;
        fs::remove_file(gone_a).unwrap();
        fs::remove_file(gone_b).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();
        let scan_runs_before: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM scan_runs", [], |row| row.get(0))
            .unwrap();

        let result = database
            .remove_missing_archives(&[id_a, id_b, id_a, id_b])
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.removed, 2);
        assert_eq!(result.archive_ids, vec![id_a, id_b]);
        let remaining = database.load_archives().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, keep_id);
        assert_eq!(database.list_platform_aliases().unwrap().len(), 1);
        let scan_runs_after: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM scan_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(scan_runs_after, scan_runs_before);
        let keep_observations: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM archive_scan_observations WHERE archive_id = ?1",
                params![keep_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(keep_observations > 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn present_archive_removal_is_rejected_without_changes() {
        let root = temp_dir("remove-present-rejected");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"present");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archive_id = database.load_archives().unwrap()[0].id;

        let error = database.remove_missing_archives(&[archive_id]).unwrap_err();

        assert!(error.to_string().contains("currently present"));
        assert_eq!(database.load_archives().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mixed_present_and_missing_removal_is_fully_rejected() {
        let root = temp_dir("remove-mixed-rejected");
        let source = root.join("source");
        let mount = root.join("mount");
        let gone = write_archive_file(&source, "gone.zip", b"gone");
        write_archive_file(&source, "present.zip", b"present");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archives = database.load_archives().unwrap();
        let gone_id = find_archive(&archives, "gone.zip").id;
        let present_id = find_archive(&archives, "present.zip").id;
        fs::remove_file(gone).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();

        let error = database
            .remove_missing_archives(&[gone_id, present_id])
            .unwrap_err();

        assert!(error.to_string().contains("currently present"));
        assert_eq!(database.load_archives().unwrap().len(), 2);
        assert!(
            database
                .load_archives()
                .unwrap()
                .iter()
                .any(|archive| archive.id == gone_id)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_archive_ids_reject_missing_removal_before_any_delete() {
        let root = temp_dir("remove-unknown-rejected");
        let source = root.join("source");
        let mount = root.join("mount");
        let gone = write_archive_file(&source, "gone.zip", b"gone");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archive_id = database.load_archives().unwrap()[0].id;
        fs::remove_file(gone).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();

        let error = database
            .remove_missing_archives(&[archive_id, i64::MAX])
            .unwrap_err();

        assert!(error.to_string().contains("not found"));
        assert_eq!(database.load_archives().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_removal_rolls_back_every_delete_on_database_failure() {
        let root = temp_dir("remove-missing-rollback");
        let source = root.join("source");
        let mount = root.join("mount");
        let gone_a = write_archive_file(&source, "n64/a.zip", b"a");
        let gone_b = write_archive_file(&source, "n64/b.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archives = database.load_archives().unwrap();
        let id_a = find_archive(&archives, "n64/a.zip").id;
        let id_b = find_archive(&archives, "n64/b.zip").id;
        fs::remove_file(gone_a).unwrap();
        fs::remove_file(gone_b).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();
        database
            .connection
            .execute_batch(&format!(
                "CREATE TRIGGER fail_second_missing_delete BEFORE DELETE ON archives \
                 WHEN OLD.id = {id_b} BEGIN SELECT RAISE(ABORT, 'simulated delete failure'); END;"
            ))
            .unwrap();
        let assignments_before: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM platform_assignments", [], |row| {
                row.get(0)
            })
            .unwrap();
        let observations_before: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM archive_scan_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(database.remove_missing_archives(&[id_a, id_b]).is_err());

        assert_eq!(database.load_archives().unwrap().len(), 2);
        let assignments_after: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM platform_assignments", [], |row| {
                row.get(0)
            })
            .unwrap();
        let observations_after: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM archive_scan_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(assignments_after, assignments_before);
        assert_eq!(observations_after, observations_before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn missing_removal_accepts_non_utf8_exact_archive_identity() {
        let root = temp_dir("remove-missing-non-utf8");
        let source = root.join("source");
        let mount = root.join("mount");
        fs::create_dir_all(&source).unwrap();
        let archive_path = source.join(OsString::from_vec(vec![
            b'g', 0x80, b'm', b'e', b'.', b'z', b'i', b'p',
        ]));
        fs::write(&archive_path, b"contents").unwrap();
        assert!(archive_path.to_str().is_none());
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archive_id = database.load_archives().unwrap()[0].id;
        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();

        assert_eq!(
            database
                .remove_missing_archives(&[archive_id])
                .unwrap()
                .removed,
            1
        );
        assert!(database.load_archives().unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_catalogue_removal_never_deletes_a_reappeared_unscanned_file() {
        let root = temp_dir("remove-missing-filesystem-boundary");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "game.zip", b"original");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();
        let archive_id = database.load_archives().unwrap()[0].id;
        fs::remove_file(&archive_path).unwrap();
        scan_and_persist(&mut database, &config, "missing").unwrap();
        fs::write(&archive_path, b"reappeared but not rescanned").unwrap();

        database.remove_missing_archives(&[archive_id]).unwrap();

        assert_eq!(
            fs::read(&archive_path).unwrap(),
            b"reappeared but not rescanned"
        );
        assert!(source.is_dir());
        assert!(mount.parent().unwrap().is_dir());
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
        let restored_summary =
            scan_and_persist(&mut database, &config, "scan-after-restore").unwrap();
        assert_eq!(restored_summary.counts.archives_restored, 1);
        assert_eq!(restored_summary.counts.archives_changed, 0);
        assert_eq!(restored_summary.counts.archives_updated, 1);
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
    fn recently_found_persists_only_latest_completed_scan_additions() {
        let root = temp_dir("recent-scan-additions");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "first.zip", b"one");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        let first = scan_and_persist(&mut database, &config, "first").unwrap();
        assert_eq!(first.counts.archives_added, 1);
        assert_eq!(
            database.latest_scan_additions().unwrap().unwrap().archives[0].display_name,
            "first"
        );

        write_archive_file(&source, "second.zip", b"two");
        fs::write(source.join("first.zip"), b"changed").unwrap();
        fs::write(source.join("README.md"), b"markdown").unwrap();
        fs::write(source.join("orphan.bin"), b"ambiguous rom").unwrap();
        fs::write(source.join("manual.txt"), b"unsupported").unwrap();
        let second = scan_and_persist(&mut database, &config, "second").unwrap();
        assert_eq!(second.counts.archives_added, 1);
        assert_eq!(second.counts.archives_changed, 1);
        assert_eq!(second.counts.skipped_unsupported_extension, 1);
        assert_eq!(second.counts.skipped_ambiguous_platform, 2);
        let recent = database.latest_scan_additions().unwrap().unwrap();
        assert_eq!(recent.scan.scan_run_id, second.scan_run_id);
        assert_eq!(recent.archives.len(), 1);
        assert_eq!(recent.archives[0].display_name, "second");

        database.close().unwrap();
        let reopened = Database::open_read_only(root.join("library.sqlite3")).unwrap();
        let completed = reopened.latest_completed_scan().unwrap().unwrap();
        assert_eq!(completed.archives_unchanged, 0);
        assert_eq!(completed.skipped_unsupported_extension, 1);
        assert_eq!(completed.skipped_ambiguous_platform, 2);
        assert_eq!(
            reopened.latest_scan_additions().unwrap().unwrap().archives[0].display_name,
            "second"
        );
        let _ = fs::remove_dir_all(root);
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
        let recent = database.latest_scan_additions().unwrap().unwrap();
        assert_eq!(recent.archives.len(), 1);
        assert_eq!(recent.archives[0].display_name, "game");

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
        assert_eq!(second.counts.archives_seen, 1);
        assert_eq!(second.counts.archives_unchanged, 1);
        assert_eq!(second.counts.errors_count, 1);
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
    fn each_source_folder_records_its_own_independent_scan_status() {
        let root = temp_dir("independent-scan-status");
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
        scan_and_persist(&mut database, &config, "test").unwrap();

        let folders = database.list_source_folders().unwrap();
        assert_eq!(folders.len(), 2);
        for folder in &folders {
            assert_eq!(folder.last_scan_status, Some(SourceScanStatus::Success));
            assert_eq!(folder.last_archive_count, Some(1));
            assert!(folder.last_scan_at.is_some());
            assert!(folder.last_successful_scan_at.is_some());
            assert!(folder.last_scan_error.is_none());
        }

        // source_a goes offline; source_b keeps scanning fine.
        fs::remove_dir_all(&source_a).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();

        let folders = database.list_source_folders().unwrap();
        let folder_a = folders.iter().find(|f| f.path == source_a).unwrap();
        let folder_b = folders.iter().find(|f| f.path == source_b).unwrap();

        assert_eq!(folder_a.last_scan_status, Some(SourceScanStatus::Failed));
        assert!(folder_a.last_scan_error.is_some());
        assert_eq!(
            folder_a.last_archive_count,
            Some(1),
            "a failed rescan must not overwrite the last genuinely known archive count"
        );

        assert_eq!(
            folder_b.last_scan_status,
            Some(SourceScanStatus::Success),
            "source_b's status must be completely unaffected by source_a's failure"
        );
        assert_eq!(folder_b.last_archive_count, Some(1));
        assert!(folder_b.last_scan_error.is_none());

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
    fn manual_assignment_survives_rescan_for_every_new_retro_platform() {
        // Requirement: manual assignments for every one of the milestone's
        // 20 new canonical platforms survive a rescan, exactly like the
        // pre-existing platforms already proven by
        // `rescan_with_no_detected_platform_still_preserves_manual_platform`.
        // Each archive first auto-detects via its new folder alias (a
        // sanity check that the alias itself works end to end through a
        // real scan, not just `detect_platform` in isolation), then gets
        // manually overridden to a different value that must win over the
        // folder alias on every subsequent rescan.
        let cases: &[(&str, &str)] = &[
            ("Game Boy", "Game Boy"),
            ("GBC", "Game Boy Color"),
            ("GBA", "Game Boy Advance"),
            ("Nintendo DS", "Nintendo DS"),
            ("C64", "Commodore 64"),
            ("ZX Spectrum", "ZX Spectrum"),
            ("Sega 32X", "Sega 32X"),
            ("Mega CD", "Sega CD"),
            ("PC Engine", "PC Engine"),
            ("TurboGrafx-16", "TurboGrafx-16"),
            ("Atari Lynx", "Atari Lynx"),
            ("Atari Jaguar", "Atari Jaguar"),
            ("NGP", "Neo Geo Pocket"),
            ("NGPC", "Neo Geo Pocket Color"),
            ("WonderSwan", "WonderSwan"),
            ("WSC", "WonderSwan Color"),
            ("3DO", "3DO"),
            ("PS Vita", "PlayStation Vita"),
            ("ColecoVision", "ColecoVision"),
            ("Vectrex", "Vectrex"),
        ];

        let root = temp_dir("manual-survives-rescan-new-platforms");
        let source = root.join("source");
        let mount = root.join("mount");
        for (folder, _) in cases {
            write_archive_file(&source, format!("{folder}/Game.zip"), b"contents");
        }
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "initial-scan").unwrap();

        for (folder, expected_auto) in cases {
            let relative = format!("{folder}/Game.zip");
            let loaded = database.load_archives().unwrap();
            let archive = find_archive(&loaded, &relative);
            assert_eq!(
                archive.platform.as_deref(),
                Some(*expected_auto),
                "sanity check: {folder:?} should auto-detect as {expected_auto:?}"
            );
            let archive_id = archive.id;
            database
                .set_manual_platform(archive_id, "Manual Override")
                .unwrap();
        }

        scan_and_persist(&mut database, &config, "rescan-1").unwrap();
        scan_and_persist(&mut database, &config, "rescan-2").unwrap();

        let archives = database.load_archives().unwrap();
        for (folder, _) in cases {
            let relative = format!("{folder}/Game.zip");
            let archive = find_archive(&archives, &relative);
            assert_eq!(
                archive.platform.as_deref(),
                Some("Manual Override"),
                "manual assignment for {folder:?} must survive rescans"
            );
            assert_eq!(
                archive.platform_source.as_deref(),
                Some(MANUAL_PLATFORM_SOURCE)
            );
        }

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

    #[cfg(unix)]
    #[test]
    fn archive_replacement_after_scan_is_rejected_before_catalogue_persistence() {
        let root = temp_dir("archive-replaced-after-scan");
        let source = root.join("source");
        let mount = root.join("mount");
        let archive_path = write_archive_file(&source, "game.zip", b"old");
        let archives = ArchiveScanner::new(&config_for(&source, &mount))
            .scan_archives()
            .unwrap();
        let old_file = fs::File::open(&archive_path).unwrap();
        fs::remove_file(&archive_path).unwrap();
        fs::write(&archive_path, b"new").unwrap();

        let error = revalidate_archive_for_catalogue(&archives[0]).unwrap_err();

        assert!(error.to_string().contains("archive changed after scan"));
        drop(old_file);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn source_root_replacement_after_scan_is_rejected_before_catalogue_persistence() {
        let root = temp_dir("source-replaced-after-scan");
        let source = root.join("source");
        let displaced = root.join("source-old");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"old");
        let archives = ArchiveScanner::new(&config_for(&source, &mount))
            .scan_archives()
            .unwrap();
        fs::rename(&source, &displaced).unwrap();
        write_archive_file(&source, "game.zip", b"new");

        let error = revalidate_archive_for_catalogue(&archives[0]).unwrap_err();

        assert!(error.to_string().contains("source root changed after scan"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_source_persistence_failure_rolls_back_every_row_for_that_source() {
        let root = temp_dir("source-persistence-rollback");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_second_scan_observation \
                 BEFORE INSERT ON archive_scan_observations \
                 WHEN (SELECT COUNT(*) FROM archive_scan_observations) >= 1 \
                 BEGIN SELECT RAISE(ABORT, 'forced second observation failure'); END;",
            )
            .unwrap();

        let summary = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(summary.folder_errors.len(), 1);
        assert_eq!(summary.counts.archives_seen, 0);
        assert!(database.load_archives().unwrap().is_empty());
        let observations: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM archive_scan_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(observations, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fatal_scan_completion_failure_rolls_back_the_complete_refresh() {
        let root = temp_dir("fatal-refresh-rollback");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_scan_completion BEFORE UPDATE ON scan_runs \
                 WHEN NEW.status = 'completed' \
                 BEGIN SELECT RAISE(ABORT, 'forced completion failure'); END;",
            )
            .unwrap();

        assert!(scan_and_persist(&mut database, &config, "test").is_err());

        assert!(database.load_archives().unwrap().is_empty());
        let scan_runs: i64 = database
            .connection
            .query_row("SELECT COUNT(*) FROM scan_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            scan_runs, 0,
            "the running row belongs to the rolled-back refresh"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_catalogue_refreshes_are_serialized_with_a_bounded_wait() {
        let root = temp_dir("concurrent-refresh-serialization");
        let database_path = root.join("library.sqlite3");
        let mut first = Database::open_or_create(&database_path).unwrap();
        let mut second = Database::open_or_create(&database_path).unwrap();
        second
            .connection
            .busy_timeout(Duration::from_millis(25))
            .unwrap();

        first.begin_catalogue_refresh().unwrap();
        let error = second.begin_catalogue_refresh().unwrap_err();
        assert!(
            error.to_string().contains("locked") || error.to_string().contains("busy"),
            "unexpected contention error: {error}"
        );

        first.rollback_catalogue_refresh();
        second.begin_catalogue_refresh().unwrap();
        second.rollback_catalogue_refresh();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupted_upsert_transaction_rolls_back_safely() {
        let root = temp_dir("interrupted-upsert-rollback");
        let database_path = root.join("library.sqlite3");
        let mut database = Database::open_or_create(&database_path).unwrap();

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
        assert!(!sidecar_path(&database_path, "-journal").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_scan_commits_without_leaving_a_rollback_journal() {
        let root = temp_dir("clean-scan-no-journal");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "game.zip", b"contents");
        let config = config_for(&source, &mount);
        let database_path = root.join("library.sqlite3");
        let mut database = Database::open_or_create(&database_path).unwrap();

        let summary = scan_and_persist(&mut database, &config, "test-clean-shutdown").unwrap();
        database.close().unwrap();

        assert_eq!(summary.counts.archives_added, 1);
        assert!(!sidecar_path(&database_path, "-journal").exists());
        assert_eq!(
            diagnose_database(&database_path).quick_check.status,
            DatabaseCheckStatus::Ok
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn hot_journal_child_helper() {
        let Some(database_path) = std::env::var_os("ARCHIVEFS_TEST_HOT_JOURNAL_DB") else {
            return;
        };
        let marker_path = std::env::var_os("ARCHIVEFS_TEST_HOT_JOURNAL_MARKER").unwrap();
        let connection = Connection::open(PathBuf::from(database_path)).unwrap();
        connection
            .execute_batch(
                "PRAGMA cache_size = 1;
                 BEGIN IMMEDIATE;
                 UPDATE schema_migrations
                 SET description = description || printf('%100000s', 'x')
                 WHERE version = 1;",
            )
            .unwrap();
        fs::write(marker_path, b"transaction pages written").unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    #[cfg(unix)]
    fn killed_fixture_transaction_leaves_a_recoverable_hot_journal() {
        use std::process::{Command, Stdio};
        use std::time::Instant;

        let root = temp_dir("killed-fixture-hot-journal");
        let database_path = root.join("library.sqlite3");
        let marker_path = root.join("transaction-ready");
        Database::open_or_create(&database_path)
            .unwrap()
            .close()
            .unwrap();
        let migration_description_before: String = Connection::open(&database_path)
            .unwrap()
            .query_row(
                "SELECT description FROM schema_migrations WHERE version = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("database::tests::hot_journal_child_helper")
            .arg("--nocapture")
            .env("ARCHIVEFS_TEST_HOT_JOURNAL_DB", &database_path)
            .env("ARCHIVEFS_TEST_HOT_JOURNAL_MARKER", &marker_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !marker_path.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fixture child did not reach its open write transaction");
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let journal_path = sidecar_path(&database_path, "-journal");
        let journal = fs::read(&journal_path).unwrap();
        assert!(journal.len() > 512);
        assert_eq!(
            &journal[..SQLITE_ROLLBACK_JOURNAL_MAGIC.len()],
            SQLITE_ROLLBACK_JOURNAL_MAGIC.as_slice()
        );

        let report = diagnose_database(&database_path);
        assert_eq!(report.open_outcome, DatabaseOpenOutcome::Failed);
        assert_eq!(
            report.sidecars[0].rollback_journal_header,
            Some(RollbackJournalHeaderState::HotCandidate)
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|item| { item.code == DatabaseDiagnosticCode::RollbackRecoveryRequired })
        );

        let connection = Connection::open(&database_path).unwrap();
        let migration_description_after: String = connection
            .query_row(
                "SELECT description FROM schema_migrations WHERE version = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let quick_check: String = connection
            .pragma_query_value(None, "quick_check", |row| row.get(0))
            .unwrap();
        connection.close().unwrap();
        assert_eq!(migration_description_after, migration_description_before);
        assert_eq!(quick_check, "ok");
        assert!(!journal_path.exists());
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
    // Bulk manual platform assignment
    // -----------------------------------------------------------------

    #[test]
    fn bulk_set_manual_platform_changes_every_selected_archive() {
        let root = temp_dir("bulk-set-changes-all");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        write_archive_file(&source, "b.zip", b"b");
        write_archive_file(&source, "c.zip", b"c");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let ids: Vec<i64> = ["a.zip", "b.zip", "c.zip"]
            .iter()
            .map(|name| find_archive(&archives, name).id)
            .collect();

        let result = database
            .set_manual_platform_for_archives(&ids, "GameCube")
            .unwrap();

        assert_eq!(result.requested, 3);
        assert_eq!(result.changed, 3);
        assert_eq!(result.unchanged, 0);
        assert!(result.missing.is_empty());

        let archives = database.load_archives().unwrap();
        for name in ["a.zip", "b.zip", "c.zip"] {
            let archive = find_archive(&archives, name);
            assert_eq!(archive.platform.as_deref(), Some("GameCube"));
            assert_eq!(
                archive.platform_source.as_deref(),
                Some(MANUAL_PLATFORM_SOURCE)
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_set_manual_platform_transaction_rolls_back_on_failure() {
        let root = temp_dir("bulk-set-rollback");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let id_a = find_archive(&archives, "a.zip").id;
        let id_b = find_archive(&archives, "b.zip").id;

        // A trigger that makes the *second* archive's insert fail, after
        // the first has already (uncommitted) succeeded within the same
        // transaction - proving the whole batch rolls back together, not
        // just the row that actually failed.
        database
            .connection
            .execute_batch(&format!(
                "CREATE TRIGGER reject_archive_b BEFORE INSERT ON platform_assignments \
                 WHEN NEW.archive_id = {id_b} \
                 BEGIN SELECT RAISE(ABORT, 'forced failure for test'); END;"
            ))
            .unwrap();

        let before = database.load_archives().unwrap();
        let before_a = find_archive(&before, "a.zip").clone();
        let before_b = find_archive(&before, "b.zip").clone();

        let result = database.set_manual_platform_for_archives(&[id_a, id_b], "GameCube");

        assert!(
            result.is_err(),
            "the forced failure must propagate as an error"
        );
        let after = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&after, "a.zip"),
            &before_a,
            "the row processed before the failure must be rolled back too, not partially applied"
        );
        assert_eq!(find_archive(&after, "b.zip"), &before_b);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_set_manual_platform_deduplicates_ids_without_duplicating_history() {
        let root = temp_dir("bulk-set-dedup");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "a.zip").id;

        let result = database
            .set_manual_platform_for_archives(&[archive_id, archive_id, archive_id], "GameCube")
            .unwrap();

        assert_eq!(
            result.requested, 1,
            "requested must reflect distinct ids after deduplication"
        );
        assert_eq!(result.changed, 1);

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
            "a duplicated id in one bulk call must not duplicate history"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_set_manual_platform_with_the_same_value_reports_unchanged() {
        let root = temp_dir("bulk-set-unchanged");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        write_archive_file(&source, "b.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let id_a = find_archive(&archives, "a.zip").id;
        let id_b = find_archive(&archives, "b.zip").id;
        database
            .set_manual_platform_for_archives(&[id_a, id_b], "GameCube")
            .unwrap();

        let result = database
            .set_manual_platform_for_archives(&[id_a, id_b], "GameCube")
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.changed, 0);
        assert_eq!(result.unchanged, 2);

        let history_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM platform_assignments WHERE archive_id = ?1",
                params![id_a],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            history_count, 1,
            "re-confirming the same manual platform in bulk must not grow history"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_set_manual_platform_reports_missing_ids_without_failing_the_batch() {
        let root = temp_dir("bulk-set-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let real_id = find_archive(&database.load_archives().unwrap(), "a.zip").id;
        let missing_id = real_id + 999_999;

        let result = database
            .set_manual_platform_for_archives(&[real_id, missing_id], "GameCube")
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.changed, 1, "the valid id must still be processed");
        assert_eq!(result.unchanged, 0);
        assert_eq!(result.missing, vec![missing_id]);

        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "a.zip").platform.as_deref(),
            Some("GameCube"),
            "a stale id elsewhere in the batch must not prevent the valid id from being updated"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_clear_manual_platform_restores_each_archives_latest_automatic_fallback() {
        let root = temp_dir("bulk-clear-restores-fallback");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"a");
        write_archive_file(&source, "neogeo/game.zip", b"b");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let id_msx2 = find_archive(&archives, "msx2/game.zip").id;
        let id_neogeo = find_archive(&archives, "neogeo/game.zip").id;
        database
            .set_manual_platform_for_archives(&[id_msx2, id_neogeo], "GameCube")
            .unwrap();

        let result = database
            .clear_manual_platform_for_archives(&[id_msx2, id_neogeo])
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.changed, 2);
        assert_eq!(result.unchanged, 0);
        assert!(result.missing.is_empty());

        // No rescan anywhere in this test - each archive's own latest
        // automatic result must be current immediately, not a shared or
        // averaged value.
        let archives = database.load_archives().unwrap();
        let msx2 = find_archive(&archives, "msx2/game.zip");
        assert_eq!(msx2.platform.as_deref(), Some("MSX2"));
        assert_eq!(msx2.platform_source.as_deref(), Some("folder_alias"));
        let neogeo = find_archive(&archives, "neogeo/game.zip");
        assert_eq!(neogeo.platform.as_deref(), Some("NeoGeo"));
        assert_eq!(neogeo.platform_source.as_deref(), Some("folder_alias"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_clear_manual_platform_on_non_manual_rows_is_a_no_op() {
        let root = temp_dir("bulk-clear-non-manual-noop");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"a"); // automatic (folder_alias)
        write_archive_file(&source, "mystery.zip", b"b"); // no assignment at all
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        let id_msx2 = find_archive(&archives, "msx2/game.zip").id;
        let id_mystery = find_archive(&archives, "mystery.zip").id;
        let before = database.load_archives().unwrap();
        let before_msx2 = find_archive(&before, "msx2/game.zip").clone();
        let before_mystery = find_archive(&before, "mystery.zip").clone();

        let result = database
            .clear_manual_platform_for_archives(&[id_msx2, id_mystery])
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.changed, 0);
        assert_eq!(result.unchanged, 2);
        let after = database.load_archives().unwrap();
        assert_eq!(find_archive(&after, "msx2/game.zip"), &before_msx2);
        assert_eq!(find_archive(&after, "mystery.zip"), &before_mystery);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_manual_precedence_survives_a_rescan() {
        let root = temp_dir("bulk-manual-precedence-survives-rescan");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "msx2/game.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "msx2/game.zip").id;
        database
            .set_manual_platform_for_archives(&[archive_id], "GameCube")
            .unwrap();

        // A rescan re-detects "MSX2" via the folder alias every time, but
        // must never be allowed to silently replace the bulk manual
        // assignment - same precedence guarantee as the single-row API.
        scan_and_persist(&mut database, &config, "rescan").unwrap();

        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "msx2/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_clear_restores_a_custom_alias_fallback_correctly() {
        let root = temp_dir("bulk-clear-restores-custom-alias");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "gc/game.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("gc", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "gc/game.zip").id;
        assert_eq!(
            find_archive(&database.load_archives().unwrap(), "gc/game.zip")
                .platform_source
                .as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );
        database
            .set_manual_platform_for_archives(&[archive_id], "N64")
            .unwrap();

        let result = database
            .clear_manual_platform_for_archives(&[archive_id])
            .unwrap();

        assert_eq!(result.changed, 1);
        let archives = database.load_archives().unwrap();
        let archive = find_archive(&archives, "gc/game.zip");
        assert_eq!(archive.platform.as_deref(), Some("GameCube"));
        assert_eq!(
            archive.platform_source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE),
            "clearing bulk-manual must restore the custom alias fallback, not just any automatic result"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn bulk_set_and_clear_work_for_non_utf8_archive_identities() {
        let root = temp_dir("bulk-non-utf8-identity");
        let source = root.join("source");
        let mount = root.join("mount");
        fs::create_dir_all(&source).unwrap();
        let non_utf8_name =
            OsString::from_vec(vec![0x66, 0x6f, 0x80, 0x6f, b'.', b'z', b'i', b'p']);
        assert!(non_utf8_name.to_str().is_none());
        fs::write(source.join(&non_utf8_name), b"contents").unwrap();
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archives = database.load_archives().unwrap();
        assert_eq!(archives.len(), 1);
        let archive_id = archives[0].id;
        assert_eq!(archives[0].relative_path, PathBuf::from(&non_utf8_name));

        let set_result = database
            .set_manual_platform_for_archives(&[archive_id], "GameCube")
            .unwrap();
        assert_eq!(set_result.changed, 1);
        let archives = database.load_archives().unwrap();
        assert_eq!(archives[0].platform.as_deref(), Some("GameCube"));
        // The exact non-UTF-8 path bytes must still round-trip perfectly -
        // bulk operations dispatch purely by archive id and never touch
        // path bytes at all.
        assert_eq!(archives[0].relative_path, PathBuf::from(&non_utf8_name));

        let clear_result = database
            .clear_manual_platform_for_archives(&[archive_id])
            .unwrap();
        assert_eq!(clear_result.changed, 1);
        let archives = database.load_archives().unwrap();
        assert_eq!(archives[0].platform, None);
        assert_eq!(archives[0].relative_path, PathBuf::from(&non_utf8_name));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_set_manual_platform_rejects_empty_platform_before_writing_anything() {
        let root = temp_dir("bulk-set-rejects-empty");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "a.zip").id;

        assert!(
            database
                .set_manual_platform_for_archives(&[archive_id], "   ")
                .is_err()
        );

        let archives = database.load_archives().unwrap();
        assert_eq!(find_archive(&archives, "a.zip").platform, None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bulk_clear_manual_platform_reports_missing_ids_too() {
        let root = temp_dir("bulk-clear-missing");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "a.zip", b"a");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        scan_and_persist(&mut database, &config, "test").unwrap();
        let real_id = find_archive(&database.load_archives().unwrap(), "a.zip").id;
        database
            .set_manual_platform_for_archives(&[real_id], "GameCube")
            .unwrap();
        let missing_id = real_id + 999_999;

        let result = database
            .clear_manual_platform_for_archives(&[real_id, missing_id])
            .unwrap();

        assert_eq!(result.requested, 2);
        assert_eq!(result.changed, 1);
        assert_eq!(result.missing, vec![missing_id]);

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

    #[test]
    fn provenance_details_follow_effective_precedence_and_explain_each_automatic_tier() {
        let root = temp_dir("provenance-details-all-tiers");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "am/game.zip", b"custom");
        write_archive_file(&source, "intellivision/game.zip", b"built-in");
        write_archive_file(&source, "xbox360/game.zip", b"heuristic");
        write_archive_file(&source, "mystery/game.zip", b"unknown");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("am", "AmigaCD32").unwrap();
        scan_and_persist(&mut database, &config, "initial").unwrap();

        let archives = database.load_archives().unwrap();
        let details = database
            .load_platform_provenance_details(&archives)
            .unwrap();
        let custom = find_archive(&archives, "am/game.zip");
        let built_in = find_archive(&archives, "intellivision/game.zip");
        let heuristic = find_archive(&archives, "xbox360/game.zip");
        let unknown = find_archive(&archives, "mystery/game.zip");

        assert_eq!(
            details[&custom.id].source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );
        assert_eq!(details[&custom.id].matched_component.as_deref(), Some("am"));
        assert_eq!(
            details[&built_in.id].source.as_deref(),
            Some("folder_alias")
        );
        assert_eq!(
            details[&built_in.id].matched_component.as_deref(),
            Some("intellivision")
        );
        assert_eq!(
            details[&heuristic.id].source.as_deref(),
            Some("heuristic-path-detector")
        );
        assert_eq!(details[&heuristic.id].matched_component, None);
        assert_eq!(details[&unknown.id].platform, None);
        assert_eq!(details[&unknown.id].source, None);

        database.set_manual_platform(custom.id, "GameCube").unwrap();
        database
            .set_manual_platform(unknown.id, "GameCube")
            .unwrap();
        let archives = database.load_archives().unwrap();
        let details = database
            .load_platform_provenance_details(&archives)
            .unwrap();
        let custom_manual = &details[&custom.id];
        assert_eq!(
            custom_manual.source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE),
            "manual must remain the effective source above the custom alias"
        );
        assert_eq!(
            custom_manual
                .automatic_fallback
                .as_ref()
                .map(|fallback| fallback.platform.as_str()),
            Some("AmigaCD32")
        );
        assert_eq!(
            custom_manual
                .automatic_fallback
                .as_ref()
                .and_then(|fallback| fallback.matched_component.as_deref()),
            Some("am")
        );
        assert_eq!(details[&unknown.id].automatic_fallback, None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn manual_provenance_matches_the_fallback_clear_would_restore() {
        let root = temp_dir("provenance-details-clear-fallback");
        let source = root.join("source");
        let mount = root.join("mount");
        write_archive_file(&source, "mystery/game.zip", b"contents");
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("mystery", "GameCube").unwrap();
        scan_and_persist(&mut database, &config, "with-alias").unwrap();
        let archive_id = find_archive(&database.load_archives().unwrap(), "mystery/game.zip").id;
        database.set_manual_platform(archive_id, "Wii").unwrap();
        assert!(database.remove_platform_alias("mystery").unwrap());

        let archives = database.load_archives().unwrap();
        let details = database
            .load_platform_provenance_details(&archives)
            .unwrap();
        assert_eq!(details[&archive_id].platform.as_deref(), Some("Wii"));
        assert_eq!(
            details[&archive_id].source.as_deref(),
            Some(MANUAL_PLATFORM_SOURCE)
        );
        let fallback = details[&archive_id].automatic_fallback.as_ref().unwrap();
        assert_eq!(fallback.platform, "GameCube");
        assert_eq!(fallback.source, CUSTOM_FOLDER_ALIAS_SOURCE);
        assert_eq!(
            fallback.matched_component, None,
            "an alias removed since the last scan must not be presented as a current path match"
        );
        let cleared = database.clear_manual_platform(archive_id).unwrap();
        assert_eq!(
            cleared.new_platform.as_deref(),
            Some(fallback.platform.as_str())
        );
        assert_eq!(
            cleared.new_source.as_deref(),
            Some(fallback.source.as_str())
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn provenance_details_do_not_panic_for_non_utf8_paths() {
        let root = temp_dir("provenance-details-non-utf8");
        let source = root.join("source");
        let mount = root.join("mount");
        let folder = OsString::from_vec(vec![b'a', 0x80, b'm']);
        let archive_path = source.join(folder).join("game.zip");
        fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        fs::write(&archive_path, b"contents").unwrap();
        assert!(archive_path.to_str().is_none());
        let config = config_for(&source, &mount);
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        database.add_platform_alias("am", "AmigaCD32").unwrap();
        scan_and_persist(&mut database, &config, "non-utf8").unwrap();

        let archives = database.load_archives().unwrap();
        assert_eq!(archives[0].absolute_path, archive_path);
        let details = database
            .load_platform_provenance_details(&archives)
            .unwrap();
        assert_eq!(
            details[&archives[0].id].source.as_deref(),
            Some(CUSTOM_FOLDER_ALIAS_SOURCE)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_all_across_three_sources_isolates_two_independent_failures_from_the_success() {
        let root = temp_dir("three-source-mixed-failures");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let source_c = root.join("source-c");
        let mount = root.join("mount");
        write_archive_file(&source_a, "a.zip", b"a");
        write_archive_file(&source_b, "b.zip", b"b");
        write_archive_file(&source_c, "c.zip", b"c");
        let config = Config {
            source_folders: vec![source_a.clone(), source_b.clone(), source_c.clone()],
            mount_root: mount,
            ratarmount_bin: "ratarmount".to_string(),
        };
        let mut database = Database::open_or_create(root.join("library.sqlite3")).unwrap();
        let first = scan_and_persist(&mut database, &config, "test").unwrap();
        assert_eq!(first.counts.archives_added, 3);
        assert!(first.folder_errors.is_empty());

        // Two sources fail for genuinely different reasons: source_b is
        // deleted entirely (missing-path failure), source_c is replaced
        // by a plain file where a directory used to be (a different
        // underlying io error). source_a is left untouched and must keep
        // scanning successfully throughout.
        fs::remove_dir_all(&source_b).unwrap();
        fs::remove_dir_all(&source_c).unwrap();
        fs::write(&source_c, b"not a directory anymore").unwrap();
        write_archive_file(&source_a, "new.zip", b"new despite partial scan");

        let second = scan_and_persist(&mut database, &config, "test").unwrap();

        assert_eq!(
            second.folder_errors.len(),
            2,
            "both source_b and source_c must fail independently"
        );
        let failed_paths: std::collections::HashSet<&PathBuf> =
            second.folder_errors.iter().map(|(path, _)| path).collect();
        assert!(failed_paths.contains(&source_b));
        assert!(failed_paths.contains(&source_c));
        let error_b = &second
            .folder_errors
            .iter()
            .find(|(path, _)| path == &source_b)
            .unwrap()
            .1;
        let error_c = &second
            .folder_errors
            .iter()
            .find(|(path, _)| path == &source_c)
            .unwrap()
            .1;
        assert_ne!(
            error_b, error_c,
            "the two failures have genuinely different root causes and must not report \
             identical error text"
        );

        // source_a's success is completely unaffected by the other two
        // sources both failing in the same run.
        assert_eq!(second.counts.source_folders_scanned, 1);
        assert_eq!(second.counts.archives_seen, 2);
        assert_eq!(second.counts.archives_added, 1);
        assert_eq!(second.counts.archives_unchanged, 1);

        let recent = database.latest_scan_additions().unwrap().unwrap();
        assert_eq!(recent.scan.scan_run_id, second.scan_run_id);
        assert_eq!(recent.archives.len(), 1);
        assert_eq!(recent.archives[0].display_name, "new");

        let archives = database.load_archives().unwrap();
        assert_eq!(
            archives.len(),
            4,
            "all three archives must still exist - a partial multi-source scan never marks \
             unrelated archives missing"
        );
        for name in ["a.zip", "b.zip", "c.zip"] {
            assert!(
                find_archive(&archives, name)
                    .last_verified_missing_at
                    .is_none(),
                "{name} must not be marked missing: either its own source's scan never \
                 succeeded (b.zip, c.zip) or nothing about it changed (a.zip)"
            );
        }

        let folders = database.list_source_folders().unwrap();
        let folder_a = folders.iter().find(|f| f.path == source_a).unwrap();
        let folder_b = folders.iter().find(|f| f.path == source_b).unwrap();
        let folder_c = folders.iter().find(|f| f.path == source_c).unwrap();
        assert_eq!(folder_a.last_scan_status, Some(SourceScanStatus::Success));
        assert_eq!(folder_b.last_scan_status, Some(SourceScanStatus::Failed));
        assert_eq!(folder_c.last_scan_status, Some(SourceScanStatus::Failed));
        // Each failed source keeps its own last-known-good count from the
        // first scan - a stale scan result never replaces a newer/older
        // source's own state, and failure never fabricates a count.
        assert_eq!(folder_b.last_archive_count, Some(1));
        assert_eq!(folder_c.last_archive_count, Some(1));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_all_applies_new_retro_platform_aliases_consistently_across_sources() {
        // The real `Scan All` entry point (`scan_all_enabled_sources_at`,
        // reading a config file's `[[source]]` blocks - not the lower-level
        // `scan_and_persist` the other tests above call directly) applied
        // to three separate enabled sources, each containing one archive
        // that should auto-detect via a different new canonical platform's
        // folder alias. Every source must get the correct platform, not
        // just the first one scanned.
        let root = temp_dir("scan_all-new-retro-platforms");
        let source_a = root.join("source-a");
        let source_b = root.join("source-b");
        let source_c = root.join("source-c");
        let mount = root.join("mount");
        write_archive_file(&source_a, "Game Boy Advance/Game.zip", b"a");
        write_archive_file(&source_b, "ColecoVision/Game.zip", b"b");
        write_archive_file(&source_c, "WSC/Game.zip", b"c");
        fs::create_dir_all(&mount).unwrap();

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "mount_root = \"{}\"\nratarmount_bin = \"ratarmount\"\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n\n\
                 [[source]]\npath = \"{}\"\nenabled = true\n",
                mount.display(),
                source_a.display(),
                source_b.display(),
                source_c.display()
            ),
        )
        .unwrap();

        let database_path = root.join("library.sqlite3");
        let summary =
            crate::scan_all_enabled_sources_at(&config_path, &database_path, "test").unwrap();
        assert!(summary.folder_errors.is_empty());
        assert_eq!(summary.counts.source_folders_scanned, 3);
        assert_eq!(summary.counts.archives_added, 3);

        let database = Database::open_or_create(&database_path).unwrap();
        let archives = database.load_archives().unwrap();
        assert_eq!(
            find_archive(&archives, "Game Boy Advance/Game.zip")
                .platform
                .as_deref(),
            Some("Game Boy Advance")
        );
        assert_eq!(
            find_archive(&archives, "ColecoVision/Game.zip")
                .platform
                .as_deref(),
            Some("ColecoVision")
        );
        assert_eq!(
            find_archive(&archives, "WSC/Game.zip").platform.as_deref(),
            Some("WonderSwan Color")
        );

        let _ = fs::remove_dir_all(&root);
    }
}
