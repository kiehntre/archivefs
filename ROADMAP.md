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

## Current development

Work in progress or the immediate next slice of work on top of the
foundations above:

- Continuing to isolate PCSX2-specific assumptions that still live outside
  `patch_manager::pcsx2` (the shared `patch_manager` orchestration layer -
  platform filtering, ambiguity heuristics, plan assembly - is intentionally
  not yet fully adapter-parameterized; see
  [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md)).
- Refining the `EmulatorAdapter` contract itself as real second-adapter work
  begins, rather than finalizing it speculatively against one adapter.
- Ongoing library inspection and catalogue-health improvements building on
  the archive inspector and `CatalogueHealthReport`.
- Documentation maintenance to keep this roadmap, the architecture docs, and
  the changelog aligned with the code as these areas change.

## Next milestones

Realistic, concrete next steps, not yet started:

- RetroArch as the second `EmulatorAdapter` implementation: installation
  discovery (native and Flatpak), config file discovery, and core
  inventory, following the same read-only-preview-first approach used for
  PCSX2.
- A read-only environment preview and capability report for an adapter (what
  was found, at what confidence, without assuming write access exists).
- Release process discipline: a documented, repeatable release checklist
  tied to the pinned toolchain (see [`docs/release-checklist.md`](docs/release-checklist.md)).

## Medium-term plans

Directions consistent with the architecture already in place, not yet
scheduled:

- Additional emulator adapters beyond PCSX2 and RetroArch - candidates
  include Dolphin, RPCS3, PPSSPP, DuckStation, Cemu, and Xenia - each
  following the same adapter boundary and read-only-preview-first pattern.
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
