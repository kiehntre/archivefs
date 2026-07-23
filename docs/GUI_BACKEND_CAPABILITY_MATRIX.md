# GUI ↔ Backend Capability Matrix

Maps each design-reference screen (`design-reference/ArchiveFS design stage
one.zip` — screens confirmed present by text search: Mount, Selected,
Active Mounts, Library, Sources, Doctor, History & Logs, Settings, About) to
the current state of `archivefs-core`/`archivefs-gui` as of campaign base
commit `fd0b4b143d64d9f8d681054eb60e8b4b8a41edd6`.

Status legend used throughout:

- **Integrated** — implemented in `archivefs-core` and already wired into
  `archivefs-gui`.
- **Backend complete, not integrated** — implemented and tested in
  `archivefs-core` (and usually CLI-reachable), no GUI code references it.
- **Partial** — some sub-interactions integrated, others not.
- **Prototype-only** — appears in the design with no backend of any kind.
- **Intentionally unsupported** — a deliberate non-goal (see
  `FABLE_CAMPAIGN_PLAN.md` §25); should not be built even if a backend
  could technically be added.

---

## Library

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Listing | Full archive table | `ArchiveRecord`, `current_archive_records` | `lib.rs` | `library-list` | core + gui tests | none | none | read-only | no | `LoadedData`/`ArchiveRow` (existing) | library-integration | Integrated |
| Search | Free-text query | `find_archive_index_entries` / in-memory filter | `lib.rs` | `library-find` | core tests | none | none | read-only | no | existing search field | library-integration | Integrated |
| Filters (platform/health/presence) | Filter chips | `LibraryRowFilters`, `classify_archive_health` | `main.rs`, `lib.rs` | n/a (GUI-only) | gui tests | verify chip-for-chip design parity | none identified yet | read-only | no | existing `LibraryRowFilters` | library-integration | Partial (parity unverified) |
| Selection | Row multi-select | GUI-local selection state | `main.rs` | n/a | gui tests | none | none | n/a | no | existing | library-integration | Integrated |
| Inspection | Archive Inspector overlay | `inspect_archive[_with_limit]` | `inspector.rs` | `info` | core tests | none | none | read-only, entry-count capped (`INSPECTOR_ENTRY_LIMIT`) | no | existing `ToolsOverlay::ArchiveInspector` | library-integration | Integrated |
| Manual platform assignment | "DETECTED PLATFORM / OVERRIDE" | `PlatformAction`, manual provenance | `lib.rs`, `main.rs` | `library-set-platform[-bulk]` | core + gui tests | none | none | must not silently clobber automatic provenance | short blocking op | existing `RunningPlatformAction` | library-integration | Integrated |
| Missing records | "ARCHIVES / MISSING" | `MissingArchiveRemovalResult` | `lib.rs` | `library-remove-missing` | core tests | none | none | catalogue-only deletion, never touches files | short blocking op | existing `RunningMissingRemoval` | library-integration | Integrated |
| Duplicates | "DUPLICATE GROUP" dashboard | `catalogue_filename_duplicates`, `CatalogueDuplicateReport` | `lib.rs` | `duplicates` | core + gui tests | none | none | read-only | no | existing `MainView::Duplicates` | library-integration | Integrated |
| Refresh | Reload catalogue view | `scan_source_folder_at` / DB reload | `lib.rs`, `main.rs` | `scan` | core + gui tests | migrate onto GUI-ARCH command envelope | none | none | background thread | existing `DatabaseState` reload | library-integration | Integrated (migration pending) |

## Sources

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Listing | Source table | `SourceFolderView`, `build_source_folder_views` | `lib.rs` | `sources` | core tests | none | none | read-only | no | existing `MainView::Sources` | sources-integration | Integrated |
| Availability | "All Sources" state badges | `SourceAvailability`, `classify_source_availability` | `lib.rs` | `sources` | core tests | none | none | read-only | no | existing | sources-integration | Integrated |
| Add | "Add Source" (folder picker) | `add_source_folder_at`, `validate_new_source_folder` | `lib.rs` | `source add` | core tests | none | none | path validation must stay in core, not re-implemented in GUI | short blocking op | existing `SourcesAddDialogState` | sources-integration | Integrated |
| Enable/disable | "Enabled" toggle | `set_source_folder_enabled_at` | `lib.rs` | `source enable/disable` | core tests | none | none | none | short blocking op | existing `SourceAction` | sources-integration | Integrated |
| Scan one | "Scan Now" | `scan_source_folder_at` | `lib.rs` | `source scan` | core tests | none | none | none | background thread | existing | sources-integration | Integrated |
| Scan all | design "Scan Now"-equivalent for all sources | `scan_all_enabled_sources_at` | `lib.rs` | `sources` (scan-all path) | core tests | verify a GUI "scan all" affordance exists distinct from per-source scan | none | none | background thread | new/extended action | sources-integration | Partial — verify GUI exposes scan-all, not just per-source |
| Remove, keep records | "Remove Source (Keep Records)" | `remove_source_folder_at` (retain mode) | `lib.rs` | `source remove` | core tests | none | none | must not touch files | short blocking op | existing `SourcesRemoveDialogState` | sources-integration | Integrated |
| Remove catalogue records | "Remove Source and Records" | `remove_source_folder_at` (delete-records mode) | `lib.rs`, `RemoveSourceFolderOutcome` | `source remove --delete-records`-style flag | core tests | none | none | catalogue-only, never deletes files on disk | short blocking op | existing | sources-integration | Integrated |
| Filesystem-safety boundaries | n/a (cross-cutting) | `validate_new_source_folder` | `lib.rs` | n/a | core tests | none | none | single source of truth for path safety — GUI must call through, never duplicate | n/a | n/a | sources-integration | Integrated |

## Doctor

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Structured checks | List of named checks | `DoctorCheck`, `run_doctor[_read_only]` | `lib.rs` | `doctor` | core tests | reconcile with `ConfigCheckReport`/`SetupDiagnostics` (two parallel shapes) | none | read-only variants preferred (`_read_only`) | background thread recommended | promote `ToolsOverlay::DoctorChecks` to full screen | doctor-integration | Partial — exists as overlay, not full screen; report-shape reconciliation open |
| Severity | Status/severity tagging | `DoctorStatus` | `lib.rs` | `doctor` | core tests | none | none | none | n/a | existing | doctor-integration | Integrated |
| Technical details | "Copy Full Details" | `DoctorCheck` fields | `lib.rs` | `doctor --json`-style | core tests | none | none | none | n/a | existing overlay shows details | doctor-integration | Integrated |
| Suggested action | "Retry Where Safe" | `DoctorCheck` (advisory text) | `lib.rs` | `doctor` | core tests | none | verify every check carries an actionable suggestion; if not, is a design-only expectation | none | n/a | doctor-integration | Partial |
| Database diagnostics | "DATABASE SCHEMA" section | `check_database_health`, `DatabaseHealthReport`, `DatabaseDiagnostic` | `database.rs` | `database-check` | core tests | none | none | read-only | no | existing `DatabasePanelAction` | doctor-integration | Integrated |
| Mount diagnostics | Mount-related checks | `DoctorCheck` covering mount tool availability (`command_available`) | `lib.rs` | `doctor` | core tests | none | none | read-only | no | new/extended | doctor-integration | Partial |
| RetroArch diagnostics | Environment checks | `emulator_environment::retroarch` | `emulator_environment/retroarch.rs` | `retroarch-environment` | core tests | not exposed in Doctor screen today; only reachable via CLI | none | read-only | no | new | doctor-integration | Backend complete, not integrated |

## History & Logs

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Operation history | "RECENT OPERATIONS" | `OperationHistory` (GUI-local, in-memory only) | `main.rs` | none (CLI has no equivalent) | gui tests | build persistence (JSON log file or new core module — undecided, see `FABLE_PROGRESS.md`) | **general-purpose persisted history does not exist** | must not become load-bearing for safety (mirrors DB additive-only principle) | no | existing `OperationHistory` extended | history-and-logs-integration | Partial — in-memory only, not persisted |
| Inspection | Expand entry detail | `HistoryEntry` fields | `main.rs` | none | gui tests | none | none | none | no | existing | history-and-logs-integration | Integrated (in-memory scope only) |
| Filtering | "All Operations"/"Any Date" filters | GUI-local filtering over `OperationHistory` | `main.rs` | none | partial gui tests | verify filter dimensions match design (operation/date/result) | none | none | no | new filter state | history-and-logs-integration | Partial |
| Structured results | Success/failure per entry | `ActivityOutcome` | `main.rs` | none | gui tests | none | none | none | no | existing | history-and-logs-integration | Integrated |
| Export feasibility | "Export Log"/"Export Current Logs"/"Export JSON" | none today; would serialize `OperationHistory` entries | `main.rs` (new) | none | none yet | new export function | none (serialization only, no new core dependency) | must redact per "Redact home directory in paths"/"Redact source-folder names" design toggles if implemented | no | new | history-and-logs-integration | Backend complete enough to build (serialize existing struct); redaction options are new |
| Log retention | Cap on stored entries | `HISTORY_LIMIT` (existing constant, in-memory ring buffer) | `main.rs` | none | gui tests | decide retention policy for persisted version | none | none | n/a | existing, extend | history-and-logs-integration | Partial |
| RetroArch cheat journal (distinct history source) | n/a in generic screen, but overlaps | `discover_cheat_history`, `inspect_cheat_install_journal` | `patch_manager/cheat_history.rs` | `retroarch-cheat-history`, `retroarch-cheat-inspect` | core tests | decide merge vs. separate presentation | none | read-only | no | new | installation-history | Backend complete, not integrated |
| Unsupported prototype interactions | any log-tailing/live-follow UI beyond simple list refresh | none | n/a | n/a | n/a | n/a | true "tail -f"-style live log streaming has no backend | n/a | n/a | n/a | history-and-logs-integration | Intentionally unsupported unless scoped later |

## Mount

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Preview | Destination + command preview before mount | `MountPlan`, `plan_mounts`, `safe_mount_name` | `lib.rs` | `mount --dry-run`-equivalent output | core tests | new dedicated Mount screen (currently folded into Library selected-archive panel) | none | preview must never mount | no | new screen | mount-preview | Backend complete, not integrated as standalone screen |
| Destination validation | Path safety before mount | `MountBatchTargetValidation` | `lib.rs` | `mount`/`mount-one` | core tests | surface in preview UI | none | none | no | new | mount-preview | Backend complete, not integrated |
| Collision handling | Existing mount / name clash | `MountBatchTargetSkipReason` | `lib.rs` | `mount` | core tests | surface every skip reason as a preview message | none | none | no | new | mount-preview | Backend complete, not integrated |
| Single mount | "Mount Selected (Ctrl+M)" | `mount_one_archive_with_backend` | `lib.rs` | `mount-one` | core + gui tests | migrate to command envelope | none | goes through `MountBackend` only | background thread | existing `ArchiveAction::Mount` | mount-execution | Integrated |
| Batch mount | "Add to Mount Queue" then execute | `mount_archives_with_backend`, `MountAllResult` | `lib.rs`, `main.rs` | `mount` (all) | core + gui tests | migrate progress to shared `ProgressEvent` | none | same as single mount, per-item | background thread | existing `RunningMountAll` | batch-mount-outcomes | Integrated |
| Partial success | Some items fail, others succeed | `MountAllResult` (successes/failures/skipped) | `main.rs` | `mount` (partial) | gui tests | none | none | none | n/a | existing | batch-mount-outcomes | Integrated |
| Progress | Per-item progress during batch mount | `MountAllEvent`, `MountAllProgress` | `main.rs` | n/a | gui tests | generalize to shared `ProgressEvent` | none | none | n/a | existing, extend | batch-mount-outcomes | Integrated (bespoke type, pending generalization) |
| Cancellation | "Cancel" button on Mount screen | none — no core cancellation support anywhere | n/a | n/a | n/a | UI-level cancel only (hide/ignore result); true abort would need new core support | **no core cancellation exists** | must not leave a partially-mounted/undefined state if UI "cancels" while a thread is still running | n/a | UI-only cancel semantics | mount-execution | Intentionally unsupported (true cancel) unless scoped later |

## Active Mounts

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Refresh | Reload active-mount list | `current_statuses` filtered by `MountState::Mounted` | `lib.rs` | `status` | core tests | new dedicated screen (today, mount state is only shown inline on Library rows) | **no direct "list active mounts" query** — must filter the full record list | read-only | no | new screen | active-mounts-integration | Backend complete (via filter), not integrated as standalone screen |
| Normal unmount | "Unmount" | `unmount_one_archive_with_backend` | `lib.rs` | `unmount-one` | core + gui tests | migrate to command envelope | none | goes through `MountBackend` only | background thread | existing `ArchiveAction::Unmount` | normal-unmount | Integrated |
| Lazy unmount | "Lazy Unmount" | `lazy_unmount_one_archive_path_with_progress` | `lib.rs` | none (GUI-only today; no CLI lazy-unmount subcommand) | core + gui tests | none | none | must fall back safely if lazy tool unavailable (`LazyUnmountTool`) | background thread, has progress channel | existing | lazy-unmount | Integrated |
| Cleanup | "Cleanup" | `cleanup_selected_mount_dir/_tree`, `clean_mount_root` | `lib.rs` | `clean` | core + gui tests | none | none | only removes empty/stale mount-point directories | background thread | existing `CleanupOutcome` | stale-mount-recovery | Integrated |
| Remount offer | "Remount" | `remount_one_archive_path` | `lib.rs` | none (GUI-only) | core tests | wire into Active Mounts screen (currently no screen exists to host it) | none | same safety as mount | background thread | new | active-mounts-integration | Backend complete, not integrated |
| Stale mount recovery | Detect + recover orphaned mounts | `cleanup_selected_mount_tree`, Doctor mount checks | `lib.rs` | `clean`, `doctor` | core tests | cross-link Doctor's mount checks into Active Mounts' recovery action | none | none | background thread | new | stale-mount-recovery | Backend complete, not integrated |

## RetroArch

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Trusted source list | Fixed list only | `trusted_retroarch_cheat_sources` | `patch_manager/cheat_sources.rs` | `retroarch-cheat-source-list` | core tests | build GUI listing | none | no user-supplied URLs | no | new | trusted-retroarch-source-workflow | Backend complete, not integrated |
| Fetch | Download + cache a trusted source | `fetch_retroarch_cheat_source` (blocking `ureq`) | `patch_manager/cheat_sources.rs` | `retroarch-cheat-source-fetch` | core tests | **must** be backgrounded before any GUI call — synchronous network I/O | none | byte/record/depth/string limits already enforced in core | background thread (mandatory) | new | trusted-retroarch-source-workflow | Backend complete, not integrated |
| Inspect | View a cached source | `inspect_retroarch_cheat_source[_snapshot]` | `patch_manager/cheat_sources.rs` | `retroarch-cheat-source-inspect` | core tests | build GUI view | none | read-only | no | new | trusted-retroarch-source-workflow | Backend complete, not integrated |
| Offline reuse | Work without network | `inspect_retroarch_cheat_source_snapshot`, `CheatSourceFreshness` | `patch_manager/cheat_sources.rs` | `retroarch-cheat-source-inspect` | core tests | none | none | none | no | new | trusted-retroarch-source-workflow | Backend complete, not integrated |
| Local catalogue | Cheat catalogue snapshot | `load_cheat_catalogue_snapshot`, `CheatCatalogueSnapshot` | `patch_manager/cheat_catalogue.rs` | `retroarch-cheat-catalogue` | core tests | none | none | read-only, versioned (`CHEAT_CATALOGUE_FORMAT_VERSION`) | no | new | catalogue-provenance | Backend complete, not integrated |
| Profile discovery | "Discovered Profiles" | `discover_retroarch_cheat_setup_profiles` | `patch_manager/retroarch_cheat_setup.rs` | `retroarch-cheat-setup` | core tests | build GUI listing | none | read-only | no | new | retroarch-profile-selection | Backend complete, not integrated |
| Profile selection | Choose a profile | `resolve_retroarch_cheat_setup_profile` | `patch_manager/retroarch_cheat_setup.rs` | `retroarch-cheat-setup` | core tests | surface `RetroArchCheatSetupProfileBlocker` reasons | none | none | no | new | retroarch-profile-selection | Backend complete, not integrated |
| Matching | Archive-to-catalogue match | `match_cheat_game_record` | `patch_manager/cheat_catalogue.rs` | `retroarch-cheat-catalogue` | core tests | build GUI view | none | read-only | no | new | cheat-preview | Backend complete, not integrated |
| Preview | Install plan preview | `build_retroarch_cheat_setup_plan` | `patch_manager/retroarch_cheat_setup.rs` | `retroarch-patch-preview` | core tests | build GUI view | none | must not write files | no | new | cheat-preview | Backend complete, not integrated |
| Install | Execute install | `execute_cheat_install_run` | `patch_manager/cheat_installer.rs` | `retroarch-cheat-install` | core tests | migrate to command envelope, backgrounded | none | must go through `destination_safety` exactly as CLI does | background thread | new | cheat-installation | Backend complete, not integrated |
| Replacement backup | Backup before overwrite | `CHEAT_INSTALL_BACKUPS_DIRECTORY_NAME`, `PreviousDestinationState` | `patch_manager/cheat_installer.rs`, `cheat_install_result.rs` | `retroarch-cheat-install` | core tests | surface backup path in GUI result | none | never skip backup on replacement | n/a | new | cheat-installation | Backend complete, not integrated |
| Journal | Install run journal | `CHEAT_INSTALL_RUNS_DIRECTORY_NAME`, `CheatInstallRun` | `patch_manager/cheat_installer.rs`, `cheat_install_result.rs` | `retroarch-cheat-install` | core tests | none | none | append-only | n/a | new | installation-history | Backend complete, not integrated |
| History | Journal history listing | `discover_cheat_history` | `patch_manager/cheat_history.rs` | `retroarch-cheat-history` | core tests | build GUI listing | none | read-only | no | new | installation-history | Backend complete, not integrated |
| Inspect | Journal detail inspection | `inspect_cheat_install_journal` | `patch_manager/cheat_history.rs` | `retroarch-cheat-inspect` | core tests | build GUI view | none | read-only | no | new | installation-history | Backend complete, not integrated |
| Rollback | Undo an install | `execute_cheat_rollback_run`, gated by `CheatRollbackAvailability` | `patch_manager/cheat_rollback.rs` | `retroarch-cheat-rollback` | core tests | migrate to command envelope, backgrounded | none | only offered when `CheatRollbackAvailability` allows it | background thread | new | rollback | Backend complete, not integrated |
| Post-install guidance | Result messaging | `CheatInstallSummary`, `CheatRollbackSummary` | `patch_manager/cheat_install_result.rs`, `cheat_rollback_result.rs` | `retroarch-cheat-install`/`-rollback` | core tests | build GUI messaging per status variant | none | none | n/a | new | cheat-installation / rollback | Backend complete, not integrated |

## Settings and About

| Interaction | Design intent | Backend capability | Module/type/API | CLI precedent | Existing tests | Missing orchestration | Missing backend | Safety requirements | Blocking? | Recommended GUI state | Milestone | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Backend-supported settings | Source folders, mount root, config path | `Config`, `parse_config`, `default_config_path`, `save_source_folder_configs_to` | `lib.rs` | `config-check` | core tests | build new Settings screen | none | writes must go through existing `save_source_folder_configs_to`/config-write paths only | short blocking op | new | settings-integration | Backend complete, not integrated |
| Prototype-only settings | "Comfortable/Compact" density, "Reset All Settings" | none | n/a | n/a | n/a | n/a | **no backend; GUI-preference-only or entirely absent** | n/a | n/a | n/a | settings-integration | Prototype-only / intentionally unsupported unless scoped as pure GUI-local prefs |
| Paths and environment info | "DATA PATH", "CONFIGURATION PATH", "DESKTOP ENVIRONMENT" | `default_database_path`, `default_config_path`, `ConfigIdentity` | `lib.rs` | `config-check` | core tests | build About screen | none | read-only | no | new | about-and-support-information | Backend complete, not integrated |
| Support-bundle feasibility | "Export Support Bundle" | none (would need to compose Doctor report + config summary + redacted logs) | n/a | n/a | n/a | new export composer | **no bundling backend exists** | must respect redaction toggles (home dir, source-folder names) | no | new, scoped-down | about-and-support-information | Prototype-only — scope a minimal version or defer |
| Update-check feasibility | "Check for Updates" | none | n/a | n/a | n/a | n/a | **no update-check backend or mechanism exists**; would require network access and a release-channel concept not present today | must not silently phone home | n/a | n/a | about-and-support-information | Intentionally unsupported unless scoped later as its own decision |

---

## Summary of genuinely missing backend capability (not just unwired GUI)

1. No general-purpose persisted operation-history store (History & Logs).
2. No direct "list active mounts" query (works today only by filtering all
   records — acceptable to reuse, not a hard blocker).
3. No update-check mechanism.
4. No support-bundle composer.
5. No mid-flight cancellation for any long-running core operation.

Everything else under RetroArch, Mount, Active Mounts, Doctor, Sources, and
Library is backend-complete and only needs GUI wiring.
