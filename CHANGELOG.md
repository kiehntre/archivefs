# Changelog

All notable user-facing changes to ArchiveFS are recorded here. The format is
loosely inspired by [Keep a Changelog](https://keepachangelog.com/), but this
project does not yet claim strict compliance with that format, and versions
prior to 1.0 do not follow semantic versioning guarantees.

Entries below `Unreleased` and each tagged version are reconstructed from git
tags, commit history, and `Cargo.toml` version bumps. Where a commit's exact
user-facing effect could not be confirmed from its message and diff alone,
this file describes only what the code and history actually show, rather than
guessing at intent, dates, or scope.

## Unreleased

Work merged to `main` since the `v0.4.3-alpha` tag, not yet released.

### Added

- Managed library views: named, symlink-based organized views of the
  catalogue (`view list`, `view preview`, `view apply`, `view repair`,
  `view remove`), backed by `~/.config/archivefs/library_views.json` and a
  per-view JSON manifest under `~/.local/share/archivefs/library_views/` that
  tracks every symlink ArchiveFS created so `repair`/`remove` only ever touch
  paths ArchiveFS itself manages.
- Read-only PCSX2 patch-preview foundation (`pcsx2-patch-preview`): fetches a
  single compiled-in PCSX2 patch metadata endpoint into bounded memory and
  prints native/Flatpak installation candidates as a non-executable advisory
  plan. This is metadata-only preview - it does not download, verify,
  install, or enable any patch, and does not write anything to disk.
- An emulator-neutral patch adapter boundary (`EmulatorAdapter` trait and
  supporting types in `archivefs-core::patch_manager::adapter`), extracted
  from the PCSX2-specific code so future emulator adapters can be added
  without redesigning the shared orchestration. `ReadOnlyPcsx2Adapter` is
  currently the only implementation of this trait.
- An archive content inspector (`archivefs-core::inspector`) that classifies
  entries inside a supported archive without extracting it, and improvements
  to mount-readiness checks that use it.
- Expanded canonical retro platform recognition (additional entries in the
  folder-name platform alias table and related database/GUI support).
- A repository maintenance script (`scripts/barry-checkpoint.sh`) and its
  tests for automated project checkpointing. This is a development/tooling
  addition, not a user-facing ArchiveFS capability.
- `DEDICATION.md`, linked from the bottom of `README.md`.
- Read-only RetroArch environment discovery (`retroarch-environment`):
  detects a native and a Flatpak (user- and system-scope) RetroArch profile,
  locates and parses `retroarch.cfg` for twelve configured path purposes
  (System, Cores, CoreInfo, Saves, SaveStates, Playlists, Shaders, Overlays,
  Thumbnails, JoypadAutoconfig, Database, Cheats), and inventories installed
  Linux cores (`*_libretro.so`) plus their optional `.info` metadata. This is
  a sibling to the patch-preview adapter boundary, not part of it - see
  [`docs/RETROARCH_ENVIRONMENT.md`](docs/RETROARCH_ENVIRONMENT.md). Strictly
  read-only: no file is created, modified, or deleted; no process is
  spawned; no network call is made; no core is loaded.
- A read-only RetroArch cheat/patch destination preview
  (`retroarch-patch-preview`): for every present catalogue archive,
  previews per-game `.cht` cheat destinations (gated on exactly one
  installed core supporting the archive's own file extension) and IPS/
  BPS/UPS/Xdelta soft-patch sibling destinations, across every discovered
  RetroArch profile. Builds directly on the RetroArch environment
  discovery above rather than rediscovering any path, and makes no
  network call - unlike PCSX2, no RetroArch metadata source has been
  reviewed for this milestone. Does not implement `EmulatorAdapter` or
  produce an `AdvisoryPatchPlan`: RetroArch's multi-root, core-selection-
  ambiguous shape does not fit that PCSX2-specific trait/type, so this is
  a separate, narrowly-scoped `RetroArchAdvisoryPlan` instead. No PCSX2
  type, plan ID, JSON shape, or CLI output was changed. See
  [`docs/RETROARCH_PATCH_PREVIEW.md`](docs/RETROARCH_PATCH_PREVIEW.md).

### Changed

- Rust CI reproducibility: the project now pins an exact Rust toolchain
  (`1.97.1`) via `rust-toolchain.toml`, and both `.github/workflows/ci.yml`
  and `.github/workflows/release.yml` install that exact version explicitly
  instead of a floating `stable` channel. See the
  [Rust toolchain policy](CONTRIBUTING.md#rust-toolchain-policy) in
  `CONTRIBUTING.md` for why.

### Fixed

- An ambiguous float literal (`egui::Stroke::new(2.0, stroke_color)`) that a
  newer Rust compiler's stricter lint started rejecting was made explicit
  (`2.0_f32`). This was a compiler-drift break, not a logic change: the code
  had been correct and passing CI until an unpinned `stable` toolchain moved
  out from under it.

## v0.4.3-alpha

### Added

- Multi-source management: `sources`, `sources scan-all`, `source add`,
  `source enable`, `source disable`, `source scan`, and `source remove`
  (with `--keep-catalogue`/`--remove-catalogue`), plus a redesigned GUI
  Sources page covering the same workflow.

## v0.4.2-alpha

### Added

- GUI duplicate review workflow, including a "select all visible" action for
  bulk-handling duplicate candidates.

## v0.4.1-alpha

### Added

- `library-remove-missing`: removes catalogue entries whose source file is
  gone, by exact id or path. It never deletes files - it only removes stale
  database rows.

## v0.4.0-alpha

### Added

- Platform detection provenance and scan summaries: the catalogue now
  records whether a platform came from the filename heuristic, the
  folder-alias fallback, or a manual override, and scans report a structured
  completion summary.

## v0.3.9-alpha

### Fixed

- GUI: an explicit float type on a focus-stroke width (a narrower, earlier
  instance of the same kind of ambiguous-literal issue fixed for Rust 1.97.1
  compatibility above).

## v0.3.8-alpha

### Added

- GUI: improved library table navigation and sorting.

## v0.3.7-alpha

### Fixed

- GUI: bulk archive selection made reliable.

## v0.3.6-alpha

### Changed

- Added an automated deployment smoke test (Nobara) to the project's CI
  tooling.

## v0.3.5-alpha

### Added

- `platform-alias-list`, `platform-alias-add`, and `platform-alias-remove`:
  persistent, user-defined folder-name-to-platform aliases.
- `--version` CLI output, aligned with the workspace version.

## v0.3.4-alpha

### Added

- An unknown-platform review workflow (`library-list --unknown-only`,
  `library-find --unknown-only`) for finding catalogue entries that need a
  manual platform assignment.

## v0.3.3-alpha

### Added

- `library-set-platform` and `library-clear-platform` (plus
  `-bulk` variants added in later releases): persistent manual platform
  assignments that outrank automatic detection.

## v0.3.2-alpha

### Fixed

- Scanner: nested archives inside N-Gage container files are skipped
  instead of being treated as separate top-level archives.

## v0.3.1-alpha

### Changed

- Improved platform detection using folder-name aliases as a fallback when
  the primary filename heuristic finds nothing.

## v0.3.0-alpha

### Added

- A persistent library database: a SQLite-backed catalogue
  (`archivefs-core::database`) that stores scanned archive records between
  runs, plus `library-status`, `library-scan`, `library-list`, and
  `library-find` CLI commands, and GUI integration that reads from the
  persistent catalogue instead of rescanning on every launch.
- Design documentation for the persistent library database.

Mount and unmount safety continued to read live filesystem and mount state
directly and were not made to depend on this new catalogue; see
[`docs/adr/0001-persistent-library-database.md`](docs/adr/0001-persistent-library-database.md).

## v0.2.3-alpha

### Fixed

- Config parser: `source_folders` arrays split across multiple lines are
  now accepted, not just single-line arrays.

## v0.2.2-alpha

### Added

- A safe Linux installer script (`install.sh`) that installs both binaries
  into `~/.local/bin`, sets up `~/.config/archivefs`, and never overwrites
  an existing config.

## v0.2.1-alpha

### Added

- Automated Linux release artifacts via GitHub Actions (the `release.yml`
  workflow, release tarballs, and `SHA256SUMS`).
- A release installation guide and an example configuration file
  (`config.toml.example`).
- MIT License.

### Documentation

- Updated GitHub issue templates.

## v0.2.0-alpha

### Added

- Desktop GUI: **Mount All**, a sequential bulk-mount workflow that reports per-archive outcomes and stops cleanly on failure
- Desktop GUI: **Unmount All**, the equivalent sequential bulk-unmount workflow, with optional cleanup of the archive's mount directory afterward
- Lazy-unmount recovery for mounts that are busy at unmount time, with a follow-up offer to remount once the previous mount has been released
- Activity panel in the GUI recording recent mount, unmount, and setup operations, with a Clear action
- First-run Setup flow and a startup Diagnostics report that check the config file, mount root, and required tools before archive actions are allowed
- `status --json` output, joining the existing `stats --json`, `info --json`, and `doctor --json`

### Changed

- README refreshed with a new project banner image and updated description

### Fixed / Safety

- The GUI now retains and can display the last known good snapshot when a background refresh fails, marking it stale instead of discarding it
- Mount and unmount actions are gated on a coherent config identity check (config path plus a SHA-256 digest of its contents), so actions are blocked if the on-disk config changed since the snapshot and diagnostics were last read

## v0.1.0-alpha

### Added

- Linux-first ArchiveScanner
- Read-only archive mounting
- JSON archive index
- File watcher
- Provider pipeline
- Duplicate detector framework
- Filename duplicate detector
- `doctor`
- `config-check`
- `stats`
- `info`
- `duplicates`
- `status`
- `watch`
- JSON output:
  - `stats --json`
  - `info --json`
  - `doctor --json`

### Quality

- GitHub Actions CI
- Clippy clean
- 59 unit tests
- Architecture documentation
- JSON API documentation
