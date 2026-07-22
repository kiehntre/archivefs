# Integrated GUI adversarial audit

Date: 2026-07-22  
Branch audited: `codex-integrated-gui-rescue`  
Integrated base: `6b7e5ce` (`Fix integrated cheat source listing result handling`)

## Scope and method

This is a source-level audit of the integrated `archivefs-gui` implementation and the Fable campaign records. It is not a visual sign-off. The audit inspected the GUI entry point, navigation and overlay state, page renderers, activity/history presentation, mount and recovery routing, RetroArch profile and trusted-source workflows, asynchronous polling, context-menu tests, and the Fable progress/feature documents.

The protected design draft and other worktrees were not accessed. No production code was changed before this report was written.

## Executive finding

The integrated GUI contains real, safety-conscious functionality, but the claimed visual redesign is not release-ready. The Fable work primarily added navigation destinations and workflow-specific grids beneath new headings. It did not establish a shared visual system, responsive content policy, consistent action hierarchy, or a user-oriented information architecture.

The backend integration should be retained. The current presentation should not be accepted as the release candidate without rescue work.

## Structure and maintainability

### Real defects

- `crates/archivefs-gui/src/main.rs` is 28,551 lines: approximately 15,500 lines of production code followed by approximately 13,000 lines of in-file tests. All GUI coordination, view state, rendering, clipboard handling, asynchronous jobs, and test fixtures share one module.
- `ArchiveFsApp::update` spans roughly 934 lines and mixes polling, safety derivation, menus, navigation, overlay routing, page routing, rendering, and action dispatch. Small changes have a large regression surface.
- `show_loaded_data` spans roughly 1,006 lines. It combines Library filtering, sorting, selection, bulk operations, summaries, warnings, table rendering, details, dialogs, and action construction.
- Other oversized renderers include the health dashboard (about 470 lines), Library Views (about 445), Sources (about 296), duplicate review (about 253), archive inspector (about 245), trusted-source workflow step 2 (about 228), Settings (about 211), Mount (about 201), History & Logs (about 190), and Selected (about 183).
- Activity starts expanded (`show_activity: true`) despite its documentation describing a compact, collapsible replacement. It therefore permanently consumes workflow height on first launch.
- The main content panel has no normal maximum width or page padding policy. Simple forms stretch across the full central panel, while fixed-width controls and grids remain clustered at the upper-left.

### Maintainability problems

- Styling is almost entirely local and ad hoc: repeated `ui.heading`, `ui.strong`, `ui.separator`, `ui.add_space`, `Frame::group`, raw RGB success colours, and direct `warn_fg_color`/`error_fg_color` calls. `apply_readability_style` changes global scale and three spacing values but is not a presentation system.
- Action hierarchy is not encoded. Primary, secondary, quiet, recovery, and destructive actions are nearly all default `egui::Button` values.
- Status classification is duplicated as unrelated label functions and local `match` blocks (`mount_validation_label`, `planned_action_label`, Doctor status colours, setup diagnostic colours, profile eligibility, cheat freshness, fetch status, feedback colours).
- Path presentation is duplicated across grids as selectable wrapping labels, sometimes with a copy button and sometimes without one. There is no consistent truncation, tooltip, or copy treatment.
- Page state is stored as a long flat list on `ArchiveFsApp`. Much of it is legitimate workflow state, but presentation-only state (filters, sorts, column widths, expanded state, focus flags, overlay selection) is not grouped by screen, making ownership and reset behaviour difficult to reason about.
- `MainView` and `ToolsOverlay` create two navigation systems. Doctor and About are both pages and separately reachable overlay/window content; Diagnostics and Database Status are tool overlays while related state is also presented in Settings. This is functional but inconsistent.
- The source contains campaign-era explanatory comments that are much longer than the code they describe and refer to superseded milestone wording. They obscure current invariants instead of documenting them concisely.
- Some cloning is required to move owned paths/results into workers or preserve exact identity. Other cloning is presentation-driven, notably cloning the complete health issue vector in `update` to escape a borrow. This is not currently a performance defect, but it is evidence that rendering and cache ownership are too entangled.

### Acceptable limitations

- `mount_queue` and Library selection are separate state on purpose: one is an ordered execution review list and one is selection/focus state. Consolidating them would change behaviour.
- Receiver ownership and generation/identity checks are appropriate for asynchronous stale-result protection. These should not be simplified into untyped flags.
- Exact `PathBuf` identity and cloning paths into background operations are justified for correctness, including non-UTF8 paths.
- The custom Library table is complex because it preserves resizing, keyboard focus, multi-selection, and context menus. It should be improved incrementally, not replaced blindly.

## Visual and usability audit

### Application shell

#### Visual/usability problems

- The left rail is a fixed 180-point panel containing a flat stack of default selectable buttons. It provides no application summary, workflow grouping beyond a separator, count/state context, or strong active indicator.
- The menu bar and rail compete as two primary navigation surfaces. Several menu items merely navigate to pages already present in the rail.
- Ordinary pages render directly into an unpadded `CentralPanel`. There is no responsive maximum content width for prose/forms and no explicit wide-content mode for tables.
- Page headings are followed by four points of spacing and then legacy controls. There is no consistent purpose line, toolbar, summary band, card rhythm, or section hierarchy.

### Activity

#### Real defect

- The panel defaults expanded and can reach 220 points plus chrome, directly contradicting the release requirement for collapsed-by-default or compact-summary behaviour.

#### Visual/usability problems

- The collapsed state shows only a count; it does not surface the latest important outcome. An error can therefore be present but not meaningfully summarised.
- Expanded entries are undifferentiated wrapped text. Outcome, operation, time, and related archive are embedded in one string rather than scan-friendly fields.
- Clearing is visually equal to benign navigation/copy operations and has no distinct quiet/destructive treatment.

### Mount and Selected

#### Visual/usability problems

- Mount uses a six-column `egui::Grid` with two unbounded wrapping path columns. Long paths determine row height and crowd name, validation, and action cells.
- Queue state is rendered as both a button and a tiny uppercase text suffix rather than a consistent badge.
- Ready, mounted, and collision states are plain text and are hard to scan.
- “Mount queue”, “Queue all visible”, “Clear queue”, and “Refresh” are default buttons with no primary/quiet hierarchy.
- Selected begins with the raw selected path and RetroArch entry controls before the queue review, mixing two different jobs. Ready and blocked queue entries are not visually grouped.
- Empty states are plain labels with no next-action emphasis.

#### Functionality to retain

- Queue ordering, pruning, validation, confirmation, per-item revalidation, and dispatch through the existing batch mount coordinator are correct boundaries.
- Opening Selected has no mount side effect.

### Active Mounts

#### Visual/usability problems

- The page uses another diagnostic-style grid and raw paths. State and archive identity lack hierarchy.
- Normal unmount, refresh, and navigation actions use identical default treatment.
- Recovery remains discoverable only through “Open in Library”; this is safe but needs clearer explanatory wording.

#### Acceptable limitation

- Lazy unmount and remount are intentionally absent from this page. They remain conditional recovery actions in the existing Library flow, which avoids casually exposing unsafe unmount behaviour.

### Library and catalogue views

#### Visual/usability problems

- The Library still dominates the product visually and retains the legacy developer-oriented table/detail composition.
- Default column widths are fixed presentation state. The custom table calculates a minimum width and falls back to horizontal scrolling rather than choosing proportions from available width.
- Paths and technical provenance dominate the selected-archive panel. Ordinary metadata and user actions do not form a concise selected-item summary.
- Health, Duplicates, and Library Views preserve useful legacy capability but are presented as first-class rail destinations with visual weight close to core workflows.

#### Functionality to retain

- Exact selection identity, keyboard movement, sorting, filtering, platform assignment, missing-entry review, bulk actions, archive inspection, and row context menus have substantial behavioural coverage.
- Context-menu tests cover right-click selection, multi-selection preservation, action parity, non-UTF8 paths, and no-side-effect opening. No context-menu regression was found in the inspected integration.

### Sources

#### Visual/usability problems

- Source rows remain a wide diagnostic grid. Status, scan result, path, counts, and actions compete in one row.
- Long source paths receive no standard path component.
- Actions have no primary/secondary/destructive hierarchy; remove-source confirmation is safe but not visually distinct.

#### Functionality to retain

- Add, scan-one, scan-all, enable/disable, refresh, remove-with-catalogue-policy, and Library filtering route through existing background/core operations.

### Doctor

#### Visual/usability problems

- The page is a summary line plus the old checks grid. Failed checks are not reliably ordered before warnings and successes.
- Successful checks receive similar space and weight to blocking failures.
- “Run All Checks” and “Copy Report” are default peers rather than primary and secondary actions.
- Technical details are always in the main flow rather than expandable.

#### Acceptable limitation

- The backend does not provide trustworthy per-check suggested fixes or JSON export. Those controls should remain absent.

### History & Logs

#### Visual/usability problems

- Filtering works but is a dense line of combo boxes and buttons.
- Entries are flattened into one long text string, so result severity and event identity are hard to scan.
- Long content wraps, but there is no visual grouping, badge, or compact timestamp/operation column.
- Export opens a native save dialog synchronously from the renderer. This is an existing behaviour to test manually; it is not evidence of network or mount safety bypass.

### Settings

#### Real defect

- Settings is an information/diagnostics dump rather than settings-oriented information architecture. Configuration, database, mount, validation, environment reporting, profile discovery, and deferral explanation are rendered as one continuous surface.

#### Visual/usability problems

- Paths are placed in a two-column grid without responsive truncation. Only some have copy actions.
- RetroArch profiles are in a five-column grid, causing configuration and cheat-destination paths to be squeezed on laptop widths.
- Every blocker is repeated below the grid as raw `profile: code — detail` text. Blocked profiles overwhelm the eligible path.
- The final paragraph about unsupported settings reads like internal project documentation.

### About

#### Visual/usability problems

- Truthful information is present, but the page shares the same diagnostic grid language as Settings and lacks a concise product summary.
- A separate About window duplicates the page presentation and navigation concept.

### RetroArch cheat workflow

#### Functionality to retain

- Entry gating, exact archive identity, single-eligible-profile preselection, explicit choice when ambiguous, trusted built-in sources only, background retrieval, cached offline reuse, retrieval history, and stale-result rejection are real and correctly bounded.
- The nested `CheatSourceList` result integration fix at the branch tip is necessary and must remain.

#### Visual/usability problems

- Step 1 exposes archive path, source root, and size in a raw grid before establishing a concise archive/profile choice.
- Blocked profiles render inline with full blocker detail, competing with eligible profiles.
- Step 2 gives source ID, URL, permitted host, provenance, licence, pinned version, digest, counts, and raw warnings similar visual weight.
- Force refresh is placed beside the primary fetch action, presenting an advanced bypass of fresh-cache reuse as an ordinary choice.
- Successful retrieval prioritises SHA-256 and filesystem paths over freshness, valid cheat count, and retrieval time.
- Backend warning strings are displayed verbatim as the main result. Retained-but-unparseable and non-UTF8 skips need calm aggregate summaries with expandable details.
- The workflow does not prominently state its truthful endpoint: catalogue retrieval is available, but matching and installation are not implemented in this GUI path.

#### Intentionally deferred functionality

- Cheat matching, preview, installation, rollback, pinning, pruning, and snapshot-maintenance controls are not part of the integrated GUI path and must not be fabricated.

## Controls and duplication

No inspected production button was found to be a pure no-op. The problem is not a large set of fake controls; it is that meaningful controls are duplicated or presented without hierarchy.

- Doctor exists as both a page and a Tools overlay.
- About exists as both a page and a Help window.
- Source navigation exists in both the rail and the menu.
- Activity toggling exists in both the panel header and Tools menu.
- The same path/status/card patterns are reimplemented across pages instead of shared.
- Mount confirmation sharing is a positive exception: Mount and Selected use the same confirmation function and batch path.

## Tests

### Strong behavioural coverage to retain

- Mount/unmount coordination, confirmation, recovery gating, stale snapshot/generation safety, source mutations, context menus, exact/non-UTF8 path identity, queue preservation, profile selection, trusted-source stale results, and clipboard editing behaviour have meaningful focused tests.

### Maintainability/test-quality problems

- Tests live in the same 28k-line module as production, making structural change expensive.
- Many render tests assert that text appears or does not appear in tessellated output. These are useful smoke tests but do not validate hierarchy, responsive proportions, disabled explanations, focus order, or action prominence.
- Campaign tests sometimes validate helper label mappings because the implementation lacks an intermediate presentation model. A shared status/profile/source presentation model would allow tests to validate meaningful classification rather than only strings.
- The existing activity test validates collapse mechanics, but the production default is expanded; the important startup-state behaviour was not tested.
- There is no focused responsive-width decision test because no responsive layout policy currently exists.

## Fable documentation accuracy

### Real defects

- `FABLE_PROGRESS.md` calls the shell and screens “redesigned”, while its Known limitations section admits visual layout, spacing, and interaction sequencing were not inspected. The implementation evidence supports “new routes and functional screens”, not visual completion.
- `FABLE_FEATURES.json` marks multiple integration items complete based on helper tests and passing builds, even where the acceptance language requires design-reference parity or cross-screen consistency.
- The progress ledger contains outdated test totals and a stale “latest clean commit”.
- Its future-session stop conditions refer to a superseded branch and even say to stop before touching the current worktree. They are campaign residue, not valid instructions for this rescue branch.

### Honest interpretation

- Fable delivered real navigation, queue review, active-unmount routing, history filtering/export, Settings/About data exposure, profile discovery, and trusted-source retrieval.
- Fable did not deliver a coherent visual redesign or a release-ready information architecture.

## Rescue recommendation by integrated Fable feature group

| Feature group | Recommendation | Reason |
| --- | --- | --- |
| Application shell | Retain after further fixes | Navigation coverage is useful; presentation and overlay duplication need rescue. |
| Mount queue and Selected review | Retain after further fixes | Safety and queue behaviour are sound; hierarchy and tables are poor. |
| Active Mounts | Retain after further fixes | Correct normal-unmount routing; needs clearer cards/state/actions. |
| History & Logs | Retain after further fixes | Filtering/export are real; event presentation is weak. |
| Doctor | Retain after further fixes | Truthful checks/reporting; needs health-dashboard hierarchy. |
| Settings and About | Retain after further fixes | Truthful data and actions; current presentation should be substantially reorganised. |
| RetroArch profile discovery | Retain after further fixes | Correct discovery and gating; grid/blocker dump is not usable enough. |
| Trusted-source catalogue workflow | Retain after further fixes | Strong safety boundary and cache handling; needs staged, user-facing results and warning summaries. |
| Legacy Library/catalogue views | Retain | High functional value and strong interaction coverage; improve incrementally. |
| Claimed visual redesign completion | Reject | The source and campaign ledger do not support release-ready visual completion. |

## Rescue constraints

- Preserve backend APIs and typed failures unless a genuine defect requires a backend correction.
- Preserve the existing operation coordinators, generation checks, exact-path identity, trusted-source restrictions, and context-menu behaviour.
- Introduce a small shared presentation layer, not a framework or broad rewrite.
- Separate ordinary content width from genuinely wide tables.
- Default Activity to compact mode and surface the latest important outcome.
- Treat unsupported RetroArch steps as unavailable in explanatory text, not controls.
