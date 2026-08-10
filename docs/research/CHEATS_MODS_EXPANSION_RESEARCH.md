# Cheats/Mods Expansion Research (EmuWiz / ArchiveFS)

**Scope:** research and architecture only. No code was changed to produce this document.
**Repo state audited:** `main` @ `50c76ce39131bc0b571172979d01df653edd548f`, clean tree.
**Claim tags used throughout:** `[FACT]` = documented fact, source cited · `[CODE]` = conclusion from reading this repo's source, file cited · `[INFERENCE]` = reasoned but not directly sourced · `[COMMUNITY]` = community wiki/forum knowledge, not authoritative.

---

## 1. Executive summary

The premise that this is a young, mostly-browse-only cheat feature is **wrong**. `crates/archivefs-core/src/patch_manager/` is a ~1.3MB, heavily-tested subsystem that already implements, end-to-end (provider → normalized record → classification → preview → confirm → atomic apply → journal → rollback), working installers for:

- **GameCube** — BSFree Archive and GameHacking.org, both installing into Dolphin `GameSettings/<ID>.ini` `[Gecko]`/`[ActionReplay]` sections, with a documented, deterministic AR→Gecko equivalence rule for the safe subset (`bsfree_gamecube.rs`).
- **Wii** — GameHacking.org, reusing the GameCube Dolphin install plan machinery.
- **PS2** — GameHacking.org, installing into PCSX2 `.pnach` files, identity-gated on a *verified* executable CRC (`pcsx2_identity.rs`, `pcsx2_pnach.rs`).
- **Xbox 360** — Xenia Canary's own TOML patch format, parsed and staged (`xenia_patch_document.rs`, `xenia_install_plan.rs`).
- **RetroArch** — a read-only *advisory* destination preview only (`retroarch.rs`); it deliberately does not write cheat content and is explicit that it is not shaped like the other adapters.
- A **shared transaction/rollback engine** (`shared_transaction.rs`, `cheat_installer.rs`, `cheat_rollback.rs`) used by every write-capable adapter, plus a **cross-provider duplicate/conflict model** already built for BSFree GameCube (`bsfree_gamecube.rs::analyze_bsfree_gamecube_duplicates`).

What is genuinely missing is breadth, not architecture: no PS1, N64, SNES/NES, Mega Drive, Game Boy family, 3DS, PSP, Saturn, Xbox(1), or PS3 install path exists; RetroArch's `.cht` format is never *generated*, only pointed at; and several platforms in the BSFree catalogue (PS2 CodeBreaker/GameShark/ARMax, everything 8/16-bit) have no adapter to receive them at all. The right next step is filling in more leaves of an already-correct tree, not building the tree.

## 2. Current EmuWiz capability map

Classification: **A** = implemented (shipped, tested) · **B** = partially implemented · **C** = browse-only by design · **D** = not implemented.

| Area | Status | Evidence |
|---|---|---|
| BSFree catalogue download/parse/browse (all systems) | A | `bsfree.rs` (`BsFreeCatalogue`, `download_bsfree_database`, `validate_bsfree_database`) |
| BSFree → GameCube classification (AR-native vs Gecko-equivalent vs unsupported vs malformed) | A | `bsfree_gamecube.rs::classify_bsfree_gamecube_cheat`, `BsFreeGameCubeCodeFormat` |
| BSFree → GameCube install (stage/preview/apply/rollback) | A | `bsfree_gamecube.rs::{build_bsfree_gamecube_install_preview, stage_bsfree_gamecube_install}` reusing `gamehacking_gamecube_install_plan.rs` |
| BSFree → any other platform install | D | module doc: "Every other BSFree system/device pairing... stays browse-only" |
| GameHacking.org GameCube provider (catalogue crawl, match, install) | A | `gamehacking_gamecube_provider.rs`, `gamehacking_gamecube_install_plan.rs` |
| GameHacking.org Wii provider (catalogue crawl, match, install via Dolphin) | A | `gamehacking_wii_provider.rs` — `WiiCodeFormat::{ActionReplay,Gecko,RawUnknown,Unsupported}`, `WiiCheatSafety` |
| GameHacking.org PS2 provider (catalogue crawl, match, PNACH parse) | A | `gamehacking_provider.rs::parse_gamehacking_pnach`, `Ps2GameHackingAdapter` |
| PS2 install (PNACH stage/preview/apply/rollback, CRC-gated identity) | A | `pcsx2_install_plan.rs`, `pcsx2_identity.rs`, `pcsx2_pnach.rs` |
| PS2 legacy-migration handling for pre-existing PNACH files | A | `pcsx2_install_plan.rs::{StagedPcsx2LegacyMigration, build_pcsx2_legacy_migration_preview}` |
| Dolphin upstream Gecko-code provider (official Dolphin codes repo) | A | `dolphin_gecko_provider.rs`, `dolphin_cheat_catalogue.rs` |
| Xenia Canary patch provider (TOML patches, install) | A | `xenia_provider.rs`, `xenia_patch_document.rs::parse_xenia_patch_toml`, `xenia_install_plan.rs` |
| RetroArch environment discovery (profiles, cores, playlists) | A | `emulator_environment/retroarch.rs` |
| RetroArch destination *preview* (where a cheat/patch file would go) | B | `retroarch.rs` — explicitly not an `EmulatorAdapter`, produces no installable content, no network fetch |
| RetroArch cheat-source fetch/cache (downloads community `.cht` bundles) | A | `cheat_sources.rs`, `cheat_cache_maintenance.rs` — fetches and caches upstream `.cht` files as-is |
| RetroArch `.cht` parsing/rendering | A | `cht_document.rs::{parse_cht_bytes, parse_cht_text, render_cht_file}` — parses/re-renders existing `.cht` files; does not *generate* cheats from another device's codes |
| RetroArch materialization of a cached snapshot into the live install | A | `retroarch_materialization.rs`, `retroarch_cheat_setup.rs` |
| Cross-provider duplicate/conflict detection | B | Fully built for BSFree GameCube (`analyze_bsfree_gamecube_duplicates`, 9 finding kinds incl. `CrossSectionCollision`, `SameLabelDifferentBody`); not yet generalized to other providers/platforms |
| Shared preview pipeline | A | `shared_preview.rs::{build_shared_preview, PreviewConflict, PreviewBlocker}` |
| Shared apply/journal/rollback pipeline | A | `shared_transaction.rs::{execute_shared_apply, execute_shared_rollback, SharedApplyJournal}`, `cheat_installer.rs`, `cheat_rollback.rs` |
| Cheat install/rollback run history & inspection | A | `cheat_history.rs::{discover_cheat_history, inspect_cheat_install_journal}` |
| Cheat source registry (9 built-in sources, priorities, per-platform overrides, health probing) | A | `cheat_source_registry/mod.rs`, `cheat_source_registry/health.rs` |
| GUI Cheat Sources page (view/edit registry, priorities, health) | A | `crates/archivefs-gui/src/cheat_sources_page.rs` |
| GUI Gamer View / Advanced View split with cheats reachable from both | A | `crates/archivefs-gui/src/main.rs` (`GuiMode::{GamerView, AdvancedView}`, `MainView::CheatsMods`, `show_gamer_view`) |
| PS1, N64, SNES, NES, Mega Drive/Genesis, Master System, Game Boy family, 3DS, PSP, Saturn, Xbox(1), Xbox 360 (beyond Xenia patches), PS3 install adapters | D | no matching module under `patch_manager/`; not in the 9-source registry (`cheat_source_registry/mod.rs`) |
| AR MAX / CodeBreaker / GameShark (PS2) decryption or conversion | D | no decryptor anywhere in `patch_manager`; `bsfree_gamecube.rs` explicitly calls out that ArchiveFS "has no verified decryptor" even for the GameCube case |
| RetroArch `.cht` *generation* from a device-format code (GameShark/AR/etc.) | D | `cht_document.rs` only parses/renders; no encoder from another IR exists |

**Housestyle observed (from `diagnostics/repair.rs`, generalized across `patch_manager`):** closed, enumerable action sets (no free-form command injection); nothing mutates without explicit confirmation; every finding/plan is re-validated against *live* state immediately before acting, never trusted from a stale scan; and a hard line between "genuinely unsupported" and "not yet verified" — the latter stays browse-only rather than being guessed at. This is the pattern the rest of this report reuses when proposing new work.

## 3. BSFree platform/format matrix

BSFree is a 2006 dump of GSCentral.org's cheat-code database, subsequently mirrored/re-packaged by the GameHacking.org/Kodewerx community and, more recently, converted to SQLite and republished on GitHub `[COMMUNITY]` (wiki.gamehacking.org/BSFree page; `github.com/andrewmackrodt/bsfree`). It is **not** an emulator-native format for any platform — it is a generic archive of cheat-device codes (GameShark/Action Replay/CodeBreaker-family) keyed by game + device, and its structure varies per platform/device rather than being one universal scheme.

EmuWiz's own device mapping is authoritative for how the codebase currently classifies BSFree devices — see `bsfree.rs::bsfree_device_mapping` / `bsfree_platform_mapping` `[CODE]`. Everything else below about the *devices themselves* is `[COMMUNITY]`/`[INFERENCE]` unless cited.

| Platform | Native cheat format / device family | Encrypted? | Conversion required for use | EmuWiz parses today? | Target emulator | Target's cheat format | Safe-install feasibility | Confidence |
|---|---|---|---|---|---|---|---|---|
| GameCube | Action Replay (GC), hex-pair `XXXXXXXX YYYYYYYY`, plaintext raw form or dash-encrypted verifier form | Sometimes (dash form is encrypted with a per-game seed) `[COMMUNITY]` | Only for a proven subset (§4) | Yes — `bsfree_gamecube.rs` classifies plaintext hex-pair codes | Dolphin | `.ini` `[ActionReplay]` / `[Gecko]` | High for the proven AR32-write subset; browse-only for the rest | High (repo evidence + Dolphin source-derived doc comments) |
| Wii | Action Replay-style hex-pair codes, plus native Gecko codes (Wii is Gecko's home platform) `[COMMUNITY]` | Same dash-encrypted verifier form exists `[COMMUNITY]` | Only for well-formed strict raw pairs | Partially — `gamehacking_wii_provider.rs` has `WiiCodeFormat`/`WiiCheatSafety`, but this is the GameHacking.org provider, not yet wired to BSFree | Dolphin | Same as GameCube | Similar to GameCube for the plaintext subset | Medium (BSFree-Wii path itself unverified in repo; GameHacking-Wii path is) |
| PS2 | GameShark/Action Replay v1/v2 (3 known encryption sub-types keyed by an `(M)` "must be on" code), CodeBreaker (its own, different encryption; multiple incompatible versions v1–v7), ARMax (its own scheme) `[COMMUNITY]` (GTPlanet/PS2-HOME community threads; corroborated by GameHacking.org's own "PS2 Code Converting" library page) | Yes, device-specific, non-interchangeable | Always, and non-trivially — different devices are not drop-in compatible even for identical effects | No — `bsfree_gamecube.rs` doc comment states PS2 CodeBreaker/GameShark/ARMax "stays browse-only: no existing adapter can represent those formats" | PCSX2 | `.pnach` (plaintext `patch=` lines) | Low without a verified per-device decryptor; PCSX2's own PNACH format is plaintext, so *if* decrypted correctly the destination is easy — the risk is entirely in decryption correctness | Medium (device existence/incompatibility is well corroborated; EmuWiz has zero decoder) |
| PS1 | GameShark/Action Replay hex-pair codes (PS1-era, distinct encryption from PS2 GameShark) `[COMMUNITY]` | Yes, device-specific | Always | No | DuckStation / PCSX2-style / RetroArch (Beetle PSX / PCSX ReARMed cores) | DuckStation `.cht` (ini-style, `Type = Gameshark` field observed) `[FACT]` (`chtdb/cheat-format.txt`, `duckstation/src/core/cheats.h`) | Low-medium; DuckStation's own format *names* a GameShark type, suggesting it has a documented decode path, but EmuWiz has none | Medium |
| Dreamcast | GameShark (Dreamcast) hex codes, seed-based decryption for encrypted AR-style codes `[COMMUNITY]` (search-derived summary of Flycast's `cheats.cpp`) | Yes for some codes | Yes | No | Flycast | Flycast's own cheat format (poke-based, plus AR-style decrypt path in `cheats.cpp`) `[COMMUNITY]` | Low; unverified format detail, no EmuWiz adapter | Low |
| Saturn | GameShark/Action Replay/Pro Action Replay hex codes `[COMMUNITY]` | Likely device-specific encryption, unverified | Yes | No | No mainstream actively-maintained standalone Saturn emulator has first-class cheat tooling comparable to the above; RetroArch cores (Beetle Saturn) exist | RetroArch `.cht` | Low | Low (weakest-researched platform in this matrix) |
| Xbox (1) | Community "Cheat Engine"/raw memory-address hacks rather than a single retail cheat-device format `[COMMUNITY]`; Xbox never had a mainstream GameShark/AR device family the way PS1/PS2/GC did | N/A / inconsistent | Not meaningfully standardizable | No | Xemu / xbox-emulators generally | No standard cheat file format documented | Low | Low |
| Xbox 360 | No retail cheat-device family either; "trainers"/mod tools instead | N/A | N/A | No (BSFree path); Xenia *does* have real coverage via its own project, unrelated to BSFree | Xenia (Canary) | Xenia's own TOML patch format `[CODE]` (`xenia_patch_document.rs`) — this is genuinely documented and already implemented in EmuWiz | High, but via a completely different, non-BSFree pipeline | High (repo evidence) |
| PSP | CWCheat (PSP's de facto standard, `_S`/`_G`/`_C0`/`_L` line format) `[FACT]` (search-derived; matches PPSSPP's own cheat.db) | No — CWCheat codes are plaintext poke instructions, not device-encrypted | Not for CWCheat-native codes; only needed if source is a different device format | No | PPSSPP | PPSSPP `cheat.db`/per-game `.ini`, itself CWCheat-formatted `[FACT]` | High *if* BSFree's PSP entries are already CWCheat text (unverified) — the target format is a near 1:1 match | Medium (format compatibility plausible, unverified whether BSFree's PSP archive uses CWCheat text) |
| Nintendo DS | Action Replay DS / CodeBreaker DS hex-pair codes, some encrypted `[COMMUNITY]` | Sometimes | Sometimes | No | melonDS / DeSmuME / RetroArch cores | No single dominant native "install cheats for me" format documented as clearly as CWCheat/PNACH | Low | Low |
| 3DS | Action Replay/Gateway/Luma3DS "cheat.txt" or "GatewayCheats"-style hex pointer codes `[COMMUNITY]` | Some encrypted (AR3DS), some plaintext (Luma cheats.txt) | Sometimes | No | Citra/Lime3DS/Azahar | Luma3DS `cheats.txt` (per-title-ID plaintext) `[COMMUNITY]` | Low-medium; format is plaintext but pointer/opcode semantics are complex (see §5's pointer-chasing caveat, same class of risk) | Low |
| N64 | GameShark N64 hex-pair codes | Some encrypted | Sometimes | No | RetroArch (Mupen64Plus/ParaLLEl cores) | RetroArch `.cht` | Low-medium (poke-based subset plausible; §5) | Low |
| SNES | Game Genie (letter-code, its own 6-char alphabet cipher) and Pro Action Replay hex codes — **two structurally different formats on one platform** `[COMMUNITY]` | Game Genie uses a letter-substitution cipher (deterministic, documented publicly by multiple community sources), PAR is closer to plain hex | Game Genie codes require decode; PAR less so | No | RetroArch (Snes9x/bsnes cores) | RetroArch `.cht` | Medium for a verified Game Genie decoder + PAR poke codes | Low-medium |
| NES | Game Genie (NES's own, different cipher from SNES's) `[COMMUNITY]` | Yes, letter-substitution cipher | Yes | No | RetroArch (Mesen/FCEUmm cores) | RetroArch `.cht` | Medium, same shape as SNES Game Genie | Low-medium |
| Mega Drive/Genesis | Game Genie (Genesis has its own cipher, again distinct from NES/SNES) + Pro Action Replay | Genie yes, PAR less so | Yes for Genie | No | RetroArch (Genesis Plus GX core) | RetroArch `.cht` | Medium | Low-medium |
| Master System | Pro Action Replay / GameGenie variants exist but far less standardized/documented than Genesis | Mixed | Mixed | No | RetroArch | RetroArch `.cht` | Low | Low |
| Game Boy | Game Genie (GB) and GameShark (GB) — again platform-specific ciphers, not shared with other Game Genie variants `[COMMUNITY]` | Yes | Yes | No | RetroArch (Gambatte/SameBoy cores) | RetroArch `.cht` | Low-medium | Low |
| GBC | Same family as Game Boy, some additional GameShark Color codes | Yes | Yes | No | RetroArch | RetroArch `.cht` | Low-medium | Low |
| GBA | GameShark Advance / Action Replay GBA / CodeBreaker GBA — three devices, each with distinct, non-interchangeable encoding `[COMMUNITY]` | Yes, device-specific | Yes, and device identification itself is ambiguous from code text alone in many cases | No | RetroArch (mGBA core) / mGBA standalone | RetroArch `.cht` / mGBA's own cheat format | Low | Low |

**Cross-cutting warning (repeated deliberately per the task's instruction):** GameShark, Action Replay, and CodeBreaker code text for the *same platform* are frequently visually similar (8 hex digits + 4/8 hex digits) but are **not** interchangeable — each device applies its own XOR/seed-based decryption keyed by an internal table and, for M-codes, a per-game seed `[COMMUNITY, high-confidence per multiple independent community sources]`. EmuWiz's own `bsfree_gamecube.rs` already encodes exactly this caution for GameCube (treating the dash-encrypted form as `Malformed`/unhandled rather than guessing). The same discipline must apply to every other platform in this table — none of the "No" rows above should be treated as safe to guess-convert.

## 4. GameCube AR/Gecko findings

This section is grounded first in EmuWiz's own code (`bsfree_gamecube.rs`), which already cites Dolphin's source behavior directly, then cross-checked against external sources.

**What EmuWiz already asserts, with its own source citations (`[CODE]`, `bsfree_gamecube.rs` lines ~10–45):**
- Dolphin's `ActionReplay.cpp` decodes an AR code's first word as `subtype:2 | type:3 | size:2 | gcaddr:25`; type 0/subtype 0/size 2 is a 32-bit write to `gcaddr | 0x80000000`.
- Dolphin's Gecko code handler (`docs/codehandler.s`) decodes `04XXXXXX` as code type 0/subtype 2 — a `stw` of the value to `0x80000000 + XXXXXX`.
- When `gcaddr < 0x01000000`, both engines write the identical value to the identical address from the identical bytes — the "conversion" is **byte-identity**, not a transformation. This is the only subset EmuWiz treats as `GeckoEquivalent`.
- Every other well-formed hex-pair AR command Dolphin's AR engine implements (16/8-bit writes, float writes, pointer writes, add codes, conditionals) is emitted **verbatim into `[ActionReplay]`**, never relabelled as Gecko — `ActionReplayNative`.
- Master codes, zero codes, and self-modifying codes are `Unsupported` — well-formed but refused at runtime by Dolphin itself.
- The base-31 dash-encrypted AR verifier format (`XXXX-XXXX-XXXXX`) is `Malformed` from EmuWiz's point of view: real, decryptable Action Replay content that Dolphin itself can decrypt, but for which "ArchiveFS has no verified decryptor," so it stays browse-only rather than being guessed at.

**External corroboration (`[FACT]`/`[COMMUNITY]`):**
- Dolphin's own wiki documents GameCube AR code types (`github.com/dolphin-emu/dolphin/wiki/GameCube-Action-Replay-Code-Types`) `[COMMUNITY — GitHub wiki, but maintained by the Dolphin project itself, so treated as near-authoritative]`, confirming the encrypted-verifier vs. decrypted-raw distinction and that the first line of an encrypted code carries game ID/region/checksum — consistent with why a decryptor needs per-game verification, which is exactly the missing piece EmuWiz flags.
- Community sources (AdituV's GCN AR format writeup, gc-forever forum decoder pseudocode) independently describe the same subtype/type/size bitfield layout EmuWiz's doc comment cites `[COMMUNITY]`.
- Wii AR/Gecko is a related but distinct case: Wii is Gecko's native platform (the Gecko OS project originated there), and GameHacking's own Wii provider in this repo (`WiiCodeFormat::{ActionReplay, Gecko, RawUnknown, Unsupported}`) already treats them as separate formats requiring the same "strict raw pairs only" discipline as GameCube (`WiiCheatSafety::Installable` requires "explicit format label and strict raw code lines") `[CODE]`.

**Known unsupported/ambiguous command types, confirmed both by EmuWiz's `Unsupported`/`Malformed` buckets and by general AR semantics:**
- Master/enabler codes (must be listed and active for other codes in the same set to function) — refused.
- Pointer-chasing codes (write to an address computed by first reading a pointer) — these exist in AR's instruction set but are **not** part of the `GeckoEquivalent` byte-identity subset; EmuWiz emits them verbatim into `[ActionReplay]` (native, unconverted) rather than attempting Gecko translation.
- Conditional (if/equal, if/not-equal, if/greater, if/less) multi-line codes — same treatment: native AR only, never reinterpreted as Gecko.
- Multi-line codes in general are handled at the *code* level (a code is a list of lines), not per-line, so a single non-write32 line anywhere in a code disqualifies the whole code from `GeckoEquivalent`.
- Region/version dependence of addresses is not solved by conversion at all — it's a game-identity problem. EmuWiz sidesteps it by keying the destination on "the selected archive's verified Dolphin Game ID," never BSFree's own metadata, and requiring user confirmation of the game match before any Apply (`bsfree_gamecube.rs` "Identity is the selected archive's, never BSFree's").

**Recommendation: (B) convert only a verified subset, matching what is already built.**
Justification: the byte-identity write32 subset is deterministic and lossless by construction (same opcode bytes execute identically in both engines per Dolphin's own decoder logic) — this is as close to provably safe as a code-conversion gets, so restricting to it is not overcautious, it's the actual safety boundary. Extending beyond it (pointer codes, conditionals, encrypted verifier codes) would require either a verified AR decryptor (for the dash form) or a second, non-trivial equivalence proof per AR command type against Gecko's instruction set (for pointer/conditional codes) — neither exists today, and guessing wrong here means writing to the wrong game memory address, which is squarely the kind of "wrong game triggered / memory corruption" risk the task asks to weight heavily. Leaving those permanently browse-only (D) until a specific, reviewed decryptor or equivalence proof lands is consistent with the project's existing "genuine refusal vs. stale/unverified finding" posture in `diagnostics/repair.rs`.

## 5. PS2/PNACH findings

**PNACH itself `[FACT]`** (pcsx2.net/docs/advanced/writing-patches/): plaintext, human-readable `patch=<type>,<place>,<address>,<size>,<value>` lines (e.g. `patch=1,EE,0032C220,word,000000FF`); files are named `<Serial>_<CRC>.pnach` and PCSX2 auto-loads the file whose CRC matches the currently running game's executable CRC. This means PNACH's "game identity" mechanism is a CRC match, not a serial-only match — a wrong or stale CRC silently loads the wrong patch (or none), which is exactly why EmuWiz's own `pcsx2_identity.rs` refuses to synthesize a CRC and only proceeds on a `Verified` state derived from `GameIdentityReport::verified_pcsx2_crc()` `[CODE]`.

**Source formats and their fitness for PNACH:**

| Source format | Encryption | Conversion to PNACH | Master codes | CRC/identity handling | Region/version risk | Safe subset today |
|---|---|---|---|---|---|---|
| Raw/plaintext PS2 codes already in `patch=` shape (e.g. GameHacking.org's own PNACH exports) | None | None needed — same format | N/A | Provider record must still be matched to the *locally verified* CRC, never trusted from the provider | Low if CRC match is exact | **Yes — already implemented** (`gamehacking_provider.rs::parse_gamehacking_pnach`, `pcsx2_install_plan.rs`) |
| GameShark/Action Replay v1/v2 (PS2) | Yes, 3 known sub-types keyed by an `(M)` seed code `[COMMUNITY]` | Requires a verified decryptor per sub-type; none exists in EmuWiz | Yes, common | N/A until decrypted | High — wrong seed = garbage address/value | No — correctly out of scope until a decryptor is verified |
| CodeBreaker (PS2, v1–v7) | Yes, its own distinct scheme, incompatible across CB versions `[COMMUNITY]` | Same problem, worse: even *which* CB version applies must be known | Yes | N/A until decrypted | High | No |
| ARMax | Yes, its own scheme `[COMMUNITY]` | Same | Unclear/less documented | N/A | High | No |

**Proposed safe subset:** exactly what is already built — accept only source records that are already plaintext PNACH `patch=` lines (or unambiguously convertible 1:1 syntax variants of the same plaintext scheme, e.g. differing only in comment style), gate installation strictly on a *verified* local executable CRC (never a CRC asserted by the provider), and never attempt GameShark/CodeBreaker/ARMax decryption without a reviewed, tested decoder for that specific device+version. This is not a new recommendation so much as a confirmation that the existing `gamehacking_provider.rs` → `pcsx2_install_plan.rs` path already draws the line in the right place, and that BSFree's PS2 entries (which are GameShark/CodeBreaker/ARMax-encoded per §3) are correctly excluded rather than an oversight.

## 6. RetroArch findings

**RetroArch does not have one universal cheat format that "solves" other platforms — this was explicitly checked, not assumed.** The `.cht` file is a flat key=value list (`cheats = N`, `cheat0_desc`, `cheat0_code`, `cheat0_enable`, …) where `cheat#_code` is itself an address+value poke string in one of RetroArch's own internal encodings (plain `address:value`, or the `+`-joined multi-part form seen in real `.cht` files) `[FACT]`, sourced from `cheat_manager.c` and `docs.libretro.com/guides/cheat-codes/`. Two mechanisms exist per RetroArch's own design: **"RetroArch Handled"** codes, where RetroArch itself pokes the core's exposed memory map directly, and **"Emulator Handled"** codes, which are passed to the core to interpret in whatever format that specific core's original cheat engine expected `[FACT]`. This means the *effective* format is core-and-platform-dependent: a poke that is correct for one core's memory map (address space, bank layout, endianness) is not portable to a different core for the same platform, let alone a different platform.

EmuWiz's own module doc for `retroarch.rs` independently reaches the same conclusion for a different reason: RetroArch has no single patch/cheat root, several purpose-tagged directories per install, and a core-selection ambiguity axis with no analogue in PCSX2 — which is exactly why `retroarch.rs` was deliberately built as a read-only advisory *destination* preview, not an `EmulatorAdapter`, and why it makes no network call and produces no installable content `[CODE]`.

**Implication for device-code translation (GameShark/AR → RetroArch `.cht`):** not safely automatable in general. A RetroArch poke's correctness depends on (a) which core, (b) that core's specific RAM layout/endianness for the platform, and (c) whether the address is core-relative or system-absolute — none of which BSFree/GameHacking metadata declares, and none of which EmuWiz currently resolves beyond "is exactly one installed core unambiguous for this file extension" (`retroarch.rs`'s own core-selection logic, which already refuses to guess when ambiguous).

**Where RetroArch realistically helps EmuWiz, and where it's riskier:**
- **Lower risk / realistic targets:** platforms where the *source* cheat is already a simple, unsigned RAM poke with a well-known, single dominant core and RetroArch's own libretro cheat database already exists for that system (NES/SNES/Genesis/Game Boy family via well-established RetroArch cheat DBs) — here EmuWiz's safest move is not "convert device codes" but "fetch/verify/install RetroArch's *own* existing `.cht` bundles for that platform," which is exactly the shape of the already-built `cheat_sources.rs`/`cht_document.rs`/`retroarch_materialization.rs` pipeline (fetch-and-materialize an upstream `.cht`, not synthesize one).
- **Higher risk:** any platform where the only available source is a device-encrypted format (GameShark/AR/CodeBreaker for GBA, N64, PS1, Dreamcast, Saturn) requiring both decryption *and* a core-specific address translation before it could become a `.cht` poke — two independent unverified steps stacked, which compounds risk rather than adding coverage cheaply.

**Conclusion for §12/Bottom line:** RetroArch's practical value to EmuWiz today is narrower than "universal cheat format" — its real, low-risk contribution is as a *distribution* target for already-native `.cht` content (which EmuWiz already fetches/parses/materializes), not as a *conversion* target for device-encrypted codes from other platforms.

## 7. Emulator adapter matrix

| Emulator | Cheat file format | Game identity mechanism | Expected location | Officially documented by project? | Rollback feasibility | Duplicate/conflict detection feasibility | EmuWiz status |
|---|---|---|---|---|---|---|---|
| Dolphin | `GameSettings/<GameID>.ini`, `[Gecko]`/`[ActionReplay]`/`[Gecko_Enabled]` sections | Dolphin Game ID (6-char, from disc header) | Dolphin user dir `GameSettings/` | Yes, project docs + wiki `[FACT/COMMUNITY]` | High — EmuWiz already journals+backs up via shared transaction pipeline `[CODE]` `dolphin_gecko_install_plan.rs` | High — already built, `analyze_bsfree_gamecube_duplicates` is a working reference implementation `[CODE]` | **Implemented** |
| PCSX2 | `.pnach`, `patch=` lines, CRC-named file | Executable CRC (+ serial in filename) | `<PCSX2>/patches/` | Yes, official docs `[FACT]` (pcsx2.net) | High — `StagedPcsx2Pnach` + shared transaction `[CODE]` | Partial — managed-block model exists (`pcsx2_pnach.rs::{extract_managed_blocks, merge_managed_pnach_cheats}`) but not generalized cross-provider like BSFree GameCube's finder | **Implemented** |
| RPCS3 | Multiple: its own patch YAML (`patch.yml`) via the built-in Cheat Search/patch manager, community "Artemis" codelists, and it separately supports `.cht`/`.pnach`/`.txt` per community docs `[COMMUNITY]` | Game title ID (`patch.yml` keyed by title ID + serial) `[COMMUNITY]` | RPCS3 config dir, `patch.yml`/patches folder | Partially — RPCS3 has its own wiki "Help:Game Patches" page (project-maintained) `[COMMUNITY]` | Unverified — no EmuWiz code exists | Unverified | **Not implemented** |
| Xenia (Canary) | TOML patch files, per-title | Title ID / hash, embedded in patch TOML | Xenia patches directory | Documented by the Xenia Canary project itself (canary-patches repo convention) `[CODE + COMMUNITY]` | High — `XeniaInstallPreview`/staged patch file + shared transaction `[CODE]` | Unverified/not yet generalized | **Implemented** |
| PPSSPP | CWCheat-style `.ini` per game ID (`_S`/`_G`/`_C0`/`_L` lines), also reads a bundled `cheat.db` | PSP game ID (e.g. `ULUS-10202`) | `PSP/Cheats/<ID>.ini` under memstick root | Yes, documented in-app and via community guides that mirror PPSSPP's own behavior `[FACT/COMMUNITY]` | Unverified — no EmuWiz code exists | Unverified | **Not implemented** |
| DuckStation | `.cht`, ini-style, per `SERIAL.cht` or `SERIAL-HASH.cht`, with a `Type = Gameshark`-style field per code | Game serial (+ optional content hash) | DuckStation cheats dir | Yes — DuckStation ships its own `chtdb` format spec (`cheat-format.txt`) `[FACT]` | Unverified | Unverified | **Not implemented** |
| Flycast | Its own `cheats.cpp`-driven system; supports GameShark-style codes with seed-based decryption for some, plus RetroArch-core delivery when run as a libretro core | Unclear from search alone; likely by game/track ID | Flycast cheats dir / via RetroArch when used as a core | Community-documented primarily (DeepWiki, forums); not a formal spec page found `[COMMUNITY]` | Unverified | Unverified | **Not implemented** |
| MAME | XML cheat files (post-0.127; old `cheat.dat` is obsolete and not convertible with any known public tool) `[FACT]` (docs.mamedev.org, mamecheat.co.uk forum) | Machine/game short name | `cheat.zip`/`cheat.7z` or extracted XML in MAME root | Yes, MAME's own debugger docs describe the cheat XML system `[FACT]` | Unverified | Unverified | **Not implemented** |
| RetroArch | `.cht`, key=value, per-core/per-content, plus core-specific "Emulator Handled" delegation | No single mechanism — depends on core/content path and, per EmuWiz's own analysis, has a genuine core-selection ambiguity axis | RetroArch `cheats/` (several purpose-tagged subdirectories) `[CODE]` `emulator_environment/retroarch.rs` | Officially documented (docs.libretro.com) `[FACT]`, though the on-disk `.cht` key=value grammar itself is not published as a formal spec — inferred from source + examples `[FACT + COMMUNITY]` | Unverified for generated content (EmuWiz never generates `.cht`); high for fetched/cached snapshots via existing cache pin/prune machinery `[CODE]` `cheat_cache_maintenance.rs` | Partial — `retroarch_inventory.rs::ArtifactConflictState`/`ArtifactDiagnostic` already flags artifact-level conflicts for discovered files | **Partially implemented** (discovery/preview/materialization of existing content; no generation) |

Cross-check against what EmuWiz already assumes: the Dolphin and PCSX2 rows above match `dolphin_local.rs`/`pcsx2_local.rs`'s own discovered directory/profile model exactly (both already parse and validate the real on-disk layout, not a guessed one) `[CODE]`. `emulator_environment/` currently only implements RetroArch discovery (`emulator_environment/mod.rs` doc comment: "Only one emulator (RetroArch...) is implemented. No generic `EmulatorEnvironmentAdapter` trait exists yet") — Dolphin/PCSX2/Xenia profile discovery instead lives directly inside their respective `patch_manager` modules (`dolphin_local.rs`, `pcsx2_local.rs`, `xenia_local.rs`), which is a naming/location detail worth knowing before adding a new emulator: there is no single place "emulator discovery" is guaranteed to live yet.

## 8. Duplicate/conflict model

**What already exists (`bsfree_gamecube.rs::analyze_bsfree_gamecube_duplicates`, `BsFreeDedupFindingKind`) — this is a strong, working reference implementation, not a gap:**

1. `DuplicateRecord` — exact same name + canonical code-body digest appearing more than once in one provider's own catalogue.
2. `DuplicateBody` — different labels, byte-identical canonical code body (source-level, pre-conversion).
3. `DuplicateNameConflict` — same display name, different body (exactly the "different cheat, same display name" case the task calls out as unsafe to conflate).
4. `ConvertedCollision` — two different selected cheats resolve to byte-identical *emulator output* after classification/conversion.
5. `AlreadyInstalled` / `AlreadyInstalledDifferentName` — output body already present at the destination, under the same or a different name.
6. `CrossSectionCollision` — a Gecko-equivalent body already exists as an Action Replay entry (or vice versa) — flagged as uncertain, requiring review, never auto-merged.
7. `SameLabelDifferentBody` — name collision with different body at the *destination* — never silently overwritten.
8. `NotInstallable` — the record isn't even a well-formed installable format.

This already implements, precisely, the task's requested separation: **exact duplicate** (1/5), **likely duplicate** (2/4/6), **different cheat with the same display name** (3/7), and it correctly refuses to treat display-name equality as identity anywhere in the list — every finding is keyed off the *canonical code body digest*, not the label, which is the right foundation (`cheat.canonical_digest`, `[CODE]`).

**Generalizing this cross-provider (BSFree vs GameHacking vs libretro/RetroArch DB vs local user file), the design should carry forward:**
- **Fingerprint key** = `(normalized platform, normalized canonical code body — address+size+value operations, not raw text) `, independent of source-provider vocabulary. This mirrors `canonical_digest` already computed per-cheat in `bsfree_gamecube.rs`.
- **Title/name** is *display metadata only*, never part of the identity key — matching the existing `SameLabelDifferentBody`/`DuplicateNameConflict` split.
- **Game identity** (serial/CRC/Game ID) and **region/version** must be part of the *destination* match, not the fingerprint itself — a byte-identical code body for two different game revisions is not "the same cheat," it's coincidence; EmuWiz's existing per-adapter identity types (`Pcsx2GameIdentity`, Dolphin's verified Game ID) are the right place to keep enforcing this, one layer up from body-fingerprinting.
- **Provider provenance** should be retained on every finding (already true — `cheat_upstream_id`/provider fields flow through `BsFreeDedupFinding`) so a human reviewing a `CrossSectionCollision` or `SameLabelDifferentBody` finding can see *which* two sources disagree.
- An **actual conflicting write** (same address+size, different value, same game/region) is a distinct, higher-severity class from all of the above and should never be silently resolved by priority order alone — it needs its own finding kind (closest existing analogue: `CrossSectionCollision`/`SameLabelDifferentBody`, but those are about label/section, not raw address collision across *different* code bodies). This is the one genuine gap: nothing in the current model detects "these two different, differently-labeled codes both write different values to the same address" unless they happen to also collide on name or canonical body. Recommend adding an explicit `ConflictingMemoryWrite` finding kind when generalizing, keyed on `(address, size)` equality with `value` inequality, scoped to same game+region.

**Why display-name-only dedup is unsafe (stated plainly, per task requirement):** two providers routinely give the same cheat different names ("Infinite Health" vs "999 HP"), and — more dangerously — the same name is reused across unrelated cheats ("Level Select", "Debug Mode") that do completely different memory writes. Deduping on name alone would either hide a genuinely different cheat behind a name collision (data loss / user confusion) or, worse, treat two unrelated writes as "the same" and silently pick one — exactly the "wrong cheat applied" risk category the task asks to weight most heavily. EmuWiz's own code already avoids this trap; any generalization must preserve that property.

## 9. Safety capability model

Proposed levels, with checkable (not vibes-based) criteria, extending the classification already implemented for GameCube (`BsFreeGameCubeCodeFormat`) to a provider-agnostic model:

| Level | Criteria (all must hold) |
|---|---|
| **VERIFIED INSTALLABLE** | (1) Source record parses under a strict grammar with zero ambiguous/placeholder tokens; (2) the parsed operation set maps to the target emulator's native format by **byte-identity or a proven-equivalent, previously-reviewed transform** (not a best-effort guess); (3) destination game identity is *verified* by the target emulator's own strongest identity signal (Dolphin Game ID, PCSX2 executable CRC, etc.) — never inferred from the provider's metadata; (4) no unresolved master/enabler-code dependency; (5) duplicate/conflict scan against the live destination has run and found no `SameLabelDifferentBody`/`ConflictingMemoryWrite`/unresolved `CrossSectionCollision`. Only this level may be Applied without an extra explicit "I understand" step beyond the normal preview/confirm flow already required everywhere. |
| **CONVERTIBLE VERIFIED SUBSET** | Same as above except (2) is relaxed to: the record matches a **named, reviewed, code-committed subset rule** (e.g. GameCube AR32-write → Gecko byte-identity) rather than the full native format — i.e., it's the `GeckoEquivalent` bucket, or its future analogues for other platforms. Still requires verified game identity and a clean dedup scan. Installable, but the specific subset rule it matched should be surfaced to the user (as `bsfree_gamecube.rs`'s `explanation()` already does). |
| **PREVIEW ONLY** | Parses cleanly and destination/identity is verified, but the operation set does **not** match a reviewed conversion rule yet (e.g. a new platform's write-poke codes before a subset rule has been written and tested) — shown to the user with exact projected output, but Apply is disabled until a rule is added in a reviewed change, matching `diagnostics/repair.rs`'s closed-action-set philosophy. |
| **BROWSE ONLY** | The record is well-formed and legible (a human/GameHacking-style renderer could display it) but requires a capability EmuWiz has explicitly decided not to build without further review — e.g. any encrypted device format with no verified decryptor (PS2 GameShark/CodeBreaker/ARMax, GC/Wii dash-encrypted AR, any 8/16-bit Game Genie cipher). This is a **permanent** state until a specific, reviewed decoder is added — not a "not implemented yet" placeholder that silently becomes installable later. |
| **UNSUPPORTED/MALFORMED** | Either (a) well-formed but contains a command class the *target emulator itself* refuses at runtime (master/zero/self-modifying codes — `Unsupported`), or (b) fails to parse under the strict grammar at all (placeholders, free text, truncated lines — `Malformed`). Never shown as installable under any circumstance; this mirrors the existing `BsFreeGameCubeCodeFormat::{Unsupported, Malformed}` split exactly. |

Parsing confidence, conversion determinism, and game-identity match are treated as **independent, all-required gates** — a high score on one never compensates for a failure on another (e.g. a perfectly-parsed code with unverified game identity is `PREVIEW ONLY` at best, never `VERIFIED INSTALLABLE`), which matches how `bsfree_gamecube.rs` already gates on classification *and* identity *and* dedup separately before allowing `stage_bsfree_gamecube_install`.

## 10. Recommended architecture

Evaluating the proposed pipeline stage-by-stage against what exists:

```
Provider record → Normalized cheat IR → Platform/code-family decoder → Verified operations → Emulator adapter → Preview → Transaction/apply → Rollback
```

| Stage | Exists today? | Where |
|---|---|---|
| Provider record | Yes, per-provider (BSFree, GameHacking×3, Dolphin upstream, Xenia) | `bsfree.rs::BsFreeCheat`, `gamehacking_*_provider.rs::*Cheat`, `xenia_patch_document.rs::XeniaPatch` |
| Normalized cheat IR | **Partially** — each provider has its own record type; there is no single cross-provider IR type. `BsFreeGameCubeCheat` is the closest thing to a normalized *classified* record, but it's GameCube+BSFree-specific | `bsfree_gamecube.rs::BsFreeGameCubeCheat` |
| Platform/code-family decoder | Yes, per platform, as a pure classification function | `bsfree_gamecube.rs::classify_bsfree_gamecube_cheat`, `gamehacking_wii_provider.rs` safety enums, `pcsx2_pnach.rs` parser |
| Verified operations | Yes, implicitly — the classification *is* the verified-operations gate (`BsFreeGameCubeCodeFormat::is_installable`) | same files |
| Emulator adapter | Yes for Dolphin/PCSX2/Xenia (write-capable); RetroArch is deliberately *not* this shape (§6) | `dolphin_gecko_install_plan.rs`, `pcsx2_install_plan.rs`, `xenia_install_plan.rs`; `adapter.rs::EmulatorAdapter` trait |
| Preview | Yes, shared across all write-capable adapters | `shared_preview.rs::build_shared_preview` |
| Transaction/apply | Yes, shared, with journaling and backups | `shared_transaction.rs::execute_shared_apply`, `cheat_installer.rs::execute_cheat_install_run` |
| Rollback | Yes, shared | `shared_transaction.rs::execute_shared_rollback`, `cheat_rollback.rs::execute_cheat_rollback_run` |

**Verdict: the pipeline already exists and should be extended, not replaced.** The one real gap is the "Normalized cheat IR" stage: today, normalization happens *inside* each platform-specific classifier rather than as a distinct, reusable type shared across providers for the same platform (e.g. BSFree-GameCube and GameHacking-GameCube each classify independently rather than both feeding a shared `GameCubeCheatIr`). This is the same shape of decision the project has already made deliberately once before (RetroArch's module doc explicitly declines to force RetroArch through the `EmulatorAdapter` trait because it doesn't fit, rather than weakening the trait) — so the right move for the IR gap is **not** to retrofit a single cross-platform IR type immediately, but to introduce a **per-platform** normalized IR (e.g. `GameCubeCheatIr`, `Ps2CheatIr`) that both BSFree and GameHacking (and eventually a third GameCube source) feed into, extending the existing per-platform decoder pattern rather than inventing a new abstraction layer speculatively. This directly enables §8's generalized duplicate/conflict model, since a shared per-platform IR with a canonical-digest field is what makes cross-provider fingerprinting (§7/§8) possible without each provider reimplementing it.

## 11. P0/P1/P2/P3 implementation roadmap

**P0 — high value / low risk (extend proven patterns to adjacent, already-scaffolded ground):**
- Wire BSFree's Wii entries through the same classify→dedup→install pattern already proven for BSFree GameCube, reusing `gamehacking_wii_provider.rs`'s `WiiCodeFormat`/`WiiCheatSafety` as the target shape. Both encoder (Dolphin `.ini`) and safety-classification precedent already exist; this is filling in a parallel leaf, not new architecture.
- Generalize `analyze_bsfree_gamecube_duplicates` into a shared, platform-parameterized dedup module (per §8/§10) so PS2/Wii installs get the same duplicate/conflict protection GameCube already has, and add the `ConflictingMemoryWrite` finding kind identified in §8.
- Extend BSFree's PS2 coverage to its *already-plaintext* records only (if any exist in the BSFree dump alongside the encrypted-device entries) — same-format, zero-decryption-risk PNACH entries, following the existing GameHacking.org PS2 pattern.

**P1 — worthwhile (real coverage gain, moderate but bounded new work):**
- PPSSPP CWCheat install adapter: PSP is one of the best-fit remaining platforms because the target format (CWCheat `.ini`) is plaintext and, per §3, plausibly already matches BSFree/community source text closely — lower conversion risk than any encrypted-device platform. Requires: PPSSPP profile discovery (new `patch_manager`/`emulator_environment` module), CWCheat `.ini` parser/writer, shared-transaction wiring. No decryption needed if source codes are already CWCheat-format.
- DuckStation `.cht` install adapter for PS1: DuckStation publishes its own format spec (`chtdb/cheat-format.txt`), which is a real advantage — build against a documented target format rather than reverse-engineering. Source-side PS1 GameShark decryption remains out of scope (P2/P3) even after the adapter exists; ship it initially as PREVIEW ONLY / manual-paste for encrypted source codes, VERIFIED INSTALLABLE only for any already-plaintext PS1 records found in a provider.
- A verified, reviewed decryptor for the GameCube/Wii dash-encrypted AR verifier format, since Dolphin's own AR engine already proves the target decode is well-defined — this would upgrade a currently-`Malformed`-bucketed slice of BSFree's *existing* GameCube/Wii catalogue straight into `VERIFIED INSTALLABLE`/`CONVERTIBLE VERIFIED SUBSET` without needing any new platform support at all, likely the single highest BSFree-coverage-per-effort item on this list (see Bottom Line).

**P2 — difficult/specialized (real value, high effort or inherently harder to verify):**
- PS2 GameShark/AR v1/v2 and CodeBreaker decryptors. Multiple incompatible encryption sub-types/versions per device (§3/§5), each needing independent verification against known-good code/decode pairs before being trusted — a correctness-heavy, slow-to-verify effort, but PS2's BSFree/GameHacking corpus is large enough that success here is high-coverage.
- RPCS3 `patch.yml` adapter — real, project-documented format exists, but RPCS3's own patch identity/versioning model is more complex (per-title, per-game-version patch groups) and less precedented in this codebase than the CRC/Game-ID model everything else uses; needs its own discovery/identity module from scratch.
- N64/SNES/NES/Genesis Game Genie decoders — each platform's cipher is distinct (§3), so this is N separate small-but-fiddly decoders, not one reusable piece of work, and the payoff per platform (RetroArch `.cht` poke) is lower-value than PS2/PS1/PSP because RetroArch's own libretro cheat databases already cover much of this ground for free (see §6/Bottom Line) — building EmuWiz-side decoders here is largely redundant with existing "fetch RetroArch's own DB" coverage.

**P3 — research/archive-only (leave browse-only for the foreseeable future):**
- Saturn, Master System, Xbox(1) — thin/unclear device-format documentation, low corroborated confidence in this research, no dominant modern emulator target with a clearly documented cheat format to build against.
- 3DS/DS pointer-chasing cheat formats — the pointer-chasing/opcode-decode risk class flagged in §4 for GameCube applies with *more* severity here (deeper pointer chains, less mature community documentation of the exact opcode semantics found in this research pass); treat as research-only until a much deeper platform-specific investigation is done.
- MAME — explicitly community-maintained cheat XML with **no known automated migration path from the legacy `cheat.dat` format** (§6/§7, `mamecheat.co.uk` forum corroboration), and MAME's per-machine (not per-game) identity model doesn't fit this codebase's per-title patterns at all; not worth pursuing without a dedicated design pass.

## 12. Unknowns requiring further research

- Whether BSFree's actual PSP, PS1, N64, SNES/NES/Genesis/Game Boy-family archive rows are stored as plaintext CWCheat/poke text or as device-encrypted hex — this determines whether §11's P1/P2 items are "wire up a parser" or "build a decryptor" and was **not** verifiable from outside the actual BSFree SQLite dump within this research pass; the next step should be loading a real BSFree export locally (the repo already has `import_local_bsfree_database`) and inspecting representative rows per platform.
- The exact grammar/versioning of PS2 GameShark v1/v2's three encryption sub-types and CodeBreaker v1–v7's schemes — community sources agree these differ but no primary specification was found and verified in this pass; needed before any P2 decryptor work starts.
- Whether RPCS3's `patch.yml` format is stable/versioned enough (and documented enough by the RPCS3 project itself, vs. community "Artemis" codelist convention) to build a safety model as strict as this project's existing ones require.
- Flycast's exact cheat file format/location when run standalone (not as a RetroArch core) — search results only reached `cheats.cpp`-level and DeepWiki summaries, not a citable format spec.
- Whether GameHacking.org's Wii catalogue's `Gecko`-labeled entries (as opposed to `ActionReplay`-labeled) are already native Gecko text that could skip classification-as-AR entirely — worth confirming against real fetched Wii catalogue data, not just the type definitions.
- Full confirmation of DuckStation's and PPSSPP's officially-supported (vs. purely community-convention) status for *externally written* cheat files — both formats are documented, but "the emulator reads a file we write" vs. "the emulator project explicitly supports third-party tools writing this file" is a distinction this research pass did not fully resolve for either.

## 13. Sources/references

**Repository (this clone, `main`@`50c76ce`):**
- `crates/archivefs-core/src/patch_manager/mod.rs`
- `crates/archivefs-core/src/patch_manager/bsfree.rs`, `bsfree_gamecube.rs`
- `crates/archivefs-core/src/patch_manager/gamehacking_provider.rs`, `gamehacking_gamecube_provider.rs`, `gamehacking_gamecube_install_plan.rs`, `gamehacking_wii_provider.rs`, `gamehacking_shared.rs`, `gamehacking_catalogue.rs`
- `crates/archivefs-core/src/patch_manager/pcsx2.rs`, `pcsx2_identity.rs`, `pcsx2_install_plan.rs`, `pcsx2_local.rs`, `pcsx2_pnach.rs`, `pcsx2_provider.rs`
- `crates/archivefs-core/src/patch_manager/dolphin_local.rs`, `dolphin_gecko_provider.rs`, `dolphin_gecko_install_plan.rs`, `dolphin_cheat_catalogue.rs`, `gecko_document.rs`
- `crates/archivefs-core/src/patch_manager/xenia_local.rs`, `xenia_provider.rs`, `xenia_patch_document.rs`, `xenia_install_plan.rs`
- `crates/archivefs-core/src/patch_manager/retroarch.rs`, `retroarch_inventory.rs`, `retroarch_cheat_library.rs`, `retroarch_cheat_setup.rs`, `retroarch_materialization.rs`, `cht_document.rs`, `cheat_sources.rs`, `cheat_cache_maintenance.rs`, `cheat_cache_lock.rs`
- `crates/archivefs-core/src/patch_manager/shared_preview.rs`, `shared_transaction.rs`, `cheat_installer.rs`, `cheat_rollback.rs`, `cheat_rollback_result.rs`, `cheat_history.rs`, `cheat_install_plan.rs`, `cheat_install_result.rs`, `cheat_candidates.rs`, `cheat_catalogue.rs`, `cheat_coverage.rs`, `cheat_source_registry/mod.rs`, `cheat_source_registry/health.rs`, `adapter.rs`, `matching.rs`, `import_safety.rs`, `destination_safety.rs`
- `crates/archivefs-core/src/diagnostics/repair.rs`, `crates/archivefs-core/src/emulator_environment/mod.rs`, `crates/archivefs-core/src/emulator_environment/retroarch.rs`
- `crates/archivefs-gui/src/cheat_sources_page.rs`, `crates/archivefs-gui/src/main.rs`

**External — DOCUMENTED FACT (project docs / source):**
- [Writing Patches — PCSX2](https://pcsx2.net/docs/advanced/writing-patches/) — official PNACH format and CRC-naming convention.
- [Cheat/Rumble Codes — Libretro Docs](https://docs.libretro.com/guides/cheat-codes/) — RetroArch cheat mechanism (Emulator Handled vs RetroArch Handled).
- [RetroArch `cheat_manager.c`](https://github.com/libretro/RetroArch/blob/master/cheat_manager.c) — `.cht` key=value structure, in-source.
- [Cheat Debugger Commands — MAME Documentation](https://docs.mamedev.org/debugger/cheats.html) — MAME's own cheat system docs.
- [`chtdb/cheat-format.txt` — DuckStation](https://github.com/duckstation/chtdb/blob/master/cheat-format.txt) and [`duckstation/src/core/cheats.h`](https://github.com/stenzek/duckstation/blob/master/src/core/cheats.h) — DuckStation's own format spec.
- [`rpcs3/rpcs3qt/cheat_manager.cpp`](https://github.com/RPCS3/rpcs3/blob/master/rpcs3/rpcs3qt/cheat_manager.cpp) — RPCS3 cheat manager source (not deeply read in this pass; flagged for follow-up).
- [`flycast/core/cheats.cpp`](https://github.com/flyinghead/flycast/blob/master/core/cheats.cpp) — Flycast cheat engine source (not deeply read in this pass; flagged for follow-up).

**External — CONCLUSION FROM SOURCE CODE (cited inside this repo's own comments, re-verified in principle against Dolphin's public wiki):**
- [GameCube Action Replay Code Types — dolphin-emu wiki](https://github.com/dolphin-emu/dolphin/wiki/GameCube-Action-Replay-Code-Types)

**External — UNCERTAIN/COMMUNITY KNOWLEDGE (forums, community wikis, third-party writeups — explicitly not authoritative):**
- [BSFree — wiki.gamehacking.org](https://wiki.gamehacking.org/BSFree) and [`andrewmackrodt/bsfree` — GitHub](https://github.com/andrewmackrodt/bsfree) — BSFree/GSCentral.org origin and 2006 dump history.
- [GCN ActionReplay code format — AdituV](https://adituv.wordpress.com/articles/gcn-ar-codes/); [Action Replay decoder pseudocode — gc-forever forum](https://www.gc-forever.com/forums/viewtopic.php?t=5112).
- [GameShark vs Code Breaker Cheat Devices — GGWP Academy](https://ggwpacademy.com/gameshark-vs-code-breaker-cheat-devices-key-differences/); [Somebody tell me about Gameshark, Codebreaker and Action Replay — GTPlanet](https://www.gtplanet.net/forum/threads/somebody-tell-me-about-gameshark-codebreaker-and-action-replay.36012/); [PS2 Code Converting — GameHacking.org library](https://gamehacking.org/library/112).
- PPSSPP CWCheat format and file location — multiple community guides (almarsguides.com, GameFAQs) cross-checked against each other; no single PPSSPP-project-authored spec page was located in this pass.
- RPCS3 cheat formats/workflow — psx-place.com, specifydev.uog.edu, fearlessrevolution.com community guides.
- Flycast cheat behavior — DeepWiki-generated summary and GitHub issue discussion, not a first-party Flycast doc page.
- MAME `cheat.dat`→XML non-convertibility — mamecheat.co.uk community forum threads.

---

## Bottom line

**What can we safely build next?** Fill in the leaves of the architecture that already exists, using patterns already proven in this codebase: (1) generalize the BSFree GameCube duplicate/conflict analyzer to a shared, platform-parameterized module and wire BSFree's Wii records through the existing GameHacking-Wii safety classification; (2) build a verified GameCube/Wii dash-encrypted Action Replay decryptor, since Dolphin's own AR engine proves the target decode is well-defined — this is bounded, well-scoped work with a large payoff (see below); (3) a PPSSPP CWCheat adapter, since PSP's target format is plaintext and likely close to source-format already.

**What should we NOT build yet?** Any conversion path that starts from an *encrypted, device-specific* code format without a reviewed, tested decryptor for that exact device/version — PS2 GameShark/AR/CodeBreaker/ARMax, Game Genie ciphers per-platform, 3DS/DS pointer-opcode cheats. Also do not build a "universal RetroArch cheat generator" — §6 shows this isn't a real universal format, so a generic translator would either be unsafe or need to be N platform-specific translators anyway, which is better done as N separate, smaller, reviewed pieces of work than one speculative abstraction.

**Which single piece of work unlocks the largest amount of BSFree coverage?** A verified GameCube/Wii dash-encrypted (`XXXX-XXXX-XXXXX`) Action Replay decryptor. It requires no new platform support, no new emulator adapter, and no new install pipeline — it only upgrades records that today sit in the already-`Malformed` bucket of an already-working GameCube/Wii pipeline into `VERIFIED INSTALLABLE`/`CONVERTIBLE VERIFIED SUBSET`. Everything downstream (classification, dedup, Dolphin install, preview/apply/rollback) already exists and would apply to the newly-decrypted codes unchanged.

**Is AR→Gecko conversion worth implementing, and at what safety level?** It's already implemented, correctly, at the right safety level: `CONVERTIBLE VERIFIED SUBSET` for the byte-identity write32 case (deterministic, provably lossless per Dolphin's own decoder logic), `VERIFIED INSTALLABLE`-as-native-AR (not "converted to Gecko") for the rest of the well-formed command set, and permanently `BROWSE ONLY`/`Malformed` for anything master/self-modifying/encrypted until a decryptor exists. No architectural change is needed here — only extending the encrypted-code decryptor as above.

**Is PS2→PNACH conversion worth implementing, and at what safety level?** Yes for the case that already works — plaintext-to-plaintext, CRC-identity-gated, at `VERIFIED INSTALLABLE`. Not yet worth it for GameShark/AR/CodeBreaker/ARMax source formats — those require independently-verified decryptors per device/version before they could even reach `PREVIEW ONLY`, and getting the decryption wrong on PS2 risks writing garbage values into a running game's memory via a CRC-matched, auto-loaded file, which is a higher blast radius than a browse-only miss.

**Does RetroArch meaningfully expand coverage, or is its value narrower than it first appears?** Narrower than it first appears. It is not a universal cheat format — it's a per-core, per-content poke format whose correctness depends on the specific core's memory layout, and EmuWiz's own `retroarch.rs` already reaches this conclusion independently. Its real, low-risk value today is as a *distribution* target for already-native `.cht` bundles (which EmuWiz already fetches, parses, caches, and materializes) — not as a conversion target for GameShark/AR-style device codes from other platforms, which would require solving both decryption and core-specific address translation before producing anything trustworthy.
