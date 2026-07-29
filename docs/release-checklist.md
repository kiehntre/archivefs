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

- [ ] `scripts/build-release.sh --output-dir target/release-candidate` -
      _recorded after the documentation commit, below_
- [ ] `scripts/verify-release-artifact.sh` - _pending_
- [ ] `scripts/test-release-artifact-verifier.sh` (malformed-artifact
      regression suite) - _pending_
- [ ] `scripts/check-version-consistency.sh` (source, binaries, archive,
      checksum) - _pending_
- [ ] `scripts/compare-release-builds.sh` (two-build byte-for-byte
      reproducibility) - _pending_
- [ ] Archive size, extracted size, SHA-256, and exact artifact path
      recorded - _pending_

### Documentation gate (RC1 result)

- [x] `CHANGELOG.md` v0.7.0-alpha entry rewritten to accurately group
      changes (Gamer View, Cheats & Mods, Emulator adapters, Safety and
      Undo, Coverage reporting, Release engineering, Dependency security,
      Documentation, Known limitations), with no "full support"/"complete
      coverage"/"production ready" language
- [x] `docs/releases/v0.7.0-alpha.md` created with all 16 required
      sections; final checksum left as a placeholder pending the canonical
      build below
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

- [ ] Extracted release-candidate CLI/GUI manually tested - _pending, see
      below_
- [ ] X11 manual QA - _pending_
- [ ] Wayland manual limitation recorded honestly (not fabricated) -
      _pending_
- [ ] Dolphin manual proof (Animal Crossing/GAFE01) - _pending_
- [ ] RetroArch manual proof (Arena and Lunar/GameGear) - _pending_
- [ ] PCSX2 recognition proof (without install) - _pending_
- [ ] PCSX2 catalogue limitation explicitly confirmed unchanged - _pending_
- [ ] No live ROM, production profile, database, or configuration was
      modified - _pending confirmation after manual QA_

### Repository hygiene (RC1 result)

- [ ] `git status --short`, `git diff --check`, `git ls-files -o
      --exclude-standard`, and a tracked-file security scan all reviewed -
      _pending, recorded after artifact build_
- [ ] No tag created
- [ ] No publication of any kind occurred

### Push

- Commit hash and remote-sync confirmation recorded in the final RC1
  report, not duplicated here to avoid this file needing another edit
  after every push.
