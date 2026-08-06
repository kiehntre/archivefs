# Current-main user-workflow audit

Audited: `main` at `ff149e7` (branch `review/current-main-user-workflow-audit`).
Scope: ArchiveFS as an integrated user-facing product — CLI (`archivefs-cli`)
and GUI (`archivefs-gui`) over the shared `archivefs-core`. This is an audit
and narrow repair pass, not a redesign.

Method: read every `show_*` page renderer and its dispatch path in
`crates/archivefs-gui/src/main.rs` (68,459 lines) and the workflow-specific
modules (`romm_config.rs`, `romm_source.rs`, `romm_browse.rs`, `romm_game.rs`,
`dat_sources_page.rs`, `cheat_sources_page.rs`, `gamer_artwork.rs`); traced
representative background-job/generation-guard pairs; built and ran the
release CLI against an isolated, empty `HOME`/`XDG_CONFIG_HOME`; and compared
every material claim in `README.md`, `ROADMAP.md`, `CHANGELOG.md`, and
`docs/GUI_BACKEND_CAPABILITY_MATRIX.md`/`docs/INTEGRATED_GUI_AUDIT.md`/
`docs/GUI_FINAL_POLISH_REPORT.md`/`docs/POST_V0.7_GUI_FOLLOWUPS.md`/
`docs/paper-cuts.md` against the current source tree.

**Headline finding: the product is in noticeably better shape than its own
documents suggest.** Several older audit/status documents in `docs/`
describe the RetroArch cheat matching/preview/install/rollback pipeline and
the Settings page as unwired, and describe `main.rs` at less than half its
current size — all contradicted by the current tree, where those flows are
fully wired, generation-guarded, and tested. Two `README.md`/`CHANGELOG.md`
lines and one internal `ROADMAP.md` contradiction were still actively wrong
in the current, non-historical sections, and are fixed by this pass (see
"Fixes made" below). No stub button, `TODO`/`FIXME`/`unimplemented!()`, or
literal no-op control was found anywhere in `crates/archivefs-gui/src`.

## Confirmed working workflows

- **First run / empty state.** `missing_config_is_first_run` (main.rs:15198)
  gates a welcome banner instead of an error wall; `SetupDiagnosticStatus::
  NotConfigured` (added in commit `0c8dcb9`, the commit immediately before
  this audit) downgrades every diagnostic/Doctor finding that fires only
  because nothing is configured yet to Info. Verified live: a fresh
  `archivefs-cli doctor --findings` under an isolated empty `HOME` reports
  **0 Critical/Error/Warning, 12 Info** findings (reproduced during this
  audit, see "Safety/stale-result risks" below for the one command class
  this softening does not cover).
- **DAT/Cheat Sources save-bar.** Both pages show an explicit "Saving will:"
  consequence list before any write, an unsaved-changes badge, and a
  "Discard changes" action that fully restores saved state
  (`dat_sources_page.rs:634-638`, `cheat_sources_page.rs:634-638`).
  `dat_sources_page.rs`'s Discard/Remove additionally cancel or abandon an
  in-flight Validate/Audit job so a late worker reply cannot land against a
  since-discarded or since-removed source — fixed in the immediately
  preceding commit `6070d70` with four regression tests; this audit found no
  equivalent gap remaining in that file, and confirmed `cheat_sources_page.rs`
  has no background jobs to race in the first place (its Discard is purely
  synchronous).
- **RomM configuration/import/browse/identity/artwork.** All generation- or
  content-key-guarded: `romm_generation` (main.rs:3611) increments per
  operation, `start_romm_operation` (main.rs:5455) refuses to start a second
  operation while one is in flight, `poll_romm_operation` (main.rs:5512)
  drops any reply whose generation doesn't match, and browse/game-panel
  state additionally checks `accepts_page`/`accepts_detail`/`accepts_cover`/
  `accepts_panel` (main.rs:5576-5675) before landing a result — so switching
  the selected archive or closing the RomM dialog mid-fetch cannot attach one
  game's cover or identity evidence to another's row. `start_romm_operation`
  also carries a true `Arc<AtomicBool>` cooperative cancellation flag through
  to the worker (main.rs:5464-5497), not just a UI-side "ignore the result"
  flag.
- **Cheats & Mods preview → apply → rollback → History & Logs.** Contrary to
  `docs/GUI_BACKEND_CAPABILITY_MATRIX.md:100-119` ("Backend complete, not
  integrated") and `docs/INTEGRATED_GUI_AUDIT.md:182-190` (documents
  matching/preview/install/rollback as "must not be fabricated" deferred
  functionality), the current tree wires all of it:
  `start_cheat_install_rollback` (main.rs:8755), `start_shared_rollback`
  (main.rs:8991), `poll_shared_rollback` (main.rs:9041), and
  `show_shared_rollback_card` on the History & Logs page (main.rs:20831).
  `ROADMAP.md:184-185` (pre-existing text, before this audit) already
  recorded that the old "Archive matching and cheat installation are not yet
  implemented" copy was removed; the string now survives only inside a
  negated test assertion (`main.rs:48323`) confirming it no longer renders.
- **Settings page.** Rated "Backend complete, not integrated" by
  `docs/GUI_BACKEND_CAPABILITY_MATRIX.md:125`; the current tree has a fully
  wired `show_settings_page` (main.rs:30202) dispatching config-folder,
  diagnostics, RetroArch-profile-rescan, and platform-artwork actions
  (main.rs:13330-13345).
- **DAT Sources / Cheat Sources per-provider workflows** (RetroArch,
  PCSX2/GameHacking PS2, Dolphin, GameHacking GameCube, GameHacking Wii,
  Xenia, BSFree): each has real request/response types and a real dispatch
  path (`CheatSourceMode`, `DolphinProviderRequestKey`,
  `Pcsx2GameHackingState`, `GameCubeGameHackingState`,
  `XeniaProviderRequestKey`, `BsFreeOperation` — main.rs:21305-21832).
  "No match" (`GameHackingMatchStatus::NoMatch`, "No matching cheat found")
  is visibly distinct from "unavailable" (`SourceAvailability::Unavailable`,
  `CoverageStatus::Unavailable { reason }`) throughout — these are separate,
  differently-worded states, not one generic empty view standing in for both.
- **Doctor findings + repairs.** Real read-only scan
  (`gather_doctor_scan`)/repair (`show_doctor_repair_review` →
  `show_doctor_repair_result`, main.rs:16018/16090) round-trip, with the
  first-run softening above layered on top.

## Partially wired / weak states (not defects selected for this pass)

- **`show_tools_overlay_header`'s "Back to Library" button** (main.rs:16879)
  does not navigate to Library from a non-Library page — it only closes the
  current Tools overlay onto whatever page was already open. This is a known,
  pre-existing label inaccuracy, already reviewed and **deliberately left
  unchanged** per an in-code comment at main.rs:16862-16878 explaining that
  changing its target would itself be a user-facing behaviour change outside
  the scope of the review that found it. Not re-litigated here; listed for
  completeness.
- **`docs/GUI_BACKEND_CAPABILITY_MATRIX.md`** (Doctor "Structured checks",
  line 55) rates Doctor's structured-checks screen "Partial — exists as
  overlay, not full screen." The current tree has both a full
  `MainView::Doctor` page (`show_doctor_page`, main.rs:15621) and a
  `ToolsOverlay::DoctorChecks` overlay path (`show_doctor_checks_panel`,
  main.rs:16434) — this matrix entry was not independently re-verified
  beyond confirming both code paths exist; whether the overlay path is now
  redundant with the full page was out of scope for this pass.
- **Stale-result tracing was bounded, not exhaustive.** 49 `thread::spawn`
  sites exist in `main.rs`. This audit traced the RomM, RetroArch catalogue
  retrieval, and cheat-workflow-request families end-to-end and found a
  matching generation/key guard at every poll site in those families (see
  "Safety/stale-result risks"), consistent with `docs/INTEGRATED_GUI_AUDIT.
  md:44`'s assessment. The remaining spawn sites were not individually
  traced; treat "no gap found" as bounded by what was actually traced.

## Unreachable or misleading controls

None found. Every literal `.on_disabled_hover_text(...)` occurrence
(53 non-test hits) is a visibly-present, explanatorily-disabled button, not a
dead one. Zero `TODO`/`FIXME`/`unimplemented!()`/"coming soon" markers exist
anywhere in `crates/archivefs-gui/src`.

## Contradictory wording (confirmed against current code)

| Document | Claim | Contradicted by |
|---|---|---|
| `README.md:191` (pre-fix) / `CHANGELOG.md:105` (pre-fix), both in their **current**, non-historical sections | "RomM integration is not included." | `crates/archivefs-core/src/identity_source/romm/{mod,config,client,import,capability,normalise}.rs`; `crates/archivefs-cli/src/romm_identity.rs` and the `identity source romm ...` CLI subcommand family; `crates/archivefs-gui/src/{romm_config,romm_source,romm_browse,romm_game}.rs` (~6,000 lines, wired into the Sources page — main.rs:13329-13359 — and generation-guarded end to end, see above). **Fixed in this pass** — see "Fixes made". |
| `ROADMAP.md:126` (pre-fix) | "Safe, journal-driven rollback for RetroArch cheat installations is now available via `retroarch-cheat-rollback`; GUI support remains out of scope." | The same document, 44 lines later (`ROADMAP.md:170-172`, pre-existing, unchanged by this pass): "A working RetroArch GUI apply/history/rollback flow ... is completed." Also contradicted directly by the GUI code cited above. **Fixed in this pass.** |
| `docs/GUI_BACKEND_CAPABILITY_MATRIX.md:100-119,125` | RetroArch preview/apply/rollback and Settings rated "Backend complete, not integrated." | See "Confirmed working workflows" above. **Not fixed in this pass** — this is a dated, timestamped snapshot document (pinned to a specific base commit in its own header), not a living reference; correcting it is lower-value than correcting the always-current README/CHANGELOG/ROADMAP and was left out of scope to keep this pass narrow. Flagged here so a maintainer can decide whether to archive or refresh it. |
| `docs/INTEGRATED_GUI_AUDIT.md:182-190` | RetroArch matching/preview/install/rollback/pinning described as "Intentionally deferred functionality... must not be fabricated," i.e. absent. | Same as above — a dated snapshot (`main.rs` at 28,551 lines vs. the current 68,459), left unedited for the same reason. |
| `README.md:160` (pre-fix) | Described the GUI as covering only "scanning, mounting, sources, library views, duplicates, and catalogue health," omitting Cheats & Mods, RomM, Doctor, History & Logs, and Settings, all of which exist and are wired. | **Fixed in this pass** (expanded alongside the RomM fix). |

## Missing prerequisites / weak error states

- **CLI: bare, non-actionable errors on a fresh install.** `archivefs-cli
  doctor` and `archivefs-cli config-check` both give structured, actionable
  guidance ("Create a starter config", "Next step: ...") when the config
  file is missing. But 14 other entry points in `crates/archivefs-cli/src/
  main.rs`'s `run()`/`run_doctor_repair` (`scan`, `mount`, `mount-one`,
  `unmount`, `unmount-one`, `status`, and others, plus `doctor --repair`)
  called `Config::load_default()?` directly and surfaced only the bare OS
  error with no next step:
  ```
  archivefs: /home/user/.config/archivefs/config.toml: No such file or directory (os error 2)
  ```
  Reproduced live under an isolated empty `HOME` (this audit): `archivefs-cli
  scan` exits 1 with only that one line, while `doctor --findings` on the
  identical fresh state gives 12 well-explained Info findings and a "Next
  step" for every one of them. `scan` is the third command in README's own
  "Common Commands" list (README.md:392-396) and the third step a user might
  reasonably run before `config-check`/`doctor`. **Fixed in this pass** — see
  "Fixes made".
- **BSFree is documented as browse-only with no Install action** (README.md,
  `docs/CHEATS_MODS_SAFETY.md`) — this is an intentional non-goal, not a
  missing prerequisite, and the GUI's own cheat_sources_page.rs:841 label
  ("source is disabled everywhere") and README.md:159 wording already state
  it plainly. Listed here only to confirm it is not a hidden gap.

## Safety / stale-result risks

- **RomM, RetroArch catalogue retrieval, and cheat-workflow requests**: no
  gap found in the families traced (see "Confirmed working workflows" and
  "Partially wired" above for the boundary of what was traced).
- **DAT Sources Discard/Remove vs. a running Validate/Audit job**: fixed in
  the commit immediately preceding this audit (`6070d70`); re-verified as
  still correct and not re-broken by this pass's unrelated changes (this
  pass touched no code in `dat_sources_page.rs`).
- **CLI first-run softening covers Doctor and `config-check` but not the
  other 14 commands** listed above — not a stale-result risk in the async
  sense, but the same class of gap (a state the product already knows how to
  explain well in one place, and doesn't in another). Addressed by the same
  fix.

## Items already completed despite stale roadmap/docs

- RomM identity source (configuration, import, browse, identity, artwork) —
  contradicted `README.md`/`CHANGELOG.md`, absent from `ROADMAP.md` entirely;
  now correctly documented in all three.
- RetroArch GUI apply/history/rollback flow — contradicted
  `ROADMAP.md:126`'s "GUI support remains out of scope" and
  `docs/GUI_BACKEND_CAPABILITY_MATRIX.md:100-119`'s "Backend complete, not
  integrated"; `ROADMAP.md` self-corrected (fixed in this pass), the matrix
  left as a dated snapshot (see table above).
- Settings page integration — contradicted
  `docs/GUI_BACKEND_CAPABILITY_MATRIX.md:125`; not otherwise claimed missing
  anywhere else.
- The stale "Archive matching and cheat installation are not yet
  implemented" Cheats & Mods copy — already recorded as removed in
  `ROADMAP.md:184-185` before this audit; `docs/INTEGRATED_GUI_AUDIT.md`
  still describes the underlying functionality as absent (dated snapshot,
  see table above).
- First-run empty-state softening across Setup/Diagnostics and Doctor
  (`SetupDiagnosticStatus::NotConfigured`) — landed one commit before this
  audit (`0c8dcb9`); re-verified live during this audit and found correct
  for the GUI/Doctor path (the CLI-command gap this audit fixes is adjacent
  to, not a regression of, that fix).

## Severity and evidence summary

| # | Finding | Severity | Evidence |
|---|---|---|---|
| 1 | 14 CLI commands give a bare OS error with no guidance on a missing config file, while `doctor`/`config-check` give full guidance for the identical condition | **High** (first-run UX; affects the most likely first command sequence a new user runs) | `crates/archivefs-cli/src/main.rs` (pre-fix) lines 175,182,193,198,209,217,227,237,258,1043,1108,1364,1368,4774; reproduced live under isolated empty `HOME` |
| 2 | `README.md`/`CHANGELOG.md` falsely claim RomM is unsupported; `ROADMAP.md` omits it entirely | **High** (a user or reviewer evaluating the product from its docs alone would conclude a real, tested, ~6,000-line feature does not exist) | `README.md:191`, `CHANGELOG.md:105` (pre-fix); `crates/archivefs-core/src/identity_source/romm/`, `crates/archivefs-gui/src/romm_*.rs` |
| 3 | `ROADMAP.md` internally contradicts itself about RetroArch GUI rollback support in the same "Completed foundations" list | **Medium** (internal doc consistency; doesn't mislead as badly as #2 since the correct claim is also present) | `ROADMAP.md:126` vs. `:170-172` (pre-fix) |
| 4 | `docs/GUI_BACKEND_CAPABILITY_MATRIX.md`/`docs/INTEGRATED_GUI_AUDIT.md` describe RetroArch matching/preview/install/rollback and Settings as unintegrated | **Low** (dated, timestamped internal snapshots, not living user docs; a maintainer reading them without the current tree in hand could still be misled) | headers of both files; contradicted by main.rs code cited above |
| 5 | `show_tools_overlay_header`'s "Back to Library" button doesn't always navigate to Library from non-Library pages | **Low** (already disclosed in-code, already reviewed, and deliberately deferred by a prior pass) | `main.rs:16862-16889` |

## Recommended repair order

1. (Done, this pass) CLI first-run error guidance — highest user impact,
   smallest, safest change.
2. (Done, this pass) `README.md`/`CHANGELOG.md`/`ROADMAP.md` factual
   corrections — zero code risk, corrects the product's public
   self-description.
3. Refresh or archive `docs/GUI_BACKEND_CAPABILITY_MATRIX.md` and
   `docs/INTEGRATED_GUI_AUDIT.md` against current `main`, or mark them
   explicitly as historical snapshots in their own headers (both already
   pin a base commit, so the fix could be as small as adding "superseded —
   see current `main`" to each header). Left out of this pass to keep it
   narrow; recommended as the next small, low-risk doc pass.
4. Fully trace the remaining ~46 `thread::spawn` sites (of 49) not traced
   in this pass for the same generation/key-guard property confirmed
   elsewhere, as a research-only follow-up before trusting "no stale-result
   gap" as a whole-app claim.

## Remaining highest-value user-facing gap

Documentation accuracy for the internal audit snapshots
(`docs/GUI_BACKEND_CAPABILITY_MATRIX.md`, `docs/INTEGRATED_GUI_AUDIT.md`) —
not user-facing in the strict sense (users don't read these), but a
contributor or reviewer relying on them to understand "what's really wired"
would be actively misled about RetroArch rollback and Settings, the exact
inverse of this pass's `README.md`/`ROADMAP.md` fix. Recommended as a
follow-up doc-only pass, not bundled here to keep this pass to two narrow,
independently-testable fixes.

## Fixes made in this pass

1. **CLI first-run guidance** (`crates/archivefs-cli/src/main.rs`): added
   `load_config()`/`explain_config_load_error()`, used at all 14 sites that
   previously called `Config::load_default()?` directly in `run()` and
   `run_doctor_repair`. A confirmed-missing config file (verified via
   `symlink_metadata`, so a dangling symlink — a real misconfiguration — is
   never mistaken for a fresh install) now gets the same "Run `archivefs
   config-check` ... or copy config.toml.example ..." guidance `doctor`/
   `config-check` already give, appended after the original error text
   (nothing that inspects the original error text elsewhere — Doctor's
   first-run softening, `config-check`'s own report — was touched or
   affected). Regression tests: `confirmed_missing_config_gets_a_first_run_
   hint`, `dangling_symlink_config_path_gets_no_first_run_hint`,
   `permission_denied_config_path_gets_no_first_run_hint` (all in
   `crates/archivefs-cli/src/main.rs`'s `mod tests`).
2. **Documentation accuracy** (`README.md`, `CHANGELOG.md`, `ROADMAP.md`):
   removed the false "RomM integration is not included" claim from both
   `README.md` and `CHANGELOG.md`'s current sections, added an accurate
   RomM description to `README.md`'s feature list and `ROADMAP.md`'s
   Completed foundations, expanded `README.md`'s one-line GUI-scope summary
   to include Cheats & Mods/RomM/Doctor/History & Logs/Settings, and
   resolved `ROADMAP.md`'s internal self-contradiction about RetroArch GUI
   rollback support. Regression tests:
   `crates/archivefs-core/tests/documentation_claims.rs` — three tests that
   read the actual `README.md`/`CHANGELOG.md`/`ROADMAP.md` files at test
   time and fail if any of the three claims reappears (verified to fail
   against the pre-fix text and pass against the fix, via `git stash`).

Both fixes are narrow, additive, touch no persistence schema, and add no new
provider or architecture.
