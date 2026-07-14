-- Migration 0001: initial schema foundation for the ArchiveFS persistent
-- library catalogue.
--
-- See docs/DATABASE_DESIGN.md for the full design and the rationale behind
-- every table and column below, and docs/adr/0001-persistent-library-database.md
-- for the architectural decision this schema implements.
--
-- This file is applied by crates/archivefs-core/src/database.rs inside a
-- single transaction. It must not itself INSERT into schema_migrations -
-- the migration runner does that generically for every migration, after
-- this SQL succeeds, so that "each migration records its version exactly
-- once" is a property of the runner rather than something every migration
-- file has to remember to do.
--
-- Note: the design document's scan_runs.trigger column is named
-- triggered_by here instead - TRIGGER is a SQL keyword, and the rest of
-- this schema avoids relying on any engine's willingness to accept
-- reserved words as bare identifiers.

CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at  TEXT NOT NULL
);

-- One row per distinct path that has ever appeared in Config.source_folders.
-- `path` is BLOB, not TEXT: on Unix a PathBuf is not guaranteed to be valid
-- UTF-8, and SQLite TEXT columns are. Storing raw path bytes is the only
-- lossless option - see docs/DATABASE_DESIGN.md section 3.
CREATE TABLE source_folders (
    id                     INTEGER PRIMARY KEY,
    path                   BLOB NOT NULL,
    first_seen_at          TEXT NOT NULL,
    last_seen_in_config_at TEXT NOT NULL,
    removed_from_config_at TEXT
);

CREATE UNIQUE INDEX source_folders_path ON source_folders(path);

-- One row per archive file identity, surviving across scans. Identity is
-- (source_folder_id, relative_path), enforced by archives_identity below -
-- not the surrogate id. No mount_path or mount_state column exists here on
-- purpose: mount state is never persisted as a source of truth (see
-- docs/DATABASE_DESIGN.md section 5 and the ADR) - every mount/unmount
-- decision keeps reading live filesystem and live mount state directly.
CREATE TABLE archives (
    id                          INTEGER PRIMARY KEY,
    source_folder_id            INTEGER NOT NULL REFERENCES source_folders(id),
    relative_path               BLOB NOT NULL,
    absolute_path_cached        BLOB NOT NULL,
    file_name_cached             BLOB NOT NULL,
    archive_kind                TEXT NOT NULL,
    display_name                TEXT NOT NULL,
    normalized_name              TEXT NOT NULL,
    size_bytes                  INTEGER,
    modified_time_unix_seconds  INTEGER,
    content_hash                 TEXT,
    archive_hash                 TEXT,
    internal_listing_hash         TEXT,
    last_known_health           TEXT NOT NULL DEFAULT 'Pending',
    first_seen_at                TEXT NOT NULL,
    last_seen_at                  TEXT NOT NULL,
    last_verified_missing_at      TEXT,
    created_at                   TEXT NOT NULL,
    updated_at                   TEXT NOT NULL
);

CREATE UNIQUE INDEX archives_identity ON archives(source_folder_id, relative_path);
CREATE INDEX archives_normalized_name ON archives(normalized_name);
CREATE INDEX archives_content_hash ON archives(content_hash) WHERE content_hash IS NOT NULL;
CREATE INDEX archives_missing ON archives(last_verified_missing_at) WHERE last_verified_missing_at IS NOT NULL;

-- One row per scan attempt, whatever triggered it. finished_at stays NULL
-- for the duration of a scan; a row still 'running' with finished_at NULL
-- when a new process starts can only be left over from an interrupted
-- previous run (scanning is synchronous and single-process today).
CREATE TABLE scan_runs (
    id                     INTEGER PRIMARY KEY,
    started_at             TEXT NOT NULL,
    finished_at            TEXT,
    triggered_by           TEXT NOT NULL,
    config_content_digest  BLOB,
    source_folders_scanned INTEGER NOT NULL DEFAULT 0,
    archives_seen          INTEGER NOT NULL DEFAULT 0,
    archives_added         INTEGER NOT NULL DEFAULT 0,
    archives_updated       INTEGER NOT NULL DEFAULT 0,
    archives_missing       INTEGER NOT NULL DEFAULT 0,
    errors_count           INTEGER NOT NULL DEFAULT 0,
    status                 TEXT NOT NULL DEFAULT 'running',
    error_message          TEXT
);

CREATE INDEX scan_runs_status ON scan_runs(status) WHERE status = 'running';

-- Append-only log of what happened to each archive during each scan. Kept
-- separate from the mutable archives cache columns so history is never
-- silently overwritten.
CREATE TABLE archive_scan_observations (
    id                          INTEGER PRIMARY KEY,
    scan_run_id                  INTEGER NOT NULL REFERENCES scan_runs(id),
    archive_id                   INTEGER NOT NULL REFERENCES archives(id),
    observation                 TEXT NOT NULL,
    size_bytes                  INTEGER,
    modified_time_unix_seconds  INTEGER,
    observed_at                  TEXT NOT NULL
);

CREATE INDEX archive_scan_observations_archive ON archive_scan_observations(archive_id, observed_at);
CREATE INDEX archive_scan_observations_run ON archive_scan_observations(scan_run_id);

-- Deliberately not a `platform` column on archives - detect_platform is a
-- heuristic that will keep changing, and this keeps every assignment's
-- provenance and history instead of silently overwriting a guess. At most
-- one row per archive_id may have is_current = 1, enforced below.
CREATE TABLE platform_assignments (
    id          INTEGER PRIMARY KEY,
    archive_id  INTEGER NOT NULL REFERENCES archives(id),
    platform    TEXT NOT NULL,
    source      TEXT NOT NULL,
    confidence  TEXT,
    is_current  INTEGER NOT NULL DEFAULT 1,
    assigned_at TEXT NOT NULL
);

CREATE UNIQUE INDEX platform_assignments_current ON platform_assignments(archive_id) WHERE is_current = 1;
CREATE INDEX platform_assignments_archive ON platform_assignments(archive_id);
CREATE INDEX platform_assignments_platform ON platform_assignments(platform);
