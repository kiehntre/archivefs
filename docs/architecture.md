# Overview

ArchiveFS is a Linux-first tool for exposing archive files as read-only directory trees, and for browsing, organizing, and previewing archived collections without extracting them permanently. The implementation focuses on conservative filesystem behavior, reusable core types, and CLI/GUI commands built on shared library code in `archivefs-core`.

Core project principles:

- Linux-first: current mount and watcher behavior assumes Linux facilities such as `/proc/self/mountinfo`, FUSE-style mount tools, and `notify` with Linux-friendly event handling.
- Read-only archive mounting: archives are mounted as folder views through `ratarmount`; ArchiveFS does not provide writable archive editing.
- Never modify user archives: scanner, status, stats, info, watcher, catalogue, library-view, and patch-preview operations read archive metadata and filesystem state but do not rewrite source archive files.
- Build reusable components before features: commands are thin wrappers over `archivefs-core` types such as `ArchiveScanner`, `ArchiveRecord`, `Database`, `LibraryViewConfig`, and the patch-manager types.
- Extension points before hardcoding, but never forced: metadata/health provider traits and the `EmulatorAdapter` trait exist so new sources of information and new emulators can be added without redesigning shared orchestration - but only when a trait genuinely fits. The current built-in providers are filename metadata and filesystem health; PCSX2's read-only patch preview is the current (and only) `EmulatorAdapter` implementation. RetroArch's read-only patch/cheat preview is a second, independent preview built the same way PCSX2's was reviewed - by checking the existing trait against real requirements first - and shipped as its own narrowly-scoped type once that trait genuinely did not fit, rather than weakening the trait to force a fit.
- The database is additive, never load-bearing for safety: the persistent catalogue (below) caches what the filesystem scanner would discover anyway. Mount, unmount, lazy-unmount, and cleanup code paths read live filesystem and mount state directly and do not depend on the catalogue - see [ADR 0001](adr/0001-persistent-library-database.md).

# High-Level Architecture

```text
                  +----------------------+
                  |    archivefs-cli     |
                  | commands + printing  |
                  +----------+-----------+
                             |
                             v
                  +----------------------+
                  |   archivefs-core     |
                  | config, scan, index, |
                  | selection, mounting  |
                  +----+-----------+-----+
                       |           |
                       |           v
                       |    +------------------+
                       |    | Providers        |
                       |    | metadata/health  |
                       |    +------------------+
                       |
        +--------------+----------------+
        |                               |
        v                               v
+---------------+              +----------------+
| Source folders|              | Mount root     |
| archives      |              | planned mounts |
+-------+-------+              +-------+--------+
        |                              |
        v                              v
+---------------+              +----------------+
| ArchiveRecord |              | ratarmount /   |
| values        |              | unmount tools  |
+-------+-------+              +----------------+
        |
        v
+---------------+
| JSON index    |
| cache/search  |
+---------------+
```

# Core Components

`ArchiveScanner` is the main read path over configured source folders. It recursively scans each source folder, detects supported archive files, sorts and deduplicates discovered archives, creates mount plans, reads current mount state, and produces `ArchiveRecord` values. It is reused by scan, status, stats, info, mount planning, index building, and watcher rebuilds.

`ArchiveRecord` is the combined current view of one archive. It carries identity, provider metadata, mount plan, archive health, and mount state. Commands that need rich archive details should prefer records instead of rescanning or recomputing separate models.

`ArchiveIdentity` stores stable archive identity data derived from the filesystem and path: display name, normalized name, source root, size, modified time, optional platform, optional region, and reserved hash fields. It is intentionally broader than a filename so future duplicate detection and metadata enrichment do not depend on filenames alone.

`ArchiveMetadata` stores provider-enriched descriptive data such as title, platform, region, language, version, publisher, developer, release year, genre, notes, and source. Today this is filled by the filename provider; richer providers are future work.

`ArchiveStats` is an aggregate summary over `ArchiveRecord` values. It reports total archives, mounted count, pending count, platform counts, extension counts, largest and smallest archive, and total archive size.

`ArchiveInfo` is the selected detail view for one archive. It includes title, platform, archive path, mount path, extension, size, modified time, health, mount state, metadata provider, and health provider.

`ArchiveFsError` is the central core error type. It groups errors by domain: config, scanner, selection, mount, unmount, index, watcher, I/O, and external command failures. Selection errors preserve the existing no-match and ambiguous-match messages used by mount-one, unmount-one, and info.

# Providers

`MetadataProvider` is the trait for producing `ArchiveMetadata` from an `Archive`. It allows metadata enrichment to be swapped without changing scanner or command code.

`FilenameMetadataProvider` is the current built-in metadata provider. It derives title, platform, and region from the archive filename/path and existing platform detection heuristics.

`HealthProvider` is the trait for producing archive health from an `Archive`. It allows future health checks to be implemented independently from scanning.

`FilesystemHealthProvider` is the current built-in health provider. It reflects the archive health already known from filesystem discovery rather than doing deep archive inspection.

# Commands

This section groups commands by area. Run `archivefs --help` for the exact,
current, authoritative list and usage examples - this document explains what
each group does, not every flag.

## Scanning and mounting

`scan` loads config, uses `ArchiveScanner`, and prints supported archive paths.

`doctor` runs readiness diagnostics: config loading, source folders, mount root, tool availability, archive scan, and mount status summary.

`config-check` validates configuration and prints pass/warn/error checks.

`status` builds current archive records and prints archive path, mount path, and mount state.

`stats` builds current archive records once and prints aggregate counts and size information.

`info <archive-path-or-name>` builds current archive records once, reuses the same selection logic as mount-one/unmount-one, and prints detailed information for one archive.

`mount` scans archives, plans mount paths, creates needed mount directories, and mounts archives that are not already mounted.

`mount-one <archive-path-or-name>` scans and plans mounts, selects one archive with shared selection logic, and mounts only that archive if needed.

`unmount` reads mounted paths under the configured mount root and unmounts those paths.

`unmount-one <archive-path-or-name>` scans and plans mounts, selects one archive with shared selection logic, and unmounts only that archive's mount path.

`clean` removes empty directories under the mount root while preserving mounted paths and non-empty directories.

`watch` observes configured source folders and rebuilds the JSON index after debounced archive-related changes. It does not mount or unmount.

`index-build` builds current archive records and writes the JSON index.

`index-show` reads the JSON index, checks for missing or stale archive paths, and prints an index summary.

`index-find <query>` reads the JSON index, checks freshness, and searches archive path, display name, platform, and mount path fields.

`duplicates` reports filename-based duplicate candidates (`FilenameDuplicateDetector`) without changing anything on disk.

## Persistent catalogue and multi-source management

`library-status` shows the persistent library database's health and counts. `database-check` is the separate safety-oriented diagnostic: it uses explicit SQLite read-only flags, never creates the database or its parent, never migrates, and reports sidecars and stable error categories. `health` shows catalogue archive health (missing entries, unknown platform) without scanning, mounting, or unmounting. `library-scan` scans configured source folders into the database. `library-list` and `library-find` read from the database without rescanning. Recovery remains a documented, copy-first manual workflow; see [`DATABASE_RECOVERY.md`](DATABASE_RECOVERY.md).

`library-set-platform` / `library-clear-platform` (and their `-bulk` variants) record manual platform overrides that outrank automatic detection. `library-remove-missing` removes catalogue rows whose source file is gone by exact id/path - it never deletes files.

`platform-alias-list` / `platform-alias-add` / `platform-alias-remove` manage persistent, user-defined folder-name-to-platform aliases, applied on the next scan.

`sources` lists configured source folders and status; `sources scan-all` scans every enabled one. `source add` / `source enable` / `source disable` / `source scan` / `source remove` manage one source folder at a time, including whether removing a source keeps or removes its catalogue entries.

See [`docs/database.md`](database.md) and [`docs/DATABASE_DESIGN.md`](DATABASE_DESIGN.md) for the schema and design rationale, and [ADR 0001](adr/0001-persistent-library-database.md) for why the catalogue is additive rather than authoritative.

## Managed library views

`view list` / `view preview` / `view apply` / `view repair` / `view remove` manage named, symlink-based organized views of the catalogue - for example, a view that groups archives by platform into a separate directory tree a frontend can browse, without moving or copying the underlying archives. `preview` shows the plan without changing anything; `apply`/`repair` create or fix the managed symlinks; `remove` removes them (optionally keeping the view's own configuration). See [`docs/library-views.md`](library-views.md).

## Patch preview

`pcsx2-patch-preview` fetches official PCSX2 patch metadata and prints a read-only, non-executable advisory plan describing native/Flatpak installation candidates. It does not download, verify, install, or enable anything. See [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md) for the full design and current implementation boundary.

## Emulator environment discovery

`retroarch-environment` discovers the local RetroArch environment: native and Flatpak (user- and system-scope) installation profiles, `retroarch.cfg` location and parse outcome, a fixed set of configured paths, and installed cores with their `.info` metadata. It lives in `archivefs-core::emulator_environment`, originally a fully independent sibling to `patch_manager` - there is no "game" or "patch" concept here, only "what already exists on disk for this emulator." It is strictly read-only: no file is created, modified, or deleted, no process is spawned, and no network call is made. See [`docs/RETROARCH_ENVIRONMENT.md`](RETROARCH_ENVIRONMENT.md) for the full design record, including the primary RetroArch/Flatpak source citations it is based on.

`retroarch-patch-preview` is the second concrete preview built on `patch_manager`, after PCSX2's. It reuses `emulator_environment::retroarch`'s discovery directly (the one intentional crossing of the "sibling, not part of" boundary above) rather than rediscovering any path, and for every present catalogue archive previews per-game `.cht` cheat and IPS/BPS/UPS/Xdelta soft-patch sibling destinations across every discovered RetroArch profile. It deliberately does **not** implement `EmulatorAdapter` or produce an `AdvisoryPatchPlan`: RetroArch's several purpose-tagged directories per installation and its core-selection ambiguity (which installed core would load a given file) do not fit PCSX2's single-`data_root`, single-hypothetical-path shape, so it ships as an independent `patch_manager::retroarch` module with its own `RetroArchAdvisoryPlan` type instead - no PCSX2 type, plan ID, JSON shape, or CLI output changed. It also makes no network call at all: unlike PCSX2, no RetroArch metadata source has been reviewed. See [`docs/RETROARCH_PATCH_PREVIEW.md`](RETROARCH_PATCH_PREVIEW.md) for the full design record, including the primary RetroArch source citations the cheat/patch destination conventions are based on.

RetroArch's own `.lpl` playlist files add a second, stronger source of matching evidence on top of the extension-only matching above: `emulator_environment::retroarch` discovers and parses them (reusing its own already-resolved `Playlists` directory, bounded and read-only, never writing or modifying a playlist), and `patch_manager::retroarch` compares each entry's resolved content path, core association, and database name against the present catalogue - strong enough evidence can upgrade an `AmbiguousCore`/`UnsupportedNoCore` result to a precise `ExactCore` one, but never overrides an already-correct extension-based match. Both `retroarch-environment --json` and `retroarch-patch-preview --json` gained only additive fields for this; `format_version` stays `1` on each. See [`docs/RETROARCH_PLAYLISTS.md`](RETROARCH_PLAYLISTS.md) for the full design record, including the primary RetroArch source citations the playlist format and matching model are based on.

`emulator_environment::retroarch` also detects RetroArch installed as an AppImage - many users' primary way of running RetroArch on Linux - by scanning a fixed set of default search roots and XDG desktop-entry directories, read-only and non-recursive, never executing, mounting, or extracting an AppImage. In the common case (an AppImage shares the native profile's own configuration) this is purely additive: a new `app_images` field on the existing native profile, with zero new profile-array entries. Only when an AppImage has verified evidence of a genuinely distinct configuration root (the official AppImage-runtime portable-mode convention, or an explicit `-c`/`--config` in its desktop launcher) does a 4th profile appear, inserted between native and Flatpak/user - and at most one such distinct profile is ever created; disagreeing evidence across multiple AppImages folds back to the native profile rather than guessing. Because this changes `profiles[]`'s positional shape (not just adding a field), `retroarch-environment --json`'s `format_version` moved from `1` to `2`; `retroarch-patch-preview` required no orchestration changes at all beyond one plan-ID hash tag, since it already iterates `environment.profiles` generically. See [`docs/RETROARCH_APPIMAGE.md`](RETROARCH_APPIMAGE.md) for the full design record, including the primary AppImage-runtime and freedesktop.org Desktop Entry Specification source citations this is based on.

# Watcher

The watcher uses the Rust `notify` crate to observe configured source folders recursively. Events are treated as hints that the archive library may have changed.

Watcher processing is debounced. Repeated archive-related events are collected for a short quiet period before rebuilding the index, which avoids writing the JSON index for every low-level filesystem event.

Event filtering accepts only archive-relevant changes. It ignores directory-only events, read-only access events, unrelated file extensions, and temporary or incomplete download suffixes such as `.part`, `.partial`, `.crdownload`, `.download`, `.tmp`, `.temp`, `.!qB`, `.aria2`, and files ending in `~`.

The current watcher rebuild path updates JSON only. It calls the same index build path used by `index-build`, preserving shared scanning, provider, and mount-state logic.

The watcher never auto-mounts archives. Mounting remains an explicit command.

The watcher never auto-unmounts archives. Cleanup and unmounting remain explicit commands.

# JSON Index

The JSON index is a lightweight cache of current archive records for fast summary and search commands.

It stores one entry per indexed archive:

- archive path
- platform
- display name
- mount path
- modified time seconds
- health
- mount state

The index exists so commands such as `index-show` and `index-find` can inspect a previously built library view without immediately rescanning every source folder. Freshness checks compare stored paths and modified times against the filesystem and warn when the index may be stale.

# Error Handling

`ArchiveFsError` is the shared error type for `archivefs-core`. It separates errors into domain categories so callers can distinguish configuration problems, scanner failures, selection failures, mount/unmount failures, index failures, watcher failures, I/O failures, and external command failures.

Display output is intentionally human-readable because the CLI prints errors directly. The current migration keeps CLI behavior stable where practical, especially for selection errors such as no match and ambiguous match.

# Logging

Normal CLI execution is quiet by default. Operational logs are not printed unless a global logging flag is used.

`--verbose` or `-v` enables info-level logging to stderr. This is intended for normal operational detail such as config loading, scan counts, index rebuilds, and mount/unmount progress.

`--debug` enables debug-level logging to stderr. This includes lower-level diagnostic detail such as scanned source folders, discovered archive paths, index paths, watcher statistics, and accepted watcher paths.

Command output remains on stdout; logs go to stderr.

# Testing

ArchiveFS tests favor small, focused unit tests around core behavior. The current suite exercises archive extension detection, split archive skipping, platform detection, safe mount naming, duplicate mount path generation, selection errors, archive stats, archive info, index JSON, index freshness, config checks, doctor reports, cleanup behavior, watcher filtering/debouncing, and command argument parsing.

The testing philosophy is to keep dangerous behavior behind abstractions and use lightweight fakes where possible. For example, mount and unmount command tests use a recording backend rather than invoking real system mount tools. Watcher behavior is tested at the filtering and debounce layers rather than requiring OS event integration for every case.

# Future Architecture

See [`/ROADMAP.md`](../ROADMAP.md) for the full, current roadmap. At the
architecture level, the main open extension points are:

- Additional emulator patch/cheat previews beyond PCSX2 and RetroArch,
  each independently reviewed for whether `EmulatorAdapter` genuinely
  fits or whether (as with RetroArch) a separate, narrowly-scoped
  advisory type is more honest - see
  [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md).
- Richer, hash-based duplicate detection beyond today's filename-based
  `FilenameDuplicateDetector`, described in
  [`docs/duplicate-detector.md`](duplicate-detector.md).
- Additional metadata providers (for example ScreenScraper) beyond today's
  `FilenameMetadataProvider`, following the provider-pipeline design in
  [`docs/provider-pipeline.md`](provider-pipeline.md).
- Growing the persistent catalogue schema toward the longer-horizon
  `platforms -> titles -> releases -> archives -> mounts/health_events`
  hierarchy sketched in [`docs/database.md`](database.md), as metadata and
  health-history become real work.
