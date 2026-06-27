# ArchiveFS Database Schema

This document describes the future SQLite schema for ArchiveFS.

SQLite is not implemented in v0.1. This is a design reference for later versions that need persistent indexing, health history, retry state, and richer duplicate detection.

## Goals

- Persist scanned archive metadata.
- Model the library hierarchy: platform, title, release, archive, mount.
- Track archive health over time.
- Support retry decisions after failures.
- Avoid relying on filenames alone for duplicate detection.
- Preserve scan history for debugging and future daemon behavior.

## tables

## platforms

Stores known systems, platforms, or source groupings.

Example rows include `Xbox`, `Xbox360`, `AtariST`, and `Atari2600`.

Suggested columns:

```sql
CREATE TABLE platforms (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    normalized_name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

Why it exists:

`platforms` provides the top-level grouping under a library. It prevents platform strings from being duplicated across many archives and gives later UI, daemon, and duplicate-detection logic a stable platform identity.

## titles

Stores normalized title records within a platform.

Suggested columns:

```sql
CREATE TABLE titles (
    id INTEGER PRIMARY KEY,
    platform_id INTEGER REFERENCES platforms(id),
    display_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(platform_id, normalized_name)
);
```

Why it exists:

`titles` groups multiple releases of the same work. For example, regional variants, revisions, and alternate dumps can share one title while remaining separate releases.

## releases

Stores a specific edition, version, region, or dump of a title.

Suggested columns:

```sql
CREATE TABLE releases (
    id INTEGER PRIMARY KEY,
    title_id INTEGER NOT NULL REFERENCES titles(id),
    display_name TEXT NOT NULL,
    region TEXT,
    version TEXT,
    source_label TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

Why it exists:

`releases` is the layer between a title and concrete archive files. It lets ArchiveFS distinguish, for example, a USA release from a Europe release or a v1.0 release from a v1.1 release.

## archives

Stores discovered archive files.

Suggested columns:

```sql
CREATE TABLE archives (
    id INTEGER PRIMARY KEY,
    release_id INTEGER REFERENCES releases(id),
    source_path TEXT NOT NULL UNIQUE,
    source_root TEXT NOT NULL,
    file_name TEXT NOT NULL,
    archive_kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    size_bytes INTEGER,
    modified_time TEXT,
    content_hash TEXT,
    archive_hash TEXT,
    internal_listing_hash TEXT,
    health TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

Why it exists:

`archives` is the source-of-truth table for actual files ArchiveFS can scan and mount. It stores the identity fields needed to avoid filename-only matching and to detect changed, missing, duplicate, corrupt, or retryable archives.

## mounts

Stores mount records for archives.

Suggested columns:

```sql
CREATE TABLE mounts (
    id INTEGER PRIMARY KEY,
    archive_id INTEGER NOT NULL REFERENCES archives(id),
    mount_path TEXT NOT NULL UNIQUE,
    mount_state TEXT NOT NULL,
    backend TEXT NOT NULL,
    mounted_at TEXT,
    unmounted_at TEXT,
    last_checked_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

Why it exists:

`mounts` separates archive identity from runtime mount state. This lets ArchiveFS answer status questions, clean up only mountpoints it owns, and later support multiple backends without changing archive records.

## health_events

Stores append-only archive health transitions and diagnostic messages.

Suggested columns:

```sql
CREATE TABLE health_events (
    id INTEGER PRIMARY KEY,
    archive_id INTEGER NOT NULL REFERENCES archives(id),
    previous_health TEXT,
    new_health TEXT NOT NULL,
    reason TEXT,
    detail TEXT,
    retry_available INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
```

Why it exists:

`health_events` records why an archive moved into states such as `Failed`, `MissingParts`, `Corrupt`, or `RetryAvailable`. This is useful for CLI status, future GUI diagnostics, automatic retry, and debugging scanner behavior.

## scan_history

Stores scan runs over configured source folders.

Suggested columns:

```sql
CREATE TABLE scan_history (
    id INTEGER PRIMARY KEY,
    source_root TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    archives_found INTEGER NOT NULL DEFAULT 0,
    archives_added INTEGER NOT NULL DEFAULT 0,
    archives_updated INTEGER NOT NULL DEFAULT 0,
    archives_missing INTEGER NOT NULL DEFAULT 0,
    errors_count INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    message TEXT
);
```

Why it exists:

`scan_history` records what happened during each scan. It gives the future daemon a durable audit trail and makes it easier to explain stale status, source changes, failed scans, and retry scheduling.

## Relationships

```text
platforms
 └── titles
     └── releases
         └── archives
             ├── mounts
             └── health_events

scan_history records scan runs across source roots and updates archives.
```

## Notes

- Timestamps should use a stable UTC format.
- Health values should match `ArchiveHealth`.
- Mount state values should match `MountState`.
- Hash columns are nullable because v0.1 and early scanner passes may not compute them.
- `source_path` remains unique because a single archive file should have one archive record.
- Richer duplicate detection should compare platform, normalized title, region, version, size, archive hash, content hash, and internal listing hash rather than filename alone.
