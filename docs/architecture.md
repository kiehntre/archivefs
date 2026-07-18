# Overview

ArchiveFS is a Linux-first tool for exposing archive files as read-only directory trees, and for browsing, organizing, and previewing archived collections without extracting them permanently. The implementation focuses on conservative filesystem behavior, reusable core types, and CLI/GUI commands built on shared library code in `archivefs-core`.

Core project principles:

- Linux-first: current mount and watcher behavior assumes Linux facilities such as `/proc/self/mountinfo`, FUSE-style mount tools, and `notify` with Linux-friendly event handling.
- Read-only archive mounting: archives are mounted as folder views through `ratarmount`; ArchiveFS does not provide writable archive editing.
- Never modify user archives: scanner, status, stats, info, watcher, catalogue, library-view, and patch-preview operations read archive metadata and filesystem state but do not rewrite source archive files.
- Build reusable components before features: commands are thin wrappers over `archivefs-core` types such as `ArchiveScanner`, `ArchiveRecord`, `Database`, `LibraryViewConfig`, and the patch-manager types.
- Extension points before hardcoding: metadata/health provider traits and, more recently, the `EmulatorAdapter` trait, exist so new sources of information and new emulators can be added without redesigning shared orchestration. The current built-in providers are filename metadata and filesystem health; the current built-in adapter is PCSX2's read-only patch preview.
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

`library-status` shows the persistent library database's health and counts. `health` shows catalogue archive health (missing entries, unknown platform) without scanning, mounting, or unmounting. `library-scan` scans configured source folders into the database. `library-list` and `library-find` read from the database without rescanning.

`library-set-platform` / `library-clear-platform` (and their `-bulk` variants) record manual platform overrides that outrank automatic detection. `library-remove-missing` removes catalogue rows whose source file is gone by exact id/path - it never deletes files.

`platform-alias-list` / `platform-alias-add` / `platform-alias-remove` manage persistent, user-defined folder-name-to-platform aliases, applied on the next scan.

`sources` lists configured source folders and status; `sources scan-all` scans every enabled one. `source add` / `source enable` / `source disable` / `source scan` / `source remove` manage one source folder at a time, including whether removing a source keeps or removes its catalogue entries.

See [`docs/database.md`](database.md) and [`docs/DATABASE_DESIGN.md`](DATABASE_DESIGN.md) for the schema and design rationale, and [ADR 0001](adr/0001-persistent-library-database.md) for why the catalogue is additive rather than authoritative.

## Managed library views

`view list` / `view preview` / `view apply` / `view repair` / `view remove` manage named, symlink-based organized views of the catalogue - for example, a view that groups archives by platform into a separate directory tree a frontend can browse, without moving or copying the underlying archives. `preview` shows the plan without changing anything; `apply`/`repair` create or fix the managed symlinks; `remove` removes them (optionally keeping the view's own configuration). See [`docs/library-views.md`](library-views.md).

## Patch preview

`pcsx2-patch-preview` fetches official PCSX2 patch metadata and prints a read-only, non-executable advisory plan describing native/Flatpak installation candidates. It does not download, verify, install, or enable anything. See [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](PATCH_CHEAT_MANAGER_DESIGN.md) for the full design and current implementation boundary.

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

- Additional `EmulatorAdapter` implementations beyond PCSX2 (RetroArch is the
  planned next one) built on the boundary in
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
