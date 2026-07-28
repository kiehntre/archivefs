# Cheats & Mods: the beginner workflow (Dolphin, Xenia)

This describes the simplified default view added to the Cheats & Mods page
for Dolphin/GameCube and Xenia Canary/Xbox 360. It does not replace or
weaken anything described in [`docs/CHEATS_MODS_SAFETY.md`](CHEATS_MODS_SAFETY.md)
or [`docs/SHARED_CHEAT_PREVIEW.md`](SHARED_CHEAT_PREVIEW.md) - every safety
check, preview, backup, verification, and rollback step those documents
describe still runs exactly as before. This document only covers what
changed in how much of that machinery is shown by default.

RetroArch's and PCSX2's own Cheats & Mods pages are unchanged by this work
and are not covered here.

## What changed

Before this work, using Cheats & Mods for a Dolphin or Xenia game meant
working through a page that showed every internal step at once: provider
fetch buttons, emulator profile paths, cache state, identity internals,
exact destination paths, a numbered six-stage transaction pipeline
(Preview → Review → Confirm → Apply → Verify → Result), and raw generated
INI/TOML content - all visible by default, whether or not you needed any
of it to install one compatible cheat.

The default page for these two adapters is now:

1. Game title and platform, with a compact "Choose another game" action
2. A plain-English status line
3. A simplified list of compatible cheats/patches, with a checkbox per item
4. An "Install selected" button
5. Once installed: a plain success/failure statement and an "Undo
   installation" button

Everything else - profile discovery details, provider source/licence
attribution, the exact destination path, cache status, a manual Refresh
control, raw code/patch bodies, the full transaction plan (plan ID,
per-entry source/destination/hash), and rollback internals - is still
there, just moved behind a **Details** disclosure that is collapsed by
default. Opening Details never triggers a new network request or changes
any state; it only reveals what was already computed.

The page-wide archive paths, mount/source context, diagnostics, planned-mod
notice, and related activity are likewise collapsed under **Workflow
diagnostics** for these two beginner routes. They remain available without
pushing the primary controls below the initial small-laptop viewport.

## Automatic provider loading

Dolphin and Xenia profile discovery, and the exact-match provider fetch
(Dolphin's upstream Gecko codes / Xenia's `xenia-canary/game-patches`
patches), now start automatically once the game's identity is verified,
instead of requiring a manual "Fetch" click first:

- A valid cached result is used immediately.
- A missing or stale result triggers exactly one background fetch - the
  trigger is a one-shot gate (`NotLoaded` → `Loading` → `Ready`/`Failed`),
  so re-rendering the page, or switching between the main view and
  Details, never re-issues a request.
- A failed fetch is never retried automatically. The existing manual
  Fetch/Refresh control is still there under Details, and the default view
  offers one plain **Try again** action without exposing the transport error.
- Every existing bound - the 24-hour cache freshness window, the 30-second
  rate-limit floor, request timeouts, response-size bounds, the offline
  cached-fallback behaviour, and source attribution - is unchanged; only
  when the fetch is *triggered* changed, not how it behaves once running.

## Remembered emulator profile

Dolphin has no single native install path (it also supports portable/
AppImage installs with their own directory), and Xenia Canary has no
native path at all - ArchiveFS only ever learns about either through an
explicitly typed directory or a previous discovery. To avoid asking every
time, ArchiveFS now remembers the profile you last chose, in
`~/.config/archivefs/emulator_profiles.toml` (a small file separate from
`config.toml`, so remembering a profile can never interact with source
folder management).

Selection order, applied whenever profile discovery completes:

1. A remembered profile, if it's still valid.
2. A profile already explicitly chosen this session.
3. A single portable/explicit profile, if there's exactly one among
   several otherwise-equal valid profiles.
4. The only valid profile, if there's just one.
5. Otherwise, if more than one valid profile exists, a chooser appears:
   *"ArchiveFS found N \<emulator\> profiles. Choose the one you use:"*
   with one friendly radio choice per profile. Clicking the choice selects
   and remembers it in the same non-destructive action; exact paths remain
   available under Details.

If a remembered profile stops being valid (its directory disappeared, or
it no longer passes the same eligibility checks), ArchiveFS does not
silently fall back to guessing - it shows a plain "Emulator setup needed"
state and asks again, the same as if nothing had ever been remembered.

A profile already bound to an install that's been reviewed or applied
this session is never silently swapped out from under it, even if
discovery re-runs.

## Plain-English status and compatibility wording

The beginner status line is one of: *Finding compatible cheats*, *N
compatible enhancement(s) found*, *No compatible cheats found*, *Emulator
setup needed*, *Choose an emulator profile*, *Could not check for cheats*,
or *Using saved results while offline*.

Compatibility evidence is translated into four labels:

| Technical                                          | Beginner label            |
| --------------------------------------------------- | -------------------------- |
| Dolphin: not `uncertain_revision`, or Xenia `ExactCompatible` | Compatible          |
| Dolphin: `uncertain_revision`, or Xenia `PartiallyVerified`   | Probably compatible |
| Xenia `Incompatible`                                 | Not compatible (hidden)   |
| (reserved for future adapters)                       | More information needed   |

Incompatible candidates are never shown in the default list - Xenia's own
candidate-matching step never even builds a selection for one - but the
exact technical evidence for anything ArchiveFS considered is still
available under Details.

For a partially verified Xenia patch, the beginner view shows exactly one
warning - *"This patch matches the game, but ArchiveFS cannot confirm the
exact executable version."* - with one checkbox: *"I understand this patch
may target a different executable version."* This is the same
`XeniaPatchSelection::partial_verification_acknowledged` flag the
technical picker already used; acknowledging it in one view acknowledges
it everywhere, and it resets whenever a new candidate/selection is built
(a fresh document from the provider, or a different chosen candidate),
never carrying over from a previous game or patch.

## One-click install

Selecting one or more compatible items and clicking **Install selected**
builds the install preview and moves straight to a confirmation - the
existing "click Preview, then separately click Review" two-step is
skipped for the ordinary case. The confirmation shows the number of
items, the emulator, a plain backup/undo statement, the existing required
replacement-approval checkbox (only when a different existing file would
actually be replaced), and a **Show exact changes** toggle that reveals
the same plan ID, per-entry source/destination path, and source hash the
technical Details view already showed. None of the underlying transaction
engine changed: the same `SharedTransactionPlan`/apply/verify/journal
pipeline described in `docs/SHARED_CHEAT_PREVIEW.md` runs regardless of
which view triggered it.

After a successful install: *"Installed successfully"* and an **Undo
installation** button, wired to the same rollback path described in
`docs/RETROARCH_CHEAT_ROLLBACK.md` (shared across adapters). Undo remains
exact: it either restores the prior file exactly or removes a
newly-created one, verified the same way regardless of which view started
the install. The completed state replaces the pre-install checklist rather
than displaying stale compatibility/installed controls beside the result;
a successful rollback returns the workflow to its exact pre-install state.

## What did not change

- Every safety check, preview step, backup, verification pass, and
  rollback behaviour described elsewhere in the `docs/` tree.
- Region/revision/Title ID/Media ID matching, cache and offline fallback,
  candidate filtering, INI/TOML generation, atomic apply, directory
  cleanup, symlink/traversal protection, and archive-context handling.
- RetroArch's and PCSX2's own Cheats & Mods pages.
- Existing transaction journals and their format.

## Manual verification still required

This document describes the implementation and its automated test
coverage. It does not claim that Dolphin or Xenia were actually launched,
or that either emulator was observed reading an installed cheat/patch -
that requires a human running the app, per the milestone's own
constraints (no GUI automation, no launching emulators, no touching ROM
archives). See the manual checklist in the milestone's final report.
