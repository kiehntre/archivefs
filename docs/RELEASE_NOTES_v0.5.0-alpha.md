# ArchiveFS v0.5.0-alpha - Release Notes

**Release classification: alpha.** This is pre-1.0, actively developed
software. Workflows are functional and tested, but incomplete areas exist and
are called out explicitly below rather than left implicit. Nothing in this
document should be read as a promise of stability, and the interfaces it
describes may still change before a 1.0 release.

This document describes the state of the `sonnet-v0.5-release-prep` branch at
the time it was written. The workspace version in `Cargo.toml` has not been
bumped to `0.5.0-alpha` and no Git tag has been created - both remain separate
steps in [`docs/release-checklist.md`](release-checklist.md), performed after
this documentation is reviewed.

## Overview

v0.5.0-alpha is a substantial hardening and workflow release. It does three
things:

1. Hardens core data-safety guarantees around mounting, source scanning, the
   catalogue, and the RetroArch cheat-source cache.
2. Redesigns the desktop GUI's navigation and page set, replacing several
   ad hoc panels with dedicated pages and a shared visual system, following
   an internal adversarial audit that rejected the first pass as
   functionally sound but not visually release-ready.
3. Introduces a first-class **Cheats & Mods** workspace and a public trust
   model (**Trusted** / **Unverified** / **Blocked**) for cheat and mod
   content, while being explicit that matching, installation, and mod
   support are not implemented in that workspace yet.

## Major user-visible changes

### Redesigned desktop GUI navigation

The GUI's primary navigation moved from a single top tab bar to a persistent
page list with these destinations: **Mount**, **Selected**, **Active
Mounts**, **Library**, **Sources**, **Doctor**, **History & Logs**,
**Settings**, **About**, and **Cheats & Mods**. The previously existing
**Health**, **Duplicates**, and **Library Views** pages remain reachable as a
secondary group - nothing was removed.

- **Mount** previews planned destinations and per-archive readiness
  (ready / already mounted / destination collision) before anything is
  queued or mounted.
- **Selected** reviews the mount queue - archive, planned destination,
  planned action - with per-item removal, before the same confirmed batch
  mount path already used elsewhere in the app.
- **Active Mounts** lists currently mounted archives with confirmed normal
  unmount; lazy unmount and remount remain conditional recovery actions
  reached through the existing Library flow, deliberately not duplicated
  here.
- **Doctor** adds a one-line check summary (passed/warned/failed counts)
  and a "Copy report" action alongside the existing structured checks.
- **History & Logs** adds operation and result filtering, newest/oldest
  sorting, and log export, over the same in-session operation history the
  app already recorded.
- **Settings** and **About** surface backend-supported configuration,
  paths, and environment/version information read-only, including a
  "Copy system information" action.

### Shared visual system

The initial redesign was reviewed by an internal adversarial audit
(`docs/INTEGRATED_GUI_AUDIT.md`) that found the backend integration behind
these pages sound, but the presentation not release-ready: inconsistent
spacing and action hierarchy, an Activity panel that defaulted to expanded
against its own stated design, and no responsive content-width policy. A
rescue pass followed, adding a small shared `ui` module (typed status badges,
cards, buttons, empty/loading states) and a responsive page-width policy that
adapts across laptop, ~1536x864, and ultrawide window widths. The audit and
rescue record are kept in `docs/FABLE_PROGRESS.md` and
`docs/INTEGRATED_GUI_AUDIT.md` for anyone auditing this release further.

### Cheats & Mods workspace

A new **Cheats & Mods** page keeps three previously separate concerns
together for one selected archive: RetroArch profile discovery, trusted
cheat-catalogue retrieval, and the trust/safety model. It is explicit about
what it does and does not do:

- Available: choosing an archive, discovering eligible RetroArch profiles,
  and fetching or reusing a trusted, reviewed cheat catalogue snapshot.
- Not available yet, and labelled as such rather than hidden: matching the
  archive against the catalogue, installing a cheat, and any mod adapter.

See [`docs/CHEATS_MODS_USER_POLICY.md`](CHEATS_MODS_USER_POLICY.md) for the
user-facing policy and [`docs/CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md)
for the fuller trust/safety/privacy model behind it.

### RetroArch cheat setup, installation, rollback, and history (CLI)

Independent of the GUI workspace above, the CLI has a complete guided
workflow: `retroarch-cheat-setup` discovers safe profiles and previews
conservative matches, a safe installer backs up any file it would replace
and writes a journal, `retroarch-cheat-rollback` can undo a completed
install from that journal, and `retroarch-cheat-history` /
`retroarch-cheat-inspect` give read-only history and single-run inspection.
None of this is reachable from the GUI yet - see Known limitations.

### Trusted cheat-catalogue retrieval and cache maintenance

`retroarch-cheat-source-list` / `-fetch` / `-inspect` retrieve from a fixed,
reviewed list of sources only - there is no arbitrary or user-supplied URL
input anywhere in ArchiveFS. Retrieval is certificate-validated HTTPS into a
bounded, validated, immutable local snapshot with a SHA-256 digest and
freshness reporting, and a previously fetched snapshot can be reused
offline. Cache maintenance (inventory, verification, pin/unpin, preview-first
pruning) keeps current, last-known-good, pinned, and unverifiable snapshots
protected from deletion, coordinated by one bounded advisory file lock
across every process sharing that cache.

## Security and data-integrity improvements

- **Mount lifecycle postcondition checking.** Mount and unmount now verify
  their own outcome against `/proc/self/mountinfo` rather than trusting the
  external tool's exit status alone.
- **Source-root validation and bounded scanning.** Source folders must be
  explicit, absolute, non-root paths; duplicate/nested roots and symlink
  path components are rejected; scans are bounded by entry-count and depth
  limits.
- **Transactional catalogue refresh.** One refresh is one SQLite write
  transaction with a savepoint per source: a single failing source rolls
  back only its own writes and is recorded truthfully; a fatal failure
  rolls back the whole refresh; a killed process cannot leave a
  half-updated catalogue visible.
- **RetroArch cheat-source cache locking.** One exclusive,
  directory-identity-based advisory lock (not a PID file, not a
  UTF-8/lossy-string identity) with a deterministic five-second timeout
  coordinates every cache-touching operation across processes. Locking is
  additional defense on top of, not a replacement for, per-candidate
  revalidation at execution time.
- **Non-UTF-8 path handling.** A non-UTF-8 source root is rejected at the
  config boundary instead of silently altered; archive names below a valid
  source remain exact, byte-preserving values throughout scanning, the
  catalogue, and the RetroArch workflows.

## Cheats & Mods status (at a glance)

| Capability | Status |
| --- | --- |
| RetroArch profile discovery | Available (GUI and CLI) |
| Trusted cheat-catalogue retrieval, offline reuse | Available (GUI and CLI) |
| Cache inventory, verification, pin/unpin, pruning | Available at CLI only |
| Archive-to-cheat matching | CLI only (`retroarch-cheat-setup`); not in the GUI workspace |
| Cheat installation, backups, journal | CLI only; not in the GUI workspace |
| Rollback | CLI only; not in the GUI workspace |
| Local/community import inspection | Not implemented anywhere |
| Arbitrary remote sources | Not accepted anywhere |
| Mod installation, mod adapters | Not implemented anywhere |
| PCSX2 read-only adapter | Under separate, parallel development; not part of this release |

## Privacy and safety model

- ArchiveFS is local-first with no telemetry. Nothing about your files -
  filenames, hashes, metadata, scan results, or file contents - is uploaded
  to ArchiveFS's developers or any third party.
- The only outbound network activity described in this release is the
  trusted RetroArch catalogue retriever downloading the reviewed catalogue
  itself; it does not upload anything about your local content.
- ArchiveFS never automatically executes unknown code as part of
  inspection, preview, matching, installation, verification, rollback, or
  cleanup.
- Catalogue retrieval by itself never installs a cheat or modifies your
  RetroArch configuration.
- Trust (**Trusted** / **Unverified** / **Blocked**) and structural
  inspection are tracked separately: passing a structural check does not
  promote an unverified source to trusted, and unverified does not mean
  malicious.
- See [`docs/CHEATS_MODS_USER_POLICY.md`](CHEATS_MODS_USER_POLICY.md) for
  the full user-facing statement of this model.

## Upgrade notes

- No config file format changes are introduced in this release.
- No database schema migration is required for existing catalogues.
- If you have an existing RetroArch cheat-source cache from an earlier
  build, the new locking layer will create/require the cache root the same
  way retrieval already did; no manual migration step is needed.
- The GUI's navigation has changed shape (page list instead of a single tab
  bar). All previously available screens remain reachable.

## Known limitations

- Cheat matching, installation, and rollback are not reachable from the
  GUI's Cheats & Mods workspace, only from the CLI.
- Cache pin/unpin and prune have no GUI controls yet.
- There is no general-purpose local or community cheat/mod import
  inspection pipeline. Local safety scanning is shown in the GUI as
  planned/unavailable, with no toggle that would misrepresent protection.
- Mod installation and emulator-specific mod adapters do not exist.
- Operation history in History & Logs is in-memory for the current
  session only; it is not persisted across restarts.
- Settings is read-only for backend-supported configuration; there is no
  editable appearance/density setting and no update-check mechanism.
- A read-only PCSX2 adapter is being developed in parallel and is not part
  of this release; PCSX2 support here remains limited to the existing
  read-only patch-preview.
- This is alpha software. See [`CHANGELOG.md`](../CHANGELOG.md) for the
  full, itemized list of what changed.

## See also

- [`docs/MANUAL_QA_v0.5.0-alpha.md`](MANUAL_QA_v0.5.0-alpha.md) - the manual
  acceptance test plan for this release.
- [`docs/CHEATS_MODS_USER_POLICY.md`](CHEATS_MODS_USER_POLICY.md) - the
  user-facing Cheats & Mods policy.
- [`docs/CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md) - the fuller
  trust/safety/privacy design behind that policy.
- [`docs/INTEGRATED_GUI_AUDIT.md`](INTEGRATED_GUI_AUDIT.md) - the
  adversarial audit of the GUI redesign referenced above.
- [`CHANGELOG.md`](../CHANGELOG.md) - the itemized change log.
