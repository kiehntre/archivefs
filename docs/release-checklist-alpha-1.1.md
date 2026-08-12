# EmuWiz v0.7.1-alpha ("Alpha 1.1") release checklist

This checklist is specific to the `v0.7.1-alpha` stabilization release. Do
not reuse [`docs/release-checklist.md`](release-checklist.md) for this
release - that file records the already-shipped `v0.7.0` promotion.

Do not create the `v0.7.1-alpha` tag until every automated gate below has a
recorded pass and every manual smoke journey has been executed and signed
off against the exact commit being released.

## Source and version

- [ ] Workspace version is `0.7.1-alpha` for `archivefs-core`, `archivefs-cli`,
      and `archivefs-gui` (`Cargo.toml` `[workspace.package].version`).
- [ ] `Cargo.lock` reflects the same version for all three workspace members.
- [ ] CLI and GUI `--version` resolve from Cargo metadata (no hardcoded
      version string elsewhere).
- [ ] `CHANGELOG.md` heading reads `## v0.7.1-alpha (unreleased)` until
      tagged, and `## v0.7.0 (2026-08-01)` no longer claims "(unreleased)".
- [ ] Schema remains version 6; migrations are exactly `0001`-`0006`
      (`crates/archivefs-core/src/migrations/`). No new migration ships in
      this release.
- [ ] No ROM, disc image, optional BSFree database, secret, or build output
      is tracked or staged.

## Automated gates

Run from a clean clone (`scripts/build-release.sh` refuses a dirty tree):

- [ ] `git diff --check`
- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo audit`
- [ ] `scripts/security-scan.sh`
- [ ] `bash tests/test_install.sh`
- [ ] `scripts/build-release.sh` (canonical release artifact build)
- [ ] `scripts/check-version-consistency.sh` against the built binaries,
      artifact, and checksum
- [ ] `scripts/verify-release-artifact.sh` / `scripts/test-release-artifact-verifier.sh`
      (canonical artifact verifier and negative-fixture suite, whichever apply)
- [ ] Built `emuwiz-cli --version` and `emuwiz --version` both report
      `0.7.1-alpha`, not `0.7.0`.

Record pass/fail and any error output for each gate in the release PR before
tagging.

## Manual smoke gates

Each journey below must be executed on a real Linux desktop session against
the exact commit being released. Record the tester, date, commit SHA, and
outcome for each.

### A. Previous-alpha / pre-manifest upgrade

Exercises the installer ownership hardening shipped in this release (#38,
`install.sh`, see its ownership/manifest design comments at the top of the
script).

- [ ] First run against a HOME with binaries from a pre-manifest install (no
      `$XDG_DATA_HOME/emuwiz-installer/manifest`) safely refuses to overwrite
      the existing binaries and reports them as foreign, per `install.sh`'s
      "no manifest, treat existing binaries as foreign" rule.
- [ ] Rerunning with `--replace-foreign` takes ownership, and a backup of the
      previously-foreign file is created before it is replaced.
- [ ] The backup is preserved (not deleted) after a successful
      `--replace-foreign` run.
- [ ] Binary aliases are present after install: `emuwiz-cli`, `emuwiz`,
      `emuwiz-gui`, `archivefs-cli`, `archivefs-gui`.
- [ ] The ownership manifest written after the run correctly lists every
      asset this run installed (SHA-256 identity, not mtime).
- [ ] Desktop launcher entry (`io.github.kiehntre.emuwiz.desktop`, from
      `assets/linux/io.github.kiehntre.emuwiz.desktop.in`) and icon are
      present after install.
- [ ] Uninstall preserves foreign and user data: any file the manifest does
      not own, and any user config/data outside the installer's own asset
      set, is left untouched.

### B. Legacy profile first launch

Exercises EmuWiz-first-with-legacy-fallback path resolution
(`crates/archivefs-core/src/app_dirs.rs`).

- [ ] With only `~/.config/archivefs` and `~/.local/share/archivefs`
      present (no `emuwiz`-named directories), EmuWiz recognizes and uses
      the legacy paths rather than creating new empty EmuWiz directories.
- [ ] A schema-6 database under the legacy data directory opens correctly.
- [ ] Settings, provider configuration (RomM, cheat sources, DAT sources),
      and GUI state (last-used view, remembered choices) carry over exactly
      as they were under the legacy path.
- [ ] Desktop launch (from the installed `.desktop` entry) works against the
      legacy profile.
- [ ] No duplicate empty `~/.config/emuwiz` or `~/.local/share/emuwiz`
      profile is created alongside the legacy one merely by launching or
      reading configuration (`app_dirs::choose_dir` is read-only and must
      not create anything - see the `resolution_never_creates_anything` unit
      test for the invariant this manual check reconfirms end-to-end).

### C. DAT preview/apply/restart/rollback

Exercises `crates/archivefs-core/src/dat/rename_apply/` end to end, using a
disposable Games-only No-Intro DAT and a disposable copy of source files (never
real/irreplaceable ROMs).

- [ ] Build a rename plan from a fresh audit against the disposable DAT
      source; confirm the Games-only policy is applied where selected.
- [ ] Preview the plan in the GUI; confirm collisions and unsafe (symlink)
      sources are correctly flagged and excluded from what can be approved.
- [ ] Approve a subset of proposals and apply; confirm the journal is written
      before any rename (inspect the journal file under the DAT
      rename-apply journal directory) and that applied entries pass
      filesystem confirmation (`confirm_rename` in `executor.rs`).
- [ ] Restart the app after apply completes; confirm the transaction still
      shows as `Applied` and the entries are visible in journal/recovery UI.
- [ ] Roll back the applied transaction
      (`crates/archivefs-core/src/dat/rename_apply/rollback.rs`); confirm
      files return to their original names and identity.
- [ ] Throughout, confirm no-clobber behavior held: at no point did apply or
      rollback overwrite an existing file at a destination path
      (`rename_noreplace` in `noclobber.rs`).

### D. Stale classifier / legacy journal

Exercises the classifier-version enforcement shipped in this release (#39,
`validate_classifier_version` in
`crates/archivefs-core/src/dat/rename_apply/executor.rs`) and journal
backward-compatibility. `validate_classifier_version` takes
`plan_version: Option<&str>` and treats "mismatched version" and "no version
at all" as two independent inputs to the same check - both must be verified,
not just one:

- [ ] **Mismatched classifier version.** Build a plan, then force its
      recorded `classifier_version` to a *different* string than the current
      `dat::classification::CLASSIFIER_VERSION` (e.g. by reloading against a
      build with a bumped classifier, or by editing a saved plan file in a
      test-only setup), so `validate_classifier_version(Some("old-version"))`
      is exercised. Applying this plan must perform **zero** filesystem
      mutation and must surface the "Rename plan is stale because
      classification rules changed. Regenerate the plan before applying."
      message (`ApplyError::StaleClassifierVersion { plan: Some(_), .. }`),
      not a partial or silent apply.
- [ ] **Missing classifier provenance entirely.** Build or edit a plan whose
      `classifier_version` field is absent/`None` rather than merely old
      (a plan with no version-provenance info at all is a distinct case from
      "mismatched" - do not conflate the two, and confirm the checklist item
      above was exercised with an actual differing string, not with a blank
      one standing in for this case). This exercises
      `validate_classifier_version(None)`, which must also reject the apply
      before any journal write or mutation, surfacing
      `ApplyError::StaleClassifierVersion { plan: None, .. }` with the same
      "regenerate the plan" message - not a crash, not a silent
      accept-as-current.
- [ ] **Legacy journal.** A journal produced by a pre-`v0.7.1-alpha` build
      (before classifier-version enforcement existed) remains inspectable: it
      opens without error in the Alpha 1.1 build's journal/recovery UI, it
      remains recoverable (crash reconciliation via
      `crates/archivefs-core/src/dat/rename_apply/reconcile.rs` correctly
      classifies any in-flight entries), and it remains rollback-able
      (`crates/archivefs-core/src/dat/rename_apply/rollback.rs`) - this is a
      backward-compatibility guarantee on the journal itself, independent of
      whether that legacy journal's transaction happens to carry a
      classifier version.

### E. Real GUI/provider/emulator workflow

A real desktop-session test. Run under X11 first; if only Wayland is
available in the tester's environment, note that explicitly - Wayland
coverage is separately tracked and thinner than X11 coverage, and must not be
recorded as equivalent to an X11 pass.

- [ ] Portal folder picker (`rfd::FileDialog::new().pick_folder()`, used in
      `crates/archivefs-gui/src/dat_sources_page.rs` and
      `crates/archivefs-gui/src/main.rs`) opens and returns a chosen folder
      correctly.
- [ ] Cached, offline, and provider-error states are exercised for at least
      one identity/artwork provider (RomM) and one cheat/patch provider
      (RetroArch or GameHacking): confirm each state is shown honestly (not
      silently treated as success) and that no network call happens without
      an explicit trigger.
- [ ] A disposable PCSX2 profile: preview a cheat/patch install, apply it,
      then Undo it; confirm the profile is restored and the Undo is recorded
      as managed-only removal (does not touch unrelated PCSX2 configuration).
- [ ] A disposable Dolphin profile: same preview -> apply -> Undo cycle for a
      GameCube or Wii cheat.
- [ ] Desktop launcher and icon: launching EmuWiz from the installed
      `.desktop` entry starts the GUI with the correct window icon and
      `StartupWMClass` (`io.github.kiehntre.emuwiz`).

### F. Pristine clean install

Exercises the actual produced release artifact end to end on a fresh user
account/profile with **zero** prior EmuWiz or ArchiveFS state - no
`~/.config/emuwiz`, `~/.config/archivefs`, `~/.local/share/emuwiz`, or
`~/.local/share/archivefs` directories, and no pre-existing installer
manifest. This is distinct from Journey A (which exercises upgrading an
*existing* pre-manifest install) and Journey B (which exercises an existing
*legacy* profile) - Journey F must start from genuinely nothing.

- [ ] Extract the exact artifact `scripts/build-release.sh` produced (e.g.
      `archivefs-v0.7.1-alpha-x86_64-linux.tar.gz`) - not a locally built
      binary - into a clean directory on the fresh account.
- [ ] Run `./install.sh` from the extracted artifact; confirm it completes
      without error and without requiring `sudo`.
- [ ] First CLI check: `emuwiz-cli --version` (and `archivefs-cli --version`
      alias) report exactly `0.7.1-alpha`, resolved from Cargo metadata, not
      a hardcoded string.
- [ ] First GUI launch: `emuwiz` (and the `emuwiz-gui` / `archivefs-gui`
      aliases) start cleanly with no prior config present; confirm the
      first-run path is taken (`crates/archivefs-cli/src/main.rs`'s
      "first-run hint" logic, see
      `confirmed_missing_config_gets_a_first_run_hint`) rather than the
      missing-config case being reported as a bare OS error.
- [ ] Initial config/default behavior: confirm EmuWiz creates its own
      `~/.config/emuwiz` / `~/.local/share/emuwiz` directories on this
      genuinely-fresh profile (no legacy ArchiveFS directories exist to fall
      back to - `app_dirs::choose_dir` in
      `crates/archivefs-core/src/app_dirs.rs`), and that defaults (no DAT
      sources, no cheat sources, no RomM source configured) are presented
      honestly as empty/first-run states, not errors.
- [ ] Dependency-warning behavior: with `ratarmount` absent from `PATH` on
      this fresh account, confirm Doctor/diagnostics surfaces it as a clear,
      actionable `Configuration`-category finding (see the `"ratarmount"`
      handling in `crates/archivefs-core/src/diagnostics/mod.rs`) rather than
      a crash or a silent no-op when a mount is attempted.
- [ ] Desktop launcher and icon: after install, the
      `io.github.kiehntre.emuwiz.desktop` entry and icon are present and
      launching EmuWiz from it starts the GUI with the correct window icon
      and `StartupWMClass`.
- [ ] Uninstall: run the installer's uninstall path; confirm it completes
      without error.
- [ ] Post-uninstall data check: confirm user data created during this
      journey (config, database, any DAT/cheat-source registrations) and any
      foreign, non-EmuWiz-owned file placed in the same directories are left
      untouched by uninstall - uninstall removes only demonstrably-owned
      installer assets (the manifest-tracked binaries, desktop entry, and
      icon), never user data and never a foreign path.

## Publication gate

- [ ] All automated gates above pass on the exact commit to be released.
- [ ] All six manual smoke journeys (A-F) are executed and signed off
      against that same commit.
- [ ] Explicit authorization received to merge, tag, and publish
      `v0.7.1-alpha`.
- [ ] Annotated tag is exactly `v0.7.1-alpha` and points at the final main
      release commit.
- [ ] Published assets are exactly the verified `archivefs-v0.7.1-alpha-x86_64-linux.tar.gz`
      archive and its `.sha256` checksum.

Do not create the tag until every box above is checked against the exact
final commit.
