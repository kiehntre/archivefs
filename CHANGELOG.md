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

## v0.6.0-alpha (unreleased)

Work merged since the `v0.5.0-alpha` tag, in preparation for release as
`v0.6.0-alpha`. The workspace version in `Cargo.toml` has not been bumped and
no tag has been created yet. See
[`docs/RELEASE_NOTES_v0.6.0-alpha.md`](docs/RELEASE_NOTES_v0.6.0-alpha.md) for
a narrative overview and
[`docs/MANUAL_QA_v0.6.0-alpha.md`](docs/MANUAL_QA_v0.6.0-alpha.md) for the
manual acceptance plan.

### Added

- **Shared verified game identity**: bounded, read-only PS2/GameCube/Wii
  disc-identity extraction (product code, Game ID, revision, and, for PS2,
  PCSX2's executable CRC) from a local ISO or single-ISO ZIP, shown as an
  explicit `Verified`/`Candidate`/`Missing`/etc. evidence state rather than a
  guessed name. Feeds exact matching in the PCSX2 and Dolphin adapters. See
  [`docs/SHARED_GAME_IDENTITY.md`](docs/SHARED_GAME_IDENTITY.md).
- **Shared read-only Cheats & Mods preview and conflict detection** across
  all three adapters: a typed `Install new` / `Already installed` /
  `Replace different` / `Conflict` / `Ambiguous` / etc. report with no apply
  path of its own. See
  [`docs/SHARED_CHEAT_PREVIEW.md`](docs/SHARED_CHEAT_PREVIEW.md).
- **Shared safe apply, backup, journal, history, and rollback foundation**: a
  bounded transaction pipeline with atomic temp-file-then-rename writes,
  verified never-overwritten backups before any replacement, schema-versioned
  journals, truthful partial-success reporting, and rollback that blocks on
  user-modified content or a missing/changed backup. See
  [`docs/SHARED_SAFE_APPLY_ROLLBACK.md`](docs/SHARED_SAFE_APPLY_ROLLBACK.md).
- **RetroArch GUI apply, history, and rollback**: an eligible exact or
  approved-strong RetroArch trusted-catalogue match can now be applied
  through the shared transaction engine directly from Cheats & Mods -
  preview, explicit confirmation (with a separate, non-preselected
  replacement approval), background execution, and a result shown as
  success, partial success, or failure. History & Logs can open the exact
  operation and preview/confirm its rollback. PCSX2 and Dolphin remain
  preview-only - see [`docs/RETROARCH_GUI_APPLY_HISTORY.md`](docs/RETROARCH_GUI_APPLY_HISTORY.md).
- **RetroArch trusted catalogue download and management**: the Sources page
  now owns catalogue retrieval end-to-end - Download/Update/Verify with an
  explicit review-then-confirm dialog before any network access, background
  retrieval with cancellation, and an activated snapshot that Cheats & Mods
  matches against immediately. See
  [`docs/RETROARCH_CHEAT_SOURCES.md`](docs/RETROARCH_CHEAT_SOURCES.md).
- **Recently Found**: a new navigation page listing only the newest
  completed scan's added archives, in exact path order, backed by a
  persistent append-only observation log and bounded to 10,000 entries with
  explicit truncation reporting. Reuses the existing Library table
  (search/filter/sort/selection all remain available). See
  [`docs/LIBRARY_SCAN_USABILITY.md`](docs/LIBRARY_SCAN_USABILITY.md).
- **Mega Drive/Genesis loose-ROM recognition**: `.gen`/`.smd` files are
  recognized case-insensitively; ambiguous `.md`/`.bin` files are recognized
  only when located under an exactly-named Mega Drive/Genesis folder
  component (`megadrive`, `mega-drive`, `genesis`, `sega-genesis`, and
  similar aliases), never from the filename alone - so an unrelated
  `README.md` outside such a folder is never imported. See
  [`docs/LIBRARY_SCAN_USABILITY.md`](docs/LIBRARY_SCAN_USABILITY.md).

### Changed

- Settings, Doctor, About, Sources, Library Views, and History & Logs now
  share one scrollable-page wrapper supporting mouse wheel, touchpad, Page
  Up/Down, Home, and End, recalculated on resize or Activity-panel
  expansion. Cheats & Mods retains its own scroll region and does not yet
  use this wrapper - see Known limitations.
- Library database schema version 4 adds persistent per-scan counters for
  unchanged, skipped-unsupported, and skipped-ambiguous files, so scan
  summaries can report them without generating one activity event per file.

### Security

- The shared apply pipeline reopens every source no-follow, rejects symlink
  components and special files, and compares device/inode/size/mtime around
  every read before trusting a digest.
- Every shared-apply transaction acquires an exclusive advisory lock on its
  one destination root (5-second timeout, released on drop), and one
  transaction always has exactly one destination root, so lock ordering is
  deadlock-free by construction.
- A confirmed apply is bound to a SHA-256 plan ID covering the exact
  adapter, archive, identity, profile, destination, and action set; any
  context change between preview and confirmation fails closed rather than
  silently re-planning.
- A journal-write failure that happens *after* a destination write already
  succeeded is reported as `partial_failure`, never as silent success or an
  opaque hard failure.
- Rollback re-derives a fresh preview immediately before acting and blocks
  on user-modified destination content, a missing or changed backup, or an
  already-completed rollback (enforced by a separate, non-overwritable
  rollback marker).
- RetroArch catalogue Download/Update never touches the network until the
  user explicitly confirms a review dialog naming the provider and the
  exact ArchiveFS-managed destination; cancelling at any point before that
  confirmation writes nothing and leaves the previously active snapshot,
  if any, unchanged.

### Known limitations

- PCSX2 and Dolphin remain **preview-only**: both have real verified
  identity and real read-only inspection of emulator-managed files, but
  neither has an approved, independently materialized source artifact to
  apply from, so neither offers Install/Apply/Enable/Disable/Rollback
  anywhere in the GUI.
- Mods remain planned and are not implemented; the Mods section of Cheats &
  Mods is a labelled placeholder, not a working feature.
- **A currently open issue**: some individual malformed entries in a
  downloaded RetroArch trusted catalogue can affect validation of the whole
  snapshot rather than being cleanly isolated as non-actionable, and the
  Cheats & Mods "Stage 3" copy in one place still reads "Archive matching
  and cheat installation are not yet implemented in this GUI workflow" even
  though RetroArch matching/apply now exist. Both are being actively fixed;
  neither is resolved as of this entry. See `docs/V0.6_RELEASE_AUDIT.md` for
  current status before relying on either behavior.
- There is no cancellation once a shared-apply write has actually begun -
  only before it starts.
- Cheats & Mods does not yet use the shared scrollable-page keyboard
  wrapper the other listed pages use.
- Operation history in the GUI's History & Logs page remains in-memory for
  the current session; it is not yet persisted to disk.
- No general-purpose local or community cheat/mod import inspection
  pipeline exists; only the fixed, reviewed RetroArch trusted-source list
  can be fetched, never an arbitrary or user-supplied URL.

## v0.5.0-alpha

Released. `Cargo.toml` reads `0.5.0-alpha` and the `v0.5.0-alpha` tag exists.
See [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md)
for the narrative overview and
[`docs/MANUAL_QA_v0.5.0-alpha.md`](docs/MANUAL_QA_v0.5.0-alpha.md) for the
manual acceptance plan used at the time.

Cheats & Mods reached its intended **three read-only emulator adapters**:
RetroArch, PCSX2, and Dolphin. Adapter expansion paused here - see
[`ROADMAP.md`](ROADMAP.md#medium-term-plans).

### Added

- **Three-adapter Cheats & Mods architecture.** Cheats & Mods now
  integrates three read-only emulator adapters - RetroArch, PCSX2, and
  Dolphin - each gated to its own platform(s) with explicit profile
  selection and no install/apply/rollback control anywhere. This is the
  intended stopping point for adapter expansion for now - see
  [`ROADMAP.md`](ROADMAP.md#medium-term-plans).
- Read-only PCSX2 profile and PNACH inspection in Cheats & Mods: discovers
  native, Flatpak, and explicitly supplied portable PCSX2 profiles, and
  inspects existing `cheats`/`cheats_ws`/`patches` directories and `.pnach`
  files - read-only, nothing written or created. Exact matching requires a
  separately verified PCSX2 executable CRC, which ArchiveFS does not yet
  have, so no exact match is ever claimed. No Install, Apply, Enable,
  Disable, or rollback control exists. See "PCSX2 read-only adapter" in
  [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md)
  for full detail.
- Read-only Dolphin profile and Game INI inspection in Cheats & Mods:
  discovers native, Flatpak, and explicitly supplied Dolphin configuration
  roots, and inspects existing `GameSettings/*.ini` files for GameCube/Wii
  archives - read-only, nothing written or created, and no texture pack,
  graphics mod, resource pack, or Riivolution asset is inspected. Exact
  matching requires a separately verified Dolphin Game ID, which ArchiveFS
  does not yet have, so no exact match is ever claimed. No Install, Apply,
  Enable, Disable, or rollback control exists. See "Dolphin read-only
  adapter" in
  [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md)
  for full detail.
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
- PCSX2's exact CRC matching remains deferred (requires a separately
  verified PCSX2 executable CRC, which ArchiveFS does not yet have), and
  there is no PCSX2 preview, installation, or rollback support.
- Dolphin's exact matching remains deferred (requires a separately verified
  Dolphin Game ID, which ArchiveFS does not yet have), there is no
  texture-pack, graphics-mod, resource-pack, or Riivolution-asset
  inspection, and there is no Dolphin installation or rollback support. A
  Nobara-specific manual QA run for the Dolphin adapter remains
  outstanding (validated on Ubuntu 24.04.4 LTS at merge time).
- Further emulator adapter expansion beyond RetroArch/PCSX2/Dolphin is
  paused for now - see [`ROADMAP.md`](ROADMAP.md#medium-term-plans).

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
