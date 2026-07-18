# Roadmap

The current roadmap - completed foundations, active work, next milestones,
medium-term plans, longer-term research, and explicit non-goals - lives at
[`/ROADMAP.md`](../ROADMAP.md) in the repository root. This file used to
duplicate that content with an older, version-numbered wishlist; that
duplication has been removed so there is a single source of truth for
planning.

What remains here are the durable design principles behind how ArchiveFS is
built, which do not change as often as the roadmap itself.

## Design Principles

- Linux-first.
- Never modify user archives.
- Read-only by default.
- Composition over duplication.
- Test before merge.
- Architecture before features.
- Extension points (providers, emulator adapters) instead of hardcoding.
- Shared core logic before command-specific logic.
- Explicit user actions for mount, unmount, and cleanup - no silent
  automation of anything that changes filesystem state.
- Prefer safe, visible stale-data warnings over destructive automatic
  repair.
- Keep provider and adapter failures isolated and understandable.
- Keep CLI output human-readable, and keep documented JSON output
  script-friendly and stable.
