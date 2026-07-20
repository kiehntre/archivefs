# JSON API

This document describes ArchiveFS command output that is intended for programmatic use.

Commands that currently support JSON output:

```sh
archivefs database-check --json
archivefs status --json
archivefs stats --json
archivefs info <archive> --json
```

JSON mode always writes the JSON document to stdout. Human headings, summaries, and explanatory text are omitted. Operational logs, when enabled with `--verbose` or `--debug`, are written to stderr by the normal logger and must not be mixed into stdout.

Three other commands support `--json` but document their own schema in a dedicated design document rather than here, since their output is defined alongside a larger feature design: `archivefs pcsx2-patch-preview --json` (see [`PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md)), `archivefs retroarch-environment --json` (see [`RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md)), and `archivefs retroarch-patch-preview --json` (see [`RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md)). The stability guarantees below apply to all documented JSON output, not only the three commands listed immediately above.

The RetroArch playlist identity/matching milestone added only additive fields to the two RetroArch schemas above (both `format_version`s stayed `1`, per the "new fields may be added" guarantee below) - see [`RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) for the playlist-specific field documentation.

A later RetroArch AppImage-detection milestone bumped `retroarch-environment --json`'s `format_version` from `1` to `2`: unlike a purely additive field, `profiles[]` can now have a 4th (AppImage) entry inserted *between* native and Flatpak/user, which shifts what a positional index like `profiles[2]` means for any consumer that indexed into the array rather than reading each profile's own `profile_kind`. This is the "deliberate JSON API change" case the Stability Guarantees below call out, not the "new fields may be added" case. `retroarch-patch-preview --json`'s own top-level `format_version` stayed `1` - only its *embedded* `environment` field's format_version changed. See [`RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md) for the full record.

The existing-artifact milestone likewise adds one top-level
`artifact_inventory` field to `retroarch-patch-preview --json` without changing
the enclosing format version. The nested inventory has its own
`format_version: 1`, exact key set, lower-snake-case enums, and byte-safe paths;
see [`RETROARCH_ARTIFACT_INVENTORY.md`](RETROARCH_ARTIFACT_INVENTORY.md).

## `archivefs database-check --json`

`database-check` is a read-only database diagnostic. Its top-level key set is
stable and exact:

```text
format_version
database_path
database_present
main_file
sidecars
open_outcome
journal_mode
quick_check
integrity_check
schema_version
diagnostics
```

`format_version` is currently `2`. `database_path` and every sidecar path use
the byte-safe `{ "display": string, "lossy": boolean }` representation.
`open_outcome` is one of `opened_read_only`, `missing_database`, or `failed`.
Because SQLite opens lazily, `failed` also covers a connection whose first real
read returns `rollback_recovery_required` even if the connection object itself
was created successfully.
Check statuses are `ok`, `failed`, `error`, or `not_run`. The bounded default
runs `quick_check`; `integrity_check` is present as `not_run` rather than
silently doing an unbounded full check.

Every diagnostic has `code`, `severity`, `message`,
`sqlite_extended_code`, and `raw_sqlite_message`. Codes are stable
lower-snake-case values including `missing_database`, `permission_denied`,
`database_locked`, `database_busy`, `rollback_journal_present`,
`hot_rollback_journal`, `non_hot_rollback_journal`,
`malformed_rollback_journal`, `rollback_recovery_required`, `wal_present`,
`shm_present`, `corrupt_database`, `malformed_database`,
`integrity_check_failed`, `schema_version_unsupported`, `migration_failed`,
`io_error`, and `sqlite_error`. `raw_sqlite_message` is explicitly unstable
presentation detail and must not be parsed. Sidecar presence is reported as
evidence and never treated as proof of corruption.

Format version 2 adds `rollback_journal_header` to each sidecar finding. It is
`hot_candidate`, `zeroed_non_hot`, `truncated_non_hot`, `malformed`, or
`unreadable` for the rollback journal and `null` for WAL/SHM. `hot_candidate`
is header evidence, not a substitute for SQLite's lock-aware recovery
decision. `malformed` describes only the sidecar header; it does not classify
the main database as corrupt.

The command never creates a missing file or parent directory, runs a migration,
changes journal mode, checkpoints WAL, deletes a sidecar, or repairs data.

## `archivefs status --json`

Command syntax:

```sh
archivefs status
archivefs status --json
```

Both forms inspect the configured archive sources through the same status collection path. The first prints the existing human-readable table. The second pretty-prints a JSON array and no human heading or explanatory text.

### Schema

Top-level value: array of `ArchiveStatusJson` objects. An empty result is represented as `[]`.

```text
ArchiveStatusJson[]

ArchiveStatusJson = {
  archive_path: string,
  mount_path: string,
  state: "Pending" | "Mounted" | "MountPathExists"
}
```

### Fields

`archive_path`

Filesystem path to the source archive represented as a string.

`mount_path`

Filesystem path where ArchiveFS mounts, or would mount, the archive.

`state`

Current mount state. `Pending` means the archive is not mounted and its mount path does not exist. `Mounted` means the planned mount path is currently mounted. `MountPathExists` means the planned mount path exists but is not currently detected as mounted.

Status output contains no byte-count fields. Any byte counts added to this schema in a future compatible extension will be JSON integers measured in bytes, never formatted size strings.

### Example response

```json
[
  {
    "archive_path": "/data/archives/xbox/Halo.zip",
    "mount_path": "/mnt/archivefs/Xbox/Halo",
    "state": "Mounted"
  },
  {
    "archive_path": "/data/archives/atari/Another World.7z",
    "mount_path": "/mnt/archivefs/AtariST/Another World",
    "state": "Pending"
  }
]
```

## `archivefs stats --json`

`archivefs stats --json` scans the configured archive sources through the normal stats path and prints a pretty JSON representation of `ArchiveStats`.

### Schema

Top-level value: object.

```json
{
  "total_archives": 103,
  "mounted_count": 2,
  "pending_count": 101,
  "platform_counts": {
    "AtariST": 91,
    "Xbox360": 6,
    "Unknown": 6
  },
  "extension_counts": {
    "7z": 6,
    "rar": 3,
    "zip": 94
  },
  "largest_archive": {
    "archive_path": "/data/archives/xbox360/Example.zip",
    "size_bytes": 7926335344
  },
  "smallest_archive": {
    "archive_path": "/data/archives/atari/Game.zip",
    "size_bytes": 20480
  },
  "total_size_bytes": 48182145024
}
```

```text
ArchiveStatsJson = {
  total_archives: number,
  mounted_count: number,
  pending_count: number,
  platform_counts: { [platform: string]: number },
  extension_counts: { [extension: string]: number },
  largest_archive: ArchiveSizeSummaryJson | null,
  smallest_archive: ArchiveSizeSummaryJson | null,
  total_size_bytes: number
}

ArchiveSizeSummaryJson = {
  archive_path: string,
  size_bytes: number
}
```

### Fields

`total_archives`

Total number of archive records considered by the stats command.

`mounted_count`

Number of records whose mount state is currently `Mounted`.

`pending_count`

Number of records whose mount state is currently `Pending`. Records with other mount states, such as an existing mount path that is not mounted, are not counted as pending.

`platform_counts`

Object keyed by platform name. Values are archive counts for that platform. Archives without a detected or provider-supplied platform are grouped under `Unknown`.

The object keys are data values, not a fixed enum. Integrations should handle new platform names.

`extension_counts`

Object keyed by lowercase archive extension, such as `zip`, `7z`, or `rar`. Values are archive counts for that extension.

The object keys are data values. Integrations should handle new keys if ArchiveFS supports more archive types later.

`largest_archive`

The largest archive with a known filesystem size, or `null` if no archive size is known.

`smallest_archive`

The smallest archive with a known filesystem size, or `null` if no archive size is known.

`archive_path`

Filesystem path to the archive represented as a string.

`size_bytes`

Archive size in bytes.

`total_size_bytes`

Sum of known archive sizes in bytes. Archives without known sizes do not contribute to this total.

## `archivefs info <archive> --json`

`archivefs info <archive> --json` resolves the archive using the same selection path as the human `info` command and prints a pretty JSON representation of `ArchiveInfo`.

If no archive matches, or multiple archives match, the command returns the same selection error as human mode instead of printing JSON.

### Schema

Top-level value: object.

```json
{
  "title": "007 Legends",
  "platform": "Xbox360",
  "archive_path": "/data/archives/xbox360/007 Legends.zip",
  "mount_path": "/mnt/archivefs/Xbox360/007 Legends",
  "extension": "zip",
  "size_bytes": 7340032000,
  "modified_time": 1717438123,
  "health": "Pending",
  "mount_state": "Pending",
  "metadata_provider": "FilenameMetadataProvider",
  "health_provider": "FilesystemHealthProvider"
}
```

```text
ArchiveInfoJson = {
  title: string,
  platform: string | null,
  archive_path: string,
  mount_path: string,
  extension: string,
  size_bytes: number | null,
  modified_time: number | null,
  health: string,
  mount_state: string,
  metadata_provider: string,
  health_provider: string
}
```

### Fields

`title`

Display title selected for the archive.

`platform`

Detected or provider-supplied platform name, or `null` when unknown.

`archive_path`

Filesystem path to the archive represented as a string.

`mount_path`

Filesystem path where ArchiveFS would mount this archive.

`extension`

Lowercase archive extension, such as `zip`, `7z`, or `rar`.

`size_bytes`

Archive size in bytes, or `null` when the size is unknown.

`modified_time`

Last modified time as Unix seconds, or `null` when the timestamp is unknown.

`health`

Current archive health value as the same string used by human output.

`mount_state`

Current mount state value as the same string used by human output.

`metadata_provider`

Name of the metadata provider that supplied the displayed metadata.

`health_provider`

Name of the health provider that supplied the displayed health value.

## Stability Guarantees

The JSON output documented here is intended for integrations, scripts, and tests.

Within the current pre-1.0 project stage:

- Field names listed in this document should remain stable unless there is a deliberate JSON API change.
- Field types listed in this document should remain stable unless there is a deliberate JSON API change.
- New fields may be added in the future.
- Integrations should ignore unknown fields.
- Object key ordering should not be treated as part of the contract.
- Pretty-print indentation should not be treated as part of the contract.
- Counts, sizes, and Unix timestamps are numeric JSON values, not strings.
- Paths are strings and should be treated as platform filesystem paths, not URLs.
- Display enum values such as `health` and `mount_state` are stable enough for display, but integrations should handle unknown future values.

Breaking JSON changes should be documented in this file and called out in release notes once release notes exist.

## Future Commands

Future commands may add JSON output, but each command should define its own schema unless it is returning an existing model directly.

Guidelines for future JSON commands:

- Add JSON output intentionally, command by command.
- Keep human-readable output unchanged unless explicitly changing that command.
- Print JSON only when the JSON flag is supplied.
- Write logs to stderr, never stdout.
- Document the schema here before treating it as integration-facing.
- Prefer structured fields over parsing display text.
- Preserve stable field names once documented.

Potential future JSON outputs may include:

- `archivefs duplicates --json`
- `archivefs index-show --json`
- `archivefs index-find --json`

These are not implemented today.
