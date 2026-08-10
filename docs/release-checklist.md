# EmuWiz v0.7.0 release checklist

This checklist records promotion of the manually approved v0.7.0-rc.1
candidate. Publication still requires the exact final commit, merge, and tag
checks below.

## Source and version

- [x] Approved RC commit recorded:
      `113b508bd6839309cfd9e25d054094cc27112860`.
- [x] Workspace version is `0.7.0` for core, CLI, and GUI.
- [x] CLI and GUI obtain their version from Cargo metadata.
- [x] Changelog and release notes preserve RC history and describe v0.7.0.
- [x] Schema is version 6; migrations are exactly `0001`–`0006`.
- [x] No ROM, disc image, optional BSFree database, secret, or build output is
      tracked or staged.

## Automated gates

- [x] `cargo fmt --all`
- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace` (2,581 passed, 0 failed)
- [x] locked release GUI build
- [x] locked release CLI build
- [ ] version-consistency script against source, binaries, archive, and checksum
- [x] secret scan (259 tracked files; no credential-shaped secret found)
- [ ] canonical artifact verifier and negative verifier suite
- [x] clean-install CLI/Doctor/BSFree smoke in isolated HOME/XDG directories
- [x] representative v0.6-state upgrade smoke (schema 4 to 6, settings and
      manual assignment preserved)

## Artifact

- [ ] Canonical `archivefs-v0.7.0-x86_64-linux.tar.gz` created by
      `scripts/build-release.sh` from the reviewed commit.
- [ ] Adjacent `.sha256` file created and verified.
- [ ] Archive contains only CLI, GUI, installer, README, changelog, licence, and
      example configuration with the documented modes/ownership.
- [ ] Both extracted binaries print `0.7.0`.
- [ ] No maintainer path, credential-shaped value, ROM, database, or emulator
      configuration is present.

## Manual desktop gate

- [x] Approved RC GUI launched on the real Sunshine desktop.
- [x] Gamer View shelf layout and navigation were verified.
- [x] Atari ST display was verified.
- [x] BSFree showed Ready/browse-only behavior and no BSFree Install action.
- [x] Manual GUI testing was approved with no remaining release blocker.

## Publication gate

- [x] Candidate branch reviewed and manually approved.
- [x] Manual results recorded against the exact RC commit.
- [x] Explicit authorization received to merge, tag, and publish v0.7.0.
- [ ] Annotated tag is exactly `v0.7.0` and points at the final main release
      commit.
- [ ] Published assets are exactly the verified archive and checksum.

Do not create the final tag until the remaining artifact and publication
checks have completed against the exact final main commit.
