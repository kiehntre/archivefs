# ArchiveFS GUI Final Polish Report

Branch: `sonnet-final-gui-polish`, based on `main` at
`7da26e83038f82f36e3fdc0660f4b4e6e8e6f332` (which already includes the
four Sources page cleanup commits, fast-forwarded in before this pass
began). No merge or push has been performed from this branch.

This report covers only the final polish pass. For the detailed
per-milestone history (Phase 1/2, Cheats & Mods structure, Library IA
migration Phases 1-3, Sources page cleanup), see
`docs/GUI_SIMPLIFICATION.md`.

## Before/after summary by page

| Page | Before | After |
|---|---|---|
| Library - Health tab | Raw `ui.heading("Health Dashboard")`, wording didn't match its own "Health" tab-bar label | `widgets::section_header(ui, "Health", ...)`; back button uses shared `action_button` styling |
| Library - Duplicates tab | Raw `ui.heading("Duplicate Review")`, same mismatch | `widgets::section_header(ui, "Duplicates", ...)`; back button restyled |
| Library - Views tab | Raw `ui.heading("Library Views")`, same mismatch, no back button | `widgets::section_header(ui, "Views", ...)`; still no back button (no `LibraryViewAction::Close` variant exists to wire one to) |
| Mount / Selected | Selected page's queue button read "Mount ready archives (N)"; Mount page's read "Mount queue (N)" for the identical action | Both read "Mount queue (N)" |
| Active Mounts | No "Recent activity" section, unlike Sources/Cheats & Mods | New `show_active_mounts_recent_activity`, same shared-component pattern as the other two pages |
| Sources | Already Overview / Configured sources / Database and catalogue management / Recent activity (from the prior Sources cleanup milestone) | Same structure; only the database section heading renamed to "Database and sources" to match Cheats & Mods |
| Cheats & Mods | "Choose a system" and "Mods" already had their own headings (audit initially miscounted this); no-archive-selected state showed two different button labels ("Choose an archive" vs "Choose archive") for the same action | Both buttons now read "Choose archive" |
| History & Logs | Rollback card had no heading above it, unlike the two sections below it; "Session activity" heading had no description | New "Recovery" `section_header` above the rollback card; "Session activity" now has a description line |
| Cheats & Mods (PCSX2/RetroArch profile cards) | "All technical blockers" / "All technical blockers (N)" as bespoke raw `CollapsingHeader`s | Migrated to the shared `widgets::technical_details`, first blocker still always visible directly |
| Mount, Selected, Active Mounts, History & Logs, Settings, Doctor, About, Recently Found | - | Reviewed against the full polish checklist; no further change found necessary |

## Screenshots still worth capturing manually

None were taken as part of this pass - this is a text-only agent session
with no ability to render or capture the actual application window. A
human reviewer running the built binary should capture before/after
screenshots of at minimum: the Library shell switching between all four
tabs (to visually confirm the heading consistency fix), the Mount and
Selected pages' queue buttons side by side, Active Mounts' new Recent
activity section, History & Logs' new "Recovery" heading, and a PCSX2 or
RetroArch profile card with 2+ blockers (to see the new "Technical
details" disclosure in place of the old bespoke wording).

## Remaining cosmetic issues

- Dialog window titles mix question form ("Use Lazy Unmount?") and
  statement form ("Confirm unmount"); the batch-dialog button family
  (Mount All / Unmount All / Lazy Unmount / Confirm Lazy Unmount / Try
  Normal Unmount Again / Remove Source) is Title Case where the rest of
  the app is sentence case. Both are internally consistent within their
  own family; left unchanged this pass because nearly every one of these
  strings is exercised by an existing confirmation test, and renaming
  them purely for casing is a real regression risk for no functional
  gain.
- Settings' numbered section headings ("1." through "5.") are the only
  numbered headings in the app. Judged intentional/helpful for that
  page's checklist-like structure rather than an inconsistency worth
  removing.
- The Library "Selected archive" panel's identity/platform/metadata is
  one dense `egui::Grid`, a different density choice than the
  several-cards pattern used on most other pages. Left as-is; it is a
  working, well-tested panel and restructuring it for density
  consistency alone was judged not worth the risk this pass.

## Remaining architectural debt

- The three legacy `MainView` compatibility variants (`Health`,
  `Duplicates`, `LibraryViews`) documented in the Library IA migration
  Phase 3 section of `docs/GUI_SIMPLIFICATION.md` remain in place,
  exactly as that milestone's brief required ("do not touch legacy
  Library MainView compatibility variants in this pass" applied here
  too).
- `show_tools_overlay_header`'s "Back to Library" button still doesn't
  always navigate to Library (a pre-existing, previously documented
  inaccuracy - see that same Phase 3 section). Not touched this pass,
  since fixing its actual target would be a real behaviour change, not
  polish.
- Active Mounts has no lazy-unmount entry point of its own; that action
  still only exists on the Library "Selected archive" panel. This is a
  gap in feature parity between the two mount-related surfaces, not
  something this polish pass introduced or was in scope to fix (adding
  a second entry point to an existing destructive-adjacent workflow is
  new UI surface, not polish).

## Areas deliberately not changed

See "Pages deliberately unchanged" and "Remaining intentional
inconsistencies" in `docs/GUI_SIMPLIFICATION.md`'s Final GUI polish
section for the full list and reasoning. In summary: Mount, Selected,
Active Mounts' core layout, History & Logs' filter/export card, Settings,
Doctor, About, and Recently Found were all reviewed and found to already
meet the milestone's hierarchy/spacing/terminology/action-placement/
status-visibility/error-presentation/empty-state/card-density/
navigation/scrolling checklist - no change was forced where none was
needed, per the milestone's own instruction not to chase theoretical
purity where the user-facing result would not improve.

## Validation results

- `cargo fmt --all --check`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass, zero
  warnings
- `cargo test --workspace`: 465 GUI tests pass (459 existing + 6 new),
  0 failed; full workspace suite (including `archivefs-core` and CLI
  tests) pass
- `cargo build --workspace --release`: pass
- `git diff --check` (against `main`): pass, no whitespace errors

## Core and CLI confirmation

Every commit in this branch touched only `crates/archivefs-gui/src/
main.rs` and documentation files (`docs/GUI_SIMPLIFICATION.md`, this
report). No file under `crates/archivefs-core/` or any CLI crate was
modified. No core or CLI change was needed at any point - every fix in
this pass was expressible entirely within the GUI crate's existing
render functions, shared components, and tests.

## Release-readiness verdict

**Ready for release from a GUI-consistency standpoint**, with the
cosmetic/architectural items above tracked as known, deliberately
deferred follow-ups rather than blockers. All behavioural, safety, and
backend semantics are unchanged and fully covered by the existing (plus
6 new) passing test suite; no confirmation flow, destructive action, or
error-detail path was altered in a way that changes what the user can
do or see beyond wording and heading consistency.

## Recommended next non-GUI milestone

Audit the backend's own error-message strings (`archivefs-core`) for the
same kind of terminology consistency this pass gave the GUI layer. The
GUI's failure banners largely pass backend error text through verbatim
by design (per this milestone's own failure-presentation rules, which
require preserving exact error strings) - so a future backend-message
consistency pass should account for the exact GUI call sites that
display that text directly, rather than requiring another GUI-side pass
to catch up afterward.

## Confirmation

Nothing from this branch has been merged or pushed. All six commits
remain local to `sonnet-final-gui-polish` in
`/home/davedap/archivefs-fable`; `main` and the `/home/davedap/archivefs`
worktree were not touched during this pass.
