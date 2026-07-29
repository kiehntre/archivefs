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
