# Release Checklist

## Code

- [ ] `rustc --version` / `cargo --version` match `rust-toolchain.toml`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo build --workspace --release --locked`

## Documentation

- [ ] README reviewed
- [ ] Architecture docs updated (`ARCHITECTURE.md`, `docs/architecture.md`)
- [ ] Roadmap updated (`ROADMAP.md`) - move completed items out of future sections
- [ ] Changelog updated (`CHANGELOG.md`) - move `Unreleased` entries under the new version

## Version

- [ ] Bump `version` in the workspace `Cargo.toml`
- [ ] `cargo build --workspace --release --locked` succeeds with the new version

## Git and CI

- [ ] Working tree clean
- [ ] CI green on the commit being tagged (`.github/workflows/ci.yml`)
- [ ] Tag pushed (`vX.Y.Z-alpha` or similar) - this triggers
      `.github/workflows/release.yml`, which installs the exact pinned
      Rust toolchain, builds `--release --locked`, and publishes the
      GitHub Release with checksums
- [ ] Release artifacts and `SHA256SUMS` verified on the GitHub Releases page
