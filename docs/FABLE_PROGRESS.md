# Fable Integration Campaign — Progress Ledger

This file is the durable, resumable record of the Fable/ArchiveFS GUI
integration campaign. Future sessions must be able to resume from this file
and Git history alone, without depending on prior conversation context.

## Campaign identity

- Campaign base commit: `fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6`
  ("Add trusted RetroArch cheat catalogue retrieval")
- Branch: `fable-archivefs-integration`
- Worktree: `/home/davedap/archivefs-fable`
- `origin/main` at initializer time: `fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6`
  (identical to campaign base — branch had not diverged from main)

## Initializer status

Milestone 0 (initializer) complete. No production Rust source was changed.
No implementation milestones have started. Do not resume mid-implementation
without re-reading `docs/FABLE_CAMPAIGN_PLAN.md` and
`docs/GUI_BACKEND_CAPABILITY_MATRIX.md` in full first.

## Files and modules inspected

- `Cargo.toml` (workspace members, edition 2024)
- `crates/archivefs-core/src/lib.rs` (full public API surface via
  `pub fn|struct|enum|trait|use` grep, ~11.3k lines)
- `crates/archivefs-core/src/patch_manager/mod.rs` (full module list and
  `pub use` re-exports)
- `crates/archivefs-core/src/database.rs`, `inspector.rs`,
  `library_views.rs` (line counts + re-export surface only, not read in
  full)
- `crates/archivefs-gui/src/main.rs` (structural grep: `mod|struct|enum|
  impl|thread::spawn|mpsc::channel|fn main`, ~25.4k lines; targeted reads of
  navigation enums `MainView`/`ToolsOverlay`, the operation-request/result
  types, `OperationHistory`, and the generic mount/unmount worker closure)
- `crates/archivefs-cli/src/main.rs` (full top-level subcommand match-arm
  list via grep, ~7.6k lines)
- `crates/archivefs-cli/src/retroarch_cheat_setup.rs`,
  `retroarch_cheat_sources.rs` (dispatch-only, confirmed thin wrappers)
- `crates/archivefs-gui/Cargo.toml`, `crates/archivefs-core/Cargo.toml`
  (dependency surface: `eframe`, `arboard`, `rfd`, `ureq`; no async runtime)
- `docs/architecture.md` (existing architecture doc — read in full for
  terminology/precedent, not duplicated)
- `docs/roadmap.md` (line count only)
- `design-reference/ArchiveFS design stage one.zip` — extracted to a unique
  `/tmp` directory, screen names and interaction labels enumerated via
  grep, then **the temporary directory was deleted** before finishing.

## Architecture findings

See `docs/FABLE_CAMPAIGN_PLAN.md` for full detail. Headlines:

1. `archivefs-gui/src/main.rs` is a 25.4k-line monolith with no
   submodules — an obligatory mechanical split, not optional cleanup.
2. There is no application-service/orchestration layer. Each of ~14
   GUI workflows hand-rolls its own `mpsc::channel()` + `thread::spawn`
   pair and its own request/result types.
3. RetroArch/cheat functionality (patch preview, catalogue matching, cheat
   install, journal, history, rollback, trusted-source retrieval) is fully
   implemented and CLI-reachable but has **zero** GUI integration.
4. The design's 9 screens (Mount, Selected, Active Mounts, Library,
   Sources, Doctor, History & Logs, Settings, About) map onto a GUI that
   currently only has Library/Health/Duplicates/Sources/LibraryViews as
   primary views, plus Doctor as one of five `ToolsOverlay` variants.
   Mount/Active-Mounts/History/Settings/About have no dedicated screen
   today.
5. Operation history is in-memory only (GUI-local `VecDeque`, not
   persisted); the only durable on-disk history is the RetroArch cheat
   install/rollback journal, which is domain-specific.
6. Two parallel Doctor-shaped report types exist (`DoctorReport` vs.
   `ConfigCheckReport`/`SetupDiagnostics`) and need reconciling before a
   single unified Doctor screen is built.
7. Mount/unmount is the best-precedented, best-tested vertical and the
   safest place to validate any new orchestration layer.
8. Trusted-source fetch (`fetch_retroarch_cheat_source`) uses blocking
   `ureq` HTTP — must be run on a background thread before any GUI call,
   same pattern as existing mount/scan workflows (no async runtime present
   or to be added).

## Verified existing capabilities

(Full matrix in `docs/GUI_BACKEND_CAPABILITY_MATRIX.md`.) Backend-complete
and CLI-exercised: source add/enable/disable/scan/remove, catalogue
scan/persist, missing-record removal, duplicate detection, manual/bulk
platform assignment, platform aliases, mount/unmount (single + batch),
lazy unmount, cleanup/stale-mount recovery, Doctor + config-check +
setup-diagnostics, archive inspection, library views (saved filters),
archive index build/read, RetroArch profile discovery/selection, cheat
catalogue matching, cheat availability report, cheat install (+ backups +
journal), cheat rollback, cheat install/rollback history & journal
inspection, trusted RetroArch cheat-source list/fetch/inspect with offline
snapshot reuse.

GUI-integrated subset of the above: source management, catalogue scan,
missing-record removal, duplicate detection (as a screen), manual/bulk
platform assignment, platform aliases (as an overlay), mount/unmount
(single + batch + lazy + cleanup), Doctor (as an overlay), library views,
archive inspection (as an overlay).

## Genuine missing capabilities (not just unwired GUI)

- No general-purpose, persisted operation-history log (only in-memory GUI
  ring buffer + RetroArch-specific journal).
- No "list currently active mounts" query distinct from "list all archives
  and filter by mount state."
- No "Check for Updates" or "Export Support Bundle" backend of any kind —
  these are design-only affordances today.
- No mid-flight cancellation support anywhere in `archivefs-core`.

## Unresolved questions

- Should the two Doctor-shaped report types (`DoctorReport` vs.
  `ConfigCheckReport`/`SetupDiagnostics`) be merged, or should the Doctor
  screen deliberately surface both under one UI without a core-level merge?
  Not decided in this initializer — flagged for Milestone 2 scoping.
- Should operation-history persistence (for the History & Logs screen) live
  in `archivefs-core` (new module) or stay GUI-local but written to a JSON
  file? Not decided — flagged for Milestone 2 scoping, default lean is
  GUI-local JSON file first (smallest surface), promote to core later only
  if CLI also needs it.
- Exact shape of the shared `Command`/`ProgressEvent` envelope (§21–23 of
  the campaign plan) is proposed but not implemented or reviewed against
  real Milestone-3 usage yet.

## Assumptions

- "Existing reusable APIs" were verified by `pub` signature/grep and
  existing call sites (CLI dispatch, GUI call sites, `tests.rs` modules),
  not by executing them in this session (full test suite was intentionally
  not run per initializer cost-discipline instructions).
- The design export's 9 top-level screen names were confirmed by string
  search in the extracted HTML; the fine-grained interaction labels listed
  in the capability matrix are the visible button/label text as of the
  `ArchiveFS design stage one.zip` snapshot present at initializer time —
  they are not a live/authoritative spec and should be re-checked against
  `design-reference/` directly during each milestone that touches a given
  screen.

## Build commands

```
cargo build --workspace
cargo build -p archivefs-core
cargo build -p archivefs-cli
cargo build -p archivefs-gui
```

## Targeted test commands

```
cargo test -p archivefs-core patch_manager::
cargo test -p archivefs-core retroarch
cargo test -p archivefs-gui <module_or_fn_substring>
cargo test -p archivefs-cli retroarch_cheat
```

## Full validation commands

Run only when production Rust files have changed (not required for this
initializer):

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Campaign direction change (2026-07-22)

Per explicit operator instruction, the campaign switched from
"foundation-first" (Milestone 1 module split + command envelope) to
"feature-first": use the Claude Design export as the target interface,
implement visible missing features directly, do **not** broadly refactor
`main.rs`, and extract code only when a feature directly requires it.
The Milestone 1 module split and Command/ProgressEvent envelope are
deferred indefinitely unless a feature forces them.

## Redesigned application shell (2026-07-22)

Landed in commit "Add redesigned ArchiveFS application shell":

- `MainView` extended with the redesign destinations `Mount`, `Selected`,
  `ActiveMounts`, `Doctor`, `HistoryLogs`, `Settings`, `About` (existing
  `Library`/`Health`/`Duplicates`/`Sources`/`LibraryViews` retained).
- Navigation moved from a horizontal top row to a persistent left
  `egui::SidePanel` rail (`show_primary_navigation`), driven by shared
  `PRIMARY_NAVIGATION_DESTINATIONS` (the nine design screens, in design
  order) and `SECONDARY_NAVIGATION_DESTINATIONS` (pre-redesign catalogue
  views Health/Duplicates/Library Views, preserved under a "Catalogue
  views" group). `navigation_destination_enabled` keeps the existing
  Health/Duplicates disabled-until-database behaviour.
- New destination content, reusing proven code wherever it exists:
  Doctor = `show_doctor_checks_panel` as a full page; History & Logs =
  full-screen `OperationHistory` listing (`show_history_logs_page`);
  Active Mounts = read-only mounted-archive listing reusing
  `pending_unmount_items` (`show_active_mounts_page`); Settings =
  read-only paths page (`show_settings_page`); About = shared
  `show_about_contents` (factored out of `show_about_window`, which now
  delegates to it). Mount and Selected are honest interim pages routing
  to the Library's proven panels; the redesigned Mount screen is the
  next deliverable.
- All `ToolsOverlay` overlays, menus, and existing workflows unchanged.
- Tests: `primary_nav_rects` test mirror now iterates the same
  destination consts as production (cannot drift); click-reachability
  test extended to all twelve destinations and renamed
  `all_navigation_destinations_are_reachable_via_a_real_click`.
  Full `archivefs-gui` suite: 347 passed, 0 failed.

## Redesigned Mount screen (2026-07-22)

Landed in commit "Add redesigned Mount screen with queue and preview":

- `show_mount_page`: filterable live-archive table (name, platform,
  validation, archive path, planned destination from
  `record.mount_plan.mount_path`), per-row Queue/Unqueue with a QUEUED
  badge, Queue-all-visible, Clear-queue, Refresh, and an inline
  confirmation strip before execution. Rendering never mounts — the
  returned `MountPageAction` is the only side-channel, handled in
  `update`.
- Queue state on `ArchiveFsApp` (`mount_queue`, `mount_search`,
  `confirm_mount_queue`): order-preserving, deduplicated, pruned against
  the live snapshot (`prune_mount_queue`); non-Pending queued archives
  stay visible with a skip label rather than vanishing.
- Validation labels are a pure `MountState` mapping
  (`mount_validation_label`): Pending → "Ready to mount", Mounted →
  already-mounted skip, `MountPathExists` → destination-collision skip.
- Execution reuses the proven `start_mount_all` batch engine via
  `queued_pending_paths` (queue order, Pending only) +
  `mount_all_items_for_paths`; `MountAllResult` renders on the Mount
  page as well as the Library page. No new mount machinery.
- New tests (4): queue order/eligibility, pruning, validation labels,
  case-insensitive row matching. Full `archivefs-gui` suite: 351
  passed, 0 failed.

Deferred from the design for later deliverables: the per-archive
inspector side panel (size / source library / confidence), Format/Size
columns, Ctrl+M shortcut, density toggle (prototype-only), and the
FUSE-options Advanced block (no backend).

## Redesigned Selected screen (2026-07-22)

Landed in commit "Add redesigned Selected screen for mount queue review":

- `show_selected_page`: queue review over the same `mount_queue` state
  the Mount page builds — Archive / Platform / Planned destination /
  Planned action table in queue order, per-row Remove, Clear queue,
  and the shared confirmation-then-`start_mount_all` execution path.
- `planned_action_label`: pure `MountState` → action-verb mapping
  ("Mount" / "Skip — already mounted" / "Skip — destination already
  exists"), the action counterpart of `mount_validation_label`.
- `show_mount_queue_confirmation` + `QueueConfirmChoice`: confirmation
  strip factored out and shared by Mount and Selected so wording and
  gating cannot drift; `handle_mount_page_action` on `ArchiveFsApp`
  shares the action handling (queue eligibility re-derived from the
  live snapshot at click time, never captured at render time).
- Deliberate deviation: the design's MOUNT COMMAND preview is absent —
  the configured ratarmount binary name is not part of any GUI-held
  state (`ConfigIdentity` has only path + digest), and guessing it
  would be untruthful. Revisit only with a core-provided command
  preview.
- Tests: planned-action label mapping added (352 passed, 0 failed).

## Active Mounts actions (2026-07-22)

Landed in commit "Add unmount actions to the Active Mounts screen":

- Per-row Unmount with an inline confirmation strip; stale
  confirmations (archive no longer mounted) are cleared automatically.
  Confirmed unmounts are routed through the exact
  `AppOperationRequest::Archive` → `start_operation` path the Library
  panel uses — no new unmount machinery.
- "Clean empty mount directories after unmount" checkbox on the page,
  bound to the same `cleanup_after_unmount` field as the Library panel.
- Refresh button, per-row "Open in Library" (selects the archive), and
  the shared `ActionFeedback` banner on the page.
- Deliberate deviation from the design: Lazy Unmount and Remount are
  not offered on this page. Both are failure-recovery offers the
  proven Library flow only unlocks after a failed/completed normal
  unmount (`lazy_unmount_offers`/`remount_offers`); duplicating that
  two-stage safety flow here would risk drift. "Open in Library" is
  the route to the full recovery toolkit.
- Rendered-frame test covers mounted-only listing, no action without
  confirmation, stale-confirmation clearing, and the confirm strip
  (353 passed, 0 failed).

## History & Logs filtering and export (2026-07-22)

Landed in commit "Add filtering and export to History & Logs":

- Operation filter (`ALL_ACTIVITY_ACTIONS`) and Result filter
  (`ALL_ACTIVITY_OUTCOMES`) as combo boxes with "All Operations"/"All
  Results" defaults; newest/oldest sort toggle; Clear Filters; shown/
  total entry count; empty-filter-result message.
- Copy Visible Log (clipboard) and Export Log (rfd save dialog +
  `std::fs::write`). The export's own outcome is recorded in the
  history as a new `ActivityAction::LogExport` entry, so exports are
  auditable in the same log.
- Filtering is pure (`history_entry_visible` /
  `visible_history_entries`) and never mutates or reorders the
  underlying `OperationHistory`.
- **HISTORY-001 decision (was an open question):** operation history
  stays in-memory for this campaign phase. When persistence is
  prioritized it will be a GUI-local JSON log file (the initializer's
  default lean), not a new core module. The design's date filter is
  deferred with it — a session-scoped history makes "Today/Yesterday"
  filters meaningless. This supersedes the "decide before HISTORY-002"
  sequencing: the screen was built against the in-memory scope with
  the deviation documented here.
- Tests: filter-list completeness and filter/sort behaviour
  (355 passed, 0 failed).

## Next deliverable

Doctor screen parity: copy-full-details affordance and suggested
actions on the Doctor page, plus surfacing the RetroArch environment
diagnostics (backend `emulator_environment::retroarch`, CLI
`retroarch-environment`) if scoping allows.

## Latest clean commit

`fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6` (campaign base; the initializer
commit itself, once made, is the new latest clean commit — see this file's
Git history / `git log` for the current value, this line is not
auto-updated).

## Latest complete workspace test totals

Not run in this session (initializer cost-discipline: full workspace test
suite intentionally not executed). Approximate `#[test]` function counts by
grep at initializer time: `archivefs-core` 739, `archivefs-cli` 118,
`archivefs-gui` 347 (1204 total). These are function counts, not a test-run
result — run `cargo test --workspace` for actual pass/fail totals before
relying on this number.

## Known limitations

- This ledger reflects a single grep/read pass, not exhaustive reading of
  every source file (`database.rs`, full `lib.rs` body, full `main.rs`
  body were not read line-by-line — only their public surface and
  structural shape).
- The design-reference screen/interaction inventory is derived from text
  labels only; visual layout, spacing, and interaction sequencing were not
  inspected in this session beyond what plain-text grep of the exported
  HTML/JS revealed.

## Stop conditions for future sessions

- Stop and re-verify preconditions if `git rev-parse HEAD` does not match
  the "Latest clean commit" recorded at the top of that milestone's own
  progress entry, if the branch differs from `fable-archivefs-integration`,
  or if `origin/main` has moved (indicates the base assumption of this
  ledger is stale).
- Stop before writing to `design-reference/` — it must remain untouched and
  git-ignored.
- Stop before touching `/home/davedap/archivefs` or
  `/home/davedap/archivefs-codex` (separate worktrees/campaigns).
- Stop before running the full workspace test suite unless production Rust
  files have actually changed in that session.
