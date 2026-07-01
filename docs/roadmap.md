# Vision

ArchiveFS is a Linux-first archive mounting engine designed around reusable components, providers, plugins, and reliability.

The long-term goal is to make archive-backed libraries easy to inspect, mount, search, validate, and extend without treating the CLI as the center of the system. The core should remain reusable by command-line tools, watchers, future daemons, graphical interfaces, and third-party integrations.

ArchiveFS should stay conservative with user data. Archives are source material, not working files. The engine should mount archives read-only, never rewrite user archives, and prefer explicit operations over surprising automation.

# Milestones

## Now

Current milestone:

- Finish core architecture.
- Polish logging.
- Improve documentation.
- Strengthen tests.

## Next

Near-term follow-up work:

- Duplicate detector plugin.
- Better archive health.
- Richer statistics.

## Future

Longer-term direction:

- Metadata providers.
- Plugin SDK.
- AI metadata provider.
- Web UI.

# Current Status

The current architecture has the main building blocks in place:

- `ArchiveScanner` scans configured source folders and produces reusable archive models.
- `ArchiveRecord` combines identity, metadata, health, mount plan, and mount state.
- A provider pipeline exists through metadata and health provider traits, with built-in filename metadata and filesystem health providers.
- The watcher uses filesystem notifications, debounce logic, event filtering, and JSON index rebuilds without auto-mounting or auto-unmounting.
- The JSON index stores archive paths, platform, display name, mount path, modified time, health, and mount state for summary and search commands.
- Logging supports quiet default output plus verbose and debug modes.
- Stats output summarizes archive counts, mount state, platforms, extensions, and sizes.
- Info output shows a selected archive's detail view using shared selection logic.
- Config validation and doctor-style diagnostics exist for setup checks.
- CI and tests cover core behavior, CLI parsing/formatting, selection, indexing, watcher filtering/debounce behavior, cleanup, stats, info, and error handling.

# Version 0.2.x

Focus: polish the core and reduce rough edges before larger feature work.

- Finish error migration across scanner, mount, unmount, watcher, index, and external command paths.
- Introduce shared output formatting so CLI commands do not each invent their own presentation style.
- Introduce shared byte and time formatting helpers for stats, info, index, and future commands.
- Improve developer documentation and keep architecture docs aligned with code.
- Improve logging messages, levels, and consistency between commands.
- Add more test coverage around error paths, index freshness, watcher rebuild behavior, and CLI output.
- Improve performance in scanning, mount planning, index building, and repeated status/stat/info queries.

# Version 0.3.x

Focus: core quality features that make ArchiveFS more useful on real libraries.

- Add a duplicate detection plugin that builds on `ArchiveIdentity` rather than filename-only matching.
- Improve archive health tracking, including clearer failed, retryable, missing, unsupported, and permission states.
- Improve statistics with more useful breakdowns and clearer handling of unknown or missing size data.
- Enhance configuration with better validation, clearer defaults, and more maintainable parsing.
- Add library cleanup features that help identify stale mount directories, stale index entries, and missing archive paths without destructive defaults.

# Version 0.4.x

Focus: metadata ecosystem.

- Add a ScreenScraper provider.
- Add a MobyGames provider.
- Add an IGDB provider.
- Add metadata caching keyed by archive identity, provider name, provider version, size, and modified time.
- Improve region and language detection from paths, filenames, and provider data.

# Version 0.5.x

Focus: plugin ecosystem.

- Define a Plugin SDK for providers and related extension points.
- Support dynamic provider loading where practical and safe.
- Support third-party plugins without requiring changes to ArchiveFS core.
- Add plugin configuration for enabling, ordering, and configuring providers.

# Version 1.0

Stable release goals:

- Stable public API for core library consumers and plugin authors.
- Comprehensive documentation for contributors, operators, and extension authors.
- Packaging suitable for normal Linux installation and maintenance.
- Long-term maintenance practices for compatibility, migration, and support.
- Complete test suite covering core behavior, CLI behavior, plugin interfaces, watcher behavior, mount safety, and index compatibility.

# Design Principles

- Linux-first.
- Never modify user archives.
- Read-only by default.
- Composition over duplication.
- Test before merge.
- Architecture before features.
- Plugins instead of hardcoding.
- Shared core logic before command-specific logic.
- Explicit user actions for mount, unmount, and cleanup.
- Prefer safe stale data warnings over destructive automatic repair.
- Keep provider failures isolated and understandable.
- Keep CLI output human-readable and script-friendly where practical.
