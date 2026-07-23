# Fable Integration Campaign — Architecture Plan

Campaign base commit: `fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6`
Branch: `fable-archivefs-integration`
Status: Milestone 0 (initializer) only. No production code has been touched.

This document is the architecture reference for integrating the Claude
Design "stage one" screens (`design-reference/ArchiveFS design stage one.zip`)
into the existing `archivefs-gui` crate, reusing `archivefs-core` wherever a
tested capability already exists.

## 1. Current workspace and crate architecture

Cargo workspace (`Cargo.toml`, resolver "2"), three members:

- `crates/archivefs-core` — all domain logic. `src/lib.rs` (11.3k lines) plus
  submodules `database.rs` (7.6k), `inspector.rs`, `library_views.rs`, and
  `patch_manager/` (RetroArch/PCSX2 patch & cheat logic), plus
  `emulator_environment/`.
- `crates/archivefs-cli` — a hand-rolled argv dispatcher in `main.rs`
  (7.6k lines, string-matched subcommands, no clap). Thin wrappers over core
  functions plus human/JSON formatting.
- `crates/archivefs-gui` — a single `eframe`/`egui` immediate-mode
  application in `main.rs` (25.4k lines). No submodules; all views, state,
  and background-worker plumbing live in one file.

No fourth "application service" crate or module exists. `archivefs-gui`
depends directly on `archivefs-core` and calls its free functions from
UI-triggered closures.

## 2. Crate responsibilities

- `archivefs-core`: config parsing, source-folder management, catalogue
  scanning and persistence (SQLite via `database.rs`), archive
  identity/health/duplicate classification, platform detection, mount
  planning and execution (`RatarmountBackend`, `MountBackend` trait),
  unmount (normal + lazy), cleanup/stale-mount recovery, Doctor diagnostics,
  archive inspection (zip/7z member listing), library views (saved
  filter/search configs), and the entire `patch_manager` subsystem (PCSX2
  patch preview, RetroArch patch/cheat preview, cheat catalogue matching,
  cheat installation, install journals, rollback, history inspection,
  trusted-source retrieval).
- `archivefs-cli`: exposes nearly all of the above as subcommands (`scan`,
  `mount`, `mount-one`, `unmount`, `unmount-one`, `status`, `stats`,
  `duplicates`, `info`, `doctor`, `config-check`, `retroarch-environment`,
  `retroarch-patch-preview`, `retroarch-cheat-catalogue`,
  `retroarch-cheat-install`, `retroarch-cheat-setup`,
  `retroarch-cheat-source-{list,fetch,inspect}`, `retroarch-cheat-history`,
  `retroarch-cheat-inspect`, `retroarch-cheat-rollback`, `index-build`,
  `index-show`, `index-find`, `library-status`, `database-check`, `health`,
  `library-scan`, `library-list`, `library-find`,
  `library-{set,clear}-platform[-bulk]`, `library-remove-missing`,
  `platform-alias-{list,add,remove}`, `clean`, `watch`, `sources`, `source`,
  `view`). This is the most complete integration of core capability; it is
  the primary precedent for what GUI plumbing must reach.
- `archivefs-gui`: covers a narrower slice — Library (list/search/filter/
  select/inspect/platform-assign), Health (duplicate + health dashboards),
  Duplicates, Sources (add/enable/disable/scan/remove), Library Views
  (saved filters), single/batch mount, single/batch unmount (+lazy +
  cleanup), Doctor (as an overlay), Database status/diagnostics, platform
  aliases, and an in-session Activity/operation-history panel.
  **RetroArch/cheat functionality has zero GUI integration today** —
  `rg -i "retroarch|cheat|patch_manager"` against `archivefs-gui/src/main.rs`
  returns no matches. There is also no dedicated "Active Mounts" screen, no
  "History & Logs" screen with persistence, no "Mount" preview screen
  separate from Library, no "Settings" screen, and no "About" screen.

## 3. GUI architecture (current)

`archivefs-gui` is a single `eframe::App` (`ArchiveFsApp`, defined at
`main.rs:2455`) driven by `update()` (`main.rs:4899`). Navigation is two
independent enums:

- `MainView` (`main.rs:2280`): `Library | Health | Duplicates | Sources |
  LibraryViews` — the always-visible primary destinations.
- `ToolsOverlay` (`main.rs:2303`): `None | Diagnostics | PlatformAliases |
  DatabaseStatus | DoctorChecks | ArchiveInspector` — one full-panel overlay
  at a time, reached from a "Tools" menu (except `ArchiveInspector`, reached
  from the selected-archive panel).

This two-axis design (`MainView` for tab-bar destinations, `ToolsOverlay` for
everything else) is already documented in-code as a deliberate choice (see
the doc comment on `ToolsOverlay`, `main.rs:2288-2299`) to avoid cramming
diagnostics/admin UI onto the Library page. It is a reasonable precedent for
where new screens should slot in (see §13).

## 4. Application-state flow

`ArchiveFsApp` holds all UI state directly as struct fields: the loaded
catalogue snapshot (`LoadedData`/`DatabaseState`), per-feature "running
operation" structs (`RunningOperation`, `RunningMountAll`,
`RunningUnmountAll`, `RunningSourceAction`, `RunningPlatformAction`,
`RunningBulkPlatformAction`, `RunningAliasAction`,
`RunningLibraryViewAction`, `RunningMissingRemoval`,
`RunningSetupAction`), dialog state (`SourcesAddDialogState`,
`SourcesRemoveDialogState`, `LibraryViewFormDialogState`, ...), filter state
per view, and the `OperationHistory` (in-memory `VecDeque`, capped at
`HISTORY_LIMIT`, **not persisted to disk** — confirmed by absence of any
`OperationHistory` reference outside `archivefs-gui`).

There is no central "app state" enum or reducer; each interactive feature
owns its own small state machine (typically an `Option<RunningX>` plus a
result/feedback struct) and `update()` polls all of them each frame.

## 5. Background-operation model

There is no shared worker abstraction. The pattern, repeated ~14 times in
`main.rs` (each call site allocates its own channel(s) and thread), is:

```rust
let (sender, receiver) = mpsc::channel();
thread::spawn(move || {
    let result = /* call into archivefs-core */;
    let _ = sender.send(result);
});
// stored as e.g. RunningOperation { receiver, .. }
// polled via receiver.try_recv() in update()
```

The single/batch mount-and-unmount path (`main.rs:4020` onward) is the most
developed instance: it takes a generic `F: FnOnce(ArchiveAction, PathBuf,
bool, mpsc::Sender<OperationProgress>) -> OperationResult`, giving that one
workflow a *second*, progress-carrying channel
(`mpsc::Sender<OperationProgress>` / `OperationProgress::CleanupStarted`) in
addition to the result channel. No other workflow has a progress channel —
Sources scans, platform bulk-assignment, Doctor runs, etc. only report
started/finished via the result channel, with no intermediate progress.

**No shared typed command/result envelope exists.** Each workflow defines
its own request enum (`OperationRequest`, `AppOperationRequest`,
`SourceAction`, `PlatformAction`, `BulkPlatformActionKind`,
`LibraryViewAction`, ...), its own outcome type, and its own
success/failure formatting. This is the single largest piece of missing
orchestration infrastructure for the campaign (see §11, GUI-ARCH milestone).

## 6. Database boundaries

Per `docs/architecture.md` and ADR 0001, the SQLite-backed `Database`
(`database.rs`) is **additive, never load-bearing for safety**: it caches
what a live filesystem scan would discover, but mount, unmount,
lazy-unmount, and cleanup code paths read live filesystem/mount state
directly and do not depend on the catalogue. Any new GUI orchestration must
preserve this: cache-derived view models are for display and queueing only,
never the source of truth for whether a mount/unmount is actually safe to
perform.

## 7. Source and catalogue boundaries

Source folders are represented by `SourceFolderConfig`/`SourceFolderRecord`
and persisted outside the SQLite database (see
`{load,save,parse}_source_folder_configs*`, `main.rs`'s `SourceAction`).
Scanning (`scan_source_folder_at`, `scan_all_enabled_sources_at`) updates the
catalogue via `database::scan_and_persist[_folders]`, returning
`ScanPersistSummary`/`ScanRunCounts`/`CompletedScanSummary` with per-archive
`ArchiveUpsertOutcome`/`ArchiveObservationKind`/`ArchiveChangeKind`. Missing
records (`MissingArchiveRemovalResult`) and duplicates
(`catalogue_filename_duplicates` → `CatalogueDuplicateReport`) are computed
over `Vec<PersistedArchive>`, not stored as separate flags. Manual platform
assignment is persisted via `PlatformProvenanceDetails`/
`AutomaticPlatformDetails`/`BulkPlatformAssignmentResult` with a distinct
`MANUAL_PLATFORM_SOURCE` provenance constant vs. `CUSTOM_FOLDER_ALIAS_SOURCE`.

## 8. Mount safety boundaries

`MountBackend` trait + `RatarmountBackend` impl abstract the actual mount
tool. `mount_one_archive_with_backend` / `unmount_one_archive_with_backend`
/ `mount_archives_with_backend` / `unmount_archives_with_backend` are the
tested entry points; `MountBatchTargetValidation` /
`MountBatchTargetSkipReason` encode pre-flight per-target validation for
batch mounts (collision handling, already-mounted, etc.), and
`MountOneOutcome`/`UnmountOneOutcome` carry per-item results.
`LazyUnmountTool`/`LazyUnmountResult`/`LazyUnmountCleanupResult` and
`cleanup_selected_mount_dir`/`cleanup_selected_mount_tree`/
`clean_mount_root` cover stale/lazy cleanup. These are exercised by both the
CLI and the GUI's mount/unmount workflows today — this is the
best-integrated vertical in the codebase and the strongest precedent for
the Mount/Active-Mounts milestone.

## 9. Operation history boundaries

**No cross-session or cross-process operation history exists in core.**
`OperationHistory` is a GUI-only, in-memory, capped ring buffer
(`main.rs:280`). The CLI has no equivalent. The only durable, on-disk
"history" in the whole system is the RetroArch cheat-install/rollback
journal (`cheat_installer::CHEAT_INSTALL_RUNS_DIRECTORY_NAME`,
`cheat_rollback::CHEAT_ROLLBACK_RUNS_DIRECTORY_NAME`,
`cheat_history::discover_cheat_history` /
`inspect_cheat_install_journal`), which is scoped to cheat operations only,
not general mount/scan/doctor activity. The design's "History & Logs" screen
(with Export Log, Clear Logs, filtering by operation/date/result) has no
general-purpose backend to draw on beyond the in-memory GUI panel — this is
a genuine gap, not just missing wiring (see §11).

## 10. Doctor diagnostic boundaries

`run_doctor[_default]` / `run_doctor_read_only[_default]` return a
`DoctorReport` (`DoctorCheck` entries with `DoctorStatus`). A parallel,
older `ConfigCheckReport`/`SetupDiagnostics` pair exists
(`run_config_check*`, `run_setup_diagnostics*`) — the GUI's
`ToolsOverlay::Diagnostics` and `ToolsOverlay::DoctorChecks` overlays appear
to correspond to these two related-but-distinct report types; this
duplication should be resolved (or at minimum clearly documented) before
building the design's single "Doctor" screen, which shows one unified list
of severity-tagged checks with suggested actions.

## 11. RetroArch safety boundaries

The `patch_manager` RetroArch surface is the most feature-complete *and*
most GUI-absent part of the system:

- Profile discovery/selection: `retroarch_cheat_setup::{
  discover_retroarch_cheat_setup_profiles, resolve_retroarch_cheat_setup_profile}`
  → `RetroArchCheatSetupProfile`/`RetroArchCheatSetupProfileState`/
  `RetroArchCheatSetupProfileBlocker`.
- Matching/preview: `cheat_catalogue::{match_cheat_game_record,
  build_cheat_availability_report}`, `retroarch_cheat_setup::
  build_retroarch_cheat_setup_plan` → `RetroArchCheatSetupPlan`/
  `RetroArchCheatSetupPreview`/`RetroArchCheatSetupPreviewSummary`.
- Install: `cheat_installer::execute_cheat_install_run` (writes cheat files
  + backups + journal), destination safety gated by
  `destination_safety::{assess_destination, construct_safe_destination,
  validate_destination_root}`.
- History/inspect/rollback: `cheat_history::{discover_cheat_history,
  inspect_cheat_install_journal}`, `cheat_rollback::
  execute_cheat_rollback_run`.
- Trusted-source retrieval: `cheat_sources::{trusted_retroarch_cheat_sources,
  list_retroarch_cheat_sources, fetch_retroarch_cheat_source,
  inspect_retroarch_cheat_source[_snapshot]}` using
  `HttpsCheatSourceTransport` over `ureq` (blocking HTTP — **must** be
  wrapped in a background thread before any GUI call, exactly like the
  existing mount/scan workflows; there is no async runtime in this
  workspace).

All of this is CLI-reachable today (`retroarch-cheat-*` subcommands) and has
dedicated core test modules (`cheat_catalogue/tests.rs`,
`cheat_history/tests.rs`, `cheat_installer/tests.rs`,
`cheat_install_result/tests.rs`) — it is backend-complete, GUI-missing. This
is the largest single bloc of "backend complete, not integrated" work in
the campaign (Milestone 4).

## 12. Trusted-source retrieval boundaries

`trusted_retroarch_cheat_sources()` returns the fixed, code-defined trusted
source list (no user-supplied URLs); `fetch_retroarch_cheat_source` performs
the actual HTTPS fetch with byte/record/depth/string limits
(`MAX_METADATA_BYTES`, `MAX_METADATA_RECORDS`, `MAX_JSON_DEPTH`,
`MAX_METADATA_STRING_BYTES`) and caches to
`default_cheat_source_cache_root()`. `CheatSourceFreshness` /
`CheatSourceFetchStatus` distinguish fresh/cached/stale outcomes.
`inspect_retroarch_cheat_source[_snapshot]` allows offline reuse without
re-fetching. GUI integration must not introduce a second trust boundary
(e.g. a free-text URL field) — only the existing trusted list may be
surfaced.

## 13. Immutable-cache boundaries

`CheatCatalogueSnapshot`/`load_cheat_catalogue_snapshot` and
`CheatSourceFetchResult`/cache metadata (`CheatSourceCacheMetadata`) form an
immutable, versioned snapshot model
(`CHEAT_CATALOGUE_FORMAT_VERSION`, `CHEAT_SOURCE_RESULT_SCHEMA_VERSION`).
Archive-side, `ArchiveIndex`/`ArchiveSnapshot`
(`build_archive_index`/`write_archive_index`/`read_archive_index`) is the
analogous immutable JSON cache for the library catalogue, with
`ArchiveIndexFreshness` for staleness checks. Both are read-only once
written; GUI code should read them through the existing `read_*`/`load_*`
functions, never hand-roll new cache files.

## 14. Existing reusable APIs

See §2–§13; in one line: nearly all core state transitions the design
screens need already exist as tested, `Result`-returning free functions in
`archivefs-core`, callable synchronously from a background thread exactly
as the CLI and existing GUI code already do. Full inventory belongs in
`docs/GUI_BACKEND_CAPABILITY_MATRIX.md`, not duplicated here.

## 15. Missing orchestration APIs

- No shared typed "command → progress/result" envelope (every GUI workflow
  hand-rolls request/response/progress types and its own channel pair).
- No general operation-history persistence (only the GUI's in-memory ring
  buffer and the RetroArch cheat journal, which is domain-specific).
- No unified Doctor report (two parallel report shapes:
  `DoctorReport` vs. `ConfigCheckReport`/`SetupDiagnostics`).
- No "Active Mounts" query — mount state is currently read by filtering the
  full archive record list (`record.mount_state == MountState::Mounted`);
  there is no direct "list current mounts" API distinct from "list all
  archives with mount state."
- No cancellation support anywhere in `archivefs-core`'s mount/scan/fetch
  functions — cancellation, where offered at all, must be UI-level
  (disable input, let the operation finish, discard the result).

## 16. Missing GUI capabilities

Per §3 and the design-reference screen list (`Mount`, `Selected`, `Active
Mounts`, `Library`, `Sources`, `Doctor`, `History & Logs`, `Settings`,
`About`), the GUI currently has no dedicated `Mount` preview screen, no
`Active Mounts` screen, no `History & Logs` screen, no `Settings` screen, no
`About` screen, and zero RetroArch/cheat UI. `Library`, `Sources`, `Doctor`
(as an overlay), and mount/unmount actions exist in some form and should be
extended/relocated rather than rebuilt (see §17).

## 17. Architectural risks

- **God-file risk**: `archivefs-gui/src/main.rs` is already 25k lines with
  no submodules. Adding 5+ new screens and a RetroArch vertical into the
  same file will make it unmaintainable. Splitting `main.rs` into modules
  (by screen/feature) is a prerequisite, not an optional cleanup — but must
  be done as a mechanical extraction (move code, do not rewrite it) to
  avoid conflating refactor risk with feature risk.
- **Channel-per-feature sprawl**: the existing `mpsc`-per-call-site pattern
  (§5) does not scale to ~9 more screens without either (a) a shared
  worker/command abstraction or (b) linear growth of near-duplicate
  boilerplate. This is the crux of the Milestone 1 "application-service
  foundation" decision.
- **Two Doctor report shapes** (§10) risk the new Doctor screen either
  picking the wrong one or needing to merge them under time pressure.

## 18. Dependency risks

- No async runtime (`tokio`, etc.) is present; all core I/O is synchronous
  (`ureq`, `std::fs`, `std::process::Command`, `rusqlite`-style DB calls).
  The GUI's only concurrency primitive is `std::thread::spawn` + `mpsc`.
  Any orchestration layer must stay within this model — do not introduce
  an async runtime as part of this campaign (the instructions also forbid
  adding dependencies without explicit approval).
- `eframe 0.32.3`, `arboard 3.6.1` (with `wayland-data-control`), `rfd
  0.17.2` are the only GUI-specific dependencies; both `arboard` and `rfd`
  feature choices are already tuned for a specific Wayland/X11 bug (see
  inline comments in `crates/archivefs-gui/Cargo.toml`) — do not touch
  their feature flags incidentally while restructuring the crate.

## 19. Migration risks

- Splitting `main.rs` into modules will touch every line of the file by
  definition (moves, not edits) — this must be its own commit(s), clearly
  separated from behavioral changes, so `git blame`/bisect stays useful.
- Any new shared command/progress envelope that replaces the ad-hoc
  per-feature channels must be introduced incrementally (new workflows
  first, or one migrated workflow at a time) rather than as a big-bang
  rewrite of all ~14 existing call sites at once.

## 20. Concurrency risks

- The existing progress model (`OperationProgress::CleanupStarted` only,
  §5) is too coarse for the design's mount/unmount progress bars and
  RetroArch install progress. A richer progress enum is needed, but must
  stay `Send`-safe across the existing `mpsc::Sender` boundary and must not
  assume operations can report *percentage* progress — most core functions
  are single blocking calls with no internal progress hooks (only
  lazy-unmount already has an internal
  `mpsc::Sender<OperationProgress>` argument at `main.rs:4020`'s call
  site — see §19 in the CLI's `lazy_unmount_one_archive_path_with_progress`
  at `lib.rs:5708`).
- Nothing in `archivefs-core` is cancellable mid-flight (§15). "Cancel" in
  the design (Mount screen "Cancel" button) can only mean "stop waiting /
  hide the UI," not "abort the underlying `ratarmount`/fetch process,"
  unless new core support is added — which should be flagged as an explicit
  non-goal unless a future milestone scopes it deliberately.

## 21. Proposed application-service boundary

Introduce a small `archivefs-gui::app_service` (or similarly named) module —
**not a new crate** — that:

1. Defines one typed `Command` enum (variants per orchestration action,
   e.g. `Command::MountOne(PathBuf)`, `Command::FetchCheatSource(SourceId)`)
   and one typed `CommandOutcome` enum, replacing the current per-feature
   request/response type sprawl (§5, §15) incrementally.
2. Owns a single place where `thread::spawn` + `mpsc::channel()` happen,
   parameterized by the command, so new screens do not hand-roll another
   channel pair.
3. Reuses the existing `OperationProgress`-style channel for progress,
   generalized to carry a small, closed set of progress events (started /
   substep / cleanup / finished) rather than one-off variants per feature.

This is additive: existing working workflows (mount/unmount, sources,
platform assignment) do **not** need to be ported on day one. New screens
(RetroArch, Active Mounts, History) should be built on the new service from
the start; old workflows migrate opportunistically, each as its own
reviewable commit.

## 22. Proposed view-model boundary

Each design screen gets a plain-data view-model struct built by a pure
function from core types (mirroring the existing `SourceFolderView`,
`ArchiveRow`, `HealthDashboardFilters` pattern already in `main.rs`) —
e.g. `ActiveMountsView::from(&[ArchiveRecord])`,
`RetroArchSetupView::from(&RetroArchCheatSetupPlan)`. These structs contain
no core types with I/O side effects, only display-ready data, so egui
rendering code never touches `archivefs-core` types directly. This matches
the existing, working pattern for `ArchiveRow`/`SourceFolderView` — extend
it, do not replace it.

## 23. Proposed progress and error model

- Errors: keep `archivefs-core`'s existing `Result<T, ArchiveFsError>` /
  domain-specific error enums (`PatchManagerError`,
  `CheatSourceError`, `DestinationSafetyError`, ...) as the source of
  truth; add one GUI-side `DisplayError { message, more_information:
  Option<String> }` mapping (the existing `ActionFeedback.more_information`
  field at `main.rs:4867` is already this shape for the mount/unmount path
  — generalize it, don't reinvent it).
- Progress: a closed `ProgressEvent` enum (see §21) shared by the new
  application-service boundary; screens that cannot report meaningful
  progress (most Doctor/Sources/RetroArch calls, which are single blocking
  functions) simply skip intermediate events and only send start/finish,
  exactly as most existing workflows already do.

## 24. Recommended implementation order

Milestone 0 (this initializer) is complete. For Milestones 1+, evidence
favors this refinement of the proposed shape:

1. **Milestone 1 — GUI application-service foundation.** Build the
   `Command`/`CommandOutcome`/`ProgressEvent` types and the shared
   spawn-and-poll plumbing (§21–23) *and* do the mechanical `main.rs`
   module split (§17) in the same milestone, since new screens should land
   in their own modules from the start rather than being added to the
   monolith and re-split later.
2. **Milestone 2 — Library, Sources, Doctor, History.** These have the most
   existing GUI code to extend (Library, Sources) or a single overlay to
   promote to a full screen (Doctor). History is the one genuinely new
   backend surface here (§9) — scope it to what's achievable without a new
   persistence layer first (e.g. persist the existing in-memory
   `OperationHistory` to a small JSON log file) unless evidence during the
   milestone shows more is needed.
3. **Milestone 3 — Mount / Active Mounts / Unmount vertical.** This is the
   best-precedented vertical (§8) — lowest risk, highest existing test
   coverage, good place to validate the new application-service layer
   before applying it to the riskier RetroArch surface.
4. **Milestone 4 — RetroArch trusted-source, setup, install, history,
   rollback.** Highest backend-to-GUI gap (§11), and the only place with a
   synchronous-network-call-must-be-backgrounded constraint (§11, §18) —
   sequence after Milestone 1's worker infrastructure exists, not before.
5. **Milestone 5 — Settings, About, final consistency.** Lowest backend
   risk; mostly reading existing config/environment data
   (`ConfigIdentity`, `default_database_path`, `default_config_path`,
   `command_available`) plus the "Check for Updates"/"Export Support
   Bundle" design affordances, which have **no backend today** and must be
   scoped down or marked intentionally unsupported (see
   `GUI_BACKEND_CAPABILITY_MATRIX.md`).
6. **Milestone 6 — Hardening, acceptance, release readiness.** Unchanged
   from the proposed shape.

This order is the proposed sequence with two adjustments: the `main.rs`
split is pulled into Milestone 1 (not left implicit), and Milestone 2's
History scope is explicitly capped pending a persistence decision.

## 25. Strict non-goals

- No async runtime, no new HTTP/mount/database dependency (§18).
- No new user-supplied trusted-source URLs (§12) — only the existing fixed
  trusted list.
- No mid-flight cancellation of core operations unless a future milestone
  explicitly adds core-level cancellation support (§20) — UI-level "cancel"
  (hide/ignore result) only.
- No rewrite of `MountBackend`/`RatarmountBackend` or the SQLite schema.
- No replacement of the `MainView`/`ToolsOverlay` navigation model — extend
  it (§3, §17).
- No pushing the campaign harness (`fable-preflight.sh`, campaign docs)
  into being load-bearing for production builds — they are dev-only.

## 26. Testing strategy

- Core logic changes (if any are ever needed to support a GUI feature, e.g.
  a new "list active mounts" query) get unit tests in
  `archivefs-core`, colocated per existing convention
  (`patch_manager/*/tests.rs` submodules).
- GUI view-model construction functions (pure `From`/`fn build_*_view`
  functions per §22) are unit-testable without eframe — follow the existing
  pattern of the ~347 `#[test]` functions already in `archivefs-gui/src/main.rs`.
- Full end-to-end GUI interaction is not unit-testable (immediate-mode, no
  existing UI test harness) — acceptance for visual/interaction correctness
  is manual, screen-by-screen, against the design reference.

## 27. Acceptance strategy

Each milestone's features (tracked in `docs/FABLE_FEATURES.json`) move from
`not_started` → `complete` only when: the relevant core capability is
verified present (or newly added with tests), the GUI wiring compiles and
is exercised by at least one automated test where feasible, and a manual
pass against the design reference confirms the interaction matches intent
(or a documented, deliberate deviation).

## 28. Milestone boundaries

Each milestone must land as an independently buildable, independently
testable commit (or small commit series) — never a partial screen half-wired
across milestones. `docs/FABLE_PROGRESS.md` is the durable record of what
landed in which milestone.
