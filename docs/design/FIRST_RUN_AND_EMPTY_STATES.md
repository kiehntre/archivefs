# First Run and Empty States

Status: implemented on `feature/first-run-empty-state-polish`. This document
describes what shipped, not a proposal.

## 1. Where the codebase already stood

Before this branch, most of EmuWiz's empty-state handling was already
correct: the DAT source registry and cheat source preferences file both
default gracefully when their file is absent
(`crates/archivefs-core/src/dat/sources/config.rs`,
`crates/archivefs-core/src/patch_manager/cheat_source_registry/config.rs`),
platform artwork treats a missing custom-artwork directory as "nothing custom
yet" rather than an error (`crates/archivefs-core/src/platform_artwork.rs`),
and the GUI already has a shared `widgets::empty_state()` component used
throughout Library, Sources, DAT Sources, Cheat Sources, and Gamer View for
"nothing here yet" messaging with an optional call-to-action button.

The one place that was not first-run-aware was the setup/diagnostics model
that gates the Setup screen and feeds the Doctor page: a **genuinely missing**
configuration file (the normal state of every fresh install) was reported
identically to a **broken** one — every downstream check rendered as a red
"Error", and Doctor showed the same fresh install as multiple `Error`
findings across four different subsystems. That is the gap this branch
closes.

## 2. The first-run state model

`SetupDiagnostics` (`crates/archivefs-core/src/lib.rs`) already distinguished
`config_missing: bool` (confirmed absent, via `PathInspection::Missing`) from
any other read failure (permission denied, wrong file type, or a config that
parses but is otherwise broken). What it lacked was a status distinct from
`Error` to carry that distinction through to every downstream check and into
Doctor's severity model.

`SetupDiagnosticStatus` gained one new variant:

```rust
pub enum SetupDiagnosticStatus {
    Ready,
    NotChecked,   // the check did not run (e.g. a read-only writability probe)
    NotConfigured, // expected first-run absence — not a fault
    Warning,
    Error,
}
```

`run_setup_diagnostics_with_checks` now performs one pass after building its
normal check list: if `config_missing` is true, every check that reads
`Error` *because* nothing is configured yet — "Config file exists", source
folder and mount root checks, "EmuWiz is ready for scanning/actions" — is
downgraded to `NotConfigured`. System-tool checks (`ratarmount is
available`, `fusermount3 or umount is available`) are left untouched:
whether those tools are installed is a fact about the machine, independent of
whether a config file exists, and a genuinely missing tool is still a real
problem on first run.

This distinguishes the states the spec requires:

| State | Detection | Status |
|---|---|---|
| Configuration genuinely missing | `config_missing == true` (confirmed `ENOENT`, not ambiguous) | `NotConfigured` (Info) |
| Configuration exists but malformed | file reads, TOML/field parse fails | `Error` |
| Configuration exists but references missing paths | file reads and parses, but a source folder or mount root does not exist | `Error` (a real misconfiguration, not a first-run state) |
| Empty library / no RomM / no cheat prefs / no DAT sources / no custom artwork | each subsystem's own loader already defaults gracefully | no finding at all |
| Unwritable config/data/cache directory | writability probe fails where it runs | `Warning`/`Error` (never downgraded) |

Malformed configuration is **never** silently overwritten: `create_starter_config`
uses `create_new` and errors if a file already exists there (verified by
`starter_config_creates_parents_and_never_overwrites`), and
`starter_config_available()` in the GUI only offers the "Create Starter
Config" action when `config_missing` is true — never when a file exists but
failed to parse.

## 3. The same softening, propagated to Doctor

Doctor's Finding model (`crates/archivefs-core/src/diagnostics/mod.rs`,
`runner.rs`) merges several independent subsystems, three of which needed the
identical fix so a fresh install does not show the same absence as `Info` in
one place and `Error` in another:

- `setup_status_severity` maps the new `NotConfigured` status to
  `DoctorSeverity::Info`.
- `doctor_check_severity` (replacing `doctor_status_severity`) recognises the
  literal "missing " detail text `complete_doctor_report`'s "config file"
  check emits for a confirmed-absent file, and reports `Info` for exactly
  that case — any other "config file" failure (unreadable, wrong type) stays
  `Error`. `run_doctor_with_mount_root_creation` was tightened to only ever
  emit that "missing " wording when `inspect_path` confirms the path is
  genuinely absent (the same primitive `SetupDiagnostics` already used) —
  before that fix, a broken symlink at the config path was indistinguishable
  from true absence by `.exists()` alone, which would have let this adapter
  soften a real, ambiguous problem to `Info` while `SetupDiagnostics`
  correctly still reported it as `Error`, producing two contradictory
  findings for the same fact.
- `database_severity` treats `DatabaseDiagnosticCode::MissingDatabase` as
  `Info`: a catalogue database that has never been created is the ordinary
  state of a library that has never been scanned, not evidence of damage.
  Every other database diagnostic code is deliberately never downgraded
  (`no_database_error_is_ever_downgraded_below_error`).
- `adapter_failure_finding` in `runner.rs` downgrades to `Info` when a
  gatherer's own input failed for the same reason — its failure text
  contains the OS's ENOENT wording or the database layer's "does not exist"
  phrase — rather than a genuine permission or corruption problem, which
  never contains that wording.

Net effect, verified with `archivefs doctor --findings` against a completely
empty temporary `HOME`: **Critical: 0, Error: 0, Warning: 0** on a fresh
install (all findings are `Info`). Pointing the same binary at a fixture with
a syntactically invalid `config.toml` still reports real `Error` findings —
the softening never fires for a broken config, only a confirmed-absent one.

## 4. Welcome guidance

The Setup/Diagnostics screen (`show_setup_diagnostics` in
`crates/archivefs-gui/src/main.rs`) now shows a compact panel above the
check list whenever `config_missing` is true: EmuWiz is unconfigured,
where to begin (Create Starter Config, then a source folder on the Sources
page), that DAT Sources and Cheat Sources live on their own pages and start
empty, that RomM is optional, and that EmuWiz never renames, moves, or
deletes a ROM without a later, explicit, reviewed action. This is a page
addition, not a new wizard: the existing gated Setup screen with its
"Create Starter Config" / "Create Mount Root" / "Continue to EmuWiz"
actions already matches the "clear start page with direct navigation
actions" shape the spec asks for.

**A missing config is not always a first run.** `SetupDiagnostics` is a pure
recomputation of current filesystem state with no memory of its own, so
`config_missing` alone cannot tell a genuine fresh install apart from a
config that was present and readable earlier in this session and has since
disappeared - which can mean an intentional removal, but can just as easily
mean a deleted file, an unmounted drive, or a bug. Showing the reassuring
"Welcome to EmuWiz, this is expected" copy in the latter case would
actively mislead. `ArchiveFsApp` tracks this with one `bool`,
`config_previously_confirmed`, set the first time `poll_diagnostics` sees a
`Ready` report with `!config_missing`. `show_setup_diagnostics` (via the
pure, directly-tested `missing_config_is_first_run` predicate) shows the
welcome panel only when that flag is still false; once a config has been
seen, a missing config instead shows a distinctly-worded, warning-toned
panel asking the person to check whether it was deleted, moved, or is on an
unmounted drive before creating a new one.

This distinction is deliberately GUI-local, not part of core's Doctor
severity model: `diagnostics/runner.rs` documents `run_doctor_scan` as a
*pure* function of its gathered inputs (enforced by
`the_same_inputs_always_produce_byte_identical_scans`), so severity itself
must never depend on session history. Doctor's Info-severity finding for a
missing config states a fact ("configuration file is missing") without any
narrative claim that this is expected - the risk this section addresses is
specific to the Setup screen's added prose, not to severity coloring.

## 5. Empty states already in place (unchanged by this branch)

Confirmed present and left as-is, since they already meet the spec:

- **Library**: `EMPTY_LIBRARY_MESSAGE` / `ZERO_FILTER_RESULTS_MESSAGE` via
  `LibraryTableMessage`, rendered through `widgets::empty_state()`.
- **Sources**: "No source folders" empty state with an "Add folder" call to
  action.
- **DAT Sources**: "No DAT sources yet" empty state explaining Add file/Add
  folder; a read-only banner is always shown; audit controls are disabled
  until a valid source and target exist; a missing `dat_sources.toml`
  produces an empty registry, never a load error.
- **Cheat Sources**: a missing preferences file loads built-in defaults
  silently; disabled sources remain visible with an explanatory label rather
  than being hidden.
- **Gamer View**: `gamer_empty_list_guidance` distinguishes "no games in your
  library yet" from "no games match your search/platform".
- **Platform artwork**: a missing custom-artwork root is `Ok(())`, not an
  error; bundled artwork and the glyph fallback are unaffected.

## 6. Clean-install testing method

Tests never touch the real `$HOME`, never contact RomM or any network
service, and never touch a real ROM collection. Two layers:

- **Unit tests** (`crates/archivefs-core/src/lib.rs`,
  `diagnostics/tests.rs`, `diagnostics/runner.rs`'s inline tests) construct
  `SetupDiagnostics`/`DoctorCheck`/`DatabaseDiagnostic`/`Gathered::Failed`
  values directly, or point `run_setup_diagnostics_with_command_check` at a
  path under a per-test temporary directory (`test_root(name)`), so a
  confirmed-missing config is exercised without mutating process-wide state
  such as the `HOME` environment variable.
- **Manual smoke test**: the release CLI binary run with `HOME`,
  `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `XDG_CACHE_HOME` all pointed at a
  freshly created `mktemp -d` tree (see the harness in the task brief this
  branch implements). `archivefs-cli doctor --findings` against that tree
  reports zero `Error`/`Critical` findings and writes no file outside the
  temporary root; the same binary against a fixture with a malformed
  `config.toml` still reports real errors.

No background cover worker, audit, or validation job starts merely from
opening the app: the GUI's Gamer View cover worker is only started once there
are visible rows requesting a cover (`cover_requests.is_empty()` gate before
`CoverWorker::start`), and DAT Sources' audit controls are disabled until a
source and target are both explicitly chosen.

## 7. Deferred (intentionally out of scope)

Per the task brief, none of the following changed on this branch:

- A full multi-step setup wizard — the existing gated Setup/Diagnostics
  screen with direct actions already serves this purpose at EmuWiz's
  current scale.
- Automatic library discovery.
- Online account setup.
- Automatic RomM discovery.
- Destructive Doctor repairs — Doctor's repair surface is unchanged; nothing
  here adds a new repair action.
- A large GUI redesign — all changes reuse existing components
  (`widgets::empty_state`, `widgets::banner`, `egui::Frame::group`) and
  existing pages.
