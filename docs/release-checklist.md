# v0.7 release checklist

The full procedure and rationale are in
[`RELEASE_ENGINEERING.md`](RELEASE_ENGINEERING.md). This checklist does not
authorize a merge, tag, or publication by itself.

## Exact commit gate

- [ ] Intended release commit recorded and branch pushed
- [ ] `git status --short` is empty
- [ ] `rustc --version` and `cargo --version` match `rust-toolchain.toml`
- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo build --workspace --release --locked`
- [ ] `scripts/security-scan.sh` and `cargo audit`

## Artifact gate

- [ ] `scripts/build-release.sh --output-dir target/release-artifacts`
- [ ] `scripts/verify-release-artifact.sh target/release-artifacts/*.tar.gz`
- [ ] `scripts/test-release-artifact-verifier.sh target/release-artifacts/*.tar.gz`
- [ ] `scripts/check-version-consistency.sh` passes for source, binaries, archive,
      and checksum
- [ ] `scripts/compare-release-builds.sh --output-dir target/reproducibility`
- [ ] Artifact contains exactly the documented seven files under one root
- [ ] Archive SHA-256 and exact artifact path recorded
- [ ] Downloaded CI artifact independently verifies

## Documentation and compatibility gate

- [ ] `CHANGELOG.md`, README, roadmap/release notes, and workspace version agree
- [ ] Prerelease/stable classification is correct
- [ ] Upgrade backup instructions are present
- [ ] Schema-5 downgrade warning is explicit; no in-place downgrade is claimed
- [ ] Previous verified binary artifact and checksum remain available

## Manual gate

- [ ] Extracted CLI and GUI tested on a supported Linux host
- [ ] GUI manual QA passes at desktop size and 1024×600
- [ ] Disposable configuration and emulator profiles used for write/rollback QA
- [ ] No live ROM, production profile, database, or configuration was modified
- [ ] Final go/no-go record names commit, host, artifact, checksum, and limitations

## Tag and publication gate

- [ ] All CI jobs are green on the exact commit
- [ ] Annotated tag name is exactly `v0.7.0-alpha`
- [ ] Tag target is independently checked before push
- [ ] Tag is pushed once; it is never moved or overwritten
- [ ] Tag workflow publishes only the canonical `.tar.gz` and `.tar.gz.sha256`
- [ ] Published files are downloaded and independently verified
- [ ] No merge or release is announced until manual QA is complete

---

## v0.7.0-alpha RC1

Source branch: `feature/gui-navigation-reset`. This section records the
actual, executed result of each check for this candidate - a box is only
checked once that exact command has run and passed against the recorded
commit. This section does not authorize a merge, tag, or publication by
itself; see the generic gates above for that authority.

### Exact commit gate (RC1 result)

- [x] Source commit recorded below and branch pushed (see "Push" at the end
      of this section for the exact hash)
- [x] `git status --short` was empty before the RC1 documentation commit
- [x] `cargo fmt --all --check` - clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` - clean, zero
      warnings
- [x] `cargo test --workspace` - **1,940 passed, 0 failed** (128 CLI + 1,177
      core unit + 14 Dolphin end-to-end + 5 PCSX2 end-to-end + 12 RetroArch
      end-to-end + 9 Xenia end-to-end + 595 GUI)
- [x] `cargo build --workspace --release --locked` - succeeded
- [x] `cargo audit` (online) - advisory DB fetched from GitHub, crates.io
      index updated, 402 dependencies scanned, **0 vulnerabilities**
- [x] `cargo audit --no-fetch` (cached) - 1,173 advisories loaded from
      cache, 402 dependencies scanned, **0 vulnerabilities**
- [x] No advisory-ignore configuration exists anywhere in the repository

### Artifact gate (RC1 result)

- [x] `scripts/build-release.sh --output-dir target/release-candidate` -
      succeeded, self-verified
- [x] `scripts/verify-release-artifact.sh` - structure, ownership, modes,
      checksum, privacy, and both binary versions verified
- [x] `scripts/test-release-artifact-verifier.sh` (malformed-artifact
      regression suite) - bad-checksum, unexpected-member, traversal,
      bad-mode, and privacy-leak fixtures all correctly rejected
- [x] `scripts/check-version-consistency.sh` (source, binaries, archive,
      checksum) - verified `0.7.0-alpha` throughout (this run also caught
      and led to fixing a CHANGELOG heading regression - see the
      Documentation gate below)
- [x] `scripts/compare-release-builds.sh` (two-build byte-for-byte
      reproducibility) - identical, matching SHA-256
      `aa5582ade4433ec9f83dc5f21ca7095f448b96dc6a4b833d82d120acabcaf104`
      across both independent isolated-target-dir builds
- [x] Archive size **13,513,500 bytes**; extracted size **37,515,485
      bytes**; canonical SHA-256
      **1749d470102ecbdf49a624d5b8995d18219b455d2a1cffac87602c6e99a35469**;
      artifact path
      `target/release-candidate/archivefs-v0.7.0-alpha-x86_64-linux.tar.gz`
      (not committed to Git - build output only)

### Documentation gate (RC1 result)

- [x] `CHANGELOG.md` v0.7.0-alpha entry rewritten to accurately group
      changes (Gamer View, Cheats & Mods, Emulator adapters, Safety and
      Undo, Coverage reporting, Release engineering, Dependency security,
      Documentation, Known limitations), with no "full support"/"complete
      coverage"/"production ready" language
- [x] `docs/releases/v0.7.0-alpha.md` created with all 16 required
      sections; final checksum recorded once the canonical build below
      passed verification
- [x] `scripts/check-version-consistency.sh`'s own `## v$VERSION
      (unreleased)` exact-heading requirement caught a heading change
      ("(release candidate)") in the first draft of the CHANGELOG entry;
      fixed in a follow-up commit and reverified before proceeding - see
      commit `5425d58`
- [x] Corrected a stale factual claim in `README.md`'s release-status
      paragraph (previously said "PCSX2 remains read-only," which is no
      longer accurate at the `archivefs-core` API level - the paragraph now
      distinguishes the new core capability from GUI/CLI non-reachability)
- [x] Reviewed `docs/ADAPTER_SUPPORT_MATRIX.md`, `docs/CHEAT_PROVIDER_COVERAGE.md`,
      `docs/PCSX2_CHEAT_ADAPTER.md`, `docs/PLATFORM_ARTWORK.md`,
      `docs/DEPENDENCY_SECURITY.md`, `docs/RELEASE_ENGINEERING.md`,
      `docs/GUI_NAVIGATION_RESET_DESIGN.md`, and `docs/json-api.md` for
      factual consistency - no further release-blocking inaccuracy found
- [x] Confirmed README's other PCSX2 "preview-only"/"does not install
      content" claims remain accurate: the GUI's PCSX2 workflow still
      renders `show_pcsx2_installation_unavailable` unconditionally, and no
      CLI subcommand for PCSX2 install exists - the new core capability is
      genuinely not user-reachable yet

### Manual gate (RC1 result)

- [x] Extracted release-candidate CLI/GUI (not the working-tree binary)
      manually tested by the user - full pass on GUI startup, Gamer View
      default, search, platform cards/scrolling/tooltips, game selection,
      Mount as primary action, Cheats & Mods + "Back to games", Dolphin and
      RetroArch cheat-workflow availability, PCSX2 recognition, Advanced
      View + Return to Gamer View, file dialog, clipboard copy/paste - no
      crash, freeze, or rendering corruption
- [x] X11 manual QA - passed (above)
- [x] Wayland manual limitation recorded honestly - `WAYLAND_DISPLAY` was
      unset/absent in the test environment for this RC1 run; only X11 was
      manually exercised, exactly as recorded in the release notes
- [x] Dolphin manual proof (Animal Crossing, GAFE01) - `cheat-provider-coverage`
      against an isolated database copy reports 1 compatible cheat,
      verified identity GAFE01, region USA, revision 0
- [x] RetroArch manual proof (Arena and Lunar, both GameGear) - Arena: 6
      compatible cheats, 2 rejected (weak evidence only); Lunar
      translation entry: 0 compatible, rejected because multiple
      candidates tied and none was selected silently - both exactly as
      expected
- [x] PCSX2 recognition proof (without install) - confirmed via manual GUI
      QA above; the workflow still renders "installation unavailable"
- [x] PCSX2 catalogue limitation explicitly confirmed unchanged - no
      approved downloadable ordinary-cheat catalogue is bundled; unchanged
      from the prior integration
- [x] No live ROM, production profile, database, or configuration was
      modified - the coverage spot check ran against a disposable `/tmp`
      copy of `~/.local/share/archivefs`, deleted immediately after use;
      the live `library.sqlite3`'s mtime was confirmed unchanged
      (2026-07-29 01:02, well before this RC1 session) both before and
      after

### Repository hygiene (RC1 result)

- [x] `git status --short` clean before each commit; `git diff --check`
      clean; `git ls-files -o --exclude-standard` empty (no stray
      untracked files); `scripts/security-scan.sh` scanned 200 tracked
      files, no credential-shaped secrets found; no `.log`/`.tmp`/`.bak`/
      `.orig` files outside `target/`; no merge-conflict markers in any
      tracked source/doc/config file; no ROM/BIOS/database/emulator-profile
      file extensions and no `.tar.gz`/`.sha256` release artifacts are
      tracked by Git
- [x] No tag created
- [x] No publication of any kind occurred

### Push

- Commit hash and remote-sync confirmation recorded in the final RC1
  report, not duplicated here to avoid this file needing another edit
  after every push.
