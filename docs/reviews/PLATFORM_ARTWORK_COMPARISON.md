# Platform artwork comparison

Audit of the platform artwork held in three places: this branch
(`feature/platform-artwork-completion`), the committed state of `main`, and the
**uncommitted working tree** of the protected main worktree at
`/home/davedap/archivefs`.

Nothing in the protected worktree was modified, staged, moved or copied to
produce this report. Every figure below comes from reading those files and from
`archivefs platform-artwork import-folder --dry-run`, pointed at a throwaway
`--root`, which wrote nothing.

**This report does not choose winners.** Where two images claim one platform,
both are described and the decision is left open — see
[Decisions needed](#decisions-that-need-a-human).

---

## 1. What is where

| Location | PNG | SVG | Notes |
|---|---:|---:|---|
| `main` committed | 17 | 10 | The 10 SVGs are the closed category fallback set |
| Protected main worktree | 67 | 10 | 17 tracked PNG (12 of them modified) + **50 new untracked** |
| This branch | 17 | 10 | Same images as `main`, three **renamed** |

The branch adds **no new images**. Its three asset changes are pure renames,
content-identical (`R100`):

| Old name | New name | Canonical ID |
|---|---|---|
| `playstation.png` | `psx.png` | `PSX` |
| `playstation2.png` | `ps2.png` | `PS2` |
| `playstation3.png` | `ps3.png` | `PS3` |

So the branch is **wiring, not artwork**: a 1,066-line
`archivefs-core/src/platform_artwork.rs`, a `platform-artwork` CLI, a GUI
artwork manager, and the rename that makes three filenames match their canonical
IDs.

The 50 new images live **only** in the protected worktree. They are not on any
branch and would be lost by a `git clean` in that worktree.

---

## 2. The naming rule

Artwork resolves by exact filename stem, where the stem is the canonical
platform ID lowercased with non-alphanumerics removed:

```
"Acorn Archimedes" -> acornarchimedes.png
"PSX"              -> psx.png
"TurboGrafx-16"    -> turbografx16.png
```

There is no substring or fuzzy matching, by design. A filename that is not an
exact stem loads nothing at all — it is inert, not approximate.

---

## 3. Coverage against the canonical registry

74 canonical platforms. **31 have artwork; 43 do not.**

Recognised today (31): `acornarchimedes`, `amiga`, `amigacd32`, `amstradcpc`,
`arcade`, `atarijaguar`, `atarilynx`, `dreamcast`, `gameboy`, `gameboycolor`,
`gamecube`, `gamegear`, `megadrive`, `n64`, `neogeo`, `neogeopocket`, `nes`,
`psp`, `psx`, `saturn`, `sega32x`, `sharpx68000`, `snes`, `switch`, `vic20`,
`virtualboy`, `wii`, `wiiu`, `wonderswancolor`, `xbox`, `xbox360`.

### Canonical platforms with no artwork (43)

`3DO`, `Acorn Electron`, `Apple II`, `Atari 8-bit`, `Atari2600`, `Atari5200`,
`Atari7800`, `AtariST`, `BBC Micro`, `ColecoVision`, `Commodore 128`,
`Commodore 64`, `Commodore CDTV`, `DOS`, `FM Towns`, `Game Boy Advance`,
`Intellivision`, `MSX`, `MSX2`, `Macintosh`, `MasterSystem`, `NEC PC-8801`,
`NEC PC-9801`, `NGage`, `Neo Geo CD`, `Neo Geo Pocket Color`, `NeoGeo64`,
`Nintendo 3DS`, `Nintendo DS`, `PC`, `PC Engine`, `PC Engine CD`, `PC-98`,
`PS2`, `PS3`, `Philips CD-i`, `PlayStation Vita`, `ScummVM`, `Sega CD`,
`TurboGrafx-16`, `Vectrex`, `WonderSwan`, `ZX Spectrum`.

`PS2` and `PS3` appear here only because the protected worktree still holds the
pre-rename names; **this branch's rename fixes both**, taking the real gap to 41.

Many of the remaining 41 have a candidate image sitting in the worktree under a
name that does not load — see the next two sections. Correcting those names
would close roughly two thirds of the gap without drawing anything new.

---

## 4. Misspelled filenames

Nine files are one or two letters away from a canonical stem. Each currently
loads **nothing**; renaming makes each load immediately. High confidence.

| File in worktree | Intended stem | Canonical platform |
|---|---|---|
| `atrai7800.png` | `atari7800.png` | Atari7800 |
| `atria5200.png` | `atari5200.png` | Atari5200 |
| `atrii2600.png` | `atari2600.png` | Atari2600 |
| `commidore64.png` | `commodore64.png` | Commodore 64 |
| `dreamcatst.png` | `dreamcast.png` | Dreamcast |
| `philpscdi.png` | `philipscdi.png` | Philips CD-i |
| `supergratx.png` | `supergrafx` → alias | PC Engine |
| `turbogratx16.png` | `turbografx16.png` | TurboGrafx-16 |
| `turbogratxcd.png` | `turbografxcd` → alias | PC Engine CD |

`dreamcatst.png` is a special case: it is the **only** one of the 50 new images
with an alpha channel, and a correctly-named `dreamcast.png` already exists.
See [Decisions](#decisions-that-need-a-human).

---

## 5. Valid platforms under a non-canonical name

These name a real platform but not by its stem, so they are inert. Renaming is
mechanical, but two collide with artwork that already exists.

| File in worktree | Canonical platform | Correct stem | Collides? |
|---|---|---|---|
| `3DO Interactive Multiplayer.png` | 3DO | `3do.png` | no |
| `Archimedes.png` | Acorn Archimedes | `acornarchimedes.png` | **yes** — already present |
| `Atari ST.png` | AtariST | `atarist.png` | no |
| `ColecoVision.png` | ColecoVision | `colecovision.png` | no (case only) |
| `apple2.png` | Apple II | `appleii.png` | no |
| `bbcmodelb.png` | BBC Micro | `bbcmicro.png` | no |
| `electron.png` | Acorn Electron | `acornelectron.png` | no |
| `gba.png` | Game Boy Advance | `gameboyadvance.png` | no |
| `vita.png` | PlayStation Vita | `playstationvita.png` | no |

Renaming these eight (all but `Archimedes.png`) closes eight of the 41 gaps
outright.

---

## 6. Orphans — no canonical platform

These map to nothing in the registry. They load nothing today and renaming
alone cannot fix them; each needs either a registry addition or removal.

| File | Assessment |
|---|---|
| `psx2.png`, `psx3.png`, `psx4.png`, `psx5.png` | The canonical stems are `ps2`/`ps3`. `psx4`/`psx5` have no platform at all (no PS4/PS5 in the registry). |
| `4448b039-69a6-4690-a61f-dfc5393c3069.png` | A bare UUID. Almost certainly an accidental export artefact. |
| `3ds.png` | Platform exists as `Nintendo 3DS` → stem `nintendo3ds.png`. Renameable. |
| `neogeocolor.png` | Closest is `Neo Geo Pocket Color` → `neogeopocketcolor.png`. Ambiguous: may be intended as Neo Geo CD. |
| `scumvm.png` | Platform is `ScummVM` → `scummvm.png`. Misspelling **and** orphan. Renameable. |
| `spectrum128k.png` | Platform is `ZX Spectrum` → `zxspectrum.png`. The "128K" model distinction does not exist in the registry. |
| `pcfx.png` | No PC-FX in the registry. |
| `xboxone.png` | No Xbox One in the registry. |
| `pcbox.png` | Unclear — possibly "PC box art" or a typo for PC Engine. |
| `dragon.png` | Likely Dragon 32/64. Not in the registry. |
| `tandycomputer.png` | Likely TRS-80. Not in the registry. |
| `vc4000.png` | Interton VC 4000. Not in the registry. |

`3ds.png`, `scumvm.png` and `spectrum128k.png` are recoverable by rename and are
listed here only because the tool reports them as unrecognised as-is.

---

## 7. Exact overlaps

### 7.1 `psx.png` — a genuine collision

This is the one case where the branch and the protected worktree both hold a
file of the **same name with different content**.

| Source | SHA-256 (12) | Dimensions | Mode | Alpha | Bytes |
|---|---|---|---|---|---|
| Branch (renamed `playstation.png`) | `d53580a7f36b` | 1024×1024 | RGBA | **yes** | 1,575,183 |
| Protected worktree (untracked) | `8b7a940c9853` | 1024×1024 | RGB | no | 889,127 |

**This blocks a naive merge.** Bringing this branch into the main worktree would
have git create a tracked `psx.png` exactly where an untracked `psx.png` already
sits. Git will refuse ("untracked working tree files would be overwritten") or,
if forced, destroy a protected file. This must be settled *before* the branch
lands. See [Decisions](#decisions-that-need-a-human).

### 7.2 Duplicate claims on one platform

| Platform | Files claiming it |
|---|---|
| Acorn Archimedes | `acornarchimedes.png` (loads) + `Archimedes.png` (inert) |
| Dreamcast | `dreamcast.png` (loads, modified) + `dreamcatst.png` (inert, **has alpha**) |
| PSX | branch `psx.png` + worktree `psx.png` — see 7.1 |

### 7.3 Files unique to each side

- **Unique to the protected worktree:** all 50 new PNGs, plus the 12 modified
  tracked PNGs whose working-tree content differs from what is committed.
- **Unique to the branch:** `ps2.png` and `ps3.png` (names only — content is
  identical to `main`'s `playstation2.png` / `playstation3.png`).

---

## 8. Transparency — a consistent divergence

| Group | Count | Alpha | Mode |
|---|---:|---|---|
| Committed, untouched | 5 | **5 of 5** | RGBA |
| Committed, modified in worktree | 12 | 0 of 12 | RGB |
| New in worktree | 50 | 1 of 50 (`dreamcatst.png`) | 49 RGB, 1 RGBA |

Every image is 1024×1024, so dimensions are consistent throughout.

The original bundled style is **transparent RGBA**. The incoming batch is
**opaque RGB** — and it also *replaces* twelve images that previously had
transparency. Platform artwork is drawn as a texture centred in a row or shelf
slot over the panel background, so an opaque square renders as a visible tile
rather than a shape floating on the background, and it will not adapt between
light and dark themes.

This is a visual-design judgement, not a correctness one, and it is the single
largest open question in this audit. It is not something this review can settle.

---

## 9. Runtime behaviour (reviewed, no defects found)

- **Resolution** tries exact canonical ID, then case-insensitive display name,
  then the alias table — so both canonical IDs and aliases resolve.
- **Fallback chain**: per-game custom PNG → exact platform PNG (custom, then
  bundled) → category PNG → painted vector glyph. The last step always draws,
  so a missing image can never leave an empty slot.
- **Invalid images fail closed**: PNG extension enforced, file-size ceiling,
  decoder width/height/allocation limits, zero dimensions refused, symlinks
  rejected. Every failure returns a `Result` and falls through to the glyph.
- **Not repeatedly decoded**: entries are keyed by a file fingerprint, and a
  failed decode is cached as a negative result rather than retried each frame.
- **Cache bound**: `PlatformArtworkCache` has *no* eviction. It is bounded in
  practice by the number of artwork files present (≤74 platforms + 7
  categories), which is fine for platform artwork. Per-*game* artwork uses the
  same cache and is bounded only by how many game images the user has placed on
  disk — unlike the RomM cover cache, which has a hard 256-entry LRU bound.
  Worth noting, not currently a defect.
- **Game covers stay separate**: a RomM cover is drawn only when ready;
  otherwise the row falls back to platform artwork. The two never stack.
- **No network**: nothing in the platform artwork path opens a socket. The CLI
  states this explicitly and the import path is local-file only.

---

## 10. Decisions that need a human

1. **`psx.png` collision.** Ship the branch's transparent 1.5 MB RGBA image, or
   the worktree's opaque 889 KB RGB one? Nothing can merge safely until this is
   answered.
2. **Transparency direction.** Accept the opaque RGB batch as the new house
   style, or require alpha to match the five originals? This also decides
   whether the twelve modified images are an improvement or a regression.
3. **`dreamcatst.png`.** It is misspelled, but it is also the only new image
   with alpha, and a correctly-named `dreamcast.png` already exists. Rename it
   over the existing one, or discard it?
4. **`Archimedes.png`.** Duplicate of an existing, working `acornarchimedes.png`.
   Which image wins?
5. **Registry additions.** Should PC-FX, Xbox One, Dragon, TRS-80, VC 4000 and
   PS4/PS5 become canonical platforms? Until then their images cannot load.
6. **`4448b039-…png`.** Confirm this is an accident and can be dropped.
7. **`pcbox.png`, `neogeocolor.png`.** Ambiguous intent; the author should say
   what platform each depicts.
8. **Committing the artwork.** The 50 new images exist only as untracked files
   in one worktree. They are one `git clean` from gone and should be committed
   somewhere deliberately.
