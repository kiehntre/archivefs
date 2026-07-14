# ArchiveFS Persistent Library Database: Design

Status: design only. No database code exists yet. This document proposes a small,
additive SQLite-backed catalogue that sits alongside the existing filesystem-scanning
core, not underneath it.

This is a narrower, more immediately actionable document than the existing
[`docs/database.md`](database.md), which sketches a longer-horizon
`platforms -> titles -> releases -> archives -> mounts / health_events` hierarchy plus
`scan_history`. That document remains a useful reference for where the schema could
grow once metadata providers, richer duplicate detection, and health history become
real work items. This document deliberately does *not* build that hierarchy yet, and
it deliberately does *not* give the database a `mounts` table with a persisted
`mount_state` column - see [Goals and non-goals](#1-goals-and-non-goals) and
[Integration boundaries](#5-integration-boundaries) for why. Where the two documents
name the same concept differently, this document is authoritative for the first
implementation stage; `docs/database.md` is authoritative for direction beyond it.

This document also relates to [`docs/duplicate-detector.md`](duplicate-detector.md),
a pre-existing design for multi-tier confidence-based duplicate detection (exact hash,
CRC, normalized title, etc.). That document predates the `FilenameDuplicateDetector`
that exists in the code today (it says duplicate detection "is not implemented yet",
which is now stale). The schema proposed here - specifically the hash columns on
`archives` - is what would let that richer detector eventually be built without a
further schema change. See [section 5](#5-integration-boundaries).

## Grounding

This proposal is written against the actual code, not a green-field guess. The
following existing types and behaviors constrain and inform every choice below:

- `Config { source_folders: Vec<PathBuf>, mount_root: PathBuf, ratarmount_bin: String }`
  ([`crates/archivefs-core/src/lib.rs`](../crates/archivefs-core/src/lib.rs)) is parsed
  fresh from `~/.config/archivefs/config.toml` on every command invocation. It is never
  persisted itself.
- `ArchiveScanner::scan_archives` walks every configured source folder recursively with
  `fs::read_dir` on *every call*, with no memory of previous scans. `index-build`,
  `index-show`'s freshness check, `duplicates`, `info`, `mount-one`, and the GUI's
  `load_read_only_snapshot` all trigger a full rescan today - there is no incremental
  path anywhere in the codebase yet.
- `ArchiveIdentity` already has `content_hash: Option<String>`, `archive_hash:
  Option<String>`, and `internal_listing_hash: Option<String>` fields. They exist today
  but `ArchiveIdentity::from_path` always sets them to `None` - nothing computes them.
  This document's optional content-fingerprint columns are not a new idea; they give a
  durable home to an extension point the domain model already reserved.
- The existing JSON index (`~/.local/share/archivefs/index.json`, written by
  `write_archive_index`/`build_and_write_archive_index`) is a full-rebuild snapshot
  cache, not an incremental store. Its staleness check
  (`check_archive_index_freshness`) compares a stored `modified_time_seconds:
  Option<u64>` (Unix epoch seconds) against a fresh `fs::metadata()` call. This
  document reuses that exact representation for archive modification time rather than
  inventing a new one.
- `ConfigIdentity { config_path: Option<PathBuf>, content_digest: Option<[u8; 32]> }`
  already fingerprints the config file with a SHA-256 digest (via the `sha2` crate,
  already a dependency of `archivefs-core`) purely to detect the config changing
  between two reads within one process. That is the existing precedent this document
  follows for using SHA-256 as a staleness/identity fingerprint, and `sha2` is reused
  rather than adding a new hashing crate.
- Mount and unmount safety is extensive and entirely filesystem-driven today:
  `mount_one_archive_with_backend`, `unmount_one_archive_with_backend`,
  `lazy_unmount_one_archive_path_with_progress`, `cleanup_selected_mount_tree`, and the
  batch validation behind Mount All / Unmount All all read live `fs::metadata`,
  `mounted_paths_under(mount_root)`, and symlink-escape checks - none of it reads a
  cache. The GUI additionally gates *which actions are even enabled* on
  `latest_generation_actions_safe`, which requires the current snapshot and current
  diagnostics to share both a refresh generation and a `ConfigIdentity`. None of this
  machinery is touched by this proposal; see [section 5](#5-integration-boundaries).
- Path handling is `PathBuf` throughout the domain model, and non-UTF-8 paths are
  already an explicit, tested case (`path_selector_matches_non_utf8_archive_exactly`,
  `mount_one_path_targets_non_utf8_archive_without_fuzzy_fallback`,
  `unmount_one_path_targets_non_utf8_archive_exactly`,
  `lazy_unmount_path_targets_non_utf8_archive_exactly`). Any schema that stores paths
  as `TEXT` would silently regress this guarantee the first time a real ROM/archive
  filename contains bytes that are not valid UTF-8. See [section 3](#3-archive-identity).
- `detect_platform(path, source_root)` is a heuristic, path-substring classifier (Xbox,
  Xbox360, AtariST, Atari2600, and a handful of hardcoded title matches today). It will
  keep changing and improving; a schema that stores a bare `platform` column with no
  provenance would make it impossible to tell "the current heuristic's guess" from "a
  user's manual correction" once that exists. See `platform_assignments` below.
- The workspace has **no async runtime dependency anywhere** - `archivefs-cli`'s
  `main.rs` is a plain blocking `fn main`, and `archivefs-gui`'s `eframe` integration is
  a synchronous immediate-mode UI loop that calls straight into `archivefs-core`
  functions like `load_read_only_snapshot`. This matters directly for
  [section 7](#7-rust-library-assessment).

## 1. Goals and non-goals

### Goals for the initial database

- Remember scanned archives between launches, so the CLI and GUI do not need to
  re-walk every configured source folder from scratch every time.
- Detect additions, removals, and changed files across scans, with a durable trail of
  what changed and when (not just a silently-overwritten cache).
- Retain the platform `detect_platform` assigns to an archive, with enough provenance
  to distinguish a heuristic guess from a future manual correction or provider result.
- Support fast GUI startup and fast, indexed searching, replacing "re-scan the
  filesystem" as the default read path for those two things.
- Remain rebuildable from the filesystem at any time. The database is a cache and
  observation log over what `ArchiveScanner` would discover; it is never the only copy
  of anything that matters. Deleting the database file must always be a safe,
  supported recovery action.
- Never become the source of truth for mount safety. Every mount/unmount/cleanup
  decision keeps reading live filesystem and live mount state exactly as it does
  today; the database is not consulted by that code path at all, at any stage in the
  delivery plan.

### Non-goals for the initial database

- Downloading metadata from any external service (ScreenScraper, MobyGames, IGDB, or
  similar - all mentioned in `docs/roadmap.md` as v0.4.x work, not this).
- Storing artwork, box art, or any binary asset.
- Emulator launching or emulator configuration of any kind.
- Cloud sync, remote backup, or any network-facing behavior for the database itself.
- Multi-user support - one database file for one local user, matching every other
  piece of ArchiveFS state (`~/.config/archivefs/config.toml`,
  `~/.local/share/archivefs/index.json`) being single-user and local already.

None of the long-term items in the task description (artwork, emulator associations,
launch history, favourites) are designed here. [Section 5](#5-integration-boundaries)
explains why the schema below still leaves a coherent landing spot for them without
predicting their exact shape today.

## 2. Proposed schema

All tables live in one SQLite file, proposed at
`~/.local/share/archivefs/library.db` - the same XDG data directory
(`~/.local/share/archivefs/`) `default_index_path()` already uses for `index.json`,
kept separate from `~/.config/archivefs/` (configuration) by the same convention the
project already follows.

Timestamps are stored as `TEXT` in ISO-8601 UTC (`YYYY-MM-DDTHH:MM:SSZ`), matching the
style already used for `Last modified` display in `archivefs info` output and being
trivially human-readable when inspecting the database directly with the `sqlite3` CLI.
File modification times are the one exception: they are stored as
`INTEGER` Unix-epoch seconds, matching `ArchiveIndexEntry.modified_time_seconds:
Option<u64>` exactly, for continuity with the existing JSON index and because
`ArchiveIndex` freshness checking already treats second-precision equality as "did
this file change" - there is no existing consumer of finer precision to preserve.

### `schema_migrations`

```sql
CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at  TEXT NOT NULL
);
```

- One row per applied migration, in order. No columns are optional.
- `PRAGMA user_version` is additionally set to the latest applied version on every
  migration, so a fast startup check (`PRAGMA user_version` needs no query) can decide
  "are we already current" before touching this table at all. `schema_migrations`
  itself remains the durable, human-readable audit log with `description` explaining
  what each version changed - `PRAGMA user_version` alone cannot carry that.
- No relationships to other tables; this table exists before any of them do.

### `source_folders`

One row per distinct path that has ever appeared in `Config.source_folders`.
Normalizing this out of `archives` (rather than repeating the full source-folder path
on every archive row) mirrors the normalization pattern already used in
`docs/database.md`'s `platforms`/`titles` tables, and gives every archive a stable
integer to identify "which configured folder was this found under" - which is half of
this document's proposed archive identity (see [section 3](#3-archive-identity)).

```sql
CREATE TABLE source_folders (
    id                       INTEGER PRIMARY KEY,
    path                     BLOB NOT NULL,
    first_seen_at            TEXT NOT NULL,
    last_seen_in_config_at   TEXT NOT NULL,
    removed_from_config_at   TEXT
);

CREATE UNIQUE INDEX source_folders_path ON source_folders(path);
```

- `path` is `BLOB`, not `TEXT` - see [section 3](#3-archive-identity) for why. It holds
  the exact `PathBuf` bytes of one entry from `Config.source_folders`, resolved but not
  otherwise normalized (no canonicalization, so a symlinked source folder keeps
  whatever path the user actually configured, matching how `Config` treats it today).
- `last_seen_in_config_at` is updated every time a config load still contains this
  path. `removed_from_config_at` is set (once, on first detection) when a config load
  no longer contains a path that used to be present. Rows are never deleted - archives
  hold a foreign key into this table, and deleting the row would either cascade-delete
  catalogue history or require nulling a supposedly-`NOT NULL` foreign key. A source
  folder no longer in the config is simply excluded from future scans; its historical
  archives and observations remain queryable.
- Nothing here says whether the directory currently exists on disk. That is a live
  `doctor`/scan-time check (see [section 4](#4-scan-lifecycle)), not a persisted
  column - a source folder can be temporarily unreachable (unmounted drive) without
  ever being "removed from config".

### `archives`

The persistent catalogue's central table: one row per archive file *identity*,
surviving across scans. This table intentionally has no `mount_path` or `mount_state`
column - see [section 5](#5-integration-boundaries) for why mount state is never
persisted here.

```sql
CREATE TABLE archives (
    id                          INTEGER PRIMARY KEY,
    source_folder_id            INTEGER NOT NULL REFERENCES source_folders(id),
    relative_path                BLOB NOT NULL,
    absolute_path_cached         BLOB NOT NULL,
    file_name_cached              BLOB NOT NULL,
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

CREATE UNIQUE INDEX archives_identity
    ON archives(source_folder_id, relative_path);
CREATE INDEX archives_normalized_name ON archives(normalized_name);
CREATE INDEX archives_content_hash ON archives(content_hash)
    WHERE content_hash IS NOT NULL;
CREATE INDEX archives_missing ON archives(last_verified_missing_at)
    WHERE last_verified_missing_at IS NOT NULL;
```

Columns and optionality:

- `id` is a surrogate integer primary key. Identity is `(source_folder_id,
  relative_path)`, enforced by the unique index, not the row id - see
  [section 3](#3-archive-identity).
- `relative_path` (required) is the archive's path *relative to its
  `source_folders.path`*, stored as raw path bytes (`BLOB`), exactly mirroring how
  `ArchiveIdentity::from_path` already receives a `source_root` and a `path` as
  separate values today.
- `absolute_path_cached` and `file_name_cached` (required) are denormalized
  conveniences recomputed on every observation - `source_folders.path` joined with
  `relative_path`, and the final path component - so that `archivefs-cli info`/
  `mount-one`-style lookups and GUI row rendering do not need a join plus a `PathBuf`
  concatenation on every read. They are a cache, not identity: if they and
  `relative_path` were ever to disagree (they should not, by construction), the FK'd
  `source_folder_id` + `relative_path` pair wins.
- `archive_kind` (required) is `'zip' | 'sevenzip' | 'rar'`, the same three values
  `ArchiveKind` has today.
- `display_name`, `normalized_name` (required) mirror `ArchiveIdentity.display_name`
  and `.normalized_name`. They are derived, human-facing strings (already produced via
  `to_string_lossy()`-based helpers today), so `TEXT` is fine for them even though the
  underlying path fields are not - see [section 3](#3-archive-identity).
- `size_bytes`, `modified_time_unix_seconds` (optional) mirror
  `ArchiveIdentity.size_bytes: Option<u64>` and the existing
  `ArchiveIndexEntry.modified_time_seconds: Option<u64>` representation. `NULL` when
  `fs::metadata` could not be read at the time of the observation that wrote the row
  (matching `ArchiveIdentity::from_path`'s existing `metadata.map(...)` /
  `metadata.and_then(...)` fallback to `None`).
- `content_hash`, `archive_hash`, `internal_listing_hash` (optional) mirror the three
  identically-named, already-`Option<String>` fields on `ArchiveIdentity` that exist
  in the code today but are never populated. They stay optional here for the same
  reason: computing any of them means reading the archive's full content, which is
  too expensive to do on every routine scan (see [section 3](#3-archive-identity)).
  `content_hash` is proposed as a SHA-256 hex digest, reusing the `sha2` crate already
  a dependency, matching `ConfigIdentity`'s existing hashing choice.
- `last_known_health` (required, defaulted) mirrors `ArchiveHealth`'s eight variants
  as their `Display` string (`Pending`, `Mounted`, `Failed`, `MissingParts`,
  `Corrupt`, `Unsupported`, `PermissionDenied`, `RetryAvailable`). This is explicitly
  a **last-observed cache for reporting**, e.g. "N archives were last seen as
  `Corrupt`" in a future `doctor`-style summary. It is never read by
  `mount_one_archive_with_backend` or any other function that decides whether to
  attempt a mount - that always recomputes health live via `FilesystemHealthProvider`,
  exactly as today.
- `first_seen_at`, `last_seen_at` (required) track when this identity was first
  discovered and when a scan most recently found it still present.
- `last_verified_missing_at` (optional) is set, and kept updated, the first time (and
  every time) a scan of a *reachable* source folder does not find this archive
  anymore. The row is never deleted automatically - see
  [section 4](#4-scan-lifecycle) for the full "missing" behavior.
- `created_at`/`updated_at` are ordinary row bookkeeping timestamps.

### `scan_runs`

One row per scan attempt, whatever triggered it.

```sql
CREATE TABLE scan_runs (
    id                       INTEGER PRIMARY KEY,
    started_at                TEXT NOT NULL,
    finished_at                TEXT,
    trigger                  TEXT NOT NULL,
    config_content_digest     BLOB,
    source_folders_scanned    INTEGER NOT NULL DEFAULT 0,
    archives_seen             INTEGER NOT NULL DEFAULT 0,
    archives_added            INTEGER NOT NULL DEFAULT 0,
    archives_updated          INTEGER NOT NULL DEFAULT 0,
    archives_missing          INTEGER NOT NULL DEFAULT 0,
    errors_count              INTEGER NOT NULL DEFAULT 0,
    status                   TEXT NOT NULL DEFAULT 'running',
    error_message             TEXT
);

CREATE INDEX scan_runs_status ON scan_runs(status) WHERE status = 'running';
```

- `finished_at` (optional) is `NULL` for the entire duration of a scan and is the
  mechanism interrupted-scan detection uses - see [section 4](#4-scan-lifecycle).
- `trigger` (required) records why the scan happened:
  `'cli-index-build' | 'cli-duplicates' | 'gui-startup' | 'gui-refresh' |
  'watch-rebuild' | 'initial-full-scan'`, matching the actual call sites that already
  exist (`build_and_write_archive_index`, `watch_archive_index`,
  `load_read_only_snapshot`) plus the initial-population case.
- `config_content_digest` (optional) is the exact same 32-byte SHA-256 digest
  `ConfigIdentity.content_digest` already computes, stored so a scan run can be tied
  to precisely the config state it used - reusing the existing fingerprint rather than
  inventing a second one.
- The `archives_*`/`errors_count` counters (required, defaulted to 0) are filled in as
  the scan proceeds and are what a future `archivefs-cli index-show` could report
  instead of (or alongside) a live freshness check.
- `status` (required) is `'running' | 'completed' | 'failed' | 'interrupted'`.
  `'interrupted'` is never set by the scan itself - it is set by the *next* process to
  open the database, for any row it finds still `'running'` (see
  [section 4](#4-scan-lifecycle)).
- No foreign keys point at `scan_runs` from `source_folders` or `archives` themselves
  (those tables' `last_seen_at`/`updated_at` are enough for their own purposes); only
  `archive_scan_observations` links back to a specific run, keeping `archives` itself
  simple to query without a join.

### `archive_scan_observations`

Append-only log of what happened to each archive during each scan. This is the
"changed since last time" trail the goals ask for, kept separate from the mutable
`archives` cache columns so that history is never silently overwritten.

```sql
CREATE TABLE archive_scan_observations (
    id                          INTEGER PRIMARY KEY,
    scan_run_id                  INTEGER NOT NULL REFERENCES scan_runs(id),
    archive_id                   INTEGER NOT NULL REFERENCES archives(id),
    observation                 TEXT NOT NULL,
    size_bytes                  INTEGER,
    modified_time_unix_seconds  INTEGER,
    observed_at                  TEXT NOT NULL
);

CREATE INDEX archive_scan_observations_archive
    ON archive_scan_observations(archive_id, observed_at);
CREATE INDEX archive_scan_observations_run
    ON archive_scan_observations(scan_run_id);
```

- `observation` (required) is one of `'added' | 'unchanged' | 'changed' | 'missing' |
  'restored'` (`'restored'` = previously `missing`, seen again). See
  [section 3](#3-archive-identity) for exactly when each applies.
- `size_bytes`/`modified_time_unix_seconds` (optional) record what was observed *this
  scan*, independent of whatever `archives` currently caches - useful for a future "show
  me the size history of this archive" or debugging a scanner regression, without
  requiring it for the initial implementation to be useful.
- This table only grows; nothing here is ever updated in place. A future retention
  policy (e.g. "keep the last N observations per archive") is a reasonable follow-up
  but is not designed here, since it is not required for the stated goals.

### `platform_assignments`

Deliberately *not* a `platform` column on `archives`. Kept as its own append-only,
provenance-tracked table because `detect_platform` is a heuristic that will keep
changing, and because the long-term direction includes the possibility of a user
correction or a future metadata provider disagreeing with it - none of which should
silently overwrite history the way a bare column update would.

```sql
CREATE TABLE platform_assignments (
    id           INTEGER PRIMARY KEY,
    archive_id    INTEGER NOT NULL REFERENCES archives(id),
    platform     TEXT NOT NULL,
    source       TEXT NOT NULL,
    confidence   TEXT,
    is_current    INTEGER NOT NULL DEFAULT 1,
    assigned_at   TEXT NOT NULL
);

CREATE UNIQUE INDEX platform_assignments_current
    ON platform_assignments(archive_id) WHERE is_current = 1;
CREATE INDEX platform_assignments_archive ON platform_assignments(archive_id);
CREATE INDEX platform_assignments_platform ON platform_assignments(platform);
```

- `platform` (required) holds the same strings `detect_platform` already returns
  today (`"Xbox360"`, `"Xbox"`, `"PC"`, `"PSP"`, etc.) - no new vocabulary.
- `source` (required) is provenance: `'heuristic-path-detector'` for everything
  `detect_platform` produces today, reserving room for `'user-override'` or a named
  provider later without a schema change.
- `confidence` (optional, currently always `NULL`) is reserved for later use.
  `detect_platform` today returns a plain `Option<String>` with no confidence concept
  at all, so there is nothing to put in this column yet; it is included now purely to
  avoid a migration the day confidence scoring is added.
- The partial unique index on `is_current = 1` guarantees at most one "current"
  assignment per archive at the database level - "what platform is this archive"
  is `SELECT platform FROM platform_assignments WHERE archive_id = ? AND is_current =
  1`, one indexed lookup. Reassigning a platform (say, the detector improves, or a
  future user override happens) means flipping the old row's `is_current` to `0` and
  inserting a new row with `is_current = 1`, preserving full history.

### Relationships

```text
source_folders
  └── archives (source_folder_id)
        ├── archive_scan_observations (archive_id)  ---- scan_runs (scan_run_id)
        └── platform_assignments (archive_id)

schema_migrations  (standalone)
```

## 3. Archive identity

The task's four candidate identity signals, and how this design uses them:

- **Exact filesystem path bytes on Unix.** On Linux, a `PathBuf`/`OsString` is not
  guaranteed valid UTF-8 - it is effectively raw bytes with a small set of reserved
  values (`std::os::unix::ffi::OsStrExt` exposes this as `&[u8]` losslessly). The
  existing test suite already exercises non-UTF-8 archive paths
  (`path_selector_matches_non_utf8_archive_exactly` and three siblings), so any schema
  that stored paths as SQLite `TEXT` (which SQLite requires to be valid UTF-8, or
  silently mangles/rejects otherwise) would be a real regression the first time a
  scanned filename is not valid UTF-8. Every path column in this schema
  (`source_folders.path`, `archives.relative_path`, `.absolute_path_cached`,
  `.file_name_cached`) is therefore `BLOB`, storing the exact `OsStr` bytes, with
  conversion to/from `PathBuf` happening only at the Rust boundary via
  `OsStrExt::from_bytes`/`.as_bytes()`. `display_name`/`normalized_name` are `TEXT`
  because they are already lossy, human-facing strings produced by existing helpers
  (`archive_title`, `normalized_title`) - the same distinction `detect_platform`
  already draws by using `to_string_lossy()` for heuristic matching while the domain
  model keeps the real `PathBuf` for anything that touches the filesystem.
- **Source-folder identity plus relative path.** This is the primary identity key:
  `UNIQUE(source_folder_id, relative_path)` on `archives`. It directly mirrors how
  `ArchiveIdentity::from_path` already separates `source_root` from `path` today,
  and it means reordering `source_folders` in the config, or a source folder simply
  being listed differently, does not change any archive's identity - only the
  underlying directory tree moving does.
- **File size.** Stored (`size_bytes`), used together with modification time as the
  default "did this change" signal - cheap (one `fs::metadata` call, already made by
  every scan today), no file content is read.
- **Modification time.** Stored as Unix-epoch seconds
  (`modified_time_unix_seconds`), reusing `ArchiveIndexEntry`'s existing
  representation and precision exactly, rather than inventing a new one nothing else
  in the codebase uses.
- **Optional content fingerprint.** `content_hash`/`archive_hash`/
  `internal_listing_hash`, mirroring the three already-reserved-but-unpopulated
  `ArchiveIdentity` fields. These must stay opt-in and computed lazily (a separate,
  explicit operation - not part of every scan), because hashing full archive content
  is proportional to file size and archive libraries in this domain (console ROM/ISO
  collections) commonly include multi-gigabyte files; making every routine scan hash
  every file would directly work against the "fast GUI startup" goal.

### Renames, changes, missing files, and duplicates

- **Rename within the same source folder** (same `source_folder_id`, different
  `relative_path`, but matching `size_bytes` and `modified_time_unix_seconds`, or a
  matching `content_hash` where one has been computed): detected as a *candidate*
  during a scan by pairing a "not found at its old `relative_path`" archive against a
  "newly discovered path with no existing `archives` row" one within the same
  `scan_run`. Only treated as a rename - i.e. the existing `archives.id` row is
  updated in place (`relative_path`, cached path columns, `updated_at`) rather than
  writing a `missing` + `added` pair - when the match is **unambiguous** (exactly one
  plausible candidate on each side). If more than one file could match, the scan
  falls back to recording plain `missing`/`added` observations rather than guessing.
  This follows the project's own stated design principle in `docs/roadmap.md`
  ("prefer safe stale data warnings over destructive automatic repair") applied to
  scan bookkeeping instead of mount cleanup.
- **Changed file** (same `source_folder_id` + `relative_path`, but `size_bytes` or
  `modified_time_unix_seconds` differs from what is stored): the existing `archives`
  row is kept (same `id` - the identity did not move, only its content did) and
  updated in place; a `changed` observation is written. Any previously-computed
  `content_hash`/`archive_hash`/`internal_listing_hash` is cleared to `NULL` on a
  `changed` observation, since a hash computed against the old content is actively
  wrong for the new content, not merely stale.
- **Missing file** (an `archives` row exists under a source folder that scanned
  successfully, but the file was not found): `last_verified_missing_at` is set/
  refreshed and a `missing` observation is written. The row is **not deleted**. Later
  cleanup (pruning archives missing for longer than some threshold) is an explicit,
  user-triggered action design question for a later stage, never an automatic side
  effect of scanning - see [section 4](#4-scan-lifecycle) for why a source folder
  being temporarily unreachable must not be treated the same as files actually being
  gone.
- **Duplicate content** (two distinct `archives` rows sharing a `content_hash`, or
  matching on `(platform, normalized_name)` the way `FilenameDuplicateDetector`
  already groups today): the database never merges these into one row - they are
  genuinely distinct files on disk with distinct identities. Duplicate detection stays
  a *query* over `archives` (see [section 5](#5-integration-boundaries)), not a schema
  concept; `DuplicateReport`/`DuplicateEntry`'s existing shape does not need to change.

## 4. Scan lifecycle

- **Initial full scan.** When `library.db` does not exist yet, or has no `archives`
  rows under the currently-configured `source_folders`, behavior is functionally
  identical to `ArchiveScanner::scan_archives` today - walk everything - except every
  discovered archive now also produces an `archives` insert and an `added`
  observation under a new `scan_runs` row (`trigger = 'initial-full-scan'`).
- **Incremental scan.** Subsequent scans still have to walk the filesystem - SQLite
  cannot know about a brand new file without the directory being listed again, and
  this proposal does not attempt to replace that with the existing `notify`-based
  watcher (though a later stage could use watcher events to trigger an incremental
  scan sooner; that is a future integration note, not a requirement here). What
  changes is that each discovered file is checked against `archives` via the
  `(source_folder_id, relative_path)` unique index instead of every field being
  recomputed from scratch: if `size_bytes`/`modified_time_unix_seconds` match what is
  stored, only `last_seen_at` is bumped and an `unchanged` observation is written -
  `display_name`/`normalized_name`/platform are not recomputed, because they are pure
  functions of the path, which has not changed. This is the actual "fast" part of
  "fast GUI startup": for a library that has not changed since the last scan, the
  database read path can serve archive rows directly with no scanning at all, and a
  background/on-demand incremental scan (the existing "Refresh" button, `watch`, or a
  future `index-build`) is what reconciles drift.
- **Stale record handling.** Archives with `last_verified_missing_at` set surface in
  reporting (a future `doctor`/`index-show`-style summary) exactly the way
  `ArchiveIndexFreshness.missing_archive_paths` already surfaces missing entries
  today, just durable across restarts instead of recomputed on every read. Nothing
  about a stale record changes automatically; it is reported, not acted on.
- **Deleted or unavailable source folders.** Two distinct cases, handled differently:
  - *Removed from config*: `source_folders.removed_from_config_at` is set on the next
    config load that no longer lists the path. Its archives are simply excluded from
    future scans; their rows and history are left alone (not deleted, not marked
    missing - they were not looked at, so nothing was observed).
  - *Still configured but currently unreachable* (unmounted drive, disconnected
    network share): scanning that source folder should fail loudly for that source
    folder specifically (`errors_count` incremented on the `scan_runs` row) and must
    **not** cascade into marking every archive under it as `missing` - a temporarily
    disconnected drive is not the same event as the user deleting thousands of files,
    and treating them identically would make the database actively misleading. A scan
    only writes `missing` observations for a source folder it successfully finished
    listing.
- **Interrupted scans.** Because scanning is synchronous and single-process today (no
  daemon, no background thread doing this concurrently), any `scan_runs` row found
  with `status = 'running'` and `finished_at IS NULL` at the *start* of a new process
  can only be left over from a previous process that did not exit cleanly. The next
  process to open the database marks it `'interrupted'`. Whatever `archives`/
  `archive_scan_observations` rows that interrupted run did manage to write remain
  valid - an archive that really was observed, was observed - the only thing
  incomplete is that not every configured source folder was necessarily reached
  before the process died, which is exactly why "only mark missing for a source
  folder whose listing fully completed" (above) also protects against an interrupted
  scan incorrectly flagging unvisited archives as missing.
- **Transaction boundaries.** The `scan_runs` row itself is inserted and committed
  in its own short transaction *before* scanning starts, so it is durably visible as
  `'running'` even if the process is killed moments later - this is what makes
  interrupted-scan detection possible at all. The actual per-archive work is then
  committed in bounded batches (for example, once per fully-scanned source folder, or
  every N archives for a very large single folder) rather than one transaction for
  the entire scan: this bounds how much work is lost and re-scanned after a crash to
  at most one batch, and avoids holding a single very large transaction open (lock
  contention, WAL growth) against a large library. `scan_runs.finished_at` and
  `status = 'completed'` are the last thing written, in their own final transaction.
- **Rebuilding the catalogue from scratch.** Because the database is explicitly a
  cache/observation-log and never the only copy of anything (see
  [section 1](#1-goals-and-non-goals)), deleting `~/.local/share/archivefs/library.db`
  outright and letting the next scan recreate it via migrations from empty is always
  a safe, supported recovery path - not a special code path, just the natural
  consequence of "initial full scan" above running again. No dedicated
  "rebuild" implementation is required beyond making sure a missing database file is
  treated the same as a freshly-migrated empty one.

## 5. Integration boundaries

- **`archivefs-core`.** A new module (proposed name: `catalogue`, to avoid colliding
  with the already-overloaded word "index" which currently means the JSON file) would
  own everything in this document: opening/creating/migrating the database, and
  narrow functions like `record_scan_run`, `upsert_archive_observation`,
  `query_archives`, `current_platform_for`. Nothing in the existing mount/unmount/
  scan/config code (`mount_one_archive_with_backend`, `unmount_one_archive_with_backend`,
  `cleanup_selected_mount_tree`, `ArchiveScanner`, `Config`, etc.) would import from
  this module. That is not just a convention: it is how "mounting must remain safe and
  independent of database availability" and "current functionality must continue
  working without a database" are actually enforced - by construction, at the
  dependency-graph level, a `catalogue` module that does not exist (or fails to open)
  cannot break code that never depends on it.
- **Current index commands.** `index-build` becomes "run an incremental scan against
  the database" instead of "always do a full rescan and overwrite `index.json`" (see
  [section 4](#4-scan-lifecycle)). `index-show`/`index-find` become indexed queries
  against `archives` instead of reading and linearly searching a JSON file. The JSON
  index and its documented `archivefs status/stats/info --json`
  contract ([`docs/json-api.md`](json-api.md)) are **not removed** - `index-build`
  can still write `index.json` as an optional export view generated from the query
  results, so any existing script or tooling parsing that file keeps working
  unchanged. `check_archive_index_freshness`'s live-`fs::metadata` freshness check
  either stays as-is for the JSON path or gains a database-backed equivalent that
  reports the same two categories (missing/stale) from `archives` instead - either
  way, the public JSON shape does not change.
- **GUI snapshot loading.** A new fast path (proposed: `load_catalogue_snapshot_default`)
  would read `ArchiveRecord`-shaped rows directly from the database for the GUI's
  first paint, instead of `load_read_only_snapshot`'s current full rescan on every
  launch. The existing full-rescan path (`load_read_only_snapshot`, triggered today
  on startup and by the **Refresh** button) stays exactly as it is and remains the
  authoritative reconciliation step that also updates the database. Critically,
  `latest_generation_actions_safe` - the function that decides whether Mount All /
  Unmount All / individual mount actions are even enabled, by comparing the current
  snapshot's and diagnostics' `ConfigIdentity` and refresh generation - continues to
  gate on the live snapshot exactly as today. A database-backed fast-start view is
  never treated as "safe to act on" by itself; only a live snapshot with a matching
  `ConfigIdentity` is, unchanged from current behavior.
- **Duplicate detection.** `DuplicateDetector` is a trait today
  (`FilenameDuplicateDetector` groups an in-memory `&[ArchiveRecord]` by
  `(platform, normalized_name)`). A database-backed implementation of the same trait
  could run the equivalent grouping as an indexed SQL query instead of an in-memory
  scan, and - once `content_hash` starts being populated for at least some archives -
  additionally group by `content_hash` for a higher-confidence tier, which is exactly
  the kind of layered confidence model `docs/duplicate-detector.md` already describes.
  `DuplicateReport`/`DuplicateEntry`'s shape does not need to change; only which
  `DuplicateDetector` implementation is wired in changes.
- **Future metadata and emulator support.** None of that is designed here (see
  non-goals), but `platform_assignments`'s shape - `archive_id` foreign key,
  `source`/provenance, `assigned_at`, append-only with an `is_current` flag - is the
  template a later `metadata_assignments`, `emulator_associations`, `favourites`, or
  `launch_history` table would follow, so that when that work happens it extends this
  schema in a familiar way rather than needing to redesign it.
- **Mount/unmount validation - restated explicitly.** Every mount, unmount, lazy
  unmount, and cleanup decision keeps reading live `fs::metadata`, live
  `mounted_paths_under(mount_root)`, and live symlink-escape checks, exactly as today.
  The database is not read by any of that code, at any stage of the delivery plan in
  [section 8](#8-delivery-plan). If the database file is missing, corrupt, or the
  `catalogue` module is not even compiled in, mounting and unmounting must behave
  identically to how they behave today.

## 6. Migration strategy

- **Schema versioning.** `schema_migrations` (durable audit log with descriptions) plus
  `PRAGMA user_version` (cheap startup check) together, as described in
  [section 2](#2-proposed-schema).
- **Forward-only migrations.** Migrations are plain embedded SQL, applied in order,
  each in its own transaction, with no down-migrations - matching this project's
  existing preference for small, hand-rolled solutions over general-purpose machinery
  (the config parser is a hand-rolled subset parser rather than pulling in a `toml`
  crate; the same philosophy applies here rather than adding a migration-framework
  dependency for what is, at this schema's size, a handful of `CREATE TABLE`
  statements).
- **Backup before destructive migration.** Every migration proposed in this document's
  initial schema is purely additive (new tables, or later, new nullable columns) and
  needs no special handling. If a future migration ever needs to drop or rename a
  column/table, the database file should be copied to a timestamped
  `library.db.bak-<version>-<timestamp>` sibling first (a plain file copy is safe for
  SQLite when nothing else has the file open, which is guaranteed here since
  ArchiveFS is not a daemon).
- **Recovery from corruption.** `PRAGMA integrity_check` on open. If it fails, the
  database is never the source of truth for anything (see
  [section 1](#1-goals-and-non-goals)), so the correct recovery is exactly the same as
  a manual rebuild: rename the corrupt file aside
  (`library.db.corrupt-<timestamp>`), start a fresh one, let normal scanning
  repopulate it. No repair logic is designed or needed, specifically *because* of the
  "always rebuildable from the filesystem" goal.
- **Safe deletion and rebuild.** A user (or a future `archivefs-cli` command) deleting
  `~/.local/share/archivefs/library.db` directly must be a supported, safe operation
  with no manual follow-up beyond running a scan again - this should be an explicit
  test case once implementation begins, not just an implied property.

## 7. Rust library assessment

Comparing `rusqlite` and `sqlx` against this specific project, not in the abstract:

| | `rusqlite` | `sqlx` |
|---|---|---|
| Core architecture | Synchronous | Async-first (`Future`-based; needs an executor) |
| Fits current codebase | Matches it exactly - **no async runtime exists anywhere** in this workspace today (`archivefs-cli`'s `main` is a plain blocking function; `archivefs-gui`'s `eframe` loop is synchronous immediate-mode) | Would require adopting `tokio` or `async-std` as a new architectural layer just to run a query, rippling async coloring into scan/mount code that is entirely synchronous today |
| Dependency weight | One well-understood C dependency (SQLite itself) | A large async dependency tree (async runtime plus its own driver/pool abstraction) even though only one database backend is needed |
| Compile time | `bundled` feature compiles SQLite's C source via `cc` - a real, known, accepted cost | Async runtime plus driver machinery tends to be heavier for a single-backend use case |
| Bundled SQLite | Yes - the `bundled` Cargo feature statically links SQLite, no system `libsqlite3` needed at runtime | No bundled option in the same sense; typically links against a system driver/library |
| Migration support | No built-in tooling either way; a small hand-rolled embedded-SQL runner is proposed for both | `sqlx::migrate!` exists, but pulls in more of the crate's async machinery |
| Testability | `Connection::open_in_memory()` gives a fast, isolated, zero-file-I/O test database - directly compatible with this project's existing fast (sub-0.1s), no-real-filesystem-writes test suite | Testable, but async tests add setup (a runtime) that this codebase does not otherwise need |
| Compile-time-checked queries | No | Yes, a genuine strength - but requires either a live database at build time or maintaining an offline query-metadata cache file in CI |

**Recommendation: `rusqlite`, with the `bundled` feature.**

The deciding factor is architectural fit, not a feature checklist: this workspace is
synchronous end to end today, and the release workflow already ships a Linux binary
that was specifically verified this project (in the release-automation work) to need
**zero additional Ubuntu packages** to build, because `archivefs-gui`'s windowing
dependencies all resolve via runtime `dlopen` rather than link-time linking. A `sqlx`
adoption would be the first thing in the entire codebase to require an async runtime,
purely to support a feature (local SQLite persistence) that does not need one.
`rusqlite`'s `bundled` feature preserves the same "no system dependency" property the
release build already has for its other dependencies, by statically linking SQLite
instead of requiring `libsqlite3-dev` on the build machine or `libsqlite3` on the
target machine. `sqlx`'s compile-time query checking is a real advantage this project
would give up, but for a schema this small, hand-written and hand-tested queries
(matching how the existing hand-rolled config parser is tested - direct unit tests,
not a macro-verified DSL) is a reasonable trade against not adopting an async runtime.

## 8. Delivery plan

Each stage is scoped to be independently testable and to leave current behavior
completely unchanged until a later stage deliberately wires it in.

1. **Database module and migrations.** New `catalogue` module in `archivefs-core`,
   proposed behind a Cargo feature flag (e.g. `catalogue`) so the mount/scan/config
   core can still be built with zero database dependency for anyone who wants that -
   a build-time expression of "mounting must remain independent of database
   availability", not just a runtime one. Deliverable: `schema_migrations` plus the
   empty schema from [section 2](#2-proposed-schema), open/create/migrate functions,
   `PRAGMA integrity_check`-on-open, and unit tests using
   `Connection::open_in_memory()`. Nothing else in the workspace references this
   module yet, so current behavior is unaffected by construction.
2. **Archive persistence.** CRUD functions for `source_folders`/`archives`/
   `scan_runs`/`archive_scan_observations`/`platform_assignments`. Still not wired
   into any CLI or GUI command - exercised only by new tests that call the existing
   `ArchiveScanner::scan_archives` and feed the results into the `catalogue` module
   manually, asserting the rows and identity rules from
   [section 3](#3-archive-identity). No user-visible change.
3. **Incremental scanning.** Implement the diff-against-database logic from
   [section 4](#4-scan-lifecycle): change detection, rename detection, missing-file
   bookkeeping, unreachable-source-folder handling, and interrupted-scan recovery on
   startup. Tested with a harness that runs multiple scan passes over a temporary
   directory (`env::temp_dir()`-based, matching the existing `test_root` test helper
   pattern) with files added/removed/modified/renamed between passes, asserting exact
   observation and row outcomes. Still not wired into any command.
4. **CLI integration.** `index-build` uses the database when the `catalogue` feature
   is compiled in and a database is available, falling back to today's full-rescan-
   and-write-JSON behavior otherwise; `index-show`/`index-find` read from the database
   when available. The documented `--json` output contract
   ([`docs/json-api.md`](json-api.md)) does not change shape. This is the first stage
   with any user-visible change, and it must be provably backward compatible: running
   without the `catalogue` feature, or with no database file present, must behave
   exactly as it does before this stage.
5. **GUI integration.** Wire a database-backed fast-start snapshot for initial paint,
   with the existing full-rescan `load_read_only_snapshot`/Refresh flow untouched as
   the authoritative reconciliation path. No change to `latest_generation_actions_safe`,
   `ConfigIdentity` gating, or any mount/unmount button behavior.
6. **Later metadata extension (not scoped here).** `metadata_assignments`,
   `emulator_associations`, `favourites`, `launch_history`, and richer duplicate
   detection extend the schema following the `platform_assignments` pattern from
   [section 5](#5-integration-boundaries), once those features are actually being
   built - deliberately left undesigned now per the [non-goals](#1-goals-and-non-goals).
