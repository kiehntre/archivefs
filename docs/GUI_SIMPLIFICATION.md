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
duplication" below.

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

## Remaining duplication (known, deliberately deferred)

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

## Test coverage added in this pass

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

All 424 tests (414 pre-existing + 10 new) pass; 0 failures.
