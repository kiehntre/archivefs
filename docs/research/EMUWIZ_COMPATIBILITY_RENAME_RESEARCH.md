# EmuWiz Rename — Compatibility Research & Review

Research/review only. No application code was modified to produce this report.

Scope: cross-reference current `main` (`kiehntre/archivefs`) against the
in-progress, unpushed EmuWiz rename branch (`feature/emuwiz-rename`, local
worktree only, one committed docs commit `b567ec0` on top of old-main
`50c76ce`, plus a substantial set of uncommitted code-level compatibility
changes) to determine exactly which identifiers can safely change now, which
need an alias/deprecation period, and which must never change.

## 1. Executive summary

The rename branch's core strategy is sound: a new `crates/archivefs-core/src/app_dirs.rs`
module resolves the EmuWiz-named config/data directory first, falls back to
the legacy `archivefs`-named directory *at the directory level* if that is
where the user's data already lives, and defaults to the EmuWiz name only for
genuinely fresh installs. It never copies, moves, or deletes anything, so
resolution is idempotent and cannot destroy or fork user data on its own.
`default_config_path()`, `default_index_path()`, and `database.rs`'s
`resolve_database_path()` are already rewired through it. Binary names are
handled with additive `[[bin]]` aliases (both `emuwiz-cli`/`archivefs-cli`,
and `emuwiz`/`emuwiz-gui`/`archivefs-gui`, all pointing at the same
`src/main.rs`) rather than a rename-and-break. Crate/package names
(`archivefs-core`, `archivefs-cli`, `archivefs-gui`) are correctly left
untouched. No `#[serde(rename)]` or struct-field renames were found anywhere
in the branch's diff — the highest-risk accidental-breakage vector (a
blanket find/replace touching a persisted field name) did not happen.

One real defect was found: `dat/rename_apply/journal.rs::rename_transaction_dir_in()`
was not rewired to use `app_dirs` when its caller was, so it silently
hardcodes the legacy path with no EmuWiz-first check — a latent bug in the
crash-recovery journal subsystem (Finding A, §6). Two smaller cosmetic gaps
were also found (Findings B and C, §6). None of these are architecturally
hard to fix; all should be fixed before this branch is proposed for merge.

## 2. Current main behavior (compatibility-sensitive surfaces, as they exist today)

All citations below are against `origin/main` at `4cae6f5b0e1b528b0f6a83cf43d83baa3cb1a368`
(the commit current at time of writing; the rename branch itself forked from
the earlier `50c76ce39131bc0b571172979d01df653edd548f`, before PRs #24/#25
landed — those PRs are unrelated docs/filter work and do not touch any
surface in this report).

**Config directory.** `crates/archivefs-core/src/lib.rs::default_config_path()`
resolves `$HOME/.config/archivefs/config.toml` directly (no XDG env var
support — deliberate, matches the module doc "rather than adopting the XDG
environment variables").

**Data directory.** `crates/archivefs-core/src/lib.rs::default_index_path()`
resolves `$HOME/.local/share/archivefs/index.json`; `database.rs::default_database_path()`
resolves `$HOME/.local/share/archivefs/library.sqlite3` via an internal
`resolve_database_path()` helper with the same `$HOME` lookup.

**DAT rename-apply journal.** `crates/archivefs-core/src/dat/rename_apply/journal.rs::default_rename_transaction_dir()`
resolves `$HOME/.local/share/archivefs/rename-transactions/`, one directory
per in-flight or completed rename transaction, used for the existing
preview/apply/rollback pipeline audited in prior work on this repo.

**RetroArch cheat-install journal/cache.** `crates/archivefs-core/src/patch_manager/`
(`cheat_cache_lock.rs`, cache/history modules) resolve their working
directories under the same `~/.local/share/archivefs` root; a subprocess
lock protocol uses `ARCHIVEFS_LOCK_CHILD` / `ARCHIVEFS_LOCK_ROOT` /
`ARCHIVEFS_LOCK_HOLD_MILLIS` (`patch_manager/cheat_cache_lock.rs:289-315`).

**SQLite database.** `library.sqlite3`. Schema search (`grep -n "CREATE TABLE" database.rs`)
found no `archivefs`-branded table, column, or pragma names — table names are
plain domain nouns (`schema_migrations`, etc.). No stored column encodes the
old product name.

**Environment variables.** Confirmed by direct grep of `main`, six
`ARCHIVEFS_`-prefixed variables exist in code:
`ARCHIVEFS_LOG` (`archivefs-gui/src/main.rs:673`, log level),
`ARCHIVEFS_LOCK_CHILD`, `ARCHIVEFS_LOCK_ROOT`, `ARCHIVEFS_LOCK_HOLD_MILLIS`
(`archivefs-core/src/patch_manager/cheat_cache_lock.rs:289-315`, subprocess
lock test protocol), and `ARCHIVEFS_TEST_HOT_JOURNAL_DB` /
`ARCHIVEFS_TEST_HOT_JOURNAL_MARKER` (`archivefs-core/src/database.rs:7355-7402`,
test subprocess protocol). A seventh, `ARCHIVEFS_PCSX2_PROOF`, is documented
shell-script usage in `docs/PCSX2_CHEAT_ADAPTER.md` (a manual verification
procedure, not code).

**Binary/package names.** `crates/archivefs-cli/Cargo.toml` and
`crates/archivefs-gui/Cargo.toml` both declare `name = "archivefs-cli"` /
`"archivefs-gui"` with no explicit `[[bin]]` section, so Cargo's implicit
single binary takes the package name.

**Desktop/application ID.** No `.desktop` file, no freedesktop application-id,
no GTK/portal app-id metadata exists anywhere in the repository on `main`.
This surface is not applicable to the current build (no packaged desktop
entry to alias).

**PCSX2/Dolphin on-disk ownership markers.** The PCSX2 cheat adapter writes
`// ArchiveFS managed block: <id>` / `// End ArchiveFS managed block` marker
comments into `.pnach` files it manages; the Dolphin GameHacking adapter
writes an `[ArchiveFS_Managed_GameHacking]` INI section. Both are parsed back
on subsequent runs to recognise EmuWiz-owned entries for diagnostics,
re-installation, and safe removal.

**Provider/source IDs.** BSFree, GameHacking, RomM, and DAT-source identifiers
are plain lowercase domain strings (e.g. `bsfree`, `gamehacking`, `romm`) with
no `archivefs` substring anywhere in the ID space — they were never
ArchiveFS-branded and are not implicated by a rename.

**Operation/transaction IDs.** `dat/rename_apply/journal.rs::new_transaction_id()`
produces `<unix_seconds>-<sequence>`, an opaque timestamp+counter string with
no product name embedded.

**Serialized config/cache field names.** Config files (`config.toml`,
`dat_sources.toml`, `cheat_sources.toml`, `library_views.json`, RomM's
`config.json`, emulator profile caches) use plain Rust-derived struct field
names as their TOML/JSON keys. `ExternalIdentityRecord.archivefs_path` in
`archivefs-core/src/database.rs` (mirrored by an identical field in
`archivefs-gui/src/romm_config.rs::RommIdentityExample`) is a persisted
struct field whose literal name is `archivefs_path` — this is exactly the
kind of field that a naive branding find/replace could accidentally rename
and corrupt every existing user's saved data.

## 3. What Codex's branch changes

### 3a. Committed docs-only commit (`b567ec0`)

`git diff 50c76ce..b567ec0` is ~9,960 lines, almost entirely prose in
`README.md`, `ROADMAP.md`, `CHANGELOG.md`, `docs/*.md`, help text, and code
comments — replacing "ArchiveFS" with "EmuWiz" as the product's spoken name.
It ends with a self-authored compatibility table (search the diff for
"compatibility-sensitive") in which Codex explicitly lists what it believes
must *not* change: the six `ARCHIVEFS_*` env vars, `ARCHIVEFS_PCSX2_PROOF`,
the `~/.config/archivefs` / `~/.local/share/archivefs` / `/mnt/archivefs` /
`/var/lib/archivefs` path family, `archivefs-v*` artifact naming, the
`kiehntre/archivefs` repository name/URLs (deliberately not renamed), the
PCSX2 `// ArchiveFS managed block` and Dolphin `[ArchiveFS_Managed_GameHacking]`
markers, and internal Rust test identifiers containing `archivefs`. This list
was cross-checked against the audit in §2 and found accurate and
non-contradictory — it correctly identifies every stable surface this report
independently found, with one gap: it does not mention the persisted
`archivefs_path` struct field (§2, last paragraph) at all. Independently
confirmed that field's literal name is untouched in the diff, so nothing
actually broke — but its omission from Codex's own list means the omission
was luck (nothing in this pass happened to touch it), not verification.

### 3b. Uncommitted code changes (still in the live worktree, unpushed)

This is the substantive compatibility engineering, on top of the docs commit:

- **New `crates/archivefs-core/src/app_dirs.rs`** (untracked, new file):
  `CONFIG_DIR_NAME = "emuwiz"`, `LEGACY_CONFIG_DIR_NAME = "archivefs"`,
  `DATA_DIR_NAME = "emuwiz"`, `LEGACY_DATA_DIR_NAME = "archivefs"`; a
  `choose_dir(primary, legacy)` helper (prefer primary if it exists, else
  legacy if it exists, else primary) used by both `config_dir()` and
  `data_dir()`; `config_path(leaf)` / `data_path(leaf)` convenience joins;
  `legacy_config_dir_exists()` / `legacy_data_dir_exists()` for future
  diagnostics/migration tooling. Directory-level resolution (not per-file),
  matching its own doc comment's stated intent. Own test suite covers fresh
  install, legacy-only, both-exist (EmuWiz wins), and — importantly — mixed
  state where config and data resolve *independently* (`config_and_data_directories_resolve_independently_mixed_states`).
- **`lib.rs`**: `default_config_path()` and `default_index_path()` rewired to
  call `app_dirs::config_path("config.toml")` / `app_dirs::data_path("index.json")`.
- **`database.rs`**: `default_database_path()` rewired to
  `app_dirs::data_path("library.sqlite3")`; the old direct-`$HOME` logic is
  kept only behind `#[cfg(test)]` as `resolve_database_path()` for unit tests
  (`default_database_path_reuses_a_legacy_archivefs_home`,
  `default_database_path_prefers_emuwiz_when_both_exist`), not used in
  production.
- **`dat/sources/config.rs`, `patch_manager/cheat_source_registry/config.rs`**:
  production path resolution (`default_dat_sources_config_path`,
  `default_cheat_sources_config_path`) calls `app_dirs::config_path()`
  directly. Verified by reading the current worktree file content (not just
  the diff): the `let legacy = root.join(".config/archivefs")`-shaped code
  visible in the diff is confined to `#[cfg(test)]`-only helper functions
  (`dat_sources_config_path_in`, `cheat_sources_config_path_in`) that exist
  purely to let tests inject an arbitrary `$HOME` without racing real
  environment variables — they mirror `app_dirs`'s constants and precedence
  for test purposes and are not a second, independently-implemented
  production code path. See §5 for the full resolution of this question.
- **`identity_source/settings.rs`**: `SUGGESTED_TOKEN_PATH` changed from
  `"~/.config/archivefs/romm-token"` to `"~/.config/emuwiz/romm-token"`. This
  is display/help text only — the doc comment on the constant states the
  token file's *actual* location always comes from the user's configured
  path, never this constant, so an existing user's real token file is
  unaffected regardless of what this string says.
- **`archivefs-gui/src/main.rs`**: new `EMUWIZ_LOG` environment variable,
  winning over legacy `ARCHIVEFS_LOG` when both are set; `ARCHIVEFS_LOG`
  alone continues to work when `EMUWIZ_LOG` is unset. Three dedicated tests
  found: `"EMUWIZ_LOG must win when both are set"`,
  `"the legacy ARCHIVEFS_LOG must still work"`, and
  `"an invalid EMUWIZ_LOG falls back to the default, never to the legacy var"`
  (`main.rs:40262-40272`).
- **`crates/archivefs-cli/Cargo.toml`**: package name stays `archivefs-cli`;
  two `[[bin]]` targets added, `emuwiz-cli` (primary) and `archivefs-cli`
  (legacy alias), both `path = "src/main.rs"` — identical code, two entry
  points.
- **`crates/archivefs-gui/Cargo.toml`**: package name stays `archivefs-gui`;
  three `[[bin]]` targets, `emuwiz` and `emuwiz-gui` (primary names) plus
  `archivefs-gui` (legacy alias), all `path = "src/main.rs"`.
- **`install.sh`**: adds a `config_root` fallback — if the new-named config
  root doesn't exist but `$HOME/.config/archivefs` does, install into the
  legacy root; otherwise defaults to the new name. Verified this precedence
  matches `app_dirs::config_dir()` exactly (see §5).
- **`dat/rename_apply/journal.rs`**: `default_rename_transaction_dir()`
  rewired to `app_dirs::data_dir()`, but the sibling helper
  `rename_transaction_dir_in()` was **not** updated — see Finding A, §6.
- **Other touched files** (`library_views.rs`, `patch_manager/emulator_profile_memory.rs`,
  `archivefs-cli/src/{dat,main,platform_artwork,retroarch_cheat_setup,romm_identity}.rs`,
  `scripts/build-release.sh`, `scripts/check-version-consistency.sh`,
  `scripts/test-on-nobara.sh`, `scripts/test-release-artifact-verifier.sh`,
  `scripts/verify-release-artifact.sh`, `tests/test_install.sh`,
  `docs/reviews/EMUWIZ_RENAME_AUDIT.md`, `config.toml.example`): a mix of
  routing call sites through the new `app_dirs` module and cosmetic text
  updates. No serde-field or config-key renames found in any of them.

## 4. Per-identifier classification table

| Identifier / surface | Current main | Rename-branch treatment | Classification | Rationale |
|---|---|---|---|---|
| `~/.config/archivefs` directory | Sole config root | `app_dirs`: EmuWiz-first, directory-level fallback to this if present | **2 — alias/deprecation** | Existing users' config must keep resolving; `app_dirs` already implements this correctly |
| `~/.local/share/archivefs` directory | Sole data root | Same `app_dirs` fallback | **2 — alias/deprecation** | Same reasoning; covers DB, index, journals, RetroArch cache |
| `library.sqlite3` filename | Fixed | Unchanged filename, only directory resolution changed | **3 — must remain stable** | Renaming the file itself (not just its directory) would strand every user's library with no fallback |
| SQLite table/column/pragma names | No `archivefs` branding present | Untouched | **N/A — nothing to rename** | Confirmed via schema grep; not a rename target at all |
| `index.json`, `dat_sources.toml`, `cheat_sources.toml`, `config.toml` filenames | Fixed | Unchanged, only directory changed | **3 — must remain stable** | Same as database filename |
| Serialized struct/JSON/TOML field names (all config/cache files) | Plain field names | None renamed anywhere in the diff | **3 — must remain stable** | A renamed field silently drops or corrupts existing values on load; verified zero occurrences of this in the diff |
| `ExternalIdentityRecord.archivefs_path` field | Persisted field, literal name `archivefs_path` | Untouched | **3 — must remain stable** | Persisted in identity-cache data; renaming breaks deserialization of every existing record. **Not on Codex's own compatibility list — flagged for explicit sign-off, see §6 Finding C-adjacent note** |
| DAT rename-apply journal directory (`rename-transactions/`) | Under legacy data root | `default_rename_transaction_dir()` uses `app_dirs`; `rename_transaction_dir_in()` does not | **2 — alias/deprecation, currently inconsistent** | See Finding A, §6 — must be fixed before merge |
| RetroArch cheat-install journal/cache directories | Under legacy data root | Resolved via `app_dirs`-routed data dir (through `patch_manager` call sites) | **2 — alias/deprecation** | Same fallback strategy as database/config |
| `ARCHIVEFS_LOG` | Sole log-level var | Still honoured; `EMUWIZ_LOG` added and wins if both set | **2 — alias/deprecation** | Correctly implemented and tested |
| `ARCHIVEFS_LOCK_CHILD`, `ARCHIVEFS_LOCK_ROOT`, `ARCHIVEFS_LOCK_HOLD_MILLIS` | Subprocess lock test protocol | Untouched in the diff | **3 — must remain stable** | Internal test-subprocess contract, not user-facing; changing it is pure risk for zero user benefit |
| `ARCHIVEFS_TEST_HOT_JOURNAL_DB`, `ARCHIVEFS_TEST_HOT_JOURNAL_MARKER` | Test subprocess protocol | Untouched | **3 — must remain stable** | Same reasoning |
| `ARCHIVEFS_PCSX2_PROOF` | Documented manual-verification shell variable | Untouched | **3 — must remain stable** (per Codex's own stated rationale, confirmed) | Env var names are outside the branding surface; changing it breaks a documented manual procedure for no gain |
| `// ArchiveFS managed block: <id>` / `// End ArchiveFS managed block` (PCSX2 `.pnach` markers) | Parsed ownership marker, written into user files | Untouched | **3 — must remain stable** | Already-written user files carry this exact text; a mismatch breaks recognition, safe removal, and rollback for every previously-managed cheat |
| `[ArchiveFS_Managed_GameHacking]` (Dolphin INI section) | Parsed ownership marker | Untouched | **3 — must remain stable** | Same reasoning, Dolphin side |
| `archivefs-cli` / `archivefs-gui` package (crate) names | Cargo package names | Untouched | **3 — must remain stable** (for this rename's scope) | Internal Rust identifiers with no user-facing surface; renaming buys nothing and risks internal breakage for no user benefit |
| `emuwiz-cli` / `emuwiz` / `emuwiz-gui` binary names | N/A | Added as new primary `[[bin]]` targets alongside kept `archivefs-cli` / `archivefs-gui` | **1 — safe to add now** | Additive; existing binary names still work unchanged |
| `.desktop` / application ID | Does not exist | Not introduced by this branch | **N/A** | Nothing to classify; flag as a gap to plan for before any desktop packaging work, not a rename risk today |
| BSFree / GameHacking / RomM provider IDs | Plain domain strings, no ArchiveFS branding | Untouched | **4 — historical only / not implicated** | Never carried the old product name; not a rename surface |
| DAT/cheat operation & transaction IDs (e.g. `new_transaction_id()` output) | Opaque timestamp+counter | Untouched | **4 — historical only / not implicated** | Contains no product name |
| `kiehntre/archivefs` GitHub repository name and URLs | Fixed | Deliberately not renamed (Codex's own stated decision) | **3 — must remain stable** | Renaming a GitHub repo breaks every existing clone URL, issue link, and CI badge; correctly out of scope |
| Human-facing prose ("ArchiveFS" as a spoken product name in docs/UI text) | "ArchiveFS" throughout | Renamed to "EmuWiz" | **1 — safe to rename now** | Purely cosmetic, no on-disk or wire impact |
| `--help` output text listing `~/.config/archivefs/config.toml` | Matches actual current default | Still says the old path in one place after the branding pass (`archivefs-cli/src/main.rs:5782`, worktree state) | **1 — safe to rename, currently incomplete** | Cosmetic-only but should say the *effective* (possibly legacy) path, see Finding B |
| RetroArch cheat-setup `--json` command-hint strings (e.g. `vec!["archivefs".into(), "retroarch-cheat-setup".into()]`) | N/A (machine-readable hint) | Still emits the literal binary name `"archivefs"` while human-facing text nearby was updated | **2 — needs a decision, not yet made** | This is a machine-consumed hint (likely feeds a script or another tool); changing it silently could break an external consumer that greps for `archivefs`, but leaving it stale after the binary is renamed is also wrong — needs an explicit compatibility decision, not a silent pick either way |

## 5. Consistency findings

**`dat/sources/config.rs` / `cheat_source_registry/config.rs` vs. `app_dirs` — resolved, no divergence.**
Read the full current content of both files in the live worktree (not just
the diff hunks). Production entry points
(`default_dat_sources_config_path`, `default_cheat_sources_config_path`) call
`app_dirs::config_path()` directly — a single, shared resolution path. The
`legacy = root.join(".config/archivefs")`-shaped lines visible in the diff
belong exclusively to `#[cfg(test)]` helper functions
(`dat_sources_config_path_in`, `cheat_sources_config_path_in`) that take an
explicit `$HOME`-equivalent root parameter so tests can exercise the
fallback deterministically without racing real environment variables across
parallel test processes. These test helpers reimplement the same constants
and precedence as `app_dirs`, which is intentional duplication for test
isolation, not a second production implementation that could diverge from
the real one at runtime.

**`install.sh` vs. `app_dirs::config_dir()` — resolved, matches.**
The shell script's fallback (`if [ ! -d "$config_root" ] && [ -d "$HOME/.config/archivefs" ]; then config_root="$HOME/.config/archivefs"; fi`) implements the identical precedence
as `app_dirs::choose_dir()`: prefer the new name if present, else the legacy
name if present, else the new name. Directory names match exactly
(`emuwiz` / `archivefs`). No divergence found. This still means install.sh's
logic is a hand-maintained shell mirror of Rust logic with no shared source
of truth — see §11 (Blocker: low severity) for the maintenance-drift risk
this creates going forward, not a bug today.

**`resolve_database_path` test-only fallback vs. production `app_dirs` — consistent by construction.**
The `#[cfg(test)]`-gated `resolve_database_path()` in `database.rs` reimplements
`choose_dir`-equivalent logic inline (checking `primary_dir.exists()` then
`legacy_dir.exists()`) rather than calling `app_dirs` directly, apparently so
tests can inject an explicit `home: Option<OsString>` parameter the same way
the sibling config-path test helpers do. Its two dedicated tests
(`default_database_path_reuses_a_legacy_archivefs_home`,
`default_database_path_prefers_emuwiz_when_both_exist`) pass and their
assertions match `app_dirs`'s own equivalent tests
(`legacy_only_install_transparently_reuses_archivefs_paths`,
`emuwiz_wins_when_both_paths_exist`) value-for-value. Consistent, but it is
the third independent reimplementation of the same three-line precedence
rule found in this audit (the others being `install.sh` and the two
`#[cfg(test)]` config-path helpers). See §11.

## 6. Concerns found in the local branding/compatibility snapshot

**Finding A — `rename_transaction_dir_in()` was not rewired (real, unfixed defect).**
`crates/archivefs-core/src/dat/rename_apply/journal.rs`. `default_rename_transaction_dir()`
(line 36-38) correctly calls `crate::app_dirs::data_dir()?.join(RENAME_TRANSACTIONS_DIRECTORY)`.
Its sibling `rename_transaction_dir_in(home: Option<OsString>)` (line 41-49),
which exists specifically to let tests and any other caller inject an
explicit home directory, still hardcodes
`PathBuf::from(home).join(".local").join("share").join("archivefs").join(RENAME_TRANSACTIONS_DIRECTORY)`
— no EmuWiz-first check at all, and no updated doc comment reflecting the
change made to its sibling function. Confirmed still present by reading the
live worktree directly (not the snapshot diff) immediately before writing
this report. This function is `pub`, exported through the crate's module
tree, and sits in the DAT-rename crash-recovery journal subsystem — exactly
the kind of code path that is rarely exercised until a rename transaction is
interrupted mid-flight, which is the worst possible moment to discover a
silently-wrong directory. No other call site for this function was found in
the diff or the worktree at time of writing, so it is currently unreachable
dead-ish code, but it must be fixed (either rewired to route through
`app_dirs` the same way its sibling was, or removed if genuinely unused) before
this branch merges, precisely because it currently contradicts its own
module's stated compatibility strategy.

**Finding B — incomplete cosmetic pass: `--help` output.**
`crates/archivefs-cli/src/main.rs:5782` (worktree state) still prints the
literal string `"Config: ~/.config/archivefs/config.toml"` unconditionally,
rather than reporting the actual effective path (which may now be the
EmuWiz-named directory for a fresh install). This is cosmetic — it does not
affect where the config file is actually read from or written to — but it
will confuse a fresh-install user who never had an `archivefs` directory and
sees `--help` point at a path that doesn't exist for them. Low severity, easy
fix: print `app_dirs::config_path("config.toml")`'s actual resolved value
instead of a hardcoded string.

**Finding C — machine-readable command hints not updated alongside human text.**
`crates/archivefs-cli/src/retroarch_cheat_setup.rs` still emits the literal
binary name `"archivefs"` inside `--json`-mode "next steps" command hints
(e.g. `vec!["archivefs".into(), "retroarch-cheat-setup".into()]`, lines
~702, ~764, ~771, ~781 in the worktree), while adjacent human-readable text
in the same area of the codebase was updated to say `emuwiz-cli`. This is a
genuine open question, not an obvious bug: if any external tool or script
consumes this JSON to construct a command line, silently changing it to
`emuwiz-cli` would break that consumer the moment this branch ships, exactly
the class of accidental breakage this whole review exists to prevent — but
leaving a machine hint pointed at the old binary name indefinitely, after
`archivefs-cli` and `emuwiz-cli` diverge in any way, would also eventually be
wrong. This needs an explicit decision (most likely: keep emitting
`archivefs-cli` in this machine-readable hint for the whole compatibility
period, on the theory that it is guaranteed to keep working per the binary
alias in §4, and only flip it once the legacy binary alias itself is
deprecated) rather than a silent pick in either direction.

**Not a concern: `ExternalIdentityRecord.archivefs_path` omission from Codex's own list.**
Noted in §3a — the field itself is untouched and nothing is currently broken.
Recorded here because Codex's self-authored compatibility table is the kind
of artifact future edits to this branch will be checked against, and it
should be corrected to include this field explicitly so a later, unrelated
find/replace pass doesn't rename it by accident.

**Not a concern: SQLite schema.** No `archivefs`-branded table, column, or
pragma name exists on `main` to begin with, so there was nothing for the
branding pass to get wrong here, and it didn't.

**Not a concern: PCSX2/Dolphin ownership markers.** Confirmed byte-identical
in the diff; both parsers and their round-trip tests are untouched.

## 7. Exact migration order (recommended)

1. **Fix Findings A, B, and C** in the rename branch (Finding A is the only
   one that is an actual defect; B and C are polish/decisions). None require
   architectural changes — all are localized to the specific functions named
   above.
2. **Add the missing `archivefs_path` field to Codex's own compatibility
   table** in `docs/reviews/EMUWIZ_RENAME_AUDIT.md`, so it is on record as a
   deliberately-checked, deliberately-untouched surface rather than an
   accidental survivor.
3. **Merge the compatibility-layer commit set alone first**, isolated from
   the cosmetic branding commit if practical: `app_dirs.rs`, the
   `default_config_path` / `default_index_path` / `default_database_path` /
   `default_rename_transaction_dir` rewires, the `EMUWIZ_LOG` alias, and the
   `[[bin]]` alias additions. This is the part with real on-disk-compatibility
   risk; shipping it separately means it can be verified against real legacy
   installs (or a close simulation — see §10) before any user-visible rename
   ships, and a regression here is trivially attributable and revertible
   without also reverting hundreds of harmless doc-text lines.
4. **Ship a beta/RC to a small population of existing users** (or, at
   minimum, construct a realistic legacy-data fixture — an actual
   `~/.local/share/archivefs/library.sqlite3` plus a `~/.config/archivefs/config.toml`
   from a real prior release — and run the full app against it) before wide
   release, specifically exercising the mixed-state and mid-journal
   scenarios in §10 that have no automated coverage yet.
5. **Only then land the cosmetic branding commit** (README/ROADMAP/CHANGELOG/
   docs/UI text) — by this point it is provably inert with respect to
   on-disk compatibility, so it becomes a low-risk, easily-reviewed change
   purely about wording.
6. **Do not build a data-moving migration tool as part of this rename.** The
   directory-level reuse-not-migrate strategy already solves the problem
   `app_dirs`'s own doc comment names ("a future migration pass may move
   legacy data into the EmuWiz directories") without needing one — a
   migration tool adds real risk (partial copy, permission failures,
   concurrent-process races) for a benefit (a tidier directory name) that
   does not justify it. Revisit only if a concrete forcing function appears
   (e.g. an OS-level packaging requirement that mandates the new directory
   name specifically).
7. **Defer the `.desktop`/application-ID question** until actual desktop
   packaging work begins — nothing to alias today since nothing exists yet,
   but whoever adds a `.desktop` file first should read this report's
   §4 table first so the ID chosen from day one is the final one (application
   IDs are typically expected to be permanently stable once published to a
   desktop environment or app store, so getting this right the first time
   avoids ever needing a second alias mechanism here).

## 8. What must not be renamed (classification 3, standalone reference)

- `library.sqlite3`, `index.json`, `config.toml`, `dat_sources.toml`,
  `cheat_sources.toml` filenames (directory may change per §9; filenames may not)
- Every serialized struct/JSON/TOML field name in any config or cache file,
  explicitly including `ExternalIdentityRecord.archivefs_path` /
  `RommIdentityExample.archivefs_path`
- `ARCHIVEFS_LOCK_CHILD`, `ARCHIVEFS_LOCK_ROOT`, `ARCHIVEFS_LOCK_HOLD_MILLIS`,
  `ARCHIVEFS_TEST_HOT_JOURNAL_DB`, `ARCHIVEFS_TEST_HOT_JOURNAL_MARKER`
  (internal test-subprocess protocols — no user-facing benefit to renaming,
  only risk)
- `ARCHIVEFS_PCSX2_PROOF` (documented manual procedure)
- `// ArchiveFS managed block: <id>` / `// End ArchiveFS managed block`
  (PCSX2 `.pnach` ownership markers, already written into user files)
- `[ArchiveFS_Managed_GameHacking]` (Dolphin INI ownership marker, already
  written into user files)
- `kiehntre/archivefs` GitHub repository name and every URL/checkout path
  derived from it
- `archivefs-core`, `archivefs-cli`, `archivefs-gui` Cargo package (crate)
  names, for this rename's scope

## 9. Compatibility aliases needed (classification 2)

- `~/.config/archivefs` ↔ `~/.config/emuwiz` — **already implemented**
  correctly via `app_dirs::config_dir()` (directory-level, EmuWiz-first,
  legacy-fallback-if-present).
- `~/.local/share/archivefs` ↔ `~/.local/share/emuwiz` — **already
  implemented** correctly via `app_dirs::data_dir()`, but see Finding A: the
  DAT rename-transaction journal directory has one caller that bypasses it
  and must be fixed to use the same alias mechanism.
- `ARCHIVEFS_LOG` ↔ `EMUWIZ_LOG` — **already implemented and tested**
  (`EMUWIZ_LOG` wins if both set, `ARCHIVEFS_LOG` alone still works).
- `archivefs-cli` ↔ `emuwiz-cli`, `archivefs-gui` ↔ `emuwiz`/`emuwiz-gui` —
  **already implemented** via additive `[[bin]]` targets, all pointing at
  identical code, no behavioral difference between names.
- RetroArch cheat-setup `--json` command hints (`"archivefs"` literal in
  emitted next-step command arrays) — **not yet decided** (Finding C); needs
  an explicit choice, recommended: keep emitting the legacy binary name here
  for the duration of the compatibility period, since that name is guaranteed
  to keep working per the binary alias above.

## 10. Test matrix

| Scenario | What must be true | Existing coverage found |
|---|---|---|
| **Fresh install** (neither `~/.config/archivefs` nor `~/.local/share/archivefs`, nor their `emuwiz` equivalents, exist) | Config and data resolve to the `emuwiz`-named directories; no legacy directory is created or referenced | `app_dirs::tests::fresh_install_uses_emuwiz_paths` — covers both `config_path_in` and `data_path_in` |
| **Legacy-only install** (only `archivefs`-named directories exist, populated with real config/DB from before the rename) | Config, database, index, and (after Finding A is fixed) the rename-transaction journal directory all resolve to the legacy directory unchanged; nothing is copied or moved | `app_dirs::tests::legacy_only_install_transparently_reuses_archivefs_paths`; `database.rs::default_database_path_reuses_a_legacy_archivefs_home`. **Gap:** no equivalent test exists yet for `rename_transaction_dir_in` specifically (consistent with it not yet being fixed per Finding A) |
| **Mixed install** (both `archivefs`- and `emuwiz`-named directories exist, e.g. a user who already ran a build with the new code once, then reverted) | EmuWiz-named directory wins, consistently, for every resource | `app_dirs::tests::emuwiz_wins_when_both_paths_exist`; `database.rs::default_database_path_prefers_emuwiz_when_both_exist` |
| **Independently-mixed install** (config already migrated/exists under `emuwiz`, but data/database directory is still only `archivefs`, or vice versa) | Config and data resolve *independently* — this is the scenario most likely to actually occur in practice (e.g. a user manually created `~/.config/emuwiz` while troubleshooting, without touching their data directory) | `app_dirs::tests::config_and_data_directories_resolve_independently_mixed_states` — explicitly covers this exact case |
| **Legacy install with an in-flight or recently-interrupted DAT rename-transaction journal at upgrade time** | The interrupted transaction must still be found and offered for recovery/rollback after upgrading to the EmuWiz-branded build, from wherever it actually was written (legacy directory) | **No test found.** This is the scenario Finding A puts at direct risk — until that function is fixed, this scenario cannot be safely claimed to work, and even after the fix, no dedicated test exercises "upgrade mid-transaction" specifically (only "legacy directory already exists at process start" is tested) |
| **Legacy install with `ARCHIVEFS_LOG` set in the user's shell profile, upgrading to a build that also supports `EMUWIZ_LOG`** | `ARCHIVEFS_LOG` alone continues to select the log level exactly as before | `main.rs` test: `"the legacy ARCHIVEFS_LOG must still work"` |
| **Both `ARCHIVEFS_LOG` and `EMUWIZ_LOG` set simultaneously** (e.g. during a transition where a user's shell profile has the old var and a new install script adds the new one) | `EMUWIZ_LOG` takes precedence, deterministically | `main.rs` test: `"EMUWIZ_LOG must win when both are set"` |
| **Legacy install invoking the old binary name after upgrade** (`archivefs-cli` / `archivefs-gui` still on `$PATH` or referenced by an existing launcher/script/systemd unit) | Old binary name still runs, identical behavior to the new name | Structurally guaranteed by the `[[bin]]` alias approach (same `path = "src/main.rs"` for both names) rather than by a runtime test; no test exercises actually invoking the built alias binary end-to-end, which would be reasonable to add as a packaging/CI smoke test rather than a unit test |
| **`install.sh` run against a machine with only a legacy `~/.config/archivefs`** | Installs into the legacy directory, matching what the Rust binary itself would resolve to, so the two never disagree | No automated test found for `install.sh` itself in this area; `tests/test_install.sh` exists and was touched by the branch but this report did not find a specific legacy-detection assertion in it — worth explicit confirmation before merge |

## 11. Blockers, ranked by severity

1. **Finding A (`rename_transaction_dir_in` not rewired)** — must be fixed
   before merge. It is the one place found where the branch's own stated
   compatibility strategy is currently violated in code, in a
   crash-recovery-relevant subsystem.
2. **No test for "legacy install with an in-flight rename-transaction journal
   at upgrade time"** (§10) — should be added alongside the Finding A fix, so
   the fix is verified against the actual scenario it protects, not just
   against "legacy directory exists at process start."
3. **Finding C (RetroArch `--json` hint naming) needs an explicit decision**,
   not a silent pick — low severity but should be resolved deliberately
   before merge so it doesn't ship as an accidental inconsistency.
4. **Finding B (`--help` text)** — cosmetic, low severity, trivial fix,
   non-blocking but should be swept up in the same pass as A/C.
5. **Triple-implemented directory-precedence logic** (`app_dirs.rs` itself,
   `install.sh`'s shell mirror, and the `#[cfg(test)]` helpers in
   `database.rs`/`dat/sources/config.rs`/`cheat_source_registry/config.rs`) —
   not a bug today (verified consistent, §5), but a maintenance-drift risk:
   any future change to the precedence rule (e.g. adding a third directory
   tier, or an XDG env var override) has four places to update in lockstep
   with no shared source of truth for the shell-script copy. Low severity,
   worth a follow-up ticket rather than a blocker for this rename specifically.
6. **`ExternalIdentityRecord.archivefs_path` missing from Codex's own
   compatibility table** — not currently broken, but should be added so it
   stays protected against a future, unrelated cleanup pass.

None of these blockers require architectural rework; all are localized fixes
or explicit decisions on already-identified surfaces.

## 12. Sources — file:line citation index

- `crates/archivefs-core/src/lib.rs` (worktree: `default_config_path`,
  `default_index_path`, new `app_dirs` module declaration)
- `crates/archivefs-core/src/app_dirs.rs` (worktree, new file — full module
  read, including its test suite)
- `crates/archivefs-core/src/database.rs` (main: `CREATE TABLE` schema
  search, `ARCHIVEFS_TEST_HOT_JOURNAL_DB`/`_MARKER` at lines ~7355-7402;
  worktree: `default_database_path`, `resolve_database_path` under
  `#[cfg(test)]`, `archivefs_path` field usage at line ~2214/~9018)
- `crates/archivefs-core/src/dat/rename_apply/journal.rs` (worktree:
  `default_rename_transaction_dir` lines 36-38, `rename_transaction_dir_in`
  lines 41-49 — Finding A)
- `crates/archivefs-core/src/dat/sources/config.rs`,
  `crates/archivefs-core/src/patch_manager/cheat_source_registry/config.rs`
  (worktree: production vs. `#[cfg(test)]`-only path resolution, §5)
- `crates/archivefs-core/src/identity_source/settings.rs` (worktree:
  `SUGGESTED_TOKEN_PATH` change)
- `crates/archivefs-core/src/identity_source/tests.rs` (main/worktree:
  `archivefs_path()` accessor usage, confirming the field is exercised and
  untouched by the rename)
- `crates/archivefs-core/src/patch_manager/cheat_cache_lock.rs` (main: lines
  289-291, 311-315 — `ARCHIVEFS_LOCK_*` env vars)
- `crates/archivefs-gui/src/main.rs` (worktree: lines ~670-681 doc comments
  and implementation for `EMUWIZ_LOG`/`ARCHIVEFS_LOG`; lines ~40262-40272,
  the three log-level-precedence tests)
- `crates/archivefs-gui/src/romm_config.rs` (worktree: `archivefs_path` field
  on `RommIdentityExample`, lines ~521-634, 1264)
- `crates/archivefs-gui/src/rom_organisation_page.rs` (worktree:
  `archivefs_path` usage, lines ~397, ~824)
- `crates/archivefs-cli/src/main.rs` (worktree: `--help` output at line
  ~5782, Finding B; test fixtures at lines ~6462-6518)
- `crates/archivefs-cli/src/retroarch_cheat_setup.rs` (worktree: literal
  `"archivefs"` command-hint strings, lines ~702, ~764, ~771, ~781 — Finding C)
- `crates/archivefs-cli/Cargo.toml`, `crates/archivefs-gui/Cargo.toml`
  (worktree: `[[bin]]` alias declarations, with inline comments explicitly
  stating the staged-rename rationale)
- `install.sh` (worktree: `config_root`/`config_dir` fallback block, §5)
- `docs/reviews/EMUWIZ_RENAME_AUDIT.md` (worktree, committed in `b567ec0`:
  the compatibility table cross-checked in §3a and §4)
- `docs/PCSX2_CHEAT_ADAPTER.md` (main: `ARCHIVEFS_PCSX2_PROOF` documented
  usage, lines ~210-225)
- Git refs used: `origin/main` at `4cae6f5b0e1b528b0f6a83cf43d83baa3cb1a368`;
  rename branch fork point `50c76ce39131bc0b571172979d01df653edd548f`;
  rename branch committed docs commit `b567ec0c34200128956c404c1268bbdcd5e7676f`
  (local, unpushed, on `feature/emuwiz-rename` in the local worktree at
  `/home/davedap/emuwiz-rename`, plus uncommitted working-tree changes on top
  of it at time of writing).
