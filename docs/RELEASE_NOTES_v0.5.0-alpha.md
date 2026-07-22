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
   model (**Trusted** / **Unverified** / **Blocked**) for cheat content
   today - the same model is intended to extend to mod content once any
   mod adapter exists, but no mod adapter is implemented yet - while being
   explicit that matching and installation are not implemented in that
   workspace yet either.
4. Brings Cheats & Mods to its intended **three-adapter architecture**:
   RetroArch and PCSX2 (merged) plus Dolphin (implemented and validated,
   pending merge). Further emulator adapter expansion is paused after
   Dolphin for now - see [`ROADMAP.md`](../ROADMAP.md#medium-term-plans).

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

A new **Cheats & Mods** page keeps profile discovery, trusted-catalogue
retrieval, and the trust/safety model together for one selected archive,
across three read-only emulator adapters: **RetroArch** (cheat catalogue
retrieval and profile discovery), **PCSX2** (PNACH inspection, PS2-only),
and **Dolphin** (Game INI inspection, GameCube/Wii-only, implemented and
validated but pending merge - see below). Only one adapter applies to a
given archive's platform at a time, and each is explicit about what it does
and does not do:

- Available today (RetroArch and PCSX2): choosing an archive, discovering
  eligible emulator profiles, and - for RetroArch - fetching or reusing a
  trusted, reviewed cheat catalogue snapshot; for PCSX2, inspecting
  existing on-disk `.pnach` files.
- Not available yet, and labelled as such rather than hidden, for any
  adapter: matching the archive against a catalogue with a verified
  identity, installing anything, and any mod adapter.

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

### PCSX2 read-only adapter

A read-only PCSX2 adapter for Cheats & Mods is merged into this release. It
is a **read-only inspection foundation**, not a complete cheat manager - it
does not install cheats, apply mods, or change any PCSX2 file:

- Discovers native, Flatpak-user, and Flatpak-system PCSX2 profiles, plus
  an explicitly supplied portable/AppImage configuration root (never
  auto-searched). A profile is eligible only with an absolute, non-root,
  symlink-free path, a readable directory, and PCSX2-specific
  configuration evidence; ineligible profiles remain visible with a typed
  blocker reason.
- Inspects the `cheats`, `cheats_ws` (widescreen patches), and `patches`
  directories where present, reporting a missing directory normally
  rather than creating one.
- Parses existing `.pnach` files read-only (path, filename-derived CRC/
  serial candidates, title/region/comment fields, enabled/disabled/
  unknown syntax counts, category, size, SHA-256, duplicate detection,
  and malformed-syntax warnings), opening files with `O_NOFOLLOW` and
  skipping symlinks and special files.
- Supports exact CRC matching **only** when given a separately verified
  PCSX2 executable CRC. ArchiveFS's current archive records do not
  contain one, so the GUI never claims an exact match and never guesses a
  CRC from a filename; it reports exact, ambiguous, unavailable, or
  no-match states truthfully instead.
- In the GUI, PCSX2 appears only for PS2 archives and defaults a PS2
  context to the PCSX2 adapter without changing queue, mount, selection,
  or platform state; RetroArch remains independently selectable. A single
  eligible profile may be auto-selected; multiple eligible profiles
  require an explicit choice. No Install, Apply, Enable, Disable, Delete,
  Replace, Fix, or rollback control exists.
- No PCSX2 file is written, copied, renamed, deleted, generated, or
  sanitized; nothing is uploaded; there is no telemetry, no network
  retrieval path in this adapter, and PCSX2/imported content is never
  executed.

Validated with 15 focused core tests and 4 GUI tests, reported alongside a
full `cargo fmt`/`clippy -D warnings`/`cargo test --workspace` (1,348
tests: CLI 127, core 822, GUI 399) pass and a successful release build.

Deferred: verified PS2 executable-CRC extraction (no bounded ISO/CHD/CSO
identity reader yet), any preview/installation/conflict-resolution/
backup/journal/rollback/enable/disable workflow, and automatic discovery
of AppImage/portable configuration roots (an exact root must be supplied
by a trusted caller).

### Dolphin read-only adapter (implemented, pending merge into this branch)

A read-only Dolphin adapter for Cheats & Mods has been implemented and
validated on the separate `codex-dolphin-readonly-adapter` branch. **It has
not been merged into this branch and is not part of any build produced
from `sonnet-v0.5-release-prep` today.** It is documented here so the
release notes are accurate once the merge happens; nothing below should be
read as available in the current branch.

Once merged, it is a **read-only inspection foundation**, not a complete
cheat manager - it does not install cheats, apply mods, inspect texture
packs, or change any Dolphin file:

- Discovers native (`$XDG_CONFIG_HOME/dolphin-emu`, falling back to
  `~/.config/dolphin-emu`) and Flatpak
  (`~/.var/app/org.DolphinEmu.dolphin-emu/config/dolphin-emu`) Dolphin
  profiles, plus an exact configuration root supplied by another trusted
  caller (never searched arbitrarily, never inferred from a filename). A
  profile is eligible only with an absolute, non-root path resolving
  through existing real directories with no symlinked components, that
  exists as a directory and contains a regular, non-symlink `Dolphin.ini`
  at its root, with identity checked for staying unchanged between
  discovery and inspection where device/inode identity is available.
  Missing standard native/Flatpak paths are ignored; missing explicit
  roots remain visible as blocked; a missing `GameSettings` directory is
  normal and does not make a profile ineligible; missing paths are never
  created.
- Inspects only regular, lowercase `*.ini` entries immediately inside a
  game's `GameSettings` directory - non-recursive, with symlinks,
  directories, and special files never opened. **No texture pack,
  graphics mod, resource pack, Riivolution asset, save, NAND, SD-card, or
  executable directory is inspected**; texture-pack support
  (`Load/Textures`, `Load/GraphicMods`, `ResourcePacks`, `Dump/Textures`)
  is explicitly not implemented.
- The bounded structural parser recognizes `[OnFrame]`/`[OnFrame_Enabled]`,
  `[ActionReplay]`/`[ActionReplay_Enabled]`, `[Gecko]`/`[Gecko_Enabled]`,
  and `[Riivolution]`/`[Riivolution_Enabled]` sections, recording named
  definitions and enabled-name references as inert text - codes are never
  evaluated or executed. Each retained file also records a
  filename-derived Game ID candidate, an optional revision candidate, a
  region candidate conservatively derived from the Game ID, file size,
  SHA-256, duplicate identity/filename/content observations, and
  malformed-syntax/encoding/resource-limit warnings.
- Supports exact matching **only** when given a separately verified
  Dolphin Game ID (three to six ASCII letters/digits), optionally with a
  verified `u16` revision. ArchiveFS's GUI has no reviewed GameCube/Wii
  disc-header reader and therefore supplies no verified Game ID; INI
  filename identities remain observations only and never prove that an
  INI belongs to the selected archive. The adapter distinguishes exact
  Game ID match, exact Game ID and revision match, multiple matching
  INIs, revision mismatch, no matching INI, invalid verified Game ID, no
  verified Game ID, and deferred identity extraction - it never
  fabricates a match.
- In the GUI, Dolphin appears only for GameCube/Wii archives (canonical
  platforms GameCube, Nintendo GameCube, Wii, Nintendo Wii) and defaults
  such a context to itself without changing queue, mount, selection, or
  platform state. A single eligible profile may be auto-selected; multiple
  eligible profiles require an explicit choice. No Install, Apply, Enable,
  Disable, Delete, Replace, Fix, or rollback control exists.
- No Dolphin file is written, copied, renamed, deleted, generated, or
  sanitized; missing directories are never created; nothing is uploaded;
  there is no telemetry, and a full audit of every filesystem-mutation
  call in the adapter's production source confined all of them to
  `#[cfg(test)]` fixtures. Dolphin and any inspected content are never
  executed; production code exposes no network, process-execution,
  shell, or socket path.

Bounded by fixed limits: 16 profiles, 10,000 `GameSettings` entries
visited, 2,048 Game INI files, 256 KiB per file, 16 MiB total input, 8,192
lines per file, 8 KiB per line, 128 retained names per supported section
kind, and 100 file cards / 50 warning lines rendered in the GUI; limit
exhaustion marks the result incomplete.

Validated with 6 focused core tests and 2 GUI tests on that branch
(native/Flatpak/exact-root discovery, supported-section parsing without
file modification, verified identity matching, symlinked profile/INI
refusal, missing/unsafe explicit-root blocking, invalid identity and
resource-limit reporting, GameCube/Wii-only visibility, and read-only
rendering with no install/apply/enable/delete control), reported
alongside a full `cargo fmt -- --check`/`clippy -D warnings`/
`cargo test --workspace --all-targets` (1,356 tests: CLI 127, core 828,
GUI 401) pass and `git diff --check` pass. Manual read-only checks
against a real native and Flatpak-style Dolphin configuration (with real
`[ActionReplay]`/`[Gecko]` INI sections) were performed on Ubuntu
24.04.4 LTS; **a Nobara-specific manual run remains outstanding** because
that was not the available environment. That manual check also corrected
an initial marker assumption: Dolphin uses a root-level `Dolphin.ini`, not
`Config/Dolphin.ini`.

Deferred even once merged: verified archive Game ID/revision extraction,
recursive `GameSettings` traversal (lowercase `.ini` extension only),
texture-pack or graphics-mod inventory, inspection of referenced
Riivolution assets, code semantic validation or any compatibility claim,
emulator launch or version detection, and any download, preview,
installation, enabling, disabling, replacement, deletion, backup,
journal, or rollback workflow. Structural inspection is not antivirus
scanning.

With Dolphin, Cheats & Mods reaches its intended three-adapter shape
(RetroArch, PCSX2, Dolphin). **Further emulator adapter expansion is
paused after Dolphin for now** - see
[`ROADMAP.md`](../ROADMAP.md#medium-term-plans).

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
| PCSX2 read-only profile/PNACH inspection | **Available** (GUI, merged) |
| PCSX2 exact CRC matching | Deferred - requires a verified PS2 executable CRC, which ArchiveFS does not yet have |
| PCSX2 installation, rollback, mutation of any kind | Not implemented anywhere |
| Dolphin read-only profile/Game INI inspection | Implemented and validated on branch `codex-dolphin-readonly-adapter`; **not yet merged into this branch** |
| Dolphin exact Game ID matching | Deferred - requires a verified GameCube/Wii Game ID, which ArchiveFS does not yet have |
| Dolphin texture-pack/graphics-mod inspection | Not implemented anywhere |
| Dolphin installation, rollback, mutation of any kind | Not implemented anywhere |
| Further emulator adapters beyond RetroArch/PCSX2/Dolphin | Paused for now - see [`ROADMAP.md`](../ROADMAP.md#medium-term-plans) |

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
- PCSX2's exact CRC matching remains deferred pending a verified PS2
  executable CRC; there is no PCSX2 preview, installation, or rollback
  workflow (see "PCSX2 read-only adapter" above).
- A read-only Dolphin adapter has been implemented and validated on a
  separate branch (see "Dolphin read-only adapter" above) but has not
  been merged into this branch; it is not part of this release until
  that merge happens. Once merged, it remains a read-only inspection
  foundation - no texture-pack inspection, no verified GameCube/Wii
  identity extraction, and no preview, install, enable, disable, backup,
  journal, or rollback workflow.
- Further emulator adapter expansion is paused after Dolphin for now -
  see [`ROADMAP.md`](../ROADMAP.md#medium-term-plans).
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
