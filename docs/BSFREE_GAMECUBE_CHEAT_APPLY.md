# BSFree GameCube cheat apply

Status: implemented on `feature/bsfree-cheat-apply`.

ArchiveFS now applies a small, proven subset of the BSFree Archive database
through its **existing** GameCube GameHacking adapter and shared
preview/apply/journal/rollback transaction pipeline. Everything else in BSFree
remains browse-only. This document records the data audit, the identity
strategy, the format classification (including the only AR→Gecko conversion
that is safe), the dedup rules, and the proof of rollback.

## Why only GameCube hex-pair codes

The BSFree database (pinned SHA-256 `4cfee264…`, ~296 MB) was audited directly
for this work. Findings:

- **Schema**: seven tables (`systems`, `devices`, `system_devices`, `games`,
  `sections`, `authors`, `codes`). The `codes.code` field is free-form text;
  the only code-format metadata is the row's `device_id` (device names are:
  Game Busters, Game Genie, Red Dragon, CWCheats, Pro Action Replay, Action
  Replay, Xploder, GameShark, CodeBreaker, Action Replay Max, GameGuru). There
  is **no** per-code type/format/region/revision column.
- **Systems**: 44 system rows across PSX, NES, SNES, Genesis, Game Gear, GB,
  Virtual Boy, PSP, MAME, Master System, GameCube, N64, Dreamcast, Saturn, PS2,
  GBA, DS, 3DO. There is **no Wii** and **no Xbox 360**.
- **Game identity**: `games.name` (title) and `games.version` (free text, e.g.
  "USA", "Version 1.1", or junk like "OFFLINE CODES ONLY"). There are **no**
  serials, CRCs, hashes, or emulator Game IDs.
- **Code formats** (GameCube, 82 889 records; device "Action Replay"):

  | Format | Count | ArchiveFS disposition |
  |---|---|---|
  | Base-31 encrypted AR (`XXXX-XXXX-XXXXX`) | 74 530 | Browse-only (no verified decryptor) |
  | Master/zero/self-modifying hex-pair AR | ~450 | Browse-only (Dolphin refuses at runtime) |
  | Malformed / placeholders (`XXXX`, `?`, `N/A`, `xx`) | ~6 300 | Browse-only |
  | Well-formed hex-pair AR | ~1 600 | **Installable via Dolphin** |

- **PS2** (797 942 records): CodeBreaker/GameShark/ARMax format. PCSX2 needs
  raw PNACH `patch=` lines; translating those code families requires dedicated
  decoders that ArchiveFS does not have and this work does not invent. PS2 is
  therefore browse-only.
- **Everything else** (NES/SNES/Genesis/GB/GBA/N64/DS/PSP/etc.): device codes
  (Game Genie, etc.) with no existing adapter. Browse-only.

## Identity strategy

BSFree carries no emulator-stable identifier, so the destination is keyed by
the **selected game archive's verified Dolphin Game ID** (exactly like the
existing GameCube GameHacking adapter). The BSFree game contributes only
platform + normalized title + region/version evidence. Matching:

- Platform must resolve to `GameCube`.
- Normalized title must agree exactly (region/edition markers such as
  "(USA)"/"(Rev 1)" are stripped so "Luigi's Mansion (USA)" matches
  "Luigi's Mansion" without weakening the equality).
- The result is always `requires_review`; the CLI additionally refuses to apply
  when an archive `--title` is supplied and disagrees with the selected BSFree
  game, and the preview is always `VerifiedExact` on the archive's Game ID.
- Nothing applies automatically.

## Format classification and the only safe AR→Gecko conversion

Every line is decoded under Dolphin's Action Replay bit layout
(`subtype:2 | type:3 | size:2 | gcaddr:25`, from `ActionReplay.cpp`) and the
Gecko code handler (`docs/codehandler.s`):

- **`GeckoEquivalent`** – every line is an Action Replay 32-bit RAM write
  (`04XXXXXX YYYYYYYY`) whose address fits Gecko's 24-bit address field
  (`gcaddr < 0x01000000`). Dolphin's own sources show the identical bytes are
  executed the same way under `[Gecko]` (single 32-bit `stw` to the same
  address). The code is emitted into `[Gecko]` **byte-identically**; there is
  no byte transformation. This is the only AR family with a proven,
  semantics-preserving Gecko equivalent.
- **`ActionReplayNative`** – any other well-formed hex-pair code Dolphin's AR
  engine implements (8/16-bit and float RAM writes, pointer writes, add codes,
  conditionals). Emitted verbatim into `[ActionReplay]`. Never relabelled as
  Gecko.
- **`Unsupported`** – well-formed hex pairs containing a master code, zero
  code, or self-modifying code (Dolphin refuses these at runtime).
- **`Malformed`** – anything else (placeholders, encrypted dash-format codes,
  free text).

The classified result is a normalized intermediate representation
(`BsFreeGameCubeCheat`) that separates provider syntax from emulator output;
the adapter decides the emulator representation.

## Reuse of the existing pipeline

The provider never writes an emulator file. Flow:

1. `classify_bsfree_gamecube_cheat` + `bsfree_gamecube_cheats` (read-only).
2. `BsFreeGameCubeCheatSelection` (selectable = installable format + well-formed
   lines; unselectable formats can never become selected).
3. `stage_bsfree_gamecube_install` → the existing
   `stage_gamecube_gamehacking_install` (routes `[Gecko]`/`[ActionReplay]` +
   `_Enabled` + the `ArchiveFS_Managed_GameHacking` bookkeeping section),
   writing only to the managed staging root.
4. `build_bsfree_gamecube_install_preview` → the existing shared preview,
   `VerifiedExact` on the archive's Game ID.
5. `build_shared_transaction_plan` +
   `require_dolphin_managed_gamehacking_verification` +
   `execute_shared_apply` (backup, atomic write, journal).
6. `preview_shared_rollback` + `execute_shared_rollback`.

## Duplicate / conflict analysis (two passes)

- **Source-level** (before any conversion): duplicate records, duplicate bodies
  under different labels, same-name-different-body conflicts.
- **Output-level** (after classification): two selected codes resolving to
  byte-identical output are deduplicated; output matching an already-installed
  code is reported `Already installed`; the same body present in the *other*
  Dolphin section is a `CrossSectionCollision` (blocked, review required); a
  same-name-different-body collision blocks staging and is never overwritten.
- The existing adapter's own merge rules (same-name-same-body = no-op;
  same-name-different-body = hard conflict) remain as a second layer.

## Rollback proof

End-to-end tests (and a manual smoke test against the real database) prove:
apply → the exact previewed bytes appear in the real GameSettings INI with the
user's own codes preserved byte-for-byte; rollback → the exact prior file is
restored; a second rollback is blocked non-destructively by the completion
marker. A failed apply leaves a recoverable journal.

## Licence / provenance

Unchanged: the BSFree database-content licence is not established and remains
clearly warned in the GUI and CLI. Applying a code locally does not establish
redistribution rights; the database is never bundled into ArchiveFS, and
acquisition stays explicit (download or local import).

## Tests

- Unit (`bsfree_gamecube/tests.rs`): classification for every family, the
  byte-identity conversion, address-range guard, unsupported/malformed
  refusal, source-level and output-level duplicate/conflict detection,
  already-installed and cross-section collision detection, identity matching.
- End-to-end (`tests/bsfree_gamecube_install_end_to_end.rs`): preview makes no
  changes, apply installs and preserves user codes, rollback restores the exact
  original, second rollback is blocked, dry-run writes nothing.
- CLI: capability output, title guard, shared-pipeline reuse assertions.

## Remaining limitations

- Only GameCube well-formed hex-pair Action Replay codes are installable;
  ~90% of GameCube codes (encrypted dash format) and every other platform
  remain browse-only.
- The encrypted dash-format codes are real AR content Dolphin could decrypt,
  but ArchiveFS has no verified decryptor and therefore cannot inspect what
  they decode to; they stay browse-only rather than risking unknown content.
- No Wii/Xbox 360 data exists in BSFree.
- The GUI Cheats & Mods page does not yet offer a BSFree apply control; install
  is available through `archivefs cheats source bsfree gamecube-apply` (and the
  GUI's BSFree browser now states the honest per-code capability).
