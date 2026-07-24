# GUI Simplification

This document tracks the GUI-simplification work done on the
`sonnet-gui-simplification` branch, why it was scoped the way it was, and
what a future, larger pass would still need to do.

## Scope of this pass

This was explicitly a **safe incremental redesign**, not the full
information-architecture (IA) rewrite the original request described. The
`archivefs-gui` crate is a single ~36,000-line `main.rs` with 413+
pre-existing tests, many of which assert on exact function names
(`show_selected_page`, `show_cheats_mods_page`, `show_primary_navigation`,
…) or exact rendered widget label text (`primary_nav_rects` clicks buttons
by literal label). A wholesale rename/merge of `MainView` destinations
would have required rewriting a large fraction of those tests in the same
pass as the redesign itself - high regression risk for a safety-relevant
app that manages live mounts, rollback, and journals.

Given that, this pass:

- Kept the `MainView` enum, its 14 destinations, and every existing
  render-function name and widget-label test hook unchanged.
- Added real, reusable shared components and applied them to reduce actual
  duplication.
- Added a genuinely new, honest explanation for Unknown-platform entries.
- Made the RetroArch cheat database reachable from Cheats & Mods without
  navigating to Sources.
- Consolidated one concrete instance of duplicated failure messaging.
- Cleaned up one piece of internal-jargon navigation terminology.
- Left clear seams (documented below) for the larger IA migration to be
  done deliberately, later, as its own reviewed effort.

## Shared components (`crates/archivefs-gui/src/ui/components.rs`)

Three new components were added, each used in at least one real call site
in this pass (not just written and left dead):

- **`technical_details`** - the one place any "collapsed by default,
  internals live here" disclosure should go through, instead of every call
  site hand-rolling its own `egui::CollapsingHeader` with its own title and
  default-open state. Applied to the RetroArch provider card's former
  "Open details" section.
- **`status_strip`** - a compact single row of status badges, replacing
  hand-rolled `horizontal_wrapped` + repeated `status_badge` calls.
  Applied to the RetroArch provider card's status/trust badges.
- **`failure_summary`** - one consistent presentation for "an operation
  failed, but the previous good result is still active": a direct
  plain-language headline plus an optional retained-state note, with the
  full original error text preserved (not discarded) behind
  `technical_details` rather than always shown inline. Applied to the
  transient post-retrieval result banner in the RetroArch catalogue card.

**Deliberately not touched:** the per-entry persistent catalogue-failure
banner inside `show_retroarch_catalogue_manager` (the one keyed off
`entry.last_error`) was left as a plain, always-visible banner rather than
routed through `failure_summary`. An existing test
(`sources_retained_snapshot_stays_visibly_usable_after_update_failure`)
asserts that the full error text (e.g. `"download_too_large"`) is directly
visible without expansion for that specific banner. Collapsing it would
have both hidden error detail a test - and by extension a user - currently
relies on, and weakened, not clarified, safety-relevant failure reporting.
This is exactly the kind of case flagged in the original request as "don't
lose error detail"; the right call here was to leave it alone.

There are ~20 more hand-rolled `egui::CollapsingHeader` call sites across
`main.rs` with inconsistent titles ("Technical detail", "Technical
metadata", "Technical provenance", "Technical blockers", …) that were not
migrated to `technical_details` in this pass, to keep the diff reviewable.
They are safe, low-risk, mechanical follow-ups - see "Remaining
duplication" below. (Most of these were migrated in Phase 2 - see that
section for the final disposition of each one.)

## Navigation

- The MainView enum, `PRIMARY_NAVIGATION_DESTINATIONS` (11 entries) and
  `SECONDARY_NAVIGATION_DESTINATIONS` (3 entries: Health, Duplicates,
  Library Views) are unchanged.
- The secondary section's sidebar label was renamed from `"CATALOGUE
  VIEWS"` to `"LIBRARY TOOLS"` (now `SECONDARY_NAVIGATION_SECTION_LABEL`,
  used by both the production renderer and its test mirror so they cannot
  drift). "Catalogue" is internal terminology a first-time user has no
  reason to know; these three destinations are all secondary lenses onto
  Library data.
- No destinations were merged, renamed, or removed.

### Proposed future Library-tab IA migration (not done in this pass)

The original request's Part 1 suggested folding Health, Duplicates, and
Library Views into a single tabbed Library view. That is still the right
long-term direction, but it means:

1. Deciding whether `MainView::Health` / `MainView::Duplicates` /
   `MainView::LibraryViews` become sub-tab state within `MainView::Library`
   or are removed as separate enum variants entirely (the former is safer
   for deep-linking/state-restoration behaviour, if any exists).
2. Rewriting every test that constructs a `MainView` value for these three
   destinations, and every test that asserts they appear in
   `SECONDARY_NAVIGATION_DESTINATIONS` specifically (see
   `every_navigation_destination_has_a_title_and_width_policy`,
   `all_navigation_destinations_are_reachable_via_a_real_click`, and the
   `primary_nav_rects` test mirror, all in `main.rs`'s `tests` module).
3. Deciding what happens to `main_view_content_width` and
   `main_view_uses_page_scroll`, both of which currently key off the exact
   enum variant.

None of this is hard, but it touches enough test surface area that it
deserves its own commit series and its own review pass, not to be folded
into a "simplification" branch alongside behavioural changes.

## Unknown-platform explanations

`archivefs-core`'s `detect_platform_with_details` (and the wrapping
`detect_platform_with_provenance` / `detect_platform`) only ever returns
`Some(detection)` or `None`. It does **not** currently record *why*
detection found nothing - there is no "unsupported extension" vs.
"ambiguous folder" vs. "archive contents not inspected" vs. "no alias
match" distinction anywhere in the returned type, in
`PlatformProvenance`, or in the persisted `platform_source` column. The
GUI therefore cannot honestly show a specific per-entry reason - only that
detection tried and failed, and one honest description of *what it tried*.

This pass added exactly that: a single shared explanation
(`UNKNOWN_PLATFORM_EXPLANATION` in `main.rs`), surfaced in two places:

- **Per-entry**, inside `show_platform_section` (the "Selected archive"
  card's Platform block, both on the Library page and reused elsewhere):
  when `platform_details.platform.is_none()`, a "Why is this Unknown?"
  banner appears directly above the Assign-platform control.
- **Aggregate**, on the Library page: when the "Unknown platform" filter
  checkbox is active and there is at least one matching row, a banner
  states the exact count (`"8257 entries with unknown platform"`) and the
  same explanation. Gated by the new pure function
  `unknown_platform_banner_visible`.

Both read from the same constant, so the copy cannot drift between the two
surfaces.

### What core would need to add for real per-entry reasons

To go from "here is what we checked" to "here is specifically why this
one file didn't match," `detect_platform_with_details` (or a new sibling
function) would need to return a reason on the `None` path too, e.g.:

```rust
enum PlatformDetectionMiss {
    UnsupportedExtension,
    AmbiguousFolder,
    ArchiveContentsNotInspected,
    NoAliasMatch,
}
```

and `ArchiveIdentity`/`PersistedArchive` would need a place to carry it
(today `platform_provenance` is `None` exactly when `platform` is `None`,
so there is nowhere to put a miss-reason without adding a field). The GUI
side is already structured so this is a small follow-up: `platform_source_label`
and `platform_provenance_lines` are the two functions that would grow a
new match arm, and `UNKNOWN_PLATFORM_EXPLANATION`'s single call sites
would become per-reason lookups instead of one constant - no page
structure changes required.

## Cheats & Mods / cheat database

- **New**: the RetroArch cheat database (Download / Update / Verify, the
  Review-then-Confirm safety step, live progress, and errors) is now
  rendered directly on the Cheats & Mods page via the same
  `show_retroarch_catalogue_manager` function Sources uses - not a copy,
  not a simplified stand-in. Both pages dispatch through the same new
  `ArchiveFsApp::handle_catalogue_manager_action`, so the two call sites
  cannot diverge in behaviour, and every existing safety property (network
  access only after an explicit Confirm step, cancellation, retained
  snapshot on failure) applies identically in both places.
- The old "Manage catalogue in Sources" button (which navigated away) is
  unchanged and still works, for anyone who prefers the full Sources page.
- Catalogue status now also lazy-loads on entering Cheats & Mods, not just
  Sources (`catalogue_status_load_needed`).
- Terminology: transient result banners after a download/update now say
  "Cheat database updated" / "Cheat database update failed" / "…
  cancelled" instead of "Catalogue activated" / "Catalogue update failed".

**Not done in this pass:** folding the RetroArch card's ~10 stacked
sections (profile, source-mode, identity, preview, archive context,
per-adapter workflow, mods, activity) into fewer, denser components (the
`GameSummary` / `MatchResult` / `PrimaryActionBar` components the original
request sketched). That is a real page-structure change to
`show_cheats_mods_page` and its ~10 sub-renderers, each with its own
tests; doing it safely means redesigning the workflow's UI in one focused
pass with its own review, not bundling it into this one alongside
navigation and Sources changes.

## Sources

The audit for this branch found Sources was already in reasonably good
shape: `show_retroarch_catalogue_manager`'s technical fields (provider ID,
canonical repository URL, resolver URL, download template, SHA-256
digest, trust classification) were already gated behind a collapsible
("Open details", now `technical_details`, labelled "Technical details")
rather than shown unconditionally. This pass's Sources-specific changes
were the `status_strip` and `failure_summary` applications described
above, plus the terminology change to the transient result banner.

## Remaining duplication (Phase 1 state - see Phase 2 below for updates)

- ~20 other `egui::CollapsingHeader::new(...)` call sites with
  inconsistent labels ("Technical detail" singular, "Technical metadata",
  "Technical provenance", "Technical blockers", "All technical blockers")
  that were not migrated to `technical_details` in this pass. Purely
  mechanical, low risk, but touches many call sites across the Cheats &
  Mods and PCSX2/Dolphin adapter code - deferred to keep this branch's
  diff reviewable.
- The Cheats & Mods page's ~10 stacked cards (see above) still repeat
  profile/source/trust/identity/destination context across sections in
  places; only the catalogue-status header badges and its failure
  messaging were consolidated in this pass.
- Activity/history still has up to four rendering surfaces (bottom bar,
  page-inline `ActionFeedback` banners, the full History & Logs page, and
  the Cheats & Mods-scoped `show_recent_cheat_activity` card). This pass
  did not touch activity rendering; each surface currently serves a
  plausibly distinct purpose (live status vs. permanent audit log vs.
  per-archive-scoped view), and consolidating them safely needs its own
  audit of which duplications are actually redundant vs. intentional.

---

## Phase 2 (`sonnet-gui-components-phase-2` branch)

A second safe-incremental pass, continuing directly from Phase 1's
deferred items above. Same constraints as Phase 1: `MainView` enum,
render-function names, and test-asserted labels/click-targets unchanged;
no IA rewrite; no mount/rollback/journal/cleanup/recovery behaviour
touched.

### What was migrated

**Technical-detail sections** (commit 1): audited all ~25 remaining
`egui::CollapsingHeader` call sites in `main.rs` individually. Migrated
the 9 that are genuinely passive technical/diagnostic internals with no
test coupling to their label text, to `technical_details`:

| Call site | Where |
|---|---|
| "Archive location" | Mount queue card's archive source path |
| "Technical detail" (journal) | History/rollback journal detail: Plan ID + per-entry outcome/verification/backup |
| "Technical metadata" ×2 | Dolphin Game INI and PCSX2 PNACH inspected-file cards' SHA-256/duplicate flags |
| "Technical provenance" | Shared game-identity evidence's method/confidence/ZIP-member detail |
| "Technical detail" (preview) | Cheats & Mods preview entry's destination-root/relative-path/blocker/warning detail |
| "Source technical details" | Cheat-source card's identifier/URL/digest/path detail |
| "Catalogue technical details" | Cheat-catalogue result card's digest/path detail |
| "Technical blockers" | A RetroArch profile's full blocker list |

Two more were migrated in commit 3 because they live inside the
activity/history rendering paths reworked there: the History & Logs
page's per-entry "Related archive" path disclosure, and page-inline
`ActionFeedback`'s "More information" elaboration.

**Cheats & Mods shared presentation** (commit 2): found one real,
byte-for-byte-identical 3x duplication - `show_cheats_mods_workflow_states`
(RetroArch), `show_pcsx2_workflow_states`, and `show_dolphin_workflow_states`
each built the same "label + status badge" row list inside a card, differing
only in row content. Extracted as `status_rows`. Also replaced the Cheats &
Mods page header's second badge row with `status_strip`.

**Activity/history consolidation** (commit 3): the bottom activity bar,
the full History & Logs page, and the Cheats & Mods "Recent related
activity" card each independently built the same "status badge + action
name [+ timestamp]" row header. Extracted as `activity_row_header`, with
a `trailing` closure parameter so the History & Logs page's right-aligned
Copy button keeps its exact original position and click target. Message
rendering, per-row actions (copy button vs. context menu vs. none), and
empty-state wording were deliberately left per-caller - those differ for
real space/interaction reasons across a thin bottom panel, a full page,
and a compact mini-card, not by accident.

### Components introduced

- **`status_rows`** (`ui/components.rs`) - a card of "label: status badge"
  rows. 3 real call sites from day one (RetroArch/PCSX2/Dolphin workflow
  states) - not a speculative wrapper.
- **`activity_row_header`** (`ui/components.rs`) - the shared activity-row
  header line, with optional timestamp and an optional right-aligned
  `trailing` slot. 3 real call sites (bottom bar, History & Logs, Cheats &
  Mods mini card).

### Which technical-detail sections were deliberately not migrated, and why

- **"Failed archives" / "Cleanup failures"** (mount-all/unmount-all
  results) - actionable per-archive failure lists a user needs to review
  and potentially act on (e.g. Lazy Unmount recovery), not internal
  technical data. Relabelling as generic "Technical details" would
  obscure that these are actionable, not internals.
- **"Passed checks (N)" / "Inspected Game INI files (N)" / "Inspected
  PNACH files (N)" / "Inspection warnings (N)" ×2 / "Identity warnings
  (N)" / "Conflict records (N)"** - all follow a distinct "count-in-title,
  expand to see the full content list" pattern, not "hidden internals."
  The count in the title is load-bearing information that a generic
  "Technical details" label would discard.
- **"All technical blockers" / "All technical blockers (N)"** (PCSX2
  profile card, RetroArch profile card variant) - these show the *first*
  blocker unconditionally, then offer "see all" as the expansion. Folding
  into `technical_details` would lose the "one is already visible above,
  this expands to the complete list" framing.
- **"Advanced retrieval options"** - contains an interactive checkbox
  (force-refresh), not passive diagnostics. `technical_details` is for
  read-only internals; putting a control behind a generically-labelled
  disclosure would make it harder to find, not easier.
- **"Library Database" / "Custom Platform Aliases"** - full Tools-overlay
  panel headers (an entire feature area each), not a "hide internals
  inside a card" disclosure. Also referenced by name in an existing
  navigation test.
- **"Safety, privacy, and responsible use"** - policy/consent content,
  not technical detail.
- **"Debug: action readiness"** - deliberately kept as its own
  always-identifiable label; an existing test
  (`debug_action_readiness_...`-style assertion) checks for this exact
  string as proof the running binary contains this code, per its own doc
  comment. Renaming it to generic "Technical details" would defeat that
  purpose.

### Remaining duplication after Phase 2

- The Cheats & Mods per-adapter profile cards (`show_pcsx2_profile_card`,
  `show_dolphin_profile_card`), the archive-context card, and the adapter
  selector still mix badges with adapter-specific controls (radio
  buttons, path fields) in ways that don't reduce to a shared shape
  without a parameter-heavy, largely-single-purpose wrapper. Left alone
  deliberately both in Phase 1 and Phase 2.
- Activity/history's per-row *message* rendering, empty states, and
  action affordances remain distinct per surface (by design - see above),
  even though the row *header* is now shared.
- The `ActionFeedback` struct itself (page-inline banners) still renders
  by hand (`ui.colored_label`) rather than through `widgets::banner` - it
  predates that component and carries slightly different semantics
  (success/failure colour choice, optional warning/cleanup sub-messages).
  Converting it fully would change its visual shape, not just its
  plumbing, so it was left alone rather than forced through Phase 1/2's
  purely-additive components.

### Remaining blockers before the full Library-tab IA rewrite

Unchanged from Phase 1 - still blocked on the same three items:

1. Deciding whether `MainView::Health` / `Duplicates` / `LibraryViews`
   become sub-tab state within `MainView::Library` or are removed as
   separate enum variants.
2. Rewriting every test that constructs those `MainView` values directly
   or asserts on `SECONDARY_NAVIGATION_DESTINATIONS`'s exact contents
   (`every_navigation_destination_has_a_title_and_width_policy`,
   `all_navigation_destinations_are_reachable_via_a_real_click`, the
   `primary_nav_rects` test mirror).
3. Deciding what happens to `main_view_content_width` and
   `main_view_uses_page_scroll`, both keyed off the exact enum variant.

Phase 2 did not add to or reduce this list - it touched no navigation
code beyond what Phase 1 already changed.

### Recommended next milestone

Two independent, similarly-scoped candidates, either of which is a
reasonable "Phase 3":

1. **Cheats & Mods page-structure pass.** Now that the byte-identical
   duplication (workflow-state rows, activity rows) is gone, the
   remaining ~8 stacked cards are structurally different enough that
   shrinking them means an actual layout redesign (e.g. combining
   archive-context + workflow-state + safety-info into one denser
   "game summary" header), not further component extraction. This is
   the `GameSummary`/`PrimaryActionBar` work the original request
   sketched - worth doing as its own pass with its own review, since it
   changes what a user sees on first load, not just how the code that
   renders it is organized.
2. **The Library-tab IA migration** described above, once someone is
   ready to take on rewriting the ~5-10 coupled navigation tests
   alongside the enum/dispatch changes in the same reviewed pass.

Either is safe to start from `main` at any time; neither depends on the
other.

## Safety boundaries (unchanged, explicitly re-verified)

Nothing in this pass touched: exact-path confirmation logic, the
Review-then-Confirm two-step for catalogue retrieval, preview-before-apply
for cheat transactions, stale-snapshot rejection, backup/journal/rollback,
destination-safety checks, or PCSX2/Dolphin's preview-only status. The one
new call path added (`show_retroarch_catalogue_manager` from Cheats &
Mods) reuses the exact same state and dispatch as Sources, so it inherits
those guarantees rather than reimplementing them. See the manual QA
checklist below for how to re-verify this by hand.

## Manual QA

1. Launch the GUI against a synthetic/test library (never real ROMs or
   emulator files).
2. Open **Library**. Confirm the sidebar's second section is labelled
   "LIBRARY TOOLS", not "CATALOGUE VIEWS", and Health/Duplicates/Library
   Views are still all individually reachable.
3. Enable the "Unknown platform" filter checkbox. Confirm the aggregate
   banner appears with the exact visible count and the same explanation
   text as below; disable the filter and confirm the banner disappears.
4. Select a row with Unknown platform. Confirm the selected-archive card's
   Platform section shows "Why is this Unknown?" with the explanation, and
   that the existing platform-assignment dropdown/Set Platform/Clear
   Manual Platform controls are still present and functional.
5. Select a row with a known platform. Confirm the "Why is this Unknown?"
   banner does *not* appear.
6. Open **Cheats & Mods** for an archive. Confirm the RetroArch cheat
   database card appears on the page itself (status badges via
   `status_strip`, Download/Update/Verify buttons) without navigating to
   Sources.
7. Click **Update** on the cheat database card from Cheats & Mods. Confirm
   the Review dialog appears (no network access yet) and nothing starts
   until **Confirm** is clicked - the same two-step behaviour as Sources.
8. Trigger (or fake, via the existing test fixtures) a catalogue update
   failure. Confirm the transient result banner reads "Cheat database
   update failed" with a retained-state note, and the raw error text is
   available under "Technical details" rather than only inline.
9. Open **Sources**. Confirm the provider card's status badges render on
   one compact row, and "Technical details" (not "Open details") gates
   provider ID / canonical repository URL / resolver URL / download
   template / SHA-256 digest / trust classification.
10. Resize the window narrower/wider. Confirm no layout breaks in the
    areas touched by this pass (Library filters row, selected-archive
    card, Cheats & Mods catalogue card, Sources provider card).
11. Navigate the sidebar with keyboard only (Tab/Enter or arrow
    navigation, depending on platform). Confirm every destination
    (including the newly-labelled "LIBRARY TOOLS" section) remains
    reachable.
12. Confirm the mount queue, active mounts, current selection, and
    History & Logs are all unaffected by any of the above (switching
    views, toggling filters, or expanding/collapsing the new components
    must not reset any of them).

### Phase 2 additions to manual QA

13. Open **Cheats & Mods** for a RetroArch-eligible archive, then for a
    PS2 archive (PCSX2 adapter), then for a GameCube/Wii archive (Dolphin
    adapter). Confirm each adapter's "Workflow state" card still shows
    all six rows (Emulator profile, Cheat or mod source, Trust state,
    Inspection state, Destination, Installation state) with the same
    values as before.
14. Open the mount queue (**Mount**) and expand an item's "Technical
    details" (formerly "Archive location"). Confirm the archive's source
    path is still there and Copy still works.
15. Open **History & Logs**, find an entry with a related archive.
    Confirm "Technical details" (formerly "Related archive") still shows
    the path and the row's **Copy** button is still in the same top-right
    position on the row.
16. Trigger an action that produces feedback with "More information"
    (e.g. an Unmount All with a partial cleanup failure). Confirm the
    elaboration is still reachable under "Technical details", collapsed
    by default.
17. Confirm PCSX2 and Dolphin remain preview-only (no install/enable/
    disable/rollback controls) after the workflow-state row migration.

### Cheats & Mods structure milestone additions to manual QA

18. Open **Cheats & Mods** for a PS2 archive. Confirm the page now shows
    "Overview", "Choose a system", and "Selected system workflow" as
    distinct headings, and the Overview card lists both "RetroArch · ..."
    and "PCSX2 · ..." availability lines (no "Dolphin · ..." line).
19. In "Choose a system", click the PCSX2 tab. Confirm the adapter
    switches (the tab row highlights PCSX2, its "PS2 only"/"Read-only"
    badges appear below the tabs), and the "Selected system workflow"
    section below now shows PCSX2's stages instead of RetroArch's.
20. Set a RetroArch profile and cheat source, switch to PCSX2, then
    switch back to RetroArch. Confirm the previously chosen profile and
    source are still selected - nothing was reset by the round trip.
21. Confirm "Database and sources" still appears on the page (below
    Recent activity) with Download/Update/Verify fully functional, exactly
    as in the Phase 1/2 checks above.
22. Navigate "Choose a system"'s tabs with the keyboard (Tab to focus,
    Enter/Space to activate). Confirm this works identically to any other
    button in the app - no new or missing keyboard behaviour.
23. Resize the window narrower/wider. Confirm the tab row wraps instead
    of overflowing, and the Overview card's cross-system strip wraps too.

## Test coverage added in Phase 1

- `ui/components.rs`: `technical_details` collapses its body by default;
  `status_strip` renders every item; `failure_summary` shows headline +
  retained note directly but hides detail until expanded, and omits the
  disclosure entirely when there is no detail to show.
- `main.rs`: `unknown_platform_aggregate_headline` singular/plural
  correctness; `unknown_platform_banner_visible` gating; the per-entry
  "Why is this Unknown?" banner appears only when the platform is actually
  unknown (`platform_section_explains_unknown_platform_only_when_it_is_actually_unknown`);
  `catalogue_status_load_needed` covers exactly Sources and Cheats & Mods;
  `handle_catalogue_manager_action`'s Review-then-Confirm sequencing (an
  update must not start on Review alone, only after Confirm) and
  CancelReview clearing state without starting retrieval.

Phase 1 total: 424 tests (414 pre-existing + 10 new) pass; 0 failures.

## Test coverage added in Phase 2

- `ui/components.rs`: `status_rows` renders every label and value;
  `activity_row_header` shows its timestamp only when one is passed, and
  renders whatever is placed in its `trailing` slot.
- `main.rs`: `pcsx2_workflow_states_render_every_row_through_the_shared_status_rows_component`
  and the equivalent Dolphin test - direct regression coverage (calling
  `show_cheats_mods_workflow_states` with each adapter selected) proving
  the `status_rows` migration didn't drop any row for the two adapters
  that previously had no direct test coverage of their workflow-state
  card (only the RetroArch path had a pre-existing test).

Phase 2 total: 429 tests (424 from Phase 1 + 5 new) pass; 0 failures.

---

## Cheats & Mods structure milestone (`sonnet-cheats-mods-structure` branch)

A focused structural redesign of the Cheats & Mods page - not the full
Library-tab IA migration, and not a further round of generic-component
extraction. Same constraints as Phases 1-2: `MainView`, render-function
names (`show_cheats_mods_page`, `show_cheat_emulator_adapter_selector`,
etc.), and test-asserted labels/click-targets all unchanged; no
mount/rollback/journal/cleanup/recovery/detection/execution code touched.

### Old structure

`show_cheats_mods_page` used to render, in order: a summary card (current
archive + readiness badges); the selected adapter's six-row "Workflow
state" card; a collapsed safety/privacy section; a static "Cheats and
emulator patches" heading; then, only with an archive selected: archive
context, shared game identity, shared preview, the adapter picker (three
stacked cards, one per RetroArch/PCSX2/Dolphin option, each with its own
radio button, badges, and description paragraph), and the selected
adapter's own multi-stage body; then an unconditional mods placeholder and
a recent-activity mini card. The RetroArch cheat-database card (added in
Phase 1) rendered after all of that, at the app level, unlabelled as its
own area.

### New structure

Five labelled areas, in this order:

1. **Overview** - the same current-archive summary card, plus a new
   cross-system availability status_strip (`"RetroArch · 2 eligible
   profiles"`, `"PCSX2 · No eligible PCSX2 profile"`, etc.), gated by the
   same `platform_is_ps2`/`platform_is_dolphin` checks the selector below
   uses. Composed entirely from the pre-existing `*_integration_presentation`
   functions - no new detection logic.
2. **Choose a system** - the RetroArch/PCSX2/Dolphin picker, now one card
   containing a `tab_row` of selectable buttons instead of three stacked
   cards; the selected option's badges and description render once,
   below the tabs, instead of every option's description always being
   visible.
3. **Selected system workflow** - the six-row workflow-state card (moved
   here from Overview, since it details the one selected adapter, not a
   cross-system summary), archive context, shared identity, shared
   preview, and the adapter-specific body - all pre-existing calls,
   unchanged, now grouped under one heading instead of the previous
   static "Cheats and emulator patches" label.
4. **Database and sources** - the RetroArch cheat-database card (Phase 1),
   now under its own heading instead of rendering unlabelled.
5. **Recent activity** - unchanged; already reused `activity_row_header`
   from Phase 2.

The mods placeholder card is unchanged and still renders between areas 3
and 5.

### Cards combined or removed

- **Adapter picker: 3 cards → 1.** The RetroArch/PCSX2/Dolphin chooser's
  three one-per-option cards became one card with a `tab_row`. Each
  option's badges/description (e.g. PCSX2's "PS2 only" / "Read-only")
  now show only for the currently selected option rather than for every
  option simultaneously - a real interaction-model change (tabs, not a
  visible-all radio list), which is exactly what "clear selector or
  tab-like control" asked for. Nothing was deleted: every badge and every
  description string still exists, just shown conditionally.
- **Empty-state workflow card removed.** When no archive is selected, the
  six-row "Workflow state" card no longer renders alongside the "Choose
  one archive" card. It previously showed mostly "Not selected"/"No
  archive context" placeholders in every row - now redundant with the
  empty-state card's own five status badges, which already say the same
  thing more clearly. No test depended on the placeholder card's
  presence.

### Which adapter-specific areas deliberately remain separate

- `show_pcsx2_profile_card`, `show_dolphin_profile_card`, and RetroArch's
  Stage 1 profile cards each mix a radio button, a path field, and a
  status badge in the same row - not a pure badge row (`status_strip`) or
  a pure label+badge list (`status_rows`), and forcing them through
  either would mean a parameter-heavy wrapper standing in for genuinely
  different per-adapter fields. This is the same conclusion Phase 2
  reached about these same three functions; this milestone re-examined
  and re-confirmed it, not skipped it.
- `show_pcsx2_workflow` and `show_dolphin_workflow`'s multi-stage bodies
  (profile → inventory → blockers) are structurally similar to each
  other but meaningfully different from RetroArch's (source-mode choice,
  existing-library vs. trusted-catalogue branching) - left as separate
  functions, per the explicit instruction to retain adapter-specific
  rendering helpers where workflows genuinely differ.

### Compatibility decisions

- `show_cheats_mods_page`'s signature is unchanged; both of its direct
  test call sites needed no changes.
- `show_cheat_emulator_adapter_selector`'s name, parameter list, and
  return type (`Option<CheatWorkflowAction>`) are unchanged - only its
  internal rendering (tab_row instead of three cards) changed. Its
  section header text changed from "Emulator adapter" to "Choose a
  system" (no test asserted the old string).
- The "Database and sources" heading was added at the card's existing
  app-level render site rather than by changing `show_cheats_mods_page`'s
  signature to accept the catalogue-manager state it would need to render
  that card itself. This is a deliberate compatibility tradeoff - see
  "known ordering compromise" below.
- `select_cheat_adapter` (the function that actually mutates
  `workflow.adapter` and drops stale per-adapter async state) was not
  touched at all. The new tests in this milestone
  (`per_adapter_profile_selections_survive_a_real_adapter_switch`) add
  coverage of a property it already had - that
  `selected_profile_id`/`selected_pcsx2_profile_id`/`source_mode`/
  `selected_source_id` all survive a switch - rather than changing its
  behaviour.

### Known ordering compromise

"Database and sources" renders after "Recent activity" on the page, not
before, as the target hierarchy lists them. The database card is composed
at the app level because it needs `self.catalogue_manager`,
`self.catalogue_review`, `self.catalogue_retrieval`, and
`self.catalogue_last_result` - state `show_cheats_mods_page`'s pure-render
signature does not carry - while Recent activity is the last thing
`show_cheats_mods_page` itself renders. Fixing the order properly would
mean either passing that catalogue state into `show_cheats_mods_page`
(a signature change) or extracting `show_recent_cheat_activity`'s call
out of it (also a signature change, since `history` would become an
unused parameter) - both trade a two-test, low-risk compatibility
guarantee for a purely cosmetic ordering fix. Left as a documented
limitation rather than forced.

### Remaining duplicated rendering

- The three adapter-specific workflow bodies still each render their own
  "Stage N · ..." progression independently; no cross-adapter "stage"
  abstraction was introduced, consistent with keeping their genuinely
  different state machines separate.
- `ActionFeedback`'s own banner rendering (page-inline feedback messages)
  remains hand-rolled, as noted in the Phase 2 section above - untouched
  by this milestone.

### Remaining blockers before the full Library-tab IA migration

Unchanged from Phase 1/2 - still the same three items (deciding whether
`Health`/`Duplicates`/`LibraryViews` become Library sub-tabs or are
removed as separate `MainView` variants; rewriting the tests that assert
on `SECONDARY_NAVIGATION_DESTINATIONS`'s exact contents; deciding what
happens to `main_view_content_width`/`main_view_uses_page_scroll`). This
milestone did not touch navigation code and did not add to or reduce
this list.

One new, relevant fact for that future work: `tab_row` (introduced here)
is now available and already proven in production use, so the IA
migration's tab bar (if it goes that route for Health/Duplicates/Library
Views) has a ready-made, tested component rather than needing to invent
one from scratch.

### Recommended next milestone

**The Library-tab IA migration is now the recommended next milestone.**
The two safe-incremental component/structure passes this branch and its
predecessors set out to do (shared components in Phase 1-2, Cheats & Mods
structure here) are essentially complete for the areas identified in the
original audit; further passes in the same style would mostly be
diminishing-returns polish. The IA migration is the one remaining
substantial piece of the original request that was explicitly deferred
pending its own dedicated review, and `tab_row` existing now removes one
of its previous open questions (what the tab control would look like).

## Test coverage added in the Cheats & Mods structure milestone

- `ui/components.rs`: `tab_row` renders every option's label.
- `main.rs`: `choose_a_system_tabs_are_reachable_via_a_real_click` -
  a real pointer-event click (not just a function call) on the PCSX2 tab
  returns the correct `SelectAdapter` action, mirroring the existing
  primary-navigation real-click test's approach;
  `per_adapter_profile_selections_survive_a_real_adapter_switch` - each
  adapter's own profile/source selections remain intact after switching
  away and back; `overview_lists_availability_only_for_applicable_systems` -
  the new cross-system strip shows RetroArch alone for a PS3 archive and
  RetroArch+PCSX2 for a PS2 archive, never fabricating Dolphin
  availability; `cheats_mods_page_renders_the_new_hierarchy_headings` -
  "Overview", "Choose a system", and "Selected system workflow" all
  render, alongside pre-existing content ("Emulator profile", "Recent
  related activity"), proving the reorganisation didn't drop anything.

Cheats & Mods structure milestone total: 434 tests (429 from Phase 2 +
5 new) pass; 0 failures.

---

## Library IA migration - Phase 1 (`sonnet-library-tabs-foundation` branch)

The Library-tab IA migration that every previous section deferred,
finally begun - but only its foundation. This phase adds new state and
routing logic; it does **not** touch the sidebar, remove any `MainView`
variant, merge any visible page, or rename any render function. Nothing
a user does today looks any different.

### Audit findings

The four Library-related `MainView` variants (`Library`, `Health`,
`Duplicates`, `LibraryViews`) are reached through:

- **The sidebar**, generically: `show_primary_navigation` returns a
  clicked `MainView`, and the single handler at the top of `update`
  (`if let Some(clicked) = navigation_request { self.view = clicked; ... }`)
  sets `self.view` directly - this one path covers all four.
- **~11 scattered direct assignments** elsewhere in `update`, each
  reached from a different action enum's dispatch: `SourcesPageAction::
  ViewInLibrary`, `DuplicateReviewAction::Close`/`ViewInLibrary`,
  `HealthDashboardAction::BackToLibrary`/`OpenMissingReview`/
  `OpenDuplicateReview`/`ViewInLibrary`, `CheatWorkflowAction::
  OpenLibrary`, `ActiveMountsPageAction::OpenInLibrary`,
  `AppOperationRequest::ShowInLibraryViews`, and the activity panel's
  "show related archive". None of these call sites know about each
  other or share any routing logic today.
- **Four independent render functions**, each already large and
  independently tested: `show_loaded_data` (Library/RecentlyFound,
  ~1,000 lines), `show_health_dashboard_panel`, `show_duplicate_review_panel`,
  `show_library_views_page` - dispatched from four separate `if self.view
  == MainView::X { ...; return; }` blocks in `update`'s central-panel
  closure, in this order: Sources, LibraryViews, Duplicates, Health,
  CheatsMods, Mount, Selected, ActiveMounts, Doctor, HistoryLogs,
  Settings, About, with Library/RecentlyFound falling through to the
  bottom (`show_loaded_data`).
- **Per-view rendering-policy tables** that already treat these four
  destinations similarly but not identically: `main_view_content_width`
  puts all four at `Wide`; `main_view_uses_page_scroll` is `false` for
  Library/Health/Duplicates (they manage their own internal scroll
  areas, e.g. the archive table) but `true` for `LibraryViews` (it uses
  the shared page-level scroll wrapper) - a real, load-bearing difference
  that any future visual unification will need to reconcile, not an
  oversight to "fix" in passing.
- **No persisted navigation state.** `ArchiveFsApp` is not serialized
  between runs (no `eframe::set_value`/`get_value` or equivalent); `view`
  always starts at its `#[default]` variant (`Library`) each launch. There
  is nothing to migrate on disk.
- **Test coupling**: 52+ existing tests reference Health/Duplicates/
  LibraryViews by name, plus the navigation-destination tests
  (`every_navigation_destination_has_a_title_and_width_policy`,
  `all_navigation_destinations_are_reachable_via_a_real_click`, the
  `primary_nav_rects` mirror) that iterate
  `SECONDARY_NAVIGATION_DESTINATIONS`'s exact three entries. None of this
  was touched - this phase adds only new, purely additive state and
  functions.
- **Keyboard paths** (arrow-key row navigation, etc.) are scoped to the
  Library page's own archive table (`show_archive_rows`) and were not
  found duplicated on the Health/Duplicates/LibraryViews pages - nothing
  to reconcile for those in this phase either.

### What was added

- **`LibraryTab`** - a new enum (`Archives`, `Health`, `Duplicates`,
  `Views`) representing the four lenses this migration will eventually
  unify into one tabbed page. `Archives` maps to `MainView::Library`;
  `Views` maps to `MainView::LibraryViews` (renamed in this enum only,
  since "Library Views" would read as "Library" twice once tabs exist).
- **One authoritative field**, `ArchiveFsApp::library_tab: LibraryTab`,
  added alongside the existing `view: MainView` field.
- **The synchronization rule** (documented on `LibraryTab` itself):
  `view` remains the single source of truth for what renders, completely
  unchanged. `library_tab` is a *read-only, derived* projection of it:
  once per frame (`ArchiveFsApp::reconcile_library_tab`, called first
  thing in `update`), if `view` is one of the four Library destinations,
  `library_tab` is set to match; otherwise `library_tab` is left alone,
  so it remembers the last Library tab across unrelated navigation. This
  makes every existing legacy route - the sidebar click and all ~11
  scattered direct assignments - a correct route into the right
  `LibraryTab`, automatically, with zero of those call sites modified or
  even aware `LibraryTab` exists.
- **Three pure mapping functions**: `library_tab_for_main_view` (the
  routing table above, `None` for every non-Library destination
  including `RecentlyFound`), `main_view_for_library_tab` (its inverse),
  and `library_tab_label` (the one shared source of truth for tab display
  text Phase 2's tab UI will use - reusing the same naming precedent as
  `main_view_title`).
- **`ArchiveFsApp::navigate_to_library_tab(&mut self, tab)`** - the one
  sanctioned way to navigate *by tab* (sets `view` and `library_tab`
  together, clears `tools_overlay` like every other navigation call
  site). Not called from any production UI yet; it is the compatibility
  wrapper Phase 2's tab control will call.

`main_view_for_library_tab`, `library_tab_label`, and
`navigate_to_library_tab` currently carry `#[allow(dead_code)]`: they are
deliberately-unwired-yet foundation code, exercised by this milestone's
tests but not yet called from any production render path. This is an
intentional, documented state, not an oversight - see "Phase 2 work"
below for when they get wired in.

### Compatibility mapping

| `MainView` | `LibraryTab` |
|---|---|
| `Library` | `Archives` |
| `Health` | `Health` |
| `Duplicates` | `Duplicates` |
| `LibraryViews` | `Views` |
| everything else (including `RecentlyFound`) | *(none)* |

### Functions/variants retained exactly as-is

`MainView` (all 14 variants), `show_primary_navigation`,
`PRIMARY_NAVIGATION_DESTINATIONS`, `SECONDARY_NAVIGATION_DESTINATIONS`,
`navigation_destination_enabled`, `navigation_destination_selected`,
`main_view_title`, `main_view_content_width`,
`main_view_uses_page_scroll`, and all four render functions
(`show_loaded_data`, `show_health_dashboard_panel`,
`show_duplicate_review_panel`, `show_library_views_page`) - none renamed,
none restructured, none of their call sites changed.

### Extracting shared tab-content renderers - scoped down, and why

The milestone brief asked for shared tab-content renderers "where
useful." The four render functions above don't share render-worthy
structure at the content level - they render an archive table, a health
issue list, duplicate groups, and saved-view definitions respectively,
each with its own data model, filters, and actions. Forcing a shared
renderer across them would mean either a parameter-heavy dispatcher
bundling all four functions' (collectively 30+) parameters, or actually
merging their bodies - real, substantial, high-risk work touching
1,000+ lines of already-tested code, and exactly the kind of change this
foundation-only phase was scoped to avoid ("do not... change visible
navigation structure" and "keep existing render functions... unchanged").
What *is* shared and genuinely reusable - the routing/label logic - was
extracted (see above). The actual page-body unification is Phase 2's
job, described next.

### Phase 2 work still required

1. **Design and build the actual tab UI** - almost certainly reusing
   `tab_row` (introduced in the Cheats & Mods structure milestone) for
   the four `LibraryTab` options, wired to
   `ArchiveFsApp::navigate_to_library_tab`.
2. **Decide the single-page dispatch shape**: does one new `MainView`
   variant (e.g. `MainView::LibraryArea`) replace all four, with
   `library_tab` deciding which content renders inside it? Or do the four
   `MainView` variants stay and the tab bar becomes a purely visual
   overlay that still ends up calling `navigate_to_library_tab`,
   preserving `MainView` unchanged a while longer? The former is the
   "real" migration; the latter is lower-risk and could be an
   intermediate step.
3. **Reconcile `main_view_uses_page_scroll`'s Library-vs-LibraryViews
   difference** noted in the audit above - once all four render inside
   one page, they need one scroll-ownership answer, not three false and
   one true.
4. **Rewrite the navigation-destination tests** that assert on
   `SECONDARY_NAVIGATION_DESTINATIONS`'s exact three entries
   (`every_navigation_destination_has_a_title_and_width_policy`,
   `all_navigation_destinations_are_reachable_via_a_real_click`, the
   `primary_nav_rects` mirror) once the sidebar actually changes -
   unchanged and passing today because nothing about the sidebar changed
   yet.
5. **Decide whether `MainView::Health`/`Duplicates`/`LibraryViews`
   survive as enum variants** (e.g. only used internally for what
   `library_tab` used to derive from) or are removed once `library_tab`
   is the only thing driving tab content - removing them will touch
   every one of the ~11 legacy call sites this phase deliberately left
   alone, at which point they should be migrated to
   `navigate_to_library_tab` directly.
6. **Only then** consider whether genuinely shared rendering exists
   across the four content bodies (e.g. a common filter-bar shape, a
   common empty-state) worth extracting - premature before the page
   structure around them is settled.

## Test coverage added in the Library IA migration Phase 1

- `main.rs`: `library_tab_for_main_view_covers_exactly_the_four_library_destinations`
  and its inverse-mapping counterpart
  (`main_view_for_library_tab_round_trips_with_library_tab_for_main_view`)
  - the routing table is exhaustive and correct in both directions, and
  no non-Library destination (explicitly including `RecentlyFound`) is
  ever misrouted; `library_tab_label_is_distinct_and_non_empty_for_every_tab`;
  `legacy_routes_reconcile_to_the_correct_library_tab` - simulates all
  four legacy routes by setting `view` directly (as every real call site
  does) and confirms `reconcile_library_tab` selects the right tab each
  time; `library_tab_survives_navigating_away_to_an_unrelated_destination`
  - visiting Settings/Mount/CheatsMods/About after Health never resets
  the remembered tab; `navigate_to_library_tab_sets_view_and_library_tab_together`
  - the write-direction wrapper keeps both fields in agreement and clears
  `tools_overlay`; `selected_archive_and_filters_survive_a_library_tab_switch`
  - the selected archive, the Library free-text filter, `library_filters`,
  `health_filters`, and `duplicate_filters` are all still exactly as set
  after cycling through all four tabs via `navigate_to_library_tab`.

Library IA migration Phase 1 total: 441 tests (434 from the Cheats & Mods
structure milestone + 7 new) pass; 0 failures.

---

## Library IA migration - Phase 2 (`sonnet-library-tabs-visible-ui` branch)

The visible half of the migration Phase 1 laid the foundation for: one
Library page, four tabs, one sidebar destination. `MainView` keeps all 14
variants; the four underlying render functions are completely unchanged.

### Old navigation

Sidebar: a "WORKFLOWS" section (11 destinations, including "Library") and
a separate "LIBRARY TOOLS" section with three more buttons ("Health",
"Duplicates", "Library Views") - four Library-related sidebar entries
total, each opening its own bare page with its own heading and, on
Health/Duplicates, a "Back to Library" button.

### New navigation

Sidebar: "WORKFLOWS" only, 11 destinations, same as before minus nothing
- "Library" was already one of them. The "LIBRARY TOOLS" section and its
three buttons are gone entirely. Clicking "Library" opens one page: a
"Library" heading, a four-tab row (Archives/Health/Duplicates/Views), and
the selected tab's content below. The Library button renders selected
while on *any* of the four tabs, and clicking it restores whichever tab
was last selected rather than always resetting to Archives.

### Unified Library shell structure

`ArchiveFsApp::update`'s central-panel dispatch now has one block, guarded
by `library_tab_for_main_view(self.view).is_some()`, replacing what used
to be three separate `if self.view == MainView::X { ...; return; }`
blocks (Health, Duplicates, LibraryViews) plus an implicit fallthrough for
Library. That block:

1. Calls `show_library_shell_header(ui, self.library_tab)` - a new,
   directly-testable function rendering the shared "Library" heading and
   the `tab_row`-based tab selector, returning the clicked tab (if any).
   A click calls `navigate_to_library_tab`.
2. `match self.library_tab { ... }`: the `Archives` arm is empty (falls
   through, unreturned, to the existing `match &self.state { Loading |
   Error | Ready(data) => show_loaded_data(...) }` block below - shared
   with `MainView::RecentlyFound`, completely unchanged); the `Health`,
   `Duplicates`, and `Views` arms contain the exact bodies their old
   standalone `if` blocks used to (same function calls, same arguments,
   same action handling), each ending in `return`.

`show_library_shell_header` was deliberately kept small - just the
heading and tab row - rather than also taking on content dispatch: each
tab's existing renderer needs direct `&mut self` field access
(`self.health_filters`, `self.duplicate_filters`, `self.library_views`,
`self.database_state`, ...) that a standalone function would have to take
as a dozen-plus parameters, exactly the "giant parameter-heavy universal
renderer" this milestone was told to avoid. The four existing render
functions (`show_loaded_data`, `show_health_dashboard_panel`,
`show_duplicate_review_panel`, `show_library_views_page`) were not
touched at all - not renamed, not restructured, not given new
parameters. They are the compatibility layer this phase preserves,
exactly as Phase 1 anticipated ("They may become wrappers around shared
tab-body renderers" - in practice, no wrapper functions were needed;
their existing call sites just moved from four `if` blocks into one
`match`'s arms, verbatim).

**One acknowledged minor redundancy**: on the Archives tab, the shell's
own "Library" heading is immediately followed by `show_loaded_data`'s own
"Library" `page_header` (unchanged, since `show_loaded_data` is shared
with `RecentlyFound` and its heading logic was not touched). The
Health/Duplicates/Views tabs don't have this problem - their own
headings ("Health Dashboard", "Duplicate Review", "Library Views") differ
from the shell's. Fixing the Archives case would mean either giving
`show_loaded_data` a way to suppress its own heading (a real behaviour
change to a heavily-tested, `RecentlyFound`-shared function) or forking
its call site - both bigger changes than this phase's "retain existing
render functions" scope justified for a cosmetic duplicate heading.
Recorded as a Phase 3 cleanup candidate below, not fixed here.

### Compatibility strategy

Kept exactly as-is: all 14 `MainView` variants (including `Health`,
`Duplicates`, `LibraryViews`); `show_loaded_data`,
`show_health_dashboard_panel`, `show_duplicate_review_panel`,
`show_library_views_page` (names, signatures, and bodies all unchanged);
`HealthDashboardAction`, `DuplicateReviewAction`, `LibraryViewAction` and
their existing handling (except two incidental
`DuplicateReviewAction::Close`/`ViewInLibrary` lines that stayed as plain
`self.view = MainView::Library` assignments rather than being switched to
`navigate_to_library_tab`, deliberately not migrated - see "Phase 3 work"
below); every one of the ~11 legacy `self.view = MainView::X` assignments
across the app (Sources, CheatsMods, ActiveMounts, AppOperationRequest,
the activity panel) - none were touched, and all still correctly land on
the right tab via the unchanged `reconcile_library_tab` from Phase 1.

New: `show_library_shell_header` (the tab chrome); the sidebar click
handler's one new special case for `MainView::Library` (restore the last
tab instead of resetting); `navigation_destination_selected`'s new
any-of-four-Library-views selected-state for the Library button.

### Scrolling solution

All four Library-related destinations now report `false` from
`main_view_uses_page_scroll` - no outer page-level `ScrollArea`. Three
(Library, Health, Duplicates) already managed their own internal,
full-height `ScrollArea` and were already `false`; `LibraryViews` was the
one exception, previously `true`. Auditing `show_library_views_page`
found both of its variable-length lists (the view definitions themselves,
and a selected view's plan-entry details) already sit in their own
bounded `egui::ScrollArea` (`max_height` 320.0 and 240.0 respectively);
everything else on the page - heading, "Add View" button, the Add/Edit/
Remove dialogs (separate `egui::Window`s with their own scrolling) - is
short, fixed-height content that was never actually at risk of
overflowing. Flipping `LibraryViews` to `false` was therefore the
smallest safe change: it makes all four tabs share one scrolling model
(the tab row is always pinned above whichever scroll area, if any, a tab
owns; there is never a second, outer scroll area fighting an inner one),
and it does not clip or hide any content that used to be reachable via
the outer page scroll, because nothing on that page actually depended on
it. Documented on `main_view_uses_page_scroll`'s doc comment in the code,
which is the authoritative source; this section summarizes it.

### Tests converted from structural to behavioural checks

- `every_navigation_destination_has_a_title_and_width_policy` no longer
  chains the removed `SECONDARY_NAVIGATION_DESTINATIONS` const (deleted);
  a new `legacy_library_destinations_still_have_a_title_and_width_policy`
  test asserts the same property directly for `Health`/`Duplicates`/
  `LibraryViews`, now that they aren't in any navigation const to iterate.
- `primary_nav_rects` (the geometry-discovery test mirror) and
  `all_navigation_destinations_are_reachable_via_a_real_click` both
  dropped their `SECONDARY_NAVIGATION_DESTINATIONS` iteration - the
  mirror now matches `show_primary_navigation`'s actual single-section
  layout, and the click-reachability test now iterates exactly the
  destinations that are still sidebar buttons.
- `long_content_pages_use_shared_scrolling_without_changing_table_pages`
  updated to assert the new scrolling rule (all four Library-related
  destinations use internal scrolling) instead of the old one
  (`LibraryViews` used page scroll).
- New, not conversions: `library_is_the_only_sidebar_destination_for_the_library_area`
  and `library_sidebar_button_is_selected_on_every_library_tab` directly
  assert the consolidated sidebar's shape and selected-state behaviour -
  the behavioural properties the old structural tests' literal 3-entry
  iteration used to only incidentally cover.

### Remaining risks / limitations

- The Archives-tab duplicate "Library" heading (see above) - cosmetic,
  not a functional bug, deferred.
- `show_library_shell_header` is directly tested; the `match
  self.library_tab { ... }` *content dispatch* inside `update` is not,
  because nothing in this codebase unit-tests the real
  `eframe::App::update` (it requires a full `eframe::Frame`, which
  nothing here constructs even for unrelated views). Confidence in the
  dispatch itself comes from: it calls the exact same functions with the
  exact same arguments the old standalone blocks did (a mechanical,
  reviewable move, not new logic); Phase 1's routing tests
  (`legacy_routes_reconcile_to_the_correct_library_tab`, etc.) prove
  `self.library_tab` ends up correct for every legacy route; and the
  four underlying render functions retain their own full existing test
  coverage, unchanged. This is a pre-existing gap in this codebase's
  testing conventions (the whole `update` method has always been
  untested this way), not one introduced by this milestone.
- PCSX2/Dolphin, mount/unmount, rollback, journal, cleanup, detection,
  and database code were not touched - out of scope, and not audited
  further here since nothing in this phase's diff touches those files.

### Phase 3 work still required

1. **Fix the Archives-tab duplicate heading** - give `show_loaded_data`
   a way to suppress its own "Library" `page_header` when called from
   the unified shell (a small, real change to a function this phase
   deliberately left untouched).
2. **Migrate the ~11 legacy `self.view = MainView::X` call sites** to
   `navigate_to_library_tab` where they target a Library destination,
   including the two `DuplicateReviewAction` arms this phase touched
   but deliberately left as plain `view` assignments. Not required for
   correctness (reconciliation already makes them work), but would make
   `library_tab` the single place Library navigation is expressed
   instead of two equivalent-but-different call patterns coexisting.
3. **Decide whether `MainView::Health`/`Duplicates`/`LibraryViews` should
   be removed as enum variants** once every call site that used to set
   them directly instead calls `navigate_to_library_tab` - only sensible
   after (2).
4. **Reconsider the per-tab "Back to Library" buttons** inside
   `show_health_dashboard_panel`/`show_duplicate_review_panel` - they
   still work (they set `view = MainView::Library`, which the shell
   correctly opens on the Archives tab), but are partially redundant now
   that a one-click tab switch does the same thing. Not broken, just a
   candidate for simplification once the page bodies are next touched
   for an unrelated reason.
5. **Only then**, as Phase 1 already noted, consider whether genuinely
   shared content-rendering code exists across the four tab bodies worth
   extracting - still premature until (1)-(4) settle the page's final
   shape.

## Test coverage added in the Library IA migration Phase 2

- `main.rs`: `views_configuration_survives_a_library_tab_switch` -
  `library_views` and `library_view_plan_filter` survive cycling through
  every tab, extending Phase 1's equivalent test (which covered
  Archives/Health/Duplicates state) to Views;
  `library_shell_header_shows_all_four_tab_labels`;
  `library_shell_header_tabs_are_reachable_via_a_real_click` - a real
  pointer-event click (not just a function call) on each of the four tab
  labels returns that exact tab, mirroring the click-test pattern used
  for primary navigation and the Cheats & Mods adapter selector;
  `library_shell_header_marks_the_current_tab_selected`;
  `library_is_the_only_sidebar_destination_for_the_library_area` -
  `PRIMARY_NAVIGATION_DESTINATIONS` contains exactly one Library-related
  entry and none of Health/Duplicates/LibraryViews;
  `library_sidebar_button_is_selected_on_every_library_tab`;
  `clicking_library_in_the_sidebar_restores_the_last_selected_tab`;
  `legacy_library_destinations_still_have_a_title_and_width_policy`
  (structural-test replacement, see above).

Library IA migration Phase 2 total: 449 tests (441 from Phase 1 + 8 new)
pass; 0 failures.

---

## Library IA migration - Phase 3 (`sonnet-library-tabs-phase-3-cleanup` branch)

Cleanup of the compatibility rough edges Phase 2 deliberately left open,
plus an evidence-backed decision on the legacy `MainView` variants. No
`MainView` variant removed, no sidebar structure changed, no render
function renamed.

### How the duplicate heading was fixed

Audited every production caller of `show_loaded_data` with
`recent_view == false`: only one remained (the unified shell's Archives
arm, since Phase 2 routed everything else through it), and no test
asserted the "Library" heading text it printed. Removed exactly that
`widgets::page_header(ui, "Library", ...)` call, replacing it with just
its description line (kept as a plain muted label - it says something
the shell's own, more general four-tab description doesn't) plus the
same spacing. `recent_view == true` (Recently Found) is a separate `if`
and was not touched. No extraction, no new rendering mode, no new
parameter on `show_loaded_data` or `LoadedViewState` - the smallest
change that fixed it.

### Direct navigation assignments migrated

All 11 remaining direct `self.view = MainView::X` assignments to the
four Library-related destinations were migrated to
`navigate_to_library_tab`. None were retained as plain assignments -
the audit found no test exercising the real `update()` dispatch (this
codebase never calls `eframe::App::update` directly in a test), so
nothing depended on the raw-assignment form specifically, and
`navigate_to_library_tab` is a strict superset of what each assignment
did (same `view`, immediate correct `library_tab` instead of waiting a
frame for `reconcile_library_tab`, and a `tools_overlay` clear that was
already a no-op at every one of these eleven sites).

Target tab by site (every other field write at each site - selected
archive, `library_source_filter`, `library_filters.missing`,
`library_view_focus_archive` - preserved exactly as it was):

| Site | Target tab |
|---|---|
| `ActivityPanelAction::ShowRelatedArchive` | Archives |
| `SourcesPageAction::ViewInLibrary` | Archives |
| `CheatWorkflowAction::OpenLibrary` | Archives |
| `ActiveMountsPageAction::OpenInLibrary` | Archives |
| `DuplicateReviewAction::Close` | Archives |
| `DuplicateReviewAction::ViewInLibrary` | Archives |
| `HealthDashboardAction::BackToLibrary` | Archives |
| `HealthDashboardAction::OpenMissingReview` | Archives |
| `HealthDashboardAction::ViewInLibrary` | Archives |
| `HealthDashboardAction::OpenDuplicateReview` | **Duplicates** |
| `AppOperationRequest::ShowInLibraryViews` | **Views** |

Also updated `health_context_menu_show_in_library_resolves_the_exact_
archive`, a test whose own comment says it intentionally mirrors this
exact dispatch line rather than calling it, to mirror the new
`navigate_to_library_tab` call and assert `library_tab` too.

### "Back to Library" control decisions

| Control | Decision | Why |
|---|---|---|
| `show_duplicate_review_panel`'s "Back to Library" (`DuplicateReviewAction::Close`) | **Kept**, handler migrated to `navigate_to_library_tab(Archives)` | Sits above the same internal scroll area the tab row is pinned outside of - not meaningfully redundant with the tab row; removing a working exit action for a small convenience gain wasn't worth the risk. |
| `show_health_dashboard_panel`'s "Back to Library" (`HealthDashboardAction::BackToLibrary`) | **Kept**, same migration | Same reasoning. |
| Cheats & Mods' "Open Library" (`CheatWorkflowAction::OpenLibrary`) | **Kept**, handler migrated to `navigate_to_library_tab(Archives)` | Distinct workflow meaning (leave Cheats & Mods to go pick an archive) - not a Library-internal control at all. |
| `show_tools_overlay_header`'s "Back to Library" (used by every Tools overlay screen without its own dismiss action) | **Left completely unchanged** | Despite the label, it only clears `self.tools_overlay` and returns to whatever page was already active - not necessarily Library (Tools overlays open from any page). This is a pre-existing label inaccuracy that predates the Library shell; changing its actual target to navigate to Library would be a real, unrelated behaviour change for anyone who opened a Tools overlay from a non-Library page. Documented on the function rather than fixed, since it's out of this migration's scope. |

No "Back to Library"-style control was removed. None were found to be
fully redundant given the button-vs-tab-row position analysis above.

### Legacy `MainView` variant status

**Decision: kept `Health`, `Duplicates`, and `LibraryViews` as real enum
variants this phase** - the default the milestone brief specified,
applied because removal was not clearly safe, small, and test-backed:

- **Production code still needs them.** `library_tab_for_main_view`,
  `main_view_title`, `main_view_content_width`, and
  `main_view_uses_page_scroll` all pattern-match every `MainView`
  variant exhaustively; the shell's content dispatch is keyed off
  `self.view` matching them via that mapping.
- **50+ tests across three milestones** construct or compare against
  these three variants directly (confirmed by grep across the Phase 1,
  2, and 3 branches' test additions plus the pre-existing suite).
- **No external, persisted, or CLI dependency found.** `ArchiveFsApp` is
  never serialized between runs (`view` always starts at its
  `#[default]`, `Library`, each launch); the CLI is a separate binary
  with no awareness of the GUI's `MainView` enum. This is the one thing
  that would *not* block removal - the blockers are entirely internal
  (production dispatch + test surface).
- **Removing them would not simplify synchronization further** at this
  point - `LibraryTab` already is the single tab-identity source of
  truth for anything that reads it (nothing reads raw `MainView::Health`
  and treats it differently from `LibraryTab::Health`); removing the
  `MainView` variants would just relocate the same information, not
  eliminate a real duplication.

Reduced to documented compatibility aliases as instructed: `MainView`
now carries a doc comment explaining exactly this (production dispatch
keys + test surface, no external dependency), and each of the three
variants' names make clear they exist to be routed through
`library_tab_for_main_view`, not as independent destinations.

**No removal proposed.** If a future milestone wants to reconsider this,
the exact remaining work is listed under "Phase 4" below.

### Compatibility code removed or retained

- **Removed:** nothing needed removing - the previous milestone already
  resolved every `#[allow(dead_code)]` introduced during the foundation
  phase (confirmed: none remain in the codebase).
- **Retained, now documented:** `navigation_destination_enabled`'s
  `MainView::Health | MainView::Duplicates` match arms are unreachable
  through any live sidebar call site today (neither appears in
  `PRIMARY_NAVIGATION_DESTINATIONS` any more), but were not pruned - the
  same database-readiness gate is the correct one to reuse if the tab
  row ever needs to grey out those two tabs before a scan completes.
  Documented as "kept ready," not "kept out of caution," on the function
  itself.
- **No wrapper functions were introduced or removed** - `show_loaded_data`,
  `show_health_dashboard_panel`, `show_duplicate_review_panel`, and
  `show_library_views_page` remain exactly as Phase 2 left them (only
  the one heading line inside `show_loaded_data` changed in this phase).

### Remaining limitations

- The three legacy `MainView` variants remain a form of duplication with
  `LibraryTab` - documented and justified, not eliminated.
- `show_tools_overlay_header`'s "Back to Library" label remains slightly
  inaccurate for the non-Library case, deliberately left alone as
  out-of-scope for this migration.
- As in Phase 2: the `match self.library_tab { ... }` content dispatch
  inside `update()` (including the eleven now-migrated
  `navigate_to_library_tab` call sites) is not directly unit-tested,
  because nothing in this codebase unit-tests `eframe::App::update`
  itself. Confidence comes from mirrored dispatch tests (this phase
  added three: `migrated_navigation_sites_preserve_their_own_side_
  effects_alongside_the_tab_switch`, `back_to_library_actions_land_on_
  the_archives_tab_from_any_starting_tab`, plus the updated
  `health_context_menu_show_in_library_resolves_the_exact_archive`) that
  exercise the exact same statements the real dispatch runs, plus full
  coverage of `navigate_to_library_tab` and `reconcile_library_tab`
  themselves from Phase 1.

### Exact work remaining before legacy MainView variants could be removed

1. Confirm (or re-confirm, since test counts will have grown) that no
   test still needs to construct/compare `MainView::Health`/
   `Duplicates`/`LibraryViews` directly - likely requires updating
   50+ tests to use `LibraryTab` instead, or introducing test-only
   helper constructors that hide the distinction.
2. Decide what `self.view`'s type becomes once it no longer needs to
   name these three specifically - either `self.view` stops being the
   single source of truth for Library tab content (and `library_tab`
   takes over that role fully, with `view` only tracking "am I in the
   Library area at all"), or some other restructuring.
3. Update `main_view_title`/`main_view_content_width`/
   `main_view_uses_page_scroll`/`library_tab_for_main_view` accordingly
   once the variants are gone.
4. Re-run the full navigation and Library test suite and expect
   significant, not incremental, churn - this is explicitly *not* the
   "small, evidence-backed" removal this phase's default-to-keep
   instruction requires; it would be its own milestone.

### Recommended next milestone

No further Library-tabs cleanup is urgent - the visible shell, its
navigation, and its compatibility layer are all in a stable, documented
state. The two open threads are, in priority order:

1. **A different area of the GUI entirely** - the Library IA migration
   (Phases 1-3) has reached a natural stopping point; further polish
   here has diminishing returns relative to auditing other pages for
   the same kind of duplication this migration found in Library.
2. **If Library work continues**, the honest next step is not "remove
   the legacy MainView variants" (see above - not small) but revisiting
   whether the per-tab-body content (archive table, health list,
   duplicate groups, saved views) has any genuinely shared rendering
   worth extracting now that the page's final shape (heading + tab row +
   one dispatch) has been stable across two full phases - the question
   Phase 1 and Phase 2 both deferred as "premature."

## Test coverage added in the Library IA migration Phase 3

- `main.rs`: `count_exact_text_occurrences` (new test helper, the
  exact-match counting counterpart of the existing
  `rendered_text_contains`/`find_exact_text_center`) -
  `archives_tab_shows_exactly_one_library_heading` uses it to confirm
  the shell contributes exactly one "Library" heading and
  `show_loaded_data`'s Archives-tab body contributes zero;
  `recently_found_still_shows_its_own_heading_with_no_library_duplicate`
  confirms Recently Found's own heading is intact and it never renders a
  bare "Library" heading; `migrated_navigation_sites_preserve_their_own_
  side_effects_alongside_the_tab_switch` mirrors three of the newly
  migrated dispatch sites (OpenMissingReview, ShowInLibraryViews,
  OpenDuplicateReview) and confirms both the correct destination tab and
  each site's own side-effect field survive together;
  `back_to_library_actions_land_on_the_archives_tab_from_any_starting_
  tab` confirms a Back-to-Library action reaches Archives regardless of
  which tab it was invoked from; `health_context_menu_show_in_library_
  resolves_the_exact_archive` updated to mirror the migrated dispatch
  line and assert `library_tab` too (see above).

Library IA migration Phase 3 total: 453 tests (449 from Phase 2 + 4 new)
pass; 0 failures.
