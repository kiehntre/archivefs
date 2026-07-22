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

## v0.5.0-alpha (unreleased)

Work merged since the `v0.4.3-alpha` tag, prepared for release as
`v0.5.0-alpha`. The workspace version in `Cargo.toml` has not been bumped and
no tag has been created yet - both remain separate release-checklist steps.
See [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md)
for a narrative overview and
[`docs/MANUAL_QA_v0.5.0-alpha.md`](docs/MANUAL_QA_v0.5.0-alpha.md) for the
manual acceptance plan.

### Added

- Redesigned desktop GUI navigation: `Mount`, `Selected`, `Active Mounts`,
  `Doctor`, `History & Logs`, `Settings`, and `About` are now dedicated
  pages, alongside the existing `Library`, `Sources`, `Health`,
  `Duplicates`, and `Library Views` pages. Mount adds a destination
  preview and an explicit mount queue reviewed on Selected before
  anything is mounted; Active Mounts adds confirmed normal unmount;
  Doctor gains a check summary and "Copy report"; History & Logs gains
  operation/result filtering, sorting, and log export; Settings and
  About surface backend-supported configuration, environment, and
  version information read-only. A shared visual system (`archivefs-gui`'s
  `ui` module: typed status badges, cards, buttons, empty/loading states,
  and a responsive page-width policy for laptop through ultrawide
  displays) replaced page-by-page ad hoc styling after an internal
  adversarial audit found the initial integration functionally sound but
  not visually release-ready; see `docs/FABLE_PROGRESS.md` and
  `docs/INTEGRATED_GUI_AUDIT.md` for that audit and rescue record.
- A first-class **Cheats & Mods** GUI workspace (`archivefs-gui`) that
  keeps exact archive context, RetroArch profile discovery, and trusted
  cheat-catalogue retrieval together in one page. It clearly labels
  matching, installation, and mod support as not yet available rather
  than hiding or fabricating those steps.
- A user-facing Cheats & Mods trust and safety model: every source is
  presented as **Trusted**, **Unverified**, or **Blocked**, with local
  safety scanning explicitly labelled planned/unavailable rather than
  silently absent. See
  [`docs/CHEATS_MODS_SAFETY.md`](docs/CHEATS_MODS_SAFETY.md) and the new
  [`docs/CHEATS_MODS_USER_POLICY.md`](docs/CHEATS_MODS_USER_POLICY.md).
- Guided RetroArch cheat setup (`retroarch-cheat-setup`): discovers safe
  native, Flatpak, and verified portable profiles, previews conservative
  matches against a local or trusted-source catalogue, and delegates
  approved changes to a journaled installer. See
  [`docs/RETROARCH_CHEAT_SETUP.md`](docs/RETROARCH_CHEAT_SETUP.md).
- A safe RetroArch cheat installer and journal-driven rollback
  (`retroarch-cheat-rollback`), with destination path safety checks,
  backups before any replacement, and read-only installation history and
  single-run inspection (`retroarch-cheat-history`,
  `retroarch-cheat-inspect`). See
  [`docs/RETROARCH_CHEAT_INSTALL.md`](docs/RETROARCH_CHEAT_INSTALL.md),
  [`docs/RETROARCH_CHEAT_ROLLBACK.md`](docs/RETROARCH_CHEAT_ROLLBACK.md), and
  [`docs/RETROARCH_CHEAT_HISTORY.md`](docs/RETROARCH_CHEAT_HISTORY.md).
- Trusted RetroArch cheat-catalogue retrieval
  (`retroarch-cheat-source-list`, `-fetch`, `-inspect`): a fixed,
  reviewed list of sources only - no arbitrary or user-supplied URLs -
  fetched over certificate-validated HTTPS into a bounded, validated,
  immutable local snapshot with SHA-256 digest and freshness reporting,
  with offline reuse of a previously fetched snapshot. See
  [`docs/RETROARCH_CHEAT_SOURCES.md`](docs/RETROARCH_CHEAT_SOURCES.md) and
  [`docs/RETROARCH_CHEAT_CATALOGUE.md`](docs/RETROARCH_CHEAT_CATALOGUE.md).
- Cheat-source cache maintenance: snapshot inventory, verification,
  pin/unpin, and preview-first pruning that keeps current, last-known-good,
  pinned, and unverifiable snapshots protected from deletion. All cache
  access across processes is coordinated by one bounded, timing-out
  advisory file lock. See
  [`docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md)
  and
  [`docs/RETROARCH_CHEAT_CACHE_LOCKING.md`](docs/RETROARCH_CHEAT_CACHE_LOCKING.md).
- Database diagnostics now distinguish SQLite hot-header evidence, zeroed and
  truncated non-hot journals, malformed headers, and the extended
  `SQLITE_READONLY_ROLLBACK` recovery-required result. Catalogue status, list,
  health, alias/source/list-view previews, and normal GUI catalogue loading use
  the explicit read-only database path. The GUI retains scan worker handles,
  refuses to replace a scan already in progress, and waits for scan/source
  workers during normal shutdown; SQLite durability remains unchanged.
- `database-check` and `database-check --json`: bounded, structured,
  explicitly read-only SQLite health diagnostics with main-file metadata,
  rollback-journal/WAL/SHM evidence, journal mode, schema version,
  `quick_check`, and stable error classifications. The command never creates,
  migrates, repairs, checkpoints, or deletes database files. See
  [`docs/DATABASE_RECOVERY.md`](docs/DATABASE_RECOVERY.md).

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
- A bounded, read-only inventory of existing RetroArch `.cht`, `.ips`,
  `.bps`, `.ups`, and `.xdelta` artifacts, included in
  `retroarch-patch-preview` human and JSON output. It reports empty and
  occupied expected destinations plus duplicate, conflicting, ambiguous,
  and orphaned files; parses only bounded non-executable `.cht` metadata;
  and never follows artifact symlinks or modifies a file. See
  [`docs/RETROARCH_ARTIFACT_INVENTORY.md`](docs/RETROARCH_ARTIFACT_INVENTORY.md).
- Read-only RetroArch playlist identity and content matching: discovers
  and parses modern JSON `.lpl` playlist files from the already-discovered
  Playlists directory (bounded at 4 MiB per file, 1024 files and 65536
  total entries per profile) and uses them as additional evidence in
  `retroarch-patch-preview` - a playlist entry's own resolved content path,
  core association, and database name can now upgrade an `AmbiguousCore`/
  `UnsupportedNoCore` result to a precise `ExactCore` one when the evidence
  is unambiguous, without ever downgrading an already-correct extension-
  based match. No playlist is ever written, repaired, or created, and the
  binary `.rdb` database is never parsed. Purely additive to both
  `retroarch-environment --json` and `retroarch-patch-preview --json`
  (`format_version` stays `1` on each, per this project's documented JSON
  policy of allowing new fields without a version bump). See
  [`docs/RETROARCH_PLAYLISTS.md`](docs/RETROARCH_PLAYLISTS.md).
- Read-only RetroArch AppImage detection: scans a fixed set of default
  locations (`~/Applications`, `~/.local/bin`,
  `~/.local/share/applications`, `~/AppImages`, `~/bin`) and your XDG
  desktop-entry directories for `.desktop` files, entirely read-only and
  non-recursive, and feeds any detected AppImage into the existing
  environment/playlist/patch-preview pipeline. An AppImage sharing the
  native profile's own configuration (the common case) is attached to the
  existing native profile's new `app_images` field with no new profile
  created; an AppImage with verified evidence of a genuinely distinct
  configuration (the official AppImage-runtime portable-mode
  `.home`/`.config` sibling-directory convention, or an explicit
  `-c`/`--config` in its desktop launcher) gets its own profile instead,
  never a duplicate. Never executes, mounts, extracts, or FUSE-mounts an
  AppImage; never invokes an external tool; never writes or modifies an
  AppImage or `.desktop` file. Because a distinct-configuration AppImage
  inserts a 4th `profiles[]` entry between native and Flatpak/user,
  `retroarch-environment --json`'s `format_version` moves from `1` to `2`;
  `retroarch-patch-preview` needed no matching/orchestration changes at
  all, since it already iterates `environment.profiles` generically. See
  [`docs/RETROARCH_APPIMAGE.md`](docs/RETROARCH_APPIMAGE.md).

### Changed

- Rust CI reproducibility: the project now pins an exact Rust toolchain
  (`1.97.1`) via `rust-toolchain.toml`, and both `.github/workflows/ci.yml`
  and `.github/workflows/release.yml` install that exact version explicitly
  instead of a floating `stable` channel. See the
  [Rust toolchain policy](CONTRIBUTING.md#rust-toolchain-policy) in
  `CONTRIBUTING.md` for why.
- The desktop GUI's primary navigation moved from a single top tab bar to
  a persistent left-hand page list covering every destination above, with
  `Health`, `Duplicates`, and `Library Views` kept reachable as a
  secondary group rather than removed.

### Security

- Mount and unmount now verify their own postcondition instead of trusting
  the external `ratarmount`/unmount command's exit status alone: a mount is
  only reported successful if the destination is actually present in
  `/proc/self/mountinfo` afterward, and an unmount is only reported
  successful once the mount has actually disappeared.
- Source-folder scanning requires an explicit, absolute, non-root path,
  rejects duplicate or nested configured roots, refuses symlink path
  components, and never follows a symlink entry encountered below a valid
  root. Recursive scans are bounded by entry-count and depth limits.
- Catalogue refreshes now run inside one SQLite write transaction with a
  savepoint per source folder: a single failing source rolls back only its
  own writes and is recorded truthfully, a fatal failure rolls back the
  entire refresh, and a killed process can never leave a half-updated
  catalogue visible to the next read.
- Every process sharing a RetroArch cheat-source cache root now
  coordinates through one exclusive, directory-identity-based advisory
  lock with a deterministic five-second timeout - covering listing,
  inspection, retrieval, publication, inventory, verification, pinning,
  and pruning - instead of relying on filesystem timing alone. Locking is
  additional defense; every prune candidate is still independently
  revalidated (pin state, current pointer, hash, path, symlinks) at
  execution time regardless of the lock.
- Non-UTF-8 source-folder and archive-path handling was hardened
  end-to-end: a non-UTF-8 source root is rejected at the config boundary
  rather than silently altered, while archive names below a valid source
  remain exact, byte-preserving values throughout scanning and the
  catalogue.

### Fixed

- Bounded emulator-environment directory listings now use
  `symlink_metadata` directly, preserving their documented
  final-component no-follow contract even when a symlink target exists.
- An ambiguous float literal (`egui::Stroke::new(2.0, stroke_color)`) that a
  newer Rust compiler's stricter lint started rejecting was made explicit
  (`2.0_f32`). This was a compiler-drift break, not a logic change: the code
  had been correct and passing CI until an unpinned `stable` toolchain moved
  out from under it.
- The GUI's trusted cheat-source listing now correctly propagates a cache
  read/lock failure as an error message instead of a type mismatch that
  the cache-locking change above introduced when source listing became
  fallible.

### Known limitations

- RetroArch cheat **matching**, **installation**, and **rollback** are
  fully implemented and tested at the CLI/core level, but are **not**
  reachable from the GUI's Cheats & Mods workspace yet - only profile
  discovery and trusted catalogue retrieval are. The GUI states this
  explicitly rather than hiding the steps.
- Cache pin/unpin and prune controls exist at the CLI/core level but have
  no GUI surface yet.
- There is no general-purpose local or community cheat/mod import
  inspection pipeline yet. Local safety scanning is presented in the GUI
  as planned/unavailable, with no toggle that would pretend to change
  protection.
- Arbitrary or user-supplied cheat-source URLs are not accepted anywhere;
  only the fixed, reviewed trusted-source list can be fetched.
- Mod installation and emulator-specific mod adapters do not exist yet;
  the Mods section of the Cheats & Mods workspace is a labelled
  placeholder.
- Operation history in the GUI's History & Logs page remains in-memory
  for the current session; it is not yet persisted to disk.
- Settings remains read-only for backend-supported configuration;
  appearance/density and other GUI-only preferences are not yet editable,
  and there is no update-check mechanism.
- A read-only PCSX2 adapter is under separate, parallel development and is
  not part of this release.

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
