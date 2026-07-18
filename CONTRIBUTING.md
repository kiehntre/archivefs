# Contributing to ArchiveFS

ArchiveFS is alpha-stage software. Thank you for considering contributing -
this document covers how to build, validate, and submit changes.

## Getting the pinned toolchain

ArchiveFS pins an exact Rust toolchain in [`rust-toolchain.toml`](rust-toolchain.toml)
so that local development, CI, and release builds all use the same compiler.
If you have `rustup` installed, running any `cargo`/`rustc` command inside
the repository automatically installs and uses the pinned version - you do
not need to run `rustup override` yourself.

```sh
cd archivefs
cargo --version   # should print the version from rust-toolchain.toml
```

If `rustup` reports it needs to install the pinned toolchain, let it - do
not substitute whatever Rust you already have installed globally. See
[Rust toolchain policy](#rust-toolchain-policy) below for why this matters.

## Building and testing

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

All four should pass before opening a pull request. Before a release build
specifically, also run:

```sh
cargo build --workspace --release --locked
```

`--locked` ensures the build uses exactly the dependency versions recorded
in `Cargo.lock`, matching what CI and the release workflow use.

## Rust toolchain policy

- `rust-toolchain.toml` is the single source of truth for which Rust
  version ArchiveFS is developed and tested against. As of this writing
  that is Rust `1.97.1`, with the `rustfmt` and `clippy` components.
- `.github/workflows/ci.yml` and `.github/workflows/release.yml` both
  install that exact version explicitly (via `dtolnay/rust-toolchain` with
  `toolchain: "1.97.1"`), rather than a floating `stable` channel. A
  floating channel silently picks up new compiler releases - including new
  lints that can turn previously passing code into a build failure with no
  code change on this project's side, which is exactly what motivated
  pinning.
- Updating the pinned version is an intentional, reviewed change: bump
  `rust-toolchain.toml` and both workflow files together in the same
  change, run the full validation commands above locally first, and note
  the bump in `CHANGELOG.md`.
- Don't rely on whatever Rust happens to already be installed on your
  machine globally - if it differs from `rust-toolchain.toml`, `rustup`
  will install and use the pinned version automatically inside this
  repository; that's the point of the file.

## Making changes

- Keep changes focused. A bug fix doesn't need an accompanying refactor;
  a new command doesn't need to reorganize unrelated modules.
- Don't break the public JSON output documented in
  [`docs/json-api.md`](docs/json-api.md) - field names and types listed
  there are meant to stay stable. If a change genuinely requires breaking
  them, document it there explicitly rather than changing it silently.
- Don't break deterministic plan generation (mount plans, Library View
  plans). Tests in this codebase pin some plan IDs to real historical
  values precisely so a regression here is caught, not just "different
  output that still looks plausible."
- Preserve the existing path-safety guarantees: mounts only ever happen
  under the configured `mount_root`, unmounts only ever target paths under
  it, and Library View destinations may never overlap a source folder. See
  [`docs/security.md`](docs/security.md).
- Add emulator support through the `EmulatorAdapter` boundary
  (`crates/archivefs-core/src/patch_manager/adapter.rs`) rather than adding
  emulator-specific branches to the shared orchestration in
  `patch_manager::mod`. See [`docs/PATCH_CHEAT_MANAGER_DESIGN.md`](docs/PATCH_CHEAT_MANAGER_DESIGN.md).
- Document user-facing changes in [`CHANGELOG.md`](CHANGELOG.md) under
  `Unreleased`, and update [`ROADMAP.md`](ROADMAP.md) if the change
  completes or changes the scope of a roadmap item.
- ArchiveFS never modifies a user's source archive files. Any change that
  would make this untrue needs a very good reason and explicit discussion
  first.

## Reporting security issues

See [`SECURITY.md`](SECURITY.md).
