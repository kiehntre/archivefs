# ArchiveFS Roadmap

This roadmap is reconstructed from the current codebase, its test suite, and
its commit/tag history - not from a fixed release schedule. It contains no
promised dates. Items move from later sections to "Completed foundations" only
once they are actually implemented and tested.

For what has already shipped release-by-release, see [`CHANGELOG.md`](CHANGELOG.md).

## Completed foundations

These are implemented, tested, and in current use today:

- Read-only archive mounting through `ratarmount`, with safe mount-name
  generation and deterministic handling of duplicate names.
- Source and destination path validation: mounts are only created under the
  configured `mount_root`; unmounts only ever target paths under it; source
  folders are validated against overlap with each other and with mount roots
  and Library View destinations.
- Safe path handling for non-UTF-8 archive paths, symlink-escape checks
  around `mount_root`, and parent-only resolution during lazy-unmount
  validation.
- Batch mounting and unmounting (Mount All / Unmount All in the GUI, `mount`
  / `unmount` in the CLI), with per-archive outcome reporting and lazy-unmount
  recovery for busy mounts.
- Execution-time revalidation: mount/unmount actions are gated on a config
  identity check (path plus content digest), so a config change since the
  last scan blocks stale actions instead of silently acting on old state.
- GUI progress reporting, operation summaries, and an activity history panel
  for recent mount/unmount/setup operations.
- Cleanup handling (`clean`) that removes empty mount directories without
  touching mounted or non-empty ones.
- A persistent SQLite-backed library catalogue (`archivefs-core::database`),
  additive to and never a dependency of mount/unmount safety - see
  [`docs/adr/0001-persistent-library-database.md`](docs/adr/0001-persistent-library-database.md).
- Read-only database diagnosis (`database-check`, including stable JSON), with
  sidecar evidence, bounded consistency checking, and recovery guidance that
  never performs automatic repair.
- Multi-source folder management (`sources`, `source add/enable/disable/scan/remove`)
  with independent per-source scan status.
- Manual platform assignment and persistent folder-name platform aliases
  that outrank automatic detection, plus an unknown-platform review
  workflow.
- Filename-based duplicate detection (`duplicates`, `FilenameDuplicateDetector`)
  and a GUI duplicate review workflow.
- Managed library views: named, symlink-based organized views of the
  catalogue with plan/preview/apply/repair/remove semantics and a manifest
  that records exactly which symlinks ArchiveFS created, so repair and
  removal only ever touch paths ArchiveFS manages.
- A read-only archive content inspector used to improve mount-readiness
  checks without extracting archives.
- A read-only PCSX2 patch-preview foundation: fetches official PCSX2 patch
  metadata and reports native/Flatpak installation candidates as a
  non-executable advisory plan. It does not download, install, or enable
  anything.
- An emulator-neutral patch adapter boundary (`EmulatorAdapter` and related
  types), extracted from the PCSX2-specific implementation so future
  adapters do not require redesigning the shared orchestration. PCSX2 is
  currently the only adapter built on this boundary.
- Stable JSON output for `status`, `stats`, and `info`, documented and
  guarded by tests in [`docs/json-api.md`](docs/json-api.md).
- Deterministic plan generation (mount plans and Library View plans produce
  the same result for the same inputs) and regression tests pinned to real
  historical plan-ID values.
- A reproducible Rust toolchain: `rust-toolchain.toml` pins an exact Rust
  version, and CI and release workflows install that exact version instead
  of a floating `stable` channel.
- Read-only RetroArch environment discovery (`retroarch-environment`):
  detects a native and a Flatpak (user- and system-scope) RetroArch
  profile, locates and parses `retroarch.cfg` for a fixed set of
  configured paths, and inventories installed cores and their `.info`
  metadata. This is a sibling to the patch-preview adapter boundary above,
  not an implementation of `EmulatorAdapter`; see
  [`docs/RETROARCH_ENVIRONMENT.md`](docs/RETROARCH_ENVIRONMENT.md).
- A read-only RetroArch cheat/patch destination preview
  (`retroarch-patch-preview`): the second concrete preview built on
  `patch_manager`, after PCSX2. For every present catalogue archive, it
  previews per-game `.cht` cheat destinations (when exactly one installed
  core supports the archive's own file extension) and IPS/BPS/UPS/Xdelta
  soft-patch sibling destinations, across every discovered RetroArch
  profile - reusing the environment discovery above rather than
  rediscovering any path, and making no network call at all. It
  deliberately does *not* implement `EmulatorAdapter` or produce an
  `AdvisoryPatchPlan` - RetroArch's multi-root, core-selection-ambiguous
  shape does not fit PCSX2's trait/type, so it is a separate, narrowly-
  scoped `RetroArchAdvisoryPlan` instead; see
  [`docs/RETROARCH_PATCH_PREVIEW.md`](docs/RETROARCH_PATCH_PREVIEW.md).
- Read-only RetroArch playlist identity and content matching: discovers
  and parses modern JSON `.lpl` playlist files from the already-discovered
  Playlists directory and uses them as additional catalogue-matching and
  core-association evidence in `retroarch-patch-preview` - strengthening
  or resolving what extension-only matching alone leaves ambiguous,
  without ever downgrading an already-correct result. No playlist is ever
  written or modified. Purely additive to both `retroarch-environment
  --json` and `retroarch-patch-preview --json`; see
  [`docs/RETROARCH_PLAYLISTS.md`](docs/RETROARCH_PLAYLISTS.md).
- Read-only RetroArch AppImage detection: scans a fixed set of default
  locations and XDG desktop-entry directories for RetroArch AppImages -
  many users' primary way of running RetroArch on Linux - and feeds any
  found AppImage into the existing environment/playlist/patch-preview
  pipeline without ever executing, mounting, or extracting it, and
  without creating a duplicate profile when it shares the existing
  RetroArch configuration. `retroarch-environment --json`'s
  `format_version` moved from `1` to `2` for this, since a
  distinct-configuration AppImage inserts a 4th profile into a previously
  fixed-length, positionally-relied-upon array; see
  [`docs/RETROARCH_APPIMAGE.md`](docs/RETROARCH_APPIMAGE.md).
- Read-only RetroArch cheat/patch artifact inventory: the existing
  `retroarch-patch-preview` now enumerates bounded `.cht` files beneath
  configured cheat roots and `.ips`/`.bps`/`.ups`/`.xdelta` files in
  catalogue-relevant content directories, then reports exact, strong,
  weak, ambiguous, duplicate, conflicting, and orphaned associations. It
  parses only bounded `.cht` metadata and never mutates or follows an
  artifact symlink; see
  [`docs/RETROARCH_ARTIFACT_INVENTORY.md`](docs/RETROARCH_ARTIFACT_INVENTORY.md).
- Read-only external cheat catalogue discovery and matching
  (`retroarch-cheat-catalogue <local-path>`): matches a local `.cht`
  directory tree or bounded JSON manifest against catalogued games using
  conservative serial/content-hash/playlist-identity/title-platform-region
  evidence tiers, cross-references `retroarch-patch-preview`'s existing
  artifact inventory for installed-state, and never downloads, installs,
  enables, or applies a cheat. No network access; local sources only. See
  [`docs/RETROARCH_CHEAT_CATALOGUE.md`](docs/RETROARCH_CHEAT_CATALOGUE.md).

- Safe, journal-driven rollback for RetroArch cheat installations is now
  available via `retroarch-cheat-rollback`; GUI support remains out of scope.
- Read-only RetroArch cheat installation history and journal inspection are
  available via `retroarch-cheat-history` and `retroarch-cheat-inspect`, with
  destination/backup hash assessment, strongly bound rollback-journal
  discovery, stable JSON, and fail-closed path/symlink handling; see
  [`docs/RETROARCH_CHEAT_HISTORY.md`](docs/RETROARCH_CHEAT_HISTORY.md).
- Guided RetroArch cheat setup is available via
  `retroarch-cheat-setup <catalogue-path>`. It reuses environment discovery,
  exact profile IDs, read-only matching, destination safety, and the existing
  installer/history/journal/rollback systems; it adds no downloader or GUI.
  See [`docs/RETROARCH_CHEAT_SETUP.md`](docs/RETROARCH_CHEAT_SETUP.md).
- Trusted RetroArch catalogue retrieval is available through a reviewed
  registry, bounded HTTPS, redirect/SSRF policy, safe ZIP extraction, strict
  existing-parser validation, immutable content-addressed snapshots,
  provenance inspection, offline reuse, and guided `--source` setup. See
  [`docs/RETROARCH_CHEAT_SOURCES.md`](docs/RETROARCH_CHEAT_SOURCES.md).
- Immutable RetroArch cheat snapshots have deterministic inventory, explicit
  integrity verification, external atomic pins, conservative prune planning,
  confirmed per-candidate deletion revalidation, and deliberate abandoned
  staging cleanup. There is no automatic pruning. See
  [`docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md`](docs/RETROARCH_CHEAT_CACHE_MAINTENANCE.md).
- A read-only PCSX2 profile and PNACH inspection adapter in the Cheats &
  Mods GUI workspace: discovers native/Flatpak/portable PCSX2 profiles and
  inspects existing `cheats`/`cheats_ws`/`patches` directories, gated to
  PS2 archives only. Exact CRC matching remains deferred pending a
  verified PS2 executable CRC. See "PCSX2 read-only adapter" in
  [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md).

## Current development

Work in progress or the immediate next slice of work on top of the
foundations above:

- A read-only Dolphin profile and Game INI inspection adapter in the
  Cheats & Mods GUI workspace has been implemented and validated
  (GameCube/Wii archives only; no texture-pack or graphics-mod inspection;
  exact matching deferred pending a verified Dolphin Game ID) but is not
  yet merged into the release branch. It ships as its own module
  (`patch_manager::dolphin_local`), not a change to the `EmulatorAdapter`
  trait or the shared orchestration layer below. See "Dolphin read-only
  adapter" in
  [`docs/RELEASE_NOTES_v0.5.0-alpha.md`](docs/RELEASE_NOTES_v0.5.0-alpha.md).
- The shared `patch_manager` orchestration layer (platform filtering,
  ambiguity heuristics, plan assembly in `patch_manager::mod`) remains
  specific to the original PCSX2 patch-preview foundation and
  adapter-parameterized only through `EmulatorAdapter`'s narrow read-only
  surface; see [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md).
  RetroArch's preview and Dolphin's new adapter both deliberately did not
  generalize or extend this layer - each shipped as its own independent
  module instead, since neither shape fit `EmulatorAdapter`'s
  PCSX2-patch-preview-specific design. Whether the newer PCSX2 Cheats &
  Mods adapter itself extends `EmulatorAdapter` or is likewise
  independent has not been separately confirmed here.
- Ongoing library inspection and catalogue-health improvements building on
  the archive inspector and `CatalogueHealthReport`.
- Documentation maintenance to keep this roadmap, the architecture docs, and
  the changelog aligned with the code as these areas change.

## Next milestones

Realistic, concrete next steps, not yet started:

- Release process discipline: a documented, repeatable release checklist
  tied to the pinned toolchain (see [`docs/release-checklist.md`](docs/release-checklist.md)).

## Medium-term plans

Directions consistent with the architecture already in place, not yet
scheduled:

- **Emulator adapter expansion pauses after Dolphin.** With RetroArch,
  PCSX2, and Dolphin, Cheats & Mods reaches its intended three-adapter
  shape for now. Further read-only emulator adapters - RPCS3, PPSSPP,
  DuckStation, Cemu, Xenia, and similar candidates - are not scheduled and
  should not be implied as forthcoming; each would still follow the same
  read-only-inspection-first pattern if and when real user demand and an
  architecture review justify revisiting this pause.
- Library health reporting beyond today's `CatalogueHealthReport`: damaged
  or unreadable archives, likely duplicates, missing BIOS/firmware
  detection, and region/revision information, where that information can be
  derived from local data.
- Deeper preservation-metadata integration built on the identity fields
  `ArchiveIdentity` already reserves (`content_hash`, `archive_hash`,
  `internal_listing_hash`).
- Launch-preparation workflows once an adapter can safely describe what a
  launch would require, without ArchiveFS becoming a launcher itself.
- Additional JSON output for more commands, and automation/scripting/CLI
  integration built on the existing JSON stability guarantees in
  [`docs/json-api.md`](docs/json-api.md).

## Longer-term research

These are research directions only. None of them are promised, scheduled,
or currently implemented in any form:

- Integration with community cataloguing/verification sources such as
  Redump, No-Intro, TOSEC, or MAME's DAT data, and metadata sources such as
  ScreenScraper - purely as optional, opt-in, offline-safe enrichment,
  consistent with the provider-pipeline principles in
  [`docs/provider-pipeline.md`](docs/provider-pipeline.md).
- Patch-metadata and artwork sources beyond the single PCSX2 endpoint used
  today.
- Update/DLC awareness for preservation collections.
- Preservation-format guidance (for example CHD, RVZ, WUA, CSO, alongside
  the ZIP/7z/RAR archives already supported) - guidance and detection only,
  not conversion tooling, unless a future design explicitly proposes that.
- Interop with existing frontends such as ES-DE, RetroDECK, or LaunchBox,
  where that is practical without ArchiveFS taking over their role.
- Remote-play workflow documentation (for example Sunshine/Moonlight) for
  users who already use ArchiveFS-managed libraries with those tools.

## Explicitly out of scope for now

ArchiveFS is not, and these are not planned:

- A ROM, BIOS, firmware, or game download service.
- A storefront or marketplace for any kind of software or media.
- A DRM system, license-enforcement layer, or content-restriction system.
- A mandatory cloud account or cloud-dependent service - ArchiveFS is
  local-first and must keep working offline.
- A telemetry or usage-analytics platform.
- A surveillance system, or any component that reports on user files to a
  third party.
- An arbiter of which files a user is "allowed" to keep or use.
- A universal emulator, launcher, or frontend replacement. Emulator
  adapters describe and preview; they do not launch games or take over
  frontend responsibilities.
