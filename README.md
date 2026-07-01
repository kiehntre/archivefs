# ArchiveFS

ArchiveFS is a Linux-first command-line tool for mounting archive files as read-only folders. It scans configured source folders, plans safe mount paths, can mount archives through `ratarmount`, and provides status, stats, info, index, and watcher commands over the same core scanner.

ArchiveFS is designed to inspect and mount archives without modifying the original archive files.

## Key Features

- Scans configured folders for supported archives: `.zip`, `.7z`, and `.rar`.
- Skips obvious split RAR continuation parts such as `.r00` and non-primary `.partN.rar` files.
- Mounts archives read-only through `ratarmount`.
- Tracks archive path, mount path, mount state, health, size, modified time, and platform hints.
- Provides `status`, `stats`, and `info` commands for inspecting a library.
- Builds a JSON index for summary and search commands.
- Watches source folders and refreshes the JSON index without auto-mounting or auto-unmounting.
- Includes config validation and doctor diagnostics.

## Installation

ArchiveFS is a Rust workspace. Build the CLI from source:

```sh
cargo build --workspace
```

The development binary will be at:

```sh
target/debug/archivefs
```

For regular local use, install it with Cargo:

```sh
cargo install --path crates/archivefs-cli
```

Archive mounting uses `ratarmount`, so install it separately and make sure it is available on `PATH`, or set `ratarmount_bin` in the config.

## Configuration

ArchiveFS reads its default config from:

```text
~/.config/archivefs/config.toml
```

Example:

```toml
source_folders = ["/data/archives"]
mount_root = "/mnt/archivefs"
ratarmount_bin = "ratarmount"
```

`source_folders` are scanned recursively. `mount_root` is where ArchiveFS creates planned mount directories. ArchiveFS does not modify files in `source_folders`.

## Common Commands

```sh
archivefs doctor
archivefs config-check
archivefs scan
archivefs status
archivefs stats
archivefs info "007 Legends"
archivefs mount-one "007 Legends"
archivefs unmount-one "007 Legends"
archivefs index-build
archivefs index-show
archivefs index-find "xbox360"
archivefs watch
```

Use verbose or debug logging when you need more detail:

```sh
archivefs --verbose stats
archivefs --debug watch
```

## Typical Workflow

1. Create `~/.config/archivefs/config.toml`.
2. Run `archivefs config-check` to validate the config.
3. Run `archivefs doctor` to check source folders, mount root, tools, and current archive state.
4. Run `archivefs stats` or `archivefs scan` to inspect what ArchiveFS sees.
5. Run `archivefs info "name"` to inspect one archive.
6. Run `archivefs mount-one "name"` to mount a single archive.
7. Run `archivefs unmount-one "name"` when finished.
8. Run `archivefs index-build` and `archivefs index-find "query"` for indexed search.
9. Run `archivefs watch` if you want ArchiveFS to refresh the JSON index when source folders change.

## Example Output

`archivefs stats`:

```text
ArchiveFS Stats

Summary:
  Total archives: 128
  Mounted: 3
  Pending: 125
  Total archive size: 42.8 GiB

Platforms:
  Unknown: 12
  Xbox360: 116

Archive extensions:
  7z: 44
  rar: 9
  zip: 75
```

`archivefs info "007 Legends"`:

```text
ArchiveFS Info

Details:
  Title: 007 Legends
  Platform: Xbox360
  Archive path: /data/archives/xbox360/007 Legends.zip
  Mount path: /mnt/archivefs/Xbox360/007_Legends
  Extension: zip
  Archive size: 7.4 GiB
  Last modified: 2026-06-01 14:22:10 UTC
  Health: Pending
  Mount state: Pending
  Metadata provider: FilenameMetadataProvider
  Health provider: FilesystemHealthProvider
```

`archivefs index-show`:

```text
ArchiveFS Index

Summary:
  Total archives: 128
  Mounted: 3
  Pending: 125

Platforms:
  Unknown: 12
  Xbox360: 116
```

## Documentation

Developer and design docs live in [`docs/`](docs/):

- [Architecture](docs/architecture.md)
- [Roadmap](docs/roadmap.md)
- [Domain model](docs/domain-model.md)
- [Watcher](docs/watcher.md)
- [Security](docs/security.md)
- [Database notes](docs/database.md)
- [Provider pipeline](docs/provider-pipeline.md)
