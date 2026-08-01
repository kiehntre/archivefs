# ArchiveFS v0.7.0-rc.1 release checklist

This checklist tracks the candidate branch only. It does not authorize a tag,
GitHub Release, or merge to `main`.

## Source and version

- [ ] Candidate commit recorded after final review.
- [x] Workspace version is `0.7.0-rc.1` for core, CLI, and GUI.
- [x] CLI and GUI obtain their version from Cargo metadata.
- [x] Changelog and release notes describe the RC rather than old alpha state.
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
- [x] secret scan (258 tracked files; no credential-shaped secret found)
- [ ] canonical artifact verifier and negative verifier suite
- [x] clean-install CLI/Doctor/BSFree smoke in isolated HOME/XDG directories;
      GUI desktop launch remains a manual gate because this shell has no X server
- [x] representative v0.6-state upgrade smoke (schema 4 to 6, settings and
      manual assignment preserved)

## Artifact

- [ ] Canonical `archivefs-v0.7.0-rc.1-x86_64-linux.tar.gz` created by
      `scripts/build-release.sh` from the reviewed commit.
- [ ] Adjacent `.sha256` file created and verified.
- [ ] Archive contains only CLI, GUI, installer, README, changelog, licence, and
      example configuration with the documented modes/ownership.
- [ ] Both extracted binaries print `0.7.0-rc.1`.
- [ ] No maintainer path, credential-shaped value, ROM, database, or emulator
      configuration is present.

## Manual desktop gate

- [ ] Launch extracted GUI on the supported Sunshine/Nobara desktop.
- [ ] Verify Gamer View shelf layout, arrows, wheel/trackpad/keyboard/TV input.
- [ ] Verify platform confidence and Atari ST `.st`/`.stx` presentation.
- [ ] Verify Sources shows BSFree only when configured and labels it Browse only.
- [ ] Verify PS2/GameCube/Wii previews with disposable emulator profiles.
- [ ] Verify Doctor findings and safe-repair review/cancel flow.
- [ ] Confirm no source ROM/archive changes and no unexpected network request.

## Publication gate

- [ ] Candidate branch reviewed and approved.
- [ ] Manual results recorded against the exact candidate commit and artifact
      SHA-256.
- [ ] Explicit authorization received to merge or tag.
- [ ] Annotated tag is exactly `v0.7.0-rc.1` and points at the reviewed commit.
- [ ] Published assets are exactly the verified archive and checksum.

Until every manual/publication item is checked, the candidate is suitable for
manual testing but not for a final release tag.
