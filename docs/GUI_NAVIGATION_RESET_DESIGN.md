# GUI Navigation Reset: Gamer View / Advanced View

Status: **design direction approved; the decisions in §9 are final for
this milestone.** No code changes made or implied by this document
existing — implementation has not begun. Written in response to the
v0.7.0-alpha manual QA NO-GO verdict (live Nobara/Saltbox run, branch
`integration-v0.7-platform-and-adapters`, commit `1e07279`). Supersedes
the incremental-milestone framing in
`docs/GUI_SIMPLIFICATION.md` for navigation-shell purposes — that
document's prior work (shared components, the Library tab shell, the
Cheats & Mods 5-area structure, the unified `selected_archive` state) is
**not discarded**; it is the implementation substrate this design reuses,
not a starting point this design throws away. See §7 for exactly what
carries over.

## 0. Why this document exists

The manual QA run proved eight release-blocking findings, all
navigation/discoverability defects, zero backend defects:

1. Duplicate and confusing navigation labels.
2. Identity details undiscoverable from a selected Library row.
3. Selection state is visually weak and does not expose the next action.
4. "NotMountable" is misleading for valid direct RVZ/ISO images.
5. Mount All is dangerously prominent and can begin processing the entire
   library accidentally (**reproduced live**: 12,551 items, user had to
   manually stop it).
6. Cheats & Mods lacks a clear game-selection path.
7. The GUI exposes its internal workflow model instead of a beginner
   workflow.
8. Even screenshot-based guidance could not reliably identify the correct
   navigation path.

The instruction that produced this design was explicit: **do not delete
capability, do not patch individual labels while keeping the same
structure — replace the navigation shell**, split into two modes.

## 1. The two-mode model

```
                    ┌─────────────────────────┐
                    │   ArchiveFsApp (unchanged core,
                    │   unchanged state, unchanged safety)
                    └────────────┬────────────┘
                                 │
              ┌──────────────────┴──────────────────┐
              │                                       │
      ┌───────▼────────┐                    ┌────────▼────────┐
      │   Gamer View     │  <- default        │  Advanced View   │
      │  (one screen)     │                    │ (many screens)   │
      └────────────────────┘                    └──────────────────┘
```

- **Gamer View** is the default for every fresh install and every user
  who has not explicitly switched. It is one primary screen: a
  searchable game list plus a selected-game action panel. It encodes
  exactly one workflow — **find game → choose cheats/mods → install →
  undo** — and handles everything else (platform detection, identity,
  provider selection, emulator profile selection, direct-image handling,
  backups, journals, rollback) automatically wherever it is safe to do
  so.
- **Advanced View** is everything that exists in the GUI today, kept in
  full, reorganized behind one clearly-labeled mode a beginner never has
  to enter. Nothing described in Advanced View is new work beyond
  relabeling/regrouping existing pages — it is explicitly **not** a
  rewrite of Sources, Mount, Active Mounts, History & Logs, Settings,
  Doctor, or Cheats & Mods' internals.
- **Mode is a view-layer switch only.** `ArchiveFsApp`'s state,
  `archivefs-core`'s transaction/journal/rollback/identity/database code,
  and every existing adapter are shared, unmodified, and used by both
  modes identically. Gamer View does not get a simplified backend; it
  gets a simplified *lens* onto the same backend, matching the review's
  explicit instruction ("the same tested backend underneath both").

### 1.1 Switching modes

Deliberately asymmetric, by decision:

- **Gamer View → Advanced View**: reached through a small settings/gear
  menu icon in Gamer View, not a permanent top-level navigation control.
  The gear menu's contents in Gamer View are minimal (at least one entry:
  "Advanced View"); it is not a general settings surface — general
  Settings itself lives in Advanced View. This keeps Gamer View's default
  screen free of any standing invitation to leave the simple mode.
- **Advanced View → Gamer View**: a clear, prominent, always-visible
  "Return to Gamer View" action — not gear-hidden, not buried. A beginner
  who has dropped into Advanced View (via a fallback link, per §3.3) must
  never have to hunt for the way back.
- Either direction requires no confirmation (switching modes is fully
  reversible and destroys no state).
- The chosen mode persists across restarts (a new, single
  `ui_mode: GuiMode` preference, alongside existing persisted settings —
  see `docs/GUI_SIMPLIFICATION.md`'s note that `ArchiveFsApp` itself is
  not serialized between runs today; this one field is the exception,
  stored the same way other on-disk preferences already are, e.g.
  alongside `~/.config/archivefs/emulator_profiles.toml`'s precedent).
- Default for a fresh profile/first launch: **Gamer View**, unconditionally.
- Nothing about which mode is active changes what data exists, what scans
  have run, what's mounted, or what history/journals contain. Switching
  modes mid-workflow (e.g. mid Cheats & Mods install) is safe: Advanced
  View's screens read the same `selected_archive` / `cheat_workflow`
  state Gamer View was using, so a user who needs to "drop down" to
  Advanced mid-task to resolve something automatic handling couldn't
  (§4's fallback cases) returns to a consistent state, not a reset one.

## 2. Gamer View

### 2.1 Screen layout

One screen, two regions, always both visible (no navigation *within*
Gamer View beyond this):

```
┌──────────────────────────────────────────────────────────┐
│  [Search games...]                                  [⚙]   │
├──────────────────────────┬─────────────────────────────────┤
│  Platform chips           │   Selected game panel            │
│  [All] [GameCube] [PS2]   │   ┌───────────────────────────┐  │
│  [SNES] [ZX Spectrum] ... │   │  Cover-less title header   │  │
│                            │   │  Platform · status line    │  │
│  Game list (search-        │   ├───────────────────────────┤  │
│  filtered, platform-       │   │  Primary action:           │  │
│  filtered)                 │   │  [ Mount ]  or              │  │
│                            │   │  [ Ready to play — no       │  │
│  ▸ Animal Crossing          │   │    mounting needed ]        │  │
│  ▸ ZooCube                  │   ├───────────────────────────┤  │
│  ▸ Sonic the Hedgehog       │   │  [ Cheats & Mods ]          │  │
│  ...                        │   │  [ Details ]                │  │
│                            │   │  [ Open location ]           │  │
│                            │   │  [ Undo last change ]  (only │  │
│                            │   │    shown if there is one)    │  │
│                            │   └───────────────────────────┘  │
└──────────────────────────┴─────────────────────────────────┘
```

This is a re-composition of existing render functions, not new
rendering logic invented from nothing:

- The game list reuses the existing filtered-archive-row rendering
  (`show_archive_rows` and its filter plumbing) minus every
  column/control that isn't "title, platform, one status word."
- Platform chips reuse the existing live, registry-driven platform strip
  (`docs/PLATFORM_LIBRARY_RECOVERY.md`'s already-unified platform filter)
  promoted from "a filter you apply" to "the first click," per the
  earlier read-only architecture review's §2.2.
- The selected-game panel reuses `show_selected_archive`'s underlying
  data (identity, platform, status) but re-renders it action-forward
  instead of detail-forward — see §2.2 for exactly what changes.
- "Cheats & Mods" opens the existing 5-area workflow page
  (`show_cheats_mods_page`), scoped to the already-selected game — no
  independent archive picker, directly fixing finding #6.
- "Details" opens the existing identity/metadata content
  (`show_selected_archive`'s grid) in a focused panel/modal reachable in
  one click from the row, directly fixing finding #2.

**Action visibility rules** (each independent, evaluated per selected
item):

| Action | Shown when |
|---|---|
| Mount / Unmount | Always, format-dependent wording (§2.3) |
| Cheats & Mods | Always, once a game is selected |
| Details | Always, once a game is selected |
| Open location | Only when the selected item has a real, accessible filesystem path — not shown for an item whose path is missing, unreadable, or otherwise not a real location to open |
| Undo last change | Only when the selected game has a genuinely reversible operation recorded — not shown speculatively, not shown for an operation that succeeded but has nothing to undo (e.g. a preview-only inspection) |

### 2.2 Fixing the weak-selection-state finding (#3)

Today, selecting a row is a quiet state change with no visible
consequence beyond a separate "Selected" page existing somewhere in the
sidebar. In Gamer View, selecting a row **immediately populates the
right-hand action panel** — selection has an obvious, adjacent, visible
effect the instant it happens, and the panel's very first visible element
is the next action (Mount, or Cheats & Mods, or Details), not a wall of
identity fields. Identity/metadata detail moves behind the explicit
"Details" button (§2.1) rather than being the default content of the
selected-state panel.

### 2.3 Fixing "NotMountable" (#4)

`NotMountable` as a literal status label is an internal state name
leaking into a beginner-facing surface, and it reads as an error for
items that are not broken — a direct GameCube RVZ/ISO/GCZ/CISO image
never needed mounting in the first place; ArchiveFS's mount step exists
for *archive* formats (ZIP/7z/RAR), not for direct images. Gamer View's
primary-action slot resolves to exactly one of three states, never a raw
backend enum name:

| Underlying state | Gamer View primary action shown |
|---|---|
| Archive format, not yet mounted | `[ Mount ]` button |
| Archive format, already mounted | `[ Unmount ]` button (plus an inline "Currently mounted" status word) |
| Direct image format (RVZ/ISO/GCM/GCZ/WBFS/CISO/etc.) | No mount button at all — a plain status line: **"No mounting needed — ready for your emulator."** |
| Direct image, identity still resolving | **"Checking this game..."** (transient — see §4.5 for the terminal-state guarantee that prevents this from spinning forever, already proven working in Phase 2/4 of the live QA run) |
| Direct image, identity undecodable (e.g. GCZ/WBFS today) | **"We can't read details for this file format yet — it still works, just without extra game info."** (a terminal, honest, non-alarming state — not hidden, not framed as broken) |

Advanced View keeps the literal internal state names (`NotMountable`
etc.) exactly as they exist today, since that audience benefits from
precision the beginner audience does not need — see §5's
"Advanced-View-only vocabulary" rule.

### 2.4 Fixing "Mount All" danger (#5) — the reproduced incident

**Mount All does not appear anywhere in Gamer View — final, no
exceptions.** This is a firm decision, not a default: Gamer View's
per-game Mount/Unmount buttons (§2.3) act on exactly one game at a time,
by construction — there is no batch surface reachable from Gamer View to
guard against, because none exists there.

**Every bulk action, without exception** (Mount All, Unmount All, bulk
platform assignment, and any bulk action added later) — all
Advanced-View-only, under Batch Tools (§3) — is gated by one fixed rule
set:

1. **Always show a preview and the exact item count** before any
   confirmation is even offered. This is unconditional — there is no
   bulk action anywhere in Advanced View that skips straight to a
   confirmation without first showing what it's about to affect and how
   many items that is. (The count that would have told the user
   "12,551 items" up front, before starting, is the single piece of
   information that would have prevented the live incident.)
2. **1–25 items**: a normal confirmation dialog, matching today's
   behavior.
3. **More than 25 items**: the confirmation requires the user to **type
   the exact item count** to proceed — not a second click, not a delay,
   a literal typed match against the count shown in the preview. This is
   the final mechanic, replacing this design's earlier "typing or a
   delayed second click" placeholder.

This is the highest-severity fix in this document because it's the one
finding that was live-reproduced against a real 12,551-item collection,
not just observed as a discoverability complaint.

### 2.5 Workflow mapping

| Step | Gamer View surface | What happens automatically underneath (§4) |
|---|---|---|
| **Find game** | Search box + platform chips + game list | Platform detection, identity resolution up to the confidence threshold in §4.2 |
| **Choose cheats/mods** | `[ Cheats & Mods ]` button on the selected-game panel | Provider selection, emulator-profile selection, direct-image identity gating — all per §4 |
| **Install** | The existing Cheats & Mods workflow's own Install step (unchanged — Review-then-Confirm, preview-before-apply, all existing safety properties preserved verbatim) | Backup + journal creation (already automatic in core) |
| **Undo** | `[ Undo last change ]` on the selected-game panel, shown only when there is something to undo for this game | Rollback via the existing journal/rollback mechanism, unchanged |

### 2.6 Banned vocabulary in Gamer View

None of the following backend/internal terms may appear anywhere in
Gamer View's default (non-fallback) rendering: *Scanner, Source(s),
Provider, Adapter, Identity Evidence, Transaction, Journal, Snapshot,
Generation, Database, Catalogue* (as a noun for the RetroArch cheat
database specifically — "cheat database" or just "cheats" is fine, the
banned form is the internal `CatalogueManagerState`-flavored wording),
*Preview* (as a workflow-stage name — "Review before installing" reads
the same to a beginner without requiring them to know it's a distinct
pipeline stage). These terms remain exactly as-is in Advanced View — this
is a Gamer-View-only vocabulary constraint, not a rename of the backend
or of Advanced View's own screens.

## 3. Advanced View

### 3.1 Screen inventory — every existing capability preserved

Advanced View is a **relabeling and regrouping** of what exists today,
not new pages. Every item the review instruction listed maps to an
already-existing, already-tested piece of the current GUI:

| Required Advanced View capability | Current implementation it maps to | Change needed |
|---|---|---|
| Sources | `MainView::Sources` / `show_sources_page` + `show_sources_overview` | None — already well-structured (`docs/GUI_SIMPLIFICATION.md`'s Sources cleanup milestone) |
| Mounts | `MainView::Mount` (queue) + `MainView::ActiveMounts` | None to their internals; entry point moves under Advanced |
| Provider details | `show_retroarch_catalogue_manager`, adapter-specific provider cards (`show_<adapter>_external_provider`) | None |
| Emulator profiles | `retroarch_profiles` / `pcsx2_profiles` / `dolphin_profiles` / `xenia_profiles` state + their `show_<adapter>_profile_card` renderers, plus Settings' profile-discovery controls | None |
| Identity evidence | `AdapterIdentityEvidence`, `platform_provenance_lines`, the "Technical provenance" disclosures already in Cheats & Mods | None |
| Scanner state | `database_state`, `database_generation`, `refresh_generation`, Settings' "Scan library" controls | None |
| Batch tools | Mount All, Unmount All, bulk platform assignment (`bulk_platform_action`) | **Relocated** here from their current prominent placement (§2.4); confirmation gate strengthened |
| History | `MainView::HistoryLogs` (full page, `HistoryLogFilters`) | None |
| Backups | Backup file locations — currently implicit in core's backup-on-install behavior, reachable only via per-operation journal detail | **None this milestone** (decision 11): existing access is preserved exactly as-is via History & Journals' existing rollback/journal disclosures. A dedicated Backups-summary screen is explicitly deferred, not part of this milestone. |
| Journals | The journal-detail disclosures already present in History & Logs' rollback card | None |
| Rollback | `SharedRollbackState`, the rollback/journal UI in History & Logs | None |
| Doctor diagnostics | `MainView::Doctor` | None |
| Cache and privacy settings | `MainView::Settings`'s existing sections | None |
| Technical logs | Currently the stdout log file (`~/archivefs-*-qa.log`-style output) — not exposed in-GUI today | **None this milestone** (decision 11): existing access is preserved exactly as-is — the log remains reachable only via its existing filesystem location, not through the GUI. A dedicated in-GUI log viewer is explicitly deferred, not part of this milestone. |

No item in this table is deleted. Backups and Technical logs keep
exactly their current (non-GUI, existing-access-only) reachability for
this milestone — see decision 11 in §9.

### 3.2 Advanced View structure

Advanced View keeps its own internal multi-page navigation — it does not
need to be one screen, per the review instruction. Recommended grouping
(sidebar sections, not a flat 11-item list — this also addresses finding
#1's "duplicate and confusing labels" for the advanced audience, by
giving related pages a visible group heading instead of a flat list
where "Mount," "Selected," "Active Mounts," and "Library" all compete at
the same visual level):

```
Advanced View
├─ Library (existing 4-tab shell: Archives/Health/Duplicates/Views — unchanged)
├─ Mount & Active Mounts
│   ├─ Mount queue
│   └─ Active mounts
├─ Cheats & Mods
├─ Sources
├─ Batch Tools          <- Mount All / Unmount All / bulk platform, relocated (§2.4)
├─ History & Journals
│   ├─ History & Logs
│   └─ Rollback / journal detail
├─ Diagnostics
│   └─ Doctor
└─ Settings
    ├─ (existing numbered sections, unchanged)
    ├─ Emulator profiles
    └─ Cache and privacy
```

No dedicated "Technical logs" or "Backups" entries appear in this tree —
per decision 11, both are deferred this milestone; their existing (non-GUI)
access is preserved unchanged (§3.1). `[ Return to Gamer View ]` (§1.1)
is a clear, always-visible action at this shell's top level, not nested
under any of the sections above.

`About` moves to a footer link (Settings or a corner "About" link) in
both modes — it's low-frequency in either audience and doesn't need a
top-level slot in either view.

### 3.3 Labeling requirement

Every Advanced View screen must be reachable only via the "Advanced View"
top-level surface — there must be no path from Gamer View into any
Advanced View screen except (a) the explicit mode switch (§1.1), or (b)
an explicit, clearly-labeled fallback link raised by §4.4's
"automatic handling failed" case, which must say what it's for (e.g.
"Open Advanced Sources to assign a platform manually" — not a bare
"Advanced View" link with no stated purpose).

## 4. Automatic-handling rules (Gamer View's "invisible plumbing")

This is the operational core of the review instruction "It should
automatically handle platform detection, identity, provider selection,
emulator profile selection, direct-image handling, backups, journals and
rollback wherever safe" and "do not expose backend concepts unless
automatic handling fails." Each subsystem gets an explicit safe-automatic
rule and an explicit fallback trigger — vague "handle it automatically"
is exactly the kind of underspecification that produces another
undiscoverable workflow, so this section is deliberately concrete.

### 4.1 Platform detection

**Automatic, always.** Already fully automatic today
(`docs/PLATFORM_LIBRARY_RECOVERY.md`'s precedence order: saved override →
bounded format/header identity → saved source assignment → folder alias →
filename/path evidence → Unknown). Gamer View never asks the user to
resolve platform detection directly.

**Fallback**: if platform is Unknown, Gamer View shows the game in an
"Unknown platform" section of the list (not hidden — QA's own Phase 3
philosophy of "honest Unknown, never hidden" carries over unchanged) with
a single link: "Help us identify this — Advanced Sources." No raw
detection-attempt explanation text in Gamer View itself (that stays an
Advanced View / Details-panel disclosure).

### 4.2 Identity (game/title matching)

**Automatic when confidence is `Verified` or `Strong`** (the existing
`preview_match_strength_presentation` badges already distinguish these
from `Candidate`/`Ambiguous`/`Unsupported` — this is reused, not
reinvented).

**Fallback**: at `Candidate` or `Ambiguous` confidence, Gamer View must
not silently pick a match. It shows the selected-game panel with an
inline, plain-language prompt — "We're not fully sure which game this
is" — with a single beginner-safe control to either accept the best
match or open Details for the full evidence (which is Advanced-View-flavored
content, reachable from a Gamer View button per the exception in §3.3).
This may also be the eventual fix for the live QA finding of "Army Men -
Sarge's War shows 'No compatible cheats found'," if that case turns out
to be an identity-confidence issue rather than a genuine no-cheats-exist
case. Per decision 10, resolving which of the two it actually is is left
to resumed manual QA and explicitly **does not block this design** — the
two fallback wordings this section already specifies (identity-confidence
uncertainty vs. a plain "no compatible cheats found" result) both already
exist in this design; QA only needs to confirm which one applies to that
specific title.

### 4.3 Provider selection (which cheat/patch source, which adapter)

**Automatic when exactly one adapter applies to the detected platform**
(e.g., GameCube/Wii → Dolphin only; today's `CheatEmulatorAdapter`
already encodes this via `platform_is_<adapter>` gating).

**Automatic when multiple adapters apply but the game has an established
choice** (existing "remembered emulator profile" behavior —
`remembered_emulator_profiles` already persists this across sessions;
reused, not reinvented).

**Fallback**: when multiple adapters apply and there's no remembered
choice (e.g. a PS2 title, RetroArch vs. PCSX2), Gamer View shows a plain
two-or-three-way choice **without the word "adapter"** — e.g. "This game
works with RetroArch or PCSX2 — which do you use?" — reusing the existing
`tab_row`-based picker's mechanics (already built, already tested) with
Gamer-View wording instead of "Choose a system."

### 4.4 Emulator profile selection

**Automatic when exactly one eligible profile is discovered** for the
chosen adapter (already-existing `eligible_<adapter>_profile_ids`
functions determine eligibility; reused as-is).

**Fallback, two cases**:
- Multiple eligible profiles found → plain picker: "Which [RetroArch/
  PCSX2/Dolphin/Xenia] setup should we use?" listing them by their
  existing display names, no path/technical detail shown unless the user
  asks (an Advanced-View-flavored "show details" expander, same pattern
  as §4.2).
- Zero eligible profiles found → this is the one case where Gamer View
  **must** name a backend concept, because there is no automatic
  resolution possible: "We couldn't find a [RetroArch] setup on this
  computer. Open Advanced Settings to point us at it." — a direct,
  labeled link into Advanced View → Settings → Emulator profiles (§3.1),
  satisfying "do not expose backend concepts unless automatic handling
  fails" by construction (it is named exactly when, and only when,
  automatic handling has in fact failed).

### 4.5 Direct-image handling

**Automatic, always** — see §2.3's full state table. This is the one
subsystem the live QA run already substantially validated (Phase 4:
"Cheats & Mods reaches a final result instead of spinning forever," RVZ
visible, GameCube count correct) — the remaining Gamer View work here is
presentation-only (§2.3's wording), not new detection logic.

### 4.6 Backups, journals, rollback

**Fully automatic and invisible in the success path**, exactly as core
already implements it — every install already creates a backup and
journal entry unconditionally; nothing new is needed for this to be
"automatic," it already is at the backend layer. Gamer View's only
visible surface for this subsystem is the single `[ Undo last change ]`
button (§2.5).

**Fallback**: if an undo/rollback itself fails (a real failure, not a
routine action), this is exactly the class of event serious enough to
warrant naming the backend concept — Gamer View shows "Something went
wrong undoing this change" with a link to Advanced View → History &
Journals, where the full journal/rollback detail already exists
unchanged. This mirrors §4.4's "name it exactly when automatic handling
fails" rule.

## 5. Terminology guide

| Concept | Gamer View wording | Advanced View wording (unchanged from today) |
|---|---|---|
| Archive not yet mounted | "Mount" | "Mount" |
| Direct image, no mount step needed | "No mounting needed — ready for your emulator" | `NotMountable` / equivalent precise internal state name, kept as-is |
| Cheat/patch database | "Cheats" | "Catalogue" / "Cheat database" (existing wording, e.g. "Database and sources") |
| Adapter choice | "Which [emulator] do you use?" | "Choose a system" (existing) |
| Identity confidence | "We're not fully sure" / accept-or-check-details | "Verified exact match" / "Strong match" / "Candidate match" / "Ambiguous" (existing badges, unchanged) |
| Undo | "Undo last change" | "Rollback" / journal terminology (existing, unchanged) |
| Whole-library batch mount | *(not shown in Gamer View at all)* | "Mount All" (existing, relocated + re-gated per §2.4/§3.1) |

## 6. Traceability — every NO-GO finding to its fix

| Finding | Fixed by |
|---|---|
| #1 Duplicate/confusing labels | §3.2's grouped Advanced sidebar + Gamer View having no sidebar at all |
| #2 Identity undiscoverable | §2.1's one-click Details button from the selected-game panel |
| #3 Weak selection state | §2.2 — selection immediately populates an action-forward panel |
| #4 Misleading "NotMountable" | §2.3's state table, translated wording |
| #5 Dangerous Mount All | §2.4 — removed from Gamer View entirely, count-scaled confirmation in Advanced View |
| #6 No clear Cheats & Mods entry | §2.1/§2.5 — Cheats & Mods opens only from an already-selected game |
| #7 Internal workflow model exposed | §2.6's banned-vocabulary list + §4's automatic-handling rules |
| #8 Screenshot-guided nav failed | §1's two-mode split with one non-navigable primary screen in Gamer View — there is no path-finding problem left when there is no path to find |

## 7. What this design reuses vs. what's genuinely new

**Reused verbatim (no design risk, already built and tested)**: the
Library archive-row rendering and filters, the platform strip, `selected_archive`
as the single authoritative selection, the Cheats & Mods 5-area workflow
and its adapter picker mechanics, all four adapters' profile-discovery
state and functions, `remembered_emulator_profiles`, the match-strength
badge system, the direct-image identity state machine, backup/journal/
rollback at the core layer, `tab_row`/`status_strip`/`technical_details`/
`activity_row_header` as shared widgets.

**Genuinely new**: the Gamer View screen itself (a new composition, not
new underlying logic); the settings/gear-menu mode switch and the
"Return to Gamer View" action; the mode-switch preference and its
persistence; the count-scaled batch-confirmation strengthening (§2.4,
now with final thresholds and mechanic).

**Explicitly deferred, not part of this milestone** (decision 11): a
Backups-summary screen and an in-GUI Technical logs viewer. Both remain
exactly as reachable (or not) as they are today — see §3.1.

## 8. Explicit non-goals of this document

- No change to `archivefs-core` — transaction safety, preview/apply,
  identity resolution, journal/rollback mechanics are unmodified. This
  design is a view-layer reorganization.
- No deletion of any `MainView` variant, render function, or adapter
  capability. Advanced View is a superset of today's functionality,
  reachable through a clearer structure.
- No code changes are proposed or should be inferred from this document.
  Implementation sequencing (which milestone ships Gamer View's shell
  first, how mode-switching is threaded through `update()`'s dispatch,
  how the count-scaled batch confirmation is implemented) is deliberately
  left for a follow-up implementation plan, not decided here.

## 9. Decisions locked in this revision

The design direction is approved. These eleven decisions are final for
this milestone and are reflected throughout the document above, not just
here:

1. Every bulk action must show a preview and the exact item count before
   any confirmation is offered (§2.4).
2. Bulk actions affecting 1–25 items use a normal confirmation (§2.4).
3. Bulk actions affecting more than 25 items require the user to type
   the exact item count to confirm (§2.4).
4. Mount All must not appear anywhere in Gamer View — no exceptions
   (§2.4).
5. The two modes are named exactly **Gamer View** and **Advanced View**
   — no alternate labels (e.g. the earlier "Simple View" wording is
   retired).
6. Gamer View exposes the Advanced View switch through a small
   settings/gear menu, not a permanent top-level navigation control
   (§1.1).
7. Advanced View provides a clear, always-visible "Return to Gamer View"
   action (§1.1, §3.2) — asymmetric with decision 6 by design.
8. Open location is shown only when the selected item has a real,
   accessible path (§2.1's action visibility rules).
9. Undo is shown only when the selected game has a genuinely reversible
   operation recorded (§2.1's action visibility rules).
10. The Army Men - Sarge's War identity-vs-no-cheats wording question is
    resolved during resumed manual QA and does not block this design
    (§4.2).
11. No new Backups-summary or Technical-logs viewer screens are added in
    this milestone. Existing (non-GUI) access to both is preserved
    unchanged; dedicated viewers are deferred (§3.1, §3.2, §7).

## 10. Implementation follow-up: visual platform picker (post-approval)

Implemented on `feature/gui-navigation-reset` after this design was
approved, in a separate "Gamer View Visual Platform Picker and Library
Layout Polish" milestone - see `docs/PLATFORM_ARTWORK.md` for the full
artwork policy, asset manifest, and licensing record. Summary of what
changed relative to §2.1's original sketch:

- The plain text platform chips (§2.1's ASCII mockup) became a
  single-row, horizontally scrollable "shelf": each item shows a small
  original vector glyph, the platform name, and its game count, with a
  clear selected state. All/Unknown are included with suitable generic
  glyphs. The shelf has a fixed height and never wraps onto multiple
  lines, regardless of how many platforms are present.
- The finishing pass makes those cards viewport-responsive within a
  96–124 logical-pixel range. Counts always remain visible; long names
  use a single ellipsis only when necessary, while hover/accessibility
  text retains the complete name. Horizontal scrolling remains the only
  overflow mechanism, so the shelf never becomes a multi-row grid.
- Gamer View search now receives the top bar's main share, up to 760
  logical pixels, while reserving room for Settings and the busy spinner.
  At 1024×600 it remains readable without displacing either control.
- Clicking a shelf item updates `library_filters.platform` exactly as
  the text chips did, and clears stale focused/multi-selection exactly
  as before (§9 decision 3's fix) - no new selection model was
  introduced.
- The game list and selected-game panel layout was hardened after manual
  QA found the list was only showing 2-3 rows regardless of window size:
  the list now explicitly reserves the page's full remaining height
  (via `allocate_ui_with_layout`, the same technique `ui_layout::page`
  itself uses) rather than relying on ambiguous height inheritance
  through nested containers, and scrolls independently while the
  selected-game panel stays fixed alongside it.
- The selected-game action panel now visually separates the primary
  action (Mount/Unmount, full-width, `ActionStyle::Primary`) from
  secondary actions (Cheats & Mods/Details/Open location, grouped in one
  row) and Undo (shown last, `ActionStyle::Quiet`), with clearer
  empty-state and empty-search/empty-platform guidance.
- No artwork is fetched over the network, ever - see
  `docs/PLATFORM_ARTWORK.md`'s no-network guarantee. Custom artwork is
  rendered only from bounded local PNG files in the explicitly configured
  directory. Missing, malformed, oversized, symlinked, or unsupported SVG
  files retain the built-in vector glyph. The existing resolved `image`
  crate is now a direct dependency with only PNG decoding enabled; its
  `MIT OR Apache-2.0` licence is compatible with ArchiveFS.

### Platform Artwork Pack v1 follow-up

The `feature/platform-artwork-pack-v1` milestone keeps the shelf and all
navigation behavior above intact while replacing temporary exact-platform
abstract glyphs with supplied original/generated PNG hardware illustrations.
The 17 approved PNGs are compiled into the GUI with `include_bytes!`; they do
not depend on repository paths after installation and add no network or
external-tool path.

Resolution is: valid user custom PNG, exact bundled hardware PNG, existing
category glyph, then Unknown. Exact case-insensitive aliases are resolved
before category inference, with narrow mappings that keep Wii U distinct from
Wii and Xbox 360 distinct from original Xbox. Decoded custom and bundled
textures are cached rather than reparsed per frame, and malformed data falls
through safely. The existing SVGs remain canonical category/fallback and
licensing references. The authoritative filename/alias table, inspection
record, bundle size, provenance statement, and offline guarantee are in
`docs/PLATFORM_ARTWORK.md`.
