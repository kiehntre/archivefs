# ArchiveFS v0.6.0-alpha - Release Notes

**Release classification: alpha.** This is pre-1.0, actively developed
software. Nothing in this document should be read as a promise of
stability, and the interfaces it describes may still change before a 1.0
release.

**This release has not shipped yet.** `Cargo.toml` still reads
`0.5.0-alpha` and no `v0.6.0-alpha` Git tag exists. This document describes
the current state of `origin/main` in preparation for that release, and
will be finalized once the remaining items in
[`docs/V0.6_RELEASE_AUDIT.md`](V0.6_RELEASE_AUDIT.md)'s go/no-go checklist
are cleared.

## Headline features

1. **Shared verified game identity.** ArchiveFS can now read narrowly
   defined, local disc metadata for a selected PS2, GameCube, or Wii
   archive - a typed evidence report (`Verified`/`Candidate`/`Missing`/
   etc.), never a guessed name. See
   [`docs/SHARED_GAME_IDENTITY.md`](SHARED_GAME_IDENTITY.md).
2. **Shared read-only Cheats & Mods preview and conflict detection**
   across all three adapters, with no apply path of its own. See
   [`docs/SHARED_CHEAT_PREVIEW.md`](SHARED_CHEAT_PREVIEW.md).
3. **A shared safe apply, backup, journal, history, and rollback
   foundation**: atomic writes, verified never-overwritten backups,
   schema-versioned journals, truthful partial-success reporting, and
   rollback that blocks on user-modified content. See
   [`docs/SHARED_SAFE_APPLY_ROLLBACK.md`](SHARED_SAFE_APPLY_ROLLBACK.md).
4. **RetroArch GUI apply, history, and rollback.** An eligible exact or
   approved-strong RetroArch trusted-catalogue match can now be applied
   directly from Cheats & Mods, with explicit confirmation, a separate
   non-preselected replacement approval, background execution, and a
   result shown as success, partial success, or failure. History & Logs
   can open the exact operation and preview/confirm its rollback. See
   [`docs/RETROARCH_GUI_APPLY_HISTORY.md`](RETROARCH_GUI_APPLY_HISTORY.md).
5. **RetroArch trusted-catalogue download and management.** The Sources
   page now owns catalogue retrieval end-to-end: Download, Update, and a
   read-only Verify, each gated behind an explicit review-then-confirm
   dialog before any network access. See
   [`docs/RETROARCH_CHEAT_SOURCES.md`](RETROARCH_CHEAT_SOURCES.md).
6. **Recently Found.** A new navigation page listing only the newest
   completed scan's added archives, in exact path order, persisted across
   restarts.
7. **Mega Drive/Genesis loose-ROM recognition**, gated on an exactly-named
   folder component so an unrelated `README.md` is never mistaken for a
   ROM. See [`docs/LIBRARY_SCAN_USABILITY.md`](LIBRARY_SCAN_USABILITY.md).

## Safety model

- **Explicit confirmation, always.** Nothing installs, applies, or
  modifies anything without it. Replacing existing different content
  requires a *separate*, non-preselected approval beyond the general
  apply confirmation.
- **ArchiveFS does not execute cheat files** - or any other retrieved or
  inspected content - at any stage of preview, apply, or rollback, for any
  adapter.
- **Catalogue download is a separate step from cheat installation.**
  Fetching or updating the RetroArch trusted catalogue never installs
  anything; installation is its own explicit, confirmed action.
- **Atomic writes and verified backups.** Every write goes through an
  exclusive-create-temp-file-then-rename sequence with post-write
  verification; a replacement always creates a verified, never-overwritten
  backup first.
- **Truthful partial success.** If a destination write succeeds but the
  journal write that records it fails, the result is reported as partial
  success - never silent success, never an opaque hard failure.
- **Stale-plan rejection.** A confirmed apply is bound to a SHA-256 plan
  ID covering the exact adapter, archive, identity, profile, and
  destination; any change between preview and confirmation fails closed.
- **Rollback safety.** Rollback re-derives a fresh preview immediately
  before acting and blocks on user-modified destination content, a
  missing or changed backup, or an already-completed rollback.
- **Locking.** Every shared-apply transaction holds an exclusive advisory
  lock on its one destination root (five-second timeout); one transaction
  always has exactly one root, so lock ordering is deadlock-free.
- **No network during Apply or Rollback.** The only network access
  anywhere in this workflow is the explicit, user-confirmed RetroArch
  catalogue Download/Update step.
- **No automatic conflict resolution.** Duplicate destinations, filename
  collisions, and ambiguous matches are all surfaced as typed conflicts,
  never silently resolved.
- **Trust is about the provider, not every individual entry.** RetroArch's
  built-in catalogue provider is reviewed (ownership, format, host, and
  retrieval limits) - that is not a claim that every individual cheat
  entry inside it is correct for every game/region/revision. See
  [`docs/ADAPTER_SUPPORT_MATRIX.md`](ADAPTER_SUPPORT_MATRIX.md).

## Supported adapters

See [`docs/ADAPTER_SUPPORT_MATRIX.md`](ADAPTER_SUPPORT_MATRIX.md) for the
full matrix. In brief:

- **RetroArch**: verified identity, trusted provider, preview, **apply,
  backup, and rollback all available**.
- **PCSX2**: verified identity, local inspection, and preview available;
  **no trusted provider yet, no apply**.
- **Dolphin**: verified identity, local inspection, and preview available;
  **no trusted provider yet, no apply**.
- **Mods**: not implemented in any form.

## Known limitations

- PCSX2 and Dolphin remain preview-only - no Install/Apply/Enable/
  Disable/Rollback control exists for either in the GUI.
- Mods are planned and not implemented; the Mods section of Cheats & Mods
  is a labelled placeholder.
- **An open issue, not yet fixed:** some individual malformed entries in a
  downloaded RetroArch trusted catalogue can affect validation of the
  whole snapshot rather than being cleanly isolated and reported as
  non-actionable. A fix is in progress on a parallel branch and is **not**
  part of `origin/main` as of this document.
- **An open issue, not yet fixed:** the Cheats & Mods GUI still shows
  stale "Archive matching and cheat installation are not yet implemented"
  copy in one place, left over from before RetroArch apply shipped. A fix
  is in progress and is **not** part of `origin/main` as of this document.
- Libretro catalogue archive size-limit handling and "usable with
  warnings" catalogue behavior (a snapshot that parses with retained
  warnings but is otherwise safe to match against) are both being actively
  worked on and are not yet merged.
- There is no cancellation once a shared-apply write has actually begun.
- Cheats & Mods does not yet use the shared scrollable-page keyboard
  wrapper (Page Up/Down/Home/End) that Settings, Doctor, About, Sources,
  Library Views, and History & Logs all share.
- Operation history in History & Logs remains in-memory for the current
  session; it is not yet persisted to disk.
- No general-purpose local or community cheat/mod import inspection
  pipeline exists; only the fixed, reviewed RetroArch trusted-source list
  can be fetched.

## Upgrade notes

- No config file format changes.
- No database schema migration action is required; the library database's
  schema version advanced to support new scan-summary counters, applied
  automatically on next open.
- All new on-disk state (identity cache, preview cache, apply journals,
  backups, catalogue snapshots) lives in new, additive paths under
  ArchiveFS's existing managed directories - nothing about an existing
  `v0.5.0-alpha` install needs to change before upgrading.
- If you have an existing RetroArch cheat-source cache from `v0.5.0-alpha`,
  it remains usable; the new Sources-page Download/Update workflow reuses
  the same cache root.

## Testing status

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 1,401+ tests passing as of this document (CLI
  127, core 868+, GUI 406+; exact totals will be re-confirmed at tag time).
- Full line-by-line verification of the shared apply/rollback/preview/
  identity safety properties against their documentation was performed as
  part of the release audit - see `docs/V0.6_RELEASE_AUDIT.md` Section on
  core safety review.

## Manual QA still required

A real manual QA pass on Nobara against
[`docs/MANUAL_QA_v0.6.0-alpha.md`](MANUAL_QA_v0.6.0-alpha.md) has **not**
been recorded yet. This is a release blocker - see
[`docs/V0.6_RELEASE_AUDIT.md`](V0.6_RELEASE_AUDIT.md) for the full go/no-go
checklist.

## See also

- [`docs/V0.6_RELEASE_AUDIT.md`](V0.6_RELEASE_AUDIT.md) - current release
  readiness and remaining blockers.
- [`docs/ADAPTER_SUPPORT_MATRIX.md`](ADAPTER_SUPPORT_MATRIX.md) - the
  per-adapter capability matrix.
- [`docs/MANUAL_QA_v0.6.0-alpha.md`](MANUAL_QA_v0.6.0-alpha.md) - the
  manual acceptance plan.
- [`CHANGELOG.md`](../CHANGELOG.md) - the itemized change log.
