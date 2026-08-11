# BSFree Wii cheat apply (P0)

Status: implemented on `feature/bsfree-wii-p0`.

This is the Wii counterpart of
[`BSFREE_GAMECUBE_CHEAT_APPLY.md`](BSFREE_GAMECUBE_CHEAT_APPLY.md): the same
classify → dedup → preview → apply → rollback pipeline, wired for Wii through
the existing GameHacking Wii / Dolphin adapter. It deliberately does **not**
introduce any new cheat-conversion semantics, any new filesystem mutation
engine, or any encrypted Action Replay decryption.

## What is installable

Only BSFree Wii records that classify into a format already trusted by the
GameHacking Wii / Dolphin path:

1. The record's declared device is a verified Wii format — **Action Replay** or
   **Gecko** (the same explicit-format gate `WiiCheatSafety` enforces for
   GameHacking Wii). Any other device (GameShark, CodeBreaker, CWCheats,
   unknown) is browse-only.
2. Every code line is a strict `XXXXXXXX YYYY` hex-pair with no placeholders.
3. No master/enable-code dependency and no Action Replay command Dolphin
   refuses at runtime (master/zero/self-modifying codes).
4. Within that subset, a body whose lines are all Action Replay 32-bit RAM
   writes with a Gecko-addressable address is emitted into `[Gecko]` unchanged
   (byte-identity, shared with GameCube); every other well-formed hex-pair code
   is emitted verbatim into `[ActionReplay]`.

## What remains browse-only

- **Encrypted dash-format Action Replay codes** (`XXXX-XXXX-XXXXX`): real AR
  content that Dolphin could decrypt, but EmuWiz has no verified decryptor.
  Deferred to a later PR; never "fixed up".
- Placeholders, free text, and malformed bodies.
- Unknown or unverified devices.
- Any format requiring an unimplemented conversion.

## Identity

The destination is keyed by the selected archive's **verified Dolphin Wii
Game ID** (`WiiGameIdentity`), exactly like the GameHacking Wii adapter.
BSFree contributes only platform + normalized title + version/region evidence,
which always requires explicit user confirmation before Apply. An ambiguous or
missing identity never installs.

## Duplicate / conflict analysis (shared and cross-provider)

The GameCube duplicate/conflict analyser has been generalized into a
platform-parameterized module (`dolphin_dedup.rs`) that both GameCube and Wii
use, and that can be fed records from **multiple providers at once** (BSFree
Wii and GameHacking Wii). It preserves the original findings exactly and adds:

- `ConflictingMemoryWrite` — two *different*, differently-labelled cheats that
  both write to the same provable address+size with different values. Blocked;
  never silently resolved by priority. Only provable direct writes
  (8/16/32-bit and float) participate; pointer/conditional/master bodies can
  never be the subject because their target addresses are not provable.
- Exact duplicates are reported (`DuplicateBody` / `AlreadyInstalled*`) and
  never installed twice.
- Same display name with different operations is a `DuplicateNameConflict`,
  never a collapse.

GameCube behaviour is unchanged: `analyze_bsfree_gamecube_duplicates` is a thin
wrapper over the shared analyser and produces the same findings.

## Transaction safety

Reuses the shared preview / confirmation / atomic apply / journal / rollback
engine. Proven by the `bsfree_wii_install_end_to_end` suite: preview writes
nothing, dry-run apply writes nothing, apply creates the correct journal and
exact previewed bytes, rollback restores the exact original Dolphin file, a
second rollback is non-destructive, and two byte-identical selected labels
stage exactly one physical write.

## Data availability

The shipped BSFree catalogue snapshot contains **no Wii rows** (see
`BSFREE_GAMECUBE_CHEAT_APPLY.md`), so the search normally resolves to
`NoMatch`. The classifier, staging, rollback and cross-provider analysis are
implemented and tested with representative fixtures so the pipeline activates —
and stays conservative — if a database containing Wii rows is ever loaded.

## CLI

`cheats source bsfree wii-preview|wii-apply|wii-rollback` mirror the GameCube
commands (`--archive`, `--game-id <verified Wii Game ID>`, `--configuration-path`,
`--bsfree-game`, `--select|--select-all`, `--confirm`).

## GUI

For a Wii game, the Cheats & Mods BSFree section shows the same beginner flow
as GameCube: search → match/candidate review → select → preview → install →
result → rollback, with simple states (`Ready`, `Already installed`,
`Conflict`, `Browse only`) and provider/format provenance kept behind
disclosures. BSFree and GameHacking Wii results coexist; the shared analyser
keeps them from colliding silently.
