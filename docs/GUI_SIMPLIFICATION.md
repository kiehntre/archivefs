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
