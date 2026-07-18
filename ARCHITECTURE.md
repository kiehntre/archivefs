# ArchiveFS Architecture

This is a short, top-level overview. The full, current architecture
reference - core components, providers, the persistent catalogue, managed
library views, the patch-preview/adapter boundary, and testing strategy -
lives in [`docs/architecture.md`](docs/architecture.md).

## Crates

ArchiveFS is a Rust workspace with three crates:

- `archivefs-core`: configuration, scanning, mount planning and execution,
  the persistent SQLite catalogue, managed library views, the archive
  inspector, and the patch-preview/adapter subsystem.
- `archivefs-cli`: the command-line interface, built as thin commands over
  `archivefs-core`.
- `archivefs-gui`: a desktop frontend (`egui`/`eframe`) over the same core
  scanning, catalogue, and mount logic as the CLI.

There is no separate daemon crate. Continuous background behavior today is
limited to the explicit `watch` command described in
[`docs/watcher.md`](docs/watcher.md), which refreshes the JSON index and
never mounts or unmounts anything on its own.

## Mount backend

Archive mounting is implemented through the `MountBackend` trait
(see [`docs/domain-model.md`](docs/domain-model.md)). The only current
implementation, `RatarmountBackend`, shells out to `ratarmount`. Mounts are
always read-only; ArchiveFS never modifies a source archive.

## Archive health states

`ArchiveHealth` currently has these variants:

- `Pending`
- `Mounted`
- `Failed`
- `MissingParts`
- `Corrupt`
- `Unsupported`
- `PermissionDenied`
- `RetryAvailable`

`Failed`, `MissingParts`, and `RetryAvailable` are retryable; retries are
always an explicit user action, never automatic.

## Duplicate detection

Filename-based duplicate detection (`FilenameDuplicateDetector`) is
implemented today, grouping by normalized filename and effective platform.
It never relies on filename alone as a *correctness* claim - matches are
reported as candidates, not proven duplicates. Stronger, hash-based
detection remains a documented future direction; see
[`docs/duplicate-detector.md`](docs/duplicate-detector.md).

## Further reading

- [`docs/architecture.md`](docs/architecture.md) - full component reference.
- [`docs/domain-model.md`](docs/domain-model.md) - core types.
- [`docs/database.md`](docs/database.md) and
  [`docs/DATABASE_DESIGN.md`](docs/DATABASE_DESIGN.md) - persistent catalogue
  schema and design rationale.
- [`docs/library-views.md`](docs/library-views.md) - managed library views.
- [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md) -
  the patch-preview system and emulator adapter boundary.
- [`docs/security.md`](docs/security.md) - safety and trust boundaries.
- [`docs/json-api.md`](docs/json-api.md) - stable JSON output contracts.
